//! Writes one "Previously on" for real subtitles, to see what Claude makes of it.
//!
//!   cargo run --profile measure --example recap_check -- <subtitles dir> <episode number> <id>:<number> …
//!
//! Each `<id>:<number>` is an episode before it (Telly's id, for its subtitles
//! file <id>.json, and its number), oldest first. EARLIER=<file> is the recap
//! this printed for the episodes before them, to carry on from. AT=<seconds>
//! is for coming back part-way through the last of them (give its number as
//! the episode number), as far as that, and with FILM=1 it's a film. It
//! prints how many scenes Claude picked from each episode, and how long each plays.
//! OPENROUTER_KEY=<key> asks gpt-oss on OpenRouter instead, as Telly does
//! when Claude Code isn't installed.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use telly_lib::{ask, claude, openrouter, subs::Line};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&args[1]);
    let number: i64 = args[2].parse()?;
    let mut window: Vec<(i64, Vec<Line>)> = Vec::new();
    for pair in &args[3..] {
        let (id, n) = pair.split_once(':').unwrap();
        let lines: Vec<Line> = serde_json::from_slice(&std::fs::read(dir.join(format!("{id}.json")))?)?;
        window.push((n.parse()?, lines));
    }
    let at: Option<f64> = std::env::var("AT").ok().map(|s| s.parse()).transpose()?;
    if let (Some(at), Some((_, lines))) = (at, window.last_mut()) {
        lines.retain(|l| l.start < at);
    }
    let film = std::env::var("FILM").is_ok_and(|f| f == "1");
    let of = match at {
        Some(_) if film => ask::RecapOf::FilmSoFar,
        Some(_) => ask::RecapOf::EpisodeSoFar,
        None => ask::RecapOf::NextEpisode,
    };
    let refs: Vec<(i64, &[Line])> = window.iter().map(|(n, l)| (*n, l.as_slice())).collect();
    let earlier: Option<ask::Recap> = std::env::var("EARLIER").ok().map(|file| anyhow::Ok(serde_json::from_slice(&std::fs::read(file)?)?)).transpose()?;
    let prompt = ask::recap_prompt("『FAIRY TAIL』 (Fairy Tail)", number, &refs, earlier.as_ref(), of, "", "English");
    eprintln!("prompt: {} characters", prompt.chars().count());
    let started = std::time::Instant::now();
    let router_key = std::env::var("OPENROUTER_KEY").ok().filter(|k| !k.is_empty());
    let model = match &router_key {
        Some(_) => openrouter::CHAT_MODEL.to_string(),
        None => std::env::var("MODEL").unwrap_or_else(|_| claude::DEFAULT_MODEL.into()),
    };
    let mut first: Option<f64> = None;
    let on_text = |_: &str| { first.get_or_insert(started.elapsed().as_secs_f64()); };
    let stop = Arc::new(AtomicBool::new(false));
    let answer = match &router_key {
        Some(key) => openrouter::chat(&openrouter::api_address(), key, claude::SYSTEM, &prompt, Some(ask::EFFORT), on_text, &stop)?,
        None => claude::ask(&claude::Ask { prompt: &prompt, model: Some(&model), effort: Some(ask::EFFORT), warm: false }, on_text, &stop)?,
    };
    eprintln!("{model}: first text after {:.1}s, done in {:.1}s", first.unwrap_or(0.0), started.elapsed().as_secs_f64());
    match ask::parse_recap(&answer) {
        Some(recap) => {
            println!("{}", serde_json::to_string_pretty(&recap)?);
            if let Some((last, lines)) = window.last() {
                eprintln!("Episode {last} plays at most {} scenes", ask::latest_most(lines, of == ask::RecapOf::FilmSoFar));
            }
            for (n, lines) in &window {
                let picked: Vec<usize> = recap.moments.iter().filter(|p| p.episode == *n).map(|p| p.line).collect();
                let lengths: Vec<String> = ask::moments(lines, &picked).iter().map(|m| format!("{:.1}s", m.end - m.start)).collect();
                eprintln!("Episode {n}: {} scenes, playing {}", picked.len(), lengths.join(", "));
            }
            let before = recap.moments.iter().filter(|p| !window.iter().any(|(n, _)| *n == p.episode)).count();
            eprintln!("From the story before: {before} scenes");
        }
        None => println!("couldn't read:\n{answer}"),
    }
    Ok(())
}
