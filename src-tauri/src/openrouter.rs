//! OpenRouter (https://openrouter.ai): speech models that make subtitles for
//! episodes Jimaku has nothing for, once Settings has a key for it, and
//! gpt-oss, which answers Explain and Previously on when Claude Code isn't on
//! this computer. It needs an account with credit; the key goes in the
//! Authorization header.
//!
//!   POST /api/v1/audio/transcriptions
//!     {"model": "elevenlabs/scribe-v2", "language": "ja",
//!      "input_audio": {"data": "<base64>", "format": "wav"},
//!      "response_format": "verbose_json", "timestamp_granularities": ["word", "segment"]}
//!   →  {"text": "…", "segments": [{"start", "end", "text"}], "words": [{"word", "start", "end"}]}
//!
//! Which of `segments` and `words` come back depends on the model's provider.
//! WAV is the audio all three models take: MAI-Transcribe 2 refuses FLAC
//! (502), and Whisper large v3 heard nothing in it.
//! Providers give a request 60 seconds, so an episode goes in pieces.
//!
//!   POST /api/v1/chat/completions
//!     {"model": "openai/gpt-oss-120b", "stream": true, "messages": [{"role", "content"}],
//!      "reasoning": {"effort": "low", "exclude": true}}
//!   →  server-sent events, each `data: {"choices": [{"delta": {"content": "…"}}]}`, then `data: [DONE]`

use crate::backend::Backend;
use crate::subs::Line;
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const API: &str = "https://openrouter.ai/api/v1";
/// The model used until Settings picks one.
const MODEL: &str = "microsoft/mai-transcribe-2";
/// The model that answers Explain and Previously on when Claude Code isn't on this computer.
pub const CHAT_MODEL: &str = "openai/gpt-oss-120b";
/// About how long a piece of an episode is, in seconds.
pub const PIECE: f64 = 300.0;
/// A cut moves this far from its even place, at most, to somewhere quiet.
const REACH: f64 = 15.0;
/// A segment up to this many characters is a subtitle line as it is.
const LINE: usize = 30;
/// A pause this long between words ends a line, in seconds.
const PAUSE: f64 = 0.6;

/// Where to ask: OpenRouter, or TELLY_OPENROUTER_API (a stand-in for tests).
pub fn api_address() -> String {
    std::env::var("TELLY_OPENROUTER_API").unwrap_or_else(|_| API.to_string())
}

/// Whether OpenRouter takes `key`: it's asked about the key itself, which costs nothing.
pub fn check_key(key: &str) -> Result<()> {
    check_key_at(&api_address(), key)
}

fn check_key_at(api: &str, key: &str) -> Result<()> {
    if key.trim().is_empty() {
        return Err(anyhow!("There’s no OpenRouter API key yet"));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).http_status_as_error(false).build().into();
    let response = agent.get(format!("{api}/key")).header("Authorization", format!("Bearer {}", key.trim())).call().map_err(|e| anyhow!("OpenRouter isn’t reachable ({e})"))?;
    match response.status().as_u16() {
        200 => Ok(()),
        401 | 403 => Err(anyhow!("OpenRouter didn’t accept the API key. Check that all of it was copied.")),
        code => Err(anyhow!("OpenRouter answered {code}")),
    }
}

/// OpenRouter's key and the model picked in Settings, when there's a key: it
/// then makes the subtitles, and not whisper.cpp on this computer.
pub fn chosen(backend: &Backend) -> Option<(String, String)> {
    let key = backend.db.setting_str("openrouter.key");
    if key.is_empty() {
        return None;
    }
    let model = match backend.db.setting_str("openrouter.model") {
        model if model.is_empty() => MODEL.to_string(),
        model => model,
    };
    Some((key, model))
}

/// Who a model's subtitles are marked as made by.
pub fn maker(model: &str) -> &'static str {
    match model.split_once('/').map_or("", |(by, _)| by) {
        "microsoft" => "MAI-Transcribe",
        "elevenlabs" => "ElevenLabs",
        _ => "Whisper",
    }
}

