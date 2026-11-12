//! Line search: a word, found in every line it's said in across the library,
//! with the scene around each to watch.
//!
//! telly-lines.sqlite, in %LOCALAPPDATA%\Telly, holds every spoken line of
//! every episode that has subtitles, and the line's words as premining saved
//! them (premine.rs: each word's dictionary form and reading, as Hachidori has
//! them). It's all made from the files in subtitles\, so it can be deleted:
//! Telly makes it again as it starts.
//!
//! - An episode's lines go in when its subtitles sync, and again when its
//!   words are saved. Until they are, its lines are found by their text only.
//! - A word finds every form of itself: 食べる, たべる and 食べた all find
//!   食べた, 食べて and 食べられる, and the whole form is what's marked.
//!   Anything that isn't a word in the index finds the lines with that text,
//!   and so does a word premining doesn't keep (俺, 君, 二人) where it's
//!   written outside every saved word.
//! - Each line found says how many of its other words you don't know, as
//!   Anki has it (known.rs), so the page lists the lines you can read first:
//!   one with none is an i+1 sentence for the word. A line of an episode
//!   whose words aren't saved yet says nothing, and is listed after those.
//! - A scene is the line with the line before and the line after it, when
//!   they're said close by.

use crate::backend::{Backend, Served};
use crate::db::{open_migrated, Episode};
use crate::names::kana::{contains_japanese, contains_kanji, kata_to_hira};
use crate::names::name_parser::HONORIFIC_SUFFIXES;
use crate::premine::{self, SavedUse};
use crate::subs::Line;
use crate::{jobs, known, media, names};
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

