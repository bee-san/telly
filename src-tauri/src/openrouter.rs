//! OpenRouter (https://openrouter.ai): speech models that make subtitles for
//! episodes Jimaku has nothing for, once Settings has a key for it. It needs
//! an account with credit; the key goes in the Authorization header.
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

use crate::backend::Backend;
use crate::subs::Line;
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

const API: &str = "https://openrouter.ai/api/v1";
/// The model used until Settings picks one.
const MODEL: &str = "microsoft/mai-transcribe-2";
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
    match status {
        200 => {}
        401 => return Err(anyhow!("OpenRouter didn’t accept the API key. Check it in Settings.")),
        402 => return Err(anyhow!("The OpenRouter account is out of credit. Add some at openrouter.ai.")),
        code => {
            let said = serde_json::from_str::<Value>(&answer).ok().and_then(|v| v["error"]["message"].as_str().map(str::to_string)).unwrap_or_default();
            return Err(anyhow!("OpenRouter answered {code} {said}"));
        }
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
