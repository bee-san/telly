//! Writes one "Previously on" for real subtitles, to see what Claude makes of it.
//!
//!   cargo run --profile measure --example recap_check -- <subtitles dir> <episode number> <id>:<number> …
//!
//! Each `<id>:<number>` is an episode before it (Telly's id, for its subtitles
//! file <id>.json, and its number), oldest first.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use telly_lib::{ask, claude, subs::Line};

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
    let refs: Vec<(i64, &[Line])> = window.iter().map(|(n, l)| (*n, l.as_slice())).collect();
    let prompt = ask::recap_prompt("『FAIRY TAIL』 (Fairy Tail)", number, &refs, "", "", "English");
    eprintln!("prompt: {} characters", prompt.chars().count());
    let started = std::time::Instant::now();
    let model = std::env::var("MODEL").unwrap_or_else(|_| claude::DEFAULT_MODEL.into());
    let question = claude::Ask { prompt: &prompt, model: Some(&model), effort: Some(ask::EFFORT), warm: false };
    let mut first: Option<f64> = None;
    let answer = claude::ask(&question, |_| { first.get_or_insert(started.elapsed().as_secs_f64()); }, &Arc::new(AtomicBool::new(false)))?;
    eprintln!("{model}: first text after {:.1}s, done in {:.1}s", first.unwrap_or(0.0), started.elapsed().as_secs_f64());
    match ask::parse_recap(&answer) {
        Some(recap) => println!("{}", serde_json::to_string_pretty(&recap)?),
        None => println!("couldn't read:\n{answer}"),
    }
    Ok(())
}
