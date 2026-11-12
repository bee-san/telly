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
#[derive(Clone, Copy, Debug, PartialEq)]
struct Word<'a> {
    headword: &'a str,
    reading: &'a str,
    from: usize,
    to: usize,
}

impl Word<'_> {
    fn is_one_of(&self, forms: &[String]) -> bool {
        forms.iter().any(|f| f == self.headword || f == self.reading)
    }
}

/// A line's words as the index keeps them. A word with a space in it has no
/// place there; no headword has one.
fn pack(words: &[Word]) -> String {
    let spaced = |s: &str| s.is_empty() || s.contains(char::is_whitespace);
    let rows: Vec<String> = words
        .iter()
        .filter(|w| !spaced(w.headword) && !w.reading.contains(char::is_whitespace))
        .map(|w| format!("{} {} {} {}", w.headword, if w.reading.is_empty() { w.headword } else { w.reading }, w.from, w.to))
        .collect();
    rows.join("\n")
}

/// A line's words, read from where the index keeps them as they're needed:
/// a common word's tens of thousands of lines have ten or so each.
fn unpack(words: &str) -> impl Iterator<Item = Word<'_>> {
    words.lines().filter_map(|row| {
        let mut parts = row.split(' ');
        let word = Word { headword: parts.next()?, reading: parts.next()?, from: parts.next()?.parse().ok()?, to: parts.next()?.parse().ok()? };
        parts.next().is_none().then_some(word)
    })
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

/// A line as put in, read from the ordinary table FTS5 keeps the lines in,
/// `lines_content AS content` (`lines`' rowid is its id, and words, text,
/// start_s and end_s its c0 to c3). Read through `lines`, each line found is
/// looked up there by a statement of FTS5's own, and each line scanned goes
/// through FTS5 as well: twice the work.
const KEPT: &str = "SELECT content.id, content.c0, content.c1, content.c2, content.c3";

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
        // FTS5 finds them, in rowid order, and the lines are read from their own table.
        let mut stmt = conn.prepare_cached(&format!("{KEPT} FROM lines CROSS JOIN lines_content AS content ON content.id = lines.rowid WHERE lines MATCH ?1 ORDER BY lines.rowid"))?;
        let found = stmt.query_map(params![asked.join(" OR ")], kept)?.collect::<rusqlite::Result<Vec<Kept>>>()?;
        Ok(found)
    }

    /// The lines with `text` in them, of every episode or only of `episodes`.
    /// Every episode's lines take about a sixth of a second for 850,000 lines,
    /// where a word takes a fiftieth of one: each line is read.
    fn with_text(&self, text: &str, episodes: Option<&[i64]>) -> Result<Vec<Kept>> {
        let conn = self.conn()?;
        // As it's written: what text_hits keeps of the lines found.
        let Some(episodes) = episodes else {
            let mut stmt = conn.prepare_cached(&format!("{KEPT} FROM lines_content AS content WHERE instr(content.c1, ?1) > 0 ORDER BY content.id"))?;
            let found = stmt.query_map(params![text], kept)?.collect::<rusqlite::Result<Vec<Kept>>>()?;
            return Ok(found);
        };
        // Each episode's lines are a run of rowids, read on their own.
        let mut stmt = conn.prepare_cached(&format!("{KEPT} FROM lines_content AS content WHERE content.id >= ?1 AND content.id < ?2 AND instr(content.c1, ?3) > 0 ORDER BY content.id"))?;
        let mut found = Vec::new();
        for id in episodes {
            let rows = stmt.query_map(params![id * PER_EPISODE, (id + 1) * PER_EPISODE, text], kept)?;
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
        let mut stmt = conn.prepare_cached(&format!("{KEPT} FROM lines_content AS content WHERE content.id >= ?1 AND content.id <= ?2 ORDER BY content.id"))?;
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
            .filter_map(|(u, (from, to))| Some(Word { headword: &u.headword, reading: &u.reading, from: from - breaks(*from)?, to: to - breaks(*to)? }))
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
            let is_it = |w: &Word| w.is_one_of(forms) || name.is_some_and(|n| with_honorific(w.headword, n));
            let (from, to) = unpack(&kept.words).find(is_it).map(|w| (w.from, w.to))?;
            Some(Hit { kept, from, to })
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
            Some(Hit { from, to: from + q.len(), kept })
        })
        .collect()
}

