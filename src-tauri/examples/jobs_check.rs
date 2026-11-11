//! Runs Telly's background work over a library until every episode's
//! subtitles are settled and nothing more is going on, and says when each was
//! and what the title bar said meanwhile.
//!
//!   cargo run --profile measure --example jobs_check -- <data dir>
//!
//! Give it a scratch copy of a library: its episodes can point at the real
//! rips, which are only read. TELLY_JIMAKU_API points it at a stand-in for Jimaku.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use telly_lib::backend::{Backend, Dirs};
use telly_lib::jobs;

/// The title bar's words for an activity, as src/lib/TitleBar.svelte writes them.
fn title_bar(activity: &Value) -> String {
    let labels: Vec<&str> = activity["tasks"].as_array().into_iter().flatten().filter_map(|t| t["label"].as_str()).collect();
    let (done, total) = (activity["done"].as_u64().unwrap_or(0), activity["total"].as_u64().unwrap_or(0));
    let shown = match activity["shows"].as_u64() {
        _ if labels.is_empty() => String::new(),
        _ if total == 0 || done >= total => labels[0].to_string(),
        Some(shows) if shows > 1 => format!("Getting {shows} shows ready · {done} of {total}"),
        _ => format!("Getting {} ready · {done} of {total}", activity["title"].as_str().unwrap_or("")),
    };
    format!("{shown}  [{}]", labels.join(" | "))
}

fn main() -> anyhow::Result<()> {
    let dir = std::path::absolute(std::env::args().nth(1).expect("usage: jobs_check <data dir>"))?;
    let backend = Arc::new(Backend::open(Dirs { local: dir.clone(), roaming: dir })?);
    backend.refresh_tools();
    let titles: HashMap<i64, String> = backend.db.shows()?.into_iter().map(|s| (s.id, s.title)).collect();
    let started = Instant::now();
    jobs::start(backend.clone());
    let mut settled = HashSet::new();
    let mut bar = String::new();
    // When the title bar last said something: work may start a moment after the last ended.
    let mut busy_at = 0.0;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let at = started.elapsed().as_secs_f64();
        let activity = backend.call("activity", json!({}))?;
        let now = title_bar(&activity);
        if activity["tasks"].as_array().is_some_and(|t| !t.is_empty()) {
            busy_at = at;
        }
        if now != bar {
            println!("{at:>7.1} s  title bar: {now}");
            bar = now;
        }
        let episodes = backend.db.all_episodes()?;
        for e in &episodes {
            if !matches!(e.subs_state.as_str(), "pending" | "working") && settled.insert(e.id) {
                let title = titles.get(&e.show_id).map(String::as_str).unwrap_or("");
                println!("{at:>7.1} s  {title} {}: {} ({}), video {}", e.number, e.subs_state, e.subs_detail, e.media_state);
            }
        }
        if settled.len() == episodes.len() && at - busy_at >= 3.0 {
            println!("{busy_at:.1} s for {} episodes", episodes.len());
            return Ok(());
        }
    }
}
