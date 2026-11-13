//! Stats: what you've watched, mined and learnt, all from what Telly already
//! keeps. Time, lines and characters come from the watching log (log.rs),
//! by the day each sitting ended, and cards from the cards Telly finished.
//! Episodes are those played to their end, and finished shows the library's
//! finishes and rewatches (db.rs). Words come from Telly's copy of your
//! known words (known.rs). A day is a day in local time, and the page adds
//! days up into its periods.

use crate::backend::Backend;
use crate::db::Episode;
use crate::known;
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// What was watched and mined on a day.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct Day {
    day: String,
    seconds: f64,
    /// Episodes played to their end that day, the first time or again.
    episodes: i64,
    lines: i64,
    chars: i64,
    cards: i64,
    /// The shows played.
    shows: Vec<i64>,
    /// The uses of words in the episodes played, and how many are of known
    /// words: the episodes' shares as they are now, not as they were then.
    uses: i64,
    known: i64,
}

/// The Stats page: every day anything was watched or mined, the known words
/// by the day their first card was made, and the shows finished or rewatched.
pub fn overview(backend: &Backend) -> Result<Value> {
    let mut by_show: HashMap<i64, Vec<Episode>> = HashMap::new();
    for e in backend.db.all_episodes()? {
        by_show.entry(e.show_id).or_default().push(e);
    }
    let scores = backend.db.word_scores().unwrap_or_default();
    // The log names an episode by its show and number.
    let score: HashMap<(i64, i64), (i64, i64)> =
        by_show.values().flatten().filter_map(|e| scores.get(&e.id).map(|s| ((e.show_id, e.number), (s.0, s.1)))).collect();

    let mut days: BTreeMap<String, Day> = BTreeMap::new();
    for p in backend.log.played(None)? {
        let d = entry(&mut days, &p.day);
        d.seconds += p.seconds;
        d.lines += p.lines;
        d.chars += p.chars;
        if !d.shows.contains(&p.show_id) {
            d.shows.push(p.show_id);
        }
        if let Some((uses, known)) = score.get(&(p.show_id, p.episode)) {
            d.uses += uses;
            d.known += known;
        }
    }
    for (day, cards) in backend.log.cards_by_day()? {
        entry(&mut days, &day).cards = cards;
    }
    for (day, episodes) in backend.db.episodes_finished_by_day()? {
        entry(&mut days, &day).episodes = episodes;
    }

    let (uses, known) = scores.values().fold((0, 0), |(u, k), s| (u + s.0, k + s.1));
    let words: Vec<Value> = by_day(&known::known_since(backend))?.into_iter().map(|(day, n)| json!({ "day": day, "n": n })).collect();

    let completions = backend.db.all_show_completions()?;
    let cards_by_show = backend.db.cards_per_show().unwrap_or_default();
    let mut shows = serde_json::Map::new();
    for show in backend.db.shows()? {
        if completions.iter().any(|c| c.0 == show.id) {
            let episodes = by_show.remove(&show.id).unwrap_or_default();
            let id = show.id;
            shows.insert(id.to_string(), serde_json::to_value(backend.card(show, &episodes, &cards_by_show, &scores)?)?);
        }
    }
    let finished: Vec<Value> = completions.into_iter().map(|(show, at, rewatch)| json!({ "show": show, "at": at, "rewatch": rewatch })).collect();

    Ok(json!({
        "days": days.into_values().collect::<Vec<_>>(),
        "words": words,
        "library": { "uses": uses, "known": known },
        "shows": shows,
        "finished": finished,
    }))
}

/// A show's own page in Stats: its card and finishes, what played each day,
/// each episode's share of known words, and the cards mined from it.
pub fn show(backend: &Backend, id: i64) -> Result<Value> {
    let show = backend.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let episodes = backend.db.episodes(id)?;
    let scores = backend.db.word_scores().unwrap_or_default();
    let rows: Vec<Value> = episodes
        .iter()
        .map(|e| json!({ "number": e.number, "words": scores.get(&e.id).map(|s| json!({ "uses": s.0, "known": s.1 })) }))
        .collect();
    let words = known::note_words(backend);
    let cards: Vec<Value> = backend
        .db
        .show_cards(id)?
        .into_iter()
        .map(|(note, episode, sentence)| json!({ "note": note, "episode": episode, "sentence": sentence, "word": words.get(&note) }))
        .collect();
    let card = backend.card(show, &episodes, &backend.db.cards_per_show().unwrap_or_default(), &scores)?;
    Ok(json!({
        "card": card,
        "completion": backend.db.show_completion(id)?,
        "episodes": rows,
        "played": backend.log.played(Some(id))?,
        "cards": cards,
    }))
}

fn entry<'a>(days: &'a mut BTreeMap<String, Day>, day: &str) -> &'a mut Day {
    days.entry(day.to_string()).or_insert_with(|| Day { day: day.to_string(), ..Default::default() })
}

/// How many of `times` (Unix ms) fall on each day, in local time as the log's days are.
fn by_day(times: &[i64]) -> Result<Vec<(String, i64)>> {
    let conn = rusqlite::Connection::open_in_memory()?;
    let mut stmt =
        conn.prepare("SELECT date(value / 1000, 'unixepoch', 'localtime') AS day, COUNT(*) FROM json_each(?1) GROUP BY day ORDER BY day")?;
    let rows = stmt.query_map([serde_json::to_string(times)?], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_times_by_local_day() {
        // Two on one day and one the next, wherever the computer is: a day apart at noon UTC can't share a local day.
        let noon = 1_794_571_200_000; // 13 November 2026, 12:00 UTC
        let days = by_day(&[noon, noon + 60_000, noon + 86_400_000]).unwrap();
        assert_eq!(days.iter().map(|d| d.1).collect::<Vec<_>>(), vec![2, 1]);
        assert!(days[0].0 < days[1].0);
    }
}
