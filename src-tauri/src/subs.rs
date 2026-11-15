//! Subtitle files (SRT, WebVTT, ASS/SSA) into lines Telly can show, look up
//! and log.
//!
//! - Text is read as UTF-8, UTF-16 or Shift_JIS, whichever it is.
//! - ASS override tags ({\an8}, {\pos(…)}) go; \N becomes a line break.
//! - Signs and songs (styles that say so, or lines placed on screen with
//!   \pos) are kept but marked, and the player hides them by default.
//! - Readings in brackets after kanji, 漢字(かんじ), become furigana, as does
//!   WebVTT's <ruby>漢字<rt>かんじ</rt></ruby>.
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
        Err(_) if mostly_utf8(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Err(_) => {
            // Shift_JIS, else EUC-JP (older files made on Linux), whichever reads cleanly.
            let (text, _, bad) = encoding_rs::SHIFT_JIS.decode(bytes);
            if bad {
                let (euc, _, euc_bad) = encoding_rs::EUC_JP.decode(bytes);
                if !euc_bad {
                    return euc.into_owned();
                }
            }
            text.into_owned()
        }
    }
}

/// Whether bytes that aren't all UTF-8 are UTF-8 apart from a few bad bytes (a
/// character cut off, a stray byte): more characters past ASCII read as UTF-8
/// than bytes fail to. Shift_JIS text fails on nearly every character.
fn mostly_utf8(bytes: &[u8]) -> bool {
    let (mut read, mut bad) = (0, 0);
    for chunk in bytes.utf8_chunks() {
        read += chunk.valid().chars().filter(|c| !c.is_ascii()).count();
        bad += chunk.invalid().len();
    }
    read > bad
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

// WebVTT's ruby is marked with these until split_ruby takes it out: the base,
// then its reading. They're private-use characters, so no subtitle's own text has them.
const RUBY_BASE: char = '\u{E000}';
const RUBY_READING: char = '\u{E001}';
const RUBY_END: char = '\u{E002}';

/// Marks WebVTT's ruby, <ruby>明日<rt>あした</rt></ruby>, for split_ruby, so
/// the reading doesn't end up in the text.
fn mark_ruby(text: &str) -> std::borrow::Cow<'_, str> {
    static RUBY: OnceLock<Regex> = OnceLock::new();
    static RT: OnceLock<Regex> = OnceLock::new();
    static RP: OnceLock<Regex> = OnceLock::new();
    let rt = re(&RT, r"(?is)<rt(?:[.\s][^>]*)?>(.*?)(?:</rt>|$)");
    let rp = re(&RP, r"(?is)<rp(?:[.\s][^>]*)?>.*?</rp>");
    re(&RUBY, r"(?is)<ruby(?:[.\s][^>]*)?>(.*?)(?:</ruby>|$)").replace_all(text, |c: &regex::Captures| {
        let inner = rp.replace_all(&c[1], "");
        let mut out = String::new();
        let mut last = 0;
        // Each reading is for the base before it.
        for r in rt.captures_iter(&inner) {
            let whole = r.get(0).map_or(0..0, |m| m.range());
            out.push(RUBY_BASE);
            out.push_str(&inner[last..whole.start]);
            out.push(RUBY_READING);
            out.push_str(&r[1]);
            out.push(RUBY_END);
            last = whole.end;
        }
        out.push_str(&inner[last..]);
        out
    })
}

