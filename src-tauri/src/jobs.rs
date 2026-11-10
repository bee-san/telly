//! Getting shows ready in the background: two workers, one converting video
//! (heavy, one at a time) and one finding and syncing subtitles.
//!
//! Work isn't queued as such. Each worker looks at the library and takes the
//! most useful episode still waiting:
//!
//!   1. each show's next episode (the one you'd play), most recently watched show first
//!   2. the next 10 after it ("convert ahead" in Settings)
//!   3. the rest: subtitles always; videos only when nothing has played for a
//!      couple of minutes and Settings allows it, as copies take disk space
//!
//! Episodes you've watched get subtitles last and are never converted.

use crate::backend::Backend;
use crate::db::{Episode, Show};
use crate::media::{self, Plan};
use crate::subs::{self, Line};
use crate::sync;
use crate::jimaku;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A sync scoring at least this shows as synced; below it, as "needs a look"
/// (0.5 is 4 spreads above chance; see sync.rs).
pub const GOOD_SYNC: f64 = 0.5;
/// After this, the quick sync stops trying more files.
const GREAT_SYNC: f64 = 0.75;
/// The most subtitle files tried per episode.
const MAX_TRIES: usize = 4;
/// Nothing played for this long counts as idle.
const IDLE_AFTER: Duration = Duration::from_secs(120);

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub show_id: i64,
    pub episode_id: i64,
    pub number: i64,
    pub title: String,
    /// What's happening, for the title bar.
    pub label: String,
    pub progress: f64,
}

pub struct Jobs {
    wake: Mutex<u64>,
    signal: Condvar,
    pub media: Mutex<Option<Task>>,
    pub subs: Mutex<Option<Task>>,
    /// A download (Whisper's model).
    pub download: Mutex<Option<Task>>,
    /// Stops the running conversion (the show was removed, or Telly is closing).
    cancel_media: AtomicBool,
    /// Jimaku's files per entry, so each episode doesn't ask again.
    jimaku_files: Mutex<HashMap<i64, (Instant, Vec<jimaku::Local>)>>,
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs {
            wake: Mutex::new(0),
            signal: Condvar::new(),
            media: Mutex::new(None),
            subs: Mutex::new(None),
            download: Mutex::new(None),
            cancel_media: AtomicBool::new(false),
            jimaku_files: Mutex::new(HashMap::new()),
        }
    }
}

impl Jobs {
    /// Something changed (a show was added, a setting changed): look for work now.
    pub fn poke(&self) {
        if let Ok(mut n) = self.wake.lock() {
            *n += 1;
        }
        self.signal.notify_all();
    }

    fn wait(&self, seen: &mut u64, timeout: Duration) {
        let Ok(guard) = self.wake.lock() else { return };
        let current = *guard;
        if current != *seen {
            *seen = current;
            return;
        }
        if let Ok((guard, _)) = self.signal.wait_timeout(guard, timeout) {
            *seen = *guard;
        }
    }

    /// Stops the conversion if it's for `show_id` (being removed).
    pub fn cancel_show(&self, show_id: i64) {
        if self.media.lock().is_ok_and(|t| t.as_ref().is_some_and(|t| t.show_id == show_id)) {
            self.cancel_media.store(true, Ordering::SeqCst);
        }
    }

    pub fn forget_jimaku(&self) {
        if let Ok(mut cache) = self.jimaku_files.lock() {
            cache.clear();
        }
    }
}

/// The episode you'd play next: a half-watched one (Resume), else the one
/// after the last you finished, else the first you haven't watched.
pub fn next_episode(episodes: &[Episode]) -> Option<&Episode> {
    let resume = episodes
        .iter()
        .filter(|e| e.watched_at.is_none() && e.position >= 20.0 && (e.duration <= 0.0 || e.position < e.duration * 0.9))
        .max_by_key(|e| e.last_played_at.unwrap_or(0));
    if resume.is_some() {
        return resume;
    }
    let last_watched = episodes.iter().filter(|e| e.watched_at.is_some()).map(|e| e.number).max();
    match last_watched {
        Some(n) => episodes
            .iter()
            .find(|e| e.number > n && e.watched_at.is_none())
            .or_else(|| episodes.iter().find(|e| e.watched_at.is_none())),
        None => episodes.first(),
    }
}

