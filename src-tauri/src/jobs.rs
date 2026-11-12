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
use crate::{anilist, aniskip, heard, matching, openrouter, scan};
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

/// Where an episode stands in the order work goes to episodes (see `pick`),
/// the lowest first: its bucket there, its show's rank, its number.
type Place = (u8, usize, i64);

/// Turns at reading a drive. A turn that ends goes to the episode waiting
/// that comes first in the order work goes to episodes (see `pick`), not to
/// the one that asked first: three workers find subtitles at once, and the
/// next episode's could wait behind the other two's reads. Measured on 5
/// episodes of Cyberpunk: Edgerunners 2 synced to Jimaku's files, read one at
/// a time on a busy laptop: episode 1's rip was read first in 3 runs of 3
/// (its subtitles in 6.0, 7.0 and 23.4 s), where with turns in the order
/// asked it was read second or third in 5 runs of 7 (8.5, 31.2 and 51.6 s).
///
/// A drive without a seek penalty (an SSD) gives `READS_AT_ONCE` turns at once.
struct Turns {
    /// How many rips are read from the drive at once.
    at_once: usize,
    /// Whether the drive has no seek penalty, as Windows says (see `drives`):
    /// not when that can't be told. A rip on it may be read twice (see
    /// `pictures_after`), where one on a hard drive is read once for everything.
    seek_free: bool,
    queue: Mutex<Queue>,
    changed: Condvar,
}

/// How many rips are read at once from a drive without a seek penalty: on an
/// NVMe SSD, Telly's read of 3 rips at once took 1.3 to 1.9 times less time
/// than one after the other, its cores busier. From a hard drive, two at
/// once take longer than one after the other (see `Jobs::drive`).
const READS_AT_ONCE: usize = 3;

/// One rip at a time, as from a hard drive.
impl Default for Turns {
    fn default() -> Self {
        Turns { at_once: 1, seek_free: false, queue: Mutex::default(), changed: Condvar::new() }
    }
}

#[derive(Default)]
struct Queue {
    /// How many rips are being read.
    reading: usize,
    /// The episodes waiting for a turn or about to ask for one (see
    /// `Waiting`), by place, then in the order they came.
    waiting: std::collections::BTreeSet<(Place, u64)>,
    came: u64,
}

/// An episode in the queue for a drive's turns: until it takes its turn, or
/// this is dropped, no episode after it starts reading. A worker joins the
/// queue as it takes an episode, before the few milliseconds' work (ffprobe)
/// that comes before its read, so that a later episode's worker, quicker to
/// that point, doesn't read first.
struct Waiting<'a> {
    turns: &'a Turns,
    key: (Place, u64),
}

/// A turn, which ends when this is dropped.
struct Turn<'a>(&'a Turns);

impl Turns {
    /// Turns at the drive whose root is `root`: a few at once on one without
    /// a seek penalty; one at a time on a hard drive, and when it can't be told.
    fn for_drive(root: &Path) -> Turns {
        match crate::drives::seek_penalty(root) {
            Some(false) => Turns { at_once: READS_AT_ONCE, seek_free: true, ..Turns::default() },
            _ => Turns::default(),
        }
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Joins the queue for a turn, for an episode at `place`.
    fn wait_at(&self, place: Place) -> Waiting<'_> {
        let mut queue = self.queue();
        let key = (place, queue.came);
        queue.came += 1;
        queue.waiting.insert(key);
        Waiting { turns: self, key }
    }

    /// Waits for a turn, for an episode at `place`.
    fn take(&self, place: Place) -> Turn<'_> {
        self.wait_at(place).take()
    }
}

impl<'a> Waiting<'a> {
    /// Waits for the turn.
    fn take(self) -> Turn<'a> {
        let turns = self.turns;
        let mut queue = turns.queue();
        while queue.reading >= turns.at_once || queue.waiting.first() != Some(&self.key) {
            queue = turns.changed.wait(queue).unwrap_or_else(|e| e.into_inner());
        }
        queue.waiting.remove(&self.key);
        queue.reading += 1;
        // The next in line may read too, on a drive giving several turns.
        turns.changed.notify_all();
        Turn(turns)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if self.turns.queue().waiting.remove(&self.key) {
            self.turns.changed.notify_all();
        }
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.queue().reading -= 1;
        self.0.changed.notify_all();
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
    /// The episodes `keep_own_files` has looked beside since Telly started.
    looked_beside: Mutex<HashSet<i64>>,
    /// Episodes given a file from beside their rip, whose words are still to
    /// work out (`words_worker`), first in first.
    words: Mutex<std::collections::VecDeque<i64>>,
    /// The episode `words_worker` is working out the words of, for the title
    /// bar's list (the title bar itself leaves it out).
    pub working_out: Mutex<Option<Task>>,
    /// A download (Whisper's model).
    pub download: Mutex<Option<Task>>,
    /// Stops the running conversion (the show was removed, or Telly is closing).
    cancel_media: AtomicBool,
    /// Jimaku's files per entry, so each episode doesn't ask again.
    jimaku_files: Mutex<HashMap<i64, (Instant, Vec<jimaku::Local>)>>,
    /// Each show's Jimaku entries, by the AniList match they were found for.
    jimaku_entries: Mutex<HashMap<(i64, Option<i64>, String), Vec<i64>>>,
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
            looked_beside: Mutex::new(HashSet::new()),
            words: Mutex::new(std::collections::VecDeque::new()),
            working_out: Mutex::new(None),
            download: Mutex::new(None),
            cancel_media: AtomicBool::new(false),
            jimaku_files: Mutex::new(HashMap::new()),
            jimaku_entries: Mutex::new(HashMap::new()),
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