/// The lines with `q` written outside every saved word of theirs, each at the
/// first such `q`: 後 in 後で, not in 最後.
fn outside_words(kept: Vec<Kept>, q: &str) -> Vec<Hit> {
    kept.into_iter()
        .filter_map(|kept| {
            let spans: Vec<(usize, usize)> = unpack(&kept.words).map(|w| (w.from, w.to)).collect();
            let from = kept.text.match_indices(q).map(|(at, _)| at).find(|&at| !spans.iter().any(|&(from, to)| from < at + q.len() && at < to))?;
            Some(Hit { from, to: from + q.len(), kept })
        })
        .collect()
}

/// How many of a line's other words you don't know: words that aren't the
/// one found, a name of the cast, a caption's note in brackets, or among
/// `known` by their headword or reading.
fn unknown(hit: &Hit, forms: &[String], known: &HashSet<String>, cast: &HashSet<String>) -> usize {
    let text = &hit.kept.text;
    // Each line found is asked, so the cheaper questions go first: a line has
    // ten words or so, and seldom brackets.
    let brackets = text.contains(['（', '(']);
    let mut others: Vec<&str> = Vec::new();
    for w in unpack(&hit.kept.words) {
        let Some(written) = text.get(w.from..w.to) else { continue };
        let found = w.is_one_of(forms) || (w.from < hit.to && hit.from < w.to);
        if found || others.contains(&w.headword) {
            continue;
        }
        let name = cast.contains(w.headword) || cast.contains(written);
        let have = known.contains(w.headword) || known.contains(w.reading);
        if !name && !have && !(brackets && premine::bracketed(text, w.from)) {
            others.push(w.headword);
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
    // Every line is ranked, but only the best of each show is written out:
    // a common word is in tens of thousands of lines.
    let mut ranked = Vec::with_capacity(hits.len());
    for (at, hit) in hits.iter().enumerate() {
        let Some(e) = episodes.get(&hit.kept.episode_id) else { continue };
        let text = &hit.kept.text;
        if text.get(..hit.from).is_none() || text.get(hit.from..hit.to).is_none() || text.get(hit.to..).is_none() {
            continue;
        }
        if !casts.contains_key(&e.show_id) {
            let show = backend.db.show(e.show_id)?;
            casts.insert(e.show_id, show.map(|s| names::show_names(backend, &s).into_iter().map(|n| n.term).collect()).unwrap_or_default());
        }
        ranked.push(Ranked {
            at,
            show_id: e.show_id,
            number: e.number,
            start: hit.kept.start + e.subs_nudge_ms as f64 / 1000.0,
            unknown: with_words.contains(&e.id).then(|| unknown(hit, &forms, &known, &casts[&e.show_id])),
            said: premine::said(text, hit.from),
        });
    }
    let (best, shows) = best_of_each_show(ranked);
    let found: Vec<Found> = best
        .iter()
        .map(|r| {
            let (hit, e) = (&hits[r.at], &episodes[&hits[r.at].kept.episode_id]);
            let text = &hit.kept.text;
            Found {
                episode_id: e.id,
                show_id: e.show_id,
                number: e.number,
                line: hit.kept.line,
                start: r.start,
                end: hit.kept.end + e.subs_nudge_ms as f64 / 1000.0,
                before: text[..hit.from].to_string(),
                word: text[hit.from..hit.to].to_string(),
                after: text[hit.to..].to_string(),
                unknown: r.unknown,
                said: r.said,
                picture: (r.start.max(0.0) / media::SHEET_EVERY).round() as usize,
            }
        })
        .collect();
    Ok(json!({ "lines": found, "shows": shows }))
}

/// A line found, ranked: which of the hits it is, and what the Library orders by.
#[derive(Clone, Debug, PartialEq)]
struct Ranked {
    at: usize,
    show_id: i64,
    number: i64,
    /// When it's said, your nudges counted.
    start: f64,
    unknown: Option<usize>,
    said: bool,
}

/// How soon you can read a line, as the Library ranks them: one that's said,
/// then the fewest other words you don't know (none: i+1), then one whose
/// episode's words aren't saved yet.
fn reach(r: &Ranked) -> usize {
    (if r.said { 0 } else { 2_000_000 }) + r.unknown.unwrap_or(1_000_000)
}

/// The order the Library lists lines in: those you can read soonest, then by
/// episode and time; lines alike stay in the order they were found.
fn soonest(a: &Ranked, b: &Ranked) -> std::cmp::Ordering {
    reach(a).cmp(&reach(b)).then(a.number.cmp(&b.number)).then(a.start.total_cmp(&b.start)).then(a.at.cmp(&b.at))
}

/// The `PER_SHOW` lines of each show you can read soonest, in the order the
/// Library lists them, and how many each show had.
fn best_of_each_show(ranked: Vec<Ranked>) -> (Vec<Ranked>, HashMap<i64, usize>) {
    let mut of_show: HashMap<i64, Vec<Ranked>> = HashMap::new();
    for r in ranked {
        of_show.entry(r.show_id).or_default().push(r);
    }
    let totals = of_show.iter().map(|(show, lines)| (*show, lines.len())).collect();
    let mut best = Vec::new();
    for mut lines in of_show.into_values() {
        if lines.len() > PER_SHOW {
            lines.select_nth_unstable_by(PER_SHOW - 1, soonest);
            lines.truncate(PER_SHOW);
        }
        best.append(&mut lines);
    }
    best.sort_unstable_by(soonest);
    (best, totals)
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
    let sheet = std::fs::read(jobs::sheet_path(backend, e.show_id, e.id))?;
    let bytes = cut(&sheet, media::SHEET_COLUMNS as u32, media::sheet_rows(e.duration) as u32, picture)?;
    Ok(Served { bytes, mime: "image/jpeg", range: None })
}

/// Picture `picture` of a sheet of them `columns` across and `rows` down, as a JPEG.
fn cut(sheet: &[u8], columns: u32, rows: u32, picture: usize) -> Result<Vec<u8>> {
    // As the seek bar does: a line said after the last picture has the last one.
    let picture = (picture as u32).min(columns * rows - 1);
    let (column, row) = (picture % columns, picture / columns);
    let frame = jpeg_frame(sheet);
    // A JPEG is decoded top to bottom, so only as much of the sheet as the
    // picture needs is: on average half of it, which is most of the work.
    let top = frame.and_then(|f| f.cut_short(sheet, (row + 1) * (f.height / rows)));
    let pictures = match top {
        Some(top) => image::load_from_memory_with_format(&top, image::ImageFormat::Jpeg),
        None => image::load_from_memory(sheet),
    }
    .context("reading the seek bar’s pictures")?;
    // The pictures' size goes by the whole sheet, which may be taller than what was read of it.
    let (sheet_width, sheet_height) = frame.map_or((pictures.width(), pictures.height()), |f| (f.width, f.height));
    let (width, height) = (sheet_width / columns, sheet_height / rows);
    let mut bytes = Vec::new();
    pictures.crop_imm(column * width, row * height, width, height).to_rgb8().write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85))?;
    Ok(bytes)
}

