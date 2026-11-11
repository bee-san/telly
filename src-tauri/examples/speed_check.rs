//! Times the requests the pages make, against a copy of a library.
//!
//!   cargo run --release --example speed_check -- <data dir> [show id] [episode id]
//!
//! The background work isn't started, so nothing is converted or fetched.
//! Give it a copy: opening a library can requeue its failed subtitles.

use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use telly_lib::backend::{Backend, Dirs};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::path::absolute(&args[1])?;
    let show: i64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(1);
    let episode: i64 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(1);
    let started = Instant::now();
    let backend = Arc::new(Backend::open(Dirs { local: dir.clone(), roaming: dir })?);
    println!("{:>9.2} ms  open", started.elapsed().as_secs_f64() * 1000.0);
    let calls: Vec<(&str, Value)> = vec![
        ("library", json!({})),
        ("activity", json!({})),
        ("show", json!({ "id": show })),
        ("episode", json!({ "id": episode })),
        ("showNames", json!({ "id": show })),
        ("settings", json!({})),
        ("status", json!({})),
        ("namesStatus", json!({})),
        ("ownHachidori", json!({})),
        ("claudeStatus", json!({})),
        ("premineStatus", json!({})),
        ("recentCards", json!({})),
        ("events", json!({ "since": 0 })),
    ];
    for (cmd, args) in calls {
        // The first time and the best of the next five: pages ask again and again.
        let mut times = Vec::new();
        let mut size = 0;
        for _ in 0..6 {
            let started = Instant::now();
            let answer = backend.call(cmd, args.clone());
            times.push(started.elapsed().as_secs_f64() * 1000.0);
            size = match answer {
                Ok(v) => v.to_string().len(),
                Err(e) => {
                    println!("{cmd}: {e:#}");
                    0
                }
            };
        }
        let best = times[1..].iter().cloned().fold(f64::MAX, f64::min);
        println!("{:>9.2} ms first, {best:>9.2} ms after, {size:>8} bytes  {cmd}", times[0]);
    }
    Ok(())
}
