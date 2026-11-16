//! What Telly asks Claude (claude.rs): "Explain this line" in the player, with
//! Gist, Break it down, Nuance and follow-up questions about it, and
//! "Previously on", a recap of the story so far, with the moments of it to
//! play back. Answers are kept (db answers), so asking twice is instant. The
//! setup also asks a test question, to see that Claude Code answers. When
//! Claude Code isn't on this computer and Settings has an OpenRouter key,
//! gpt-oss on OpenRouter (openrouter.rs) answers instead.
//!
//! Each episode's recap carries on from the one kept for the episode before,
//! so the story so far grows by one episode's subtitles at a time. A film's
//! recap is of the film so far, for picking it up part-way. Before a season's
//! first episode, the recap tells the season before (series.rs has which it
//! is), from what you watched of it; the season's other recaps are of the
//! season alone and never carry that one on.
//!
//! An answer is written while you watch: `start` returns an ask's id and the
//! player asks `progress` for the text so far, the way it ticks.

use crate::backend::Backend;
use crate::claude;
use crate::db::{Episode, Show};
use crate::jobs;
use crate::names;
use crate::openrouter;
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
    /// `warm` asks in Claude Code's warm session (short questions).
    fn start(&self, prompt: String, who: Who, effort: &'static str, warm: bool, keep: impl FnOnce(&str) + Send + 'static) -> u64 {
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
            let on_text = |piece: &str| {
                if let Ok(mut t) = asking.text.lock() {
                    t.push_str(piece);
                }
            };
            let result = match &who {
                Who::Claude(model) => {
                    let question = claude::Ask { prompt: &prompt, model: model.as_deref(), effort: Some(effort), warm };
                    claude::ask(&question, on_text, &asking.stop)
                }
                Who::OpenRouter(key) => openrouter::chat(&openrouter::api_address(), key, claude::SYSTEM, &prompt, Some(effort), on_text, &asking.stop),
            };
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

/// Who answers: the Claude Code on this computer, with the model Settings
/// picked (None for Claude Code's own default), or gpt-oss on OpenRouter, with
/// the key from Settings.
pub enum Who {
    Claude(Option<String>),
    OpenRouter(String),
}

/// Claude Code, unless it isn't installed and Settings has an OpenRouter key.
pub fn who(backend: &Backend) -> Who {
    let key = backend.db.setting_str("openrouter.key");
    if claude::find().is_none() && !key.trim().is_empty() {
        Who::OpenRouter(key.trim().to_string())
    } else {
        Who::Claude(model(backend))
    }
}

/// Whether Explain and Previously on can be answered, and by whom, for
/// Settings, the setup and the player: Claude Code's status, or gpt-oss on
/// OpenRouter's (with `openrouter` true) when Claude Code isn't installed.
pub fn status(backend: &Backend) -> Value {
    match who(backend) {
        Who::OpenRouter(_) => json!({
            "available": true,
            "version": openrouter::CHAT_MODEL,
            "detail": "Claude Code isn’t installed on this computer, so gpt-oss on OpenRouter answers, with your OpenRouter key.",
            "openrouter": true,
        }),
        Who::Claude(_) => json!(claude::status()),
    }
}

/// Explain's effort: low answers in about a third of the time.
pub const EFFORT: &str = "low";

/// The prompt version of a saved explanation.
const LINE_ANSWER_VERSION: &str = "2";

/// English, or simple Japanese for when you'd rather not read English.
fn answer_in(backend: &Backend) -> &'static str {
    if backend.db.setting_str("claude.language") == "ja" { "simple Japanese" } else { "English" }
}

/// What an explanation (`base` its version) or a recap (`base` empty) is kept
/// with, so one written in another language isn't shown: English's is `base` alone.
fn kept_with(base: &str, language: &str) -> String {
    if language == "English" { base.to_string() } else { format!("{base}|{language}") }
}

fn spoken(lines: &[Line]) -> impl DoubleEndedIterator<Item = (usize, &Line)> {
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

/// `lines[index]` for a prompt, with the dialogue just before it.
fn line_in_context(show_title: &str, number: i64, lines: &[Line], index: usize, names: &str) -> String {
    let line = &lines[index];
    let before: Vec<String> = spoken(&lines[..index])
        .filter(|(_, l)| l.start >= line.start - 90.0)
        .map(|(_, l)| one_line(&l.text))
        .collect();
    let before = &before[before.len().saturating_sub(12)..];
    format!(
        "I’m watching {show_title}, Episode {number}, an anime in Japanese, with Japanese subtitles.{names}\n\n\
         The subtitles just before the line, oldest first:\n<before>\n{before}\n</before>\n\n\
         The line:\n<line>{line}</line>\n\n",
        before = before.join("\n"),
        line = one_line(&line.text),
    )
}

/// The prompt for explaining `lines[index]`.
pub fn explain_prompt(show_title: &str, number: i64, lines: &[Line], index: usize, names: &str, answer_in: &str) -> String {
    format!(
        "{}Explain the line for a learner, in {answer_in}. Start immediately with the natural translation itself, on a line of its own. \
         Then, one per line, the words, grammar and expressions a learner might trip on, each as \"term (reading): explanation\", \
         with the reading in hiragana for words with kanji. Leave out what a beginner already knows. \
         Give just the translation and learning notes. Use only the subtitles in this request as scene context. Keep it under 160 words.",
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
         Answer the question directly in {answer_in}, for a learner, in under 120 words. Use only the subtitles in this request as scene context.",
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
    let near: String = lines[index.saturating_sub(12)..=index].iter().map(|l| l.text.as_str()).collect();
    let context = line_in_context(&show_title(&show), episode.number, &lines, index, &names_line(backend, &show, &near));
    let prompt = follow_up_prompt(&context, explanation, thread, question, answer_in(backend));
    let id = backend.asks.start(prompt, who(backend), EFFORT, true, |_| {});
    Ok(json!({ "ask": id }))
}

/// What each button under an explanation asks about the line, by its kind:
/// Gist, Break it down and Nuance.
fn more_asked(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "gist" => "Give me just the gist of the line: the important part of what it says, in a sentence or two.",
        "breakdown" => {
            "Break the line down for me: every word, particle and ending in it, in order, one per line, each as \
             \"part (reading): what it is and does\", with the reading in hiragana for words with kanji. Trace each \
             conjugated form back to its dictionary form, step by step. Keep it under 200 words."
        }
        "nuance" => {
            "Tell me why the line is put the way it is: its tone, how polite or casual it is, and anything cultural in \
             how it’s said. Keep it under 120 words."
        }
        _ => return None,
    })
}

/// The prompt for one of the buttons under an explanation (`kind`: gist,
/// breakdown or nuance). Like a follow-up it carries the line and its
/// explanation, but not the questions before, so its answer can be kept for the line.
pub fn explain_more_prompt(context: &str, explanation: &str, kind: &str, answer_in: &str) -> Option<String> {
    let asked = more_asked(kind)?;
    Some(format!(
        "{context}You explained the line to me like this:\n<explanation>\n{}\n</explanation>\n\n\
         {asked} Answer directly in {answer_in}, for a learner. Use only the subtitles in this request as scene context.",
        explanation.trim(),
    ))
}

/// The answer kept for a button under the explanation of line `index`, while
/// the line reads as it did (`about`) and the answer is in `language`.
fn kept_more(backend: &Backend, episode_id: i64, index: usize, kind: &str, about: &str, language: &str) -> Option<String> {
    let kept = backend.db.answer(episode_id, &format!("line.{kind}"), index as i64).ok().flatten()?;
    (kept.about == about && kept.extra == language).then_some(kept.text)
}

/// One of the buttons under an explanation: a kept answer, or an ask to
/// follow. Asked in the warm session, and kept for the line in the language
/// it's written in, so English and Japanese answers don't mix.
pub fn explain_more(backend: &Arc<Backend>, episode_id: i64, index: usize, explanation: &str, kind: &str) -> Result<Value> {
    let episode = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let show = backend.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let lines = jobs::read_lines(backend, episode_id);
    let line = lines.get(index).ok_or_else(|| anyhow!("That line isn’t in the subtitles any more"))?;
    let about = one_line(&line.text);
    let language = answer_in(backend);
    if let Some(text) = kept_more(backend, episode_id, index, kind, &about, language) {
        return Ok(json!({ "text": text, "done": true }));
    }
    let near: String = lines[index.saturating_sub(12)..=index].iter().map(|l| l.text.as_str()).collect();
    let context = line_in_context(&show_title(&show), episode.number, &lines, index, &names_line(backend, &show, &near));
    let prompt = explain_more_prompt(&context, explanation, kind, language).ok_or_else(|| anyhow!("Telly doesn’t know that question"))?;
    let keeper = backend.clone();
    let kept_as = format!("line.{kind}");
    let id = backend.asks.start(prompt, who(backend), EFFORT, true, move |answer| {
        let _ = keeper.db.save_answer(episode_id, &kept_as, index as i64, &about, answer, language);
    });
    Ok(json!({ "ask": id }))
}