/// What a baseline JPEG's header says of its frame: its size, where its height
/// is written, and how tall a row of its blocks is.
#[derive(Clone, Copy, Debug, PartialEq)]
struct JpegFrame {
    width: u32,
    height: u32,
    height_at: usize,
    block_rows: u32,
}

/// The frame of a baseline JPEG (as ffmpeg writes the seek bar's sheets) whose
/// one scan has every component, which is decoded top to bottom a row of
/// blocks at a time. Nothing for any other kind.
fn jpeg_frame(jpeg: &[u8]) -> Option<JpegFrame> {
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let (mut at, mut frame) = (2, None);
    loop {
        if *jpeg.get(at)? != 0xFF {
            return None;
        }
        // A marker may have more 0xFF before it.
        let mut marker = at + 1;
        while *jpeg.get(marker)? == 0xFF {
            marker += 1;
        }
        let length = u16::from_be_bytes([*jpeg.get(marker + 1)?, *jpeg.get(marker + 2)?]) as usize;
        let body = jpeg.get(marker + 3..marker + 1 + length)?;
        match jpeg[marker] {
            // Baseline and extended, Huffman-coded.
            0xC0 | 0xC1 => {
                let components = *body.get(5)? as usize;
                let sampling: Vec<u32> = (0..components).map(|c| body.get(7 + 3 * c).map(|s| u32::from(s & 0x0F))).collect::<Option<_>>()?;
                // One component is decoded a block at a time, whatever its sampling says.
                let tallest = if components == 1 { 1 } else { sampling.into_iter().max()? };
                frame = Some((
                    components,
                    JpegFrame {
                        width: u32::from(u16::from_be_bytes([*body.get(3)?, *body.get(4)?])),
                        height: u32::from(u16::from_be_bytes([*body.get(1)?, *body.get(2)?])),
                        height_at: marker + 4,
                        block_rows: 8 * tallest,
                    },
                ));
            }
            // Progressive, lossless or arithmetic-coded frames.
            0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            0xDA => {
                let (components, frame) = frame?;
                return (body.first() == Some(&(components as u8)) && frame.height > 0).then_some(frame);
            }
            0xD0..=0xD9 | 0x01 => return None,
            _ => {}
        }
        at = marker + 1 + length;
    }
}

