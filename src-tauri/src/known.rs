//! The words you know, as Anki says. Which cards count is chosen in Settings
//! → Known words: a deck, note types, the field the word is in, and whether a
//! card must be mature.
//!
//! - studied: a card of the note has been studied, young or mature. A studied
//!   word is one you have a card for, so premining doesn't list it.
//! - suspended: every studied card of the note is suspended. It's still a
//!   card you have, but its word isn't known.
//! - mature: a card of it that isn't suspended is 21 days or more from its
//!   next review, as Anki counts it.
//! - known: studied and not suspended, or mature, as Settings says. Known
//!   words are the share of an episode's words you know.
//!
//! Telly keeps a copy of the studied notes' words, so a show's page and the
//! Library don't read every note again each time, and work with Anki closed.
//! Bringing the copy up to date is three searches for note ids and one for
//! the notes edited since it last looked, then reading only the notes it
//! hasn't seen and those edited, so a word corrected in Anki is corrected here.

use crate::backend::Backend;
use crate::util::now_ms;
use crate::{anki, cards, premine};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

/// Notes read from Anki at a time: a note with its definitions is about 10 KB,
/// and Anki's whole answer has to arrive within its 15 seconds.
const NOTES_PER_READ: usize = 500;

/// The setting that keeps when Telly last read Anki into its copy, in Unix ms.
const LOOKED: &str = "known.lookedAt";

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Held while the copy is brought up to date, so when Telly last looked goes
/// with the copy it saved then.
static REFRESHING: Mutex<()> = Mutex::new(());

/// Telly's copy of the studied notes.
#[derive(Serialize, Deserialize, Default, PartialEq)]
struct Copy {
    /// The search the notes came from: another choice in Settings starts the copy again.
    search: String,
    /// Each studied note's word, by note id.
    notes: HashMap<i64, String>,
    /// The notes with a mature card that isn't suspended.
    mature: HashSet<i64>,
    /// The notes whose studied cards are all suspended (none in a copy saved
    /// before Telly read them, until Anki is next read).
    #[serde(default)]
    suspended: HashSet<i64>,
}

impl Copy {
    /// Whether note `id`'s word counts as known.
    fn counts(&self, id: i64, mature_only: bool) -> bool {
        !self.suspended.contains(&id) && (!mature_only || self.mature.contains(&id))
    }
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

/// The ids of the notes Anki finds for `query`.
fn note_ids(url: &str, query: &str) -> Result<HashSet<i64>> {
    let found = anki::invoke(url, "findNotes", json!({ "query": query }))?;
    Ok(found.as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default())
}

/// The notes `search` finds in Anki now: those studied, those of them whose
/// studied cards are all suspended, and those with a mature card that isn't.
fn found(url: &str, search: &str) -> Result<(HashSet<i64>, HashSet<i64>, HashSet<i64>)> {
    let ids = |extra: &str| note_ids(url, &format!("{search} {extra}"));
    let studied = ids("-is:new")?;
    let unsuspended: HashSet<i64> = ids("-is:new -is:suspended")?.intersection(&studied).copied().collect();
    let suspended = studied.difference(&unsuspended).copied().collect();
    let mature = ids("prop:ivl>=21 -is:suspended")?.intersection(&unsuspended).copied().collect();
    Ok((studied, suspended, mature))
}

/// The n for Anki's `edited:n` that finds every note edited since `looked`,
/// at `now` (both Unix ms). Anki counts n whole days back from the start of
/// its next day, which can be up to a day away, so n is the days since
/// `looked`, rounded up, and one more.
fn days_back(looked: i64, now: i64) -> i64 {
    ((now - looked).max(0) + DAY_MS - 1) / DAY_MS + 1
}

/// Brings Telly's copy up to date with Anki. True when it changed.
pub fn refresh(backend: &Backend) -> Result<bool> {
    let _one_at_a_time = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
    let url = cards::anki_url(backend);
    let search = chosen_search(backend)?;
    let field = field(backend)?;
    // An edit made while Anki is being read is read again next time.
    let looking = now_ms();
    let (studied, suspended, mature) = found(&url, &search)?;
    let before = read(backend);
    let mut copy = if before.search == search { read(backend) } else { Copy { search: search.clone(), ..Default::default() } };
    copy.notes.retain(|id, _| studied.contains(id));
    // The notes edited since Telly last looked are read again; when it never has, every note is.
    let edited: HashSet<i64> = match backend.db.setting(LOOKED).ok().flatten().and_then(|v| v.as_i64()) {
        Some(looked) if !copy.notes.is_empty() => note_ids(&url, &format!("{search} -is:new edited:{}", days_back(looked, looking)))?,
        _ => copy.notes.keys().copied().collect(),
    };
    let unseen: Vec<i64> = studied.iter().filter(|id| !copy.notes.contains_key(id) || edited.contains(id)).copied().collect();
    for chunk in unseen.chunks(NOTES_PER_READ) {
        for note in anki::notes_info(&url, chunk)? {
            copy.notes.insert(note.id, note.field(&field).unwrap_or_default());
        }
    }
    copy.mature = mature;
    copy.suspended = suspended;
    let changed = copy != before;
    if changed {
        crate::util::write_whole(&path(backend), serde_json::to_vec(&copy)?)?;
    }
    backend.db.set_setting(LOOKED, &json!(looking))?;
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
    for (id, word) in &copy.notes {
        if copy.counts(*id, mature_only) {
            words.known.insert(word.clone());
        }
    }
    words.studied = copy.notes.into_values().collect();
    words
}

/// When the first card of each known word was made, in Unix ms (a note's id
/// is when it was made), for Stats. It isn't when the word was learnt.
pub fn known_since(backend: &Backend) -> Vec<i64> {
    let copy = read(backend);
    let mature_only = mature_only(backend);
    let mut first: HashMap<&str, i64> = HashMap::new();
    for (id, word) in &copy.notes {
        if copy.counts(*id, mature_only) {
            let at = first.entry(word).or_insert(*id);
            *at = (*at).min(*id);
        }
    }
    first.into_values().collect()
}

/// Each studied note's word, by note id, from Telly's copy.
pub fn note_words(backend: &Backend) -> HashMap<i64, String> {
    read(backend).notes
}

/// What Settings → Cards and Anki → Known words chooses from and shows: Anki's decks, its note
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
    let (studied, suspended, mature) = found(&url, &chosen_search(backend)?)?;
    Ok(json!({
        "field": field,
        "decks": names("deckNames")?,
        "noteTypes": note_types,
        "studied": studied.len(),
        "suspended": suspended.len(),
        "mature": mature.len(),
    }))
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

