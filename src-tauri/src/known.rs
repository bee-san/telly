//! The words you know, as Anki says. Which cards count is chosen in Settings
//! → Known words: a deck, note types, the field the word is in, and whether a
//! card must be mature.
//!
//! - studied: a card of the note has been studied, young or mature. A studied
//!   word is one you have a card for, so premining doesn't list it.
//! - mature: a card of it is 21 days or more from its next review, as Anki
//!   counts it.
//! - known: studied, or mature, as Settings says. Known words are the share
//!   of an episode's words you know.
//!
//! Telly keeps a copy of the studied notes' words, so a show's page and the
//! Library don't read every note again each time, and work with Anki closed.
//! Bringing the copy up to date is two searches for note ids, then reading
//! only the notes it hasn't seen.

use crate::backend::Backend;
use crate::{anki, cards, premine};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Notes read from Anki at a time: a note with its definitions is about 10 KB,
/// and Anki's whole answer has to arrive within its 15 seconds.
const NOTES_PER_READ: usize = 500;

/// Telly's copy of the studied notes.
#[derive(Serialize, Deserialize, Default, PartialEq)]
struct Copy {
    /// The search the notes came from: another choice in Settings starts the copy again.
    search: String,
    /// Each studied note's word, by note id.
    notes: HashMap<i64, String>,
    /// The notes with a mature card.
    mature: HashSet<i64>,
}

/// The words Anki says you have a card for, and those that count as known.
#[derive(Default)]
pub struct Words {
    pub studied: HashSet<String>,
    pub known: HashSet<String>,
}

fn path(backend: &Backend) -> PathBuf {
    backend.dirs.local.join("known-words.json")
}

fn read(backend: &Backend) -> Copy {
    std::fs::read(path(backend)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Whether a word must be on a mature card to count as known.
fn mature_only(backend: &Backend) -> bool {
    backend.db.setting_str("known.when") == "mature"
}

/// The field the word is in: the one chosen in Settings, else the one your
/// Hachidori card format puts the word in.
pub fn field(backend: &Backend) -> Result<String> {
    let chosen = backend.db.setting_str("known.field");
    if chosen.trim().is_empty() { premine::word_field(backend) } else { Ok(chosen.trim().to_string()) }
}

/// Anki's search for the notes that count: `field` isn't empty, in `deck`
/// (and its subdecks) when one is chosen, of one of `note_types` when any are.
pub fn search(field: &str, deck: &str, note_types: &[String]) -> String {
    let mut search = format!("\"{field}:_*\"");
    if !deck.trim().is_empty() {
        search.push_str(&format!(" \"deck:{}\"", deck.trim()));
    }
    let types: Vec<String> = note_types.iter().filter(|t| !t.trim().is_empty()).map(|t| format!("\"note:{}\"", t.trim())).collect();
    match types.len() {
        0 => {}
        1 => search.push_str(&format!(" {}", types[0])),
        _ => search.push_str(&format!(" ({})", types.join(" OR "))),
    }
    search
}

fn chosen_search(backend: &Backend) -> Result<String> {
    let types: Vec<String> = backend
        .db
        .setting("known.noteTypes")
        .ok()
        .flatten()
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()))
        .unwrap_or_default();
    Ok(search(&field(backend)?, &backend.db.setting_str("known.deck"), &types))
}

/// The notes `search` finds in Anki now: those studied, and those of them with a mature card.
fn found(url: &str, search: &str) -> Result<(HashSet<i64>, HashSet<i64>)> {
    let ids = |extra: &str| -> Result<HashSet<i64>> {
        let found = anki::invoke(url, "findNotes", json!({ "query": format!("{search} {extra}") }))?;
        Ok(found.as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default())
    };
    let studied = ids("-is:new")?;
    let mature = ids("prop:ivl>=21")?.intersection(&studied).copied().collect();
    Ok((studied, mature))
}

/// Brings Telly's copy up to date with Anki. True when it changed.
pub fn refresh(backend: &Backend) -> Result<bool> {
    let url = cards::anki_url(backend);
    let search = chosen_search(backend)?;
    let field = field(backend)?;
    let (studied, mature) = found(&url, &search)?;
    let before = read(backend);
    let mut copy = if before.search == search { read(backend) } else { Copy { search, ..Default::default() } };
    copy.notes.retain(|id, _| studied.contains(id));
    let unseen: Vec<i64> = studied.iter().filter(|id| !copy.notes.contains_key(id)).copied().collect();
    for chunk in unseen.chunks(NOTES_PER_READ) {
        for note in anki::notes_info(&url, chunk)? {
            copy.notes.insert(note.id, note.field(&field).unwrap_or_default());
        }
    }
    copy.mature = mature;
    let changed = copy != before;
    if changed {
        crate::util::write_whole(&path(backend), serde_json::to_vec(&copy)?)?;
    }
    Ok(changed)
}

/// Stands for what counts as known now: Telly's copy as it was last saved, and
/// whether a card must be mature. A share worked out at another stamp is stale.
pub fn stamp(backend: &Backend) -> String {
    let saved = std::fs::metadata(path(backend)).and_then(|m| m.modified()).ok();
    let ms = saved.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis()).unwrap_or(0);
    format!("{ms}{}", if mature_only(backend) { " mature" } else { "" })
}

/// The words, from Telly's copy (none before Anki has been read once).
pub fn words(backend: &Backend) -> Words {
    let copy = read(backend);
    let mature_only = mature_only(backend);
    let mut words = Words::default();
    for (id, word) in copy.notes {
        if !mature_only || copy.mature.contains(&id) {
            words.known.insert(word.clone());
        }
        words.studied.insert(word);
    }
    words
}

/// What Settings → Known words chooses from and shows: Anki's decks, its note
/// types that have the word's field, and how many notes the choice finds now.
pub fn sources(backend: &Backend) -> Result<Value> {
    let url = cards::anki_url(backend);
    let field = field(backend)?;
    let names = |action: &str| -> Result<Vec<String>> {
        Ok(anki::invoke(&url, action, json!({}))?.as_array().map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect()).unwrap_or_default())
    };
    let mut note_types = Vec::new();
    for name in names("modelNames")? {
        if anki::model_field_names(&url, &name)?.contains(&field) {
            note_types.push(name);
        }
    }
    let (studied, mature) = found(&url, &chosen_search(backend)?)?;
    Ok(json!({ "field": field, "decks": names("deckNames")?, "noteTypes": note_types, "studied": studied.len(), "mature": mature.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_the_deck_and_note_types_chosen() {
        assert_eq!(search("Expression", "", &[]), "\"Expression:_*\"");
        assert_eq!(search("Expression", "Type", &["Kiku".into()]), "\"Expression:_*\" \"deck:Type\" \"note:Kiku\"");
        assert_eq!(
            search("Expression", " 例文マイニング ", &["Kiku".into(), "kiku_typed".into(), " ".into()]),
            "\"Expression:_*\" \"deck:例文マイニング\" (\"note:Kiku\" OR \"note:kiku_typed\")"
        );
    }
}
