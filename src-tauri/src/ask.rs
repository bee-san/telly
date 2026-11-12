//! What Telly asks Claude (claude.rs): "Explain this line" in the player, with
//! follow-up questions about it, and
//! "Previously on", a recap of the story so far, with the moments of it to
//! play back. Answers are kept (db answers), so asking twice is instant.
//!
//! Each episode's recap carries on from the one kept for the episode before,
//! so the story so far grows by one episode's subtitles at a time.
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
    /// The recaps being written, by what each is of, so one is never written twice at once.
    recaps: Mutex<HashMap<String, u64>>,
}

impl Asks {
    /// The ask writing the recap `key`, if it's still going.
    fn going(&self, key: &str) -> Option<u64> {
        let id = *self.recaps.lock().ok()?.get(key)?;
        let asking = self.running.lock().ok()?.get(&id).cloned()?;
        (!asking.done.load(Ordering::SeqCst)).then_some(id)
    }

    fn writing(&self, key: String, id: u64) {
        if let (Ok(mut recaps), Ok(running)) = (self.recaps.lock(), self.running.lock()) {
            recaps.retain(|_, id| running.contains_key(id));
            recaps.insert(key, id);
        }
    }

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

/// `lines[index]` for a prompt: the line, what came just before it, and the
/// next two lines.
fn line_in_context(show_title: &str, number: i64, lines: &[Line], index: usize, names: &str) -> String {
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
         The next subtitles:\n<after>\n{after}\n</after>\n\n",
        before = before.join("\n"),
        line = one_line(&line.text),
        after = after.join("\n"),
    )
}

/// The prompt for explaining `lines[index]`.
pub fn explain_prompt(show_title: &str, number: i64, lines: &[Line], index: usize, names: &str, answer_in: &str) -> String {
    format!(
        "{}Explain the line for a learner, in {answer_in}. First a natural translation of it, on a line of its own. \
         Then, one per line, the words, grammar and expressions a learner might trip on, each as \"term (reading): explanation\", \
         with the reading in hiragana for words with kanji. Leave out what a beginner already knows. \
         Use the other subtitles only to understand the line. Keep it under 160 words.",
        line_in_context(show_title, number, lines, index, names)
    )
}

/// A follow-up question that was asked about a line, and its answer.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Exchange {
    pub q: String,
    pub a: String,
}

/// The prompt for a follow-up question about `lines[index]`. It carries the
/// line, its explanation and the questions before, so it can be asked in any
/// session: a kept explanation never went through the warm one.
pub fn follow_up_prompt(context: &str, explanation: &str, thread: &[Exchange], question: &str, answer_in: &str) -> String {
    let earlier: String = thread
        .iter()
        .map(|e| format!("I then asked:\n<question>{}</question>\nYou answered:\n<answer>\n{}\n</answer>\n\n", e.q.trim(), e.a.trim()))
        .collect();
    format!(
        "{context}You explained the line to me like this:\n<explanation>\n{}\n</explanation>\n\n{earlier}\
         Now I ask:\n<question>{}</question>\n\n\
         Answer the question in {answer_in}, for a learner, in under 120 words. Use the other subtitles only to understand the line.",
        explanation.trim(),
        question.trim(),
    )
}

/// A follow-up question about an explained line: an ask to follow. Asked in the warm session; not kept.
pub fn follow_up(backend: &Arc<Backend>, episode_id: i64, index: usize, explanation: &str, thread: &[Exchange], question: &str) -> Result<Value> {
    let episode = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let show = backend.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let lines = jobs::read_lines(backend, episode_id);
    if index >= lines.len() {
        return Err(anyhow!("That line isn’t in the subtitles any more"));
    }
    let near: String = lines[index.saturating_sub(12)..(index + 3).min(lines.len())].iter().map(|l| l.text.as_str()).collect();
    let context = line_in_context(&show_title(&show), episode.number, &lines, index, &names_line(backend, &show, &near));
    let prompt = follow_up_prompt(&context, explanation, thread, question, answer_in(backend));
    let id = backend.asks.start(prompt, model(backend), EFFORT, true, |_| {});
    Ok(json!({ "ask": id }))
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
    pub from: i64,
    pub to: i64,
    pub summary: String,
}