const MIGRATIONS: &[&str] = &["
-- One row per spoken line. Its rowid says where it's from: the episode's ID
-- times a million, plus which of the episode's spoken lines it is.
CREATE VIRTUAL TABLE lines USING fts5(
  words,              -- a word per row: 'headword reading from to', its bytes in text
  text UNINDEXED,     -- the line, without its line breaks
  start_s UNINDEXED,
  end_s UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 0'
);
-- The episodes in it, the state of the files each was made from, and
-- whether its words were saved then (or only its lines' text is there).
CREATE TABLE indexed (
  episode_id INTEGER PRIMARY KEY,
  stamp      TEXT NOT NULL,
  words      INTEGER NOT NULL
);
"];

/// Room for an episode's lines among the rowids.
const PER_EPISODE: i64 = 1_000_000;

/// The most lines a search sends for one show: those you can read soonest. A
/// common word is said in tens of thousands of lines, which take seconds to
/// send and to list, and nobody reads past the first hundred.
const PER_SHOW: usize = 100;

/// How long before or after a line another is still part of its scene, in seconds.
const NEAR: f64 = 5.0;
/// What a scene plays before its first line and after its last, in seconds.
const LEAD: f64 = 0.5;

/// A word in a line: as Hachidori has it, and its bytes in the line's text
/// (with its endings, for a verb or an adjective).
#[derive(Clone, Debug, PartialEq)]
struct Word {
    headword: String,
    reading: String,
    from: usize,
    to: usize,
}

impl Word {
    fn is_one_of(&self, forms: &[String]) -> bool {
        forms.iter().any(|f| *f == self.headword || *f == self.reading)
    }
}

/// A line's words as the index keeps them. A word with a space in it has no
/// place there; no headword has one.
fn pack(words: &[Word]) -> String {
    let spaced = |s: &str| s.is_empty() || s.contains(char::is_whitespace);
    let rows: Vec<String> = words
        .iter()
        .filter(|w| !spaced(&w.headword) && !w.reading.contains(char::is_whitespace))
        .map(|w| format!("{} {} {} {}", w.headword, if w.reading.is_empty() { &w.headword } else { &w.reading }, w.from, w.to))
        .collect();
    rows.join("\n")
}

fn unpack(words: &str) -> Vec<Word> {
    words
        .lines()
        .filter_map(|row| {
            let mut parts = row.split(' ');
            let word = Word { headword: parts.next()?.to_string(), reading: parts.next()?.to_string(), from: parts.next()?.parse().ok()?, to: parts.next()?.parse().ok()? };
            parts.next().is_none().then_some(word)
        })
        .collect()
}

/// A line as the index keeps it.
#[derive(Clone, Debug, PartialEq)]
struct Kept {
    episode_id: i64,
    line: usize,
    words: String,
    text: String,
    start: f64,
    end: f64,
}

/// What goes in for a line: its words, its text and when it's said.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    words: String,
    text: String,
    start: f64,
    end: f64,
}

pub struct Index {
    conn: Mutex<Connection>,
}

const KEPT: &str = "SELECT rowid, words, text, start_s, end_s FROM lines";

fn kept(row: &rusqlite::Row) -> rusqlite::Result<Kept> {
    let id: i64 = row.get(0)?;
    Ok(Kept { episode_id: id / PER_EPISODE, line: (id % PER_EPISODE) as usize, words: row.get(1)?, text: row.get(2)?, start: row.get(3)?, end: row.get(4)? })
}

impl Index {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self { conn: Mutex::new(open_migrated(path, MIGRATIONS)?) })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| anyhow!("line search is unavailable"))
    }

    /// The state of the files an episode's lines were made from, when it's in the index.
    fn stamp(&self, episode_id: i64) -> Result<Option<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached("SELECT stamp FROM indexed WHERE episode_id = ?1")?;
        let mut rows = stmt.query_map(params![episode_id], |r| r.get(0))?;
        let stamp = rows.next().transpose()?;
        Ok(stamp)
    }

    /// Puts an episode's lines in, in place of those it had. `words` is whether their words are saved.
    fn replace(&self, episode_id: i64, entries: &[Entry], stamp: &str, words: bool) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM lines WHERE rowid >= ?1 AND rowid < ?2", params![episode_id * PER_EPISODE, (episode_id + 1) * PER_EPISODE])?;
        {
            let mut insert = tx.prepare("INSERT INTO lines (rowid, words, text, start_s, end_s) VALUES (?1, ?2, ?3, ?4, ?5)")?;
            for (i, e) in entries.iter().enumerate() {
                insert.execute(params![episode_id * PER_EPISODE + i as i64, e.words, e.text, e.start, e.end])?;
            }
        }
        tx.execute(
            "INSERT INTO indexed (episode_id, stamp, words) VALUES (?1, ?2, ?3) ON CONFLICT (episode_id) DO UPDATE SET stamp = excluded.stamp, words = excluded.words",
            params![episode_id, stamp, words],
        )?;
        Ok(tx.commit()?)
    }

    /// Takes out an episode that isn't in Telly any more.
    pub(crate) fn forget(&self, episode_id: i64) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM lines WHERE rowid >= ?1 AND rowid < ?2", params![episode_id * PER_EPISODE, (episode_id + 1) * PER_EPISODE])?;
        tx.execute("DELETE FROM indexed WHERE episode_id = ?1", params![episode_id])?;
        Ok(tx.commit()?)
    }

    fn episodes(&self) -> Result<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT episode_id FROM indexed")?;
        let ids = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    /// The episodes whose words are in the index.
    fn with_words(&self) -> Result<HashSet<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached("SELECT episode_id FROM indexed WHERE words")?;
        let ids = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<HashSet<i64>>>()?;
        Ok(ids)
    }

    /// The lines with a word whose headword or reading is one of `forms`, or,
    /// with `prefix`, starts with it (a name with an honorific after it).
    fn with_word(&self, forms: &[String], prefix: Option<&str>) -> Result<Vec<Kept>> {
        let quoted = |f: &str| format!("\"{}\"", f.replace('"', "\"\""));
        let mut asked: Vec<String> = forms.iter().map(|f| quoted(f)).collect();
        asked.extend(prefix.map(|p| format!("{}*", quoted(p))));
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(&format!("{KEPT} WHERE lines MATCH ?1"))?;
        let found = stmt.query_map(params![asked.join(" OR ")], kept)?.collect::<rusqlite::Result<Vec<Kept>>>()?;
        Ok(found)
    }

    /// The lines with `text` in them, of every episode or only of `episodes`.
    /// Every episode's lines take about half a second for 850,000 lines, where
    /// a word takes a twentieth of one: each line is read.
    fn with_text(&self, text: &str, episodes: Option<&[i64]>) -> Result<Vec<Kept>> {
        let like = format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let conn = self.conn()?;
        let Some(episodes) = episodes else {
            let mut stmt = conn.prepare_cached(&format!("{KEPT} WHERE text LIKE ?1 ESCAPE '\\'"))?;
            let found = stmt.query_map(params![like], kept)?.collect::<rusqlite::Result<Vec<Kept>>>()?;
            return Ok(found);
        };
        // Each episode's lines are a run of rowids, which FTS5 reads on their own.
        let mut stmt = conn.prepare_cached(&format!("{KEPT} WHERE rowid >= ?1 AND rowid < ?2 AND text LIKE ?3 ESCAPE '\\'"))?;
        let mut found = Vec::new();
        for id in episodes {
            let rows = stmt.query_map(params![id * PER_EPISODE, (id + 1) * PER_EPISODE, like], kept)?;
            for row in rows {
                found.push(row?);
            }
        }
        Ok(found)
    }

    /// An episode's line, with the one before it and the one after.
    fn around(&self, episode_id: i64, line: usize) -> Result<Vec<Kept>> {
        let at = episode_id * PER_EPISODE + line as i64;
        let first = (at - 1).max(episode_id * PER_EPISODE);
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(&format!("{KEPT} WHERE rowid >= ?1 AND rowid <= ?2 ORDER BY rowid"))?;
        let found = stmt.query_map(params![first, at + 1], kept)?.collect::<rusqlite::Result<Vec<Kept>>>()?;
        Ok(found)
    }
}