/// Shows in the order work should go to them: the one playing, then the most recently watched, then the newest.
fn shows_in_order(backend: &Backend) -> Vec<Show> {
    let mut shows = backend.db.shows().unwrap_or_default();
    if let Some(playing) = backend.playing_show() {
        if let Some(i) = shows.iter().position(|s| s.id == playing) {
            let s = shows.remove(i);
            shows.insert(0, s);
        }
    }
    shows
}

/// The most useful episode waiting for `kind` ("media" or "subs") work.
fn pick(backend: &Backend, kind: &str) -> Option<(Show, Episode, usize)> {
    let ahead = backend.db.setting_i64("convert.ahead", 10).max(1);
    let convert_rest = backend.db.setting_bool("convert.rest", false);
    let idle = backend.idle_for().is_none_or(|d| d >= IDLE_AFTER);
    let mut best: Option<((u8, usize, i64), Show, Episode, usize)> = None;
    for (rank, show) in shows_in_order(backend).into_iter().enumerate() {
        let episodes = backend.db.episodes(show.id).unwrap_or_default();
        let count = episodes.len();
        let next = next_episode(&episodes).map(|e| e.number).unwrap_or(1);
        for e in episodes {
            let waiting = match kind {
                "media" => e.media_state == "pending",
                "whisper" => e.subs_state == "whisper",
                _ => e.subs_state == "pending",
            };
            if !waiting {
                continue;
            }
            let bucket = if e.number == next {
                0
            } else if e.number > next && e.number <= next + ahead {
                1
            } else if e.number > next {
                2
            } else {
                3
            };
            if kind == "media" && (bucket == 3 || (bucket == 2 && !(idle && convert_rest))) {
                continue;
            }
            let key = (bucket, rank, e.number);
            if best.as_ref().is_none_or(|(k, ..)| key < *k) {
                best = Some((key, show.clone(), e, count));
            }
        }
    }
    best.map(|(_, s, e, n)| (s, e, n))
}

fn set_task(slot: &Mutex<Option<Task>>, task: Option<Task>) {
    if let Ok(mut t) = slot.lock() {
        *t = task;
    }
}

fn update_task(slot: &Mutex<Option<Task>>, label: Option<&str>, progress: f64) {
    if let Ok(mut t) = slot.lock() {
        if let Some(t) = t.as_mut() {
            if let Some(label) = label {
                t.label = label.to_string();
            }
            t.progress = progress;
        }
    }
}

pub fn start(backend: Arc<Backend>) {
    {
        let backend = backend.clone();
        std::thread::Builder::new().name("telly-media".into()).spawn(move || media_worker(&backend)).ok();
    }
    std::thread::Builder::new().name("telly-subs".into()).spawn(move || subs_worker(&backend)).ok();
}

// ---- video ---------------------------------------------------------------------

fn media_worker(backend: &Backend) {
    let mut seen = 0;
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        let tools = backend.tools();
        let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else { continue };
        while let Some((show, episode, _)) = pick(backend, "media") {
            if !backend.db.claim_media(episode.id).unwrap_or(false) {
                continue;
            }
            backend.jobs.cancel_media.store(false, Ordering::SeqCst);
            set_task(
                &backend.jobs.media,
                Some(Task {
                    show_id: show.id,
                    episode_id: episode.id,
                    number: episode.number,
                    title: show.title.clone(),
                    label: format!("Reading {} {}", show.title, episode.number),
                    progress: 0.0,
                }),
            );
            let result = prepare_video(backend, &ffmpeg, &ffprobe, &tools.encoder, &show, &episode);
            set_task(&backend.jobs.media, None);
            match result {
                Ok(()) => {}
                Err(e) if e.to_string() == "cancelled" => {
                    let _ = backend.db.media_reset(episode.id);
                }
                Err(e) => {
                    let _ = backend.db.media_failed(episode.id, &format!("{e:#}"));
                }
            }
        }
    }
}

/// Telly's copy of an episode: %LOCALAPPDATA%\Telly\media\<show>\<episode>.mp4.
pub fn copy_path(backend: &Backend, show_id: i64, episode_id: i64) -> PathBuf {
    backend.dirs.media().join(show_id.to_string()).join(format!("{episode_id}.mp4"))
}

