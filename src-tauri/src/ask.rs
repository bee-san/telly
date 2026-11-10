//! What Telly asks Claude (claude.rs): "Explain this line" in the player, and
//! "Previously on", a recap of the episode before, with the moments of it to
//! play back. Answers are kept (db answers), so asking twice is instant.
//!
//! An answer is written while you watch: `start` returns an ask's id and the
//! player asks `progress` for the text so far, the way it ticks.

use crate::backend::Backend;
use crate::claude;
use crate::db::{Episode, Show};
use crate::jobs;
use crate::names;
use crate::subs::Line;
use crate::util::{clock, now_ms};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A question being answered.
pub struct Asking {
    text: Mutex<String>,
    done: AtomicBool,
    error: Mutex<String>,
    stop: Arc<AtomicBool>,
    started: Instant,
}

#[derive(Default)]
pub struct Asks {
    next: AtomicU64,
    running: Mutex<HashMap<u64, Arc<Asking>>>,
}

impl Asks {
    /// Answers `prompt` in the background; `keep` hears the whole answer.
    fn start(&self, prompt: String, model: String, effort: &'static str, keep: impl FnOnce(&str) + Send + 'static) -> u64 {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let asking = Arc::new(Asking {
            text: Mutex::new(String::new()),
            done: AtomicBool::new(false),
            error: Mutex::new(String::new()),
            stop: Arc::new(AtomicBool::new(false)),
            started: Instant::now(),
        });
        if let Ok(mut running) = self.running.lock() {
            // Answers nobody asked about for 10 minutes go.
            running.retain(|_, a| a.started.elapsed().as_secs() < 600 || !a.done.load(Ordering::SeqCst));
            running.insert(id, asking.clone());
        }
        std::thread::spawn(move || {
            let model = (!model.trim().is_empty()).then_some(model.trim().to_string());
            let question = claude::Ask { prompt: &prompt, model: model.as_deref(), effort: Some(effort) };
            let result = claude::ask(
                &question,
                |piece| {
                    if let Ok(mut t) = asking.text.lock() {
                        t.push_str(piece);
                    }
                },
                &asking.stop,
            );
            match result {
                Ok(answer) => {
                    keep(&answer);
                    if let Ok(mut t) = asking.text.lock() {
                        *t = answer;
                    }
                }
                Err(e) => {
                    if let Ok(mut err) = asking.error.lock() {
                        *err = format!("{e:#}");
                    }
                }
            }
            asking.done.store(true, Ordering::SeqCst);
        });
        id
    }

    /// The answer so far.
    pub fn progress(&self, id: u64) -> Value {
        let Some(asking) = self.running.lock().ok().and_then(|r| r.get(&id).cloned()) else {
            return json!({ "done": true, "error": "That answer isn’t here any more. Ask again." });
        };
        let text = asking.text.lock().map(|t| t.clone()).unwrap_or_default();
        let error = asking.error.lock().map(|e| e.clone()).unwrap_or_default();
        json!({ "text": text, "done": asking.done.load(Ordering::SeqCst), "error": error, "seconds": asking.started.elapsed().as_secs() })
    }

    pub fn stop(&self, id: u64) {
        if let Some(asking) = self.running.lock().ok().and_then(|r| r.get(&id).cloned()) {
            asking.stop.store(true, Ordering::SeqCst);
        }
    }
}

fn model(backend: &Backend) -> String {
    backend.db.setting_str("claude.model")
}

/// English, or simple Japanese for when you'd rather not read English.
fn answer_in(backend: &Backend) -> &'static str {
    if backend.db.setting_str("claude.language") == "ja" { "simple Japanese" } else { "English" }
}

fn spoken(lines: &[Line]) -> impl Iterator<Item = (usize, &Line)> {
    lines.iter().enumerate().filter(|(_, l)| !l.sign && !l.text.trim().is_empty())
}