// ---- Putting lines in ----

/// What goes in the index for an episode's spoken lines and their saved words.
fn entries(lines: &[Line], uses: &[SavedUse]) -> Result<Vec<Entry>> {
    let mut out = Vec::with_capacity(lines.len());
    let mut uses = uses.iter().peekable();
    for (l, line) in lines.iter().enumerate() {
        let mut of_line: Vec<&SavedUse> = Vec::new();
        while let Some(u) = uses.next_if(|u| u.line <= l) {
            if u.line == l {
                of_line.push(u);
            }
        }
        let mut spans: Vec<(usize, usize)> = of_line.iter().map(|u| (u.from, u.to)).collect();
        if !spans.is_empty() {
            premine::with_endings(&line.text, &mut spans)?;
        }
        // The text is kept without its line breaks, so a word's bytes move back by those before it.
        let breaks = |at: usize| line.text.get(..at).map(|before| before.matches('\n').count());
        let words: Vec<Word> = of_line
            .iter()
            .zip(&spans)
            .filter_map(|(u, (from, to))| Some(Word { headword: u.headword.clone(), reading: u.reading.clone(), from: from - breaks(*from)?, to: to - breaks(*to)? }))
            .collect();
        out.push(Entry { words: pack(&words), text: line.text.replace('\n', ""), start: line.start, end: line.end });
    }
    Ok(out)
}

/// The state of the files an episode's lines are made from: each one's size and when it was saved.
fn stamp(backend: &Backend, episode_id: i64) -> String {
    let of = |path: std::path::PathBuf| match std::fs::metadata(path) {
        Ok(m) => {
            let saved = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis()).unwrap_or(0);
            format!("{}@{saved}", m.len())
        }
        Err(_) => "none".to_string(),
    };
    format!("{} {}", of(jobs::lines_path(backend, episode_id)), of(premine::words_path(backend, episode_id)))
}

fn index_episode(backend: &Backend, episode_id: i64) -> Result<()> {
    let stamp = stamp(backend, episode_id);
    if backend.lines.stamp(episode_id)?.as_deref() == Some(stamp.as_str()) {
        return Ok(());
    }
    let lines = premine::spoken_lines(backend, episode_id);
    let uses = premine::saved_uses(backend, episode_id, &lines);
    backend.lines.replace(episode_id, &entries(&lines, uses.as_deref().unwrap_or_default())?, &stamp, uses.is_some())
}

/// Puts an episode's spoken lines in the index as they are now, with their
/// words when those are saved. Nothing happens when it has them already.
pub fn index(backend: &Backend, episode_id: i64) {
    if let Err(e) = index_episode(backend, episode_id) {
        eprintln!("line search: episode {episode_id} not indexed: {e:#}");
    }
}

/// Brings the index up to date with the library, in the background: episodes
/// whose lines or words were saved while it wasn't looking go in, and those
/// that have left Telly come out.
pub fn start(backend: Arc<Backend>) {
    std::thread::Builder::new()
        .name("telly-lines".into())
        .spawn(move || {
            if let Err(e) = catch_up(&backend) {
                eprintln!("line search: not brought up to date: {e:#}");
            }
        })
        .ok();
}

fn catch_up(backend: &Backend) -> Result<()> {
    let here: HashSet<i64> = backend.db.all_episodes()?.iter().map(|e| e.id).collect();
    for id in backend.lines.episodes()? {
        if !here.contains(&id) {
            backend.lines.forget(id)?;
        }
    }
    for id in here {
        index(backend, id);
    }
    Ok(())
}

// ---- Finding lines ----