/// A line to play in a recap, and what's happening at it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pick {
    pub episode: i64,
    pub line: usize,
    pub caption: String,
}

/// A recap: the whole story so far in its arcs, the episode just left (or the
/// part of this one that's been watched), and the moments to play.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Recap {
    pub arcs: Vec<StoryArc>,
    pub last: String,
    pub moments: Vec<Pick>,
}

impl Recap {
    /// As plain text.
    pub fn text(&self) -> String {
        let mut out: Vec<String> = self.arcs.iter().map(|a| format!("{}: {}", a.title, a.summary)).collect();
        if !self.last.is_empty() {
            out.push(format!("Last time: {}", self.last));
        }
        out.join("\n")
    }
}

/// The most episodes a recap reads the subtitles of. The story before them
/// comes from the recap kept for the first of them.
const RECAP_EPISODES: usize = 6;

/// The prompt for "Previously on" before Episode `number`: the subtitles of
/// the episodes it reads (`window`, oldest first, each line numbered), and the
/// recap kept for the first of them (`earlier`), which it carries on from.
/// `resuming` is for coming back part-way through Episode `number`: it's the
/// last of the window, as far as it was watched.
pub fn recap_prompt(show_title: &str, number: i64, window: &[(i64, &[Line])], earlier: Option<&Recap>, resuming: bool, names: &str, answer_in: &str) -> String {
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
    let opening = if resuming {
        format!(
            "I’m coming back to Episode {number} of {show_title}, an anime in Japanese, part-way through it. Here are the Japanese \
             subtitles of {span}, each line numbered, with Episode {number} only as far as I watched:{names}"
        )
    } else {
        format!(
            "I’m about to watch Episode {number} of {show_title}, an anime in Japanese. Here are the Japanese subtitles of {span}, \
             from before it, each line numbered:{names}"
        )
    };
    let (before, carry_arcs, carry_moments) = match earlier {
        Some(recap) => (
            format!(
                "\n\nThe story before Episode {first}, as you told it to me then, with the moments you chose:\n<earlier>\n{}\n</earlier>",
                serde_json::to_string(recap).unwrap_or_default()
            ),
            " Carry on from the earlier story: keep its arcs, run the latest one on or add a new one, and fold the oldest together \
             when there would be more than five."
                .to_string(),
            format!(" For the story before Episode {first}, keep the best of the earlier moments, with the episode and line they had."),
        ),
        None => (String::new(), String::new(), String::new()),
    };
    let (last_is, last_end) = if resuming {
        (
            format!("what has happened so far in Episode {last}, in {answer_in}: two to four sentences, ending where I stopped, so I can pick up from there"),
            "where I stopped",
        )
    } else {
        (
            format!("what happened in Episode {last}, the one I just left, in {answer_in}: three or four sentences, ending where it ended, so I can pick up from there"),
            "its end",
        )
    };
    format!(
        "{opening}\n\n{body}{before}\n\n\
         Reply with only a JSON object, and no other text, shaped like this: \
         {{\"arcs\": [{{\"title\": \"…\", \"from\": {first}, \"to\": {last}, \"summary\": \"…\"}}], \"last\": \"…\", \
         \"moments\": [{{\"episode\": {last}, \"line\": 12, \"caption\": \"…\"}}]}}.\n\
         \"arcs\": the whole story so far, cut into its arcs or parts, oldest first, five at most.{carry_arcs} Each has a short title \
         saying what happens in it, taken from the subtitles (never a name from elsewhere that could hint at what’s ahead), the first \
         and last episode it covers (\"from\" and \"to\"), and two or three sentences in {answer_in}. An arc can run on into Episode {last}.\n\
         \"last\": {last_is}.\n\
         \"moments\": 4 to 6 lines, in story order, that best tell the story when each is played as a short clip: complete lines \
         spoken at key moments, two or more from Episode {last}, the last near {last_end}.{carry_moments} Each has a \"caption\" in \
         {answer_in}: one sentence of under 15 words saying what’s happening at that moment, for someone who has forgotten it.\n\
         Write the characters’ names as AniList does. Keep it under 350 words in all. Use only the subtitles and the earlier story \
         above, and don’t guess at what comes next. If you recognise the show, still tell me only what they say: what you know of \
         it from elsewhere could spoil what’s ahead.",
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
                    from: arc["from"].as_i64().unwrap_or(0),
                    to: arc["to"].as_i64().unwrap_or(0),
                    summary: text(&arc["summary"]),
                })
                .filter(|arc| !arc.summary.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let moments: Vec<Pick> = value["moments"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| Some(Pick { episode: m["episode"].as_i64()?, line: m["line"].as_u64()? as usize, caption: text(&m["caption"]) }))
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

/// What a recap is made from.
#[derive(Default)]
struct Source {
    /// The episodes it reads the subtitles of, oldest first.
    episodes: Vec<(Episode, Vec<Line>)>,
    /// The recap kept for the first of them: the story before.
    earlier: Option<Recap>,
}

/// Enough lines to tell a story from.
fn tells(lines: &[Line]) -> bool {
    spoken(lines).count() >= 20
}

/// What the recap before `episode` is made from: the episodes before it with
/// subtitles, back to the nearest one with a recap kept (which tells the story
/// before), six at most. With `at`, `episode` itself is the last of them, as
/// far as `at` seconds in. Nothing when the last of them has too few lines.
fn source(backend: &Backend, episode: &Episode, at: Option<f64>) -> Result<Source> {
    let all = backend.db.episodes(episode.show_id)?;
    let Some(i) = all.iter().position(|e| e.id == episode.id) else { return Ok(Source::default()) };
    let mut source = Source::default();
    if let Some(at) = at {
        let mut lines = jobs::read_lines(backend, episode.id);
        let nudge = episode.subs_nudge_ms as f64 / 1000.0;
        lines.truncate(lines.iter().take_while(|l| l.start + nudge < at).count());
        if !tells(&lines) {
            return Ok(source);
        }
        source.episodes.push((episode.clone(), lines));
        source.earlier = kept(backend, episode.id);
    }
    for e in all[..i].iter().rev() {
        if source.earlier.is_some() || source.episodes.len() >= RECAP_EPISODES {
            break;
        }
        let lines = jobs::read_lines(backend, e.id);
        if tells(&lines) {
            source.episodes.push((e.clone(), lines));
            source.earlier = kept(backend, e.id);
        } else if source.episodes.is_empty() {
            return Ok(source);
        }
    }
    source.episodes.reverse();
    Ok(source)
}

/// How `about` starts for a recap that tells the whole story so far.
const ABOUT: &str = "story|";

/// One episode's subtitles, in what a recap was made from.
fn about_part(id: i64, lines: &[Line]) -> String {
    format!("{id}:{}:{}", lines.len(), lines.iter().map(|l| l.text.chars().count()).sum::<usize>())
}

/// What a recap was made from: the episodes and their subtitles.
fn recap_about(source: &Source) -> String {
    let parts: Vec<String> = source.episodes.iter().map(|(e, lines)| about_part(e.id, lines)).collect();
    format!("{ABOUT}{}", parts.join(","))
}

/// The recap kept for before the episode `id`, while the subtitles it was made from are as they were.
fn kept(backend: &Backend, id: i64) -> Option<Recap> {
    let answer = backend.db.answer(id, "recap", -1).ok().flatten()?;
    let same = answer.about.strip_prefix(ABOUT)?.split(',').all(|part| {
        let of: Option<i64> = part.split(':').next().and_then(|id| id.parse().ok());
        of.is_some_and(|of| about_part(of, &jobs::read_lines(backend, of)) == part)
    });
    if !same {
        return None;
    }
    serde_json::from_str(&answer.text).ok()
}

/// "recap" before an episode, "resume" part-way through it.
fn kind(at: Option<f64>) -> &'static str {
    if at.is_some() { "resume" } else { "recap" }
}

