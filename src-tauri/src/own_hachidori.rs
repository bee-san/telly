//! Telly's own Hachidori, ready to mine without a browser. Its hidden engine
//! page (hachidori-addon/telly/host.js) asks for this work over the media
//! protocol, at /hd/…:
//!
//! - Your dictionaries: every .zip in the dictionaries folder (Settings →
//!   Hachidori; Documents\dictionaries when there is one), installed once each
//!   and again when the file changes. The word dictionaries go in first, so
//!   they lead the popup and the card's definition; Hachidori Settings can
//!   reorder them.
//! - Its Anki template: the note type (kiku_typed) in the deck from Settings
//!   (例文マイニング), tagged Telly and the show being watched (Fairy_Tail),
//!   with Hachidori's own field mapping for Kiku, so a card can be added the moment a line is hovered.
//! - Premining's questions: the Yomitan API requests Hachidori Relay would
//!   send a sharing Hachidori, answered by Hachidori's own API code in the page,
//!   but for its lookups, which the page answers with only what Telly reads.
//!
//! None of this happens while it's linked to the Hachidori in a browser, which
//! then has the dictionaries and the Anki setup.

use crate::backend::Backend;
use crate::{anki, cards};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Settings: the dictionaries folder, the note type (Settings → Cards and Anki, also
/// what Check its fields reads), and the deck ({show} is the show's title).
pub const FOLDER: &str = "hachidori.dictionaries";
pub const MODEL: &str = "anki.noteType";
pub const DECK: &str = "anki.deck";
pub const INSTALLED: &str = "hachidori.installed";
const DEFAULT_MODEL: &str = "kiku_typed";
const DEFAULT_DECK: &str = "例文マイニング";
/// Word dictionaries, by a word in their file name, in the order they go in.
const FIRST: [&str; 4] = ["jitendex", "jmdict", "daijirin", "sankoku"];
/// The page asks for work every second or so while it's open; after this long
/// without hearing from it, premining asks Hachidori Relay instead.
const PAGE_GONE: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Own {
    state: Mutex<State>,
    signal: Condvar,
}

#[derive(Default)]
struct State {
    /// The Anki setup the page should make: {model, deck, fields}.
    anki: Option<Value>,
    anki_error: String,
    /// The dictionary being installed, and the last one that failed.
    installing: String,
    failed: HashMap<String, String>,
    /// Premining's requests, and their answers.
    next_id: u64,
    requests: VecDeque<(u64, Value)>,
    answers: HashMap<u64, std::result::Result<Value, String>>,
    page_seen: Option<Instant>,
}

/// What Settings shows.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub folder: String,
    pub total: usize,
    pub installed: usize,
    pub installing: String,
    pub failed: Vec<String>,
    pub model: String,
    pub deck: String,
    pub anki_error: String,
}

fn lock(own: &Own) -> std::sync::MutexGuard<'_, State> {
    own.state.lock().unwrap_or_else(|e| e.into_inner())
}

// ---- Dictionaries ----

/// The folder of dictionaries: the one chosen in Settings, else Documents\dictionaries.
pub fn folder(backend: &Backend) -> Option<PathBuf> {
    let chosen = backend.db.setting_str(FOLDER);
    if !chosen.trim().is_empty() {
        return Some(PathBuf::from(chosen.trim()));
    }
    let documents = PathBuf::from(std::env::var_os("USERPROFILE")?).join("Documents").join("dictionaries");
    documents.is_dir().then_some(documents)
}

/// Each .zip in `dir`: its name, size and revision (size and time modified).
pub(crate) fn archives(dir: &Path) -> Vec<(String, u64, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || !name.to_lowercase().ends_with(".zip") {
            continue;
        }
        let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        out.push((name, meta.len(), format!("{}:{modified}", meta.len())));
    }
    out
}

