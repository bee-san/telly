//! Runs Telly's background work over a library until all of it is done, and
//! says when each episode's subtitles, seek-bar pictures and English lines
//! were ready, and what the title bar said meanwhile.
//!
//!   cargo run --profile measure --example jobs_check -- <data dir>
//!
//! Give it a scratch copy of a library: its episodes can point at the real
//! rips, which are only read. TELLY_JIMAKU_API points it at a stand-in for Jimaku.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use telly_lib::backend::{Backend, Dirs};
use telly_lib::db::Episode;
use telly_lib::{jobs, media};

/// How long a worker waits between looks for work when nothing wakes it:
/// with the title bar empty for longer, no worker has more to do.
const WORKERS_WAIT: f64 = 5.0;
/// With the title bar empty this long, what's still missing won't come.
const GIVE_UP: f64 = 60.0;

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

/// What's still to come for an episode: its subtitles, and once it's ready to
/// play, the seek bar's pictures and the rip's English lines (none when it has
/// no English text track).
fn missing(backend: &Backend, e: &Episode) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if matches!(e.subs_state.as_str(), "pending" | "working") {
        missing.push("subtitles");
    }
    if e.media_state == "ready" {
        if !jobs::sheet_path(backend, e.show_id, e.id).is_file() {
            missing.push("pictures");
        }
        let english = jobs::stored_probe(backend, e.id).is_some_and(|probe| media::english_text_track(&probe).is_some());
        if english && !jobs::translation_path(backend, e.id).is_file() {
            missing.push("English lines");
        }
    }
    missing
}

fn main() -> anyhow::Result<()> {
    let dir = std::path::absolute(std::env::args().nth(1).expect("usage: jobs_check <data dir>"))?;
    let backend = Arc::new(Backend::open(Dirs { local: dir.clone(), roaming: dir })?);
    backend.refresh_tools();
    let titles: HashMap<i64, String> = backend.db.shows()?.into_iter().map(|s| (s.id, s.title)).collect();
    let started = Instant::now();
    jobs::start(backend.clone());
    let mut said: HashSet<(i64, &str)> = HashSet::new();
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
        let mut left = Vec::new();
        for e in &episodes {
            let title = titles.get(&e.show_id).map(String::as_str).unwrap_or("");
            let missing = missing(&backend, e);
            if !missing.contains(&"subtitles") && said.insert((e.id, "subtitles")) {
                println!("{at:>7.1} s  {title} {}: {} ({}), video {}", e.number, e.subs_state, e.subs_detail, e.media_state);
            }
            if e.media_state == "ready" {
                let kept = |path: &Path| std::fs::read(path).ok().map(|b| b.len());
                if !missing.contains(&"pictures") && said.insert((e.id, "pictures")) {
                    println!("{at:>7.1} s  {title} {}: seek bar’s pictures", e.number);
                }
                if !missing.contains(&"English lines") && said.insert((e.id, "English")) {
                    let lines = match kept(&jobs::translation_path(&backend, e.id)) {
                        Some(n) if n > 2 => "English lines",
                        _ => "no English lines",
                    };
                    println!("{at:>7.1} s  {title} {}: {lines}", e.number);
                }
            }
            if !missing.is_empty() {
                left.push(format!("{title} {}: {}", e.number, missing.join(", ")));
            }
        }
        let quiet = at - busy_at;
        if (left.is_empty() && quiet > WORKERS_WAIT) || quiet > GIVE_UP {
            println!("{busy_at:.1} s for {} episodes", episodes.len());
            for l in &left {
                println!("still missing: {l}");
            }
            return Ok(());
        }
    }
}