/// What to look for as a word: the text as typed, in hiragana (readings are
/// kept in it), and its dictionary form when it's a word in some other form.
fn forms(q: &str) -> Vec<String> {
    let mut forms = vec![q.to_string(), kata_to_hira(q)];
    forms.extend(premine::dictionary_form(q));
    forms.sort();
    forms.dedup();
    forms
}

/// A line found, and the bytes of its text that were looked for.
struct Hit {
    kept: Kept,
    words: Vec<Word>,
    from: usize,
    to: usize,
}

/// The start of a word that's also looked for as a name with an honorific
/// after it (フリーレン in フリーレン様), which Telly's character dictionary
/// makes one word. A single character would start too many other words.
fn name_prefix(q: &str) -> Option<&str> {
    (q.chars().count() >= 2).then_some(q)
}

/// Whether `headword` is `name` with an honorific after it: フリーレン様, ナツさん.
fn with_honorific(headword: &str, name: &str) -> bool {
    headword.strip_prefix(name).is_some_and(|rest| HONORIFIC_SUFFIXES.iter().any(|(term, _, _)| *term == rest))
}

/// Whether `q` is worth looking for in the lines' text. A single kana is in
/// nearly every line, and is never a word of its own.
fn by_text(q: &str) -> bool {
    q.chars().count() >= 2 || contains_kanji(q)
}

/// The lines among `kept` that have one of `forms` as a word, or `name` with an
/// honorific after it: the first such word is what's found.
fn word_hits(kept: Vec<Kept>, forms: &[String], name: Option<&str>) -> Vec<Hit> {
    kept.into_iter()
        .filter_map(|kept| {
            let words = unpack(&kept.words);
            let is_it = |w: &Word| w.is_one_of(forms) || name.is_some_and(|n| with_honorific(&w.headword, n));
            let (from, to) = words.iter().find(|w| is_it(w)).map(|w| (w.from, w.to))?;
            Some(Hit { kept, words, from, to })
        })
        .collect()
}

/// The lines a search for `q` finds: those with `q` as a word, in any of its
/// forms or as a name with an honorific, those of episodes whose words aren't
/// saved yet with `q` in their text, and those with `q` written outside every
/// saved word (a pronoun or a number). When no line has it as a word,
/// every line with `q` in its text (a phrase).
fn hits(index: &Index, q: &str) -> Result<Vec<Hit>> {
    let forms = forms(q);
    let name = name_prefix(q);
    let mut hits = word_hits(index.with_word(&forms, name)?, &forms, name);
    if !by_text(q) {
        return Ok(hits);
    }
    if hits.is_empty() {
        return Ok(text_hits(index.with_text(q, None)?, q));
    }
    let with_words = index.with_words()?;
    // A word premining keeps is a saved word wherever it's said, so only
    // episodes whose words aren't saved are read for it. One it doesn't keep
    // (a pronoun, a number, an interjection: 俺, 二人, ありがとう) is found by
    // its text too, written outside every saved word: otherwise a search for
    // 俺 finds only オラ and おれ, which Hachidori reads as 俺.
    let more = if crate::premine::dictionary_form(q).is_some() {
        let without: Vec<i64> = index.episodes()?.into_iter().filter(|e| !with_words.contains(e)).collect();
        if without.is_empty() { Vec::new() } else { text_hits(index.with_text(q, Some(&without))?, q) }
    } else {
        let found: HashSet<(i64, usize)> = hits.iter().map(|h| (h.kept.episode_id, h.kept.line)).collect();
        outside_words(index.with_text(q, None)?, q).into_iter().filter(|h| !found.contains(&(h.kept.episode_id, h.kept.line))).collect()
    };
    hits.extend(more);
    Ok(hits)
}

fn text_hits(kept: Vec<Kept>, q: &str) -> Vec<Hit> {
    kept.into_iter()
        .filter_map(|kept| {
            let from = kept.text.find(q)?;
            Some(Hit { words: unpack(&kept.words), from, to: from + q.len(), kept })
        })
        .collect()
}

/// The lines with `q` written outside every saved word of theirs, each at the
/// first such `q`: 後 in 後で, not in 最後.
fn outside_words(kept: Vec<Kept>, q: &str) -> Vec<Hit> {
    kept.into_iter()
        .filter_map(|kept| {
            let words = unpack(&kept.words);
            let from = kept.text.match_indices(q).map(|(at, _)| at).find(|&at| !words.iter().any(|w| w.from < at + q.len() && at < w.to))?;
            Some(Hit { words, from, to: from + q.len(), kept })
        })
        .collect()
}

