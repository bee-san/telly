//! A dry run of Telly's subtitle matching over a real library, read-only:
//! for each episode, the files it would try (by Telly's number, the file's own
//! number, and that number after earlier season folders), each synced the way
//! the app syncs it, and the one it would keep.
//!
//!   cargo run --profile measure --example sync_real -- <telly.sqlite> <jimaku dir>[;<dir>…] <first>-<last> [all] [speech] [threads=N]
//!
//! `all` prints every file tried, not just the one kept; `speech` matches to
//! FireRedVAD's view of the speech even where the rip has its own subtitles.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use telly_lib::db::Episode;
use telly_lib::{jimaku, jobs, matching, media, sync, tools};

struct Outcome {
    number: i64,
    group: String,
    report: String,
    /// great, unsure, none fit, no files, error
    verdict: &'static str,
    against: String,
    seconds: f64,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let db = rusqlite::Connection::open_with_flags(&args[1], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let dirs: Vec<PathBuf> = args[2].split(';').map(PathBuf::from).collect();
    let (first, last) = args[3].split_once('-').map(|(a, b)| (a.parse::<i64>().unwrap(), b.parse::<i64>().unwrap())).unwrap();
    let all = args.iter().any(|a| a == "all");
    let speech = args.iter().any(|a| a == "speech");
    let threads: usize = args.iter().find_map(|a| a.strip_prefix("threads=")).and_then(|n| n.parse().ok()).unwrap_or(4);
    let folder: String = db.query_row("SELECT folder FROM shows WHERE id = (SELECT show_id FROM episodes WHERE number = ?1)", [first], |r| r.get(0))?;
    let mut stmt = db.prepare("SELECT number, path FROM episodes WHERE show_id = (SELECT show_id FROM episodes WHERE number = ?1) ORDER BY number")?;
    let listing: Vec<(i64, String)> = stmt.query_map([first], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let groups = matching::groups(&folder, &listing);
    let found = tools::find(Path::new("."), Path::new("."), "", "");
    let (ffmpeg, ffprobe) = (found.ffmpeg.expect("ffmpeg"), found.ffprobe.expect("ffprobe"));
    let locals: Vec<jimaku::Local> = dirs.iter().flat_map(|d| jimaku::cached(d)).collect();
    let eps: Vec<&(i64, String)> = listing.iter().filter(|(n, _)| (first..=last).contains(n)).collect();
    eprintln!("{} episodes, {} groups {:?}, {} cached files, {threads} at a time", eps.len(), groups.len(), groups, locals.len());
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());
    let count = listing.len();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(&(number, path)) = eps.get(i) else { break };
                let outcome = run(*number, path, &folder, &groups, count, &ffmpeg, &ffprobe, &locals, all, speech);
                eprint!("{}", outcome.report);
                out.lock().unwrap().push(outcome);
            });
        }
    });
    let mut outcomes = out.into_inner().unwrap();
    outcomes.sort_by_key(|o| o.number);
    println!("{}", outcomes.iter().map(|o| o.report.as_str()).collect::<String>());
    println!("Summary");
    for (group, _) in &groups {
        let mine: Vec<&Outcome> = outcomes.iter().filter(|o| &o.group == group).collect();
        if mine.is_empty() {
            continue;
        }
        let tally = |v: &str| mine.iter().filter(|o| o.verdict == v).count();
        let mean = mine.iter().map(|o| o.seconds).sum::<f64>() / mine.len() as f64;
        let against: std::collections::BTreeSet<&str> = mine.iter().map(|o| o.against.as_str()).collect();
        println!(
            "  {group}: {} episodes · {} synced · {} unsure · {} none fit · {} no files · {} errors · {mean:.1}s each · matched to {against:?}",
            mine.len(),
            tally("great"),
            tally("unsure"),
            tally("none fit"),
            tally("no files"),
            tally("error")
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run(
    number: i64,
    path: &str,
    folder: &str,
    groups: &[(String, usize)],
    count: usize,
    ffmpeg: &Path,
    ffprobe: &Path,
    locals: &[jimaku::Local],
    all: bool,
    speech: bool,
) -> Outcome {
    let started = Instant::now();
    let group = matching::group_of(folder, path);
    let mut outcome = Outcome { number, group: group.clone(), report: String::new(), verdict: "error", against: String::new(), seconds: 0.0 };
    let file_name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let result = (|| -> anyhow::Result<()> {
        let probe = media::probe(ffprobe, Path::new(path))?;
        let episode = Episode { number, path: path.to_string(), ..Default::default() };
        let timing = if speech {
            let samples = media::read_audio_filtered(ffmpeg, Path::new(path), media::japanese_audio(&probe), sync::FIRERED_RATE, None, None, None)?;
            let length = samples.len() as f64 / sync::FIRERED_RATE as f64;
            jobs::Timing { spans: sync::firered_spans(&samples)?, length, against: "the speech".into(), lead: 0.1, calibration: sync::Calibration::CLEAN }
        } else {
            jobs::timing_of(ffmpeg, ffprobe, &probe, &episode)?
        };
        outcome.against = timing.against.clone();
        let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let local = telly_lib::scan::episode_number(&stem).map(|(_, e)| e);
        let earlier: Vec<usize> = groups.iter().take_while(|(g, _)| *g != group).map(|(_, c)| *c).collect();
        let mut numbers = matching::numbers_for(number, local, &earlier, None);
        if let (Some(spread), Some(l)) = (std::env::var("SPREAD").ok().and_then(|v| v.parse::<i64>().ok()), local) {
            let base: i64 = earlier.iter().filter(|c| **c >= 13).map(|c| *c as i64).sum();
            for d in 1..=spread {
                for n in [l + base + d, l + base - d] {
                    if n >= 1 && !numbers.contains(&n) {
                        numbers.push(n);
                    }
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        let mut files = Vec::new();
        for n in &numbers {
            for f in jimaku::candidates(locals, *n, count) {
                if seen.insert(f.path.clone()) {
                    files.push(f);
                }
            }
        }
        let (best, tried) = jobs::pick_subtitles(&files, locals, &timing, &file_name, |_, _| {});
        let rows: Vec<String> = tried
            .iter()
            .map(|t| format!("      {:+7.2}s score {:.2}{} | {}
", t.offset_ms as f64 / 1000.0, t.score, if t.error.is_empty() { String::new() } else { format!(" ({})", t.error) }, t.name.chars().take(70).collect::<String>()))
            .collect();
        let searched = tried.len() > files.len().min(12);
        let best = best.map(|p| (p.synced, p.fit, format!("{}{}", p.name, if searched { "  [found by searching]" } else { "" })));
        let head = format!("E{number:03} {file_name} · {} · numbers {:?} · {} files", timing.against, numbers, files.len());
        match best {
            None => {
                outcome.verdict = "no files";
                outcome.report = format!("{head}\n   NO FILES\n");
            }
            Some((synced, fit, name)) => {
                outcome.verdict = if fit.score < 0.15 {
                    "none fit"
                } else if fit.score >= jobs::GOOD_SYNC {
                    "great"
                } else {
                    "unsure"
                };
                outcome.report = format!(
                    "{head}\n   {} {:+.2}s score {:.2} z {:.1} clean {:.2}{} ← {}\n",
                    outcome.verdict.to_uppercase(),
                    fit.offset_ms as f64 / 1000.0,
                    fit.score,
                    fit.z,
                    fit.quality,
                    if synced.shifts > 1 { format!(" {} shifts", synced.shifts) } else { String::new() },
                    name.chars().take(80).collect::<String>()
                );
                if all {
                    outcome.report.push_str(&rows.concat());
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        outcome.report = format!("E{number:03} {file_name}\n   ERROR {e:#}\n");
    }
    outcome.seconds = started.elapsed().as_secs_f64();
    outcome
}