/// The order dictionaries go in: word dictionaries first (Jitendex, JMdict,
/// then the monolingual ones), then the rest, smallest first.
pub(crate) fn install_order(name: &str, size: u64) -> (usize, u64) {
    let lower = name.to_lowercase();
    let first = FIRST.iter().position(|w| lower.contains(w) && !lower.contains("jmnedict")).unwrap_or(FIRST.len());
    (first, size)
}

fn installed(backend: &Backend) -> Map<String, Value> {
    backend.db.setting(INSTALLED).ok().flatten().and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

/// The next dictionary for the page to install, or {} when there's none.
pub fn next_dictionary(backend: &Backend) -> Value {
    let Some(dir) = folder(backend) else { return json!({}) };
    let done = installed(backend);
    let failed = lock(&backend.own_hachidori).failed.clone();
    let mut due: Vec<(String, u64, String)> = archives(&dir)
        .into_iter()
        .filter(|(name, _, revision)| done.get(name).and_then(Value::as_str) != Some(revision) && failed.get(name) != Some(revision))
        .collect();
    due.sort_by_key(|(name, size, _)| install_order(name, *size));
    match due.into_iter().next() {
        Some((name, size, revision)) => {
            lock(&backend.own_hachidori).installing = name.clone();
            let url = format!("/hd/dictionaries/file/{}", percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC));
            json!({ "file": name, "size": size, "revision": revision, "url": url })
        }
        None => json!({}),
    }
}

/// The dictionaries installed, each with its revision, as text.
pub fn installed_revisions(backend: &Backend) -> String {
    Value::Object(installed(backend)).to_string()
}

/// Whether a dictionary in the folder is still to be installed (or is being
/// installed), so a lookup now could miss its words. One that failed to
/// install isn't waited for.
pub fn installing(backend: &Backend) -> bool {
    let Some(dir) = folder(backend) else { return false };
    let done = installed(backend);
    let failed = lock(&backend.own_hachidori).failed.clone();
    archives(&dir).iter().any(|(name, _, revision)| done.get(name).and_then(Value::as_str) != Some(revision) && failed.get(name) != Some(revision))
}

/// A dictionary file for the page to upload.
pub fn dictionary_file(backend: &Backend, name: &str) -> Result<PathBuf> {
    let dir = folder(backend).ok_or_else(|| anyhow!("no dictionaries folder"))?;
    // A file's name alone: on Windows "C:…" joined to the folder would be anywhere on C:.
    if name.contains(['/', '\\', ':']) || name.contains("..") {
        return Err(anyhow!("not a dictionary"));
    }
    let path = dir.join(name);
    if path.is_file() { Ok(path) } else { Err(anyhow!("{name} isn’t in the dictionaries folder any more")) }
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_encoding::percent_decode_str(&v.replace('+', " ")).decode_utf8_lossy().into_owned())
    })
}

/// The page's report on one dictionary: `file`, `rev`, `ok`, and `error`.
pub fn dictionary_done(backend: &Backend, query: &str) -> Result<()> {
    let file = query_value(query, "file").ok_or_else(|| anyhow!("no file"))?;
    let revision = query_value(query, "rev").ok_or_else(|| anyhow!("no revision"))?;
    let mut state = lock(&backend.own_hachidori);
    state.installing.clear();
    if query_value(query, "ok").as_deref() == Some("1") {
        state.failed.remove(&file);
        drop(state);
        let mut done = installed(backend);
        done.insert(file, json!(revision));
        backend.db.set_setting(INSTALLED, &Value::Object(done))?;
    } else {
        let error = query_value(query, "error").unwrap_or_else(|| "Hachidori didn’t take it".into());
        state.failed.insert(file.clone(), revision);
        drop(state);
        backend.db.set_setting(&format!("{INSTALLED}.error"), &json!(format!("{file}: {error}")))?;
    }
    Ok(())
}

// ---- The Anki template ----

/// The deck cards from `show` go in.
pub fn deck_for(backend: &Backend, show: &str) -> String {
    let pattern = backend.db.setting_str(DECK);
    let pattern = if pattern.trim().is_empty() { DEFAULT_DECK.to_string() } else { pattern.trim().to_string() };
    // A title's own "::" would make deeper decks.
    pattern.replace("{show}", &show.replace("::", ":"))
}