/// Takes furigana out of `text`: 漢字(かんじ) → 漢字 with a reading, and the
/// same for ruby marked by mark_ruby.
fn split_ruby(text: &str) -> (String, Vec<(usize, usize, String)>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut ruby = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == RUBY_BASE {
            let rest = &chars[i + 1..];
            let reading = rest.iter().position(|&x| x == RUBY_READING);
            let end = rest.iter().position(|&x| x == RUBY_END);
            if let (Some(reading), Some(end)) = (reading, end)
                && reading < end
            {
                let base = &rest[..reading];
                let text = rest[reading + 1..end].iter().collect::<String>().trim().to_string();
                if !base.is_empty() && !text.is_empty() {
                    ruby.push((out.len(), base.len(), text));
                }
                out.extend_from_slice(base);
                i += end + 2;
                continue;
            }
        }
        if matches!(c, RUBY_BASE | RUBY_READING | RUBY_END) {
            i += 1;
            continue;
        }
        if (c == '(' || c == '（') && out.last().is_some_and(|&p| is_kanji(p)) {
            let close = if c == '(' { ')' } else { '）' };
            if let Some(len) = chars[i + 1..].iter().position(|&x| x == close) {
                let inner: String = chars[i + 1..i + 1 + len].iter().collect();
                // The base is the run of kanji just before the bracket, after
                // any that have a reading already (魔導士(まどうし)達(たち): 達).
                let floor = ruby.last().map_or(0, |r: &(usize, usize, String)| r.0 + r.1).min(out.len());
                let base_len = out[floor..].iter().rev().take_while(|&&p| is_kanji(p)).count();
                if !inner.is_empty() && len <= 12 && inner.chars().all(is_kana) && base_len > 0 {
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
    let text = mark_ruby(raw);
    let text = re(&TAGS, r"\{[^}]*\}").replace_all(&text, "");
    let text = re(&HTML, r"</?[a-zA-Z][^>]*>").replace_all(&text, "");
    let text = text.replace("\\N", "\n").replace("\\n", "\n").replace("\\h", " ").replace('\u{200B}', "");
    let text: String = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    if text.is_empty() || end <= start {
        return None;
    }
    // Readings first, so a speaker's name with one, （炭治郎(たんじろう)）, comes off whole.
    let (text, ruby) = split_ruby(&text);
    let (speaker, rest) = split_speaker(&text);
    let cut = text.chars().count() - rest.chars().count();
    let ruby = ruby.into_iter().filter(|r| r.0 >= cut).map(|(at, len, reading)| (at - cut, len, reading)).collect();
    Some(Line { start, end, text: rest, ruby, speaker, sign })
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
        } else if raw.trim_matches(|c: char| c.is_ascii_whitespace()).is_empty() {
            // (A line of a full-width space, "　", is part of the cue.)
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

/// The words of a style's name, in lower case: "OP Romaji", "TLNote" and
/// "sign_0423_EpTitle" give op and romaji, tl and note, sign, 0423, ep and title.
fn style_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            continue;
        }
        if !word.is_empty() {
            let p = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            if (p.is_ascii_lowercase() && c.is_ascii_uppercase())
                || (p.is_ascii_uppercase() && c.is_ascii_uppercase() && next_lower)
                || p.is_ascii_digit() != c.is_ascii_digit()
            {
                words.push(std::mem::take(&mut word));
            }
        }
        word.push(c.to_ascii_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether a style (or an effect) is for signs, songs or notes: its name says
/// so in a word of its own, as in Sign, Signs, Title, OP Kara or TL Note, but
/// not Subtitle or Notebook.
fn sign_style(style: &str) -> bool {
    const WORDS: &[&str] = &[
        "sign", "signs", "typeset", "ts", "title", "titles", "logo", "screen", "onscreen", "insert", "song", "songs", "lyric",
        "lyrics", "karaoke", "kara", "kfx", "fx", "op", "ed", "opening", "ending", "note", "notes", "tl", "tn",
    ];
    style_words(style).iter().any(|w| WORDS.contains(&w.as_str()))
}

fn parse_ass(text: &str) -> Vec<Line> {
    // (start, end, text, a sign by its style or effect, placed with \pos or \move)
    let mut events: Vec<(f64, f64, std::borrow::Cow<str>, bool, bool)> = Vec::new();
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
        // A drawing ({\p1}m 0 0 l 100 0 …) is a shape on screen, not words.
        static DRAWING: OnceLock<Regex> = OnceLock::new();
        let body = re(&DRAWING, r"\{[^}]*\\p[1-9][^}]*\}[^{]*").replace_all(body, "");
        let named = sign_style(get("style")) || sign_style(get("effect"));
        let placed = body.contains("\\pos(") || body.contains("\\move(");
        events.push((start, end, body, named, placed));
    }
    // Signs are placed where they go on screen with \pos or \move. Dialogue
    // isn't, though it may be moved to the top ({\an8}, while two people talk
    // at once): so a line with \an8 alone is dialogue, and so are placed
    // lines in a file that places most of its lines, as some conversions do.
    let unnamed = events.iter().filter(|e| !e.3).count();
    let placed = events.iter().filter(|e| !e.3 && e.4).count();
    let placing_marks_signs = placed * 2 <= unnamed;
    events
        .into_iter()
        .filter_map(|(start, end, body, named, placed)| make_line(start, end, &body, named || (placed && placing_marks_signs)))
        .collect()
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

/// The share of dialogue lines that read as Japanese: some kana or kanji, and
/// not mostly gaps (furigana set as lines of their own, as some TV-caption
/// rips do) or Latin letters (English lines). 1 for a clean file.
pub fn quality(lines: &[Line]) -> f64 {
    let dialogue: Vec<&Line> = lines.iter().filter(|l| !l.sign && !l.text.trim().is_empty()).collect();
    if dialogue.is_empty() {
        return 0.0;
    }
    let good = dialogue
        .iter()
        .filter(|l| {
            let chars: Vec<char> = l.text.chars().filter(|c| *c != '\n').collect();
            let japanese = chars.iter().filter(|&&c| is_kana(c) || is_kanji(c)).count();
            let gaps = chars.iter().filter(|c| c.is_whitespace()).count();
            let latin = chars.iter().filter(|c| c.is_ascii_alphabetic()).count();
            japanese >= 2 && gaps * 10 <= chars.len() * 3 && latin * 10 <= chars.len() * 3
        })
        .count();
    good as f64 / dialogue.len() as f64
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
    fn reads_a_file_in_euc_jp() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\n日本語の字幕です\n";
        let (euc, _, _) = encoding_rs::EUC_JP.encode(srt);
        assert_eq!(parse("ep.srt", &euc).unwrap()[0].text, "日本語の字幕です");
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode(srt);
        assert_eq!(parse("ep.srt", &sjis).unwrap()[0].text, "日本語の字幕です");
    }

    #[test]
    fn readings_side_by_side_and_on_a_speaker_s_name() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\n鬼殺隊(きさつたい)隊士(たいし)だ\n\n2\n00:00:03,000 --> 00:00:04,000\n（炭治郎(たんじろう)）禰豆子(ねずこ)！\n\n3\n00:00:05,000 --> 00:00:06,000\n（<ruby>男<rt>おとこ</rt></ruby>）どうした？\n";
        let lines = parse("ep.vtt", format!("WEBVTT\n\n{srt}").as_bytes()).unwrap();
        assert_eq!((lines[0].text.as_str(), &lines[0].ruby), ("鬼殺隊隊士だ", &vec![(0, 3, "きさつたい".into()), (3, 2, "たいし".into())]));
        assert_eq!((lines[1].speaker.as_str(), lines[1].text.as_str(), &lines[1].ruby), ("炭治郎", "禰豆子！", &vec![(0, 3, "ねずこ".into())]));
        assert_eq!((lines[2].speaker.as_str(), lines[2].text.as_str()), ("男", "どうした？"));
    }

    #[test]
    fn a_line_of_a_full_width_space_doesn_t_end_a_cue() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\n\u{3000}\nこんにちは\n  \n2\n00:00:03,000 --> 00:00:04,000\nえっ？\n";
        let lines = parse("ep.srt", srt.as_bytes()).unwrap();
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["こんにちは", "えっ？"]);
    }

    #[test]
    fn a_drawing_isn_t_read_as_words() {
        let ass = "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\p1}m 0 0 l 100 0 100 100 0 100{\\p0}\nDialogue: 0,0:00:04.00,0:00:06.00,Default,,0,0,0,,{\\c&H000000&\\p2}m 0 0 l 10 0{\\p0}元気？\nDialogue: 0,0:00:07.00,0:00:08.00,Default,,0,0,0,,{\\pos(10,10)\\pbo0}もしもし\n";
        let lines = parse("ep.ass", ass.as_bytes()).unwrap();
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["元気？", "もしもし"]);
    }

    #[test]
    fn a_style_named_subtitle_is_dialogue_and_one_named_sign_or_title_is_not() {
        let ass = "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:03.00,Subtitle,,0,0,0,,もしもし\nDialogue: 0,0:00:04.00,0:00:06.00,Subtitle,,0,0,0,,元気？\n";
        let lines = parse("ep.ass", ass.as_bytes()).unwrap();
        assert!(lines.iter().all(|l| !l.sign));
        assert!(is_japanese(&lines));
        for style in ["Subtitle", "Subtitles", "Notebook", "Default", "Main Top", "DefaultItalics", "Edward"] {
            assert!(!sign_style(style), "{style} is dialogue");
        }
        for style in ["Sign", "Signs", "sign_0423_EpTitle", "Title", "Episode Title", "OP Kara", "ED-Romaji", "OPRomaji", "Note", "TL Note", "TLNote", "Insert Song", "Lyrics"] {
            assert!(sign_style(style), "{style} is a sign or song");
        }
    }

    #[test]
    fn a_line_moved_to_the_top_is_dialogue_and_a_placed_one_a_sign() {
        let events = "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";
        let ass = format!(
            "{events}Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{{\\an8}}待って！\nDialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,行くぞ\nDialogue: 0,0:00:04.00,0:00:06.00,Default,,0,0,0,,{{\\an7\\pos(320,40)}}木ノ葉病院\nDialogue: 0,0:00:07.00,0:00:09.00,Default,,0,0,0,,そうか\n"
        );
        let signs: Vec<bool> = parse("ep.ass", ass.as_bytes()).unwrap().iter().map(|l| l.sign).collect();
        assert_eq!(signs, [false, false, true, false]);
        // A file that places every line: placing says nothing about which are signs.
        let ass = format!(
            "{events}Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{{\\pos(640,680)}}もしもし\nDialogue: 0,0:00:04.00,0:00:06.00,Default,,0,0,0,,{{\\pos(640,40)}}元気？\n"
        );
        let lines = parse("ep.ass", ass.as_bytes()).unwrap();
        assert!(lines.iter().all(|l| !l.sign) && is_japanese(&lines));
    }

    #[test]
    fn decodes_shift_jis() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("1\n00:00:01,000 --> 00:00:02,000\n日本語\n");
        let lines = parse("a.srt", &bytes).unwrap();
        assert_eq!(lines[0].text, "日本語");
    }

    #[test]
    fn a_utf8_file_with_a_character_cut_off_is_still_read_as_utf8() {
        let mut bytes = "1\n00:00:01,000 --> 00:00:02,000\n明日の試験\n".as_bytes().to_vec();
        bytes.extend([0xE3, 0x81]);
        let lines = parse("a.srt", &bytes).unwrap();
        assert_eq!(lines[0].text, "明日の試験\n\u{FFFD}");
        // Shift_JIS kana and kanji, which read as UTF-8 only now and then, are still Shift_JIS.
        let text = "1\n00:00:01,000 --> 00:00:02,000\n憂鬱な朝、窓の外には霧雨が降っていた。\n\n2\n00:00:03,000 --> 00:00:04,000\nｱﾆﾒを見ながら日本語を勉強する。\n";
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(text);
        assert_eq!(decode(&bytes), text);
    }

    #[test]
    fn reads_webvtt() {
        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.500 align:center\n<c.japanese>こんにちは</c>\n";
        let lines = parse("a.vtt", vtt.as_bytes()).unwrap();
        assert_eq!(lines[0].text, "こんにちは");
        assert_eq!(lines[0].end, 2.5);
    }

    #[test]
    fn webvtt_ruby_is_furigana_not_text() {
        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.500\n<ruby>明日<rt>あした</rt></ruby>の試験\n\n00:03.000 --> 00:04.000\n<ruby.big>ＡＩ<rp>(</rp><rt>エーアイ</rt><rp>)</rp></ruby>の<ruby>未<rt>み</rt>来<rt>らい</rt></ruby>\n";
        let lines = parse("a.vtt", vtt.as_bytes()).unwrap();
        assert_eq!(lines[0].text, "明日の試験");
        assert_eq!(lines[0].ruby, vec![(0, 2, "あした".into())]);
        assert_eq!(lines[1].text, "ＡＩの未来");
        assert_eq!(lines[1].ruby, vec![(0, 2, "エーアイ".into()), (3, 1, "み".into()), (4, 1, "らい".into())]);
    }
}