fn one_line(text: &str) -> String {
    text.split('\n').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

fn show_title(show: &Show) -> String {
    if !show.title_native.is_empty() && show.title_native != show.title {
        format!("『{}』 ({})", show.title_native, show.title)
    } else {
        format!("『{}』", show.title)
    }
}

fn names_line(backend: &Backend, show: &Show, text: &str) -> String {
    let Some(cast) = names::cast_for(backend, show) else { return String::new() };
    let named = names::named_in(&cast, text, 25);
    if named.is_empty() { String::new() } else { format!("\nCharacters named in them, from AniList: {}.", named.join(", ")) }
}

/// The prompt for explaining `lines[index]`: the line, what came just before
/// it, and the next two lines.
pub fn explain_prompt(show_title: &str, number: i64, lines: &[Line], index: usize, names: &str, answer_in: &str) -> String {
    let line = &lines[index];
    let before: Vec<String> = spoken(&lines[..index])
        .filter(|(_, l)| l.start >= line.start - 90.0)
        .map(|(_, l)| one_line(&l.text))
        .collect();
    let before = &before[before.len().saturating_sub(12)..];
    let after: Vec<String> = spoken(&lines[index + 1..]).take(2).map(|(_, l)| one_line(&l.text)).collect();
    format!(
        "I’m watching {show_title}, Episode {number}, an anime in Japanese, with Japanese subtitles.{names}\n\n\
         The subtitles just before the line, oldest first:\n<before>\n{before}\n</before>\n\n\
         The line:\n<line>{line}</line>\n\n\
         The next subtitles:\n<after>\n{after}\n</after>\n\n\
         Explain the line for a learner, in {answer_in}. First a natural translation of it, on a line of its own. \
         Then, one per line, the words, grammar and expressions a learner might trip on, each as \"term (reading): explanation\", \
         with the reading in hiragana for words with kanji. Leave out what a beginner already knows. \
         Use the other subtitles only to understand the line. Keep it under 160 words.",
        before = before.join("\n"),
        line = one_line(&line.text),
        after = after.join("\n"),
    )
}

/// "Explain this line": a kept answer, or an ask to follow.
pub fn explain(backend: &Arc<Backend>, episode_id: i64, index: usize) -> Result<Value> {
    let episode = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let show = backend.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let lines = jobs::read_lines(backend, episode_id);
    let line = lines.get(index).ok_or_else(|| anyhow!("That line isn’t in the subtitles any more"))?;
    let about = one_line(&line.text);
    if let Some(kept) = backend.db.answer(episode_id, "line", index as i64)? {
        if kept.about == about {
            return Ok(json!({ "text": kept.text, "done": true, "line": about }));
        }
    }
    let context: String = lines[index.saturating_sub(12)..(index + 3).min(lines.len())].iter().map(|l| l.text.as_str()).collect();
    let prompt = explain_prompt(&show_title(&show), episode.number, &lines, index, &names_line(backend, &show, &context), answer_in(backend));
    let keeper = backend.clone();
    let kept_about = about.clone();
    let id = backend.asks.start(prompt, model(backend), "low", move |answer| {
        let _ = keeper.db.save_answer(episode_id, "line", index as i64, &kept_about, answer, "");
    });
    Ok(json!({ "ask": id, "line": about }))
}

/// A stretch of the episode before to play in the recap.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Moment {
    pub start: f64,
    pub end: f64,
    pub lines: Vec<Line>,
}

/// The stretches to play for the chosen lines: each line from just before it
/// starts, with the lines that run straight on from it, up to about 7 seconds.
pub fn moments(lines: &[Line], chosen: &[usize]) -> Vec<Moment> {
    let mut out: Vec<Moment> = Vec::new();
    let mut chosen: Vec<usize> = chosen.iter().copied().filter(|&i| i < lines.len()).collect();
    chosen.sort_unstable();
    chosen.dedup();
    for i in chosen {
        let first = &lines[i];
        let mut last = i;
        while last + 1 < lines.len() && last < i + 2 {
            let next = &lines[last + 1];
            if next.sign || next.start - lines[last].end > 1.2 || next.end - first.start > 7.0 {
                break;
            }
            last += 1;
        }
        let start = (first.start - 0.6).max(0.0);
        let end = lines[last].end + 0.5;
        match out.last_mut() {
            Some(prev) if start <= prev.end => {
                prev.end = prev.end.max(end);
                for l in &lines[i..=last] {
                    if !prev.lines.contains(l) {
                        prev.lines.push(l.clone());
                    }
                }
            }
            _ => out.push(Moment { start, end, lines: lines[i..=last].to_vec() }),
        }
    }
    out
}