    #[test]
    fn a_note_whose_studied_cards_are_all_suspended_isn_t_known() {
        let copy = Copy {
            search: String::new(),
            notes: HashMap::from([(1, "食べる".into()), (2, "飲む".into()), (3, "見る".into())]),
            mature: HashSet::from([3]),
            suspended: HashSet::from([2]),
        };
        assert_eq!([1, 2, 3].map(|id| copy.counts(id, false)), [true, false, true]);
        assert_eq!([1, 2, 3].map(|id| copy.counts(id, true)), [false, false, true]);
        // A copy saved before suspended cards were read has none, so its words stay known until Anki is next read.
        let before: Copy = serde_json::from_value(json!({ "search": "", "notes": { "1": "食べる" }, "mature": [] })).unwrap();
        assert!(before.suspended.is_empty() && before.counts(1, false));
    }

    #[test]
    fn looks_for_edits_a_day_further_back_than_it_last_looked() {
        let (now, hour) = (1_700_000_000_000, 60 * 60 * 1000);
        // edited:1 is since the start of Anki's day, which may have been a moment ago.
        assert_eq!(days_back(now, now), 1);
        assert_eq!(days_back(now - 1, now), 2);
        assert_eq!(days_back(now - 24 * hour, now), 2);
        assert_eq!(days_back(now - 24 * hour - 1, now), 3);
        assert_eq!(days_back(now - 30 * hour, now), 3);
        // A clock put back looks no further than for no time at all.
        assert_eq!(days_back(now + hour, now), 1);
    }

