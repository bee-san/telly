//! Finishing the cards you mine in Telly. Hachidori adds the note as it always
//! does (word, reading, definitions, sentence). When Anki Beacon (or polling)
//! says a note was just added while the player is open, Telly:
//!
//! 1. reads the note's Sentence and finds that line among those within 2
//!    minutes of the playhead, exactly first, then loosely;
//! 2. queues a job (in telly.sqlite, so it survives a restart) to cut the line
//!    from 250 ms before to 250 ms after (your call in Settings) as an animated
//!    AVIF and Opus audio;
//! 3. stores both in Anki's media folder and fills Picture, SentenceAudio and
//!    MiscInfo ("NARUTO · E04 · 12:31") where they're empty, then tags the note
//!    `telly` and `telly::naruto`.
//!
//! Notes that already have a picture and audio are left alone. AnkiConnect
//! can't change a note that's open in Anki's browser, so Telly checks its
//! change landed and tries again for a while if it didn't.

use crate::anki;
use crate::backend::Backend;
use crate::clip;
use crate::db::MineJob;
use crate::jobs;
use crate::subs::Line;
use crate::util::{clock, now_ms};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How far from the playhead a card's line can be.
const WINDOW: f64 = 120.0;
const MAX_TRIES: i64 = 20;

/// Something for a toast in the player.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: u64,
    /// working, done or failed.
    pub state: String,
    pub title: String,
    pub text: String,
    pub note_id: i64,
    pub job_id: i64,
}

#[derive(Default)]
pub struct Queue {
    /// When each job may next be tried.
    not_before: Mutex<HashMap<i64, Instant>>,
    wake: Mutex<u64>,
    signal: std::sync::Condvar,
}

impl Queue {
    pub fn poke(&self) {
        if let Ok(mut n) = self.wake.lock() {
            *n += 1;
        }
        self.signal.notify_all();
    }
}

/// The note fields Telly fills, from Settings (Kiku's and Lapis's names by default).
pub struct Fields {
    pub sentence: String,
    pub picture: String,
    pub audio: String,
    pub info: String,
}

pub fn fields(backend: &Backend) -> Fields {
    let get = |key: &str, default: &str| {
        let v = backend.db.setting_str(key);
        if v.trim().is_empty() { default.to_string() } else { v.trim().to_string() }
    };
    Fields {
        sentence: get("anki.field.sentence", "Sentence"),
        picture: get("anki.field.picture", "Picture"),
        audio: get("anki.field.audio", "SentenceAudio"),
        info: get("anki.field.info", "MiscInfo"),
    }
}

pub fn anki_url(backend: &Backend) -> String {
    let url = backend.db.setting_str("anki.url");
    if url.trim().is_empty() { anki::DEFAULT_URL.to_string() } else { url.trim().to_string() }
}

