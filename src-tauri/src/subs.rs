//! Subtitle files (SRT, WebVTT, ASS/SSA) into lines Telly can show, look up
//! and log.
//!
//! - Text is read as UTF-8, UTF-16 or Shift_JIS, whichever it is.
//! - ASS override tags ({\an8}, {\pos(…)}) go; \N becomes a line break.
//! - Signs and songs (styles or lines that say so, or that are placed on
//!   screen) are kept but marked, and the player hides them by default.
//! - Readings in brackets after kanji, 漢字(かんじ), become furigana.
//! - A speaker's name in brackets at the start, （ナルト）, is kept apart.

use anyhow::{anyhow, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// Seconds.
    pub start: f64,
    pub end: f64,
    /// The words, with furigana and speaker taken out. Line breaks are "\n".
    pub text: String,
    /// Furigana: (character offset in `text` where the base starts, base length in characters, reading).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ruby: Vec<(usize, usize, String)>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub speaker: String,
    /// A sign or song, not dialogue.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sign: bool,
}

/// Decodes a subtitle file's bytes.
pub fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return encoding_rs::UTF_16LE.decode(bytes).0.into_owned();
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return encoding_rs::UTF_16BE.decode(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::SHIFT_JIS.decode(bytes).0.into_owned(),
    }
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid pattern"))
}

/// "00:01:02,345", "01:02.345" or ASS's "0:01:02.34" in seconds.
fn timestamp(text: &str) -> Option<f64> {
    let text = text.trim().replace(',', ".");
    let parts: Vec<&str> = text.split(':').collect();
    let (h, m, s) = match parts.as_slice() {
        [h, m, s] => (h.parse::<f64>().ok()?, m.parse::<f64>().ok()?, s.parse::<f64>().ok()?),
        [m, s] => (0.0, m.parse::<f64>().ok()?, s.parse::<f64>().ok()?),
        _ => return None,
    };
    Some(h * 3600.0 + m * 60.0 + s)
}

fn is_kanji(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '々' | '〆' | 'ヶ')
}

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30FF}')
}

/// Takes furigana in brackets out of `text`: 漢字(かんじ) → 漢字 with a reading.
fn split_ruby(text: &str) -> (String, Vec<(usize, usize, String)>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut ruby = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == '(' || c == '（') && out.last().is_some_and(|&p| is_kanji(p)) {
            let close = if c == '(' { ')' } else { '）' };
            if let Some(len) = chars[i + 1..].iter().position(|&x| x == close) {
                let inner: String = chars[i + 1..i + 1 + len].iter().collect();
                if !inner.is_empty() && len <= 12 && inner.chars().all(is_kana) {
                    // The base is the run of kanji just before the bracket.
                    let base_len = out.iter().rev().take_while(|&&p| is_kanji(p)).count();
                    ruby.push((out.len() - base_len, base_len, inner));
                    i += len + 2;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    (out.into_iter().collect(), ruby)
}

/// Takes a speaker's name in brackets off the front: （ナルト）よし！ → ("ナルト", "よし！").
fn split_speaker(text: &str) -> (String, String) {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(&R, r"^[（(]([^）)\n]{1,12})[）)]\s*");
    if let Some(c) = r.captures(text) {
        let rest = text[c.get(0).map(|m| m.end()).unwrap_or(0)..].to_string();
        if !rest.trim().is_empty() {
            return (c[1].to_string(), rest);
        }
    }
    (String::new(), text.to_string())
}

fn make_line(start: f64, end: f64, raw: &str, sign: bool) -> Option<Line> {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    static HTML: OnceLock<Regex> = OnceLock::new();
    let text = re(&TAGS, r"\{[^}]*\}").replace_all(raw, "");
    let text = re(&HTML, r"</?[a-zA-Z][^>]*>").replace_all(&text, "");
    let text = text.replace("\\N", "\n").replace("\\n", "\n").replace("\\h", " ").replace('\u{200B}', "");
    let text: String = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    if text.is_empty() || end <= start {
        return None;
    }
    let (speaker, text) = split_speaker(&text);
    let (text, ruby) = split_ruby(&text);
    Some(Line { start, end, text, ruby, speaker, sign })
}

fn parse_srt_like(text: &str) -> Vec<Line> {
    static ARROW: OnceLock<Regex> = OnceLock::new();
    let arrow = re(&ARROW, r"^\s*([\d:.,]+)\s*-->\s*([\d:.,]+)");
    let mut lines = Vec::new();
    let mut current: Option<(f64, f64, Vec<String>)> = None;
    for raw in text.lines() {
        if let Some(c) = arrow.captures(raw) {
            if let Some((s, e, body)) = current.take() {
                lines.extend(make_line(s, e, &body.join("\n"), false));
            }
            if let (Some(s), Some(e)) = (timestamp(&c[1]), timestamp(&c[2])) {
                current = Some((s, e, Vec::new()));
            }
        } else if raw.trim().is_empty() {
            if let Some((s, e, body)) = current.take() {
                lines.extend(make_line(s, e, &body.join("\n"), false));
            }
        } else if let Some((_, _, body)) = current.as_mut() {
            body.push(raw.to_string());
        }
    }
    if let Some((s, e, body)) = current.take() {
        lines.extend(make_line(s, e, &body.join("\n"), false));
    }
    // A cue number left on the end of the previous cue's text.
    for line in &mut lines {
        if let Some((before, last)) = line.text.rsplit_once('\n') {
            if last.trim().chars().all(|c| c.is_ascii_digit()) {
                line.text = before.to_string();
            }
        }
    }
    lines
}

fn sign_style(style: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)sign|song|lyric|karaoke|kara|\bop\b|\bed\b|title|note|insert|logo|screen|^tl").is_match(style)
}

fn parse_ass(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut in_events = false;
    let mut format: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_events = line.eq_ignore_ascii_case("[events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Format:") {
            format = rest.split(',').map(|f| f.trim().to_lowercase()).collect();
            continue;
        }
        let Some(rest) = line.strip_prefix("Dialogue:") else { continue };
        if format.is_empty() {
            format = ["layer", "start", "end", "style", "name", "marginl", "marginr", "marginv", "effect", "text"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        }
        let fields: Vec<&str> = rest.splitn(format.len(), ',').collect();
        let get = |name: &str| format.iter().position(|f| f == name).and_then(|i| fields.get(i)).map(|s| s.trim()).unwrap_or("");
        let (Some(start), Some(end)) = (timestamp(get("start")), timestamp(get("end"))) else { continue };
        let body = format.iter().position(|f| f == "text").and_then(|i| fields.get(i)).copied().unwrap_or("");
        let placed = body.contains("\\pos(") || body.contains("\\move(") || body.contains("\\an8");
        let sign = sign_style(get("style")) || sign_style(get("effect")) || placed;
        lines.extend(make_line(start, end, body, sign));
    }
    lines
}

/// Reads a subtitle file. `name` (its file name) decides the format when the text doesn't.
pub fn parse(name: &str, bytes: &[u8]) -> Result<Vec<Line>> {
    let text = decode(bytes);
    let lower = name.to_lowercase();
    let mut lines = if text.contains("[Events]") || text.contains("[events]") || lower.ends_with(".ass") || lower.ends_with(".ssa") {
        parse_ass(&text)
    } else {
        parse_srt_like(&text)
    };
    if lines.is_empty() {
        return Err(anyhow!("{name} has no lines Telly can read"));
    }
    lines.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
    // Lines shown twice (the same text, touching) become one.
    let mut merged: Vec<Line> = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some(last) = merged.last_mut() {
            if last.text == line.text && last.sign == line.sign && line.start - last.end < 0.05 {
                last.end = last.end.max(line.end);
                continue;
            }
        }
        merged.push(line);
    }
    Ok(merged)
}