#[derive(Deserialize)]
struct Heard {
    #[serde(default)]
    text: String,
    #[serde(default)]
    segments: Vec<Segment>,
    #[serde(default)]
    words: Vec<Word>,
}

#[derive(Deserialize)]
struct Segment {
    start: f64,
    end: f64,
    text: String,
}

#[derive(Deserialize)]
struct Word {
    word: String,
    start: f64,
    end: f64,
}

/// OpenRouter answered 429: too many requests for now. How long it asked to
/// wait (its Retry-After), when it said.
#[derive(Debug)]
pub struct RateLimited(pub Option<Duration>);

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OpenRouter is taking no more requests for now (429)")
    }
}

impl std::error::Error for RateLimited {}

/// The lines `model` hears in a piece of audio (a WAV file's bytes), timed from the piece's start.
pub fn transcribe(api: &str, key: &str, model: &str, wav: &[u8]) -> Result<Vec<Line>> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(180))).http_status_as_error(false).build().into();
    let body = json!({
        "model": model,
        "language": "ja",
        "input_audio": { "data": base64::engine::general_purpose::STANDARD.encode(wav), "format": "wav" },
        "response_format": "verbose_json",
        "timestamp_granularities": ["word", "segment"],
    });
    let mut response = agent
        .post(format!("{api}/audio/transcriptions"))
        .header("Authorization", format!("Bearer {key}"))
        .send_json(&body)
        .map_err(|e| anyhow!("OpenRouter isn’t reachable ({e})"))?;
    let status = response.status().as_u16();
    if status == 429 {
        let wait = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
        return Err(RateLimited(wait.map(Duration::from_secs)).into());
    }
    let answer = response.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().context("reading OpenRouter’s answer")?;
    if status != 200 {
        return Err(refused(status, &answer));
    }
    let heard: Heard = serde_json::from_str(&answer).context("OpenRouter sent something unexpected")?;
    let said_something = !heard.text.trim().is_empty();
    let lines = lines(heard);
    // A provider that doesn't time what it hears sends the words alone: there's nowhere to put them.
    if lines.is_empty() && said_something {
        return Err(anyhow!("{model} sent words without their times. Choose the other model in Settings."));
    }
    Ok(lines)
}

/// What OpenRouter turning a request away means, in words for whoever asked.
fn refused(code: u16, answer: &str) -> anyhow::Error {
    match code {
        401 => anyhow!("OpenRouter didn’t accept the API key. Check it in Settings."),
        402 => anyhow!("The OpenRouter account is out of credit. Add some at openrouter.ai."),
        429 => anyhow!("OpenRouter is taking no more requests for now. Try again in a minute."),
        code => {
            let said = serde_json::from_str::<Value>(answer).ok().and_then(|v| v["error"]["message"].as_str().map(str::to_string)).unwrap_or_default();
            anyhow!("OpenRouter answered {code} {said}")
        }
    }
}

