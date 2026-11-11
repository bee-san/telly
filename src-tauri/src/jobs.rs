//! Getting shows ready in the background, several episodes at once:
//!
//!   - one worker converts video (heavy, one at a time)
//!   - one reads what's in rips nobody has looked at yet, so a rip that plays
//!     as it is doesn't wait behind a conversion
//!   - a few find and sync subtitles (`SUBS_WORKERS`). The one read of a rip
//!     that takes also makes what the player would else read the rip for when
//!     the episode first plays: the seek bar's pictures and its English lines
//!   - one catches up episodes got ready before that was so
//!
//! Work isn't queued as such. Each worker looks at the library and takes the
//! most useful episode still waiting, which no other worker then gets:
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
use crate::{anilist, matching, openrouter, scan};
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A sync scoring at least this shows as synced; below it, as "needs a look"
/// (0.5 is 4 spreads above chance; see sync.rs).
pub const GOOD_SYNC: f64 = 0.5;
/// Below this, a file is another episode's: Telly keeps none rather than a wrong one.
const FITS_NOT: f64 = 0.15;
/// After this, the quick sync stops trying more files.
const GREAT_SYNC: f64 = 0.75;
/// The most subtitle files tried per episode. Matching a file to the rip's own
/// subtitles takes a fraction of a second, so it can try every set Jimaku has.
const MAX_TRIES: usize = 12;
/// Nothing played for this long counts as idle.
const IDLE_AFTER: Duration = Duration::from_secs(120);
/// How many episodes have their subtitles found at once. Measured on 24
/// episodes on an SSD: 81 and 103 s with one worker, 54 and 59 s with two,
/// 34 s with three, 27 and 47 s with four, 26 and 39 s with six. On a hard
/// drive the drive is the limit, read by one episode at a time (see
/// `Jobs::drive`): the others get on with the rest of their work meanwhile.
const SUBS_WORKERS: usize = 3;

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

/// Turns at something only one worker may use at a time, taken in the order
/// they were asked for. A plain lock goes to any of those waiting, so the
/// episode that matters most could wait longest.
#[derive(Default)]
struct Turns {
    /// How many turns have been asked for, and how many have ended.
    counts: Mutex<(u64, u64)>,
    ended: Condvar,
}

/// A turn, which ends when this is dropped.
struct Turn<'a>(&'a Turns);

impl Turns {
    /// Waits for a turn.
    fn take(&self) -> Turn<'_> {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        let mine = counts.0;
        counts.0 += 1;
        while counts.1 != mine {
            counts = self.ended.wait(counts).unwrap_or_else(|e| e.into_inner());
        }
        Turn(self)
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.counts.lock().unwrap_or_else(|e| e.into_inner()).1 += 1;
        self.0.ended.notify_all();
    }
}