fn prepare_video(backend: &Backend, ffmpeg: &Path, ffprobe: &Path, encoder: &str, show: &Show, episode: &Episode) -> Result<()> {
    let source = PathBuf::from(&episode.path);
    if !source.is_file() {
        return Err(anyhow!("The file isn’t there any more: {}", episode.path));
    }
    let probe = media::probe(ffprobe, &source)?;
    let forced = backend.db.setting_bool(&format!("convert.force.{}", episode.id), false);
    let plan = if forced { Plan::Video } else { media::plan(&probe, &source) };
    let probe_json = serde_json::to_string(&probe).unwrap_or_default();
    backend.db.set_probe(episode.id, &probe_json, probe.duration, media::ending_at(&probe), plan.name())?;
    if plan == Plan::Direct {
        backend.db.media_ready(episode.id, &episode.path, media::japanese_audio(&probe) as i64)?;
        return Ok(());
    }
    let output = copy_path(backend, show.id, episode.id);
    if output.is_file() && std::fs::metadata(&output).map(|m| m.len() > 0).unwrap_or(false) {
        backend.db.media_ready(episode.id, &output.to_string_lossy(), 0)?;
        return Ok(());
    }
    std::fs::create_dir_all(output.parent().expect("a folder"))?;
    let partial = output.with_extension("part");
    let verb = match plan {
        Plan::Video => "Converting",
        _ => "Copying",
    };
    update_task(&backend.jobs.media, Some(&format!("{verb} {} {}", show.title, episode.number)), 0.0);
    let last = Mutex::new(0.0f64);
    let report = |p: f64| {
        update_task(&backend.jobs.media, None, p);
        if let Ok(mut l) = last.lock() {
            if p - *l >= 0.01 {
                *l = p;
                let _ = backend.db.set_media_progress(episode.id, p);
            }
        }
    };
    let mut result = media::run_ffmpeg(ffmpeg, &media::convert_args(&probe, plan, encoder, &source, &partial), probe.duration, &backend.jobs.cancel_media, &report);
    // A GPU encoder can fail on some inputs; the CPU one always works.
    if plan == Plan::Video && encoder != "libx264" && result.as_ref().is_err_and(|e| e.to_string() != "cancelled") {
        result = media::run_ffmpeg(ffmpeg, &media::convert_args(&probe, plan, "libx264", &source, &partial), probe.duration, &backend.jobs.cancel_media, &report);
    }
    if let Err(e) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, &output).context("saving the converted video")?;
    backend.db.media_ready(episode.id, &output.to_string_lossy(), 0)?;
    Ok(())
}

// ---- subtitles -----------------------------------------------------------------

fn subs_worker(backend: &Backend) {
    let mut seen = 0;
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        let tools = backend.tools();
        let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else { continue };
        while let Some((show, episode, count)) = pick(backend, "subs") {
            if !backend.db.claim_subs(episode.id).unwrap_or(false) {
                continue;
            }
            set_task(
                &backend.jobs.subs,
                Some(Task {
                    show_id: show.id,
                    episode_id: episode.id,
                    number: episode.number,
                    title: show.title.clone(),
                    label: format!("Finding subtitles for {} {}", show.title, episode.number),
                    progress: 0.0,
                }),
            );
            let result = find_subtitles(backend, &ffmpeg, &ffprobe, &show, &episode, count);
            set_task(&backend.jobs.subs, None);
            if let Err(e) = result {
                let _ = backend.db.set_subs_state(episode.id, "failed", &format!("{e:#}"));
            }
        }
        // Then episodes you asked Whisper to make subtitles for, one at a time.
        if let (Some(whisper), Some(model)) = (tools.whisper.clone(), tools.whisper_model.clone()) {
            if let Some((show, episode, _)) = pick(backend, "whisper") {
                if backend.db.claim_whisper(episode.id).unwrap_or(false) {
                    set_task(
                        &backend.jobs.subs,
                        Some(Task {
                            show_id: show.id,
                            episode_id: episode.id,
                            number: episode.number,
                            title: show.title.clone(),
                            label: format!("Whisper is listening to {} {}", show.title, episode.number),
                            progress: 0.0,
                        }),
                    );
                    let result = whisper_subtitles(backend, &ffmpeg, &ffprobe, &whisper, &model, &episode);
                    set_task(&backend.jobs.subs, None);
                    if let Err(e) = result {
                        let _ = backend.db.set_subs_state(episode.id, "failed", &format!("Whisper: {e:#}"));
                    }
                    backend.jobs.poke();
                }
            }
        }
    }
}