/// Asks gpt-oss (`CHAT_MODEL`) with `system` as its instructions, calling
/// `on_text` with each piece of the answer as it's written, and returns the
/// whole answer. `effort` is how hard it thinks; `stop` ends it early.
pub fn chat(api: &str, key: &str, system: &str, prompt: &str, effort: Option<&str>, mut on_text: impl FnMut(&str), stop: &AtomicBool) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(240))).http_status_as_error(false).build().into();
    let mut body = json!({
        "model": CHAT_MODEL,
        "stream": true,
        "messages": [{ "role": "system", "content": system }, { "role": "user", "content": prompt }],
    });
    if let Some(effort) = effort {
        // Only the answer is shown, so its thinking isn't sent.
        body["reasoning"] = json!({ "effort": effort, "exclude": true });
    }
    let mut response = agent
        .post(format!("{api}/chat/completions"))
        .header("Authorization", format!("Bearer {key}"))
        .send_json(&body)
        .map_err(|e| anyhow!("OpenRouter isn’t reachable ({e})"))?;
    let status = response.status().as_u16();
    if status != 200 {
        let answer = response.body_mut().read_to_string().unwrap_or_default();
        return Err(refused(status, &answer));
    }
    let mut text = String::new();
    for line in BufReader::new(response.into_body().into_reader()).lines() {
        if stop.load(Ordering::SeqCst) {
            return Err(anyhow!("Stopped."));
        }
        let line = line.map_err(|e| anyhow!("OpenRouter stopped answering ({e})"))?;
        // Lines that start with ':' only keep the connection open.
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else { continue };
        if let Some(message) = chunk["error"]["message"].as_str() {
            return Err(anyhow!("OpenRouter: {message}"));
        }
        if let Some(piece) = chunk["choices"][0]["delta"]["content"].as_str().filter(|p| !p.is_empty()) {
            text.push_str(piece);
            on_text(piece);
        }
    }
    match text.trim() {
        "" => Err(anyhow!("{CHAT_MODEL} gave no answer.")),
        answer => Ok(answer.to_string()),
    }
}

fn line(start: f64, end: f64, text: &str) -> Option<Line> {
    let text = text.trim();
    (!text.is_empty()).then(|| Line { start, end, text: text.to_string(), ruby: vec![], speaker: String::new(), sign: false })
}

/// Subtitle lines from what a model heard: its segments, with one too long
/// for a line made again from the words in it. With no segments, the words alone.
fn lines(heard: Heard) -> Vec<Line> {
    if heard.segments.is_empty() {
        return from_words(&heard.words.iter().collect::<Vec<_>>());
    }
    let mut out = Vec::new();
    for s in &heard.segments {
        let inside: Vec<&Word> = heard.words.iter().filter(|w| w.start >= s.start && w.start < s.end).collect();
        if s.text.trim().chars().count() <= LINE || inside.is_empty() {
            out.extend(line(s.start, s.end, &s.text));
        } else {
            out.extend(from_words(&inside));
        }
    }
    out
}

/// Lines from timed words. A line ends with its sentence, at a pause, at a
/// comma once it's 20 characters long, or at 30 characters.
fn from_words(words: &[&Word]) -> Vec<Line> {
    let closes = |w: &Word| !w.word.trim().is_empty() && w.word.trim().chars().all(|c| "。！？!?、,…」』）)".contains(c));
    let mut out = Vec::new();
    let mut text = String::new();
    let mut start = 0.0;
    for (i, w) in words.iter().enumerate() {
        if text.trim().is_empty() {
            start = w.start;
        }
        text.push_str(&w.word);
        let length = text.trim().chars().count();
        // What the line ends with, inside any closing bracket.
        let last = text.trim_end().trim_end_matches(['」', '』', '）', ')']).chars().last();
        let ends = match words.get(i + 1) {
            None => true,
            // Marks that close a sentence stay on its line.
            Some(next) if closes(next) => false,
            Some(next) => {
                matches!(last, Some('。' | '！' | '？' | '!' | '?'))
                    || next.start - w.end > PAUSE
                    || (length >= 20 && matches!(last, Some('、' | ',')))
                    || length >= LINE
            }
        };
        if ends {
            out.extend(line(start, w.end, &text));
            text.clear();
        }
    }
    out
}