/// Letters and numbers only, for comparing a card's sentence with a line.
fn squash(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// How alike two texts are, 0 to 1 (shared character pairs).
fn dice(a: &str, b: &str) -> f64 {
    let pairs = |s: &str| {
        let chars: Vec<char> = s.chars().collect();
        chars.windows(2).map(|w| (w[0], w[1])).collect::<Vec<_>>()
    };
    let (pa, mut pb) = (pairs(a), pairs(b));
    if pa.is_empty() || pb.is_empty() {
        return if a == b && !a.is_empty() { 1.0 } else { 0.0 };
    }
    let total = pa.len() + pb.len();
    let mut shared = 0;
    for p in pa {
        if let Some(i) = pb.iter().position(|q| *q == p) {
            pb.swap_remove(i);
            shared += 1;
        }
    }
    2.0 * shared as f64 / total as f64
}

/// The line a card's sentence came from: among lines near `position`, the
/// exact one nearest the playhead, else the most alike.
pub fn match_line<'a>(lines: &'a [Line], sentence: &str, position: f64) -> Option<&'a Line> {
    let target = squash(sentence);
    if target.is_empty() {
        return None;
    }
    let near: Vec<&Line> = lines.iter().filter(|l| !l.sign && l.start >= position - WINDOW && l.start <= position + 5.0).collect();
    let distance = |l: &&Line| ((l.start + l.end) / 2.0 - position).abs();
    let equal = near.iter().filter(|l| squash(&l.text) == target).min_by(|a, b| distance(a).total_cmp(&distance(b)));
    if let Some(l) = equal {
        return Some(l);
    }
    // Loosely: a line that holds the whole sentence (Hachidori trimmed it)
    // counts fully; one the sentence holds counts for its share of it; else
    // how alike they are.
    let target_len = target.chars().count() as f64;
    let likeness = |l: &Line| {
        let text = squash(&l.text);
        if text.is_empty() {
            return 0.0;
        }
        let held = if text.contains(&target) {
            1.0
        } else if target.contains(&text) {
            text.chars().count() as f64 / target_len
        } else {
            0.0
        };
        held.max(dice(&text, &target))
    };
    near.into_iter()
        .map(|l| (likeness(l), l))
        .filter(|(score, _)| *score >= 0.5)
        .max_by(|a, b| a.0.total_cmp(&b.0).then(distance(&b.1).total_cmp(&distance(&a.1))))
        .map(|(_, l)| l)
}

/// The clip around a line: with 250 ms or 500 ms either side, or the line before it too.
pub fn clip_bounds(lines: &[Line], line: &Line, padding: &str) -> (f64, f64) {
    let (pad, with_previous) = match padding {
        "500" => (0.5, false),
        "prev" => (0.25, true),
        _ => (0.25, false),
    };
    let mut start = line.start;
    if with_previous {
        if let Some(prev) = lines.iter().filter(|l| !l.sign && l.end <= line.start + 0.01 && line.start - l.end < 8.0).next_back() {
            start = prev.start;
        }
    }
    let (start, end) = ((start - pad).max(0.0), line.end + pad);
    (start, end.min(start + clip::MAX_SECONDS))
}

/// Notes Beacon (or polling) just reported. Runs off the listener's thread.
pub fn notes_found(backend: &Backend, ids: Vec<i64>) {
    if let Err(e) = take_notes(backend, &ids) {
        backend.push_event("failed", "Couldn't finish the card", &format!("{e:#}"), ids.first().copied().unwrap_or(0), 0);
    }
}

fn take_notes(backend: &Backend, ids: &[i64]) -> Result<()> {
    let Some(now) = backend.now_playing() else { return Ok(()) };
    let url = anki_url(backend);
    let f = fields(backend);
    let notes = anki::notes_info(&url, ids)?;
    let lines = jobs::read_lines(backend, now.episode_id);
    let padding = backend.db.setting_str("clip.padding");
    for note in notes {
        if backend.db.has_mine_job_for(note.id)? {
            continue;
        }
        let filled = |name: &str| note.field(name).is_some_and(|v| !v.trim().is_empty());
        if filled(&f.picture) && filled(&f.audio) {
            continue;
        }
        if note.field(&f.picture).is_none() && note.field(&f.audio).is_none() {
            backend.push_event(
                "failed",
                "Card not finished",
                &format!("{} notes have no \"{}\" or \"{}\" field. Set the field names in Settings.", note.model, f.picture, f.audio),
                note.id,
                0,
            );
            continue;
        }
        let sentence = anki::plain_text(&note.field(&f.sentence).unwrap_or_default());
        let line = match_line(&lines, &sentence, now.position)
            .or_else(|| lines.iter().filter(|l| !l.sign && l.start <= now.position + 0.5).next_back())
            .cloned();
        let Some(line) = line else {
            backend.push_event("failed", "Card not finished", "This episode has no subtitle lines to cut a clip from.", note.id, 0);
            continue;
        };
        let (start, end) = clip_bounds(&lines, &line, &padding);
        let job = backend.db.add_mine_job(note.id, now.episode_id, start, end, &line.text)?;
        backend.push_event("working", "Adding picture and audio", &line.text.replace('\n', " "), note.id, job);
    }
    backend.cards.poke();
    Ok(())
}