impl JpegFrame {
    /// The JPEG cut short to what's decoded of it down to row `bottom` of its
    /// pixels, with a row of blocks after that row's: a row's colours are
    /// smoothed from the row below, as there's half as many rows of them.
    /// Nothing when that's the whole of it.
    fn cut_short(&self, jpeg: &[u8], bottom: u32) -> Option<Vec<u8>> {
        let height = (bottom.div_ceil(self.block_rows) + 1) * self.block_rows;
        if height >= self.height {
            return None;
        }
        let mut top = jpeg.to_vec();
        top[self.height_at..self.height_at + 2].copy_from_slice(&(height as u16).to_be_bytes());
        Some(top)
    }
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
    fn text_is_found_as_it_s_written() {
        let index = index();
        let lines = [line(1.0, 2.0, "全品50%オフ！"), line(3.0, 4.0, "50_50だね"), line(5.0, 6.0, "Abcの歌")];
        index.replace(8, &entries(&lines, &[]).unwrap(), "now", false).unwrap();
        // What LIKE takes for any characters, and other letter cases, are only themselves.
        assert_eq!(found(&index, "0%オフ"), [(0, "0%オフ".to_string())]);
        assert_eq!(found(&index, "%オ"), [(0, "%オ".to_string())]);
        assert_eq!(found(&index, "_50だ"), [(1, "_50だ".to_string())]);
        assert_eq!(found(&index, "0_5"), [(1, "0_5".to_string())]);
        assert_eq!(found(&index, "abcの"), []);
        assert_eq!(found(&index, "Abcの"), [(2, "Abcの".to_string())]);
        // Each line once, at the first place it's said.
        assert_eq!(found(&index, "べ"), []);
        let lines = [line(1.0, 2.0, "走って走って")];
        index.replace(9, &entries(&lines, &[]).unwrap(), "now", false).unwrap();
        let hits = hits(&index, "走って").unwrap();
        assert_eq!(hits.iter().map(|h| (h.kept.episode_id, h.from)).collect::<Vec<_>>(), [(9, 0)]);
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
        let r = |show_id: i64, number: i64, unknown: Option<usize>, said: bool| Ranked { at: 0, show_id, number, start: 0.0, unknown, said };
        let mut ranked: Vec<Ranked> = (0..PER_SHOW as i64 + 20).map(|n| r(1, n, Some(3), true)).collect();
        ranked.push(r(1, 500, Some(0), true));
        ranked.push(r(1, 501, None, true));
        ranked.push(r(1, 502, Some(0), false));
        ranked.push(r(2, 1, Some(1), true));
        // Found in reverse, so the order they're found in can't be what lists them.
        ranked.reverse();
        for (at, r) in ranked.iter_mut().enumerate() {
            r.at = at;
        }
        let (best, totals) = best_of_each_show(ranked.clone());
        assert_eq!(totals, HashMap::from([(1, PER_SHOW + 23), (2, 1)]));
        let one: Vec<&Ranked> = best.iter().filter(|l| l.show_id == 1).collect();
        assert_eq!(one.len(), PER_SHOW);
        // The i+1 line first; the rest by episode; one not yet scored and one sung don't make it.
        assert_eq!((one[0].number, one[1].number, one[PER_SHOW - 1].number), (500, 0, PER_SHOW as i64 - 2));
        assert!(best.iter().any(|l| l.show_id == 2));
        // The same lines in the same order as sorting every line and keeping each show's first ones:
        // lines alike (here, two said at once, the last place between them) stay in the order they were found.
        let last = ranked.iter().find(|l| l.show_id == 1 && l.number == PER_SHOW as i64 - 2).unwrap().clone();
        ranked.push(Ranked { at: ranked.len(), ..last });
        let mut all = ranked.clone();
        all.sort_by(|a, b| reach(a).cmp(&reach(b)).then(a.number.cmp(&b.number)).then(a.start.total_cmp(&b.start)));
        let mut kept: HashMap<i64, usize> = HashMap::new();
        all.retain(|l| {
            let n = kept.entry(l.show_id).or_insert(0);
            *n += 1;
            *n <= PER_SHOW
        });
        assert_eq!(best_of_each_show(ranked).0, all);
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
    fn a_word_said_twice_counts_once_and_one_in_brackets_not_at_all() {
        let text = "（ため息）ちゃんと食べて、ちゃんと寝て。";
        let at = |surface: &str, nth: usize| text.match_indices(surface).nth(nth).unwrap().0;
        let w = |surface: &'static str, nth: usize, headword: &'static str| Word { headword, reading: headword, from: at(surface, nth), to: at(surface, nth) + surface.len() };
        let words = [w("ため息", 0, "ため息"), w("ちゃんと", 0, "ちゃんと"), w("食べ", 0, "食べる"), w("ちゃんと", 1, "ちゃんと"), w("寝", 0, "寝る")];
        let kept = Kept { episode_id: 7, line: 0, words: pack(&words), text: text.to_string(), start: 0.0, end: 1.0 };
        let hit = Hit { kept, from: at("食べ", 0), to: at("食べ", 0) + "食べ".len() };
        let none = HashSet::new();
        // ちゃんと and 寝る: ため息 is a caption's note.
        assert_eq!(unknown(&hit, &forms("食べる"), &none, &none), 2);
        let known: HashSet<String> = ["寝る".to_string()].into();
        assert_eq!(unknown(&hit, &forms("食べる"), &known, &none), 1);
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

    /// Bits of a JPEG's coded picture, with a 0 after each 0xFF byte.
    #[derive(Default)]
    struct Bits {
        bytes: Vec<u8>,
        byte: u32,
        count: u32,
    }

    impl Bits {
        fn put(&mut self, value: u32, count: u32) {
            for i in (0..count).rev() {
                self.byte = (self.byte << 1) | ((value >> i) & 1);
                self.count += 1;
                if self.count == 8 {
                    self.bytes.push(self.byte as u8);
                    if self.byte == 0xFF {
                        self.bytes.push(0);
                    }
                    (self.byte, self.count) = (0, 0);
                }
            }
        }
    }

    /// A baseline JPEG with its colour at half the width and half the height
    /// (4:2:0), as ffmpeg makes the seek bar's sheets, of flat 8×8 blocks:
    /// `shade` gives each block's level by component (Y, Cb, Cr), column and row.
    fn flat_jpeg(width: u16, height: u16, shade: impl Fn(usize, usize, usize) -> u8) -> Vec<u8> {
        let mut jpeg = vec![0xFF, 0xD8];
        let mut segment = |marker: u8, body: &[u8]| {
            jpeg.extend([0xFF, marker]);
            jpeg.extend(((body.len() + 2) as u16).to_be_bytes());
            jpeg.extend(body);
        };
        segment(0xDB, &[[0].as_slice(), &[1; 64]].concat());
        let ([h0, h1], [w0, w1]) = (height.to_be_bytes(), width.to_be_bytes());
        segment(0xC0, &[8, h0, h1, w0, w1, 3, 1, 0x22, 0, 2, 0x11, 0, 3, 0x11, 0]);
        // A DC difference's size in four bits, and one AC code: the block's end, "0".
        let mut tables = vec![0x00, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        tables.extend(0..12);
        tables.extend([0x10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x00]);
        segment(0xC4, &tables);
        segment(0xDA, &[3, 1, 0x00, 2, 0x00, 3, 0x00, 0, 63, 0]);
        let mut bits = Bits::default();
        let mut last = [0i32; 3];
        for y in 0..usize::from(height).div_ceil(16) {
            for x in 0..usize::from(width).div_ceil(16) {
                for (c, bx, by) in [(0, 2 * x, 2 * y), (0, 2 * x + 1, 2 * y), (0, 2 * x, 2 * y + 1), (0, 2 * x + 1, 2 * y + 1), (1, x, y), (2, x, y)] {
                    let dc = 8 * (i32::from(shade(c, bx, by)) - 128);
                    let difference = dc - last[c];
                    last[c] = dc;
                    let size = 32 - difference.unsigned_abs().leading_zeros();
                    bits.put(size, 4);
                    bits.put((if difference < 0 { difference - 1 } else { difference }) as u32 & ((1 << size) - 1), size);
                    bits.put(0, 1);
                }
            }
        }
        while bits.count != 0 {
            bits.put(1, 1);
        }
        jpeg.extend(bits.bytes);
        jpeg.extend([0xFF, 0xD9]);
        jpeg
    }

    #[test]
    fn a_picture_cut_from_the_top_of_its_sheet_is_the_one_cut_from_all_of_it() {
        // 10 columns of pictures 16 wide, 12 rows of them 9 high: their edges fall inside rows of blocks.
        let shade = |c: usize, x: usize, y: usize| ((c * 7919 + x * 104_729 + y * 1_299_709).wrapping_mul(2_654_435_761) >> 13) as u8;
        let sheet = flat_jpeg(160, 108, shade);
        let frame = jpeg_frame(&sheet).unwrap();
        assert_eq!(frame, JpegFrame { width: 160, height: 108, height_at: frame.height_at, block_rows: 16 });
        // The first row of pictures needs the first two rows of blocks; the last, all of them.
        let top = frame.cut_short(&sheet, 9).unwrap();
        assert_eq!(image::load_from_memory(&top).unwrap().height(), 32);
        assert_eq!(frame.cut_short(&sheet, 108), None);
        let whole = image::load_from_memory(&sheet).unwrap();
        for picture in 0..125 {
            let p = picture.min(119) as u32;
            let mut expected = Vec::new();
            whole
                .crop_imm(p % 10 * 16, p / 10 * 9, 16, 9)
                .to_rgb8()
                .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut expected, 85))
                .unwrap();
            assert!(cut(&sheet, 10, 12, picture).unwrap() == expected, "picture {picture}");
        }
        // Not a JPEG that can be cut short: decoded whole.
        let mut png = Vec::new();
        whole.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert_eq!(jpeg_frame(&png), None);
        let mut expected = Vec::new();
        whole.crop_imm(16, 9, 16, 9).to_rgb8().write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut expected, 85)).unwrap();
        assert!(cut(&png, 10, 12, 11).unwrap() == expected);
    }
}