/// Where to cut an episode's audio (mono samples at `rate` Hz) into pieces of
/// about `PIECE` seconds: the quietest half second near each even cut, so few
/// words are cut in two. Starts at 0 and ends with the audio's length.
pub fn cuts(samples: &[i16], rate: usize) -> Vec<f64> {
    let length = samples.len() as f64 / rate as f64;
    let window = rate / 2;
    let mut out = vec![0.0];
    let mut even = PIECE;
    // A last piece under half a minute stays with the one before.
    while even + 30.0 < length {
        let from = ((even - REACH) * rate as f64) as usize;
        let to = ((even + REACH) * rate as f64) as usize;
        let quietest = (from..to)
            .step_by(rate / 10)
            .min_by_key(|&i| samples[i..i + window].iter().map(|s| s.unsigned_abs() as u64).sum::<u64>())
            .unwrap_or(from);
        out.push((quietest + window / 2) as f64 / rate as f64);
        even += PIECE;
    }
    out.push(length);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(json: Value) -> Vec<(f64, f64, String)> {
        lines(serde_json::from_value(json).unwrap()).into_iter().map(|l| (l.start, l.end, l.text)).collect()
    }

    #[test]
    fn words_become_lines_at_sentences_and_pauses() {
        // One word per character, as ElevenLabs sends Japanese.
        let word = |w: &str, start: f64| json!({ "word": w, "start": start, "end": start + 0.2 });
        let words: Vec<Value> = "そうか。」"
            .chars()
            .enumerate()
            .map(|(i, c)| word(&c.to_string(), 1.0 + i as f64 * 0.2))
            .chain("行くぞ".chars().enumerate().map(|(i, c)| word(&c.to_string(), 2.0 + i as f64 * 0.2)))
            // A pause, with no full stop before it.
            .chain("待て".chars().enumerate().map(|(i, c)| word(&c.to_string(), 5.0 + i as f64 * 0.2)))
            .collect();
        let got = heard(json!({ "text": "そうか。」行くぞ待て", "words": words }));
        let texts: Vec<&str> = got.iter().map(|l| l.2.as_str()).collect();
        assert_eq!(texts, ["そうか。」", "行くぞ", "待て"]);
        assert_eq!((got[1].0, got[1].1), (2.0, 2.6));
    }

    #[test]
    fn a_long_segment_is_made_again_from_its_words() {
        let long = "あ".repeat(24) + "、" + &"い".repeat(12) + "。";
        let words: Vec<Value> = long.chars().enumerate().map(|(i, c)| json!({ "word": c.to_string(), "start": 10.0 + i as f64 * 0.1, "end": 10.1 + i as f64 * 0.1 })).collect();
        let got = heard(json!({
            "text": "…",
            "segments": [{ "start": 2.0, "end": 4.0, "text": " 短いせりふ。" }, { "start": 10.0, "end": 13.8, "text": long }],
            "words": words,
        }));
        let texts: Vec<String> = got.iter().map(|l| l.2.clone()).collect();
        assert_eq!(texts, ["短いせりふ。".to_string(), "あ".repeat(24) + "、", "い".repeat(12) + "。"]);
    }

    #[test]
    fn cuts_fall_where_it_is_quiet() {
        let rate = 100;
        // 11 minutes of sound, quiet for a second from 4:52.
        let mut samples = vec![1000i16; 660 * rate];
        samples[292 * rate..293 * rate].fill(0);
        let got = cuts(&samples, rate);
        assert_eq!(got.len(), 4);
        assert_eq!((got[0], got[3]), (0.0, 660.0));
        assert!((292.0..=293.0).contains(&got[1]), "{got:?}");
        assert!((585.0..=615.0).contains(&got[2]), "{got:?}");
        // 20 seconds past 5 minutes isn't worth a piece of its own.
        assert_eq!(cuts(&vec![1000i16; 320 * rate], rate), [0.0, 320.0]);
    }

    /// A stand-in for OpenRouter that answers one request, and hands back what it was sent.
    fn stand_in(status: u16, answer: Value) -> (String, std::thread::JoinHandle<(String, Value)>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let api = format!("http://{}/api/v1", server.server_addr());
        let asked = std::thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let key = request.headers().iter().find(|h| h.field.equiv("Authorization")).map(|h| h.value.as_str().to_string()).unwrap_or_default();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert_eq!(request.url(), "/api/v1/audio/transcriptions");
            request.respond(tiny_http::Response::from_string(answer.to_string()).with_status_code(status)).unwrap();
            (key, serde_json::from_str(&body).unwrap())
        });
        (api, asked)
    }

    #[test]
    fn asks_openrouter_for_timed_japanese() {
        let (api, asked) = stand_in(200, json!({ "text": "こんにちは。", "segments": [{ "start": 0.5, "end": 1.5, "text": "こんにちは。" }] }));
        let lines = transcribe(&api, "a-key", "openai/whisper-large-v3", b"RIFF").unwrap();
        assert_eq!(lines.iter().map(|l| (l.start, l.end, l.text.as_str())).collect::<Vec<_>>(), [(0.5, 1.5, "こんにちは。")]);
        let (key, body) = asked.join().unwrap();
        assert_eq!(key, "Bearer a-key");
        assert_eq!(body["model"], "openai/whisper-large-v3");
        assert_eq!(body["language"], "ja");
        assert_eq!(body["response_format"], "verbose_json");
        assert_eq!(body["input_audio"], json!({ "data": "UklGRg==", "format": "wav" }));
    }

    #[test]
    fn gpt_oss_answers_as_it_writes() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let api = format!("http://{}/api/v1", server.server_addr());
        let asked = std::thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert_eq!(request.url(), "/api/v1/chat/completions");
            let events = [
                ": OPENROUTER PROCESSING",
                r#"data: {"choices":[{"delta":{"role":"assistant","content":""}}]}"#,
                r#"data: {"choices":[{"delta":{"content":"Nice to "}}]}"#,
                r#"data: {"choices":[{"delta":{"content":"meet you."}}]}"#,
                "data: [DONE]",
            ];
            request.respond(tiny_http::Response::from_string(events.join("\n\n") + "\n\n")).unwrap();
            serde_json::from_str::<Value>(&body).unwrap()
        });
        let mut pieces = Vec::new();
        let answer = chat(&api, "a-key", "Be brief.", "はじめまして？", Some("low"), |p| pieces.push(p.to_string()), &AtomicBool::new(false)).unwrap();
        assert_eq!(answer, "Nice to meet you.");
        assert_eq!(pieces, ["Nice to ", "meet you."]);
        let body = asked.join().unwrap();
        assert_eq!(body["model"], CHAT_MODEL);
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"], json!([{ "role": "system", "content": "Be brief." }, { "role": "user", "content": "はじめまして？" }]));
        assert_eq!(body["reasoning"], json!({ "effort": "low", "exclude": true }));

        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let api = format!("http://{}/api/v1", server.server_addr());
        let asked = std::thread::spawn(move || {
            let answer = json!({ "error": { "code": 402, "message": "Insufficient credits" } }).to_string();
            server.recv().unwrap().respond(tiny_http::Response::from_string(answer).with_status_code(402)).unwrap();
        });
        let error = chat(&api, "a-key", "", "", None, |_| {}, &AtomicBool::new(false)).unwrap_err().to_string();
        assert_eq!(error, "The OpenRouter account is out of credit. Add some at openrouter.ai.");
        asked.join().unwrap();
    }

    #[test]
    fn says_what_went_wrong() {
        let (api, asked) = stand_in(401, json!({ "error": { "code": 401, "message": "No auth credentials found" } }));
        let error = transcribe(&api, "", "elevenlabs/scribe-v2", b"").unwrap_err().to_string();
        assert_eq!(error, "OpenRouter didn’t accept the API key. Check it in Settings.");
        asked.join().unwrap();
        let (api, asked) = stand_in(200, json!({ "text": "こんにちは。" }));
        let error = transcribe(&api, "a-key", "elevenlabs/scribe-v2", b"").unwrap_err().to_string();
        assert_eq!(error, "elevenlabs/scribe-v2 sent words without their times. Choose the other model in Settings.");
        asked.join().unwrap();
    }
}
