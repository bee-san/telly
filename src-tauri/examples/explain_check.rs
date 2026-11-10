//! Times Explain through Telly's warm Claude Code session: starts one, waits
//! for it, then asks two questions in it the way the player does.
//!
//!   cargo run --profile measure --example explain_check

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use telly_lib::{ask, claude};

fn main() -> anyhow::Result<()> {
    let model = claude::DEFAULT_MODEL;
    let started = Instant::now();
    claude::warm_up(Some(model), Some(ask::EFFORT));
    // The player opens long before anyone presses E.
    std::thread::sleep(Duration::from_secs(25));
    eprintln!("warmed for {:.0}s", started.elapsed().as_secs_f64());
    for line in ["彼とて、悪意があったわけじゃないんだからね", "腹は減ったし、お金もないもんね"] {
        let prompt = format!("Explain this line for a learner, in English, in under 60 words: first a translation, then one note.\n<line>{line}</line>");
        let asked = Instant::now();
        let mut first: Option<f64> = None;
        let question = claude::Ask { prompt: &prompt, model: Some(model), effort: Some(ask::EFFORT), warm: true };
        let answer = claude::ask(&question, |_| { first.get_or_insert(asked.elapsed().as_secs_f64()); }, &Arc::new(AtomicBool::new(false)))?;
        println!("{line}\n  first text {:.1}s, done {:.1}s\n  {}\n", first.unwrap_or(-1.0), asked.elapsed().as_secs_f64(), answer.replace('\n', "\n  "));
    }
    Ok(())
}