/// Whether the lines are Japanese: most dialogue has kana or kanji.
pub fn is_japanese(lines: &[Line]) -> bool {
    let dialogue: Vec<&Line> = lines.iter().filter(|l| !l.sign).collect();
    if dialogue.is_empty() {
        return false;
    }
    let japanese = dialogue.iter().filter(|l| l.text.chars().any(|c| is_kana(c) || is_kanji(c))).count();
    japanese * 2 > dialogue.len()
}

/// The text as a person reads it, furigana dropped (for matching a card's sentence).
pub fn plain(text: &str) -> String {
    text.replace('\n', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_srt_with_furigana_and_speakers() {
        let srt = "1\r\n00:12:11,000 --> 00:12:13,500\r\n（ナルト）明日(あした)の試験(しけん)\r\n絶対に合格してみせる。\r\n\r\n2\r\n00:12:14,000 --> 00:12:15,000\r\nえっ？\r\n";
        let lines = parse("ep04.srt", srt.as_bytes()).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start, 731.0);
        assert_eq!(lines[0].speaker, "ナルト");
        assert_eq!(lines[0].text, "明日の試験\n絶対に合格してみせる。");
        assert_eq!(lines[0].ruby, vec![(0, 2, "あした".into()), (3, 2, "しけん".into())]);
        assert!(is_japanese(&lines));
    }

    #[test]
    fn reads_ass_and_marks_signs() {
        let ass = "[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:03.50,Default,,0,0,0,,{\\i1}もしもし{\\i0}\\N元気？\nDialogue: 0,0:00:02.00,0:00:04.00,Sign,,0,0,0,,{\\pos(100,200)}木ノ葉\nDialogue: 0,0:00:05.00,0:00:06.00,OP Romaji,,0,0,0,,{\\k20}ha{\\k30}ru\n";
        let lines = parse("ep.ass", ass.as_bytes()).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "もしもし\n元気？");
        assert!(!lines[0].sign);
        assert!(lines[1].sign && lines[2].sign);
    }

    #[test]
    fn decodes_shift_jis() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("1\n00:00:01,000 --> 00:00:02,000\n日本語\n");
        let lines = parse("a.srt", &bytes).unwrap();
        assert_eq!(lines[0].text, "日本語");
    }

    #[test]
    fn reads_webvtt() {
        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.500 align:center\n<c.japanese>こんにちは</c>\n";
        let lines = parse("a.vtt", vtt.as_bytes()).unwrap();
        assert_eq!(lines[0].text, "こんにちは");
        assert_eq!(lines[0].end, 2.5);
    }
}