/// Where Whisper's Japanese model comes from: whisper.cpp's own models on Hugging Face.
pub const WHISPER_MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin";
pub const WHISPER_MODEL_NAME: &str = "ggml-small-q5_1.bin";

/// Makes subtitles from the episode's Japanese audio with whisper.cpp,
/// marked as machine-made. Whisper times them itself, so they need no sync.
fn whisper_subtitles(backend: &Backend, ffmpeg: &Path, ffprobe: &Path, whisper: &Path, model: &Path, episode: &Episode) -> Result<()> {
    let source = Path::new(&episode.path);
    std::fs::create_dir_all(backend.dirs.subs())?;
    let subs_dir = std::path::absolute(backend.dirs.subs()).unwrap_or_else(|_| backend.dirs.subs());
    let wav = subs_dir.join(format!("{}.whisper.wav", episode.id));
    // whisper.cpp adds ".srt" to this.
    let out = subs_dir.join(format!("{}.whisper", episode.id));
    let srt = subs_dir.join(format!("{}.whisper.srt", episode.id));
    // A transcript newer than the model and the rip is still good (a retry after an error).
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let fresh = matches!((modified(&srt), modified(model), modified(source)), (Some(s), Some(m), Some(r)) if s > m && s > r);
    if !fresh {
        let probe = media::probe(ffprobe, source)?;
        let status = crate::util::command(ffmpeg)
            .args(["-nostdin", "-hide_banner", "-v", "error", "-y", "-i"])
            .arg(source)
            .args(["-map", &format!("0:a:{}", media::japanese_audio(&probe)), "-vn", "-sn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
            .arg(&wav)
            .status()
            .context("starting ffmpeg")?;
        if !status.success() {
            return Err(anyhow!("ffmpeg couldn’t read the audio"));
        }
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 12);
        // -mc 0: no text carried from one window to the next, which stops
        // Whisper repeating itself over music. -ml 60: lines of about 20
        // characters (it counts bytes), timed from the words in them.
        let output = crate::util::command(whisper)
            .arg("-m")
            .arg(model)
            .args(["-l", "ja", "-t", &threads.to_string(), "-mc", "0", "-ml", "60", "-osrt", "-np", "-of"])
            .arg(&out)
            .arg("-f")
            .arg(&wav)
            .current_dir(whisper.parent().unwrap_or(Path::new(".")))
            .output()
            .context("starting whisper.cpp")?;
        let _ = std::fs::remove_file(&wav);
        if !output.status.success() {
            let text = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("whisper.cpp stopped: {}", text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no message")));
        }
    }
    let bytes = std::fs::read(&srt).context("reading what Whisper wrote")?;
    let lines = without_repeats(subs::parse("whisper.srt", &bytes)?);
    write_lines(backend, episode.id, &lines)?;
    backend.db.set_subs(episode.id, "synced", "Made by Whisper (machine-made)", "Whisper", None, None)?;
    Ok(())
}

/// Whisper sometimes repeats a sentence over music ("彼らは「魔道士」と呼ばれる。"
/// again and again): a line saying the same thing as the one before is dropped,
/// and a line that's one sentence over and over keeps it once.
pub fn without_repeats(lines: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    for mut line in lines {
        let parts: Vec<&str> = line.text.split(['。', ' ', '\u{3000}', '\n']).map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.len() > 1 && parts.iter().all(|p| *p == parts[0]) {
            line.text = format!("{}。", parts[0].trim_end_matches('。'));
        }
        let same = out.last().is_some_and(|last: &Line| {
            let a: String = last.text.chars().filter(|c| c.is_alphanumeric()).collect();
            let b: String = line.text.chars().filter(|c| c.is_alphanumeric()).collect();
            !b.is_empty() && (a == b || a.contains(&b))
        });
        if !same {
            out.push(line);
        }
    }
    out
}

/// Downloads Whisper's Japanese model into Telly's models folder, in the background.
pub fn get_whisper_model(backend: std::sync::Arc<Backend>) {
    std::thread::spawn(move || {
        let task = |label: &str, progress: f64| {
            set_task(
                &backend.jobs.download,
                Some(Task { show_id: 0, episode_id: 0, number: 0, title: "Whisper".into(), label: label.into(), progress }),
            )
        };
        task("Getting Whisper’s Japanese model", 0.0);
        let result = (|| -> Result<()> {
            let dir = backend.dirs.models();
            std::fs::create_dir_all(&dir)?;
            let partial = dir.join(format!("{WHISPER_MODEL_NAME}.part"));
            let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3600))).build().into();
            let mut response = agent.get(WHISPER_MODEL_URL).call().map_err(|e| anyhow!("couldn’t download the model ({e})"))?;
            let total = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let mut reader = response.body_mut().with_config().limit(2 * 1024 * 1024 * 1024).reader();
            let mut file = std::fs::File::create(&partial)?;
            let mut buffer = vec![0u8; 1 << 20];
            let mut done = 0.0;
            loop {
                let n = std::io::Read::read(&mut reader, &mut buffer)?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut file, &buffer[..n])?;
                done += n as f64;
                if total > 0.0 {
                    task("Getting Whisper’s Japanese model", done / total);
                }
            }
            drop(file);
            std::fs::rename(&partial, dir.join(WHISPER_MODEL_NAME))?;
            Ok(())
        })();
        set_task(&backend.jobs.download, None);
        if let Err(e) = result {
            backend.push_event("failed", "Whisper’s model didn’t download", &format!("{e:#}"), 0, 0);
        } else {
            backend.push_event("done", "Whisper’s model is ready", "Telly can make subtitles for episodes Jimaku has nothing for.", 0, 0);
        }
        backend.refresh_tools();
    });
}