fn recap_view(backend: &Backend, show_id: i64, source: &Source, recap: &Recap) -> Result<Value> {
    let all = backend.db.episodes(show_id)?;
    let mut clips = Vec::new();
    // The clip before: its episode, start and end.
    let mut before: Option<(i64, f64, f64)> = None;
    for pick in &recap.moments {
        let Some(e) = all.iter().find(|e| e.number == pick.episode) else { continue };
        // An episode this recap read is played as far as it read it.
        let read;
        let lines = match source.episodes.iter().find(|(s, _)| s.id == e.id) {
            Some((_, lines)) => lines,
            None => {
                read = jobs::read_lines(backend, e.id);
                &read
            }
        };
        // Played where its lines are heard: with the episode's nudge, as the player shows them.
        let nudge = e.subs_nudge_ms as f64 / 1000.0;
        for m in moments(lines, &[pick.line]) {
            let (mut start, end) = ((m.start + nudge).max(0.0), m.end + nudge);
            // Two lines picked close together: the second's clip carries on from where the first's
            // ended, rather than playing its lines again, and goes if the first played all of it.
            if let Some((episode, from, to)) = before {
                if episode == e.id && start < to && end > from {
                    if end <= to {
                        continue;
                    }
                    start = to;
                }
            }
            before = Some((e.id, start, end));
            clips.push(json!({
                "start": start,
                "end": end,
                "lines": m.lines,
                "caption": pick.caption,
                "episodeId": e.id,
                "number": e.number,
                "src": format!("/ep/{}?v={}", e.id, e.media_plan),
                "listedAudio": jobs::stored_probe(backend, e.id).map(|p| jobs::listed_track(e, &p)).unwrap_or(0),
                "playable": e.media_state == "ready",
                "nudgeMs": e.subs_nudge_ms,
            }));
        }
    }
    Ok(json!({
        "done": true,
        "recap": recap.text(),
        "arcs": recap.arcs,
        "last": recap.last,
        "lastNumber": source.episodes.last().map(|(e, _)| e.number),
        "moments": clips,
    }))
}