/// The tags cards from `show` get: Telly, and the show's title (a tag can't hold a space).
pub fn tags_for(show: &str) -> Vec<String> {
    vec!["Telly".to_string(), show.split_whitespace().collect::<Vec<_>>().join("_")]
}

pub fn model(backend: &Backend) -> String {
    let chosen = backend.db.setting_str(MODEL);
    if chosen.trim().is_empty() { DEFAULT_MODEL.to_string() } else { chosen.trim().to_string() }
}

/// The player opened an episode of `show`: Hachidori mines into its deck, with its tags.
/// Reads the note type's fields, then leaves the rest to the page. The deck
/// isn't made here: only premining makes decks in Anki.
pub fn watching(backend: &Arc<Backend>, show: &str) {
    let backend = backend.clone();
    let show = show.to_string();
    std::thread::spawn(move || {
        let url = cards::anki_url(&backend);
        let (model, deck) = (model(&backend), deck_for(&backend, &show));
        let result = anki::model_field_names(&url, &model)
            .and_then(|fields| if fields.is_empty() { Err(anyhow!("Anki has no note type called {model}")) } else { Ok(fields) });
        let mut state = lock(&backend.own_hachidori);
        match result {
            Ok(fields) => {
                state.anki = Some(json!({ "model": model, "deck": deck, "tags": tags_for(&show), "fields": fields }));
                state.anki_error.clear();
            }
            Err(e) => state.anki_error = format!("{e:#}"),
        }
    });
}

/// The Anki setup for the page to make, or {}.
pub fn anki_setup(backend: &Backend) -> Value {
    lock(&backend.own_hachidori).anki.clone().unwrap_or_else(|| json!({}))
}

/// The page's report on the Anki setup: `ok`, and `error`.
pub fn anki_done(backend: &Backend, query: &str) {
    let mut state = lock(&backend.own_hachidori);
    if query_value(query, "ok").as_deref() == Some("1") {
        state.anki_error.clear();
    } else {
        state.anki_error = query_value(query, "error").unwrap_or_else(|| "Hachidori didn’t take the Anki setup".into());
    }
}

pub fn report(backend: &Backend) -> Report {
    let dir = folder(backend);
    let files = dir.as_deref().map(archives).unwrap_or_default();
    let done = installed(backend);
    let state = lock(&backend.own_hachidori);
    let anki = state.anki.clone().unwrap_or_default();
    Report {
        folder: dir.map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
        total: files.len(),
        installed: files.iter().filter(|(name, _, revision)| done.get(name).and_then(Value::as_str) == Some(revision)).count(),
        installing: state.installing.clone(),
        failed: state.failed.keys().cloned().collect(),
        model: anki.get("model").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| model(backend)),
        deck: anki.get("deck").and_then(Value::as_str).unwrap_or("").to_string(),
        anki_error: state.anki_error.clone(),
    }
}

// ---- Premining's questions ----

impl Own {
    /// Whether the page is there to answer.
    pub fn answering(&self) -> bool {
        lock(self).page_seen.is_some_and(|t| t.elapsed() < PAGE_GONE)
    }