pub struct Jobs {
    wake: Mutex<u64>,
    signal: Condvar,
    /// What each worker is doing, for the title bar: the one converting, the
    /// one reading rips for the first time, each one finding subtitles, and
    /// the one catching up.
    media: Mutex<Option<Task>>,
    probing: Mutex<Option<Task>>,
    subs: [Mutex<Option<Task>>; SUBS_WORKERS],
    catching_up: Mutex<Option<Task>>,
    /// The episodes `catch_up_worker` has looked at since Telly started.
    caught_up: Mutex<HashSet<i64>>,
    /// A download (Whisper's model).
    pub download: Mutex<Option<Task>>,
    /// Stops the running conversion (the show was removed, or Telly is closing).
    cancel_media: AtomicBool,
    /// Jimaku's files per entry, so each episode doesn't ask again.
    jimaku_files: Mutex<HashMap<i64, (Instant, Vec<jimaku::Local>)>>,
    /// Held while Jimaku is asked, so it's asked by one worker at a time: it
    /// answers 429 when asked too fast, and what one worker fetched the next finds kept.
    jimaku: Mutex<()>,
    /// Turns at each drive rips are on (see `drive`).
    drives: Mutex<HashMap<PathBuf, Arc<Turns>>>,
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs {
            wake: Mutex::new(0),
            signal: Condvar::new(),
            media: Mutex::new(None),
            probing: Mutex::new(None),
            subs: std::array::from_fn(|_| Mutex::new(None)),
            catching_up: Mutex::new(None),
            caught_up: Mutex::new(HashSet::new()),
            download: Mutex::new(None),
            cancel_media: AtomicBool::new(false),
            jimaku_files: Mutex::new(HashMap::new()),
            jimaku: Mutex::new(()),
            drives: Mutex::new(HashMap::new()),
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

    /// What the workers are doing now, for the title bar.
    pub fn tasks(&self) -> Vec<Task> {
        [&self.media, &self.probing].into_iter().chain(&self.subs).chain([&self.catching_up, &self.download]).filter_map(|slot| slot.lock().ok()?.clone()).collect()
    }

    /// Turns at the drive `rip` is on. A worker takes one to read a rip from
    /// end to end: two rips read at once from one hard drive take longer than
    /// one after the other, as the drive spends its time moving between them.
    /// Measured on a USB hard drive, with 8 episodes of four shows: 38 and
    /// 48 MB/s one at a time, 23 MB/s two at a time, 21 and 23 MB/s four at a time.
    fn drive(&self, rip: &Path) -> Arc<Turns> {
        let root = rip.ancestors().last().unwrap_or(rip).to_path_buf();
        self.drives.lock().map(|mut drives| drives.entry(root).or_default().clone()).unwrap_or_default()
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

/// Whether an episode ready to play may be without its seek-bar pictures or
/// its English lines: one of the files isn't there, and `catch_up_worker`
/// hasn't looked at the episode yet.
fn behind(backend: &Backend, episode: &Episode) -> bool {
    let looked = backend.jobs.caught_up.lock().unwrap_or_else(|e| e.into_inner()).contains(&episode.id);
    !looked && !(sheet_path(backend, episode.show_id, episode.id).is_file() && translation_path(backend, episode.id).is_file())
}

/// The most useful episode waiting for `kind` work: "media", "probe", "subs",
/// "whisper" or "catch up".
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
                // A rip nobody has looked at yet has no plan.
                "probe" => e.media_state == "pending" && e.media_plan.is_empty(),
                "whisper" => e.subs_state == "whisper",
                // While its subtitles are still to find, finding them reads the rip and makes these.
                "catch up" => e.media_state == "ready" && !matches!(e.subs_state.as_str(), "pending" | "working") && behind(backend, &e),
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
            if matches!(kind, "media" | "probe") && (bucket == 3 || (bucket == 2 && !(idle && convert_rest))) {
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

/// Takes the most useful episode waiting for `kind` work ("media", "probe" or
/// "subs") for one worker. The claim is what keeps two workers off one episode:
/// only the first to make it gets the episode, and the other looks again.
fn take(backend: &Backend, kind: &str) -> Option<(Show, Episode, usize)> {
    loop {
        let (show, episode, count) = pick(backend, kind)?;
        let claimed = if kind == "subs" { backend.db.claim_subs(episode.id) } else { backend.db.claim_media(episode.id) };
        if claimed.unwrap_or(false) {
            return Some((show, episode, count));
        }
    }
}

fn task(show: &Show, episode: &Episode, doing: &str) -> Task {
    Task {
        show_id: show.id,
        episode_id: episode.id,
        number: episode.number,
        title: show.title.clone(),
        label: format!("{doing} {} {}", show.title, episode.number),
        progress: 0.0,
    }
}

pub fn start(backend: Arc<Backend>) {
    let start = |name: String, work: fn(&Backend, usize), n: usize| {
        let backend = backend.clone();
        std::thread::Builder::new().name(name).spawn(move || work(&backend, n)).ok();
    };
    start("telly-media".into(), |backend, _| media_worker(backend), 0);
    start("telly-probe".into(), |backend, _| probe_worker(backend), 0);
    start("telly-catch-up".into(), |backend, _| catch_up_worker(backend), 0);
    for n in 0..SUBS_WORKERS {
        start(format!("telly-subs-{n}"), subs_worker, n);
    }
}

// ---- video ---------------------------------------------------------------------

fn media_worker(backend: &Backend) {
    let mut seen = 0;
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        let tools = backend.tools();
        let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else { continue };
        while let Some((show, episode, _)) = take(backend, "media") {
            backend.jobs.cancel_media.store(false, Ordering::SeqCst);
            set_task(&backend.jobs.media, Some(task(&show, &episode, "Reading")));
            let result = plan_video(backend, &ffprobe, &show, &episode).and_then(|planned| match planned {
                Some((probe, plan, audio)) => convert_video(backend, &ffmpeg, &tools.encoder, &show, &episode, &probe, plan, audio),
                None => Ok(()),
            });
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

/// Reads what's in rips nobody has looked at yet, while `media_worker`
/// converts. A rip that plays as it is is ready there and then; one that
/// needs converting goes back, its plan known, for `media_worker`.
fn probe_worker(backend: &Backend) {
    let mut seen = 0;
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        let Some(ffprobe) = backend.tools().ffprobe else { continue };
        while let Some((show, episode, _)) = take(backend, "probe") {
            set_task(&backend.jobs.probing, Some(task(&show, &episode, "Reading")));
            let result = plan_video(backend, &ffprobe, &show, &episode);
            set_task(&backend.jobs.probing, None);
            match result {
                Ok(None) => {}
                Ok(Some(_)) => {
                    let _ = backend.db.media_reset(episode.id);
                    backend.jobs.poke();
                }
                Err(e) => {
                    let _ = backend.db.media_failed(episode.id, &format!("{e:#}"));
                }
            }
        }
    }
}

/// Makes the seek bar's pictures and keeps the rip's English lines for
/// episodes got ready before `find_subtitles` made them, so the player doesn't
/// read the rip from end to end when one first plays.
fn catch_up_worker(backend: &Backend) {
    let slot = &backend.jobs.catching_up;
    let mut seen = 0;
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        let Some(ffmpeg) = backend.tools().ffmpeg else { continue };
        while let Some((show, episode, _)) = pick(backend, "catch up") {
            backend.jobs.caught_up.lock().unwrap_or_else(|e| e.into_inner()).insert(episode.id);
            let Some(probe) = stored_probe(backend, episode.id) else { continue };
            let rip = Path::new(&episode.path);
            let sheet = sheet_path(backend, show.id, episode.id);
            let pictures = !sheet.is_file();
            let english = lacks_translation(backend, episode.id, &probe);
            if !pictures && !english {
                continue;
            }
            let doing = if pictures { "Making the seek bar’s pictures for" } else { "Reading the English lines of" };
            set_task(slot, Some(task(&show, &episode, doing)));
            let _ = std::fs::create_dir_all(sheet.parent().expect("a folder"));
            // Telly's copy of an episode is quick to read: its pictures are made from it, on their own.
            let from_rip = pictures && episode.play_path == episode.path;
            if pictures && !from_rip {
                let _ = media::make_sheet(&ffmpeg, Path::new(&episode.play_path), episode.duration, &sheet);
            }
            if from_rip || english {
                let drive = backend.jobs.drive(rip);
                let _turn = drive.take();
                if let Ok(tracks) = media::read_rip(&ffmpeg, rip, &probe, from_rip.then_some(sheet.as_path())) {
                    keep_translation(backend, episode.id, &probe, &tracks);
                }
            }
            set_task(slot, None);
        }
    }
}

/// The audio track a show's episodes play, as a position among the rip's
/// audio tracks: the one picked in the player, else the Japanese one.
pub fn audio_track(backend: &Backend, show_id: i64, probe: &media::Probe) -> usize {
    backend
        .db
        .setting(&audio_setting(show_id))
        .ok()
        .flatten()
        .and_then(|v| v.as_u64())
        .map(|t| t as usize)
        // An episode with fewer audio tracks than the one picked plays its Japanese one.
        .filter(|t| *t < probe.audio.len())
        .unwrap_or_else(|| media::japanese_audio(probe))
}

pub fn audio_setting(show_id: i64) -> String {
    format!("audio.track.{show_id}")
}

/// The audio track an episode that's ready plays.
pub fn playing_track(episode: &Episode, probe: &media::Probe) -> usize {
    episode.audio_track.map(|t| t as usize).unwrap_or_else(|| media::japanese_audio(probe))
}

/// Which of the audio tracks the player lists is the one a ready episode
/// plays. The player lists the tracks it can play of the file it loads, in
/// the file's order: all of a rip that plays as it is, the only one of a copy.
pub fn listed_track(episode: &Episode, probe: &media::Probe) -> usize {
    probe.audio.iter().take(episode.audio_index as usize).filter(|a| media::audio_plays(a)).count()
}

/// What ffprobe found in an episode's rip when it was got ready.
pub fn stored_probe(backend: &Backend, episode_id: i64) -> Option<media::Probe> {
    serde_json::from_str(&backend.db.probe(episode_id).ok()?).ok()
}

/// Gives a ready episode another of the rip's audio tracks, keeping its
/// picture: seconds, where converting again takes minutes.
pub fn change_audio(backend: &Backend, ffmpeg: &Path, episode: &Episode, probe: &media::Probe, track: usize) -> Result<()> {
    let source = PathBuf::from(&episode.path);
    let output = copy_path(backend, episode.show_id, episode.id);
    let forced = backend.db.setting_bool(&format!("convert.force.{}", episode.id), false);
    if !forced && media::plan(probe, &source, track) == Plan::Direct {
        let _ = std::fs::remove_file(&output);
        backend.db.media_ready(episode.id, &episode.path, track as i64, track as i64)?;
        return Ok(());
    }
    std::fs::create_dir_all(output.parent().expect("a folder"))?;
    let partial = output.with_extension("part");
    let video = if output.is_file() { &output } else { &source };
    let args = media::swap_audio_args(probe, video, &source, track, &partial);
    if let Err(e) = media::run_ffmpeg(ffmpeg, &args, probe.duration, &AtomicBool::new(false), &|_| {}) {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, &output).context("saving the video with its new audio")?;
    backend.db.media_ready(episode.id, &output.to_string_lossy(), 0, track as i64)?;
    Ok(())
}

/// Telly's copy of an episode: <copies folder>\<show>\<episode>.mp4, in
/// %LOCALAPPDATA%\Telly\media unless Settings chose another copies folder.
pub fn copy_path(backend: &Backend, show_id: i64, episode_id: i64) -> PathBuf {
    backend.media_dir().join(show_id.to_string()).join(format!("{episode_id}.mp4"))
}

/// The seek bar's pictures of an episode, beside where Telly's copy goes.
pub fn sheet_path(backend: &Backend, show_id: i64, episode_id: i64) -> PathBuf {
    copy_path(backend, show_id, episode_id).with_extension("jpg")
}

/// Moves Telly's copies of episodes into the copies folder (Settings just
/// chose another one), in the background. A copy on another drive is copied
/// across, then deleted where it was.
pub fn move_copies(backend: Arc<Backend>) {
    std::thread::spawn(move || {
        let copies: Vec<Episode> = backend.db.all_episodes().unwrap_or_default().into_iter().filter(|e| !e.play_path.is_empty() && e.play_path != e.path).collect();
        let task = |progress: f64| {
            let label = "Moving Telly’s copies of episodes".to_string();
            set_task(&backend.jobs.download, Some(Task { show_id: 0, episode_id: 0, number: 0, title: "Telly".into(), label, progress }))
        };
        let mut failed: Vec<String> = Vec::new();
        for (i, e) in copies.iter().enumerate() {
            task(i as f64 / copies.len() as f64);
            let from = PathBuf::from(&e.play_path);
            let to = copy_path(&backend, e.show_id, e.id);
            if from == to || !from.is_file() {
                continue;
            }
            let moved = (|| -> Result<()> {
                std::fs::create_dir_all(to.parent().expect("a folder"))?;
                if std::fs::rename(&from, &to).is_err() {
                    std::fs::copy(&from, &to)?;
                    backend.db.set_play_path(e.id, &to.to_string_lossy())?;
                    // An episode playing stays open; its old copy goes with Free up space, or by hand.
                    let _ = std::fs::remove_file(&from);
                    return Ok(());
                }
                backend.db.set_play_path(e.id, &to.to_string_lossy())
            })();
            if let Err(error) = moved {
                failed.push(format!("Episode {}: {error:#}", e.number));
            }
        }
        set_task(&backend.jobs.download, None);
        if failed.is_empty() {
            backend.push_event("done", "Copies moved", &backend.media_dir().to_string_lossy(), 0, 0);
        } else {
            backend.push_event("failed", "Some copies didn’t move", &failed.join("\n"), 0, 0);
        }
    });
}

/// How an episode gets playing, from what ffprobe found in its rip, and the
/// audio track it plays.
fn plan_for(backend: &Backend, episode: &Episode, probe: &media::Probe) -> (Plan, usize) {
    let forced = backend.db.setting_bool(&format!("convert.force.{}", episode.id), false);
    let audio = audio_track(backend, episode.show_id, probe);
    (if forced { Plan::Video } else { media::plan(probe, Path::new(&episode.path), audio) }, audio)
}

/// Reads what's in an episode's rip and plans how to get it playing. A rip
/// that plays as it is is ready there and then: None. Otherwise, what
/// `convert_video` needs: what ffprobe found, the plan and the audio track.
fn plan_video(backend: &Backend, ffprobe: &Path, show: &Show, episode: &Episode) -> Result<Option<(media::Probe, Plan, usize)>> {
    let source = PathBuf::from(&episode.path);
    if !source.is_file() {
        return Err(anyhow!("The file isn’t there any more: {}", episode.path));
    }
    let probe = media::probe(ffprobe, &source)?;
    let (plan, audio) = plan_for(backend, episode, &probe);
    let probe_json = serde_json::to_string(&probe).unwrap_or_default();
    backend.db.set_probe(episode.id, &probe_json, probe.duration, media::ending_at(&probe), plan.name())?;
    if plan == Plan::Direct {
        // A copy made for another audio track isn't needed any more.
        let _ = std::fs::remove_file(copy_path(backend, show.id, episode.id));
        backend.db.media_ready(episode.id, &episode.path, audio as i64, audio as i64)?;
        return Ok(None);
    }
    Ok(Some((probe, plan, audio)))
}

/// Makes Telly's copy of an episode whose rip doesn't play as it is (see `plan_video`).
#[allow(clippy::too_many_arguments)]
fn convert_video(backend: &Backend, ffmpeg: &Path, encoder: &str, show: &Show, episode: &Episode, probe: &media::Probe, plan: Plan, audio: usize) -> Result<()> {
    let source = PathBuf::from(&episode.path);
    let output = copy_path(backend, show.id, episode.id);
    if output.is_file() && std::fs::metadata(&output).map(|m| m.len() > 0).unwrap_or(false) {
        if playing_track(episode, probe) != audio {
            update_task(&backend.jobs.media, Some(&format!("Changing the audio of {} {}", show.title, episode.number)), 0.0);
            return change_audio(backend, ffmpeg, episode, probe, audio);
        }
        backend.db.media_ready(episode.id, &output.to_string_lossy(), 0, audio as i64)?;
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
    let mut result = media::run_ffmpeg(ffmpeg, &media::convert_args(probe, plan, encoder, &source, &partial, audio), probe.duration, &backend.jobs.cancel_media, &report);
    // A GPU encoder can fail on some inputs; the CPU one always works.
    if plan == Plan::Video && encoder != "libx264" && result.as_ref().is_err_and(|e| e.to_string() != "cancelled") {
        result = media::run_ffmpeg(ffmpeg, &media::convert_args(probe, plan, "libx264", &source, &partial, audio), probe.duration, &backend.jobs.cancel_media, &report);
    }
    if let Err(e) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, &output).context("saving the converted video")?;
    // The seek bar's pictures of the copy, so the player needn't make them when it first plays.
    let _ = media::make_sheet(ffmpeg, &output, probe.duration, &sheet_path(backend, show.id, episode.id));
    backend.db.media_ready(episode.id, &output.to_string_lossy(), 0, audio as i64)?;
    Ok(())
}

// ---- subtitles -----------------------------------------------------------------

/// One of the workers finding subtitles: the `n`th, whose task shows in `jobs.subs[n]`.
fn subs_worker(backend: &Backend, n: usize) {
    let slot = &backend.jobs.subs[n];
    let mut seen = 0;
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        let tools = backend.tools();
        let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else { continue };
        while let Some((show, episode, count)) = take(backend, "subs") {
            set_task(slot, Some(task(&show, &episode, "Finding subtitles for")));
            let result = find_subtitles(backend, slot, &ffmpeg, &ffprobe, &show, &episode, count);
            set_task(slot, None);
            if let Err(e) = result {
                let _ = backend.db.set_subs_state(episode.id, "failed", &format!("{e:#}"));
            }
        }
        // Then episodes you asked to have subtitles made for, one at a time: the first worker makes them.
        if n > 0 {
            continue;
        }
        if let Some(listener) = listener(backend, &tools) {
            if let Some((show, episode, _)) = pick(backend, "whisper") {
                let name = listener.name();
                if backend.db.claim_whisper(episode.id, &format!("{name} is listening")).unwrap_or(false) {
                    set_task(slot, Some(task(&show, &episode, &format!("{name} is listening to"))));
                    let result = match &listener {
                        Listener::OpenRouter { key, model } => openrouter_subtitles(backend, slot, &ffmpeg, &ffprobe, key, model, &episode),
                        Listener::Local { whisper, model } => whisper_subtitles(backend, &ffmpeg, &ffprobe, whisper, model, &episode),
                    };
                    set_task(slot, None);
                    if let Err(e) = result {
                        let _ = backend.db.set_subs_state(episode.id, "failed", &format!("{name}: {e:#}"));
                    }
                    backend.jobs.poke();
                }
            }
        }
    }
}

/// What makes subtitles for episodes Jimaku has nothing for.
pub enum Listener {
    /// A model on OpenRouter, with the key for it.
    OpenRouter { key: String, model: String },
    /// whisper.cpp on this computer, with its model.
    Local { whisper: PathBuf, model: PathBuf },
}

impl Listener {
    /// Who its subtitles are marked as made by.
    pub fn name(&self) -> &'static str {
        match self {
            Listener::OpenRouter { model, .. } => openrouter::maker(model),
            Listener::Local { .. } => "Whisper",
        }
    }
}

/// OpenRouter once Settings has a key for it, else whisper.cpp when it and a model are here.
pub fn listener(backend: &Backend, tools: &crate::tools::Tools) -> Option<Listener> {
    if let Some((key, model)) = openrouter::chosen(backend) {
        return Some(Listener::OpenRouter { key, model });
    }
    let (whisper, model) = tools.whisper.clone().zip(tools.whisper_model.clone())?;
    Some(Listener::Local { whisper, model })
}

/// Makes subtitles from the episode's Japanese audio with a model on
/// OpenRouter, marked as machine-made and timed by the model. The audio goes
/// in pieces (see openrouter.rs), each a file in the subtitles folder for a moment.
#[allow(clippy::too_many_arguments)]
fn openrouter_subtitles(backend: &Backend, slot: &Mutex<Option<Task>>, ffmpeg: &Path, ffprobe: &Path, key: &str, model: &str, episode: &Episode) -> Result<()> {
    /// Enough to find where the audio is quiet.
    const RATE: u32 = 8000;
    let source = Path::new(&episode.path);
    let track = media::japanese_audio(&media::probe(ffprobe, source)?);
    let cuts = openrouter::cuts(&media::read_audio(ffmpeg, source, track, RATE, None, None)?, RATE as usize);
    std::fs::create_dir_all(backend.dirs.subs())?;
    let piece = backend.dirs.subs().join(format!("{}.piece.flac", episode.id));
    let api = openrouter::api_address();
    let mut lines: Vec<Line> = Vec::new();
    for (i, cut) in cuts.windows(2).enumerate() {
        media::write_flac(ffmpeg, source, track, cut[0], cut[1] - cut[0], &piece)?;
        let flac = std::fs::read(&piece);
        let _ = std::fs::remove_file(&piece);
        let heard = openrouter::transcribe(&api, key, model, &flac.context("reading a piece of the audio")?)?;
        lines.extend(heard.into_iter().map(|mut line| {
            line.start += cut[0];
            line.end += cut[0];
            line
        }));
        update_task(slot, None, (i + 1) as f64 / (cuts.len() - 1) as f64);
    }
    let maker = openrouter::maker(model);
    // Repeating itself over music is Whisper's habit.
    let lines = if maker == "Whisper" { without_repeats(lines) } else { lines };
    write_lines(backend, episode.id, &lines)?;
    backend.db.set_subs(episode.id, "synced", &format!("Made by {maker} (machine-made)"), maker, None, None)?;
    crate::premine::prepare(backend, episode.id);
    Ok(())
}

/// Where Whisper's models come from: whisper.cpp's own on Hugging Face.
const WHISPER_MODELS_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
/// The model used unless Settings picks a bigger one.
pub const WHISPER_MODEL_NAME: &str = "ggml-small-q5_1.bin";

/// The model file Settings picked ("whisper.model"), else the small one.
pub fn whisper_model_name(backend: &Backend) -> String {
    match backend.db.setting_str("whisper.model") {
        name if name.is_empty() => WHISPER_MODEL_NAME.to_string(),
        name => name,
    }
}

/// Makes subtitles from the episode's Japanese audio with whisper.cpp,
/// marked as machine-made. Whisper times them itself, so they need no sync.
fn whisper_subtitles(backend: &Backend, ffmpeg: &Path, ffprobe: &Path, whisper: &Path, model: &Path, episode: &Episode) -> Result<()> {
    let source = Path::new(&episode.path);
    std::fs::create_dir_all(backend.dirs.subs())?;
    let subs_dir = std::path::absolute(backend.dirs.subs()).unwrap_or_else(|_| backend.dirs.subs());
    let wav = subs_dir.join(format!("{}.whisper.wav", episode.id));
    // One transcript per model, so a bigger model's isn't taken for a smaller one's.
    let made_by = model.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // whisper.cpp adds ".srt" to this.
    let out = subs_dir.join(format!("{}.whisper.{made_by}", episode.id));
    let srt = subs_dir.join(format!("{}.whisper.{made_by}.srt", episode.id));
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
    crate::premine::prepare(backend, episode.id);
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

/// Downloads Whisper's model (the one Settings picked) into Telly's models folder, in the background.
pub fn get_whisper_model(backend: std::sync::Arc<Backend>) {
    std::thread::spawn(move || {
        let name = whisper_model_name(&backend);
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
            let partial = dir.join(format!("{name}.part"));
            let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3600))).build().into();
            let mut response = agent.get(format!("{WHISPER_MODELS_URL}/{name}")).call().map_err(|e| anyhow!("couldn’t download the model ({e})"))?;
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
            std::fs::rename(&partial, dir.join(&name))?;
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

pub fn translation_path(backend: &Backend, episode_id: i64) -> PathBuf {
    backend.dirs.subs().join(format!("{episode_id}.en.json"))
}

/// Whether a rip has English lines as text that aren't kept yet.
fn lacks_translation(backend: &Backend, episode_id: i64, probe: &media::Probe) -> bool {
    media::english_text_track(probe).is_some() && !translation_path(backend, episode_id).is_file()
}

/// Keeps a rip's English lines for the player, without its signs: `text` is
/// the track as a subtitle file of the kind `format`. A track with no lines
/// Telly can read is kept as none, so the rip isn't read for them again.
fn keep_lines(backend: &Backend, episode_id: i64, text: &str, format: &str) -> Result<Vec<Line>> {
    let lines: Vec<Line> = subs::parse(&format!("track.{format}"), text.as_bytes()).unwrap_or_default().into_iter().filter(|l| !l.sign).collect();
    std::fs::create_dir_all(backend.dirs.subs())?;
    std::fs::write(translation_path(backend, episode_id), serde_json::to_vec(&lines)?)?;
    Ok(lines)
}

/// Keeps a rip's English lines from its tracks as `media::read_rip` just read
/// them, when they aren't kept yet: the player then never reads the rip for them.
fn keep_translation(backend: &Backend, episode_id: i64, probe: &media::Probe, tracks: &[media::TrackTiming]) {
    if !lacks_translation(backend, episode_id, probe) {
        return;
    }
    let read = media::english_text_track(probe).and_then(|english| tracks.iter().find(|t| t.index == english.index));
    if let Some(track) = read.filter(|t| !t.text.is_empty()) {
        let _ = keep_lines(backend, episode_id, &track.text, media::text_format(&track.codec));
    }
}

/// The rip's own English subtitles, timed to it already, for the player to
/// show while Tab is held. Kept when the rip is read for its subtitles; else
/// read from the rip once, then kept. None when the rip has none as text
/// (picture subtitles can't be shown this way).
pub fn translation(backend: &Backend, episode: &Episode) -> Result<Vec<Line>> {
    let kept = translation_path(backend, episode.id);
    if let Some(lines) = std::fs::read(&kept).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        return Ok(lines);
    }
    let Some(probe) = stored_probe(backend, episode.id) else { return Ok(Vec::new()) };
    let Some(track) = media::english_text_track(&probe) else { return Ok(Vec::new()) };
    let ffmpeg = backend.tools().ffmpeg.ok_or_else(|| anyhow!("Telly needs ffmpeg for this. Choose it in Settings."))?;
    let format = media::text_format(&track.codec);
    let text = media::read_subtitle_track(&ffmpeg, Path::new(&episode.path), track.index, format)?;
    keep_lines(backend, episode.id, &text, format)
}

/// Where people speak in an episode, from its Japanese track, and how long it
/// is: FireRedVAD's view, or WebRTC's detector if FireRedVAD can't run.
/// The bool says whether it's FireRedVAD's (clean) view.
fn speech_of(ffmpeg: &Path, probe: &media::Probe, episode: &Episode) -> Result<(Vec<(f64, f64)>, f64, bool)> {
    let source = Path::new(&episode.path);
    let audio = media::japanese_audio(probe);
    let samples = media::read_audio_filtered(ffmpeg, source, audio, sync::FIRERED_RATE, None, None, None)?;
    let length = samples.len() as f64 / sync::FIRERED_RATE as f64;
    match sync::firered_spans(&samples) {
        Ok(spans) if !spans.is_empty() => Ok((spans, length, true)),
        _ => {
            let samples = media::read_audio_filtered(ffmpeg, source, audio, sync::RATE, None, None, Some(sync::VOICE_FILTER))?;
            Ok((sync::voice_spans(&samples), length, false))
        }
    }
}

/// What subtitle files are matched against: when the rip's own dialogue
/// subtitles show (in any language, pictures included), or where its people speak.
pub struct Timing {
    pub spans: Vec<(f64, f64)>,
    pub length: f64,
    /// For the episode's details: "the rip’s English subtitles", "the speech".
    pub against: String,
    /// Seconds subtitles show before the voice starts. Matched to speech, a
    /// file lands this much late (0.1 s, measured on Fairy Tail's Blu-rays
    /// against files made for them); matched to subtitles, nothing.
    pub lead: f64,
    /// How sure a fit to this timing is (see `sync::Calibration`).
    pub calibration: sync::Calibration,
}

/// Syncs a file to `timing`, moving it back by the timing's lead.
pub fn sync_to(lines: &[Line], timing: &Timing) -> sync::Synced {
    led(sync::sync_with(lines, &timing.spans, timing.length, &timing.calibration), timing)
}

/// `lines` cut into pieces to fit `timing` (see `sync::sync_in_pieces`), at
/// the speed change (`ratio`) their sync with one shift found.
pub fn sync_to_in_pieces(lines: &[Line], ratio: f64, timing: &Timing) -> Option<sync::Synced> {
    sync::sync_in_pieces(lines, ratio, &timing.spans, timing.length, &timing.calibration).map(|synced| led(synced, timing))
}

/// `synced` moved back by the timing's lead.
fn led(mut synced: sync::Synced, timing: &Timing) -> sync::Synced {
    if timing.lead > 0.0 {
        for line in &mut synced.lines {
            line.start = (line.start - timing.lead).max(0.0);
            line.end = (line.end - timing.lead).max(line.start + 0.05);
        }
        synced.offset_ms -= (timing.lead * 1000.0).round() as i64;
    }
    synced
}

pub fn language_name(code: &str) -> &'static str {
    match code.to_lowercase().as_str() {
        "eng" | "en" => "English ",
        "jpn" | "ja" | "jp" => "Japanese ",
        "spa" | "es" => "Spanish ",
        "fre" | "fra" | "fr" => "French ",
        "ger" | "deu" | "de" => "German ",
        "por" | "pt" => "Portuguese ",
        "ita" | "it" => "Italian ",
        "chi" | "zho" | "zh" => "Chinese ",
        "kor" | "ko" => "Korean ",
        _ => "",
    }
}

/// The rip's own dialogue subtitles when it has them: timed to this very
/// video, they place a file far more surely than the speech does. Otherwise its speech.
pub fn timing_of(ffmpeg: &Path, probe: &media::Probe, episode: &Episode) -> Result<Timing> {
    let tracks = media::read_rip(ffmpeg, Path::new(&episode.path), probe, None).unwrap_or_default();
    match subtitle_timing(probe, &tracks) {
        Some(timing) => Ok(timing),
        None => speech_timing(ffmpeg, probe, episode),
    }
}

/// The timing the rip's own dialogue subtitles give, from its subtitle tracks
/// as `media::read_rip` read them. None when it has no such track.
fn subtitle_timing(probe: &media::Probe, tracks: &[media::TrackTiming]) -> Option<Timing> {
    let track = media::dialogue_track(tracks)?;
    let end = track.events.last().map(|e| e.1).unwrap_or(0.0);
    Some(Timing {
        spans: track.events.clone(),
        length: probe.duration.max(end),
        against: format!("the rip’s {}subtitles", language_name(&track.language)),
        lead: 0.0,
        calibration: sync::Calibration::CLEAN,
    })
}

/// The timing the episode's speech gives, heard in its Japanese audio.
fn speech_timing(ffmpeg: &Path, probe: &media::Probe, episode: &Episode) -> Result<Timing> {
    let (spans, length, clean) = speech_of(ffmpeg, probe, episode)?;
    let calibration = if clean { sync::Calibration::CLEAN } else { sync::Calibration::NOISY };
    Ok(Timing { spans, length, against: "the speech".into(), lead: 0.1, calibration })
}

/// An entry's files, cached for an hour.
fn entry_files(backend: &Backend, key: &str, id: i64) -> Result<Vec<jimaku::Local>> {
    let cached = backend
        .jobs
        .jimaku_files
        .lock()
        .ok()
        .and_then(|c| c.get(&id).filter(|(at, _)| at.elapsed() < Duration::from_secs(3600)).map(|(_, f)| f.clone()));
    if let Some(files) = cached {
        return Ok(files);
    }
    let files = jimaku::fetch_all(key, id, &backend.dirs.jimaku().join(id.to_string()))?;
    if let Ok(mut c) = backend.jobs.jimaku_files.lock() {
        c.insert(id, (Instant::now(), files.clone()));
    }
    Ok(files)
}

/// Jimaku's files for a show.
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
        all.extend(entry_files(backend, key, id)?);
    }
    Ok(all)
}