/// Whether a recap can be made for `episode` (before it, or with `at`, up to
/// there in it), and whether it's written.
pub fn recap_state(backend: &Backend, episode: &Episode, at: Option<f64>) -> Value {
    let Ok(source) = source(backend, episode, at) else { return json!({ "possible": false }) };
    if source.episodes.is_empty() {
        return json!({ "possible": false });
    }
    let ready = backend.db.answer(episode.id, kind(at), -1).ok().flatten().is_some_and(|a| a.about == recap_about(&source));
    json!({ "possible": true, "ready": ready })
}

/// Whether `episode` would be picked up part-way through, `position` seconds in.
pub fn resuming(episode: &Episode, position: f64) -> bool {
    episode.watched_at.is_none() && position > 30.0 && (episode.duration <= 0.0 || position < episode.duration * 0.95)
}

/// "Previously on" before `episode_id` (or with `at`, up to there in it): the
/// kept recap, or an ask to follow (when `start`), or nothing.
pub fn recap(backend: &Arc<Backend>, episode_id: i64, start: bool, at: Option<f64>) -> Result<Value> {
    let episode = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let show = backend.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let source = source(backend, &episode, at)?;
    if source.episodes.is_empty() {
        return Ok(json!({ "done": true, "error": "There’s no episode before this one with subtitles to recap." }));
    }
    let about = recap_about(&source);
    if let Some(kept) = backend.db.answer(episode_id, kind(at), -1)? {
        if kept.about == about {
            if let Ok(recap) = serde_json::from_str::<Recap>(&kept.text) {
                return recap_view(backend, show.id, &source, &recap);
            }
        }
    }
    if !start {
        return Ok(json!({ "done": false }));
    }
    let key = format!("{episode_id}|{}|{about}", kind(at));
    if let Some(id) = backend.asks.going(&key) {
        return Ok(json!({ "ask": id }));
    }
    let text: String = source.episodes.iter().flat_map(|(_, lines)| lines.iter().map(|l| l.text.as_str())).collect();
    let numbered: Vec<(i64, &[Line])> = source.episodes.iter().map(|(e, lines)| (e.number, lines.as_slice())).collect();
    let prompt = recap_prompt(&show_title(&show), episode.number, &numbered, source.earlier.as_ref(), at.is_some(), &names_line(backend, &show, &text), answer_in(backend));
    let keeper = backend.clone();
    let kind = kind(at);
    let id = backend.asks.start(prompt, model(backend), EFFORT, false, move |answer| {
        if let Some(recap) = parse_recap(answer) {
            let _ = keeper.db.save_answer(episode_id, kind, -1, &about, &serde_json::to_string(&recap).unwrap_or_default(), "");
        }
    });
    backend.asks.writing(key, id);
    Ok(json!({ "ask": id }))
}