/// M: puts the line on screen on your newest card (one made somewhere else).
pub fn add_line_to_last_card(backend: &Backend, episode_id: i64, start: f64, end: f64, text: &str) -> Result<i64> {
    let url = anki_url(backend);
    let newest = anki::notes_added_today(&url)?
        .into_iter()
        .max()
        .ok_or_else(|| anyhow!("No card was added to Anki today. Add one with Hachidori first."))?;
    let lines = jobs::read_lines(backend, episode_id);
    let padding = backend.db.setting_str("clip.padding");
    let line = Line { start, end, text: text.to_string(), ruby: vec![], speaker: String::new(), sign: false };
    let (s, e) = clip_bounds(&lines, &line, &padding);
    let job = backend.db.add_mine_job(newest, episode_id, s, e, text)?;
    backend.push_event("working", "Adding this line to your last card", &text.replace('\n', " "), newest, job);
    backend.cards.poke();
    Ok(job)
}

/// Tries a failed card again.
pub fn retry(backend: &Backend, job_id: i64) -> Result<()> {
    let job = backend.db.mine_job(job_id)?.ok_or_else(|| anyhow!("That card's job is gone"))?;
    backend.db.update_mine_job(job.id, "pending", "", 0)?;
    if let Ok(mut nb) = backend.cards.not_before.lock() {
        nb.remove(&job.id);
    }
    backend.push_event("working", "Trying the card again", &job.sentence.replace('\n', " "), job.note_id, job.id);
    backend.cards.poke();
    Ok(())
}

/// A name for tags and files: "Fairy Tail" → "fairy_tail".
pub fn slug(title: &str) -> String {
    let s: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let s = s.split('_').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("_");
    if s.is_empty() { "show".into() } else { s }
}

/// Works through queued cards, one at a time.
pub fn worker(backend: &Backend) {
    let mut seen = 0u64;
    loop {
        {
            let Ok(guard) = backend.cards.wake.lock() else { return };
            if *guard == seen {
                if let Ok((g, _)) = backend.cards.signal.wait_timeout(guard, Duration::from_secs(3)) {
                    seen = *g;
                }
            } else {
                seen = *guard;
            }
        }
        let jobs = backend.db.pending_mine_jobs().unwrap_or_default();
        for job in jobs {
            let due = backend.cards.not_before.lock().ok().and_then(|nb| nb.get(&job.id).copied()).is_none_or(|t| Instant::now() >= t);
            if !due {
                continue;
            }
            match finish(backend, &job) {
                Ok(()) => {
                    let _ = backend.db.update_mine_job(job.id, "done", "", job.tries + 1);
                    backend.push_event("done", "Card ready", &job.sentence.replace('\n', " "), job.note_id, job.id);
                }
                Err(e) => {
                    let tries = job.tries + 1;
                    let text = format!("{e:#}");
                    let waiting = text.contains("open in Anki") || text.contains("isn't reachable");
                    if waiting && tries < MAX_TRIES {
                        let _ = backend.db.update_mine_job(job.id, "pending", &text, tries);
                        if let Ok(mut nb) = backend.cards.not_before.lock() {
                            nb.insert(job.id, Instant::now() + Duration::from_secs((3 * tries as u64).min(30)));
                        }
                    } else {
                        let _ = backend.db.update_mine_job(job.id, "failed", &text, tries);
                        backend.push_event("failed", "Card not finished", &text, job.note_id, job.id);
                    }
                }
            }
        }
    }
}

