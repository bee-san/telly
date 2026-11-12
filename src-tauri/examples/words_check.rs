//! Times working out episodes' words, which premining and each episode's share
//! of known words read, with Telly's own Hachidori, as the app does after a sync.
//!
//!   cargo run --profile measure --example words_check -- <data dir> <port> <dictionaries folder> <subtitle file>...
//!
//! It serves Telly's media protocol (GET and POST) at http://127.0.0.1:<port>
//! for a copy of Telly's Hachidori open in a browser. media.localhost is
//! WebView2's, so only the app has it:
//!
//! 1. `npm run prepare:hachidori`, then copy src-tauri/extensions/hachidori
//!    somewhere else (WebView2 would load a second copy left there).
//! 2. In the copy's telly/host.js, set TELLY to "http://127.0.0.1:<port>".
//! 3. Open chrome-extension://<id>/telly/host.html?link=0 in Chromium started
//!    with --load-extension=<the copy> (and --headless=new if you like), the
//!    id being the one src-tauri/hachidori.json names.
//!
//! Once the page is answering and the dictionaries are installed, it works out
//! each subtitle file's words as an episode, in order, as the subtitle workers
//! do: Telly's lookup cache starts empty and stays warm from one episode to
//! the next, as it does while Telly is open.
//!
//! Each episode's words are left in <data dir>/subtitles/<n>.words.json, and
//! the words pass logs its own timings. Give it a data dir of its own (it
//! writes episodes 1, 2, …), and the same one each time: a new one installs
//! the dictionaries again.
//!
//! With --shares before the files, they're a show's episodes instead, with no
//! words saved, and it times the pass the Library starts as it opens until
//! each episode has its share of known words.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use telly_lib::backend::{Backend, Dirs};
use telly_lib::db::{NewEpisode, NewShow};
use telly_lib::{jobs, own_hachidori, premine, subs};

/// The show --shares makes, replaced each run.
const SHOW: &str = "words_check";