/// How many of a line's other words you don't know: words that aren't the
/// one found, a name of the cast, a caption's note in brackets, or among
/// `known` by their headword or reading.
fn unknown(hit: &Hit, forms: &[String], known: &HashSet<String>, cast: &HashSet<String>) -> usize {
    let text = &hit.kept.text;
    let mut others: HashSet<&str> = HashSet::new();
    for w in &hit.words {
        let found = w.is_one_of(forms) || (w.from < hit.to && hit.from < w.to);
        let Some(written) = text.get(w.from..w.to) else { continue };
        let name = cast.contains(&w.headword) || cast.contains(written);
        let have = known.contains(&w.headword) || known.contains(&w.reading);
        if !found && !name && !have && !premine::bracketed(text, w.from) {
            others.insert(&w.headword);
        }
    }
    others.len()
}

/// A line found, for the Library.
#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Found {
    episode_id: i64,
    show_id: i64,
    number: i64,
    /// Which of the episode's spoken lines it is.
    line: usize,
    /// When it's said in the video, your nudges counted.
    start: f64,
    end: f64,
    /// The line around what was found, as it's written there.
    before: String,
    word: String,
    after: String,
    /// How many of its other words you don't know; nothing when its episode's words aren't saved yet.
    unknown: Option<usize>,
    /// Whether what was found is said: not sung, and not a caption's note in brackets.
    said: bool,
    /// Which of the seek bar's pictures shows it.
    picture: usize,
}

/// The lines a search finds across the library: those with `q` as a word, in
/// any of its forms, or else those with `q` in their text.
pub fn find(backend: &Backend, q: &str) -> Result<Value> {
    let q = q.trim();
    if !contains_japanese(q) {
        return Ok(json!({ "lines": [] }));
    }
    let forms = forms(q);
    let hits = hits(&backend.lines, q)?;
    let episodes: HashMap<i64, Episode> = backend.db.all_episodes()?.into_iter().map(|e| (e.id, e)).collect();
    let known = known::words(backend).known;
    let with_words = backend.lines.with_words()?;
    let mut casts: HashMap<i64, HashSet<String>> = HashMap::new();
    let mut found = Vec::with_capacity(hits.len());
    for hit in hits {
        let Some(e) = episodes.get(&hit.kept.episode_id) else { continue };
        let text = &hit.kept.text;
        let (Some(before), Some(word), Some(after)) = (text.get(..hit.from), text.get(hit.from..hit.to), text.get(hit.to..)) else { continue };
        if !casts.contains_key(&e.show_id) {
            let show = backend.db.show(e.show_id)?;
            casts.insert(e.show_id, show.map(|s| names::show_names(backend, &s).into_iter().map(|n| n.term).collect()).unwrap_or_default());
        }
        let nudge = e.subs_nudge_ms as f64 / 1000.0;
        found.push(Found {
            episode_id: e.id,
            show_id: e.show_id,
            number: e.number,
            line: hit.kept.line,
            start: hit.kept.start + nudge,
            end: hit.kept.end + nudge,
            before: before.to_string(),
            word: word.to_string(),
            after: after.to_string(),
            unknown: with_words.contains(&e.id).then(|| unknown(&hit, &forms, &known, &casts[&e.show_id])),
            said: premine::said(text, hit.from),
            picture: ((hit.kept.start + nudge).max(0.0) / media::SHEET_EVERY).round() as usize,
        });
    }
    let shows = best_of_each_show(&mut found);
    Ok(json!({ "lines": found, "shows": shows }))
}

/// How soon you can read a line, as the Library ranks them: one that's said,
/// then the fewest other words you don't know (none: i+1), then one whose
/// episode's words aren't saved yet.
fn reach(f: &Found) -> usize {
    (if f.said { 0 } else { 2_000_000 }) + f.unknown.unwrap_or(1_000_000)
}

/// Keeps the `PER_SHOW` lines of each show you can read soonest, in the order
/// the Library lists them; returns how many each show had.
fn best_of_each_show(found: &mut Vec<Found>) -> HashMap<i64, usize> {
    found.sort_by(|a, b| reach(a).cmp(&reach(b)).then(a.number.cmp(&b.number)).then(a.start.total_cmp(&b.start)));
    let mut totals: HashMap<i64, usize> = HashMap::new();
    found.retain(|f| {
        let n = totals.entry(f.show_id).or_insert(0);
        *n += 1;
        *n <= PER_SHOW
    });
    totals
}

// ---- A line's scene ----