fn finish(backend: &Backend, job: &MineJob) -> Result<()> {
    let episode = backend.db.episode(job.episode_id)?.ok_or_else(|| anyhow!("The episode is no longer in Telly"))?;
    let show = backend.db.show(episode.show_id)?.ok_or_else(|| anyhow!("The show is no longer in Telly"))?;
    let tools = backend.tools();
    let ffmpeg = tools.ffmpeg.ok_or_else(|| anyhow!("Telly needs ffmpeg to cut clips. Choose it in Settings."))?;
    if !tools.can_clip {
        return Err(anyhow!("This ffmpeg can't make AVIF and Opus (it needs libsvtav1 and libopus)"));
    }
    let (video, audio_index) = if episode.media_state == "ready" && !episode.play_path.is_empty() {
        (PathBuf::from(&episode.play_path), episode.audio_index)
    } else {
        (PathBuf::from(&episode.path), 0)
    };
    let url = anki_url(backend);
    let f = fields(backend);
    let note = anki::notes_info(&url, &[job.note_id])?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("The note was deleted in Anki"))?;
    let empty = |name: &str| note.field(name).is_some_and(|v| v.trim().is_empty());

    let base = format!("telly-{}-e{:02}-{}", slug(&show.title), episode.number, job.note_id);
    let clip = clip::cut(&ffmpeg, &video, audio_index, job.start, job.end, &backend.dirs.clips(), &base)?;
    let mut update = Map::new();
    if empty(&f.picture) {
        let name = anki::store_media_file(&url, &format!("{base}.avif"), &clip.picture)?;
        update.insert(f.picture.clone(), Value::String(format!("<img src=\"{name}\">")));
    }
    if empty(&f.audio) {
        let name = anki::store_media_file(&url, &format!("{base}.opus"), &clip.audio)?;
        update.insert(f.audio.clone(), Value::String(format!("[sound:{name}]")));
    }
    if empty(&f.info) {
        update.insert(f.info.clone(), Value::String(format!("{} · E{:02} · {}", show.title, episode.number, clock(job.start + 0.25))));
    }
    if !update.is_empty() {
        anki::update_fields(&url, job.note_id, &update)?;
        // AnkiConnect says nothing when the note is open in the browser; check it landed.
        let after = anki::notes_info(&url, &[job.note_id])?.into_iter().next();
        let landed = after.is_some_and(|n| update.keys().all(|k| n.field(k).is_some_and(|v| !v.trim().is_empty())));
        if !landed {
            return Err(anyhow!("The card is open in Anki's browser, so Telly can't change it yet. Close it and Telly will try again."));
        }
    }
    anki::add_tags(&url, &[job.note_id], &format!("telly telly::{}", slug(&show.title)))?;
    let _ = backend.log.card(job.note_id, &show.title, show.anilist_id, episode.number, now_ms());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: f64, text: &str) -> Line {
        Line { start, end: start + 2.0, text: text.into(), ruby: vec![], speaker: String::new(), sign: false }
    }

    #[test]
    fn matches_the_cards_sentence_to_a_line() {
        let lines = vec![
            line(700.0, "明日の試験、"),
            line(731.0, "明日の試験、絶対に合格してみせる。"),
            line(740.0, "えっ？"),
            line(1000.0, "明日の試験、絶対に合格してみせる。"),
        ];
        let found = match_line(&lines, "明日の試験、絶対に合格してみせる。", 745.0).unwrap();
        assert_eq!(found.start, 731.0);
        // Loosely: the card's sentence lost its punctuation and a character.
        let loose = match_line(&lines, "明日の試験絶対合格してみせる", 745.0).unwrap();
        assert_eq!(loose.start, 731.0);
        assert!(match_line(&lines, "全然違う文", 745.0).is_none());
    }

    #[test]
    fn pads_the_clip() {
        let lines = vec![line(10.0, "一"), line(12.5, "二")];
        assert_eq!(clip_bounds(&lines, &lines[1], "250"), (12.25, 14.75));
        assert_eq!(clip_bounds(&lines, &lines[1], "500"), (12.0, 15.0));
        assert_eq!(clip_bounds(&lines, &lines[1], "prev"), (9.75, 14.75));
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Fairy Tail"), "fairy_tail");
        assert_eq!(slug("NARUTO -ナルト-"), "naruto_ナルト");
    }
}