/// The prompt for the recap shown before an episode: the whole episode
/// before it, numbered, and the recap shown before that one, for the story so far.
pub fn recap_prompt(show_title: &str, number: i64, before_number: i64, lines: &[Line], earlier: &str, names: &str, answer_in: &str) -> String {
    let numbered: Vec<String> = spoken(lines).map(|(i, l)| format!("[{i}] {} {}", clock(l.start), one_line(&l.text))).collect();
    let earlier = if earlier.trim().is_empty() {
        String::new()
    } else {
        format!("\n\nFor the story before that, this was the recap shown before Episode {before_number}:\n<earlier>\n{}\n</earlier>", earlier.trim())
    };
    format!(
        "I’m about to watch Episode {number} of {show_title}, an anime in Japanese. \
         Here are the Japanese subtitles of Episode {before_number}, the episode before it, numbered, with their times:{names}\n\n\
         <subtitles>\n{subtitles}\n</subtitles>{earlier}\n\n\
         Reply with only a JSON object, and no other text: {{\"recap\": \"…\", \"moments\": [12, 40, 87, 133, 201]}}.\n\
         \"recap\": a short \"previously on\" recap of Episode {before_number} in {answer_in}, so I can pick up where I left off: \
         who was in it, what happened, and how it ended. Write the characters' names as AniList does. \
         Start with the recap itself, with no title. Keep it under 120 words.\n\
         \"moments\": the numbers of 4 to 6 subtitles, in order, that best tell the episode’s story when each is played as a short clip: \
         complete lines, spoken at key moments, spread through the episode, the last one near its end.\n\
         Use only the subtitles above, and don’t guess at what comes next. If you recognise the show, still tell me only what \
         the subtitles say: what you know of it from elsewhere could spoil what’s ahead.",
        subtitles = numbered.join("\n"),
    )
}

/// Claude's recap, from its answer (the JSON object, wherever it is in it).
pub fn parse_recap(answer: &str) -> Option<(String, Vec<usize>)> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    let value: Value = serde_json::from_str(answer.get(start..=end)?).ok()?;
    let recap = value["recap"].as_str()?.trim().to_string();
    let moments: Vec<usize> = value["moments"].as_array().map(|a| a.iter().filter_map(Value::as_u64).map(|n| n as usize).collect()).unwrap_or_default();
    (!recap.is_empty()).then_some((recap, at_most(moments, MAX_MOMENTS)))
}

/// The most moments a recap plays: about 40 seconds.
const MAX_MOMENTS: usize = 6;

/// `n` of the moments, spread from the first to the last (the episode's end matters most).
fn at_most(moments: Vec<usize>, n: usize) -> Vec<usize> {
    if moments.len() <= n {
        return moments;
    }
    if n < 2 {
        return moments.into_iter().take(n).collect();
    }
    let last = moments.len() - 1;
    (0..n).map(|k| moments[(k * last + (n - 1) / 2) / (n - 1)]).collect()
}

/// The episode before `episode` in its show, if it has subtitles to recap.
fn before(backend: &Backend, episode: &Episode) -> Result<Option<(Episode, Vec<Line>)>> {
    let episodes = backend.db.episodes(episode.show_id)?;
    let Some(i) = episodes.iter().position(|e| e.id == episode.id) else { return Ok(None) };
    if i == 0 {
        return Ok(None);
    }
    let prev = episodes[i - 1].clone();
    let lines = jobs::read_lines(backend, prev.id);
    Ok((spoken(&lines).count() >= 20).then_some((prev, lines)))
}