/// The stretch of an episode to play for one of its lines: the line, with
/// the one before and the one after when they're said within `NEAR` of it.
/// Returns where it starts and ends, its lines, and which of them was asked for.
fn scene_of(around: &[Kept], line: usize) -> Option<(f64, f64, Vec<&Kept>, usize)> {
    let at = around.iter().position(|k| k.line == line)?;
    let asked = &around[at];
    let mut lines: Vec<&Kept> = Vec::new();
    if let Some(before) = at.checked_sub(1).map(|i| &around[i]).filter(|b| asked.start - b.end <= NEAR) {
        lines.push(before);
    }
    let which = lines.len();
    lines.push(asked);
    if let Some(after) = around.get(at + 1).filter(|a| a.start - asked.end <= NEAR) {
        lines.push(after);
    }
    let start = (lines[0].start - LEAD).max(0.0);
    let end = lines[lines.len() - 1].end + LEAD;
    Some((start, end, lines, which))
}

/// The scene around a line of an episode, for the Library to play.
pub fn scene(backend: &Backend, episode_id: i64, line: usize) -> Result<Value> {
    let e = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let around = backend.lines.around(episode_id, line)?;
    let (start, end, lines, which) = scene_of(&around, line).ok_or_else(|| anyhow!("That line isn’t in the episode’s subtitles any more"))?;
    let nudge = e.subs_nudge_ms as f64 / 1000.0;
    let lines: Vec<Value> = lines.iter().map(|k| json!({ "start": k.start + nudge, "end": k.end + nudge, "text": k.text })).collect();
    Ok(json!({
        "episodeId": e.id,
        "start": (start + nudge).max(0.0),
        "end": end + nudge,
        "lines": lines,
        "line": which,
        // As the player's, so what it has read of the video is there for Watch from here.
        "src": format!("/ep/{}?v={}-{}", e.id, e.media_plan, e.audio_track.unwrap_or(-1)),
        "listedAudio": jobs::stored_probe(backend, e.id).map(|p| jobs::listed_track(&e, &p)).unwrap_or(0),
        "playable": e.media_state == "ready",
    }))
}