/// The Jimaku files of a season folder's own AniList entry, for a show added
/// as one folder holding several seasons (Fairy Tail's OVAs and 2014 series
/// are entries of their own). Which entry that is is worked out once, from
/// the folder's name, and kept.
fn group_files(backend: &Backend, key: &str, show: &Show, group: &str, in_group: usize) -> Result<Vec<jimaku::Local>> {
    if group.is_empty() {
        return Ok(Vec::new());
    }
    let setting = format!("jimaku.group.{}.{group}", show.id);
    let entry = match backend.db.setting(&setting)?.and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => {
            let title = scan::clean_title(group.rsplit('/').next().unwrap_or(group));
            let mut found = anilist::search_loosely(&title).unwrap_or_default();
            anilist::rank(&mut found, &title, in_group);
            let id = match found.first() {
                Some(a) if Some(a.id) != show.anilist_id && a.score >= 0.5 => {
                    jimaku::find_entries(key, Some(a.id), &a.romaji)?.first().map(|e| e.id).unwrap_or(-1)
                }
                _ => -1,
            };
            backend.db.set_setting(&setting, &serde_json::json!(id))?;
            id
        }
    };
    if entry <= 0 || Some(entry) == show.jimaku_id {
        return Ok(Vec::new());
    }
    entry_files(backend, key, entry)
}