/// What a recap was made from: the episode before and its subtitles.
fn recap_about(prev: &Episode, lines: &[Line]) -> String {
    let chars: usize = lines.iter().map(|l| l.text.chars().count()).sum();
    format!("{}:{}:{chars}", prev.id, lines.len())
}

fn recap_view(prev: &Episode, lines: &[Line], text: &str, chosen: &[usize]) -> Value {
    json!({
        "done": true,
        "recap": text,
        "moments": moments(lines, chosen),
        "from": {
            "id": prev.id,
            "number": prev.number,
            "src": format!("/ep/{}?v={}", prev.id, prev.media_plan),
            "playable": prev.media_state == "ready",
            "nudgeMs": prev.subs_nudge_ms,
        },
    })
}

/// Whether a recap can be made for `episode` (there's an episode before it
/// with subtitles), and whether one is ready.
pub fn recap_state(backend: &Backend, episode: &Episode) -> Value {
    let Ok(Some((prev, lines))) = before(backend, episode) else { return json!({ "possible": false }) };
    let ready = backend.db.answer(episode.id, "recap", -1).ok().flatten().is_some_and(|a| a.about == recap_about(&prev, &lines));
    json!({ "possible": true, "ready": ready, "beforeNumber": prev.number })
}

/// "Previously on" before `episode_id`: the kept recap, or an ask to follow
/// (when `start`), or nothing.
pub fn recap(backend: &Arc<Backend>, episode_id: i64, start: bool) -> Result<Value> {
    let episode = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let show = backend.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let Some((prev, lines)) = before(backend, &episode)? else {
        return Ok(json!({ "done": true, "error": "There’s no episode before this one with subtitles to recap." }));
    };
    let about = recap_about(&prev, &lines);
    if let Some(kept) = backend.db.answer(episode_id, "recap", -1)? {
        if kept.about == about {
            let chosen: Vec<usize> = serde_json::from_str(&kept.extra).unwrap_or_default();
            return Ok(recap_view(&prev, &lines, &kept.text, &chosen));
        }
    }
    if !start {
        return Ok(json!({ "done": false }));
    }
    let id = start_recap(backend, &show, &episode, prev, lines, about);
    Ok(json!({ "ask": id }))
}

fn start_recap(backend: &Arc<Backend>, show: &Show, episode: &Episode, prev: Episode, lines: Vec<Line>, about: String) -> u64 {
    // The recap shown before the episode before, for the story before it.
    let earlier = backend.db.answer(prev.id, "recap", -1).ok().flatten().map(|a| a.text).unwrap_or_default();
    let text: String = lines.iter().map(|l| l.text.as_str()).collect();
    let prompt = recap_prompt(&show_title(show), episode.number, prev.number, &lines, &earlier, &names_line(backend, show, &text), answer_in(backend));
    let keeper = backend.clone();
    let episode_id = episode.id;
    backend.asks.start(prompt, model(backend), "low", move |answer| {
        if let Some((recap, chosen)) = parse_recap(answer) {
            let _ = keeper.db.save_answer(episode_id, "recap", -1, &about, &recap, &serde_json::to_string(&chosen).unwrap_or_default());
        }
    })
}

/// The recap for an ask that has finished, from what it kept.
pub fn recap_answer(backend: &Arc<Backend>, episode_id: i64, ask: u64) -> Result<Value> {
    let progress = backend.asks.progress(ask);
    if !progress["done"].as_bool().unwrap_or(false) {
        return Ok(json!({ "done": false, "seconds": progress["seconds"] }));
    }
    if let Some(error) = progress["error"].as_str().filter(|e| !e.is_empty()) {
        return Ok(json!({ "done": true, "error": error }));
    }
    let answer = recap(backend, episode_id, false)?;
    if answer["recap"].is_string() {
        return Ok(answer);
    }
    Ok(json!({ "done": true, "error": "Claude’s recap didn’t come back in a form Telly could read. Try again." }))
}