/// One of the seek bar's pictures of an episode, cut from the sheet of them
/// (made when the episode was got ready).
pub fn picture(backend: &Backend, episode_id: i64, picture: usize) -> Result<Served> {
    let e = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let sheet = image::ImageReader::open(jobs::sheet_path(backend, e.show_id, e.id))?.with_guessed_format()?.decode().context("reading the seek bar’s pictures")?;
    let (columns, rows) = (media::SHEET_COLUMNS as u32, media::sheet_rows(e.duration) as u32);
    let (width, height) = (sheet.width() / columns, sheet.height() / rows);
    // As the seek bar does: a line said after the last picture has the last one.
    let picture = (picture as u32).min(columns * rows - 1);
    let (column, row) = (picture % columns, picture / columns);
    let mut bytes = Vec::new();
    sheet.crop_imm(column * width, row * height, width, height).to_rgb8().write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85))?;
    Ok(Served { bytes, mime: "image/jpeg", range: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: f64, end: f64, text: &str) -> Line {
        Line { start, end, text: text.into(), ruby: vec![], speaker: String::new(), sign: false }
    }

    fn word(line: usize, text: &str, surface: &str, headword: &str, reading: &str) -> SavedUse {
        let from = text.find(surface).unwrap();
        serde_json::from_value(json!({ "line": line, "from": from, "to": from + surface.len(), "headword": headword, "reading": reading, "rank": null })).unwrap()
    }

    /// An index with one episode (7), of three lines.
    fn index() -> Index {
        let dir = std::env::temp_dir().join(format!("telly-lines-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let index = Index::open(&dir.join("telly-lines.sqlite")).unwrap();
        let lines = [line(10.0, 12.0, "魔法の研究ばかりしてないで。"), line(13.0, 15.0, "ちゃんと食べないと\n倒れますよ。"), line(15.5, 17.0, "フリーレン様、あとで食べ物を。")];
        let uses = [
            word(0, &lines[0].text, "魔法", "魔法", "まほう"),
            word(0, &lines[0].text, "研究", "研究", "けんきゅう"),
            word(1, &lines[1].text, "ちゃんと", "ちゃんと", "ちゃんと"),
            word(1, &lines[1].text, "食べ", "食べる", "たべる"),
            word(1, &lines[1].text, "倒れ", "倒れる", "たおれる"),
            word(2, &lines[2].text, "食べ物", "食べ物", "たべもの"),
        ];
        index.replace(7, &entries(&lines, &uses).unwrap(), "now", true).unwrap();
        index
    }

    fn found(index: &Index, q: &str) -> Vec<(usize, String)> {
        hits(index, q).unwrap().iter().map(|h| (h.kept.line, h.kept.text[h.from..h.to].to_string())).collect()
    }

    #[test]
    fn a_word_finds_every_form_of_itself_and_marks_the_whole_form() {
        let index = index();
        // The dictionary form, its reading and another form all find the line, and not 食べ物.
        for q in ["食べる", "たべる", "タベル", "食べた", "食べられる"] {
            assert_eq!(found(&index, q), [(1, "食べない".to_string())], "{q}");
        }
        assert_eq!(found(&index, "食べ物"), [(2, "食べ物".to_string())]);
        // A word after a line break is marked where it is in the line as kept.
        assert_eq!(found(&index, "倒れる"), [(1, "倒れます".to_string())]);
    }

    #[test]
    fn what_isn_t_a_word_finds_lines_by_their_text() {
        let index = index();
        assert_eq!(found(&index, "フリーレン"), [(2, "フリーレン".to_string())]);
        // Across where the line broke.
        assert_eq!(found(&index, "食べないと倒れ"), [(1, "食べないと倒れ".to_string())]);
        assert_eq!(found(&index, "雪合戦"), []);
    }

    #[test]
    fn a_name_finds_itself_with_an_honorific_and_a_word_not_the_longer_words_it_starts() {
        let index = index();
        let lines = [line(1.0, 2.0, "フリーレン、何を食べる？"), line(3.0, 4.0, "フリーレン様が来た。"), line(5.0, 6.0, "旅行に行く。"), line(7.0, 8.0, "旅に出る。")];
        let uses = [
            word(0, &lines[0].text, "フリーレン", "フリーレン", "ふりーれん"),
            word(1, &lines[1].text, "フリーレン様", "フリーレン様", "ふりーれんさま"),
            word(2, &lines[2].text, "旅行", "旅行", "りょこう"),
            word(3, &lines[3].text, "旅", "旅", "たび"),
        ];
        index.replace(8, &entries(&lines, &uses).unwrap(), "now", true).unwrap();
        // Episode 7's フリーレン様 isn't a word there, though its words are saved: a name
        // premining doesn't keep is found where it's written, outside every saved word.
        let mut names = found(&index, "フリーレン");
        names.sort();
        assert_eq!(names, [(0, "フリーレン".to_string()), (1, "フリーレン様".to_string()), (2, "フリーレン".to_string())]);
        // 旅 finds 旅 alone: 旅行 is another word, not 旅 with an honorific.
        assert_eq!(found(&index, "旅"), [(3, "旅".to_string())]);
    }

    #[test]
    fn an_episode_whose_words_aren_t_saved_is_found_by_its_text_beside_the_words() {
        let index = index();
        index.replace(9, &entries(&[line(1.0, 2.0, "パンを食べる。"), line(3.0, 4.0, "もう食べた。")], &[]).unwrap(), "now", false).unwrap();
        let hits = hits(&index, "食べる").unwrap();
        let found: Vec<(i64, usize, &str)> = hits.iter().map(|h| (h.kept.episode_id, h.kept.line, &h.kept.text[h.from..h.to])).collect();
        // Its text has only the form asked for.
        assert_eq!(found, [(7, 1, "食べない"), (9, 0, "食べる")]);
    }

    #[test]
    fn a_word_premining_doesn_t_keep_is_found_where_it_s_written() {
        let index = index();
        // 俺 is a pronoun, so no saved word; Hachidori read オラ as 俺 in one line.
        let lines = [line(1.0, 2.0, "オラは強い"), line(3.0, 4.0, "俺が行く"), line(5.0, 6.0, "最後に、俺だ"), line(7.0, 8.0, "最後だ"), line(9.0, 10.0, "後で行く")];
        let uses = [
            word(0, &lines[0].text, "オラ", "俺", "おれ"),
            word(2, &lines[2].text, "最後", "最後", "さいご"),
            word(3, &lines[3].text, "最後", "最後", "さいご"),
            word(4, &lines[4].text, "後", "後", "あと"),
        ];
        index.replace(8, &entries(&lines, &uses).unwrap(), "now", true).unwrap();
        let found = found(&index, "俺");
        assert!(found.contains(&(0, "オラ".to_string())), "{found:?}");
        assert!(found.contains(&(1, "俺".to_string())), "{found:?}");
        assert!(found.contains(&(2, "俺".to_string())), "{found:?}");
        // 後, a word of its own, isn't found inside a saved 最後, though it's written there.
        let after: Vec<(i64, usize, String)> = found_in(&index, "後").into_iter().filter(|f| f.0 == 8).collect();
        assert_eq!(after, [(8, 4, "後".to_string())]);
    }

    fn found_in(index: &Index, q: &str) -> Vec<(i64, usize, String)> {
        hits(index, q).unwrap().iter().map(|h| (h.kept.episode_id, h.kept.line, h.kept.text[h.from..h.to].to_string())).collect()
    }

    #[test]
    fn a_single_kana_isn_t_looked_for_in_the_text() {
        let index = index();
        assert_eq!(found(&index, "の"), []);
        assert_eq!(found(&index, "研"), [(0, "研".to_string())]);
        assert_eq!(found(&index, "ばかり"), [(0, "ばかり".to_string())]);
    }

    #[test]
    fn keeps_the_lines_of_each_show_you_can_read_soonest() {
        let f = |show_id: i64, number: i64, unknown: Option<usize>, said: bool| Found {
            episode_id: number,
            show_id,
            number,
            line: 0,
            start: 0.0,
            end: 1.0,
            before: String::new(),
            word: String::new(),
            after: String::new(),
            unknown,
            said,
            picture: 0,
        };
        let mut found: Vec<Found> = (0..PER_SHOW as i64 + 20).map(|n| f(1, n, Some(3), true)).collect();
        found.push(f(1, 500, Some(0), true));
        found.push(f(1, 501, None, true));
        found.push(f(1, 502, Some(0), false));
        found.push(f(2, 1, Some(1), true));
        let totals = best_of_each_show(&mut found);
        assert_eq!(totals, HashMap::from([(1, PER_SHOW + 23), (2, 1)]));
        let one: Vec<&Found> = found.iter().filter(|l| l.show_id == 1).collect();
        assert_eq!(one.len(), PER_SHOW);
        // The i+1 line first; the rest by episode; one not yet scored and one sung don't make it.
        assert_eq!((one[0].number, one[1].number, one[PER_SHOW - 1].number), (500, 0, PER_SHOW as i64 - 2));
        assert!(found.iter().any(|l| l.show_id == 2));
    }

    #[test]
    fn counts_the_other_words_you_don_t_know() {
        let index = index();
        let forms = forms("食べる");
        let hits = word_hits(index.with_word(&forms, None).unwrap(), &forms, None);
        let none = HashSet::new();
        // ちゃんと and 倒れる are the others.
        assert_eq!(unknown(&hits[0], &forms, &none, &none), 2);
        let known: HashSet<String> = ["ちゃんと".to_string(), "たおれる".to_string()].into();
        assert_eq!(unknown(&hits[0], &forms, &known, &none), 0);
        let cast: HashSet<String> = ["倒れる".to_string()].into();
        assert_eq!(unknown(&hits[0], &forms, &none, &cast), 1);
    }

    #[test]
    fn an_episode_s_lines_are_replaced_and_forgotten() {
        let index = index();
        assert_eq!(index.stamp(7).unwrap().as_deref(), Some("now"));
        assert!(index.with_words().unwrap().contains(&7));
        // Lines whose words aren't saved: found by their text, and not among the episodes with words.
        index.replace(7, &entries(&[line(1.0, 2.0, "魔法だ。")], &[]).unwrap(), "later", false).unwrap();
        assert!(index.with_words().unwrap().is_empty());
        assert_eq!(found(&index, "食べる"), []);
        assert_eq!(found(&index, "魔法"), [(0, "魔法".to_string())]);
        index.forget(7).unwrap();
        assert_eq!(index.episodes().unwrap(), Vec::<i64>::new());
        assert_eq!(found(&index, "魔法"), []);
    }

    #[test]
    fn a_scene_is_the_line_with_those_said_close_by() {
        let index = index();
        let around = index.around(7, 1).unwrap();
        let (start, end, lines, which) = scene_of(&around, 1).unwrap();
        assert_eq!((start, end, lines.len(), which), (9.5, 17.5, 3, 1));
        // The first line has none before it, and a line said long after isn't part of the scene.
        let far = [Kept { line: 0, start: 10.0, end: 12.0, ..around[0].clone() }, Kept { line: 1, start: 40.0, end: 42.0, ..around[1].clone() }];
        let (start, end, lines, which) = scene_of(&far, 0).unwrap();
        assert_eq!((start, end, lines.len(), which), (9.5, 12.5, 1, 0));
    }
}
