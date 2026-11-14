//! AnkiConnect, for what Hachidori doesn't do from inside Telly: adding the
//! moving picture and the line's audio to the card it just made, and reading
//! Anki's decks, note types and notes.

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::time::Duration;

pub const DEFAULT_URL: &str = "http://127.0.0.1:8765";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // Anki closed answers at once on Linux, but Windows tries a closed
        // port for about 2 s: Settings asks every 5 s.
        .timeout_connect(Some(Duration::from_secs(1)))
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into()
}

pub fn invoke(url: &str, action: &str, params: Value) -> Result<Value> {
    let mut response = agent()
        .post(url)
        .send_json(json!({ "action": action, "version": 6, "params": params }))
        .map_err(|e| anyhow!("Anki isn’t reachable at {url} ({e}). Is Anki open with AnkiConnect?"))?;
    let body: Value = response.body_mut().with_config().limit(64 * 1024 * 1024).read_json()?;
    if let Some(error) = body.get("error").and_then(Value::as_str) {
        return Err(anyhow!("AnkiConnect: {error}"));
    }
    Ok(body.get("result").cloned().unwrap_or(Value::Null))
}

pub fn version(url: &str) -> Result<i64> {
    invoke(url, "version", json!({}))?.as_i64().ok_or_else(|| anyhow!("AnkiConnect gave no version"))
}

/// The notes added today. A note's ID is when it was made, in Unix ms.
pub fn notes_added_today(url: &str) -> Result<Vec<i64>> {
    let ids = invoke(url, "findNotes", json!({ "query": "added:1" }))?;
    Ok(ids.as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default())
}

/// One note: its type, its fields' values and its tags.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub id: i64,
    pub model: String,
    pub fields: Map<String, Value>,
    pub tags: Vec<String>,
}

impl Note {
    pub fn field(&self, name: &str) -> Option<String> {
        self.fields.get(name).and_then(|f| f.get("value")).and_then(Value::as_str).map(str::to_string)
    }
}

pub fn notes_info(url: &str, ids: &[i64]) -> Result<Vec<Note>> {
    let info = invoke(url, "notesInfo", json!({ "notes": ids }))?;
    Ok(info
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            Some(Note {
                id: n.get("noteId")?.as_i64()?,
                model: n.get("modelName").and_then(Value::as_str).unwrap_or("").to_string(),
                fields: n.get("fields").and_then(Value::as_object).cloned().unwrap_or_default(),
                tags: n
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|t| t.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// Copies a file on this PC into Anki's media folder; returns the name Anki gave it.
pub fn store_media_file(url: &str, name: &str, path: &std::path::Path) -> Result<String> {
    let stored = invoke(url, "storeMediaFile", json!({ "filename": name, "path": path.to_string_lossy() }))?;
    Ok(stored.as_str().unwrap_or(name).to_string())
}

pub fn update_fields(url: &str, note: i64, fields: &Map<String, Value>) -> Result<()> {
    invoke(url, "updateNoteFields", json!({ "note": { "id": note, "fields": fields } }))?;
    Ok(())
}

pub fn add_tags(url: &str, notes: &[i64], tags: &str) -> Result<()> {
    invoke(url, "addTags", json!({ "notes": notes, "tags": tags }))?;
    Ok(())
}

pub fn model_field_names(url: &str, model: &str) -> Result<Vec<String>> {
    let names = invoke(url, "modelFieldNames", json!({ "modelName": model }))?;
    Ok(strings(&names))
}

/// Anki's decks and its note types, by name.
pub fn deck_and_model_names(url: &str) -> Result<(Vec<String>, Vec<String>)> {
    Ok((strings(&invoke(url, "deckNames", json!({}))?), strings(&invoke(url, "modelNames", json!({}))?)))
}

fn strings(list: &Value) -> Vec<String> {
    list.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

/// A field's text without HTML, furigana brackets or spaces, for matching against subtitles.
pub fn plain_text(html: &str) -> String {
    let mut out = String::new();
    let mut tag = String::new();
    let (mut in_tag, mut in_rt) = (false, false);
    for c in html.chars() {
        if in_tag {
            if c == '>' {
                in_tag = false;
                let t = tag.trim().to_ascii_lowercase();
                if t == "rt" || t.starts_with("rt ") {
                    in_rt = true;
                } else if t == "/rt" {
                    in_rt = false;
                }
                tag.clear();
            } else {
                tag.push(c);
            }
        } else if c == '<' {
            in_tag = true;
        } else if !in_rt {
            out.push(c);
        }
    }
    // Anki's own furigana: 漢字[かんじ].
    let mut cleaned = String::new();
    let mut skip = false;
    for c in out.replace("&nbsp;", " ").replace("&amp;", "&").chars() {
        match c {
            '[' => skip = true,
            ']' => skip = false,
            _ if !skip && !c.is_whitespace() => cleaned.push(c),
            _ => {}
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_drops_html_and_furigana() {
        assert_eq!(plain_text("<b>明日</b>の<ruby>試験<rt>しけん</rt></ruby>、 絶対に"), "明日の試験、絶対に");
        assert_eq!(plain_text(" 明日[あした]の 試験[しけん]"), "明日の試験");
    }
}
