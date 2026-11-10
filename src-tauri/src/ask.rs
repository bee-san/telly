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
use crate::util::now_ms;
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
    /// `model` is None for Claude Code's own default. `warm` asks in the warm session (short questions).
    fn start(&self, prompt: String, model: Option<String>, effort: &'static str, warm: bool, keep: impl FnOnce(&str) + Send + 'static) -> u64 {
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
            let question = claude::Ask { prompt: &prompt, model: model.as_deref(), effort: Some(effort), warm };
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

/// The model Settings picked: Haiku 5.5 unless it says otherwise, or None
/// ("default") for Claude Code's own default.
pub fn model(backend: &Backend) -> Option<String> {
    match backend.db.setting_str("claude.model").trim() {
        "" => Some(claude::DEFAULT_MODEL.to_string()),
        "default" => None,
        other => Some(other.to_string()),
    }
}

/// Explain's effort: low answers in about a third of the time.
pub const EFFORT: &str = "low";

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
    let id = backend.asks.start(prompt, model(backend), EFFORT, true, move |answer| {
        let _ = keeper.db.save_answer(episode_id, "line", index as i64, &kept_about, answer, "");
    });
    Ok(json!({ "ask": id, "line": about }))
}

/// A stretch of an episode before, to play in the recap.
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

/// A part of the story in a recap: what happens in it, and the episodes it covers.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoryArc {
    pub title: String,
    pub episodes: Vec<i64>,
    pub summary: String,
}

/// A recap: the story so far in its arcs, the episode just left, and the
/// moments to play, as (episode number, line number).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Recap {
    pub arcs: Vec<StoryArc>,
    pub last: String,
    pub moments: Vec<(i64, usize)>,
}

impl Recap {
    /// As plain text, for the next recap's "story before these".
    pub fn text(&self) -> String {
        let mut out: Vec<String> = self.arcs.iter().map(|a| format!("{}: {}", a.title, a.summary)).collect();
        if !self.last.is_empty() {
            out.push(format!("Last time: {}", self.last));
        }
        out.join("\n")
    }
}

/// How many episodes a recap covers: the one just left and the five before it.
const RECAP_EPISODES: usize = 6;

/// The prompt for "Previously on" before Episode `number`: the subtitles of
/// the episodes before it (`window`, oldest first, each line numbered), and the
/// recap shown before the first of them, for the story before.
pub fn recap_prompt(show_title: &str, number: i64, window: &[(i64, &[Line])], earlier: &str, names: &str, answer_in: &str) -> String {
    let episodes: Vec<String> = window
        .iter()
        .map(|(n, lines)| {
            let numbered: Vec<String> = spoken(lines).map(|(i, l)| format!("[{i}] {}", one_line(&l.text))).collect();
            format!("<episode number=\"{n}\">\n{}\n</episode>", numbered.join("\n"))
        })
        .collect();
    let first = window.first().map(|(n, _)| *n).unwrap_or(number - 1);
    let last = window.last().map(|(n, _)| *n).unwrap_or(number - 1);
    let span = if first == last { format!("Episode {last}") } else { format!("Episodes {first} to {last}") };
    let earlier = if earlier.trim().is_empty() {
        String::new()
    } else {
        format!("\n\nFor the story before Episode {first}, this was the recap shown before it:\n<earlier>\n{}\n</earlier>", earlier.trim())
    };
    format!(
        "I’m about to watch Episode {number} of {show_title}, an anime in Japanese. Here are the Japanese subtitles of {span}, \
         the episodes before it, each line numbered:{names}\n\n{body}{earlier}\n\n\
         Reply with only a JSON object, and no other text, shaped like this: \
         {{\"arcs\": [{{\"title\": \"…\", \"episodes\": [{first}, …], \"summary\": \"…\"}}], \"last\": \"…\", \"moments\": [{{\"episode\": {last}, \"line\": 12}}]}}.\n\
         \"arcs\": the story of these episodes cut into its arcs or parts, one to three, oldest first. Each has a short title saying \
         what happens in it, taken from the subtitles (never a name from elsewhere that could hint at what’s ahead), the episodes it \
         covers, and two or three sentences in {answer_in}. An arc can run on into Episode {last}.\n\
         \"last\": what happened in Episode {last}, the one I just left, in {answer_in}: three or four sentences, ending where it ended, \
         so I can pick up from there.\n\
         \"moments\": 4 to 6 lines, in story order, that best tell this stretch of the story when each is played as a short clip: \
         complete lines spoken at key moments, two or more from Episode {last}, the last near its end.\n\
         Write the characters’ names as AniList does. Keep it under 250 words in all. Use only the subtitles above, and don’t guess at \
         what comes next. If you recognise the show, still tell me only what the subtitles say: what you know of it from elsewhere \
         could spoil what’s ahead.",
        body = episodes.join("\n\n"),
    )
}