    #[test]
    fn a_suspended_note_is_had_but_not_known_and_an_edited_one_is_read_again() {
        use std::sync::Arc;
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("telly-known-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        backend.db.set_setting("known.field", &json!("Word")).unwrap();
        // A stand-in for AnkiConnect: the notes each search finds after `"Word:_*"`, each note's
        // word, and the searches Telly made and the notes it read.
        #[derive(Default)]
        struct Anki {
            found: HashMap<&'static str, Vec<i64>>,
            words: HashMap<i64, &'static str>,
            asked: Vec<String>,
            read: Vec<i64>,
            down: bool,
        }
        let anki = Arc::new(Mutex::new(Anki::default()));
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        backend.db.set_setting("anki.url", &json!(format!("http://{}", server.server_addr().to_ip().unwrap()))).unwrap();
        {
            let anki = anki.clone();
            std::thread::spawn(move || {
                for mut request in server.incoming_requests() {
                    let mut body = String::new();
                    let _ = std::io::Read::read_to_string(request.as_reader(), &mut body);
                    let body: Value = serde_json::from_str(&body).unwrap();
                    let mut anki = anki.lock().unwrap();
                    let answer = match body["action"].as_str().unwrap() {
                        "findNotes" => {
                            let extra = body["params"]["query"].as_str().unwrap().strip_prefix("\"Word:_*\" ").unwrap().to_string();
                            let ids = anki.found.get(extra.as_str()).cloned().unwrap_or_default();
                            anki.asked.push(extra);
                            json!({ "result": ids, "error": null })
                        }
                        "notesInfo" if anki.down => json!({ "result": null, "error": "collection is not available" }),
                        "notesInfo" => {
                            let ids: Vec<i64> = serde_json::from_value(body["params"]["notes"].clone()).unwrap();
                            anki.read.extend(&ids);
                            let notes: Vec<Value> = ids.iter().map(|id| json!({ "noteId": id, "fields": { "Word": { "value": anki.words[id] } } })).collect();
                            json!({ "result": notes, "error": null })
                        }
                        "modelNames" => json!({ "result": ["Kiku"], "error": null }),
                        "modelFieldNames" => json!({ "result": ["Word"], "error": null }),
                        "deckNames" => json!({ "result": ["Mining"], "error": null }),
                        action => panic!("{action}"),
                    };
                    let _ = request.respond(tiny_http::Response::from_string(answer.to_string()));
                }
            });
        }
        let notes_read = || {
            let mut read = std::mem::take(&mut anki.lock().unwrap().read);
            read.sort();
            read
        };
        let asked = || std::mem::take(&mut anki.lock().unwrap().asked);
        let set = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<HashSet<String>>();

        // A copy saved before Telly read suspended cards: its words are known until Anki is next read.
        let saved = json!({ "search": "\"Word:_*\"", "notes": { "1": "食べる", "2": "飲む", "3": "見る" }, "mature": [3] });
        std::fs::write(dir.join("known-words.json"), saved.to_string()).unwrap();
        assert_eq!(words(&backend).known, set(&["食べる", "飲む", "見る"]));

        // 飲む's studied cards are all suspended, and 走る's only mature card is.
        {
            let mut anki = anki.lock().unwrap();
            anki.found = HashMap::from([("-is:new", vec![1, 2, 3, 4]), ("-is:new -is:suspended", vec![1, 3, 4]), ("prop:ivl>=21 -is:suspended", vec![3])]);
            anki.words = HashMap::from([(1, "食べる"), (2, "飲む"), (3, "見る"), (4, "走る")]);
        }
        assert!(refresh(&backend).unwrap());
        // Telly hadn't looked before: every note is read, without a search for edits.
        assert_eq!(asked(), ["-is:new", "-is:new -is:suspended", "prop:ivl>=21 -is:suspended"]);
        assert_eq!(notes_read(), [1, 2, 3, 4]);
        let have = words(&backend);
        assert_eq!(have.studied, set(&["食べる", "飲む", "見る", "走る"]));
        assert_eq!(have.known, set(&["食べる", "見る", "走る"]));
        let mut since = known_since(&backend);
        since.sort();
        assert_eq!(since, [1, 3, 4]);
        backend.db.set_setting("known.when", &json!("mature")).unwrap();
        assert_eq!(words(&backend).known, set(&["見る"]));
        backend.db.set_setting("known.when", &json!("studied")).unwrap();
        let shown = sources(&backend).unwrap();
        assert_eq!([&shown["studied"], &shown["suspended"], &shown["mature"]], [4, 1, 1]);

        // 見る is corrected to 観る in Anki, 30 hours after Telly last looked.
        backend.db.set_setting(LOOKED, &json!(now_ms() - 30 * 60 * 60 * 1000)).unwrap();
        {
            let mut anki = anki.lock().unwrap();
            anki.words.insert(3, "観る");
            anki.found.insert("-is:new edited:3", vec![3]);
        }
        assert!(refresh(&backend).unwrap());
        assert_eq!(asked().last().unwrap(), "-is:new edited:3");
        assert_eq!(notes_read(), [3]);
        assert_eq!(words(&backend).known, set(&["食べる", "観る", "走る"]));

        // 飲む's card is unsuspended: it's known, with no note read, and every share is stale.
        let stamped = stamp(&backend);
        std::thread::sleep(Duration::from_millis(20));
        anki.lock().unwrap().found.insert("-is:new -is:suspended", vec![1, 2, 3, 4]);
        assert!(refresh(&backend).unwrap());
        assert_eq!(asked().last().unwrap(), "-is:new edited:2");
        assert!(notes_read().is_empty());
        assert_eq!(words(&backend).known, set(&["食べる", "飲む", "観る", "走る"]));
        assert_ne!(stamp(&backend), stamped);

        // Nothing changed: the shares stand.
        let stamped = stamp(&backend);
        std::thread::sleep(Duration::from_millis(20));
        assert!(!refresh(&backend).unwrap());
        assert_eq!(stamp(&backend), stamped);

        // A new note can't be read: when Telly last looked stays as it was.
        let looked = backend.db.setting_i64(LOOKED, 0);
        std::thread::sleep(Duration::from_millis(20));
        {
            let mut anki = anki.lock().unwrap();
            anki.found.insert("-is:new", vec![1, 2, 3, 4, 5]);
            anki.down = true;
        }
        assert!(refresh(&backend).is_err());
        assert_eq!(backend.db.setting_i64(LOOKED, 0), looked);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