fn serve(backend: Arc<Backend>, port: u16) {
    let server = tiny_http::Server::http(("127.0.0.1", port)).expect("starting the server");
    for mut request in server.incoming_requests() {
        let backend = backend.clone();
        std::thread::spawn(move || {
            let cors = || tiny_http::Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap();
            let url = request.url().to_string();
            let response = match request.method() {
                tiny_http::Method::Options => {
                    let _ = request.respond(
                        tiny_http::Response::empty(204)
                            .with_header(cors())
                            .with_header(tiny_http::Header::from_bytes("Access-Control-Allow-Headers", "content-type, range").unwrap()),
                    );
                    return;
                }
                tiny_http::Method::Post => {
                    let mut body = Vec::new();
                    let _ = request.as_reader().read_to_end(&mut body);
                    match backend.receive(&url, &body) {
                        Ok(()) => tiny_http::Response::from_data(b"{}".to_vec()),
                        Err(e) => tiny_http::Response::from_data(format!("{e:#}").into_bytes()).with_status_code(400),
                    }
                }
                _ => match backend.serve(&url, None) {
                    Ok(served) => tiny_http::Response::from_data(served.bytes)
                        .with_header(tiny_http::Header::from_bytes("Content-Type", served.mime).unwrap()),
                    Err(e) => tiny_http::Response::from_data(format!("{e:#}").into_bytes()).with_status_code(404),
                },
            };
            let _ = request.respond(response.with_header(cors()));
        });
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        anyhow::bail!("usage: words_check <data dir> <port> <dictionaries folder> <subtitle file>...");
    }
    let dir = std::path::absolute(&args[1])?;
    let port: u16 = args[2].parse()?;
    let dictionaries = std::path::absolute(&args[3])?;
    let shares = args[4] == "--shares";
    let files: Vec<PathBuf> = args[if shares { 5 } else { 4 }..].iter().map(PathBuf::from).collect();

    let backend = Arc::new(Backend::open(Dirs { local: dir.clone(), roaming: dir })?);
    backend.db.set_setting(own_hachidori::FOLDER, &serde_json::json!(dictionaries.to_string_lossy()))?;
    backend.db.set_setting("hachidori.link", &serde_json::json!(false))?;
    let server = backend.clone();
    std::thread::spawn(move || serve(server, port));

    // The page, then the dictionaries.
    let waiting = Instant::now();
    let mut said = String::new();
    loop {
        let report = own_hachidori::report(&backend);
        let now = format!(
            "page {}, dictionaries {} of {}{}{}",
            if backend.own_hachidori.answering() { "answering" } else { "not there yet" },
            report.installed,
            report.total,
            if report.installing.is_empty() { String::new() } else { format!(", installing {}", report.installing) },
            if report.failed.is_empty() { String::new() } else { format!(", failed {:?}", report.failed) }
        );
        if now != said {
            eprintln!("{:>7.1} s  {now}", waiting.elapsed().as_secs_f64());
            said = now;
        }
        if backend.own_hachidori.answering() && report.total > 0 && report.installed == report.total && report.installing.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    if shares {
        return time_shares(&backend, &files);
    }
    let started = Instant::now();
    let mut lines_total = 0;
    for (i, file) in files.iter().enumerate() {
        let id = i as i64 + 1;
        let lines = subs::parse(&file.to_string_lossy(), &std::fs::read(file)?)?;
        jobs::write_lines(&backend, id, &lines)?;
        let _ = std::fs::remove_file(premine::words_path(&backend, id));
        let episode = Instant::now();
        premine::prepare(&backend, id);
        if !premine::words_path(&backend, id).is_file() {
            anyhow::bail!("episode {id}'s words weren't saved");
        }
        lines_total += lines.len();
        println!("episode {id}: {:.2} s", episode.elapsed().as_secs_f64());
    }
    println!("{} episodes, {lines_total} lines: {:.2} s", files.len(), started.elapsed().as_secs_f64());
    Ok(())
}

/// The files as a new show's episodes, and the Library's pass over the shares.
fn time_shares(backend: &Arc<Backend>, files: &[PathBuf]) -> anyhow::Result<()> {
    for show in backend.db.shows()?.into_iter().filter(|s| s.title == SHOW) {
        backend.db.remove_show(show.id)?;
    }
    let folder = backend.dirs.local.join(SHOW);
    let episodes = (1..=files.len() as i64)
        .map(|n| NewEpisode { number: n, path: folder.join(format!("{n}.mkv")).to_string_lossy().into_owned(), size: 0, duration: 1440.0 })
        .collect();
    let show_id = backend.db.add_show(&NewShow { folder: folder.to_string_lossy().into_owned(), title: SHOW.into(), episodes, ..Default::default() })?;
    let mut ids = Vec::new();
    for e in backend.db.episodes(show_id)? {
        let file = &files[e.number as usize - 1];
        jobs::write_lines(backend, e.id, &subs::parse(&file.to_string_lossy(), &std::fs::read(file)?)?)?;
        let _ = std::fs::remove_file(premine::words_path(backend, e.id));
        ids.push(e.id);
    }
    let started = Instant::now();
    premine::start_shares(backend, None);
    loop {
        let scores = backend.db.word_scores()?;
        let done = ids.iter().filter(|id| scores.contains_key(id)).count();
        if done == ids.len() {
            break;
        }
        if started.elapsed() > Duration::from_secs(600) {
            anyhow::bail!("only {done} of {} episodes have a share after 10 minutes", ids.len());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let (shares, error) = premine::shares(backend);
    for id in &ids {
        let share = shares[id];
        println!("episode {id}: {} uses, {} known", share.uses, share.known);
    }
    println!("{} episodes' shares: {:.2} s{}", ids.len(), started.elapsed().as_secs_f64(), if error.is_empty() { String::new() } else { format!(" ({error})") });
    Ok(())
}