/// The number in an episode file's own name.
fn local_number(path: &str) -> Option<i64> {
    let stem = Path::new(path).file_stem()?.to_string_lossy().to_string();
    scan::episode_number(&stem).map(|(_, e)| e)
}

fn offset_setting(show: i64, group: &str) -> String {
    format!("subs.numbering.{show}.{group}")
}

/// Whether a subtitle track is Japanese text: subtitles of the rip's own to keep or sync.
fn japanese_text(language: &str, codec: &str) -> bool {
    matches!(language.to_lowercase().as_str(), "jpn" | "ja" | "jp") && media::text_codec(codec)
}

/// Japanese text subtitles inside the rip itself (among `tracks`, read from it), written out as files.
fn embedded(backend: &Backend, tracks: &[media::TrackTiming], episode: &Episode) -> Vec<jimaku::Local> {
    let mut out = Vec::new();
    for track in tracks.iter().filter(|t| japanese_text(&t.language, &t.codec) && !t.text.is_empty()) {
        let path = backend.dirs.subs().join(format!("{}.track{}.{}", episode.id, track.index, media::text_format(&track.codec)));
        if std::fs::create_dir_all(backend.dirs.subs()).is_ok() && std::fs::write(&path, &track.text).is_ok() {
            out.push(jimaku::Local { name: format!("In the rip (track {})", track.index), path, episode: Some(episode.number) });
        }
    }
    out
}