pub fn recaps_on(backend: &Backend) -> bool {
    backend.db.setting_bool("recap.enabled", true)
}

/// Writes the recap for the episode after `episode_id` in the background, so
/// it's ready the next time you come back to the show. Called when you leave
/// the player having watched most of an episode.
pub fn prepare_next_recap(backend: &Arc<Backend>, episode_id: i64) {
    if !recaps_on(backend) || claude::find().is_none() {
        return;
    }
    let Ok(Some(episode)) = backend.db.episode(episode_id) else { return };
    let Ok(episodes) = backend.db.episodes(episode.show_id) else { return };
    let Some(next) = episodes.iter().skip_while(|e| e.id != episode_id).nth(1).cloned() else { return };
    let Ok(Some(show)) = backend.db.show(episode.show_id) else { return };
    let Ok(Some((prev, lines))) = before(backend, &next) else { return };
    let about = recap_about(&prev, &lines);
    if backend.db.answer(next.id, "recap", -1).ok().flatten().is_some_and(|a| a.about == about) {
        return;
    }
    start_recap(backend, &show, &next, prev, lines, about);
}

/// How long since the show was last open, in ms, from when `open_player` saw it.
pub fn away_ms(last_watched_at: Option<i64>) -> Option<i64> {
    last_watched_at.map(|t| now_ms() - t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: f64, end: f64, text: &str) -> Line {
        Line { start, end, text: text.into(), ruby: Vec::new(), speaker: String::new(), sign: false }
    }

    #[test]
    fn explains_a_line_with_what_came_before() {
        let lines = vec![line(1.0, 2.0, "ナツ！"), line(3.0, 4.0, "なんだよ"), line(5.0, 6.0, "腹減った…"), line(7.0, 8.0, "また？"), line(9.0, 10.0, "うん")];
        let p = explain_prompt("『FAIRY TAIL』", 3, &lines, 2, "", "English");
        assert!(p.contains("<line>腹減った…</line>"));
        assert!(p.contains("<before>\nナツ！\nなんだよ\n</before>"));
        assert!(p.contains("<after>\nまた？\nうん\n</after>"));
        assert!(p.contains("in English") && p.contains("Episode 3"));
    }

    #[test]
    fn recaps_number_the_lines_and_read_back_the_moments() {
        let lines: Vec<Line> = (0..30).map(|i| line(i as f64 * 3.0, i as f64 * 3.0 + 2.0, &format!("台詞{i}"))).collect();
        let p = recap_prompt("『FAIRY TAIL』", 5, 4, &lines, "Natsu fought.", "", "English");
        assert!(p.contains("[3] 0:09 台詞3") && p.contains("<earlier>\nNatsu fought.\n</earlier>") && p.contains("Episode 4"));
        let answer = "Here you go:\n{\"recap\": \"Lucy joined Fairy Tail.\", \"moments\": [2, 10, 29]}";
        assert_eq!(parse_recap(answer), Some(("Lucy joined Fairy Tail.".into(), vec![2, 10, 29])));
        assert_eq!(parse_recap("no json here"), None);
        // Seven moments become six, keeping the first and the last.
        let seven = "{\"recap\": \"r\", \"moments\": [3, 40, 90, 150, 200, 260, 330]}";
        assert_eq!(parse_recap(seven).unwrap().1, vec![3, 40, 90, 200, 260, 330]);
    }

    #[test]
    fn moments_run_on_through_close_lines_and_merge() {
        let lines = vec![line(10.0, 12.0, "a"), line(12.5, 14.0, "b"), line(30.0, 31.0, "c"), line(31.2, 32.0, "d")];
        let m = moments(&lines, &[0, 3, 2, 9]);
        let tenths = |x: f64| (x * 10.0).round() as i64;
        assert_eq!(m.len(), 2);
        assert_eq!((tenths(m[0].start), tenths(m[0].end), m[0].lines.len()), (94, 145, 2));
        assert_eq!((tenths(m[1].start), tenths(m[1].end), m[1].lines.len()), (294, 325, 2));
    }
}