/// The recap for an ask that has finished, from what it kept.
pub fn recap_answer(backend: &Arc<Backend>, episode_id: i64, ask: u64, at: Option<f64>) -> Result<Value> {
    let progress = backend.asks.progress(ask);
    if !progress["done"].as_bool().unwrap_or(false) {
        return Ok(json!({ "done": false, "seconds": progress["seconds"] }));
    }
    if let Some(error) = progress["error"].as_str().filter(|e| !e.is_empty()) {
        return Ok(json!({ "done": true, "error": error }));
    }
    let answer = recap(backend, episode_id, false, at)?;
    if answer["recap"].is_string() {
        return Ok(answer);
    }
    Ok(json!({ "done": true, "error": "Claude’s recap didn’t come back in a form Telly could read. Try again." }))
}

pub fn recaps_on(backend: &Backend) -> bool {
    backend.db.setting_bool("recap.enabled", true)
}

/// Writes the recap for coming back, in the background, so it's ready then.
/// Called when you leave the player: after most of an episode, the recap
/// before the next one; part-way through (`resume_at`), the recap up to there.
pub fn prepare_recaps(backend: &Arc<Backend>, episode_id: i64, finished: bool, resume_at: Option<f64>) {
    if !recaps_on(backend) || claude::find().is_none() {
        return;
    }
    if finished {
        let next = backend.db.episode(episode_id).ok().flatten().and_then(|e| {
            let episodes = backend.db.episodes(e.show_id).ok()?;
            episodes.into_iter().skip_while(|e| e.id != episode_id).nth(1)
        });
        if let Some(next) = next {
            let _ = recap(backend, next.id, true, None);
        }
    }
    if resume_at.is_some() {
        let _ = recap(backend, episode_id, true, resume_at);
    }
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
    fn a_follow_up_carries_the_line_its_explanation_and_the_questions_before() {
        let lines = vec![line(1.0, 2.0, "ナツ！"), line(5.0, 6.0, "腹減った…"), line(7.0, 8.0, "また？")];
        let context = line_in_context("『FAIRY TAIL』", 3, &lines, 1, "");
        let thread = vec![Exchange { q: "Why no は?".into(), a: "It’s dropped in casual speech.".into() }];
        let p = follow_up_prompt(&context, "I’m hungry…\n腹 (はら): belly", &thread, " Is 腹 rude? ", "English");
        assert!(p.contains("<line>腹減った…</line>") && p.contains("<before>\nナツ！\n</before>"));
        assert!(p.contains("<explanation>\nI’m hungry…\n腹 (はら): belly\n</explanation>"));
        assert!(p.contains("<question>Why no は?</question>\nYou answered:\n<answer>\nIt’s dropped in casual speech.\n</answer>"));
        assert!(p.contains("Now I ask:\n<question>Is 腹 rude?</question>") && p.contains("in English"));
    }

    #[test]
    fn recaps_cover_the_episodes_before_in_arcs() {
        let lines: Vec<Line> = (0..30).map(|i| line(i as f64 * 3.0, i as f64 * 3.0 + 2.0, &format!("台詞{i}"))).collect();
        let window: Vec<(i64, &[Line])> = vec![(3, &lines[..]), (4, &lines[..])];
        let p = recap_prompt("『FAIRY TAIL』", 5, &window, None, false, "", "English");
        assert!(p.contains("<episode number=\"3\">\n[0] 台詞0") && p.contains("<episode number=\"4\">"));
        assert!(p.contains("Episodes 3 to 4") && !p.contains("<earlier>"));
        assert!(p.contains("\"arcs\"") && p.contains("Episode 4, the one I just left"));
        let answer = "Sure:\n{\"arcs\": [{\"title\": \"The Lullaby flute\", \"from\": 3, \"to\": 4, \"summary\": \"Erigor steals Lullaby.\"}], \"last\": \"Natsu wins.\", \"moments\": [{\"episode\": 3, \"line\": 2, \"caption\": \"Erigor takes the flute.\"}, {\"episode\": 4, \"line\": 20}]}";
        let recap = parse_recap(answer).unwrap();
        assert_eq!(recap.arcs[0].title, "The Lullaby flute");
        assert_eq!((recap.arcs[0].from, recap.arcs[0].to), (3, 4));
        assert_eq!(recap.last, "Natsu wins.");
        let pick = |episode, line, caption: &str| Pick { episode, line, caption: caption.into() };
        assert_eq!(recap.moments, vec![pick(3, 2, "Erigor takes the flute."), pick(4, 20, "")]);
        assert!(recap.text().contains("The Lullaby flute: Erigor steals Lullaby.") && recap.text().contains("Last time: Natsu wins."));
        assert_eq!(parse_recap("no json here"), None);
        // Seven moments become six, keeping the first and the last.
        let seven = "{\"last\": \"r\", \"moments\": [{\"episode\": 1, \"line\": 3}, {\"episode\": 1, \"line\": 40}, {\"episode\": 2, \"line\": 90}, {\"episode\": 2, \"line\": 150}, {\"episode\": 3, \"line\": 200}, {\"episode\": 3, \"line\": 260}, {\"episode\": 4, \"line\": 330}]}";
        let kept: Vec<(i64, usize)> = parse_recap(seven).unwrap().moments.iter().map(|m| (m.episode, m.line)).collect();
        assert_eq!(kept, vec![(1, 3), (1, 40), (2, 90), (3, 200), (3, 260), (4, 330)]);
    }

    #[test]
    fn recaps_carry_on_from_the_story_before() {
        let lines: Vec<Line> = (0..30).map(|i| line(i as f64 * 3.0, i as f64 * 3.0 + 2.0, &format!("台詞{i}"))).collect();
        let earlier = parse_recap("{\"arcs\": [{\"title\": \"Lucy joins\", \"from\": 1, \"to\": 4, \"summary\": \"Lucy joined Fairy Tail.\"}], \"last\": \"Natsu wins.\", \"moments\": [{\"episode\": 2, \"line\": 7, \"caption\": \"Lucy gets her mark.\"}]}").unwrap();
        // One episode since the kept recap: only its subtitles go, with the story before.
        let window: Vec<(i64, &[Line])> = vec![(5, &lines[..])];
        let p = recap_prompt("『FAIRY TAIL』", 6, &window, Some(&earlier), false, "", "English");
        assert!(p.contains("subtitles of Episode 5,") && !p.contains("<episode number=\"4\">"));
        assert!(p.contains("<earlier>\n{\"arcs\":[{\"title\":\"Lucy joins\",\"from\":1,\"to\":4"));
        assert!(p.contains("Lucy gets her mark.") && p.contains("Carry on from the earlier story") && p.contains("keep the best of the earlier moments"));
        // Part-way through Episode 5: the story so far in it, up to where it was left.
        let p = recap_prompt("『FAIRY TAIL』", 5, &window, Some(&earlier), true, "", "English");
        assert!(p.contains("coming back to Episode 5") && p.contains("what has happened so far in Episode 5") && p.contains("the last near where I stopped"));
    }

    /// A show of `count` episodes in a scratch data folder, each with `lines` as its subtitles.
    fn library(name: &str, count: i64, lines: &[Line]) -> (Arc<Backend>, Vec<Episode>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("telly-recap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Arc::new(Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        let episodes = (1..=count).map(|n| crate::db::NewEpisode { number: n, path: format!("{n}.mkv"), size: 1, duration: 1400.0 }).collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let episodes = backend.db.episodes(show).unwrap();
        for e in &episodes {
            jobs::write_lines(&backend, e.id, lines).unwrap();
        }
        (backend, episodes, dir)
    }

    /// Keeps `recap` as the one before `episode`, as if Claude had just written it.
    fn keep(backend: &Backend, episode: &Episode, recap: &Recap) {
        let about = recap_about(&source(backend, episode, None).unwrap());
        backend.db.save_answer(episode.id, "recap", -1, &about, &serde_json::to_string(recap).unwrap(), "").unwrap();
    }

    #[test]
    fn a_moment_plays_where_its_lines_are_heard_in_an_episode_with_its_subtitles_nudged() {
        let lines: Vec<Line> = (0..30).map(|i| line(100.0 + i as f64 * 3.0, 102.0 + i as f64 * 3.0, &format!("台詞{i}"))).collect();
        let (backend, episodes, dir) = library("nudge", 2, &lines);
        // Episode 1's subtitles were nudged 2 s later (Z and X): line 10, at 130 s in the file, is heard at 132 s.
        backend.db.set_nudge(episodes[0].id, 2000).unwrap();
        let recap = Recap { arcs: Vec::new(), last: "Roman runs.".into(), moments: vec![Pick { episode: 1, line: 10, caption: "Roman runs.".into() }] };
        keep(&backend, &episodes[1], &recap);
        let view = super::recap(&backend, episodes[1].id, false, None).unwrap();
        let clip = &view["moments"][0];
        let tenths = |v: &Value| (v.as_f64().unwrap() * 10.0).round() as i64;
        // From just before line 10 is heard to just after line 11 ends: 132 - 0.6 to 137 + 0.5.
        assert_eq!((tenths(&clip["start"]), tenths(&clip["end"])), (1314, 1375), "{clip}");
        // Its lines are as the file has them, and the player shows them nudged, as in the episode.
        assert_eq!((clip["lines"][0]["start"].as_f64(), clip["nudgeMs"].as_i64()), (Some(130.0), Some(2000)));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_moment_that_runs_into_the_one_before_carries_on_from_it_instead_of_playing_its_lines_again() {
        let lines: Vec<Line> = (0..30).map(|i| line(100.0 + i as f64 * 3.0, 102.0 + i as f64 * 3.0, &format!("台詞{i}"))).collect();
        let (backend, episodes, dir) = library("overlap", 2, &lines);
        // Lines 10 and 11 are a question and its answer, 3 s apart: line 10's moment already plays line 11.
        let pick = |line, caption: &str| Pick { episode: 1, line, caption: caption.into() };
        let recap = Recap { arcs: Vec::new(), last: "Roman runs.".into(), moments: vec![pick(10, "Asked."), pick(11, "Answered."), pick(11, "Again."), pick(25, "Later.")] };
        keep(&backend, &episodes[1], &recap);
        let view = super::recap(&backend, episodes[1].id, false, None).unwrap();
        let clips: Vec<(i64, i64, &str)> = view["moments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| ((c["start"].as_f64().unwrap() * 10.0).round() as i64, (c["end"].as_f64().unwrap() * 10.0).round() as i64, c["caption"].as_str().unwrap()))
            .collect();
        // Line 11's moment starts where line 10's ended; the same moment again, wholly played already, goes.
        assert_eq!(clips, vec![(1294, 1355, "Asked."), (1355, 1385, "Answered."), (1744, 1805, "Later.")]);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
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