/// A subtitle file Telly tried for an episode, and how it synced.
#[derive(Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tried {
    pub name: String,
    pub path: String,
    pub score: f64,
    pub offset_ms: i64,
    #[serde(default)]
    pub error: String,
}

pub fn lines_path(backend: &Backend, episode_id: i64) -> PathBuf {
    backend.dirs.subs().join(format!("{episode_id}.json"))
}

fn tried_path(backend: &Backend, episode_id: i64) -> PathBuf {
    backend.dirs.subs().join(format!("{episode_id}.tried.json"))
}

pub fn read_tried(backend: &Backend, episode_id: i64) -> Vec<Tried> {
    std::fs::read(tried_path(backend, episode_id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn read_lines(backend: &Backend, episode_id: i64) -> Vec<Line> {
    std::fs::read(lines_path(backend, episode_id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn write_lines(backend: &Backend, episode_id: i64, lines: &[Line]) -> Result<()> {
    std::fs::create_dir_all(backend.dirs.subs())?;
    std::fs::write(lines_path(backend, episode_id), serde_json::to_vec(lines)?)?;
    Ok(())
}

/// Where people speak in an episode, from its Japanese track, and how long it is.
fn speech_of(ffmpeg: &Path, probe: &media::Probe, episode: &Episode) -> Result<(Vec<(f64, f64)>, f64)> {
    let source = Path::new(&episode.path);
    let samples = media::read_audio_filtered(ffmpeg, source, media::japanese_audio(probe), sync::RATE, None, None, Some(sync::VOICE_FILTER))?;
    let length = samples.len() as f64 / sync::RATE as f64;
    Ok((sync::voice_spans(&samples), length))
}

/// Jimaku's files for a show, cached for an hour.
fn jimaku_files(backend: &Backend, key: &str, show: &Show) -> Result<Vec<jimaku::Local>> {
    let entry_ids: Vec<i64> = match show.jimaku_id {
        Some(id) => vec![id],
        None => {
            let entries = jimaku::find_entries(key, show.anilist_id, &show.title_romaji)?;
            if let Some(first) = entries.first() {
                backend.db.set_jimaku_id(show.id, Some(first.id))?;
            }
            entries.into_iter().take(3).map(|e| e.id).collect()
        }
    };
    let mut all = Vec::new();
    for id in entry_ids {
        let cached = backend
            .jobs
            .jimaku_files
            .lock()
            .ok()
            .and_then(|c| c.get(&id).filter(|(at, _)| at.elapsed() < Duration::from_secs(3600)).map(|(_, f)| f.clone()));
        let files = match cached {
            Some(files) => files,
            None => {
                let files = jimaku::fetch_all(key, id, &backend.dirs.jimaku().join(id.to_string()))?;
                if let Ok(mut c) = backend.jobs.jimaku_files.lock() {
                    c.insert(id, (Instant::now(), files.clone()));
                }
                files
            }
        };
        all.extend(files);
    }
    Ok(all)
}

/// Japanese text subtitles inside the rip itself, written out as files.
fn embedded(backend: &Backend, ffmpeg: &Path, probe: &media::Probe, episode: &Episode) -> Vec<jimaku::Local> {
    let mut out = Vec::new();
    for s in &probe.subtitles {
        let japanese = matches!(s.language.to_lowercase().as_str(), "jpn" | "ja" | "jp");
        let text_codec = matches!(s.codec.as_str(), "ass" | "ssa" | "subrip" | "srt" | "webvtt" | "mov_text");
        if !japanese || !text_codec {
            continue;
        }
        let format = if matches!(s.codec.as_str(), "ass" | "ssa") { "ass" } else { "srt" };
        let Ok(text) = media::read_subtitle_track(ffmpeg, Path::new(&episode.path), s.index, format) else { continue };
        let path = backend.dirs.subs().join(format!("{}.track{}.{format}", episode.id, s.index));
        if std::fs::create_dir_all(backend.dirs.subs()).is_ok() && std::fs::write(&path, text).is_ok() {
            out.push(jimaku::Local { name: format!("In the rip (track {})", s.index), path, episode: Some(episode.number) });
        }
    }
    out
}

/// Syncs one file to the episode's speech.
pub fn sync_file(path: &Path, name: &str, speech: &[(f64, f64)], length: f64) -> Result<sync::Synced> {
    let bytes = std::fs::read(path)?;
    let lines = subs::parse(name, &bytes)?;
    if !subs::is_japanese(&lines) {
        return Err(anyhow!("not Japanese"));
    }
    Ok(sync::sync(&lines, speech, length))
}

fn find_subtitles(backend: &Backend, ffmpeg: &Path, ffprobe: &Path, show: &Show, episode: &Episode, count: usize) -> Result<()> {
    let key = backend.db.setting_str("jimaku.key");
    let probe = media::probe(ffprobe, Path::new(&episode.path))?;
    let mut files = embedded(backend, ffmpeg, &probe, episode);
    if !key.trim().is_empty() {
        let all = jimaku_files(backend, key.trim(), show)?;
        files.extend(jimaku::candidates(&all, episode.number, count));
    }
    if files.is_empty() {
        let (state, detail) = if key.trim().is_empty() {
            ("nokey", "Add your Jimaku API key in Settings")
        } else if show.jimaku_id.is_none() && backend.db.show(show.id)?.and_then(|s| s.jimaku_id).is_none() {
            ("none", "Jimaku has no subtitles for this show")
        } else {
            ("none", "Jimaku has nothing for this episode")
        };
        backend.db.set_subs(episode.id, state, detail, "", None, None)?;
        return Ok(());
    }
    update_task(&backend.jobs.subs, Some(&format!("Listening to {} {}", show.title, episode.number)), 0.0);
    let (speech, length) = speech_of(ffmpeg, &probe, episode).context("reading the episode’s audio")?;
    let mut tried: Vec<Tried> = Vec::new();
    let mut best: Option<(sync::Synced, String)> = None;
    for (i, file) in files.iter().take(MAX_TRIES).enumerate() {
        update_task(&backend.jobs.subs, Some(&format!("Syncing subtitles for {} {}", show.title, episode.number)), i as f64 / files.len().min(MAX_TRIES) as f64);
        match sync_file(&file.path, &file.name, &speech, length) {
            Ok(synced) => {
                tried.push(Tried { name: file.name.clone(), path: file.path.to_string_lossy().into_owned(), score: synced.score, offset_ms: synced.offset_ms, error: String::new() });
                let better = best.as_ref().is_none_or(|(b, _)| synced.score > b.score);
                let great = synced.score >= GREAT_SYNC;
                if better {
                    best = Some((synced, file.name.clone()));
                }
                if great {
                    break;
                }
            }
            Err(e) => tried.push(Tried { name: file.name.clone(), path: file.path.to_string_lossy().into_owned(), score: 0.0, offset_ms: 0, error: format!("{e:#}") }),
        }
    }
    std::fs::create_dir_all(backend.dirs.subs())?;
    std::fs::write(tried_path(backend, episode.id), serde_json::to_vec(&tried)?)?;
    let Some((synced, name)) = best else {
        backend.db.set_subs(episode.id, "none", "None of Jimaku’s files for this episode are Japanese subtitles Telly can read", "", None, None)?;
        return Ok(());
    };
    write_lines(backend, episode.id, &synced.lines)?;
    let state = if synced.score >= GOOD_SYNC { "synced" } else { "check" };
    let detail = describe(&synced);
    backend.db.set_subs(episode.id, state, &detail, &name, Some(synced.score), Some(synced.offset_ms))?;
    Ok(())
}

fn describe(s: &sync::Synced) -> String {
    let mut parts = vec![format!("shifted {:+.1} s", s.offset_ms as f64 / 1000.0)];
    if (s.ratio - 1.0).abs() > 1e-6 {
        parts.push(format!("speed ×{:.4}", s.ratio));
    }
    if s.shifts > 1 {
        parts.push(format!("{} cuts", s.shifts - 1));
    }
    parts.join(", ")
}

/// Uses a subtitle file you picked (or one Telly tried before) for an episode.
pub fn use_file(backend: &Backend, episode: &Episode, path: &Path, name: &str) -> Result<sync::Synced> {
    let tools = backend.tools();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg, tools.ffprobe) else {
        return Err(anyhow!("Telly needs ffmpeg for this. Choose it in Settings."));
    };
    let probe = media::probe(&ffprobe, Path::new(&episode.path))?;
    let (speech, length) = speech_of(&ffmpeg, &probe, episode)?;
    let bytes = std::fs::read(path)?;
    let lines = subs::parse(name, &bytes)?;
    let synced = sync::sync(&lines, &speech, length);
    write_lines(backend, episode.id, &synced.lines)?;
    let state = if synced.score >= GOOD_SYNC { "synced" } else { "check" };
    backend.db.set_subs(episode.id, state, &describe(&synced), name, Some(synced.score), Some(synced.offset_ms))?;
    Ok(synced)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(number: i64, watched: bool, position: f64, played: i64) -> Episode {
        Episode {
            id: number,
            show_id: 1,
            number,
            path: String::new(),
            size: 0,
            duration: 1400.0,
            media_state: "ready".into(),
            media_plan: String::new(),
            media_progress: 1.0,
            media_error: String::new(),
            play_path: String::new(),
            audio_index: 0,
            ending_at: None,
            position,
            watched_at: watched.then_some(1),
            last_played_at: (played > 0).then_some(played),
            subs_state: "synced".into(),
            subs_detail: String::new(),
            subs_file: String::new(),
            subs_score: None,
            subs_offset_ms: None,
            subs_nudge_ms: 0,
        }
    }

    #[test]
    fn whisper_s_repeats_are_dropped() {
        let line = |start: f64, text: &str| Line { start, end: start + 2.0, text: text.into(), ruby: vec![], speaker: String::new(), sign: false };
        let lines = vec![
            line(0.0, "そこは魔法の世界。"),
            line(2.0, "彼らは「魔道士」と呼ばれる。 彼らは「魔道士」と呼ばれる。"),
            line(4.0, "彼らは「魔道士」と呼ばれる。"),
            line(6.0, "ギルドだ。"),
        ];
        let kept: Vec<String> = without_repeats(lines).into_iter().map(|l| l.text).collect();
        assert_eq!(kept, ["そこは魔法の世界。", "彼らは「魔道士」と呼ばれる。", "ギルドだ。"]);
    }

    #[test]
    fn next_is_resume_then_after_the_last_watched() {
        let eps = vec![ep(1, true, 0.0, 1), ep(2, true, 0.0, 2), ep(3, false, 0.0, 0), ep(4, false, 0.0, 0)];
        assert_eq!(next_episode(&eps).map(|e| e.number), Some(3));
        let eps = vec![ep(1, true, 0.0, 1), ep(2, false, 0.0, 0), ep(3, true, 0.0, 3), ep(4, false, 0.0, 0)];
        assert_eq!(next_episode(&eps).map(|e| e.number), Some(4));
        let eps = vec![ep(1, true, 0.0, 1), ep(2, false, 300.0, 5), ep(3, false, 0.0, 0)];
        assert_eq!(next_episode(&eps).map(|e| e.number), Some(2));
        let eps = vec![ep(1, false, 0.0, 0), ep(2, false, 0.0, 0)];
        assert_eq!(next_episode(&eps).map(|e| e.number), Some(1));
        let eps = vec![ep(1, true, 0.0, 1), ep(2, true, 0.0, 2)];
        assert_eq!(next_episode(&eps).map(|e| e.number), None);
    }
}