/// Japanese subtitle files beside the rip and named after it: "Episode 01.srt",
/// "Episode 01.ja.srt", "Episode 01.alass.srt".
fn beside(episode: &Episode) -> Vec<jimaku::Local> {
    let rip = Path::new(&episode.path);
    let (Some(folder), Some(stem)) = (rip.parent(), rip.file_stem().and_then(|s| s.to_str())) else { return Vec::new() };
    let mut out: Vec<jimaku::Local> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.to_string();
            let ext = path.extension()?.to_str()?.to_ascii_lowercase();
            let named_after = name.strip_prefix(stem).is_some_and(|rest| rest.starts_with('.'));
            let wanted = named_after && matches!(ext.as_str(), "srt" | "ass" | "ssa" | "vtt") && japanese_lines(&path, &name).is_ok();
            wanted.then(|| jimaku::Local { name, path, episode: Some(episode.number) })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// A file a sync would move less than this is in sync already (a sync centres
/// its shift within half a second).
const IN_SYNC_MS: i64 = 500;

/// Whether a sync left one of the rip's own subtitles where it was: one shift
/// of under half a second, no change of speed. Whose subtitles they are isn't
/// in doubt, and a file that doesn't belong lands within half a second of
/// where it was once in 600 tries. So it needn't fit well, only better than
/// another episode's file would: where subtitles show two thirds of the time,
/// as in Bocchi the Rock!, the right file stands only 6 spreads up.
fn in_sync(synced: &sync::Synced) -> bool {
    synced.score >= FITS_NOT && !synced.cut && synced.shifts == 1 && (synced.ratio - 1.0).abs() < 1e-6 && synced.offset_ms.abs() < IN_SYNC_MS
}

/// Keeps the best of the rip's own subtitles (`own`) that's in sync as it is,
/// its lines untouched. False when none is.
fn keep_own(backend: &Backend, episode: &Episode, own: &[jimaku::Local], timing: &Timing, video: &str) -> Result<bool> {
    let mut tried: Vec<Tried> = Vec::new();
    let mut best: Option<(&jimaku::Local, sync::Synced, matching::Fit)> = None;
    for file in own {
        let path = file.path.to_string_lossy().into_owned();
        match sync_file(&file.path, &file.name, timing, video) {
            Ok((synced, fit)) => {
                tried.push(Tried { name: file.name.clone(), path, score: synced.score, offset_ms: synced.offset_ms, error: String::new() });
                if in_sync(&synced) && best.as_ref().is_none_or(|(_, _, b)| matching::better(&fit, b)) {
                    best = Some((file, synced, fit));
                }
            }
            Err(e) => tried.push(Tried { name: file.name.clone(), path, score: 0.0, offset_ms: 0, error: format!("{e:#}") }),
        }
    }
    let Some((file, synced, _)) = best else { return Ok(false) };
    write_lines(backend, episode.id, &japanese_lines(&file.path, &file.name)?)?;
    std::fs::write(tried_path(backend, episode.id), serde_json::to_vec(&tried)?)?;
    let detail = format!("in sync as it is · matched to {}", timing.against);
    backend.db.set_subs(episode.id, "synced", &detail, &file.name, Some(synced.score), Some(0))?;
    crate::premine::prepare(backend, episode.id);
    Ok(true)
}

/// Syncs one file to the episode's timing (its own subtitles or its speech),
/// with how it compares with the others (see `matching::better`).
pub fn sync_file(path: &Path, name: &str, timing: &Timing, video: &str) -> Result<(sync::Synced, matching::Fit)> {
    let lines = japanese_lines(path, name)?;
    let own = is_the_timing(&lines, timing);
    let synced = if own { as_timed(&lines, timing) } else { sync_to(&lines, timing) };
    let fit = fit_of(&synced, &lines, own || matching::made_for(name, video));
    Ok((synced, fit))
}

/// The same file cut into pieces (see `sync_to_in_pieces`), for one that
/// didn't sync with one shift (at `ratio`). None if the pieces don't fit.
pub fn sync_file_in_pieces(path: &Path, name: &str, ratio: f64, timing: &Timing, video: &str) -> Result<Option<(sync::Synced, matching::Fit)>> {
    let lines = japanese_lines(path, name)?;
    Ok(sync_to_in_pieces(&lines, ratio, timing).map(|synced| {
        let fit = fit_of(&synced, &lines, matching::made_for(name, video));
        (synced, fit)
    }))
}

fn japanese_lines(path: &Path, name: &str) -> Result<Vec<Line>> {
    let lines = subs::parse(name, &std::fs::read(path)?)?;
    if !subs::is_japanese(&lines) {
        return Err(anyhow!("not Japanese"));
    }
    Ok(lines)
}

fn fit_of(synced: &sync::Synced, lines: &[Line], made_for: bool) -> matching::Fit {
    matching::Fit { score: synced.score, quality: subs::quality(lines), offset_ms: synced.offset_ms, z: synced.standing.above, made_for, cut: synced.cut }
}

/// Whether `lines` are the subtitles `timing` was read from: the rip's own
/// Japanese track, timed to the episode already. Matched to itself, such a
/// track fits perfectly yet can stand as little as 5.7 spreads above chance
/// where the dialogue is dense (Fairy Tail 3's), which would read as unsure.
fn is_the_timing(lines: &[Line], timing: &Timing) -> bool {
    let mut spans: Vec<(f64, f64)> = lines.iter().filter(|l| !l.sign).map(|l| (l.start, l.end)).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    spans.len() == timing.spans.len() && spans.iter().zip(&timing.spans).all(|(a, b)| (a.0 - b.0).abs() < 0.001 && (a.1 - b.1).abs() < 0.001)
}

/// `lines` as they are, an exact fit to `timing`.
fn as_timed(lines: &[Line], timing: &Timing) -> sync::Synced {
    let spans: Vec<(f64, f64)> = lines.iter().filter(|l| !l.sign).map(|l| (l.start, l.end)).collect();
    sync::Synced { lines: lines.to_vec(), score: 1.0, offset_ms: 0, ratio: 1.0, shifts: 1, cut: false, standing: sync::standing(&spans, &timing.spans, timing.length) }
}

/// The file kept for an episode, synced.
pub struct Pick {
    pub synced: sync::Synced,
    pub fit: matching::Fit,
    pub name: String,
    /// The episode number the file was filed under.
    pub filed_as: Option<i64>,
}

/// How many files a search looks at fully, of those that screen best.
const SEARCH_TOP: usize = 6;

/// Parsed files' dialogue timing, kept while Telly runs: a search screens
/// every file of an entry, episode after episode.
fn screened(file: &jimaku::Local) -> Option<std::sync::Arc<Vec<(f64, f64)>>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, Option<std::sync::Arc<Vec<(f64, f64)>>>>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&file.path).cloned()) {
        return hit;
    }
    let spans = std::fs::read(&file.path)
        .ok()
        .and_then(|bytes| subs::parse(&file.name, &bytes).ok())
        .filter(|lines| subs::is_japanese(lines))
        .map(|lines| std::sync::Arc::new(lines.iter().filter(|l| !l.sign).map(|l| (l.start, l.end)).collect::<Vec<_>>()));
    if let Ok(mut c) = cache.lock() {
        c.insert(file.path.clone(), spans.clone());
    }
    spans
}