    /// Waits for a poke, or `timeout`; `seen` is the last poke the caller saw.
    pub fn wait(&self, seen: &mut u64, timeout: Duration) {
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

    /// The episodes whose words are still to work out (`words_worker`), first first.
    pub fn words_waiting(&self) -> Vec<i64> {
        self.words.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect()
    }

    /// Turns at the drive `rip` is on. A worker takes one to read a rip from
    /// end to end: two rips read at once from one hard drive take longer than
    /// one after the other, as the drive spends its time moving between them.
    /// Measured on a USB hard drive, with 8 episodes of four shows: 38 and
    /// 48 MB/s one at a time, 23 MB/s two at a time, 21 and 23 MB/s four at a
    /// time. A drive without a seek penalty gives a few turns at once (see `Turns`).
    fn drive(&self, rip: &Path) -> Arc<Turns> {
        let root = rip.ancestors().last().unwrap_or(rip).to_path_buf();
        let drives = || self.drives.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(turns) = drives().get(&root) {
            return turns.clone();
        }
        // Asked of Windows once a drive, without the other drives waiting.
        let turns = Arc::new(Turns::for_drive(&root));
        drives().entry(root).or_insert(turns).clone()
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
/// its English lines: one of the files isn't there (a rip without English
/// text has an empty list of them, see `keep_translation`), and
/// `catch_up_worker` hasn't looked at the episode yet.
fn behind(backend: &Backend, episode: &Episode) -> bool {
    let looked = backend.jobs.caught_up.lock().unwrap_or_else(|e| e.into_inner()).contains(&episode.id);
    !looked && !(sheet_path(backend, episode.show_id, episode.id).is_file() && translation_path(backend, episode.id).is_file())
}

/// Which of `pick`'s buckets episode `number` is in, when its show's next
/// episode is `next`: 0 the next, 1 the `ahead` after it, 2 the rest after,
/// 3 those before (watched).
fn bucket(number: i64, next: i64, ahead: i64) -> u8 {
    if number == next {
        0
    } else if number > next && number <= next + ahead {
        1
    } else if number > next {
        2
    } else {
        3
    }
}

/// How many episodes after a show's next are got ready ahead of the rest.
fn ahead(backend: &Backend) -> i64 {
    backend.db.setting_i64("convert.ahead", 10).max(1)
}

/// Where `episode` stands now in the order work goes to episodes (see `pick`).
fn place(backend: &Backend, episode: &Episode) -> Place {
    let rank = shows_in_order(backend).iter().position(|s| s.id == episode.show_id).unwrap_or(usize::MAX);
    let next = next_episode(&backend.db.episodes(episode.show_id).unwrap_or_default()).map(|e| e.number).unwrap_or(1);
    (bucket(episode.number, next, ahead(backend)), rank, episode.number)
}

/// The most useful episode waiting for `kind` work: "media", "probe", "subs",
/// "whisper" or "catch up".
fn pick(backend: &Backend, kind: &str) -> Option<(Show, Episode, usize)> {
    let library = library(backend);
    waiting(backend, kind, &library).first().map(|&(show, e, count)| (show.clone(), e.clone(), count))
}

/// Every show with its episodes, in the order work goes to them
/// (`shows_in_order`), read in two queries however many shows there are.
pub fn library(backend: &Backend) -> Vec<(Show, Vec<Episode>)> {
    let mut by_show: HashMap<i64, Vec<Episode>> = HashMap::new();
    for e in backend.db.all_episodes().unwrap_or_default() {
        by_show.entry(e.show_id).or_default().push(e);
    }
    shows_in_order(backend)
        .into_iter()
        .map(|show| {
            let episodes = by_show.remove(&show.id).unwrap_or_default();
            (show, episodes)
        })
        .collect()
}

/// The episodes of `library` waiting for `kind` work (see `pick`), most
/// useful first: the order the workers take them in. With each, its show and
/// how many episodes that has.
pub fn waiting<'a>(backend: &Backend, kind: &str, library: &'a [(Show, Vec<Episode>)]) -> Vec<(&'a Show, &'a Episode, usize)> {
    let ahead = ahead(backend);
    let convert_rest = backend.db.setting_bool("convert.rest", false);
    let idle = backend.idle_for().is_none_or(|d| d >= IDLE_AFTER);
    let mut found: Vec<((u8, usize, i64), &Show, &Episode, usize)> = Vec::new();
    for (rank, (show, episodes)) in library.iter().enumerate() {
        let count = episodes.len();
        let next = next_episode(episodes).map(|e| e.number).unwrap_or(1);
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
            let bucket = bucket(e.number, next, ahead);
            // Only episodes near the next are got ready, but for a copy got ready
            // again for another audio track (follow_audio): it's there already,
            // and until it's done the episode doesn't play.
            if matches!(kind, "media" | "probe")
                && (bucket == 3 || (bucket == 2 && !(idle && convert_rest)))
                && !(kind == "media" && copy_path(backend, show.id, e.id).is_file())
            {
                continue;
            }
            let key = (bucket, rank, e.number);
            found.push((key, show, e, count));
        }
    }
    // A stable sort: of two alike, the one met first comes first.
    found.sort_by_key(|(key, ..)| *key);
    found.into_iter().map(|(_, s, e, n)| (s, e, n)).collect()
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

/// Waits while the player has an episode open from the drive `rip` is on. A
/// worker reading a rip from end to end takes the drive from the player,
/// which then waits for its video; so no such read starts until the player
/// is closed (one already going is left to end).
fn after_the_player(backend: &Backend, rip: &Path) {
    let drive = |path: &Path| path.ancestors().last().map(Path::to_path_buf);
    let playing_there = || {
        let playing = backend.now_playing().and_then(|p| backend.db.episode(p.episode_id).ok().flatten());
        playing.is_some_and(|e| drive(Path::new(&e.play_path)) == drive(rip))
    };
    while playing_there() {
        std::thread::sleep(Duration::from_secs(2));
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
    start("telly-words".into(), |backend, _| words_worker(backend), 0);
    start("telly-openings".into(), |backend, _| openings_worker(backend), 0);
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
                // Ready to play: the workers waiting for that (catching up,
                // asking AniSkip) look now, not at their next look 5 s on.
                Ok(None) => backend.jobs.poke(),
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
    let mut remembered = false;
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        let Some(ffmpeg) = backend.tools().ffmpeg else { continue };
        if !remembered {
            remember_without_english(backend);
            remembered = true;
        }
        while let Some((show, episode, _)) = pick(backend, "catch up") {
            backend.jobs.caught_up.lock().unwrap_or_else(|e| e.into_inner()).insert(episode.id);
            let Some(probe) = stored_probe(backend, episode.id) else { continue };
            let rip = Path::new(&episode.path);
            let sheet = sheet_path(backend, show.id, episode.id);
            let pictures = !sheet.is_file();
            let english = lacks_translation(backend, episode.id, &probe);
            // A rip with no English text is remembered as such, so this
            // episode isn't behind again at the next start.
            keep_translation(backend, episode.id, &probe, &[]);
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
                after_the_player(backend, rip);
                let drive = backend.jobs.drive(rip);
                let _turn = drive.take(place(backend, &episode));
                // What's still missing once it's this episode's turn: the
                // worker that found its subtitles may have made its pictures
                // meanwhile, or be making them (see `pictures_after`).
                let from_rip = from_rip && !sheet.is_file() && !media::sheet_being_made(&sheet);
                let english = lacks_translation(backend, episode.id, &probe);
                if from_rip || english {
                    if let Ok(tracks) = media::read_rip(&ffmpeg, rip, &probe, from_rip.then_some(sheet.as_path())) {
                        keep_translation(backend, episode.id, &probe, &tracks);
                    }
                }
            }
            set_task(slot, None);
        }
    }
}

/// Remembers, in one look over the library, which ready episodes' rips have
/// no English text (see `keep_translation`), for those got ready before that
/// was remembered. Found one at a time by `pick`, each took a look over the
/// whole library: on 2,700 made-up episodes, half of them without, 38 to 69 s
/// of a core at every start.
fn remember_without_english(backend: &Backend) {
    for episode in backend.db.all_episodes().unwrap_or_default() {
        if episode.media_state != "ready" || translation_path(backend, episode.id).is_file() {
            continue;
        }
        if let Some(probe) = stored_probe(backend, episode.id) {
            keep_translation(backend, episode.id, &probe, &[]);
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
    let partial = part_path(&output);
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

/// Brings an episode that's ready to the audio track its show plays now. A
/// rip that plays that track as it is changes at once; a copy of Telly's is
/// got ready again, keeping its picture, wherever the episode is (see `pick`).
pub fn follow_audio(backend: &Backend, ffmpeg: &Path, episode: &Episode, probe: &media::Probe) -> Result<()> {
    let (plan, audio) = plan_for(backend, episode, probe);
    if playing_track(episode, probe) == audio {
        return Ok(());
    }
    if plan == Plan::Direct {
        return change_audio(backend, ffmpeg, episode, probe, audio);
    }
    backend.db.media_requeue(episode.id)
}

/// Where a copy is made before it's put in place: a name of this run of
/// Telly's own. An ffmpeg a closed Telly left going (Windows doesn't end it)
/// then never writes the same file as the one started again, which would
/// leave a broken copy that's taken as ready.
fn part_path(output: &Path) -> PathBuf {
    output.with_extension(format!("{}.part", std::process::id()))
}

/// Copies being made when Telly last closed, before the work starts again.
pub fn clear_parts(backend: &Backend) {
    for show in std::fs::read_dir(backend.media_dir()).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(show.path()).into_iter().flatten().flatten() {
            if file.path().extension().is_some_and(|e| e == "part") {
                let _ = std::fs::remove_file(file.path());
            }
        }
    }
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

/// Moves Telly's copies of episodes, and every episode's seek-bar pictures,
/// from the copies folder `before` into the one Settings just chose, in the
/// background. A file on another drive is copied across, then deleted where
/// it was. Pictures left behind would make every rip be read again.
pub fn move_copies(backend: Arc<Backend>, before: PathBuf) {
    std::thread::spawn(move || {
        for e in backend.db.all_episodes().unwrap_or_default() {
            let from = before.join(e.show_id.to_string()).join(format!("{}.jpg", e.id));
            let to = sheet_path(&backend, e.show_id, e.id);
            if from != to && from.is_file() {
                let _ = std::fs::create_dir_all(to.parent().expect("a folder"));
                if std::fs::rename(&from, &to).is_err() && std::fs::copy(&from, &to).is_ok() {
                    let _ = std::fs::remove_file(&from);
                }
            }
        }
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
    // A rip with no English text has no English lines to keep (see `behind`).
    keep_translation(backend, episode.id, &probe, &[]);
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
    let partial = part_path(&output);
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
/// How long OpenRouter is left after it turns an episode away (429), doubling
/// each time it does again, up to LISTEN_WAIT_MAX.
const LISTEN_WAIT: Duration = Duration::from_secs(60);
const LISTEN_WAIT_MAX: Duration = Duration::from_secs(1800);

fn subs_worker(backend: &Backend, n: usize) {
    let slot = &backend.jobs.subs[n];
    let mut seen = 0;
    let (mut listen_after, mut wait): (Option<Instant>, Duration) = (None, LISTEN_WAIT);
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        let tools = backend.tools();
        if n == 0 {
            keep_own_files(backend);
            hear_again(backend);
        }
        let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else { continue };
        while let Some((show, episode, count)) = take(backend, "subs") {
            set_task(slot, Some(task(&show, &episode, "Finding subtitles for")));
            let result = find_subtitles(backend, slot, &ffmpeg, &ffprobe, &show, &episode, count);
            if let Err(e) = result {
                let _ = backend.db.set_subs_state(episode.id, "failed", &format!("{e:#}"));
            }
            pictures_after(backend, slot, &ffmpeg, &show, &episode);
            set_task(slot, None);
        }
        // Then episodes you asked to have subtitles made for, one at a time: the first worker makes them.
        if n > 0 {
            continue;
        }
        if let Some(listener) = listener(backend, &tools) {
            // OpenRouter turned an episode away (429): the rest goes on meanwhile.
            if listen_after.is_some_and(|at| Instant::now() < at) {
                continue;
            }
            if let Some((show, episode, _)) = pick(backend, "whisper") {
                let name = listener.name();
                if backend.db.claim_whisper(episode.id, &format!("{name} is listening")).unwrap_or(false) {
                    set_task(slot, Some(task(&show, &episode, &format!("{name} is listening to"))));
                    let result = match &listener {
                        Listener::OpenRouter { key, model } => openrouter_subtitles(backend, slot, &ffmpeg, &ffprobe, key, model, &episode),
                        Listener::Local { whisper, model } => whisper_subtitles(backend, &ffmpeg, &ffprobe, whisper, model, &episode),
                    };
                    set_task(slot, None);
                    match result {
                        // Back in the queue as it was, and asked again in a minute, then 2, 4… up to 30.
                        Err(e) if e.downcast_ref::<openrouter::RateLimited>().is_some() => {
                            let _ = backend.db.set_subs_state(episode.id, "whisper", "OpenRouter is busy: asked again soon");
                            listen_after = Some(Instant::now() + wait);
                            wait = (wait * 2).min(LISTEN_WAIT_MAX);
                        }
                        Err(e) => {
                            let _ = backend.db.set_subs_state(episode.id, "failed", &format!("{name}: {e:#}"));
                        }
                        Ok(()) => wait = LISTEN_WAIT,
                    }
                    backend.jobs.poke();
                }
            }
        }
    }
}

/// Makes the seek bar's pictures of an episode whose subtitles were just
/// found from a read that left them out. On a drive without a seek penalty,
/// `find_subtitles` reads the rip's subtitle tracks on their own (a fraction
/// of a second), not in one pass with the pictures, which waits for every
/// keyframe to be decoded (4 s for a 24-minute 1080p HEVC episode); the
/// pictures are made after, from what that read left in the file cache. A
/// hard drive's rips are read once for both: a film is bigger than the cache.
/// An episode whose English lines are still to read is left to
/// `catch_up_worker`, which reads both in one pass.
fn pictures_after(backend: &Backend, slot: &Mutex<Option<Task>>, ffmpeg: &Path, show: &Show, episode: &Episode) {
    let rip = Path::new(&episode.path);
    let drive = backend.jobs.drive(rip);
    let sheet = sheet_path(backend, show.id, episode.id);
    let Some(probe) = stored_probe(backend, episode.id) else { return };
    if !drive.seek_free || sheet.is_file() || plan_for(backend, episode, &probe).0 != Plan::Direct || lacks_translation(backend, episode.id, &probe) {
        return;
    }
    after_the_player(backend, rip);
    let _turn = drive.take(place(backend, episode));
    update_task(slot, Some(&format!("Making the seek bar’s pictures for {} {}", show.title, episode.number)), 0.0);
    let _ = std::fs::create_dir_all(sheet.parent().expect("a folder"));
    let _ = media::make_sheet(ffmpeg, rip, probe.duration, &sheet);
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
    let piece = backend.dirs.subs().join(format!("{}.piece.wav", episode.id));
    let api = openrouter::api_address();
    let mut lines: Vec<Line> = Vec::new();
    // Each piece heard is kept until the episode is done, so one OpenRouter
    // turned away (429) is taken up where it stopped, not paid for again.
    let heard_path = |i: usize, cut: &[f64]| {
        backend.dirs.subs().join(format!("{}.heard.{}.{i}-{:.0}-{:.0}.json", episode.id, crate::util::safe_file_name(model), cut[0] * 1000.0, cut[1] * 1000.0))
    };
    for (i, cut) in cuts.windows(2).enumerate() {
        let kept = heard_path(i, cut);
        let heard: Vec<Line> = match std::fs::read(&kept).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(heard) => heard,
            None => {
                media::write_wav(ffmpeg, source, track, cut[0], cut[1] - cut[0], &piece)?;
                let wav = std::fs::read(&piece);
                let _ = std::fs::remove_file(&piece);
                let wav = wav.context("reading a piece of the audio")?;
                let heard = transcribe_patiently(&api, key, model, &wav)?;
                let _ = crate::util::write_whole(&kept, serde_json::to_vec(&heard)?);
                heard
            }
        };
        lines.extend(heard.into_iter().map(|mut line| {
            line.start += cut[0];
            line.end += cut[0];
            line
        }));
        update_task(slot, None, (i + 1) as f64 / (cuts.len() - 1) as f64);
    }
    for (i, cut) in cuts.windows(2).enumerate() {
        let _ = std::fs::remove_file(heard_path(i, cut));
    }
    let maker = openrouter::maker(model);
    // Repeating itself over music is Whisper's habit.
    let lines = if maker == "Whisper" { without_repeats(lines) } else { lines };
    write_lines(backend, episode.id, &lines)?;
    backend.db.set_subs(episode.id, "synced", &format!("Made by {maker} (machine-made)"), maker, None, None)?;
    crate::premine::prepare(backend, episode.id);
    Ok(())
}

/// A piece transcribed, asked again twice when OpenRouter turns it away for
/// now (429), after what it says to wait (10 to 30 s); turned away a third
/// time, the episode waits its turn (see subs_worker).
fn transcribe_patiently(api: &str, key: &str, model: &str, wav: &[u8]) -> Result<Vec<Line>> {
    let mut tries = 0;
    loop {
        match openrouter::transcribe(api, key, model, wav) {
            Err(e) if tries < 2 && e.downcast_ref::<openrouter::RateLimited>().is_some() => {
                let asked = e.downcast_ref::<openrouter::RateLimited>().and_then(|r| r.0);
                std::thread::sleep(asked.unwrap_or(Duration::from_secs(10)).clamp(Duration::from_secs(10), Duration::from_secs(30)));
                tries += 1;
            }
            heard => return heard,
        }
    }
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
    /// A file of yours beside the rip: used as it is, never synced, so its score says nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub beside: bool,
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
    crate::util::write_whole(&lines_path(backend, episode_id), serde_json::to_vec(lines)?)?;
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
    crate::util::write_whole(&translation_path(backend, episode_id), serde_json::to_vec(&lines)?)?;
    Ok(lines)
}

/// Keeps a rip's English lines from its tracks as `media::read_rip` just read
/// them, when they aren't kept yet: the player then never reads the rip for
/// them. A rip with no English text track, or whose track gave no lines, is
/// kept as having none (an empty list, which the player shows as none), so
/// the episode isn't behind (see `behind`) and its rip isn't read for them
/// again. Given no tracks (nothing read), it keeps only that a rip has no
/// English text track.
fn keep_translation(backend: &Backend, episode_id: i64, probe: &media::Probe, tracks: &[media::TrackTiming]) {
    if translation_path(backend, episode_id).is_file() {
        return;
    }
    let read = match media::english_text_track(probe) {
        Some(english) => tracks.iter().find(|t| t.index == english.index).map(|t| (t.text.as_str(), media::text_format(&t.codec))),
        None => Some(("", "srt")),
    };
    if let Some((text, format)) = read {
        let _ = keep_lines(backend, episode_id, text, format);
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
/// The bool says whether it's FireRedVAD's (clean) view. `heard`: that track
/// as `media::read_rip_and` heard it already, at `sync::FIRERED_RATE`.
fn speech_of(ffmpeg: &Path, probe: &media::Probe, episode: &Episode, heard: Option<&[i16]>) -> Result<(Vec<(f64, f64)>, f64, bool)> {
    let source = Path::new(&episode.path);
    let audio = media::japanese_audio(probe);
    let read;
    let samples = match heard {
        Some(samples) => samples,
        None => {
            read = media::read_audio_filtered(ffmpeg, source, audio, sync::FIRERED_RATE, None, None, None)?;
            &read[..]
        }
    };
    let length = samples.len() as f64 / sync::FIRERED_RATE as f64;
    match sync::firered_spans(samples) {
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
        None => speech_timing(ffmpeg, probe, episode, None),
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
fn speech_timing(ffmpeg: &Path, probe: &media::Probe, episode: &Episode, heard: Option<&[i16]>) -> Result<Timing> {
    let (spans, length, clean) = speech_of(ffmpeg, probe, episode, heard)?;
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

/// Jimaku's files for a show: those of its first 3 entries (Jimaku can have a
/// show under more than one: a TV and a BD release, a season of its own), for
/// every episode, not only the first. The entries are asked for once while
/// Telly is open, and again after Change match.
fn jimaku_files(backend: &Backend, key: &str, show: &Show) -> Result<Vec<jimaku::Local>> {
    let match_key = (show.id, show.anilist_id, show.title_romaji.clone());
    let known = backend.jobs.jimaku_entries.lock().ok().and_then(|e| e.get(&match_key).cloned());
    let entry_ids: Vec<i64> = match known {
        Some(ids) => ids,
        None => match jimaku::find_entries(key, show.anilist_id, &show.title_romaji) {
            Ok(entries) => {
                if let Some(first) = entries.first().filter(|f| show.jimaku_id != Some(f.id)) {
                    backend.db.set_jimaku_id(show.id, Some(first.id))?;
                }
                let ids: Vec<i64> = entries.into_iter().take(3).map(|e| e.id).collect();
                if let Ok(mut known) = backend.jobs.jimaku_entries.lock() {
                    known.insert(match_key, ids.clone());
                }
                ids
            }
            // Jimaku can't be asked: the entry found before, if any.
            Err(e) => match show.jimaku_id {
                Some(id) => vec![id],
                None => return Err(e),
            },
        },
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
        Some(id) if id != crate::anilist_list::NO_ENTRY_BEFORE => id,
        _ => {
            let title = crate::anilist_list::group_title(show, group);
            let mut found = anilist::search_loosely(&title).unwrap_or_default();
            anilist::rank(&mut found, &title, in_group);
            let _ = anilist::follow_sequels(&mut found, &title);
            let id = match found.first() {
                Some(a) if Some(a.id) != show.anilist_id && a.score >= 0.5 => {
                    jimaku::find_entries(key, Some(a.id), &a.romaji)?.first().map(|e| e.id).unwrap_or(crate::anilist_list::NO_ENTRY)
                }
                _ => crate::anilist_list::NO_ENTRY,
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

/// Where a subtitle file beside a rip stands among the others there, from
/// what its name adds to the rip's (".alass.srt"): one you re-timed with alass
/// first, then one marked as Japanese (".ja.srt", ".jpn.srt"), then one named
/// just as the rip (".srt"), and a machine-made one (".ai.srt") last. Of two
/// alike, the shorter name: ".ja.srt" before ".jp[sdh].ja.srt".
fn beside_rank(added: &str) -> (u8, usize) {
    let added = added.to_lowercase();
    let kind = if added.contains(".alass.") {
        0
    } else if added.contains(".ai.") {
        3
    } else if added.matches('.').count() == 1 {
        2
    } else {
        1
    };
    (kind, added.len())
}

/// Japanese subtitle files beside the rip and named after it: "Episode 01.srt",
/// "Episode 01.ja.srt", "Episode 01.alass.srt". The one to use comes first
/// (see `beside_rank`).
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
    out.sort_by(|a, b| beside_rank(&a.name[stem.len()..]).cmp(&beside_rank(&b.name[stem.len()..])).then(a.name.cmp(&b.name)));
    out
}

/// In an episode's details when its subtitles are a file of yours, untouched.
const AS_IT_IS: &str = "as it is";
/// In an episode's details when you picked its subtitle file yourself.
const BY_HAND: &str = "chosen by hand";

/// Uses a Japanese subtitle file beside the rip as it is: the first of `own`
/// (see `beside`). Nothing of the rip is read, and no sync moves the file: you
/// put it there. The others stay on the episode's page to switch to.
fn keep_beside(backend: &Backend, episode: &Episode, own: &[jimaku::Local]) -> Result<()> {
    keep_beside_file(backend, episode, &own[0], own)
}

/// Uses `file`, one of the Japanese subtitle files beside the rip (`own`), as it is.
fn keep_beside_file(backend: &Backend, episode: &Episode, file: &jimaku::Local, own: &[jimaku::Local]) -> Result<()> {
    write_lines(backend, episode.id, &japanese_lines(&file.path, &file.name)?)?;
    let tried: Vec<Tried> = own.iter().map(|f| Tried { name: f.name.clone(), path: f.path.to_string_lossy().into_owned(), score: 0.0, offset_ms: 0, error: String::new(), beside: true }).collect();
    crate::util::write_whole(&tried_path(backend, episode.id), serde_json::to_vec(&tried)?)?;
    backend.db.set_subs(episode.id, "synced", &format!("beside the rip · {AS_IT_IS}"), &file.name, None, Some(0))?;
    backend.jobs.words.lock().unwrap_or_else(|e| e.into_inner()).push_back(episode.id);
    backend.jobs.poke();
    Ok(())
}

/// Works out the words of episodes given a file from beside their rip
/// (`premine::prepare`). That asks Hachidori about every word, a second or so
/// an episode, and thousands of episodes get their file in one pass: so the
/// pass doesn't wait for it, and this does it after.
fn words_worker(backend: &Backend) {
    let mut seen = 0;
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        loop {
            let next = backend.jobs.words.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
            let Some(episode_id) = next else { break };
            let episode = backend.db.episode(episode_id).ok().flatten();
            let show = episode.as_ref().and_then(|e| backend.db.show(e.show_id).ok().flatten());
            set_task(&backend.jobs.working_out, show.zip(episode).map(|(s, e)| task(&s, &e, "Working out the words of")));
            crate::premine::prepare(backend, episode_id);
            set_task(&backend.jobs.working_out, None);
        }
    }
}

/// How long AniSkip's answer of none for an episode stands before it's asked
/// again: people mark an episode's opening in the days after it airs.
const OPENING_AGAIN_MS: i64 = 7 * 24 * 3_600_000;
/// How long to wait after AniList or AniSkip couldn't be reached.
const OPENING_RETRY: Duration = Duration::from_secs(600);

/// Asks AniSkip where the opening is in episodes whose rips' chapters don't
/// say, so Skip opening shows for them too. Its answer is kept, none
/// included. When AniList or AniSkip can't be reached, it tries again 10
/// minutes later.
fn openings_worker(backend: &Backend) {
    let mut asking = Openings::default();
    let mut seen = 0;
    let mut retry_at: Option<Instant> = None;
    loop {
        backend.jobs.wait(&mut seen, Duration::from_secs(5));
        if retry_at.is_some_and(|at| Instant::now() < at) {
            continue;
        }
        retry_at = match asking.ask(backend) {
            Ok(()) => None,
            Err(e) => {
                eprintln!("openings: {e:#}; trying again in 10 minutes");
                Some(Instant::now() + OPENING_RETRY)
            }
        };
    }
}

#[derive(Default)]
struct Openings {
    /// Episodes AniSkip isn't asked about this run: their chapters show the
    /// opening, or no AniList entry with a MyAnimeList ID is sure to be theirs.
    passed: HashSet<i64>,
    /// MyAnimeList's ID for each AniList ID, which AniSkip knows anime by (None: AniList has none).
    mal: HashMap<i64, Option<i64>>,
}

impl Openings {
    /// Asks AniSkip about each episode that's due, show by show; stops at the first that fails.
    fn ask(&mut self, backend: &Backend) -> Result<()> {
        let again = crate::util::now_ms() - OPENING_AGAIN_MS;
        for show in shows_in_order(backend) {
            if show.anilist_id.is_none() {
                continue;
            }
            let due: Vec<i64> = backend.db.unasked_openings(show.id, again)?.into_iter().filter(|id| !self.passed.contains(id)).collect();
            if due.is_empty() {
                continue;
            }
            let episodes = backend.db.episodes(show.id)?;
            for id in due {
                let Some(episode) = episodes.iter().find(|e| e.id == id) else { continue };
                let probe = stored_probe(backend, id).unwrap_or_default();
                // The chapters say where both are: nothing to ask. One without the other: AniSkip has it.
                if probe.duration <= 0.0 || (media::opening(&probe).is_some() && media::ending(&probe).is_some()) {
                    self.passed.insert(id);
                    continue;
                }
                // AniSkip numbers each AniList entry's episodes from 1: a
                // later season folder is an entry of its own (as on your AniList list).
                let place = crate::anilist_list::place(&show, &episodes, episode, |group, count| crate::anilist_list::group_entry(backend, &show, group, count))?;
                let Some((entry, number)) = place else {
                    self.passed.insert(id);
                    continue;
                };
                let mal_id = match self.mal.get(&entry) {
                    Some(known) => *known,
                    None => *self.mal.entry(entry).or_insert(aniskip::mal_id(entry)?),
                };
                let Some(mal_id) = mal_id else {
                    self.passed.insert(id);
                    continue;
                };
                // Kept only if Change match or Fix order hasn't come between.
                let own = show.anilist_id.unwrap_or_default();
                backend.db.set_opening_if(id, episode.number, own, aniskip::skips(mal_id, number, probe.duration)?)?;
                std::thread::sleep(aniskip::PACE);
            }
        }
        Ok(())
    }
}

/// Gives every episode with a Japanese subtitle file beside its rip that file,
/// all shows through, before any rip is read: thousands of episodes in the
/// time one rip takes to read. Once, it also goes back over episodes Telly
/// settled another way although such a file was there (a sync had moved the
/// file, swapped it for one of Jimaku's, or found it didn't fit), but not
/// those whose file you picked.
fn keep_own_files(backend: &Backend) {
    let again = backend.db.setting_i64("subs.own", 0) < 1;
    for show in shows_in_order(backend) {
        for episode in backend.db.episodes(show.id).unwrap_or_default() {
            let waiting = episode.subs_state == "pending";
            let settled_otherwise = again
                && matches!(episode.subs_state.as_str(), "synced" | "check" | "none")
                && !episode.subs_detail.contains(AS_IT_IS)
                && !episode.subs_detail.contains(BY_HAND);
            if !(waiting || settled_otherwise) || !backend.jobs.looked_beside.lock().unwrap_or_else(|e| e.into_inner()).insert(episode.id) {
                continue;
            }
            let own = beside(&episode);
            if own.is_empty() || (waiting && !backend.db.claim_subs(episode.id).unwrap_or(false)) {
                continue;
            }
            if let Err(e) = keep_beside(backend, &episode, &own) {
                let _ = backend.db.set_subs_state(episode.id, "failed", &format!("{e:#}"));
            }
        }
    }
    if again {
        let _ = backend.db.set_setting("subs.own", &serde_json::json!(1));
    }
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
                tried.push(Tried { name: file.name.clone(), path, score: synced.score, offset_ms: synced.offset_ms, error: String::new(), beside: false });
                if in_sync(&synced) && best.as_ref().is_none_or(|(_, _, b)| matching::better(&fit, b)) {
                    best = Some((file, synced, fit));
                }
            }
            Err(e) => tried.push(Tried { name: file.name.clone(), path, score: 0.0, offset_ms: 0, error: format!("{e:#}"), beside: false }),
        }
    }
    let Some((file, synced, _)) = best else { return Ok(false) };
    write_lines(backend, episode.id, &japanese_lines(&file.path, &file.name)?)?;
    crate::util::write_whole(&tried_path(backend, episode.id), serde_json::to_vec(&tried)?)?;
    let detail = format!("in sync {AS_IT_IS} · matched to {}", timing.against);
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
            tried.push(Tried { name: file.name.clone(), path: file.path.to_string_lossy().into_owned(), score: synced.score, offset_ms: synced.offset_ms, error: String::new(), beside: false });
            synced_files.push((file.clone(), synced.ratio));
            if best.as_ref().is_none_or(|b| matching::better(&fit, &b.fit)) {
                *best = Some(Pick { synced, fit, name: file.name.clone(), filed_as: file.episode });
            }
        }
        Err(e) => tried.push(Tried { name: file.name.clone(), path: file.path.to_string_lossy().into_owned(), score: 0.0, offset_ms: 0, error: format!("{e:#}"), beside: false }),
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

/// Once OpenRouter has a key, looks once more for subtitles for the episodes
/// Telly found none for or wasn't sure of, now that it can listen to them.
fn hear_again(backend: &Backend) {
    if backend.db.setting_i64("subs.heard", 0) >= 1 || openrouter::chosen(backend).is_none() {
        return;
    }
    for show in shows_in_order(backend) {
        for episode in backend.db.episodes(show.id).unwrap_or_default() {
            if matches!(episode.subs_state.as_str(), "none" | "check") && !episode.subs_detail.contains(BY_HAND) {
                let _ = backend.db.set_subs_state(episode.id, "pending", "");
            }
        }
    }
    let _ = backend.db.set_setting("subs.heard", &serde_json::json!(1));
}

/// Settles an episode no subtitle file fits: OpenRouter makes its subtitles
/// when Settings has a key for it; else it has none, and `detail` says why.
fn no_subtitles(backend: &Backend, episode_id: i64, detail: &str, score: Option<f64>) -> Result<()> {
    forget_lines(backend, episode_id);
    let (state, detail) = if openrouter::chosen(backend).is_some() { ("whisper", "") } else { ("none", detail) };
    backend.db.set_subs(episode_id, state, detail, "", score, None)?;
    Ok(())
}

/// An episode's lines go when finding its subtitles again finds none: they
/// were another file's, maybe another show's (Change match), and the player,
/// premining and line search would go on using them. Its words, its lines in
/// line search and its share of known words go with them.
fn forget_lines(backend: &Backend, episode_id: i64) {
    let _ = std::fs::remove_file(lines_path(backend, episode_id));
    let _ = std::fs::remove_file(crate::premine::words_path(backend, episode_id));
    let _ = backend.lines.forget(episode_id);
    let _ = backend.db.forget_word_score(episode_id);
}

/// What a model on OpenRouter hears in a few stretches of an episode (see
/// heard.rs), timed to the episode. Each stretch is a file in the subtitles
/// folder for a moment.
fn listen(backend: &Backend, ffmpeg: &Path, probe: &media::Probe, key: &str, model: &str, episode: &Episode) -> Result<Vec<Vec<Line>>> {
    let source = Path::new(&episode.path);
    let track = media::japanese_audio(probe);
    std::fs::create_dir_all(backend.dirs.subs())?;
    let piece = backend.dirs.subs().join(format!("{}.stretch.wav", episode.id));
    let api = openrouter::api_address();
    heard::stretches(probe.duration)
        .into_iter()
        .map(|(start, length)| {
            media::write_wav(ffmpeg, source, track, start, length, &piece)?;
            let wav = std::fs::read(&piece);
            let _ = std::fs::remove_file(&piece);
            let mut lines = openrouter::transcribe(&api, key, model, &wav.context("reading a piece of the audio")?)?;
            for line in &mut lines {
                line.start += start;
                line.end += start;
            }
            Ok(lines)
        })
        .collect()
}

/// The file among those tried whose words are the ones heard, moved to where
/// they're said: its lines, how surely, and its name.
fn place_by_words(tried: &[Tried], heard: &[Vec<Line>]) -> Option<(Vec<Line>, heard::Placed, String)> {
    let (mut lines, placed, name) = tried
        .iter()
        .filter_map(|t| {
            let lines = japanese_lines(Path::new(&t.path), &t.name).ok()?;
            let placed = heard::place(&lines, heard)?;
            Some((lines, placed, t.name.clone()))
        })
        .max_by_key(|(_, placed, _)| placed.agreeing)?;
    for line in &mut lines {
        line.start = (line.start + placed.shift).max(0.0);
        line.end = (line.end + placed.shift).max(line.start + 0.05);
    }
    Some((lines, placed, name))
}

fn find_subtitles(backend: &Backend, slot: &Mutex<Option<Task>>, ffmpeg: &Path, ffprobe: &Path, show: &Show, episode: &Episode, count: usize) -> Result<()> {
    let own = beside(episode);
    if !own.is_empty() {
        return keep_beside(backend, episode, &own);
    }
    let key = backend.db.setting_str("jimaku.key");
    let rip = Path::new(&episode.path);
    // One rip is read through at a time on each drive, the episodes that come
    // first first (see `Jobs::drive`): this one is queued from now until it
    // reads, or knows it won't before asking Jimaku.
    let drive = backend.jobs.drive(rip);
    let place = place(backend, episode);
    let queued = std::cell::Cell::new(Some(drive.wait_at(place)));
    // What ffprobe found when the rip was got ready; else it's asked now.
    let probe = match stored_probe(backend, episode.id) {
        Some(probe) => probe,
        None => media::probe(ffprobe, rip)?,
    };
    let reading = || {
        after_the_player(backend, rip);
        let turn = match queued.take() {
            Some(waiting) => waiting.take(),
            None => drive.take(place),
        };
        update_task(slot, Some(&format!("Reading the timing of {} {}", show.title, episode.number)), 0.0);
        turn
    };
    // The seek bar's pictures are made in the same read when the rip itself
    // is what plays, and its English lines kept: the player then needn't read
    // the rip when the episode first plays. On a drive without a seek
    // penalty, the pictures are made after (see `pictures_after`).
    let sheet = Some(sheet_path(backend, show.id, episode.id)).filter(|sheet| !drive.seek_free && !sheet.is_file() && plan_for(backend, episode, &probe).0 == Plan::Direct);
    // A rip without subtitle tracks is matched to its speech, when there are
    // files to match (there's a Jimaku key): heard in the same read.
    let speech = (!key.trim().is_empty() && !media::timed_subtitles(&probe) && !probe.audio.is_empty()).then(|| (media::japanese_audio(&probe), sync::FIRERED_RATE));
    // The rip's own subtitle tracks (and its speech), read in one pass over it the first time they're wanted.
    let read = std::cell::OnceCell::new();
    let rip_read = || {
        read.get_or_init(|| {
            let _turn = reading();
            if let Some(folder) = sheet.as_deref().and_then(Path::parent) {
                let _ = std::fs::create_dir_all(folder);
            }
            let (tracks, heard) = media::read_rip_and(ffmpeg, rip, &probe, sheet.as_deref(), speech).unwrap_or_default();
            keep_translation(backend, episode.id, &probe, &tracks);
            (tracks, heard)
        })
    };
    let rip_tracks = || &rip_read().0;
    if sheet.is_some() || lacks_translation(backend, episode.id, &probe) {
        rip_tracks();
    }
    let mut files = if probe.subtitles.iter().any(|s| japanese_text(&s.language, &s.codec)) { embedded(backend, rip_tracks(), episode) } else { Vec::new() };
    drop(queued.take());
    let video = rip.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let read_timing = || match subtitle_timing(&probe, rip_tracks()) {
        Some(timing) => Ok(timing),
        None => {
            let heard = rip_read().1.as_deref();
            let _turn = heard.is_none().then(reading);
            speech_timing(ffmpeg, &probe, episode, heard).context("reading the episode’s timing")
        }
    };
    // A Japanese track inside the rip is kept as it is when it's in sync
    // already, and Jimaku isn't asked.
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
        if state == "none" {
            return no_subtitles(backend, episode.id, detail, None);
        }
        forget_lines(backend, episode.id);
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
    crate::util::write_whole(&tried_path(backend, episode.id), serde_json::to_vec(&tried)?)?;
    // A doubtful fit, or none: what OpenRouter hears in a few stretches says
    // whose words these are, and where they're said. If it can't be asked, or
    // what it hears places no file, the timing's own answer stands.
    if best.as_ref().is_none_or(|b| b.synced.score < GOOD_SYNC) {
        if let Some((key, model)) = openrouter::chosen(backend) {
            let maker = openrouter::maker(&model);
            let heard = {
                let _turn = reading();
                update_task(slot, Some(&format!("{maker} is listening to {} {}", show.title, episode.number)), 0.0);
                listen(backend, ffmpeg, &probe, &key, &model, episode)
            };
            if let Some((lines, placed, name)) = heard.ok().and_then(|heard| place_by_words(&tried, &heard)) {
                write_lines(backend, episode.id, &lines)?;
                let detail = format!("shifted {:+.1} s · matched to what {maker} heard", placed.shift);
                let score = (placed.agreeing as f64 / placed.heard as f64).max(GOOD_SYNC);
                backend.db.set_subs(episode.id, "synced", &detail, &name, Some(score), Some((placed.shift * 1000.0).round() as i64))?;
                crate::premine::prepare(backend, episode.id);
                return Ok(());
            }
        }
    }
    let Some(Pick { synced, name, filed_as, .. }) = best else {
        return no_subtitles(backend, episode.id, "None of Jimaku’s files for this episode are Japanese subtitles Telly can read", None);
    };
    if synced.score < FITS_NOT {
        // Every file is another episode's, or another cut's: none is better than wrong ones.
        return no_subtitles(backend, episode.id, &format!("None of Jimaku’s {} files fit this episode", tried.len()), Some(synced.score));
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
/// One of the Japanese files beside the rip is used as it is, as the first
/// of them was: nothing of the rip is read, and no sync moves it.
pub fn use_file(backend: &Backend, episode: &Episode, path: &Path, name: &str) -> Result<()> {
    let own = beside(episode);
    if let Some(file) = own.iter().find(|f| f.path == path) {
        return keep_beside_file(backend, episode, file, &own);
    }
    // Read before the rip is, which can take minutes: a file that can't be read says so at once.
    let lines = subs::parse(name, &std::fs::read(path)?)?;
    let tools = backend.tools();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg, tools.ffprobe) else {
        return Err(anyhow!("Telly needs ffmpeg for this. Choose it in Settings."));
    };
    let probe = media::probe(&ffprobe, Path::new(&episode.path))?;
    let timing = timing_of(&ffmpeg, &probe, episode)?;
    let mut synced = sync_to(&lines, &timing);
    if synced.score < GOOD_SYNC {
        if let Some(cut) = sync_to_in_pieces(&lines, synced.ratio, &timing) {
            synced = cut;
        }
    }
    write_lines(backend, episode.id, &synced.lines)?;
    let state = if synced.score >= GOOD_SYNC { "synced" } else { "check" };
    let detail = format!("{} · matched to {} · {BY_HAND}", describe(&synced), timing.against);
    backend.db.set_subs(episode.id, state, &detail, name, Some(synced.score), Some(synced.offset_ms))?;
    crate::premine::prepare(backend, episode.id);
    Ok(())
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
    fn another_audio_track_leaves_every_later_episode_playable() {
        let dir = std::env::temp_dir().join(format!("telly-audio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = std::sync::Arc::new(Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        backend.db.set_setting("tools.ffmpeg", &serde_json::json!("ffmpeg")).unwrap();
        backend.refresh_tools();
        let rips = dir.join("rips");
        std::fs::create_dir_all(&rips).unwrap();
        let episodes = (1..=24)
            .map(|number| crate::db::NewEpisode { number, path: rips.join(format!("Show - {number:02}.mkv")).to_string_lossy().into_owned(), size: 1, duration: 1440.0 })
            .collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: rips.to_string_lossy().into_owned(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        // Rips that play as they are (H.264 in MKV, AAC), with a Japanese and an English track.
        let probe = serde_json::json!({
            "format": "matroska,webm", "duration": 1440.0,
            "video": { "codec": "h264", "profile": "High", "pixFmt": "yuv420p", "width": 1920, "height": 1080, "sar": 1.0, "fieldOrder": "progressive", "fps": 23.976 },
            "audio": [
                { "index": 1, "codec": "aac", "channels": 2, "language": "jpn", "title": "", "default": true },
                { "index": 2, "codec": "aac", "channels": 2, "language": "eng", "title": "", "default": false }
            ],
            "subtitles": [], "chapters": []
        });
        let eps = backend.db.episodes(show).unwrap();
        for e in &eps {
            std::fs::write(&e.path, b"x").unwrap();
            backend.db.set_probe(e.id, &probe.to_string(), 1440.0, None, "direct").unwrap();
            backend.db.media_ready(e.id, &e.path, 0, 0).unwrap();
        }
        if backend.tools().ffmpeg.is_none() {
            return; // Picking a track needs ffmpeg, and this computer has none.
        }
        // Watching episode 3, the English track is picked: every later episode plays it at once.
        backend.call("setAudio", serde_json::json!({ "id": eps[2].id, "track": 1 })).unwrap();
        let after = backend.db.episodes(show).unwrap();
        assert!(after.iter().all(|e| e.media_state == "ready"), "{:?}", after.iter().map(|e| (e.number, e.media_state.clone())).collect::<Vec<_>>());
        assert!(after.iter().filter(|e| e.number >= 3).all(|e| e.audio_track == Some(1)));

        // A copy of Telly's got ready again for another track is done however far ahead it is.
        let far = &after[19];
        let copy = copy_path(&backend, show, far.id);
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::write(&copy, b"x").unwrap();
        backend.db.media_requeue(far.id).unwrap();
        assert_eq!(take(&backend, "media").map(|(_, e, _)| e.number), Some(20));
        // One that never had a copy still waits until it's near.
        backend.db.media_reset(after[21].id).unwrap();
        assert_eq!(take(&backend, "media").map(|(_, e, _)| e.number), None);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_episode_left_without_subtitles_doesn_t_keep_its_old_lines() {
        let dir = std::env::temp_dir().join(format!("telly-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = vec![crate::db::NewEpisode { number: 1, path: "1.mkv".into(), size: 1, duration: 1400.0 }];
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let id = backend.db.episodes(show).unwrap()[0].id;
        // Lines from before Change match, with their words and share.
        write_lines(&backend, id, &[Line { start: 1.0, end: 2.0, text: "前の番組".into(), ruby: vec![], speaker: String::new(), sign: false }]).unwrap();
        std::fs::write(crate::premine::words_path(&backend, id), b"{}").unwrap();
        backend.db.set_word_score(id, 10, 5, "x").unwrap();
        no_subtitles(&backend, id, "Jimaku has nothing for this episode", None).unwrap();
        assert!(read_lines(&backend, id).is_empty());
        assert!(!crate::premine::words_path(&backend, id).exists());
        assert!(!backend.db.word_scores().unwrap().contains_key(&id));
        assert_eq!(backend.db.episode(id).unwrap().unwrap().subs_state, "none");
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn finding_subtitles_reads_what_s_in_the_rip_from_when_it_was_got_ready() {
        let dir = std::env::temp_dir().join(format!("telly-probe-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = (1..=2).map(|number| crate::db::NewEpisode { number, path: format!("{number}.mkv"), size: 1, duration: 1400.0 }).collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let show = backend.db.show(show).unwrap().unwrap();
        let eps = backend.db.episodes(show.id).unwrap();
        // Episode 1 was got ready: what ffprobe found then is kept. Episode 2 wasn't.
        let probe = media::Probe { format: "matroska,webm".into(), duration: 1400.0, ..Default::default() };
        backend.db.set_probe(eps[0].id, &serde_json::to_string(&probe).unwrap(), 1400.0, None, "video").unwrap();
        // No ffprobe (or ffmpeg) here: only episode 1 gets as far as wanting a Jimaku key.
        let (nothing, slot) = (Path::new("no-such-tool"), Mutex::new(None));
        find_subtitles(&backend, &slot, nothing, nothing, &show, &eps[0], 2).unwrap();
        assert_eq!(backend.db.episode(eps[0].id).unwrap().unwrap().subs_state, "nokey");
        assert!(find_subtitles(&backend, &slot, nothing, nothing, &show, &eps[1], 2).is_err());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
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
    fn what_s_waiting_is_listed_in_the_order_the_workers_take_it() {
        let dir = std::env::temp_dir().join(format!("telly-waiting-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = std::sync::Arc::new(Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        let add = |title: &str, count: i64| {
            let episodes = (1..=count).map(|number| crate::db::NewEpisode { number, path: format!("{title} {number}.mkv"), size: 1, duration: 1400.0 }).collect();
            backend.db.add_show(&crate::db::NewShow { folder: title.into(), title: title.into(), episodes, ..Default::default() }).unwrap()
        };
        let (fairy_tail, frieren) = (add("Fairy Tail", 30), add("Frieren", 15));
        // Frieren was watched last, up to episode 5: its episode 6 is next, the 10 after it near, the 5 before it last.
        backend.db.touch_show(frieren).unwrap();
        for e in backend.db.episodes(frieren).unwrap().iter().take(5) {
            backend.db.set_watched(e.id, true).unwrap();
        }
        // Videos are got ready for every episode, not just those near the next.
        backend.db.set_setting("convert.rest", &serde_json::json!(true)).unwrap();
        let listed = |kind: &str| -> Vec<(String, i64)> {
            let library = library(&backend);
            waiting(&backend, kind, &library).into_iter().map(|(s, e, _)| (s.title.clone(), e.number)).collect()
        };
        let named = |taken: Option<(Show, Episode, usize)>| taken.map(|(s, e, _)| (s.title, e.number));

        let subs = listed("subs");
        assert_eq!(subs.len(), 45);
        assert_eq!(subs[..3], [("Frieren".to_string(), 6), ("Fairy Tail".to_string(), 1), ("Frieren".to_string(), 7)]);
        assert_eq!(subs.last(), Some(&("Frieren".to_string(), 5)));
        // The title bar's list says as much: the count, and the first 10 in that order.
        let queue = backend.call("queue", serde_json::json!({})).unwrap();
        let group = queue["waiting"].as_array().unwrap().iter().find(|w| w["kind"] == "subs").unwrap().clone();
        assert_eq!(group["count"], 45);
        let first: Vec<(String, i64)> = group["first"].as_array().unwrap().iter().map(|q| (q["title"].as_str().unwrap().to_string(), q["number"].as_i64().unwrap())).collect();
        assert_eq!(first, subs[..10]);
        assert_eq!(queue["waiting"][0]["first"][0]["step"], "To read");

        // Taking work one episode after another goes down the list, kind by kind.
        assert_eq!(std::iter::from_fn(|| named(take(&backend, "subs"))).collect::<Vec<_>>(), subs);
        let media = listed("media");
        assert_eq!(media.len(), 40, "all but the episodes watched");
        assert_eq!(std::iter::from_fn(|| named(take(&backend, "media"))).collect::<Vec<_>>(), media);
        let all: Vec<Episode> = [fairy_tail, frieren].iter().flat_map(|&id| backend.db.episodes(id).unwrap()).collect();
        for e in &all {
            backend.db.media_reset(e.id).unwrap();
        }
        let probe = listed("probe");
        assert_eq!(probe, media);
        assert_eq!(std::iter::from_fn(|| named(take(&backend, "probe"))).collect::<Vec<_>>(), probe);
        // Episodes got ready before the seek bar's pictures were made with them are caught up in the same order.
        for e in &all {
            backend.db.media_ready(e.id, &e.path, 0, 0).unwrap();
            backend.db.set_subs(e.id, "synced", "", "", None, None).unwrap();
        }
        let catch_up = listed("catch up");
        assert_eq!(catch_up.len(), 45);
        let caught_up = std::iter::from_fn(|| {
            let (show, e, _) = pick(&backend, "catch up")?;
            backend.jobs.caught_up.lock().unwrap().insert(e.id);
            Some((show.title, e.number))
        });
        assert_eq!(caught_up.collect::<Vec<_>>(), catch_up);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_japanese_file_beside_the_rip_is_used_as_it_is() {
        let dir = std::env::temp_dir().join(format!("telly-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rips = dir.join("rips");
        std::fs::create_dir_all(&rips).unwrap();
        let japanese = "1\n00:00:01,000 --> 00:00:03,000\nそこは魔法の世界。\n\n2\n00:00:04,000 --> 00:00:06,000\n彼らは魔道士と呼ばれる。\n";
        // Episode 1 has a file of its own, episode 2 had one but Telly kept nothing, episode 3 was picked by hand, episode 4 has none.
        for name in ["Show - 01.ja.srt", "Show - 02.ja.srt", "Show - 03.ja.srt"] {
            std::fs::write(rips.join(name), japanese).unwrap();
        }
        // The other file of episode 1 is timed a little later.
        std::fs::write(rips.join("Show - 01.srt"), japanese.replace("00:00:01,000", "00:00:01,500").replace("00:00:04,000", "00:00:04,500")).unwrap();
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = (1..=4).map(|number| crate::db::NewEpisode { number, path: rips.join(format!("Show - 0{number}.mkv")).to_string_lossy().into_owned(), size: 1, duration: 1400.0 }).collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: rips.to_string_lossy().into_owned(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let ids: Vec<i64> = backend.db.episodes(show).unwrap().iter().map(|e| e.id).collect();
        backend.db.set_subs(ids[1], "none", "None of Jimaku’s 3 files fit this episode", "", Some(0.0), None).unwrap();
        backend.db.set_subs(ids[2], "synced", &format!("shifted +1.0 s · matched to the speech · {BY_HAND}"), "Other.srt", Some(0.9), Some(1000)).unwrap();
        // No rip is there to read, and none is.
        keep_own_files(&backend);
        let after = backend.db.episodes(show).unwrap();
        let told: Vec<(&str, &str, &str)> = after.iter().map(|e| (e.subs_state.as_str(), e.subs_file.as_str(), e.subs_detail.as_str())).collect();
        assert_eq!(told[0], ("synced", "Show - 01.ja.srt", "beside the rip · as it is"));
        assert_eq!(told[1], ("synced", "Show - 02.ja.srt", "beside the rip · as it is"));
        assert_eq!((told[2].0, told[2].1), ("synced", "Other.srt"));
        assert_eq!(told[3].0, "pending");
        // Its lines are where the file has them, and the other file is there to switch to.
        let starts: Vec<f64> = read_lines(&backend, ids[0]).iter().map(|l| l.start).collect();
        assert_eq!(starts, [1.0, 4.0]);
        let others: Vec<String> = read_tried(&backend, ids[0]).into_iter().map(|t| t.name).collect();
        assert_eq!(others, ["Show - 01.ja.srt", "Show - 01.srt"]);
        // Switching to the other is using it as it is too: no rip is read (there's none to read), nothing moves it.
        let first = backend.db.episode(ids[0]).unwrap().unwrap();
        use_file(&backend, &first, &rips.join("Show - 01.srt"), "Show - 01.srt").unwrap();
        let after = backend.db.episode(ids[0]).unwrap().unwrap();
        assert_eq!((after.subs_state.as_str(), after.subs_file.as_str(), after.subs_detail.as_str()), ("synced", "Show - 01.srt", "beside the rip · as it is"));
        let starts: Vec<f64> = read_lines(&backend, ids[0]).iter().map(|l| l.start).collect();
        assert_eq!(starts, [1.5, 4.5]);
        // The list to switch between stays as it was, each file marked as yours.
        let tried = read_tried(&backend, ids[0]);
        assert_eq!(tried.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Show - 01.ja.srt", "Show - 01.srt"]);
        assert!(tried.iter().all(|t| t.beside));
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
    fn a_rip_without_english_lines_is_caught_up_once() {
        let dir = std::env::temp_dir().join(format!("telly-no-english-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let open = || Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let backend = open();
        let episodes = (1..=3).map(|number| crate::db::NewEpisode { number, path: format!("{number}.mkv"), size: 1, duration: 1400.0 }).collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let eps = backend.db.episodes(show).unwrap();
        // Three episodes ready, with their pictures: one rip has no subtitles,
        // one an English track that gave no lines, one an English track not read yet.
        let english = media::SubtitleInfo { index: 2, codec: "subrip".into(), language: "eng".into(), title: String::new() };
        let probes = [media::Probe::default(), media::Probe { subtitles: vec![english.clone()], ..Default::default() }, media::Probe { subtitles: vec![english], ..Default::default() }];
        for (e, probe) in eps.iter().zip(&probes) {
            backend.db.set_probe(e.id, &serde_json::to_string(probe).unwrap(), 1400.0, None, "direct").unwrap();
            backend.db.media_ready(e.id, &e.path, 0, 0).unwrap();
            backend.db.set_subs(e.id, "synced", "", "", None, None).unwrap();
            let sheet = sheet_path(&backend, show, e.id);
            std::fs::create_dir_all(sheet.parent().unwrap()).unwrap();
            std::fs::write(&sheet, "pictures").unwrap();
        }
        assert_eq!(pick(&backend, "catch up").map(|(_, e, _)| e.number), Some(1));
        // Looked over at once, or read: the first two have no English lines, and the player is told so.
        remember_without_english(&backend);
        keep_translation(&backend, eps[1].id, &probes[1], &[media::TrackTiming { index: 2, codec: "subrip".into(), ..Default::default() }]);
        assert!(translation(&backend, &eps[0]).unwrap().is_empty() && translation(&backend, &eps[1]).unwrap().is_empty());
        // At the next start, only the one whose English lines are still to read is behind.
        drop(backend);
        let backend = open();
        assert_eq!(pick(&backend, "catch up").map(|(_, e, _)| e.number), Some(3));
        backend.jobs.caught_up.lock().unwrap().insert(eps[2].id);
        assert!(pick(&backend, "catch up").is_none());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn on_a_drive_without_a_seek_penalty_the_pictures_come_after_the_subtitles() {
        let dir = std::env::temp_dir().join(format!("telly-after-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rips = dir.join("rips");
        std::fs::create_dir_all(&rips).unwrap();
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        backend.refresh_tools();
        let tools = backend.tools();
        let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
            return; // This computer has no ffmpeg to make rips with.
        };
        // Two rips that play as they are, with English subtitles.
        std::fs::write(rips.join("en.srt"), "1\n00:00:01,000 --> 00:00:03,000\nA world of magic.\n\n").unwrap();
        let mut episodes = Vec::new();
        for number in 1..=2 {
            let path = rips.join(format!("Show - 0{number}.mkv"));
            let made = crate::util::command(&ffmpeg)
                .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=64x36:r=2", "-f", "lavfi", "-i", "anullsrc=r=8000:cl=mono", "-i"])
                .arg(rips.join("en.srt"))
                .args(["-t", "20", "-map", "0", "-map", "1", "-map", "2", "-c:v", "libx264", "-preset", "ultrafast", "-g", "4", "-c:a", "aac", "-c:s", "srt", "-metadata:s:s:0", "language=eng"])
                .arg(&path)
                .status();
            if !made.is_ok_and(|s| s.success()) {
                return; // This ffmpeg can't make H.264.
            }
            episodes.push(crate::db::NewEpisode { number, path: path.to_string_lossy().into_owned(), size: 1, duration: 20.0 });
        }
        let id = backend.db.add_show(&crate::db::NewShow { folder: rips.to_string_lossy().into_owned(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let show = backend.db.show(id).unwrap().unwrap();
        for e in backend.db.episodes(id).unwrap() {
            assert!(plan_video(&backend, &ffprobe, &show, &e).unwrap().is_none());
        }
        let eps = backend.db.episodes(id).unwrap();
        let (slot, english) = (Mutex::new(None), |e: &Episode| translation(&backend, e).unwrap().into_iter().map(|l| l.text).collect::<Vec<_>>());
        // From a hard drive (or one that can't be told about), one read makes both.
        find_subtitles(&backend, &slot, &ffmpeg, &ffprobe, &show, &eps[0], 2).unwrap();
        assert!(sheet_path(&backend, id, eps[0].id).is_file());
        assert_eq!(english(&eps[0]), ["A world of magic."]);
        // From an SSD, the subtitles are read on their own, and the pictures made after.
        let root = Path::new(&eps[1].path).ancestors().last().unwrap().to_path_buf();
        backend.jobs.drives.lock().unwrap().insert(root, Arc::new(Turns { at_once: READS_AT_ONCE, seek_free: true, ..Turns::default() }));
        find_subtitles(&backend, &slot, &ffmpeg, &ffprobe, &show, &eps[1], 2).unwrap();
        assert!(!sheet_path(&backend, id, eps[1].id).is_file());
        assert_eq!(english(&eps[1]), ["A world of magic."]);
        pictures_after(&backend, &slot, &ffmpeg, &show, &eps[1]);
        assert!(sheet_path(&backend, id, eps[1].id).is_file());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rips_on_one_drive_share_its_turns() {
        let jobs = Jobs::default();
        let drive = jobs.drive(Path::new("D:\\Anime\\Show\\01.mkv"));
        assert!(Arc::ptr_eq(&drive, &jobs.drive(Path::new("D:\\Films\\Film.mkv"))));
        assert!(!Arc::ptr_eq(&drive, &jobs.drive(Path::new("C:\\Anime\\Show\\02.mkv"))));
    }

    #[test]
    fn a_drive_s_turns_go_to_the_episodes_that_come_first() {
        let turns = Turns::default();
        let order = Mutex::new(Vec::new());
        let queued = |n: usize| {
            while turns.queue().waiting.len() < n {
                std::thread::yield_now();
            }
        };
        // Three workers ask for the drive while episode 9 (far off) is read:
        // for one of the 10 after the next episode, the next, another far off.
        let reading = turns.take((2, 0, 9));
        std::thread::scope(|scope| {
            for (n, place) in [(1, 0, 5), (0, 0, 1), (2, 0, 7)].into_iter().enumerate() {
                let (turns, order) = (&turns, &order);
                scope.spawn(move || {
                    let _turn = turns.take(place);
                    order.lock().unwrap().push(place.2);
                });
                queued(n + 1);
            }
            assert!(order.lock().unwrap().is_empty());
            drop(reading);
        });
        assert_eq!(*order.lock().unwrap(), [1, 5, 7]);
        // The next episode, queued but not asking yet (its worker is reading
        // what's in the rip), keeps a later one from starting on the idle drive.
        let next = turns.wait_at((0, 0, 2));
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _turn = turns.take((1, 0, 3));
                order.lock().unwrap().push(3);
            });
            queued(2);
            std::thread::sleep(Duration::from_millis(20));
            let turn = next.take();
            order.lock().unwrap().push(2);
            drop(turn);
        });
        assert_eq!(order.lock().unwrap()[3..], [2, 3]);
        // One that stops waiting doesn't hold the others up.
        let gone = turns.wait_at((0, 0, 4));
        drop(gone);
        drop(turns.take((2, 0, 8)));
    }

    #[test]
    fn a_drive_without_a_seek_penalty_is_read_a_few_rips_at_once() {
        // One that can't be told about (any here, but on Windows) is read one rip at a time.
        assert_eq!(Jobs::default().drive(Path::new("/anime/Show/01.mkv")).at_once, 1);
        let turns = Turns { at_once: READS_AT_ONCE, ..Turns::default() };
        let reading: Vec<Turn> = (1..=3).map(|n| turns.take((1, 0, n))).collect();
        std::thread::scope(|scope| {
            // A fourth waits for one of the three to end.
            let fourth = scope.spawn(|| drop(turns.take((1, 0, 4))));
            while turns.queue().waiting.is_empty() {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(20));
            assert!(!fourth.is_finished());
            drop(reading);
            fourth.join().unwrap();
        });
        assert_eq!(turns.queue().reading, 0);
    }

    #[test]
    fn finds_the_japanese_subtitles_beside_a_rip() {
        let folder = std::env::temp_dir().join(format!("telly-beside-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let japanese = "1\n00:00:01,000 --> 00:00:03,000\nそこは魔法の世界。\n\n2\n00:00:04,000 --> 00:00:06,000\n彼らは魔道士と呼ばれる。\n";
        let english = "1\n00:00:01,000 --> 00:00:03,000\nA world of magic.\n\n2\n00:00:04,000 --> 00:00:06,000\nThey are called wizards.\n";
        for (name, text) in [("Show - 01.mkv", ""), ("Show - 01.en.srt", english), ("Show - 01.srt", japanese), ("Show - 01.ai.srt", japanese), ("Show - 01.jp[sdh].ja.srt", japanese), ("Show - 01.jpn.srt", japanese), ("Show - 01.alass.srt", japanese), ("Show - 010.srt", japanese), ("Show - 01.txt", japanese)] {
            std::fs::write(folder.join(name), text).unwrap();
        }
        let mut rip = ep(1, false, 0.0, 0);
        rip.path = folder.join("Show - 01.mkv").to_string_lossy().into_owned();
        let found: Vec<String> = beside(&rip).into_iter().map(|f| f.name).collect();
        std::fs::remove_dir_all(&folder).unwrap();
        // Not the English file, another episode's, or one that isn't subtitles. The one
        // re-timed with alass first, then those marked Japanese, the bare one, the machine-made one.
        assert_eq!(found, ["Show - 01.alass.srt", "Show - 01.jpn.srt", "Show - 01.jp[sdh].ja.srt", "Show - 01.srt", "Show - 01.ai.srt"]);
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