    /// Asks the page a Yomitan API request (Hachidori's hd_api_* message) and
    /// waits for its answer.
    pub fn ask(&self, request: Value, timeout: Duration) -> Result<Value> {
        let id = {
            let mut state = lock(self);
            state.next_id += 1;
            let id = state.next_id;
            state.requests.push_back((id, request));
            id
        };
        self.signal.notify_all();
        let deadline = Instant::now() + timeout;
        let mut state = lock(self);
        loop {
            if let Some(answer) = state.answers.remove(&id) {
                return answer.map_err(|e| anyhow!("Hachidori: {e}"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                state.requests.retain(|(r, _)| *r != id);
                return Err(anyhow!("Telly’s Hachidori didn’t answer in time"));
            }
            state = self.signal.wait_timeout(state, left).map(|(s, _)| s).unwrap_or_else(|e| e.into_inner().0);
        }
    }

    /// The page's next request, waiting up to `wait` for one: {id, request}, or {}.
    pub fn next_request(&self, wait: Duration) -> Value {
        let deadline = Instant::now() + wait;
        let mut state = lock(self);
        loop {
            state.page_seen = Some(Instant::now());
            if let Some((id, request)) = state.requests.pop_front() {
                return json!({ "id": id, "request": request });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return json!({});
            }
            state = self.signal.wait_timeout(state, left).map(|(s, _)| s).unwrap_or_else(|e| e.into_inner().0);
        }
    }

    /// The page's answer to request `id`: {ok, result} or {ok: false, error}.
    pub fn answer(&self, query: &str, body: &[u8]) -> Result<()> {
        let id: u64 = query_value(query, "id").and_then(|v| v.parse().ok()).ok_or_else(|| anyhow!("no request id"))?;
        let reply: Value = serde_json::from_slice(body)?;
        let answer = if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(reply.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(reply.get("error").and_then(Value::as_str).unwrap_or("it failed").to_string())
        };
        lock(self).answers.insert(id, answer);
        self.signal.notify_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_dictionaries_go_in_first() {
        let mut names = vec![
            ("Japanese Proper Nouns Mk II.zip", 292_117_300),
            ("JMnedict [2026-08-11].zip", 11_426_576),
            ("Bee's Frequency Dictionary.zip", 427_502),
            ("JMdict.zip", 13_290_494),
            ("daijirin4-gpt-6-luna-en-jp-v2.zip", 149_093_667),
            ("Jitendex.org [2026-08-11].zip", 38_698_313),
        ];
        names.sort_by_key(|(n, s)| install_order(n, *s));
        let order: Vec<&str> = names.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            order,
            [
                "Jitendex.org [2026-08-11].zip",
                "JMdict.zip",
                "daijirin4-gpt-6-luna-en-jp-v2.zip",
                "Bee's Frequency Dictionary.zip",
                "JMnedict [2026-08-11].zip",
                "Japanese Proper Nouns Mk II.zip"
            ]
        );
    }

    #[test]
    fn only_a_dictionary_in_the_folder_is_served() {
        let dir = std::env::temp_dir().join(format!("telly-dictionary-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let folder = dir.join("dictionaries");
        std::fs::create_dir_all(&folder).unwrap();
        backend.db.set_setting(FOLDER, &json!(folder.to_string_lossy())).unwrap();
        // "C:x.zip" names a file on drive C on Windows, wherever the folder is (a file of that name here stands in for it).
        for name in ["Jitendex.zip", "C:x.zip"] {
            std::fs::write(folder.join(name), b"zip").unwrap();
        }
        assert!(dictionary_file(&backend, "Jitendex.zip").is_ok());
        for name in ["C:x.zip", "../telly.sqlite", "a/b.zip"] {
            assert!(dictionary_file(&backend, name).is_err(), "{name}");
        }
        drop(backend);
        // On Windows the stand-in is in the current folder of drive C (src-tauri, for a checkout there), not in `dir`.
        let _ = std::fs::remove_file(folder.join("C:x.zip"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn premining_s_questions_reach_the_page_and_come_back() {
        let own = Arc::new(Own::default());
        let page = own.clone();
        let worker = std::thread::spawn(move || {
            let next = page.next_request(Duration::from_secs(5));
            assert_eq!(next["request"]["type"], "hd_api_term_entries");
            let id = next["id"].as_u64().unwrap();
            page.answer(&format!("id={id}"), br#"{"ok": true, "result": {"results": []}}"#).unwrap();
        });
        let answer = own.ask(json!({ "type": "hd_api_term_entries", "terms": ["悪意"] }), Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
        assert_eq!(answer, json!({ "results": [] }));
        assert!(own.answering());
        // Nobody there: it gives up.
        let lonely = Own::default();
        assert!(lonely.ask(json!({}), Duration::from_millis(50)).is_err());
    }
}