/// Syncs the files that could be this episode's (`files`, likeliest first)
/// and keeps the best (`matching::better`). When none of them fits, it
/// searches the rest of `pool`: every file is screened by `sync::quick_fit`,
/// and the few that screen best are synced in full. That finds an episode
/// filed under a number nothing predicted (Judas's Fairy Tail 2014 runs in
/// another order than Netflix's from its 48th episode). When still nothing
/// syncs with one shift, each file is tried cut into pieces, as a file timed
/// to a TV broadcast needs (see `sync::sync_in_pieces`).
pub fn pick_subtitles(files: &[jimaku::Local], pool: &[jimaku::Local], timing: &Timing, video: &str, mut progress: impl FnMut(&str, f64)) -> (Option<Pick>, Vec<Tried>) {
    let mut tried: Vec<Tried> = Vec::new();
    let mut best: Option<Pick> = None;
    // The files that synced, with the speed change each took.
    let mut synced_files: Vec<(jimaku::Local, f64)> = Vec::new();
    let mut consider = |file: &jimaku::Local, tried: &mut Vec<Tried>, best: &mut Option<Pick>| match sync_file(&file.path, &file.name, timing, video) {
        Ok((synced, fit)) => {
            tried.push(Tried { name: file.name.clone(), path: file.path.to_string_lossy().into_owned(), score: synced.score, offset_ms: synced.offset_ms, error: String::new() });
            synced_files.push((file.clone(), synced.ratio));
            if best.as_ref().is_none_or(|b| matching::better(&fit, &b.fit)) {
                *best = Some(Pick { synced, fit, name: file.name.clone(), filed_as: file.episode });
            }
        }
        Err(e) => tried.push(Tried { name: file.name.clone(), path: file.path.to_string_lossy().into_owned(), score: 0.0, offset_ms: 0, error: format!("{e:#}") }),
    };
    let first: Vec<&jimaku::Local> = files.iter().take(MAX_TRIES).collect();
    for (i, file) in first.iter().enumerate() {
        progress("Syncing subtitles for", i as f64 / first.len() as f64);
        consider(file, &mut tried, &mut best);
    }
    if best.as_ref().is_none_or(|b| b.fit.score < GOOD_SYNC) && !pool.is_empty() {
        progress("Searching every subtitle file for", 0.0);
        let done: std::collections::HashSet<&PathBuf> = first.iter().map(|f| &f.path).collect();
        let mut screen: Vec<(f64, &jimaku::Local)> = pool
            .iter()
            .filter(|f| !done.contains(&f.path))
            .filter_map(|f| screened(f).map(|spans| (sync::quick_fit(&spans, &timing.spans), f)))
            .collect();
        screen.sort_by(|a, b| b.0.total_cmp(&a.0));
        let top: Vec<&jimaku::Local> = screen.into_iter().take(SEARCH_TOP).map(|(_, f)| f).collect();
        for (i, file) in top.iter().enumerate() {
            progress("Searching every subtitle file for", (i + 1) as f64 / top.len() as f64);
            consider(file, &mut tried, &mut best);
        }
    }
    if best.as_ref().is_none_or(|b| b.fit.score < GOOD_SYNC) {
        for (i, (file, ratio)) in synced_files.iter().enumerate() {
            progress("Syncing subtitles for", i as f64 / synced_files.len() as f64);
            let Ok(Some((synced, fit))) = sync_file_in_pieces(&file.path, &file.name, *ratio, timing, video) else { continue };
            if let Some(t) = tried.iter_mut().find(|t| Path::new(&t.path) == file.path) {
                (t.score, t.offset_ms) = (synced.score, synced.offset_ms);
            }
            if best.as_ref().is_none_or(|b| matching::better(&fit, &b.fit)) {
                best = Some(Pick { synced, fit, name: file.name.clone(), filed_as: file.episode });
            }
        }
    }
    (best, tried)
}