/// The setup's test question: one short question, asked as Explain asks, so
/// its answer (or why there's none) says whether Claude Code works here. An
/// ask to follow; not kept.
pub fn test(backend: &Arc<Backend>) -> Value {
    let prompt = format!("In {}, in a few words: what does 「はじめまして」 mean?", answer_in(backend));
    let id = backend.asks.start(prompt, who(backend), EFFORT, false, |_| {});
    json!({ "ask": id })
}

/// "Explain this line": a kept answer, or an ask to follow.
pub fn explain(backend: &Arc<Backend>, episode_id: i64, index: usize) -> Result<Value> {
    let episode = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let show = backend.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let lines = jobs::read_lines(backend, episode_id);
    let line = lines.get(index).ok_or_else(|| anyhow!("That line isn’t in the subtitles any more"))?;
    let about = one_line(&line.text);
    let language = answer_in(backend);
    let extra = kept_with(LINE_ANSWER_VERSION, language);
    if let Some(kept) = backend.db.answer(episode_id, "line", index as i64)? {
        if kept.about == about && kept.extra == extra {
            return Ok(json!({ "text": kept.text, "done": true, "line": about }));
        }
    }
    let context: String = lines[index.saturating_sub(12)..=index].iter().map(|l| l.text.as_str()).collect();
    let prompt = explain_prompt(&show_title(&show), episode.number, &lines, index, &names_line(backend, &show, &context), language);
    let keeper = backend.clone();
    let kept_about = about.clone();
    let id = backend.asks.start(prompt, who(backend), EFFORT, true, move |answer| {
        let _ = keeper.db.save_answer(episode_id, "line", index as i64, &kept_about, answer, &extra);
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

/// The stretches to play for the chosen lines, with room for the exchange to play out.
pub fn moments(lines: &[Line], chosen: &[usize]) -> Vec<Moment> {
    let mut out: Vec<Moment> = Vec::new();
    let mut chosen: Vec<usize> = chosen.iter().copied().filter(|&i| i < lines.len()).collect();
    chosen.sort_unstable();
    chosen.dedup();
    for i in chosen {
        let first = &lines[i];
        if first.sign || first.text.trim().is_empty() {
            continue;
        }
        let mut following = spoken(&lines[i + 1..]).peekable();
        let mut last = i;
        let mut end = first.end;
        while let Some(&(offset, next)) = following.peek() {
            if next.start - end > 1.2 || next.end - first.start > 10.0 {
                break;
            }
            last = i + 1 + offset;
            end = end.max(next.end);
            following.next();
        }
        let before = spoken(&lines[..i]).next_back().map_or(0.0, |(_, line)| line.end.min(first.start));
        let start = (first.start - 0.8).max(before).max(0.0);
        // Let the dialogue finish and leave room for the fade before another line begins.
        let tail = (end + 0.9).max(start + 4.5).min(end + 2.0);
        let end = following.peek().map_or(tail, |(_, next)| tail.min(next.start.max(end)));
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

/// What a recap is of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RecapOf {
    /// The story before the next episode.
    NextEpisode,
    /// The story up to where you stopped, part-way through an episode.
    EpisodeSoFar,
    /// A film up to where you stopped in it.
    FilmSoFar,
    /// The story of the season before, ahead of a season's first episode.
    SeasonBefore,
}

/// The most scenes a recap plays from the episode you left, by how long it
/// ran as far as you watched it: one for every 6 minutes, from 1 to 4; for a
/// `film`, one for every 10 minutes of it so far, from 1 to 6.
pub fn latest_most(lines: &[Line], film: bool) -> usize {
    let (per, most) = if film { (10.0, 6) } else { (6.0, 4) };
    let minutes = spoken(lines).next_back().map_or(0.0, |(_, l)| l.end) / 60.0;
    ((minutes / per).round() as usize).clamp(1, most)
}

/// The most scenes a recap plays from the story before the episode you left.
const EARLIER_MOMENTS: usize = 3;
/// How long the scenes from the episode you left play for at most, in
/// seconds: past that, those in the middle go.
const LATEST_S: f64 = 45.0;
/// How long a recap's scenes play for at most, in seconds: past that, those
/// from the story before go.
const MONTAGE_S: f64 = 75.0;

/// The prompt for "Previously on" before Episode `number`: the subtitles of
/// the episodes it reads (`window`, oldest first, each line numbered), and the
/// recap kept for the first of them (`earlier`), which it carries on from.
/// Coming back part-way through Episode `number` (`of`), it's the last of the
/// window, as far as it was watched; in a film, the window is the film so far.
pub fn recap_prompt(show_title: &str, number: i64, window: &[(i64, &[Line])], earlier: Option<&Recap>, of: RecapOf, names: &str, answer_in: &str) -> String {
    let resuming = of == RecapOf::EpisodeSoFar;
    let film = of == RecapOf::FilmSoFar;
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
    let opening = if film {
        format!(
            "I’m coming back to {show_title}, an anime film in Japanese, part-way through it. Here are its Japanese subtitles as far \
             as I watched, each line numbered:{names}"
        )
    } else if resuming {
        format!(
            "I’m coming back to Episode {number} of {show_title}, an anime in Japanese, part-way through it. Here are the Japanese \
             subtitles of {span}, each line numbered, with Episode {number} only as far as I watched:{names}"
        )
    } else if of == RecapOf::SeasonBefore {
        format!(
            "I’ve watched {show_title}, an anime in Japanese, and I’m about to start the season that carries on from it. Here are the \
             Japanese subtitles of {span}, the last of it I watched, each line numbered:{names}"
        )
    } else {
        format!(
            "I’m about to watch Episode {number} of {show_title}, an anime in Japanese. Here are the Japanese subtitles of {span}, \
             from before it, each line numbered:{names}"
        )
    };
    let (before, carry_arcs, take_from) = match earlier {
        Some(recap) => (
            format!(
                "\n\nThe story before Episode {first}, as you told it to me then, with the moments you chose:\n<earlier>\n{}\n</earlier>",
                serde_json::to_string(recap).unwrap_or_default()
            ),
            " Carry on from the earlier story: keep its arcs, run the latest one on or add a new one, and fold the oldest together \
             when there would be more than five."
                .to_string(),
            "from the earlier moments, with the episode, line and caption they had, or from the subtitles above: never a line of an \
             episode whose subtitles aren’t here",
        ),
        None => (String::new(), String::new(), "from the subtitles above"),
    };
    // Claude counts the scenes from what it wrote in "last", which comes before them.
    let most = window.last().map_or(1, |(_, lines)| latest_most(lines, film));
    let (last_is, play_before, from_last, done_at, near, only) = if resuming {
        (
            format!("what has happened so far in Episode {last}, in {answer_in}: two to four sentences, ending where I stopped, so I can pick up from there"),
            "before I pick up again",
            format!("From Episode {last} as far as I watched"),
            "where I stopped",
            "near where I stopped",
            "just the one near where I stopped if nothing else is open",
        )
    } else {
        (
            format!(
                "what happened in Episode {last}, {left}, in {answer_in}: three or four sentences, ending where its story \
                 ends (not a preview of the next episode), so I can pick up from there",
                left = if of == RecapOf::SeasonBefore { "the last of it I watched" } else { "the one I just left" },
            ),
            "before I watch",
            format!("From Episode {last}"),
            "when the episode ends",
            "near its end",
            "just the one near its end if nothing else is left open",
        )
    };
    let story_before = if earlier.is_some() || window.len() > 1 {
        format!(
            " For the story before Episode {last}: a scene for each of its arcs that the story isn’t done with {done_at}, none for an \
             arc that’s over, at most {EARLIER_MOMENTS}. Take them {take_from}."
        )
    } else {
        String::new()
    };
    // A film is told in its parts, and who someone is counts as worth a scene: there's no recap before it that tells them.
    let (arcs, last_is, moments) = if film {
        (
            format!(
                "the film so far, cut into its parts, oldest first, five at most. Each has a short title saying what happens in it, taken \
                 from the subtitles (never a name from elsewhere that could hint at what’s ahead), \"from\" and \"to\" both {last}, and two \
                 or three sentences in {answer_in}. The last part runs up to where I stopped."
            ),
            format!(
                "where the film is when I stopped, in {answer_in}: three or four sentences on the part I stopped in, ending where I stopped, \
                 so I can pick up from there"
            ),
            format!(
                "the scenes to play before I pick up again, each the first complete spoken line of an exchange that shows it when played \
                 as a clip: a scene for each thing in the film so far that its story isn’t done with where I stopped (who someone is or what \
                 they want, a turn, a reveal, a promise, a threat, a choice), from the parts you told in \"arcs\" and \"last\", never two for \
                 the same thing, the last of them near where I stopped. How many follows what has happened, not the most allowed: at most \
                 {most}. Return the picks in line order."
            ),
        )
    } else {
        (
            format!(
                "the whole story so far, cut into its arcs or parts, oldest first, five at most.{carry_arcs} Each has a short title saying \
                 what happens in it, taken from the subtitles (never a name from elsewhere that could hint at what’s ahead), the first and \
                 last episode it covers (\"from\" and \"to\"), and two or three sentences in {answer_in}. An arc can run on into Episode {last}."
            ),
            last_is,
            format!(
                "the scenes to play {play_before}, each the first complete spoken line of an exchange that shows it when played as a clip. \
                 {from_last}: a scene for each thing you told in \"last\" that the story isn’t done with {done_at} (a turn, a reveal, a \
                 promise, a threat, a choice), never two for the same thing, the last of them {near}. How many follows what happened, not \
                 the most allowed: {only}, and at most {most}.{story_before} Return the picks in episode and line order."
            ),
        )
    };
    let what = if film { "film" } else { "show" };
    format!(
        "{opening}\n\n{body}{before}\n\n\
         Reply with only a JSON object, and no other text, shaped like this: \
         {{\"arcs\": [{{\"title\": \"…\", \"from\": {first}, \"to\": {last}, \"summary\": \"…\"}}], \"last\": \"…\", \
         \"moments\": [{{\"episode\": {last}, \"line\": 12, \"caption\": \"…\"}}]}}.\n\
         \"arcs\": {arcs}\n\
         \"last\": {last_is}.\n\
         \"moments\": {moments} Each has a \"caption\" in {answer_in}: one sentence of under 15 words saying what’s happening at that \
         moment, for someone who has forgotten it.\n\
         Write the characters’ names as AniList does. Keep it under 400 words in all. Use only the subtitles and the earlier story \
         above, and don’t guess at what comes next. If you recognise the {what}, still tell me only what they say, and choose the scenes \
         only by what’s in the subtitles: what you know of it from elsewhere could spoil what’s ahead.",
        body = episodes.join("\n\n"),
    )
}

/// Claude's recap, from its answer: the first JSON object in it that's a recap,
/// whatever is written around it, braces and all.
pub fn parse_recap(answer: &str) -> Option<Recap> {
    answer.match_indices('{').find_map(|(at, _)| {
        let value = serde_json::Deserializer::from_str(&answer[at..]).into_iter::<Value>().next()?.ok()?;
        recap_from(&value)
    })
}

fn recap_from(value: &Value) -> Option<Recap> {
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

/// The most moments kept from Claude's answer: more than a recap ever plays, so only junk is cut.
const MAX_MOMENTS: usize = 10;

/// `n` of the moments, spread from the first to the last (the end matters most).
fn at_most<T: Clone>(moments: Vec<T>, n: usize) -> Vec<T> {
    if moments.len() <= n {
        return moments;
    }
    if n < 2 {
        return moments.into_iter().rev().take(n).collect();
    }
    let last = moments.len() - 1;
    (0..n).map(|k| moments[(k * last + (n - 1) / 2) / (n - 1)].clone()).collect()
}

/// The picks Claude could have seen: those from the episodes whose subtitles
/// it read (`window`, by number), and the earlier recap's own, as they were.
/// A line of any other episode would be made up, and play under the wrong caption.
fn seen_picks(moments: Vec<Pick>, window: &[i64], earlier: Option<&Recap>) -> Vec<Pick> {
    let carried = |p: &Pick| earlier.is_some_and(|r| r.moments.iter().any(|m| (m.episode, m.line) == (p.episode, p.line)));
    moments.into_iter().filter(|p| window.contains(&p.episode) || carried(p)).collect()
}

/// What a recap is made from.
#[derive(Default)]
struct Source {
    /// The episodes it reads the subtitles of, oldest first.
    episodes: Vec<(Episode, Vec<Line>)>,
    /// The recap kept for the first of them: the story before.
    earlier: Option<Recap>,
    /// The season before, when the recap is of that and not of the episode's own show.
    of: Option<Show>,
}

/// Enough lines to tell a story from.
fn tells(lines: &[Line]) -> bool {
    spoken(lines).count() >= 20
}

/// How far into an episode, in seconds, there's a recap up to where you
/// stopped. Before that it would be of the cold open, and the recap of the
/// episode before is the one to see.
const RESUME_FROM_S: f64 = 300.0;

/// What the recap before `episode` is made from: the episodes before it with
/// subtitles, back to the nearest one with a recap kept (which tells the story
/// before), six at most. With `at`, `episode` itself is the last of them, as
/// far as `at` seconds in. Nothing when the last of them has too few lines, or
/// `at` is in the first 5 minutes.
///
/// A show's first episode has none before it: its recap is of the season
/// before (`season_before`), and no recap of this show carries that one on.
fn source(backend: &Backend, episode: &Episode, at: Option<f64>) -> Result<Source> {
    let all = backend.db.episodes(episode.show_id)?;
    let Some(i) = all.iter().position(|e| e.id == episode.id) else { return Ok(Source::default()) };
    if i == 0 && at.is_none() {
        return season_before(backend, episode);
    }
    let first = all.first().map(|e| e.id);
    let kept = |id: i64| if Some(id) == first { None } else { kept(backend, id) };
    let mut source = Source::default();
    if let Some(at) = at {
        if at < RESUME_FROM_S {
            return Ok(source);
        }
        let mut lines = jobs::read_lines(backend, episode.id);
        let nudge = episode.subs_nudge_ms as f64 / 1000.0;
        lines.truncate(lines.iter().take_while(|l| l.start + nudge < at).count());
        if !tells(&lines) {
            return Ok(source);
        }
        source.episodes.push((episode.clone(), lines));
        source.earlier = kept(episode.id);
    }
    for e in all[..i].iter().rev() {
        if source.earlier.is_some() || source.episodes.len() >= RECAP_EPISODES {
            break;
        }
        let lines = seen_lines(backend, e);
        if tells(&lines) {
            source.episodes.push((e.clone(), lines));
            source.earlier = kept(e.id);
        } else if source.episodes.is_empty() {
            return Ok(source);
        }
    }
    source.episodes.reverse();
    Ok(source)
}

/// What the recap before a season's first episode is made from: the season
/// it carries on from (AniList's prequel, when it's in the library), told
/// from the episodes of it you watched, the last six at most, carrying on
/// from that season's own recaps. Nothing when you watched none of it here:
/// the story is only ever told from what you've seen.
fn season_before(backend: &Backend, episode: &Episode) -> Result<Source> {
    let mut source = Source::default();
    let Some(show) = backend.db.show(episode.show_id)? else { return Ok(source) };
    let Some(before) = crate::series::previous_show(backend, &show) else { return Ok(source) };
    let all = backend.db.episodes(before.id)?;
    let first = all.first().map(|e| e.id);
    for e in all.iter().rev().skip_while(|e| e.watched_at.is_none()) {
        if source.earlier.is_some() || source.episodes.len() >= RECAP_EPISODES {
            break;
        }
        let lines = seen_lines(backend, e);
        if tells(&lines) {
            source.episodes.push((e.clone(), lines));
            source.earlier = if Some(e.id) == first { None } else { kept(backend, e.id) };
        } else if source.episodes.is_empty() {
            return Ok(source);
        }
    }
    source.episodes.reverse();
    if !source.episodes.is_empty() {
        source.of = Some(before);
    }
    Ok(source)
}

/// An earlier episode's lines, as far as you've seen it: up to where its
/// ending starts, unless you've played on past the ending's end. What comes
/// after the credits (a last scene, the next episode's preview) is told only
/// once you've played it, so an ending skipped or closed on spoils nothing.
fn seen_lines(backend: &Backend, episode: &Episode) -> Vec<Line> {
    let mut lines = jobs::read_lines(backend, episode.id);
    let duration = episode.duration;
    let aniskip = backend.db.ending(episode.id).ok().flatten();
    let Some(start) = episode.ending_at.or(aniskip.map(|e| e.0)).filter(|s| duration > 0.0 && *s > duration * 0.5 && *s < duration) else {
        return lines;
    };
    let end = aniskip.map(|e| e.1).filter(|e| *e > start && *e <= duration).unwrap_or(duration);
    if backend.db.reached(episode.id).unwrap_or(0.0) >= end - 3.0 {
        return lines;
    }
    let nudge = episode.subs_nudge_ms as f64 / 1000.0;
    // Cut where the ending starts, as a recap up to where you stopped is: line numbers stay as they were.
    lines.truncate(lines.iter().take_while(|l| l.start + nudge < start).count());
    lines
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

/// The recap kept for before the episode `id`, while the subtitles it was
/// made from are as they were, and you've seen no more of those episodes
/// than it tells (a scene after the credits, played since).
fn kept(backend: &Backend, id: i64) -> Option<Recap> {
    let answer = backend.db.answer(id, "recap", -1).ok().flatten()?;
    if answer.extra != kept_with("", answer_in(backend)) {
        return None;
    }
    let same = answer.about.strip_prefix(ABOUT)?.split(',').all(|part| {
        let of: Option<i64> = part.split(':').next().and_then(|id| id.parse().ok());
        // As far as you've seen each, which is what the recap was made from: an
        // episode left at its ending is its lines up to there, not the whole file.
        let episode = of.and_then(|of| backend.db.episode(of).ok().flatten());
        episode.is_some_and(|e| about_part(e.id, &seen_lines(backend, &e)) == part)
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

/// A clip of a recap's moments: its episode's id, where it starts and ends, and what the player is sent.
#[derive(Clone)]
struct Clip {
    episode: i64,
    start: f64,
    end: f64,
    view: Value,
}

/// Where each clip starts playing: where the one before it in the same
/// episode ended, if that's later, so its lines aren't played twice.
fn play_from(clips: &[Clip]) -> Vec<f64> {
    let mut previous: Option<&Clip> = None;
    clips
        .iter()
        .map(|c| {
            let from = match previous {
                Some(p) if p.episode == c.episode => c.start.max(p.end),
                _ => c.start,
            };
            previous = Some(c);
            from
        })
        .collect()
}

/// How long the clips play for, one after another.
fn length(clips: &[Clip]) -> f64 {
    clips.iter().zip(play_from(clips)).map(|(c, from)| (c.end - from).max(0.0)).sum()
}

/// `most` of the clips, spread from the first to the last, and fewer while
/// they play for longer than `seconds`: those in the middle go, oldest first,
/// then the first, leaving at least `least`. The last goes only when `least` is 0.
fn keep_within(clips: Vec<Clip>, most: usize, seconds: f64, least: usize) -> Vec<Clip> {
    let mut clips = at_most(clips, most);
    while clips.len() > least && length(&clips) > seconds {
        clips.remove(if clips.len() > 2 { 1 } else { 0 });
    }
    clips
}

/// The recap as the player shows it, with the clips to play. In a `film`,
/// every clip is of the film so far, and plays whenever you come back.
fn recap_view(backend: &Backend, show_id: i64, source: &Source, recap: &Recap, at: Option<f64>, film: bool) -> Result<Value> {
    let all = backend.db.episodes(show_id)?;
    let last = source.episodes.last().map(|(e, _)| e.number);
    // The clips of the story before the episode you left, and of that episode (or the film so far).
    let mut earlier = Vec::new();
    let mut latest = Vec::new();
    // The clip before: its episode, start and end.
    let mut before: Option<(i64, f64, f64)> = None;
    let mut picks: Vec<&Pick> = recap.moments.iter().filter(|p| last.is_some_and(|last| p.episode <= last)).collect();
    picks.sort_by_key(|p| (p.episode, p.line));
    for pick in picks {
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
        let is_last = Some(e.number) == last;
        // In a film, every clip is of the film so far.
        let from_latest = is_last || film;
        let duration = if e.duration > 0.0 { e.duration } else { f64::INFINITY };
        let limit = if is_last { at.unwrap_or(duration).min(duration) } else { duration };
        for m in moments(lines, &[pick.line]) {
            let start = (m.start + nudge).max(0.0);
            let end = (m.end + nudge).min(limit);
            if end <= start {
                continue;
            }
            // Two lines picked close together: the second's clip carries on from where the first's
            // ended, rather than playing its lines again (`play_from`), and goes if the first played all of it.
            let mut after_previous = start;
            if let Some((episode, from, to)) = before {
                if episode == e.id && start < to && end > from {
                    if end <= to {
                        continue;
                    }
                    after_previous = to;
                }
            }
            before = Some((e.id, after_previous, end));
            let view = json!({
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
                "latest": from_latest,
            });
            let clip = Clip { episode: e.id, start, end, view };
            if from_latest {
                latest.push(clip);
            } else {
                earlier.push(clip);
            }
        }
    }
    // As many scenes as the episode you left ran to, and the story before them,
    // within their time: an older recap may have picked more. The last always plays.
    let most = source.episodes.last().map_or(1, |(_, lines)| latest_most(lines, film));
    let latest = keep_within(latest, most, if film { MONTAGE_S } else { LATEST_S }, 1);
    let earlier = keep_within(earlier, EARLIER_MOMENTS, MONTAGE_S - length(&latest), 0);
    let kept: Vec<Clip> = earlier.into_iter().chain(latest).collect();
    // If a clip was dropped, the one after it can play from its own start again.
    let clips: Vec<Value> = kept
        .iter()
        .zip(play_from(&kept))
        .map(|(clip, from)| {
            let mut view = clip.view.clone();
            view["start"] = json!(from);
            view
        })
        .collect();
    Ok(json!({
        "done": true,
        "recap": recap.text(),
        "arcs": recap.arcs,
        "last": recap.last,
        "lastNumber": last,
        "moments": clips,
        // The season before, when that's whose story this is.
        "season": source.of.as_ref().map(|s| s.title.as_str()),
    }))
}

/// Whether a recap can be made for `episode` (before it, or with `at`, up to
/// there in it), and whether it's written.
pub fn recap_state(backend: &Backend, episode: &Episode, at: Option<f64>) -> Value {
    let Ok(source) = source(backend, episode, at) else { return json!({ "possible": false }) };
    if source.episodes.is_empty() {
        return json!({ "possible": false });
    }
    let extra = kept_with("", answer_in(backend));
    let ready = backend.db.answer(episode.id, kind(at), -1).ok().flatten().is_some_and(|a| a.about == recap_about(&source) && a.extra == extra);
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
    let lengths: Vec<f64> = backend.db.episodes(show.id)?.iter().map(|e| e.duration).collect();
    let film = crate::backend::is_movie(&show.format, show.anilist_id.is_some(), &lengths);
    let of = match at {
        Some(_) if film => RecapOf::FilmSoFar,
        Some(_) => RecapOf::EpisodeSoFar,
        None if source.of.is_some() => RecapOf::SeasonBefore,
        None => RecapOf::NextEpisode,
    };
    // The show whose story is told: this one, or before its first episode, the season before.
    let told = source.of.clone().unwrap_or_else(|| show.clone());
    let language = answer_in(backend);
    let extra = kept_with("", language);
    if let Some(kept) = backend.db.answer(episode_id, kind(at), -1)? {
        if kept.about == about && kept.extra == extra {
            if let Ok(recap) = serde_json::from_str::<Recap>(&kept.text) {
                return recap_view(backend, told.id, &source, &recap, at, of == RecapOf::FilmSoFar);
            }
        }
    }
    if !start {
        return Ok(json!({ "done": false }));
    }
    let key = format!("{episode_id}|{}|{about}|{language}", kind(at));
    if let Some(id) = backend.asks.going(&key) {
        return Ok(json!({ "ask": id }));
    }
    let text: String = source.episodes.iter().flat_map(|(_, lines)| lines.iter().map(|l| l.text.as_str())).collect();
    let numbered: Vec<(i64, &[Line])> = source.episodes.iter().map(|(e, lines)| (e.number, lines.as_slice())).collect();
    let number = if of == RecapOf::SeasonBefore { numbered.last().map_or(1, |(n, _)| n + 1) } else { episode.number };
    let prompt = recap_prompt(&show_title(&told), number, &numbered, source.earlier.as_ref(), of, &names_line(backend, &told, &text), language);
    let keeper = backend.clone();
    let kind = kind(at);
    let read: Vec<i64> = source.episodes.iter().map(|(e, _)| e.number).collect();
    let earlier = source.earlier.clone();
    let id = backend.asks.start(prompt, who(backend), EFFORT, false, move |answer| {
        if let Some(mut recap) = parse_recap(answer) {
            recap.moments = seen_picks(recap.moments, &read, earlier.as_ref());
            let _ = keeper.db.save_answer(episode_id, kind, -1, &about, &serde_json::to_string(&recap).unwrap_or_default(), &extra);
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
/// before the next one; part-way through (`resume_at`), the recap up to there,
/// or in its first 5 minutes the recap before it.
pub fn prepare_recaps(backend: &Arc<Backend>, episode_id: i64, finished: bool, resume_at: Option<f64>) {
    if !recaps_on(backend) || matches!(who(backend), Who::Claude(_)) && claude::find().is_none() {
        return;
    }
    if finished {
        let next = backend.db.episode(episode_id).ok().flatten().and_then(|e| {
            let episodes = backend.db.episodes(e.show_id).ok()?;
            episodes.into_iter().skip_while(|e| e.id != episode_id).nth(1)
        });
        // After a season's last episode, the next season's first is next: its recap tells this season.
        let sequel = || {
            let show = backend.db.show(backend.db.episode(episode_id).ok()??.show_id).ok()??;
            backend.db.episodes(crate::series::next_show(backend, &show)?.id).ok()?.into_iter().next()
        };
        if let Some(next) = next.or_else(sequel) {
            let _ = recap(backend, next.id, true, None);
        }
    }
    if let Some(at) = resume_at {
        let _ = recap(backend, episode_id, true, (at >= RESUME_FROM_S).then_some(at));
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
        assert!(!p.contains("また？") && !p.contains("うん"));
        assert!(p.contains("in English") && p.contains("Episode 3"));
    }

    #[test]
    fn a_follow_up_carries_the_line_its_explanation_and_the_questions_before() {
        let lines = vec![line(1.0, 2.0, "ナツ！"), line(5.0, 6.0, "腹減った…"), line(7.0, 8.0, "また？")];
        let context = line_in_context("『FAIRY TAIL』", 3, &lines, 1, "");
        let thread = vec![Exchange { q: "Why no は?".into(), a: "It’s dropped in casual speech.".into() }];
        let p = follow_up_prompt(&context, "I’m hungry…\n腹 (はら): belly", &thread, " Is 腹 rude? ", "English");
        assert!(p.contains("<line>腹減った…</line>") && p.contains("<before>\nナツ！\n</before>"));
        assert!(!p.contains("また？"));
        assert!(p.contains("<explanation>\nI’m hungry…\n腹 (はら): belly\n</explanation>"));
        assert!(p.contains("<question>Why no は?</question>\nYou answered:\n<answer>\nIt’s dropped in casual speech.\n</answer>"));
        assert!(p.contains("Now I ask:\n<question>Is 腹 rude?</question>") && p.contains("in English"));
    }

    #[test]
    fn gist_break_it_down_and_nuance_each_carry_the_line_and_its_explanation() {
        let lines = vec![line(1.0, 2.0, "ナツ！"), line(5.0, 6.0, "腹減った…"), line(7.0, 8.0, "また？")];
        let context = line_in_context("『FAIRY TAIL』", 3, &lines, 1, "");
        let explanation = "I’m hungry…\n減る (へる): to decrease";
        let gist = explain_more_prompt(&context, explanation, "gist", "English").unwrap();
        let breakdown = explain_more_prompt(&context, explanation, "breakdown", "simple Japanese").unwrap();
        let nuance = explain_more_prompt(&context, explanation, "nuance", "English").unwrap();
        for p in [&gist, &breakdown, &nuance] {
            assert!(p.contains("<line>腹減った…</line>") && p.contains("<before>\nナツ！\n</before>"));
            assert!(!p.contains("また？"));
            assert!(p.contains("<explanation>\nI’m hungry…\n減る (へる): to decrease\n</explanation>"));
        }
        assert!(gist.contains("just the gist") && gist.contains("a sentence or two") && gist.contains("in English"));
        assert!(breakdown.contains("every word, particle and ending") && breakdown.contains("dictionary form") && breakdown.contains("in simple Japanese"));
        assert!(nuance.contains("tone") && nuance.contains("polite") && nuance.contains("cultural"));
        assert_eq!(explain_more_prompt(&context, explanation, "summary", "English"), None);
    }

    #[test]
    fn a_button_s_answer_is_kept_for_the_line_in_the_language_it_was_written_in() {
        let lines: Vec<Line> = (0..3).map(|i| line(i as f64 * 3.0, i as f64 * 3.0 + 2.0, &format!("台詞{i}"))).collect();
        let (backend, episodes, dir) = library("more", 1, &lines);
        let id = episodes[0].id;
        backend.db.save_answer(id, "line.gist", 1, "台詞1", "A line.", "English").unwrap();
        // Pressing Gist again is instant, in English as it was written.
        let view = explain_more(&backend, id, 1, "", "gist").unwrap();
        assert_eq!((view["text"].as_str(), view["done"].as_bool()), (Some("A line."), Some(true)));
        assert_eq!(kept_more(&backend, id, 1, "gist", "台詞1", "English").as_deref(), Some("A line."));
        // Not when Japanese answers are asked for, for another button or line, or once the line reads differently.
        assert_eq!(kept_more(&backend, id, 1, "gist", "台詞1", "simple Japanese"), None);
        assert_eq!(kept_more(&backend, id, 1, "nuance", "台詞1", "English"), None);
        assert_eq!(kept_more(&backend, id, 2, "gist", "台詞2", "English"), None);
        assert_eq!(kept_more(&backend, id, 1, "gist", "台詞一", "English"), None);
        // With Settings → Claude on simple Japanese, the answer kept in Japanese comes back.
        backend.db.set_setting("claude.language", &json!("ja")).unwrap();
        backend.db.save_answer(id, "line.nuance", 1, "台詞1", "ていねいです。", "simple Japanese").unwrap();
        assert_eq!(explain_more(&backend, id, 1, "", "nuance").unwrap()["text"], "ていねいです。");
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recaps_cover_the_episodes_before_in_arcs() {
        let lines: Vec<Line> = (0..30).map(|i| line(i as f64 * 3.0, i as f64 * 3.0 + 2.0, &format!("台詞{i}"))).collect();
        let window: Vec<(i64, &[Line])> = vec![(3, &lines[..]), (4, &lines[..])];
        let p = recap_prompt("『FAIRY TAIL』", 5, &window, None, RecapOf::NextEpisode, "", "English");
        assert!(p.contains("<episode number=\"3\">\n[0] 台詞0") && p.contains("<episode number=\"4\">"));
        assert!(p.contains("Episodes 3 to 4") && !p.contains("<earlier>"));
        assert!(p.contains("\"arcs\"") && p.contains("Episode 4, the one I just left") && p.contains("(not a preview of the next episode)"));
        // Claude counts the scenes from what it told of Episode 4, up to as many as its 1½ minutes allow.
        assert!(p.contains("a scene for each thing you told in \"last\" that the story isn’t done with when the episode ends"));
        assert!(p.contains("How many follows what happened, not the most allowed: just the one near its end if nothing else is left open, and at most 1."));
        // And from the story before, from the subtitles it has.
        assert!(p.contains("For the story before Episode 4: a scene for each of its arcs that the story isn’t done with when the episode ends"));
        assert!(p.contains("at most 3. Take them from the subtitles above.") && p.contains("under 400 words"));
        // With only the episode just left and no story before it, there's none to pick from.
        let p = recap_prompt("『FAIRY TAIL』", 2, &window[1..], None, RecapOf::NextEpisode, "", "English");
        assert!(!p.contains("For the story before"));
        let answer = "Sure:\n{\"arcs\": [{\"title\": \"The Lullaby flute\", \"from\": 3, \"to\": 4, \"summary\": \"Erigor steals Lullaby.\"}], \"last\": \"Natsu wins.\", \"moments\": [{\"episode\": 3, \"line\": 2, \"caption\": \"Erigor takes the flute.\"}, {\"episode\": 4, \"line\": 20}]}";
        let recap = parse_recap(answer).unwrap();
        assert_eq!(recap.arcs[0].title, "The Lullaby flute");
        assert_eq!((recap.arcs[0].from, recap.arcs[0].to), (3, 4));
        assert_eq!(recap.last, "Natsu wins.");
        let pick = |episode, line, caption: &str| Pick { episode, line, caption: caption.into() };
        assert_eq!(recap.moments, vec![pick(3, 2, "Erigor takes the flute."), pick(4, 20, "")]);
        assert!(recap.text().contains("The Lullaby flute: Erigor steals Lullaby.") && recap.text().contains("Last time: Natsu wins."));
        assert_eq!(parse_recap("no json here"), None);
        // Eleven moments are junk, and become ten, keeping the first and the last.
        let picks: Vec<String> = (0..11).map(|i| format!("{{\"episode\": {}, \"line\": {}}}", 1 + i / 3, i * 30)).collect();
        let eleven = format!("{{\"last\": \"r\", \"moments\": [{}]}}", picks.join(", "));
        let kept: Vec<(i64, usize)> = parse_recap(&eleven).unwrap().moments.iter().map(|m| (m.episode, m.line)).collect();
        assert_eq!(kept, vec![(1, 0), (1, 30), (1, 60), (2, 90), (2, 120), (3, 180), (3, 210), (3, 240), (4, 270), (4, 300)]);
    }

    #[test]
    fn one_kept_of_several_is_the_last() {
        assert_eq!(at_most(vec![1, 2, 3], 1), vec![3]);
        assert_eq!(at_most(vec![1, 2, 3], 0), Vec::<i32>::new());
        assert_eq!(at_most(vec![1, 2, 3, 4, 5], 3), vec![1, 3, 5]);
    }

    #[test]
    fn the_scenes_from_the_episode_you_left_follow_how_long_it_ran() {
        // Lines every 20 seconds, up to `minutes` in.
        let ran = |minutes: f64| -> Vec<Line> { (0..(minutes * 3.0) as usize).map(|i| line(i as f64 * 20.0 + 18.0, i as f64 * 20.0 + 20.0, "台詞")).collect() };
        for (minutes, most) in [(2.0, 1), (6.7, 1), (8.0, 1), (12.0, 2), (24.0, 4), (45.0, 4)] {
            assert_eq!(latest_most(&ran(minutes), false), most, "{minutes} minutes");
        }
        assert_eq!(latest_most(&[], false), 1);
        // A film so far: one for every 10 minutes, up to 6.
        for (minutes, most) in [(4.0, 1), (15.0, 2), (40.0, 4), (55.0, 6), (100.0, 6)] {
            assert_eq!(latest_most(&ran(minutes), true), most, "a film {minutes} minutes in");
        }
    }

    #[test]
    fn a_film_is_recapped_as_the_film_so_far() {
        // Stopped 40 minutes into a film.
        let lines: Vec<Line> = (0..120).map(|i| line(i as f64 * 20.0 + 18.0, i as f64 * 20.0 + 20.0, &format!("台詞{i}"))).collect();
        let window: Vec<(i64, &[Line])> = vec![(1, &lines[..])];
        let p = recap_prompt("『となりのトトロ』", 1, &window, None, RecapOf::FilmSoFar, "", "English");
        assert!(p.starts_with("I’m coming back to 『となりのトトロ』, an anime film in Japanese, part-way through it. Here are its Japanese subtitles as far as I watched"));
        assert!(!p.contains("Episode 1") && !p.contains("episode it covers") && !p.contains("Carry on from"));
        assert!(p.contains("<episode number=\"1\">\n[0] 台詞0") && p.contains("{\"episode\": 1, \"line\": 12"));
        assert!(p.contains("\"arcs\": the film so far, cut into its parts") && p.contains("\"from\" and \"to\" both 1"));
        assert!(p.contains("\"last\": where the film is when I stopped"));
        assert!(p.contains("a scene for each thing in the film so far that its story isn’t done with where I stopped (who someone is or what they want,"));
        assert!(p.contains("How many follows what has happened, not the most allowed: at most 4. Return the picks in line order."));
        assert!(p.contains("If you recognise the film, still tell me only what they say"));
    }

    #[test]
    fn every_recap_keeps_to_the_subtitles_so_nothing_ahead_is_spoiled() {
        let lines: Vec<Line> = (0..30).map(|i| line(i as f64 * 3.0, i as f64 * 3.0 + 2.0, &format!("台詞{i}"))).collect();
        let window: Vec<(i64, &[Line])> = vec![(4, &lines[..])];
        for of in [RecapOf::NextEpisode, RecapOf::EpisodeSoFar, RecapOf::FilmSoFar] {
            let p = recap_prompt("『FAIRY TAIL』", 4, &window, None, of, "", "English");
            assert!(p.contains("choose the scenes only by what’s in the subtitles: what you know of it from elsewhere could spoil what’s ahead"), "{of:?}");
            assert!(p.contains("Use only the subtitles") && p.contains("don’t guess at what comes next"), "{of:?}");
        }
    }

    #[test]
    fn a_pick_from_an_episode_claude_didn_t_read_goes() {
        let pick = |episode, line| Pick { episode, line, caption: String::new() };
        let earlier = Recap { moments: vec![pick(2, 7)], ..Recap::default() };
        let picks = vec![pick(1, 40), pick(2, 7), pick(2, 8), pick(5, 3), pick(6, 12), pick(7, 1)];
        // Episodes 5 and 6 were read; Episode 2's line 7 was carried from the earlier recap.
        let kept: Vec<(i64, usize)> = seen_picks(picks.clone(), &[5, 6], Some(&earlier)).iter().map(|p| (p.episode, p.line)).collect();
        assert_eq!(kept, vec![(2, 7), (5, 3), (6, 12)]);
        let kept: Vec<(i64, usize)> = seen_picks(picks, &[5, 6], None).iter().map(|p| (p.episode, p.line)).collect();
        assert_eq!(kept, vec![(5, 3), (6, 12)]);
    }

    #[test]
    fn a_recap_is_read_from_an_answer_with_braces_in_the_words_around_it() {
        let json = "{\"arcs\": [{\"title\": \"Roman\", \"from\": 1, \"to\": 1, \"summary\": \"Roman films {everything}.\"}], \"last\": \"Talia runs.\", \"moments\": [{\"episode\": 1, \"line\": 9}]}";
        for answer in [
            format!("Sure {{as asked}}, here it is:\n{json}"),
            format!("{json}\n\nI left out the {{opening}}."),
            format!("```json\n{json}\n```"),
            format!("The shape was {{\"a\": 1}}, so: {json}"),
        ] {
            let recap = parse_recap(&answer).unwrap_or_else(|| panic!("{answer}"));
            assert_eq!((recap.arcs[0].summary.as_str(), recap.last.as_str(), recap.moments.len()), ("Roman films {everything}.", "Talia runs.", 1), "{answer}");
        }
        assert_eq!(parse_recap("Roman films the city. {I couldn't make it JSON.}"), None);
    }

    #[test]
    fn recaps_carry_on_from_the_story_before() {
        let lines: Vec<Line> = (0..30).map(|i| line(i as f64 * 3.0, i as f64 * 3.0 + 2.0, &format!("台詞{i}"))).collect();
        let earlier = parse_recap("{\"arcs\": [{\"title\": \"Lucy joins\", \"from\": 1, \"to\": 4, \"summary\": \"Lucy joined Fairy Tail.\"}], \"last\": \"Natsu wins.\", \"moments\": [{\"episode\": 2, \"line\": 7, \"caption\": \"Lucy gets her mark.\"}]}").unwrap();
        // One episode since the kept recap: only its subtitles go, with the story before.
        let window: Vec<(i64, &[Line])> = vec![(5, &lines[..])];
        let p = recap_prompt("『FAIRY TAIL』", 6, &window, Some(&earlier), RecapOf::NextEpisode, "", "English");
        assert!(p.contains("subtitles of Episode 5,") && !p.contains("<episode number=\"4\">"));
        assert!(p.contains("<earlier>\n{\"arcs\":[{\"title\":\"Lucy joins\",\"from\":1,\"to\":4"));
        assert!(p.contains("Lucy gets her mark.") && p.contains("Carry on from the earlier story"));
        // The story before's scenes come from the earlier moments as they were, or from Episode 5: never made up.
        assert!(p.contains("For the story before Episode 5: a scene for each of its arcs that the story isn’t done with when the episode ends"));
        assert!(p.contains(
            "Take them from the earlier moments, with the episode, line and caption they had, or from the subtitles above: never a line of an episode whose subtitles aren’t here."
        ));
        // Part-way through Episode 5: the story so far in it, up to where it was left.
        let p = recap_prompt("『FAIRY TAIL』", 5, &window, Some(&earlier), RecapOf::EpisodeSoFar, "", "English");
        assert!(p.contains("coming back to Episode 5") && p.contains("what has happened so far in Episode 5"));
        assert!(p.contains("the scenes to play before I pick up again") && p.contains("From Episode 5 as far as I watched: a scene for each thing"));
        assert!(p.contains("isn’t done with where I stopped") && p.contains("the last of them near where I stopped"));
        assert!(p.contains("just the one near where I stopped if nothing else is open, and at most 1."));
        assert!(p.contains("For the story before Episode 5: a scene for each of its arcs that the story isn’t done with where I stopped"));
        assert!(!p.contains("when the episode ends") && !p.contains("before I watch"));
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

    #[test]
    fn what_comes_after_the_ending_is_told_only_once_you_ve_played_past_it() {
        // Lines all through a 1400 s episode, the last ones after the credits (1260–1350 s).
        let lines: Vec<Line> = (0..46).map(|i| line(i as f64 * 30.0, i as f64 * 30.0 + 2.0, &format!("台詞{i}"))).collect();
        let (backend, episodes, dir) = library("after-credits", 2, &lines);
        let told = |backend: &Backend| source(backend, &episodes[1], None).unwrap().episodes[0].1.len();
        // No ending known: every line.
        assert_eq!(told(&backend), 46);
        // AniSkip's ending at 1260–1350: closed at the end card, the scene after it isn't told.
        let skips = crate::aniskip::Skips { opening: None, ending: Some((1260.0, 1350.0)) };
        backend.db.set_show_anilist(episodes[0].show_id, &crate::db::NewShow { title: "Show".into(), anilist_id: Some(5), ..Default::default() }).unwrap();
        assert!(backend.db.set_opening_if(episodes[0].id, 1, 5, skips).unwrap());
        backend.db.reach(episodes[0].id, 1261.0).unwrap();
        assert_eq!(told(&backend), 42);
        // A recap made from that is carried on from, though the file has lines after the ending.
        keep(&backend, &episodes[1], &Recap { last: "Roman runs.".into(), ..Recap::default() });
        assert!(kept(&backend, episodes[1].id).is_some());
        // Played on past the credits: it is.
        backend.db.reach(episodes[0].id, 1380.0).unwrap();
        assert_eq!(told(&backend), 46);
        // And that recap doesn't tell the scene after the credits: it isn't carried on from any more.
        assert!(kept(&backend, episodes[1].id).is_none());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_seasons_first_episode_is_recapped_from_the_season_before_and_no_other_episode_is() {
        let lines: Vec<Line> = (0..30).map(|i| line(i as f64 * 3.0, i as f64 * 3.0 + 2.0, &format!("台詞{i}"))).collect();
        let (backend, first, dir) = library("seasons", 3, &lines);
        // A second season of two episodes, which AniList says carries on from the first.
        let add = |title: &str, count: i64| {
            let episodes = (1..=count).map(|n| crate::db::NewEpisode { number: n, path: format!("{title}/{n}.mkv"), size: 1, duration: 1400.0 }).collect();
            backend.db.add_show(&crate::db::NewShow { folder: title.into(), title: title.into(), anilist_id: Some(2), episodes, ..Default::default() }).unwrap()
        };
        let second = backend.db.episodes(add("Show II", 2)).unwrap();
        for e in &second {
            jobs::write_lines(&backend, e.id, &lines).unwrap();
        }
        backend.db.set_show_anilist(first[0].show_id, &crate::db::NewShow { folder: "Show".into(), title: "Show".into(), anilist_id: Some(1), ..Default::default() }).unwrap();
        let entries = vec![
            crate::series::tests::entry(1, "Show", "TV", 3, 20200101, &[("SEQUEL", 2)]),
            crate::series::tests::entry(2, "Show II", "TV", 2, 20210101, &[("PREQUEL", 1)]),
        ];
        std::fs::write(dir.join("series.json"), serde_json::to_vec(&entries).unwrap()).unwrap();

        // None of the first season watched here: nothing to tell it from.
        assert!(source(&backend, &second[0], None).unwrap().episodes.is_empty());
        // Two of its three watched: the story as far as that, and no further.
        backend.db.set_watched(first[0].id, true).unwrap();
        backend.db.set_watched(first[1].id, true).unwrap();
        let before = source(&backend, &second[0], None).unwrap();
        assert_eq!(before.episodes.iter().map(|(e, _)| e.id).collect::<Vec<_>>(), [first[0].id, first[1].id]);
        assert_eq!(before.of.as_ref().map(|s| s.title.as_str()), Some("Show"));
        let p = recap_prompt("『Show』", 3, &[(1, &lines[..]), (2, &lines[..])], None, RecapOf::SeasonBefore, "", "English");
        assert!(p.contains("I’ve watched 『Show』") && p.contains("about to start the season that carries on from it"));
        assert!(p.contains("Episode 2, the last of it I watched") && p.contains("what you know of it from elsewhere could spoil what’s ahead"));

        // Its recap is kept as the one before the second season's first episode, and plays the first season's scenes.
        let summary = Recap { last: "The season ends.".into(), moments: vec![Pick { episode: 2, line: 5, caption: String::new() }], ..Recap::default() };
        keep(&backend, &second[0], &summary);
        let view = super::recap(&backend, second[0].id, false, None).unwrap();
        assert_eq!((view["season"].as_str(), view["moments"][0]["episodeId"].as_i64()), (Some("Show"), Some(first[1].id)));
        // The second episode's recap is of the second season alone: it doesn't carry the first season's on.
        let own = source(&backend, &second[1], None).unwrap();
        assert_eq!((own.episodes.iter().map(|(e, _)| e.id).collect::<Vec<_>>(), own.earlier.is_none(), own.of.is_none()), (vec![second[0].id], true, true));
        // Nor does picking the first episode up part-way.
        let part_way = source(&backend, &second[0], Some(600.0)).unwrap();
        assert!(part_way.earlier.is_none() && part_way.of.is_none());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Keeps `recap` as the one before `episode`, as if Claude had just written it.
    fn keep(backend: &Backend, episode: &Episode, recap: &Recap) {
        let about = recap_about(&source(backend, episode, None).unwrap());
        backend.db.save_answer(episode.id, "recap", -1, &about, &serde_json::to_string(recap).unwrap(), "").unwrap();
    }

    #[test]
    fn an_explanation_or_recap_kept_in_english_isn_t_shown_once_answers_are_in_japanese() {
        let lines: Vec<Line> = (0..30).map(|i| line(100.0 + i as f64 * 3.0, 102.0 + i as f64 * 3.0, &format!("台詞{i}"))).collect();
        let (backend, episodes, dir) = library("language", 2, &lines);
        backend.db.save_answer(episodes[0].id, "line", 1, "台詞1", "A line.", LINE_ANSWER_VERSION).unwrap();
        keep(&backend, &episodes[1], &Recap { last: "Roman runs.".into(), ..Recap::default() });
        assert_eq!(explain(&backend, episodes[0].id, 1).unwrap()["text"], "A line.");
        assert_eq!(recap_state(&backend, &episodes[1], None)["ready"], true);
        assert!(kept(&backend, episodes[1].id).is_some());

        backend.db.set_setting("claude.language", &json!("ja")).unwrap();
        assert_eq!(super::recap(&backend, episodes[1].id, false, None).unwrap()["done"], false);
        assert_eq!(recap_state(&backend, &episodes[1], None)["ready"], false);
        assert!(kept(&backend, episodes[1].id).is_none());
        backend.db.save_answer(episodes[0].id, "line", 1, "台詞1", "セリフです。", &kept_with(LINE_ANSWER_VERSION, "simple Japanese")).unwrap();
        assert_eq!(explain(&backend, episodes[0].id, 1).unwrap()["text"], "セリフです。");
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
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
        // From just before line 10 is heard to just after the exchange with lines 11 and 12.
        assert_eq!((tenths(&clip["start"]), tenths(&clip["end"])), (1312, 1409), "{clip}");
        // Its lines are as the file has them, and the player shows them nudged, as in the episode.
        assert_eq!((clip["lines"][0]["start"].as_f64(), clip["nudgeMs"].as_i64()), (Some(130.0), Some(2000)));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_moment_that_runs_into_the_one_before_carries_on_from_it_instead_of_playing_its_lines_again() {
        let mut lines: Vec<Line> = (0..30).map(|i| line(100.0 + i as f64 * 3.0, 102.0 + i as f64 * 3.0, &format!("台詞{i}"))).collect();
        // The episode runs to 22 minutes, so it plays up to 4 scenes.
        lines.push(line(1300.0, 1302.0, "またね"));
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
        assert_eq!(clips, vec![(1292, 1389, "Asked."), (1389, 1419, "Answered."), (1742, 1839, "Later.")]);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn moments_run_on_through_close_lines_and_merge() {
        let mut sign = line(11.0, 20.0, "a sign");
        sign.sign = true;
        let lines = vec![line(10.0, 12.0, "a"), sign, line(12.5, 14.0, "b"), line(30.0, 31.0, "c"), line(31.2, 32.0, "d")];
        let m = moments(&lines, &[0, 4, 3, 9]);
        let tenths = |x: f64| (x * 10.0).round() as i64;
        assert_eq!(m.len(), 2);
        assert_eq!((tenths(m[0].start), tenths(m[0].end), m[0].lines.len()), (92, 149, 3));
        assert_eq!((tenths(m[1].start), tenths(m[1].end), m[1].lines.len()), (292, 340, 2));
    }

    /// Where each clip's first line starts, and whether it's from the episode you left.
    fn played(view: &Value) -> Vec<(f64, bool)> {
        view["moments"].as_array().unwrap().iter().map(|c| (c["lines"][0]["start"].as_f64().unwrap(), c["latest"].as_bool().unwrap())).collect()
    }

    #[test]
    fn saved_recaps_play_a_scene_for_every_six_minutes_of_the_last_episode_in_order() {
        let lines: Vec<Line> = (0..30).map(|i| line(100.0 + i as f64 * 12.0, 101.0 + i as f64 * 12.0, &format!("台詞{i}"))).collect();
        let (backend, episodes, dir) = library("pacing", 3, &lines);
        let pick = |episode, line| Pick { episode, line, caption: String::new() };
        let recap = Recap {
            last: "Last time.".into(),
            moments: vec![pick(2, 25), pick(2, 5), pick(1, 10), pick(2, 20), pick(2, 15), pick(2, 10), pick(2, 26), pick(3, 12)],
            ..Recap::default()
        };
        // Stopped 6.7 minutes into Episode 2: one scene from it, the last, after Episode 1's.
        let at = 402.0;
        let source = source(&backend, &episodes[1], Some(at)).unwrap();
        let view = recap_view(&backend, episodes[1].show_id, &source, &recap, Some(at), false).unwrap();
        assert_eq!(played(&view), vec![(220.0, false), (400.0, true)]);
        assert_eq!(view["moments"][1]["end"], at);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);

        // A 23-minute episode before: four of the six scenes picked from it, the first and last among them.
        let lines: Vec<Line> = (0..30).map(|i| line(60.0 + i as f64 * 45.0, 61.0 + i as f64 * 45.0, &format!("台詞{i}"))).collect();
        let (backend, episodes, dir) = library("pacing-long", 2, &lines);
        let recap = Recap { last: "Last time.".into(), moments: [2, 8, 14, 20, 26, 29].map(|l| pick(1, l)).to_vec(), ..Recap::default() };
        keep(&backend, &episodes[1], &recap);
        let view = super::recap(&backend, episodes[1].id, false, None).unwrap();
        assert_eq!(played(&view), [2, 14, 20, 29].map(|l| (60.0 + l as f64 * 45.0, true)).to_vec());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn past_a_minute_and_a_quarter_the_story_before_gives_way_and_the_last_scene_always_plays() {
        // Long lines, so each scene plays for 12.7 seconds, in a 23-minute episode.
        let lines: Vec<Line> = (0..30).map(|i| line(60.0 + i as f64 * 45.0, 71.0 + i as f64 * 45.0, &format!("台詞{i}"))).collect();
        let (backend, episodes, dir) = library("within", 4, &lines);
        let pick = |episode, line| Pick { episode, line, caption: String::new() };
        let at = |l: usize| 60.0 + l as f64 * 45.0;
        // Three scenes from Episode 3, the one left (38 s), and three from before it (38 s more).
        let recap = Recap { last: "Last time.".into(), moments: vec![pick(1, 5), pick(2, 10), pick(2, 20), pick(3, 3), pick(3, 20), pick(3, 28)], ..Recap::default() };
        keep(&backend, &episodes[3], &recap);
        let view = super::recap(&backend, episodes[3].id, false, None).unwrap();
        assert_eq!(played(&view), vec![(at(5), false), (at(20), false), (at(3), true), (at(20), true), (at(28), true)]);
        // Four from Episode 3 run past 45 seconds: the oldest in the middle goes.
        let recap = Recap { moments: vec![pick(3, 3), pick(3, 10), pick(3, 20), pick(3, 28)], ..recap };
        keep(&backend, &episodes[3], &recap);
        let view = super::recap(&backend, episodes[3].id, false, None).unwrap();
        assert_eq!(played(&view), vec![(at(3), true), (at(20), true), (at(28), true)]);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
        // However long the scenes, the last stays; the story before can go altogether.
        let clip = |episode, start: f64| Clip { episode, start, end: start + 30.0, view: json!(start) };
        let starts = |clips: Vec<Clip>| clips.iter().map(|c| c.start).collect::<Vec<_>>();
        assert_eq!(starts(keep_within(vec![clip(3, 0.0), clip(3, 100.0), clip(3, 200.0)], 4, 45.0, 1)), vec![200.0]);
        assert_eq!(starts(keep_within(vec![clip(1, 0.0), clip(2, 0.0)], 3, 20.0, 0)), Vec::<f64>::new());
        // Clips that run into each other count once.
        assert_eq!(length(&[clip(3, 0.0), clip(3, 20.0)]), 50.0);
        assert_eq!(length(&[clip(2, 0.0), clip(3, 20.0)]), 60.0);
    }

    #[test]
    fn picking_up_in_the_first_5_minutes_recaps_the_episode_before() {
        let lines: Vec<Line> = (0..30).map(|i| line(100.0 + i as f64 * 3.0, 102.0 + i as f64 * 3.0, &format!("台詞{i}"))).collect();
        let (backend, episodes, dir) = library("early", 2, &lines);
        // Four minutes into Episode 2, its 30 lines would be a recap of the cold open: there's none up to there.
        assert_eq!(recap_state(&backend, &episodes[1], Some(240.0)), json!({ "possible": false }));
        assert_eq!(recap_state(&backend, &episodes[1], Some(400.0)), json!({ "possible": true, "ready": false }));
        // The player opens on the recap before it, Episode 1's, and plays its Last time and scenes.
        keep(&backend, &episodes[1], &Recap { last: "Roman runs.".into(), moments: vec![Pick { episode: 1, line: 10, caption: "Roman runs.".into() }], ..Recap::default() });
        backend.db.save_position(episodes[1].id, 240.0).unwrap();
        let view = backend.call("openPlayer", json!({ "id": episodes[1].id })).unwrap();
        assert_eq!((&view["resume"]["possible"], &view["recap"]), (&json!(false), &json!({ "possible": true, "ready": true })));
        let recap = super::recap(&backend, episodes[1].id, false, None).unwrap();
        assert_eq!((&recap["last"], &recap["lastNumber"], played(&recap)), (&json!("Roman runs."), &json!(1), vec![(130.0, true)]));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_film_plays_scenes_from_all_of_it_so_far_whenever_you_come_back() {
        // A film of one file, stopped 40 minutes in, with a scene picked every 5 minutes.
        let lines: Vec<Line> = (0..150).map(|i| line(10.0 + i as f64 * 20.0, 12.0 + i as f64 * 20.0, &format!("台詞{i}"))).collect();
        let dir = std::env::temp_dir().join(format!("telly-recap-film-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Arc::new(Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        let episodes = vec![crate::db::NewEpisode { number: 1, path: "film.mkv".into(), size: 1, duration: 3000.0 }];
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Film".into(), title: "Film".into(), format: "MOVIE".into(), episodes, ..Default::default() }).unwrap();
        let film = backend.db.episodes(show).unwrap().remove(0);
        jobs::write_lines(&backend, film.id, &lines).unwrap();
        let picks: Vec<Pick> = (0..8).map(|k| Pick { episode: 1, line: 15 * k, caption: String::new() }).collect();
        let at = 2400.0;
        let about = recap_about(&source(&backend, &film, Some(at)).unwrap());
        backend.db.save_answer(film.id, "resume", -1, &about, &serde_json::to_string(&Recap { last: "So far.".into(), moments: picks, ..Recap::default() }).unwrap(), "").unwrap();
        let view = super::recap(&backend, film.id, false, Some(at)).unwrap();
        // Four of the eight, from its start to where it stopped, each to play even back within 3 days.
        let at_line = |l: usize| 10.0 + l as f64 * 20.0;
        assert_eq!(played(&view), [0, 30, 75, 105].map(|l| (at_line(l), true)).to_vec());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