/// Claude's recap, from its answer (the JSON object, wherever it is in it).
pub fn parse_recap(answer: &str) -> Option<Recap> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    let value: Value = serde_json::from_str(answer.get(start..=end)?).ok()?;
    let text = |v: &Value| v.as_str().map(str::trim).unwrap_or("").to_string();
    let arcs: Vec<StoryArc> = value["arcs"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|arc| StoryArc {
                    title: text(&arc["title"]),
                    episodes: arc["episodes"].as_array().map(|e| e.iter().filter_map(Value::as_i64).collect()).unwrap_or_default(),
                    summary: text(&arc["summary"]),
                })
                .filter(|arc| !arc.summary.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let moments: Vec<(i64, usize)> = value["moments"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| match m {
                    Value::Object(_) => Some((m["episode"].as_i64()?, m["line"].as_u64()? as usize)),
                    Value::Array(pair) => Some((pair.first()?.as_i64()?, pair.get(1)?.as_u64()? as usize)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let recap = Recap { arcs, last: text(&value["last"]), moments: at_most(moments, MAX_MOMENTS) };
    (!recap.last.is_empty() || !recap.arcs.is_empty()).then_some(recap)
}

/// The most moments a recap plays: about 40 seconds.
const MAX_MOMENTS: usize = 6;

/// `n` of the moments, spread from the first to the last (the end matters most).
fn at_most<T: Clone>(moments: Vec<T>, n: usize) -> Vec<T> {
    if moments.len() <= n {
        return moments;
    }
    if n < 2 {
        return moments.into_iter().take(n).collect();
    }
    let last = moments.len() - 1;
    (0..n).map(|k| moments[(k * last + (n - 1) / 2) / (n - 1)].clone()).collect()
}

/// The episodes a recap before `episode` covers: up to six before it with
/// subtitles, oldest first. None when the episode just before has none.
fn window(backend: &Backend, episode: &Episode) -> Result<Vec<(Episode, Vec<Line>)>> {
    let episodes = backend.db.episodes(episode.show_id)?;
    let Some(i) = episodes.iter().position(|e| e.id == episode.id) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for e in episodes[..i].iter().rev() {
        if out.len() >= RECAP_EPISODES {
            break;
        }
        let lines = jobs::read_lines(backend, e.id);
        if spoken(&lines).count() >= 20 {
            out.push((e.clone(), lines));
        } else if out.is_empty() {
            return Ok(Vec::new());
        }
    }
    out.reverse();
    Ok(out)
}

/// What a recap was made from: the episodes and their subtitles.
fn recap_about(window: &[(Episode, Vec<Line>)]) -> String {
    let parts: Vec<String> = window
        .iter()
        .map(|(e, lines)| format!("{}:{}:{}", e.id, lines.len(), lines.iter().map(|l| l.text.chars().count()).sum::<usize>()))
        .collect();
    format!("arcs|{}", parts.join(","))
}

fn recap_view(window: &[(Episode, Vec<Line>)], recap: &Recap) -> Value {
    let mut clips = Vec::new();
    for &(number, line) in &recap.moments {
        let Some((e, lines)) = window.iter().find(|(e, _)| e.number == number) else { continue };
        for m in moments(lines, &[line]) {
            clips.push(json!({
                "start": m.start,
                "end": m.end,
                "lines": m.lines,
                "episodeId": e.id,
                "number": e.number,
                "src": format!("/ep/{}?v={}", e.id, e.media_plan),
                "playable": e.media_state == "ready",
                "nudgeMs": e.subs_nudge_ms,
            }));
        }
    }
    json!({
        "done": true,
        "recap": recap.text(),
        "arcs": recap.arcs,
        "last": recap.last,
        "lastNumber": window.last().map(|(e, _)| e.number),
        "moments": clips,
    })
}

/// Whether a recap can be made for `episode` (the episode before it has
/// subtitles), whether one is ready, and which episodes it covers.
pub fn recap_state(backend: &Backend, episode: &Episode) -> Value {
    let Ok(window) = window(backend, episode) else { return json!({ "possible": false }) };
    let (Some((first, _)), Some((last, _))) = (window.first(), window.last()) else { return json!({ "possible": false }) };
    let ready = backend.db.answer(episode.id, "recap", -1).ok().flatten().is_some_and(|a| a.about == recap_about(&window));
    json!({ "possible": true, "ready": ready, "beforeNumber": last.number, "fromNumber": first.number })
}

/// "Previously on" before `episode_id`: the kept recap, or an ask to follow
/// (when `start`), or nothing.
pub fn recap(backend: &Arc<Backend>, episode_id: i64, start: bool) -> Result<Value> {
    let episode = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let show = backend.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let window = window(backend, &episode)?;
    if window.is_empty() {
        return Ok(json!({ "done": true, "error": "There’s no episode before this one with subtitles to recap." }));
    }
    let about = recap_about(&window);
    if let Some(kept) = backend.db.answer(episode_id, "recap", -1)? {
        if kept.about == about {
            if let Ok(recap) = serde_json::from_str::<Recap>(&kept.text) {
                return Ok(recap_view(&window, &recap));
            }
        }
    }
    if !start {
        return Ok(json!({ "done": false }));
    }
    let id = start_recap(backend, &show, &episode, window, about);
    Ok(json!({ "ask": id }))
}

fn start_recap(backend: &Arc<Backend>, show: &Show, episode: &Episode, window: Vec<(Episode, Vec<Line>)>, about: String) -> u64 {
    // The recap shown before the first of these episodes, for the story before them.
    let earlier = window
        .first()
        .and_then(|(e, _)| backend.db.answer(e.id, "recap", -1).ok().flatten())
        .and_then(|a| serde_json::from_str::<Recap>(&a.text).ok().map(|r| r.text()))
        .unwrap_or_default();
    let text: String = window.iter().flat_map(|(_, lines)| lines.iter().map(|l| l.text.as_str())).collect();
    let numbered: Vec<(i64, &[Line])> = window.iter().map(|(e, lines)| (e.number, lines.as_slice())).collect();
    let prompt = recap_prompt(&show_title(show), episode.number, &numbered, &earlier, &names_line(backend, show, &text), answer_in(backend));
    let keeper = backend.clone();
    let episode_id = episode.id;
    backend.asks.start(prompt, model(backend), EFFORT, false, move |answer| {
        if let Some(recap) = parse_recap(answer) {
            let _ = keeper.db.save_answer(episode_id, "recap", -1, &about, &serde_json::to_string(&recap).unwrap_or_default(), "");
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
    let Ok(window) = window(backend, &next) else { return };
    if window.is_empty() {
        return;
    }
    let about = recap_about(&window);
    if backend.db.answer(next.id, "recap", -1).ok().flatten().is_some_and(|a| a.about == about) {
        return;
    }
    start_recap(backend, &show, &next, window, about);
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
    fn recaps_cover_the_episodes_before_in_arcs() {
        let lines: Vec<Line> = (0..30).map(|i| line(i as f64 * 3.0, i as f64 * 3.0 + 2.0, &format!("台詞{i}"))).collect();
        let window: Vec<(i64, &[Line])> = vec![(3, &lines[..]), (4, &lines[..])];
        let p = recap_prompt("『FAIRY TAIL』", 5, &window, "Lucy joined Fairy Tail.", "", "English");
        assert!(p.contains("<episode number=\"3\">\n[0] 台詞0") && p.contains("<episode number=\"4\">"));
        assert!(p.contains("Episodes 3 to 4") && p.contains("<earlier>\nLucy joined Fairy Tail.\n</earlier>"));
        assert!(p.contains("\"arcs\"") && p.contains("Episode 4, the one I just left"));
        let answer = "Sure:\n{\"arcs\": [{\"title\": \"The Lullaby flute\", \"episodes\": [3, 4], \"summary\": \"Erigor steals Lullaby.\"}], \"last\": \"Natsu wins.\", \"moments\": [{\"episode\": 3, \"line\": 2}, {\"episode\": 4, \"line\": 20}]}";
        let recap = parse_recap(answer).unwrap();
        assert_eq!(recap.arcs[0].title, "The Lullaby flute");
        assert_eq!(recap.arcs[0].episodes, vec![3, 4]);
        assert_eq!(recap.last, "Natsu wins.");
        assert_eq!(recap.moments, vec![(3, 2), (4, 20)]);
        assert!(recap.text().contains("The Lullaby flute: Erigor steals Lullaby.") && recap.text().contains("Last time: Natsu wins."));
        assert_eq!(parse_recap("no json here"), None);
        // Seven moments become six, keeping the first and the last.
        let seven = "{\"last\": \"r\", \"moments\": [[1, 3], [1, 40], [2, 90], [2, 150], [3, 200], [3, 260], [4, 330]]}";
        assert_eq!(parse_recap(seven).unwrap().moments, vec![(1, 3), (1, 40), (2, 90), (3, 200), (3, 260), (4, 330)]);
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