fn find_subtitles(backend: &Backend, slot: &Mutex<Option<Task>>, ffmpeg: &Path, ffprobe: &Path, show: &Show, episode: &Episode, count: usize) -> Result<()> {
    let key = backend.db.setting_str("jimaku.key");
    let rip = Path::new(&episode.path);
    let probe = media::probe(ffprobe, rip)?;
    // One rip is read through at a time on each drive (see `Jobs::drive`).
    let drive = backend.jobs.drive(rip);
    let reading = || {
        let turn = drive.take();
        update_task(slot, Some(&format!("Reading the timing of {} {}", show.title, episode.number)), 0.0);
        turn
    };
    // The seek bar's pictures are made in the same read when the rip itself
    // is what plays, and its English lines kept: the player then needn't read
    // the rip when the episode first plays.
    let sheet = Some(sheet_path(backend, show.id, episode.id)).filter(|sheet| !sheet.is_file() && plan_for(backend, episode, &probe).0 == Plan::Direct);
    // The rip's own subtitle tracks, read in one pass over it the first time they're wanted.
    let tracks = std::cell::OnceCell::new();
    let rip_tracks = || {
        tracks.get_or_init(|| {
            let _turn = reading();
            if let Some(folder) = sheet.as_deref().and_then(Path::parent) {
                let _ = std::fs::create_dir_all(folder);
            }
            let tracks = media::read_rip(ffmpeg, rip, &probe, sheet.as_deref()).unwrap_or_default();
            keep_translation(backend, episode.id, &probe, &tracks);
            tracks
        })
    };
    if sheet.is_some() || lacks_translation(backend, episode.id, &probe) {
        rip_tracks();
    }
    let mut files = if probe.subtitles.iter().any(|s| japanese_text(&s.language, &s.codec)) { embedded(backend, rip_tracks(), episode) } else { Vec::new() };
    files.extend(beside(episode));
    let video = rip.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let read_timing = || match subtitle_timing(&probe, rip_tracks()) {
        Some(timing) => Ok(timing),
        None => {
            let _turn = reading();
            speech_timing(ffmpeg, &probe, episode).context("reading the episode’s timing")
        }
    };
    // The rip's own Japanese subtitles (a track inside it, a file beside it)
    // are kept as they are when they're in sync already, and Jimaku isn't asked.
    let mut timing = None;
    if !files.is_empty() {
        let read = read_timing()?;
        if keep_own(backend, episode, &files, &read, &video)? {
            return Ok(());
        }
        timing = Some(read);
    }
    // Where this episode sits among the show's season folders.
    let listing: Vec<(i64, String)> = backend.db.episodes(show.id)?.into_iter().map(|e| (e.number, e.path)).collect();
    let groups = matching::groups(&show.folder, &listing);
    let group = matching::group_of(&show.folder, &episode.path);
    let local = local_number(&episode.path);
    let mut pool: Vec<jimaku::Local> = Vec::new();
    if !key.trim().is_empty() {
        // One worker asks Jimaku at a time, about the show as it is now:
        // another worker may just have found its entry there.
        let asking = backend.jobs.jimaku.lock();
        let show = &backend.db.show(show.id)?.unwrap_or_else(|| show.clone());
        let mut all = jimaku_files(backend, key.trim(), show)?;
        if groups.len() > 1 {
            let in_group = groups.iter().find(|(g, _)| *g == group).map(|(_, c)| *c).unwrap_or(0);
            all.extend(group_files(backend, key.trim(), show, &group, in_group).unwrap_or_default());
        }
        drop(asking);
        let earlier: Vec<usize> = groups.iter().take_while(|(g, _)| *g != group).map(|(_, c)| *c).collect();
        let learned = backend.db.setting(&offset_setting(show.id, &group)).ok().flatten().and_then(|v| v.as_i64());
        let mut seen = std::collections::HashSet::new();
        for n in matching::numbers_for(episode.number, local, &earlier, learned) {
            for file in jimaku::candidates(&all, n, count) {
                if seen.insert(file.path.clone()) {
                    files.push(file);
                }
            }
        }
        pool = all;
    }
    if files.is_empty() && pool.is_empty() {
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
    let timing = match timing {
        Some(read) => read,
        None => read_timing()?,
    };
    let (best, tried) = pick_subtitles(&files, &pool, &timing, &video, |what, progress| {
        update_task(slot, Some(&format!("{what} {} {}", show.title, episode.number)), progress);
    });
    std::fs::create_dir_all(backend.dirs.subs())?;
    std::fs::write(tried_path(backend, episode.id), serde_json::to_vec(&tried)?)?;
    let Some(Pick { synced, name, filed_as, .. }) = best else {
        backend.db.set_subs(episode.id, "none", "None of Jimaku’s files for this episode are Japanese subtitles Telly can read", "", None, None)?;
        return Ok(());
    };
    if synced.score < FITS_NOT {
        // Every file is another episode's, or another cut's: none is better than wrong ones.
        let _ = std::fs::remove_file(lines_path(backend, episode.id));
        backend.db.set_subs(episode.id, "none", &format!("None of Jimaku’s {} files fit this episode", tried.len()), "", Some(synced.score), None)?;
        return Ok(());
    }
    // The numbering that fitted, for the rest of this season folder.
    if synced.score >= GREAT_SYNC {
        if let (Some(filed), Some(l)) = (filed_as, local) {
            let _ = backend.db.set_setting(&offset_setting(show.id, &group), &serde_json::json!(filed - l));
        }
    }
    write_lines(backend, episode.id, &synced.lines)?;
    let state = if synced.score >= GOOD_SYNC { "synced" } else { "check" };
    let detail = format!("{} · matched to {}", describe(&synced), timing.against);
    backend.db.set_subs(episode.id, state, &detail, &name, Some(synced.score), Some(synced.offset_ms))?;
    crate::premine::prepare(backend, episode.id);
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
    let timing = timing_of(&ffmpeg, &probe, episode)?;
    let bytes = std::fs::read(path)?;
    let lines = subs::parse(name, &bytes)?;
    let mut synced = sync_to(&lines, &timing);
    if synced.score < GOOD_SYNC {
        if let Some(cut) = sync_to_in_pieces(&lines, synced.ratio, &timing) {
            synced = cut;
        }
    }
    write_lines(backend, episode.id, &synced.lines)?;
    let state = if synced.score >= GOOD_SYNC { "synced" } else { "check" };
    let detail = format!("{} · matched to {}", describe(&synced), timing.against);
    backend.db.set_subs(episode.id, state, &detail, name, Some(synced.score), Some(synced.offset_ms))?;
    crate::premine::prepare(backend, episode.id);
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
            audio_track: None,
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
    fn workers_never_take_the_same_episode() {
        let dir = std::env::temp_dir().join(format!("telly-take-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = (1..=40).map(|number| crate::db::NewEpisode { number, path: format!("{number}.mkv"), size: 1, duration: 1400.0 }).collect();
        backend.db.add_show(&crate::db::NewShow { folder: "D:\\Anime\\Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        // Four workers take subtitle work until none is left: each episode goes to one of them.
        let mut taken: Vec<i64> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4).map(|_| scope.spawn(|| std::iter::from_fn(|| take(&backend, "subs")).map(|(_, e, _)| e.number).collect::<Vec<_>>())).collect();
            workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
        });
        taken.sort();
        assert_eq!(taken, (1..=40).collect::<Vec<_>>());
        // A rip nobody has looked at goes to either video worker. Once its
        // plan says it needs converting, it's left for the one that converts.
        let (_, first, _) = take(&backend, "probe").unwrap();
        assert_eq!(first.number, 1);
        backend.db.set_probe(first.id, "{}", 1400.0, None, "video").unwrap();
        backend.db.media_reset(first.id).unwrap();
        assert_eq!(take(&backend, "probe").map(|(_, e, _)| e.number), Some(2));
        assert_eq!(take(&backend, "media").map(|(_, e, _)| e.number), Some(1));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_the_player_wants_of_a_rip_is_kept_or_caught_up() {
        let dir = std::env::temp_dir().join(format!("telly-catch-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = (1..=2).map(|number| crate::db::NewEpisode { number, path: format!("{number}.mkv"), size: 1, duration: 1400.0 }).collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: "D:\\Anime\\Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let first = backend.db.episodes(show).unwrap().remove(0);
        // The rip's English lines are kept from its tracks as they were read: the full track's, without its signs.
        let track = |index: i64, title: &str, text: &str| (
            media::SubtitleInfo { index, codec: "subrip".into(), language: "eng".into(), title: title.into() },
            media::TrackTiming { index, codec: "subrip".into(), text: text.into(), ..Default::default() },
        );
        let (signs, read_signs) = track(2, "Signs & Songs", "1\n00:00:01,000 --> 00:00:03,000\nFAIRY TAIL\n\n");
        let (full, read_full) = track(3, "Full", "1\n00:00:01,000 --> 00:00:03,000\nA world of magic.\n\n");
        let probe = media::Probe { subtitles: vec![signs, full], ..Default::default() };
        assert!(lacks_translation(&backend, first.id, &probe));
        keep_translation(&backend, first.id, &probe, &[read_signs, read_full]);
        assert!(!lacks_translation(&backend, first.id, &probe));
        // The player gets them without the rip (which isn't even there) being read.
        let lines: Vec<String> = translation(&backend, &first).unwrap().into_iter().map(|l| l.text).collect();
        assert_eq!(lines, ["A world of magic."]);
        // An episode whose subtitles are still to find gets its pictures then; one ready before is caught up.
        assert!(pick(&backend, "catch up").is_none());
        backend.db.media_ready(first.id, &first.path, 0, 0).unwrap();
        backend.db.set_subs(first.id, "synced", "", "", None, None).unwrap();
        assert_eq!(pick(&backend, "catch up").map(|(_, e, _)| e.number), Some(1));
        let sheet = sheet_path(&backend, show, first.id);
        std::fs::create_dir_all(sheet.parent().unwrap()).unwrap();
        std::fs::write(&sheet, "pictures").unwrap();
        assert!(pick(&backend, "catch up").is_none());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rips_on_one_drive_are_read_one_at_a_time_in_the_order_asked() {
        let jobs = Jobs::default();
        let drive = jobs.drive(Path::new("D:\\Anime\\Show\\01.mkv"));
        assert!(Arc::ptr_eq(&drive, &jobs.drive(Path::new("D:\\Films\\Film.mkv"))));
        assert!(!Arc::ptr_eq(&drive, &jobs.drive(Path::new("C:\\Anime\\Show\\02.mkv"))));
        // Three workers ask for the drive while it's being read, one after another.
        let order = Mutex::new(Vec::new());
        let reading = drive.take();
        std::thread::scope(|scope| {
            for n in 1..=3 {
                let (drive, order) = (&drive, &order);
                scope.spawn(move || {
                    let _turn = drive.take();
                    order.lock().unwrap().push(n);
                });
                while drive.counts.lock().unwrap().0 <= n {
                    std::thread::yield_now();
                }
            }
            assert!(order.lock().unwrap().is_empty());
            drop(reading);
        });
        assert_eq!(*order.lock().unwrap(), [1, 2, 3]);
    }

    #[test]
    fn finds_the_japanese_subtitles_beside_a_rip() {
        let folder = std::env::temp_dir().join(format!("telly-beside-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let japanese = "1\n00:00:01,000 --> 00:00:03,000\nそこは魔法の世界。\n\n2\n00:00:04,000 --> 00:00:06,000\n彼らは魔道士と呼ばれる。\n";
        let english = "1\n00:00:01,000 --> 00:00:03,000\nA world of magic.\n\n2\n00:00:04,000 --> 00:00:06,000\nThey are called wizards.\n";
        for (name, text) in [("Show - 01.mkv", ""), ("Show - 01.srt", english), ("Show - 01.jpn.srt", japanese), ("Show - 01.alass.srt", japanese), ("Show - 010.srt", japanese), ("Show - 01.txt", japanese)] {
            std::fs::write(folder.join(name), text).unwrap();
        }
        let mut rip = ep(1, false, 0.0, 0);
        rip.path = folder.join("Show - 01.mkv").to_string_lossy().into_owned();
        let found: Vec<String> = beside(&rip).into_iter().map(|f| f.name).collect();
        std::fs::remove_dir_all(&folder).unwrap();
        // Not the English file, another episode's, or one that isn't subtitles.
        assert_eq!(found, ["Show - 01.alass.srt", "Show - 01.jpn.srt"]);
    }

    #[test]
    fn subtitles_a_sync_barely_moves_are_in_sync_already() {
        let synced = |score: f64, offset_ms: i64, ratio: f64, shifts: usize, cut: bool| sync::Synced { lines: vec![], score, offset_ms, ratio, shifts, cut, standing: Default::default() };
        assert!(in_sync(&synced(0.9, 120, 1.0, 1, false)));
        assert!(in_sync(&synced(0.9, -480, 1.0, 1, false)));
        // A weak fit will do: where it lands says enough.
        assert!(in_sync(&synced(0.3, -80, 1.0, 1, false)));
        // Moved 3 seconds, sped up for PAL, cut for a broadcast, or fitting no better than another episode's.
        assert!(!in_sync(&synced(0.9, 3000, 1.0, 1, false)));
        assert!(!in_sync(&synced(0.9, 0, 25.0 / 23.976, 1, false)));
        assert!(!in_sync(&synced(0.9, 0, 1.0, 3, true)));
        assert!(!in_sync(&synced(0.1, 0, 1.0, 1, false)));
    }

    #[test]
    fn the_player_s_list_leaves_out_tracks_it_can_t_play() {
        let track = |codec: &str| media::AudioInfo { codec: codec.into(), ..Default::default() };
        let probe = media::Probe { audio: vec![track("dts"), track("ac3"), track("opus")], ..Default::default() };
        // The rip's third track is the second the player lists: it can't play DTS.
        let mut rip = ep(1, false, 0.0, 0);
        rip.audio_index = 2;
        assert_eq!(listed_track(&rip, &probe), 1);
        // Telly's copy holds one track.
        assert_eq!(listed_track(&ep(2, false, 0.0, 0), &probe), 0);
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
    fn the_rip_s_own_track_is_an_exact_fit() {
        let srt: String = (0..40)
            .map(|i| {
                let s = 5 + i * 7;
                format!("{}\n00:{:02}:{:02},000 --> 00:{:02}:{:02},500\nこれは{}番目のせりふです。\n\n", i + 1, s / 60, s % 60, (s + 3) / 60, (s + 3) % 60, i)
            })
            .collect();
        let path = std::env::temp_dir().join(format!("telly-own-track-{}.srt", std::process::id()));
        std::fs::write(&path, &srt).unwrap();
        let lines = subs::parse("track.srt", srt.as_bytes()).unwrap();
        let spans: Vec<(f64, f64)> = lines.iter().filter(|l| !l.sign).map(|l| (l.start, l.end)).collect();
        let timing = Timing { spans: spans.clone(), length: 300.0, against: "the rip’s Japanese subtitles".into(), lead: 0.0, calibration: sync::Calibration::CLEAN };
        let (synced, fit) = sync_file(&path, "In the rip (track 2)", &timing, "Show - 01.mkv").unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((synced.score, synced.offset_ms, fit.made_for), (1.0, 0, true));
        // Another file's timing, even one moved by 2 seconds, isn't the rip's own.
        let moved = Timing { spans: spans.iter().map(|&(s, e)| (s + 2.0, e + 2.0)).collect(), ..timing };
        assert!(!is_the_timing(&lines, &moved));
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
