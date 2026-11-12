//! Everything the UI asks for goes through `Backend::call(cmd, args)`, from
//! the app (lib.rs) or from the development server (devserver.rs), so both
//! run the same code.

use crate::anilist::{self, Anime};
use crate::anilist_list;
use crate::ask::{self, Asks};
use crate::cards::{self, Event};
use crate::covers;
use crate::db::{Db, Episode, NewShow, Show};
use crate::jobs::{self, Jobs, Task};
use crate::known;
use crate::log::{Log, Sitting};
use crate::media;
use crate::mining::{self, Mining};
use crate::names::{self, Names};
use crate::own_hachidori::{self, Own};
use crate::premine::{self, Premining};
use crate::recommend;
use crate::scan;
use crate::search;
use crate::subs::Line;
use crate::sync;
use crate::tools::{self, Tools};
use crate::util::now_ms;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// Where Telly keeps things. %LOCALAPPDATA%\Telly for the library, copies,
/// subtitles and clips; %APPDATA%\Telly for the log Tidepool reads.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub local: PathBuf,
    pub roaming: PathBuf,
}

impl Dirs {
    pub fn media(&self) -> PathBuf {
        self.local.join("media")
    }
    pub fn covers(&self) -> PathBuf {
        self.local.join("covers")
    }
    pub fn subs(&self) -> PathBuf {
        self.local.join("subtitles")
    }
    pub fn jimaku(&self) -> PathBuf {
        self.local.join("jimaku")
    }
    pub fn clips(&self) -> PathBuf {
        self.local.join("clips")
    }
    pub fn tools(&self) -> PathBuf {
        self.local.join("tools")
    }
    pub fn models(&self) -> PathBuf {
        self.local.join("models")
    }
}

/// How subtitles are matched; a newer one syncs every episode again.
const SUBS_ENGINE: i64 = 2;
/// Telly's own Hachidori has your dictionaries and Anki setup (own_hachidori.rs),
/// so it stops waiting on a browser to link to, once.
const OWN_HACHIDORI: i64 = 1;

/// The episode open in the player.
struct Playing {
    episode_id: i64,
    show_id: i64,
    position: f64,
    playing: bool,
    at: Instant,
    saved_at: Instant,
    session: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
pub struct NowPlaying {
    pub episode_id: i64,
    pub show_id: i64,
    pub position: f64,
}

pub struct Backend {
    pub db: Db,
    pub log: Log,
    pub dirs: Dirs,
    tools: RwLock<Tools>,
    pub jobs: Jobs,
    pub cards: cards::Queue,
    playing: Mutex<Option<Playing>>,
    last_play: Mutex<Option<Instant>>,
    ahead: Arc<Ahead>,
    events: Mutex<(u64, VecDeque<Event>)>,
    pub mining: OnceLock<Arc<Mining>>,
    /// Questions for Claude being answered (ask.rs).
    pub asks: Asks,
    /// The character dictionary's worker (names).
    pub names: Names,
    /// Premining's list and cards (premine.rs).
    pub premining: Premining,
    /// The work Telly's own Hachidori does for it (own_hachidori.rs).
    pub own_hachidori: Own,
    /// Every spoken line and its words, for line search (search.rs).
    pub lines: search::Index,
    /// The lines last moved by "Earlier/Later from here" (move_lines).
    moved: Mutex<Option<Moved>>,
}

/// Where "Earlier/Later from here" last moved an episode's lines from, so
/// pressing it again at the same spot moves the same lines again, though
/// they now start before it.
struct Moved {
    episode: i64,
    at: f64,
    first: usize,
    when: Instant,
}

/// One line that just played, for the log.
#[derive(Deserialize, Debug)]
struct Played {
    chars: i64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct TickArgs {
    id: i64,
    position: f64,
    playing: bool,
    #[serde(default)]
    subtitles: bool,
    #[serde(default)]
    lines: Vec<Played>,
    #[serde(default)]
    since: u64,
}

/// What the library shows for a show.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ShowCard {
    #[serde(flatten)]
    pub show: Show,
    /// The cover, served by Telly ("" when there's none yet).
    pub cover: String,
    /// The cover is a frame from the video: the title is drawn over it.
    pub cover_caption: bool,
    /// A film, listed as one rather than as an episode.
    pub movie: bool,
    /// The uses of words in its scored episodes, and how many are of words you know (0 and 0 when none is scored).
    pub words_uses: i64,
    pub words_known: i64,
    pub episodes: usize,
    pub watched: usize,
    pub media_ready: usize,
    pub subs_ready: usize,
    pub subs_found: usize,
    /// new, watching, finished or getting (the next episode isn't ready to play yet).
    pub status: String,
    pub next: Option<NextUp>,
    pub cards: i64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NextUp {
    pub episode_id: i64,
    pub number: i64,
    pub resume: bool,
    pub position: f64,
    pub duration: f64,
    pub ready: bool,
    pub media_state: String,
    pub media_progress: f64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeRow {
    #[serde(flatten)]
    pub episode: Episode,
    pub file: String,
    pub watched: bool,
    pub is_next: bool,
    /// How much of its words you know, once worked out (premine.rs).
    pub words: Option<premine::Share>,
}

/// What's being worked on, for the title bar.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub tasks: Vec<Task>,
    /// The shows being got ready: how many, the first one and its title, and
    /// between them, episodes with subtitles settled and episodes in all.
    pub shows: usize,
    pub show_id: Option<i64>,
    pub title: String,
    pub done: usize,
    pub total: usize,
}

fn arg<T: for<'de> Deserialize<'de>>(args: &Value, key: &str) -> Result<T> {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null)).with_context(|| format!("the \"{key}\" argument"))
}

fn to_value<T: Serialize>(v: T) -> Result<Value> {
    Ok(serde_json::to_value(v)?)
}

/// Whether a show is a film: AniList says so (`format`), or AniList doesn't
/// know it (`matched` is false) and it's one file, an hour or more long.
fn is_movie(format: &str, matched: bool, lengths: &[f64]) -> bool {
    format == "MOVIE" || (!matched && lengths.len() == 1 && lengths[0] >= 3600.0)
}

/// The setting that says a show's cover is a frame from its video.
fn frame_setting(show_id: i64) -> String {
    format!("cover.frame.{show_id}")
}

fn settled(state: &str) -> bool {
    !matches!(state, "pending" | "working")
}

pub struct Served {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub range: Option<(u64, u64, u64)>,
}

fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("opus") => "audio/ogg",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// The most of a file one request returns; the player asks for the rest as it goes.
const CHUNK: u64 = 4 * 1024 * 1024;

/// How much of a file the reader ahead of the player reads at a time. A hard
/// drive gives big reads its full speed, where the player's own small ones,
/// taken in turn with anything else reading the drive, get a fraction of it.
const AHEAD_BLOCK: usize = 8 * 1024 * 1024;

/// How far the reader goes past where the player last asked: a quarter of the
/// computer's memory, from 512 MB to 4 GB. An episode is read to its end in
/// one go. A file bigger than Windows' file cache (a 40 GB film) would push
/// out what was read first, which is what the player wants next, so it's
/// read as it plays instead.
fn ahead_max() -> u64 {
    static MAX: OnceLock<u64> = OnceLock::new();
    *MAX.get_or_init(|| {
        let total = sysinfo::System::new().ok().map(|mut s| {
            s.refresh_memory();
            s.total_memory()
        });
        (total.unwrap_or(0) / 4).clamp(512 << 20, 4 << 30)
    })
}

/// The copies folder's size as last worked out (Backend::space_used): when, of which folder, and the bytes.
static SPACE_USED: Mutex<Option<(Instant, PathBuf, u64)>> = Mutex::new(None);

fn forget_space_used() {
    if let Ok(mut kept) = SPACE_USED.lock() {
        *kept = None;
    }
}

/// The reading ahead of the episode in the player (see `Backend::read_ahead`):
/// the file, where the player last asked from, how far the reading has got,
/// and which reader it is (a new one takes over when the player jumps).
#[derive(Default)]
struct Ahead {
    at: Mutex<(PathBuf, u64, u64, u64)>,
}

fn read_range(path: &Path, range: Option<&str>) -> Result<Served> {
    let mut file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let total = file.metadata()?.len();
    let mime = mime_of(path);
    let wanted = range.and_then(|r| r.trim().strip_prefix("bytes=")).and_then(|r| {
        let (a, b) = r.split_once('-')?;
        let start: u64 = a.trim().parse().ok()?;
        let end: Option<u64> = b.trim().parse().ok();
        Some((start, end))
    });
    match wanted {
        Some((start, end)) => {
            if total == 0 || start >= total {
                return Ok(Served { bytes: Vec::new(), mime, range: Some((0, 0, total)) });
            }
            let end = end.unwrap_or(total - 1).min(total - 1).min(start + CHUNK - 1);
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = vec![0; (end - start + 1) as usize];
            file.read_exact(&mut bytes)?;
            Ok(Served { bytes, mime, range: Some((start, end, total)) })
        }
        None if total > CHUNK && mime.starts_with("video/") => {
            // A whole video in one answer would be huge; answer as a range so the player pages through it.
            let mut bytes = vec![0; CHUNK as usize];
            file.read_exact(&mut bytes)?;
            Ok(Served { bytes, mime, range: Some((0, CHUNK - 1, total)) })
        }
        None => {
            let mut bytes = Vec::with_capacity(total as usize);
            file.read_to_end(&mut bytes)?;
            Ok(Served { bytes, mime, range: None })
        }
    }
}

impl Backend {
    pub fn open(dirs: Dirs) -> Result<Self> {
        std::fs::create_dir_all(&dirs.local)?;
        std::fs::create_dir_all(&dirs.roaming)?;
        let db = Db::open(&dirs.local.join("telly.sqlite"))?;
        let log = Log::open(&dirs.roaming.join("telly-log.sqlite"))?;
        let _ = log.tidy();
        let lines = search::Index::open(&dirs.local.join("telly-lines.sqlite"))?;
        // Failed subtitles get another go each time Telly starts (Jimaku may have been down).
        let _ = db.requeue_subs(None, &["failed"]);
        // Subtitles synced by an older way of matching are synced again: since
        // version 2 files are matched to the rip's own subtitles when it has
        // them, or FireRedVAD's view of the speech, and season folders are numbered right.
        if db.setting_i64("subs.engine", 1) < SUBS_ENGINE {
            let _ = db.requeue_subs(None, &["synced", "check", "none"]);
            let _ = db.set_setting("subs.engine", &json!(SUBS_ENGINE));
        }
        if db.setting_i64("hachidori.own", 0) < OWN_HACHIDORI {
            let _ = db.set_setting("hachidori.link", &json!(false));
            let _ = db.set_setting("hachidori.own", &json!(OWN_HACHIDORI));
        }
        // What ffmpeg can do is asked once Telly is open (start): that takes a few seconds.
        let found = tools::find_files(&dirs.tools(), &dirs.models(), &db.setting_str("tools.ffmpeg"), &db.setting_str("whisper.model"));
        Ok(Backend {
            db,
            log,
            dirs,
            tools: RwLock::new(found),
            jobs: Jobs::default(),
            cards: cards::Queue::default(),
            playing: Mutex::new(None),
            last_play: Mutex::new(None),
            ahead: Arc::default(),
            events: Mutex::new((0, VecDeque::new())),
            mining: OnceLock::new(),
            asks: Asks::default(),
            names: Names::default(),
            premining: Premining::default(),
            own_hachidori: Own::default(),
            lines,
            moved: Mutex::new(None),
        })
    }

    /// Starts the background work: getting shows ready and finishing cards.
    pub fn start(self: &Arc<Self>) {
        let backend = self.clone();
        // The work starts once ffmpeg has said what it can do, so none of it sees half an answer.
        std::thread::spawn(move || {
            backend.refind_tools();
            jobs::clear_parts(&backend);
            jobs::start(backend.clone());
            let for_cards = backend.clone();
            std::thread::Builder::new().name("telly-cards".into()).spawn(move || cards::worker(&for_cards)).ok();
            backend.watch_folders();
            backend.fill_covers();
            recommend::gather(&backend);
            premine::start_shares(&backend, None);
            names::start(backend.clone());
            anilist_list::start(backend.clone());
            search::start(backend.clone());
        });
    }

    /// Listens for Anki Beacon and polls AnkiConnect while the player is open.
    pub fn start_mining(self: &Arc<Self>) {
        let (for_url, for_found) = (self.clone(), self.clone());
        let m = Mining::start(
            || true,
            move || cards::anki_url(&for_url),
            move |ids| {
                let backend = for_found.clone();
                std::thread::spawn(move || cards::notes_found(&backend, ids));
            },
        );
        let _ = self.mining.set(m);
    }

    /// Where Telly's copies of episodes go: the folder Settings chose
    /// ("media.folder", on an external drive, say), else Telly's own.
    pub fn media_dir(&self) -> PathBuf {
        match self.db.setting_str("media.folder") {
            folder if folder.is_empty() => self.dirs.media(),
            folder => PathBuf::from(folder),
        }
    }

    /// Chooses the copies folder (None: Telly's own again) and moves the copies there.
    fn set_media_folder(self: &Arc<Self>, folder: Option<String>) -> Result<Value> {
        let folder = folder.map(|f| f.trim().to_string()).filter(|f| !f.is_empty());
        if let Some(folder) = &folder {
            std::fs::create_dir_all(folder).map_err(|e| anyhow!("Telly can’t use {folder}: {e}. Choose another folder."))?;
        }
        let before = self.media_dir();
        forget_space_used();
        self.db.set_setting("media.folder", &folder.map(Value::String).unwrap_or(Value::Null))?;
        jobs::move_copies(self.clone(), before);
        Ok(self.status())
    }

    pub fn tools(&self) -> Tools {
        self.tools.read().map(|t| t.clone()).unwrap_or_default()
    }

    pub fn refresh_tools(&self) {
        self.refind_tools();
    }

    fn refind_tools(&self) {
        let found = tools::find(&self.dirs.tools(), &self.dirs.models(), &self.db.setting_str("tools.ffmpeg"), &self.db.setting_str("whisper.model"));
        if let Ok(mut t) = self.tools.write() {
            *t = found;
        }
        self.jobs.poke();
    }

    pub fn push_event(&self, state: &str, title: &str, text: &str, note_id: i64, job_id: i64) {
        if let Ok(mut e) = self.events.lock() {
            e.0 += 1;
            let id = e.0;
            e.1.push_back(Event { id, state: state.into(), title: title.into(), text: text.into(), note_id, job_id });
            while e.1.len() > 50 {
                e.1.pop_front();
            }
        }
    }

    fn events_since(&self, since: u64) -> Vec<Event> {
        self.events.lock().map(|e| e.1.iter().filter(|ev| ev.id > since).cloned().collect()).unwrap_or_default()
    }

    /// Reads an episode's video ahead of the player, from `start` (where the
    /// player just asked for) on, up to `ahead_max` past where the player last
    /// asked (an episode's end), so what the player asks for next is in
    /// Windows' file cache already and comes from memory, not the drive.
    /// The player itself keeps only a few seconds ahead. One reader at a time:
    /// asking from somewhere it isn't (another episode, a jump) starts a new
    /// one there, and the old one stops.
    fn read_ahead(&self, path: &Path, start: u64) {
        let Ok(mut at) = self.ahead.at.lock() else { return };
        // The reader is here, or will be within a block: it goes on from where the player is.
        if at.0 == path && start >= at.1 && start <= at.2 + AHEAD_BLOCK as u64 {
            at.1 = start;
            return;
        }
        *at = (path.to_path_buf(), start, start, at.3 + 1);
        let (ahead, path, reader) = (self.ahead.clone(), path.to_path_buf(), at.3);
        std::thread::spawn(move || {
            let Ok(mut file) = std::fs::File::open(&path) else { return };
            if file.seek(SeekFrom::Start(start)).is_err() {
                return;
            }
            let mut block = vec![0; AHEAD_BLOCK];
            loop {
                // Far enough ahead: wait for the player to catch up, or for another reader.
                loop {
                    let Ok(at) = ahead.at.lock() else { return };
                    if at.3 != reader {
                        return;
                    }
                    if at.2 < at.1 + ahead_max() {
                        break;
                    }
                    drop(at);
                    std::thread::sleep(Duration::from_millis(500));
                }
                let read = file.read(&mut block).unwrap_or(0);
                let Ok(mut at) = ahead.at.lock() else { return };
                if read == 0 || at.3 != reader {
                    return;
                }
                at.2 += read as u64;
            }
        });
    }

    /// The episode playing (or paused) in the player, if it's open.
    pub fn now_playing(&self) -> Option<NowPlaying> {
        let p = self.playing.lock().ok()?;
        let p = p.as_ref()?;
        (p.at.elapsed() < Duration::from_secs(90)).then_some(NowPlaying { episode_id: p.episode_id, show_id: p.show_id, position: p.position })
    }

    pub fn playing_show(&self) -> Option<i64> {
        self.now_playing().map(|p| p.show_id)
    }

    /// How long since anything played.
    pub fn idle_for(&self) -> Option<Duration> {
        self.last_play.lock().ok().and_then(|t| t.map(|t| t.elapsed()))
    }

    /// A show's cover, as it was fetched.
    fn cover_path(&self, show_id: i64) -> PathBuf {
        self.dirs.covers().join(format!("{show_id}.jpg"))
    }

    /// The copy of it the app draws (covers::make_small), made when the cover is got.
    fn small_cover_path(&self, show_id: i64) -> PathBuf {
        self.dirs.covers().join(format!("{show_id}.small.jpg"))
    }

    fn remove_cover(&self, show_id: i64) {
        let _ = std::fs::remove_file(self.cover_path(show_id));
        let _ = std::fs::remove_file(self.small_cover_path(show_id));
    }

    fn fetch_cover(self: &Arc<Self>, show_id: i64, url: String) {
        let backend = self.clone();
        std::thread::spawn(move || backend.get_cover(show_id, &url));
    }

    /// Gets a show its cover: AniList's (`url`) when it has one; else the
    /// poster from Wikipedia's article about a film or series of that name;
    /// else a frame from its first episode.
    fn get_cover(&self, show_id: i64, url: &str) {
        let path = self.cover_path(show_id);
        // A frame has the show's title drawn over it, as a poster has; a real poster has its own.
        // Either way the small copy the app draws is made now, not when the Library opens.
        let framed = |is: bool| {
            let _ = self.db.set_setting(&frame_setting(show_id), &if is { Value::Bool(true) } else { Value::Null });
            let _ = covers::make_small(&path, &self.small_cover_path(show_id));
        };
        if !url.is_empty() {
            if anilist::download_cover(url, &path).is_ok() {
                framed(false);
            }
            return;
        }
        let Ok(Some(show)) = self.db.show(show_id) else { return };
        if let Ok(Some(poster)) = covers::wikipedia_poster(&show.title) {
            if anilist::download_cover(&poster, &path).is_ok() {
                framed(false);
                return;
            }
        }
        let first = self.db.episodes(show_id).ok().and_then(|episodes| episodes.into_iter().next());
        if let (Some(ffmpeg), Some(first)) = (self.tools().ffmpeg, first) {
            if covers::frame(&ffmpeg, Path::new(&first.path), first.duration, &path).is_ok() {
                framed(true);
            }
        }
    }

    /// Shows without a cover (added before Telly made its own, or offline) get
    /// one, and covers without a small copy (got before Telly made those) get theirs.
    fn fill_covers(self: &Arc<Self>) {
        let backend = self.clone();
        std::thread::Builder::new()
            .name("telly-covers".into())
            .spawn(move || {
                for show in backend.db.shows().unwrap_or_default() {
                    if !backend.cover_path(show.id).is_file() {
                        backend.get_cover(show.id, &show.cover_url);
                    } else if !backend.small_cover_path(show.id).is_file() {
                        let _ = covers::make_small(&backend.cover_path(show.id), &backend.small_cover_path(show.id));
                    }
                }
            })
            .ok();
    }

    /// A show's card, from its episodes (in order, as `Db::episodes` has them).
    fn card(&self, show: Show, episodes: &[Episode], cards_by_show: &std::collections::HashMap<i64, i64>, scores: &std::collections::HashMap<i64, (i64, i64, String)>) -> Result<ShowCard> {
        let watched = episodes.iter().filter(|e| e.watched_at.is_some()).count();
        let media_ready = episodes.iter().filter(|e| e.media_state == "ready").count();
        let subs_ready = episodes.iter().filter(|e| settled(&e.subs_state)).count();
        let subs_found = episodes.iter().filter(|e| matches!(e.subs_state.as_str(), "synced" | "check" | "manual")).count();
        let next = jobs::next_episode(episodes).map(|e| NextUp {
            episode_id: e.id,
            number: e.number,
            resume: e.watched_at.is_none() && e.position >= 20.0 && (e.duration <= 0.0 || e.position < e.duration * 0.9),
            position: e.position,
            duration: e.duration,
            ready: e.media_state == "ready",
            media_state: e.media_state.clone(),
            media_progress: e.media_progress,
        });
        let started = watched > 0 || episodes.iter().any(|e| e.position >= 20.0);
        let status = match &next {
            None if !episodes.is_empty() => "finished",
            Some(n) if !n.ready => "getting",
            _ if started => "watching",
            _ => "new",
        };
        // The small copy once it's made (another address, so the big one isn't kept in its place).
        let small = self.small_cover_path(show.id).is_file();
        let cover = if small || self.cover_path(show.id).is_file() {
            format!("/cover/{}?v={}{}", show.id, show.anilist_id.unwrap_or(0), if small { "s" } else { "" })
        } else {
            String::new()
        };
        let lengths: Vec<f64> = episodes.iter().map(|e| e.duration).collect();
        let (words_uses, words_known) = episodes.iter().filter_map(|e| scores.get(&e.id)).fold((0, 0), |(uses, known), s| (uses + s.0, known + s.1));
        Ok(ShowCard {
            words_uses,
            words_known,
            cards: cards_by_show.get(&show.id).copied().unwrap_or(0),
            movie: is_movie(&show.format, show.anilist_id.is_some(), &lengths),
            cover_caption: !cover.is_empty() && self.db.setting_bool(&frame_setting(show.id), false),
            show,
            cover,
            episodes: episodes.len(),
            watched,
            media_ready,
            subs_ready,
            subs_found,
            status: status.into(),
            next,
        })
    }

    fn activity(&self) -> Activity {
        let mut tasks = self.jobs.tasks();
        tasks.extend(self.names.task.lock().ok().and_then(|t| t.clone()));
        let mut activity = Activity { tasks, ..Default::default() };
        // Several episodes are worked on at once, often of different shows.
        let mut seen = std::collections::HashSet::new();
        for t in activity.tasks.iter().filter(|t| t.show_id != 0) {
            if !seen.insert(t.show_id) {
                continue;
            }
            let Ok(eps) = self.db.episodes(t.show_id) else { continue };
            if activity.shows == 0 {
                activity.show_id = Some(t.show_id);
                activity.title = t.title.clone();
            }
            activity.shows += 1;
            activity.total += eps.len();
            activity.done += eps.iter().filter(|e| settled(&e.subs_state)).count();
        }
        activity
    }

    fn setup(&self) -> Value {
        let tools = self.tools();
        json!({
            "ffmpeg": tools.ffmpeg.is_some(),
            "jimaku": !self.db.setting_str("jimaku.key").trim().is_empty(),
            "canClip": tools.can_clip,
        })
    }

    fn library(&self) -> Result<Value> {
        let cards_by_show = self.db.cards_per_show().unwrap_or_default();
        let scores = self.db.word_scores().unwrap_or_default();
        let shows = self.db.shows()?;
        // Every show's episodes in one query, not one each.
        let mut by_show: std::collections::HashMap<i64, Vec<Episode>> = std::collections::HashMap::new();
        for e in self.db.all_episodes()? {
            by_show.entry(e.show_id).or_default().push(e);
        }
        let mut cards = Vec::with_capacity(shows.len());
        for s in shows {
            let episodes = by_show.remove(&s.id).unwrap_or_default();
            cards.push(self.card(s, &episodes, &cards_by_show, &scores)?);
        }
        Ok(json!({ "shows": cards, "activity": self.activity(), "setup": self.setup() }))
    }

    fn show_view(&self, id: i64) -> Result<Value> {
        let show = self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let cards_by_show = self.db.cards_per_show().unwrap_or_default();
        let episodes = self.db.episodes(id)?;
        let next_id = jobs::next_episode(&episodes).map(|e| e.id);
        let card = self.card(show, &episodes, &cards_by_show, &self.db.word_scores().unwrap_or_default())?;
        let (shares, words_error) = premine::shares(self);
        let rows: Vec<EpisodeRow> = episodes
            .into_iter()
            .map(|e| EpisodeRow {
                words: shares.get(&e.id).copied(),
                file: Path::new(&e.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                watched: e.watched_at.is_some(),
                is_next: Some(e.id) == next_id,
                episode: e,
            })
            .collect();
        let tools = self.tools();
        let listener = jobs::listener(self, &tools);
        let whisper = json!({
            "ready": listener.is_some(),
            "needsModel": listener.is_none() && tools.whisper.is_some(),
            "maker": listener.map_or("Whisper", |l| l.name()),
        });
        Ok(json!({ "card": card, "episodes": rows, "activity": self.activity(), "whisper": whisper, "wordsError": words_error }))
    }

    fn scan(&self, folder: &str) -> Result<Value> {
        let tools = self.tools();
        let scan = scan::scan(Path::new(folder), |paths| scan::probe_durations(tools.ffprobe.as_deref(), paths))?;
        let count = scan.files.iter().filter(|f| f.number.is_some()).count();
        let (matches, error) = match anilist::search_loosely(&scan.guess) {
            Ok(mut found) => {
                anilist::rank(&mut found, &scan.guess, count);
                let error = anilist::follow_sequels(&mut found, &scan.guess).err().map(|e| format!("{e:#}"));
                (found, error)
            }
            Err(e) => (Vec::new(), Some(format!("{e:#}"))),
        };
        Ok(json!({ "scan": scan, "matches": matches, "error": error, "ffprobe": tools.ffprobe.is_some() }))
    }

    /// Looks in a show's folder again for episodes that weren't there, or
    /// couldn't be read (still downloading), when it was added. Returns how many it added.
    pub fn rescan(&self, id: i64) -> Result<usize> {
        let show = self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let existing = self.db.episodes(id)?;
        let known: std::collections::HashMap<String, f64> = existing.iter().map(|e| (e.path.clone(), e.duration)).collect();
        let taken: std::collections::HashSet<i64> = existing.iter().map(|e| e.number).collect();
        let tools = self.tools();
        let scan = scan::scan(Path::new(&show.folder), |paths| {
            // Only files Telly doesn't know yet need ffprobe.
            let unknown: Vec<PathBuf> = paths.iter().filter(|p| !known.contains_key(p.to_string_lossy().as_ref())).cloned().collect();
            let probed = scan::probe_durations(tools.ffprobe.as_deref(), &unknown);
            let mut by_path: std::collections::HashMap<PathBuf, Option<f64>> = unknown.into_iter().zip(probed).collect();
            paths
                .iter()
                .map(|p| match known.get(p.to_string_lossy().as_ref()) {
                    // Known episodes count as readable whatever their length.
                    Some(d) => Some(if *d > 0.0 { *d } else { 600.0 }),
                    None => by_path.remove(p).flatten(),
                })
                .collect()
        })?;
        let mut next_number = existing.iter().map(|e| e.number).max().unwrap_or(0) + 1;
        // Numbered as their names say: the folder as a whole is, or the show's
        // episodes are (a second release of one makes the folder's numbers
        // repeat, but its own number is still in its name).
        let parsed: std::collections::HashMap<&str, Option<i64>> = scan.files.iter().map(|f| (f.path.as_str(), f.parsed)).collect();
        let by_names = scan.numbering == "names" || {
            let mut named = existing.iter().filter_map(|e| parsed.get(e.path.as_str()).map(|p| *p == Some(e.number))).peekable();
            named.peek().is_some() && named.all(|by_name| by_name)
        };
        // Episodes whose rip isn't there any more: a new file of the same number replaces it.
        let mut gone: std::collections::HashMap<i64, i64> =
            existing.iter().filter(|e| !Path::new(&e.path).is_file()).map(|e| (e.number, e.id)).collect();
        let mut new = Vec::new();
        let mut replaced = 0;
        for f in scan.files.iter().filter(|f| f.reason.is_empty() && !known.contains_key(&f.path)) {
            let number = match f.parsed {
                Some(n) if by_names => {
                    if let Some(episode) = gone.remove(&n) {
                        self.replace_rip(id, episode, f)?;
                        replaced += 1;
                        continue;
                    }
                    if taken.contains(&n) || new.iter().any(|e: &crate::db::NewEpisode| e.number == n) {
                        continue; // another copy of an episode Telly has
                    }
                    n
                }
                _ => {
                    next_number += 1;
                    next_number - 1
                }
            };
            new.push(crate::db::NewEpisode { number, path: f.path.clone(), size: f.size, duration: f.duration });
        }
        if !new.is_empty() {
            self.db.add_episodes(id, &new)?;
        }
        if replaced + new.len() > 0 {
            self.jobs.poke();
        }
        Ok(new.len() + replaced)
    }

    /// Puts a new rip in place of an episode's that's gone. What Telly made
    /// from the old one (its copy, seek-bar pictures, the files in subtitles)
    /// goes, so the new one is read and synced from the start.
    fn replace_rip(&self, show_id: i64, episode_id: i64, file: &scan::ScannedFile) -> Result<()> {
        self.db.replace_rip(episode_id, &file.path, file.size, file.duration)?;
        let _ = std::fs::remove_file(jobs::copy_path(self, show_id, episode_id));
        let _ = std::fs::remove_file(jobs::sheet_path(self, show_id, episode_id));
        self.forget_episode_files(&[episode_id]);
        let _ = self.lines.forget(episode_id);
        Ok(())
    }

    /// Removes everything kept in subtitles by these episodes' ids: their
    /// lines, translation, words, the files tried, a rip's text tracks and
    /// Whisper's transcripts.
    fn forget_episode_files(&self, ids: &[i64]) {
        let prefixes: Vec<String> = ids.iter().map(|id| format!("{id}.")).collect();
        for entry in std::fs::read_dir(self.dirs.subs()).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefixes.iter().any(|prefix| name.starts_with(prefix.as_str())) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Looks for new episodes in every show's folder now, and every 10 minutes.
    fn watch_folders(self: &Arc<Self>) {
        let backend = self.clone();
        std::thread::Builder::new()
            .name("telly-folders".into())
            .spawn(move || loop {
                for show in backend.db.shows().unwrap_or_default() {
                    if Path::new(&show.folder).is_dir() {
                        let _ = backend.rescan(show.id);
                    }
                }
                std::thread::sleep(Duration::from_secs(600));
            })
            .ok();
    }

    fn new_show_from(anime: Option<&Anime>, mut base: NewShow) -> NewShow {
        if let Some(a) = anime {
            base.title = a.short_title();
            base.title_native = a.native.clone();
            base.title_english = a.english.clone();
            base.title_romaji = a.romaji.clone();
            base.anilist_id = Some(a.id);
            base.anilist_episodes = a.episodes;
            base.format = a.format.clone();
            base.year = a.year;
            base.cover_url = a.cover.clone();
            base.cover_color = a.color.clone();
        }
        base
    }

    fn add_show(self: &Arc<Self>, args: &Value) -> Result<Value> {
        let mut new: NewShow = serde_json::from_value(args.clone()).context("the show to add")?;
        if new.episodes.is_empty() {
            return Err(anyhow!("There are no episodes to add"));
        }
        let anime = match new.anilist_id {
            Some(id) => Some(anilist::by_id(id)?),
            None => None,
        };
        if new.title.trim().is_empty() {
            new.title = scan::clean_title(Path::new(&new.folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().as_str());
        }
        let new = Self::new_show_from(anime.as_ref(), new);
        let id = self.db.add_show(&new)?;
        self.fetch_cover(id, new.cover_url.clone());
        recommend::gather(self);
        self.jobs.poke();
        Ok(json!({ "id": id }))
    }

    fn rematch(self: &Arc<Self>, id: i64, anilist_id: i64) -> Result<Value> {
        let show = self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let anime = anilist::by_id(anilist_id)?;
        let new = Self::new_show_from(Some(&anime), NewShow { folder: show.folder.clone(), ..Default::default() });
        self.db.set_show_anilist(id, &new)?;
        self.remove_cover(id);
        self.fetch_cover(id, new.cover_url.clone());
        recommend::gather(self);
        // Subtitles were found, or found missing and queued to be made, under the old show's name.
        self.db.requeue_subs(Some(id), &["none", "failed", "check", "synced", "nokey", "whisper"])?;
        self.jobs.forget_jimaku();
        self.jobs.poke();
        self.show_view(id)
    }

    /// "Earlier/Later from here": moves an episode's lines from `at` seconds
    /// into the video (the player's time, your nudge in it) by `ms`.
    fn move_lines(&self, id: i64, at: f64, ms: i64) -> Result<Value> {
        let mut lines = jobs::read_lines(self, id);
        let mut moved = self.moved.lock().map_err(|_| anyhow!("the lines are being moved already"))?;
        let first = match moved.as_ref() {
            Some(m) if m.episode == id && (m.at - at).abs() < 2.0 && m.when.elapsed() < Duration::from_secs(60) => m.first,
            _ => sync::first_from(&lines, at - cards::nudge(self, id)),
        };
        sync::shift_from(&mut lines, first, ms);
        *moved = Some(Moved { episode: id, at, first, when: Instant::now() });
        drop(moved);
        jobs::write_lines(self, id, &lines)?;
        // Line search plays scenes at the lines' times.
        search::index(self, id);
        to_value(lines)
    }

    fn remove_show(&self, id: i64) -> Result<Value> {
        self.jobs.cancel_show(id);
        let episodes = self.db.episodes(id)?;
        self.db.remove_show(id)?;
        // Only Telly's own files go; your rips stay where they are.
        let _ = std::fs::remove_dir_all(self.media_dir().join(id.to_string()));
        self.forget_episode_files(&episodes.iter().map(|e| e.id).collect::<Vec<_>>());
        for e in &episodes {
            let _ = self.lines.forget(e.id);
        }
        self.remove_cover(id);
        Ok(json!({}))
    }

    /// Removes Telly's copies of episodes you've watched (or of a whole finished show).
    fn free_space(&self, show_id: Option<i64>) -> Result<Value> {
        forget_space_used();
        let mut freed: u64 = 0;
        let mut count = 0;
        let shows: Vec<i64> = match show_id {
            Some(id) => vec![id],
            None => self.db.shows()?.into_iter().map(|s| s.id).collect(),
        };
        let playing = self.now_playing().map(|p| p.episode_id);
        for sid in shows {
            for e in self.db.episodes(sid)? {
                let copy = jobs::copy_path(self, sid, e.id);
                if e.watched_at.is_none() || Some(e.id) == playing || !copy.is_file() {
                    continue;
                }
                freed += std::fs::metadata(&copy).map(|m| m.len()).unwrap_or(0);
                if std::fs::remove_file(&copy).is_ok() {
                    let _ = std::fs::remove_file(self.sheet_path(&e));
                    count += 1;
                    self.db.media_reset(e.id)?;
                }
            }
        }
        Ok(json!({ "episodes": count, "bytes": freed }))
    }

    /// The size of Telly's copies, kept for 30 s: Settings asks every 5 s, and
    /// the copies folder may be on a hard drive. Freeing space or moving the
    /// copies works it out again.
    fn space_used(&self) -> u64 {
        let dir = self.media_dir();
        let kept = SPACE_USED.lock().ok().and_then(|k| k.as_ref().filter(|(at, of, _)| at.elapsed() < Duration::from_secs(30) && *of == dir).map(|k| k.2));
        if let Some(bytes) = kept {
            return bytes;
        }
        fn size(dir: &Path) -> u64 {
            std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|e| {
                            let p = e.path();
                            if p.is_dir() { size(&p) } else { e.metadata().map(|m| m.len()).unwrap_or(0) }
                        })
                        .sum()
                })
                .unwrap_or(0)
        }
        let bytes = size(&dir);
        if let Ok(mut kept) = SPACE_USED.lock() {
            *kept = Some((Instant::now(), dir, bytes));
        }
        bytes
    }

    fn episode_view(&self, id: i64) -> Result<Value> {
        let episode = self.db.episode(id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
        let show = self.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let episodes = self.db.episodes(show.id)?;
        let i = episodes.iter().position(|e| e.id == id).unwrap_or(0);
        let lite = |e: &Episode| json!({ "id": e.id, "number": e.number, "ready": e.media_state == "ready", "mediaState": e.media_state, "mediaProgress": e.media_progress });
        let lines: Vec<Line> = jobs::read_lines(self, id);
        // From what ffprobe found in the rip: its chapters, its opening (else AniSkip's) and its audio tracks.
        let probe = jobs::stored_probe(self, id).unwrap_or_default();
        let audio: Vec<String> = probe
            .audio
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let language = match jobs::language_name(&a.language).trim() {
                    "" if a.language.is_empty() || a.language == "und" => format!("Track {}", i + 1),
                    "" => a.language.clone(),
                    name => name.to_string(),
                };
                if a.title.is_empty() || a.title.eq_ignore_ascii_case(&language) { language } else { format!("{language} · {}", a.title) }
            })
            .collect();
        Ok(json!({
            "show": show,
            "chapters": probe.chapters,
            "opening": media::opening(&probe).or(self.db.opening(id)?).map(|(start, end)| json!({ "start": start, "end": end })),
            "audio": audio,
            "audioTrack": jobs::playing_track(&episode, &probe),
            "listedAudio": jobs::listed_track(&episode, &probe),
            "src": format!("/ep/{id}?v={}-{}", episode.media_plan, episode.audio_track.unwrap_or(-1)),
            "episode": episode,
            "lines": lines,
            "prev": if i > 0 { Some(lite(&episodes[i - 1])) } else { None },
            "next": episodes.get(i + 1).map(lite),
            "count": episodes.len(),
            "tried": jobs::read_tried(self, id),
        }))
    }

    /// The seek bar's pictures of an episode, beside where Telly's copy goes.
    fn sheet_path(&self, episode: &Episode) -> PathBuf {
        jobs::sheet_path(self, episode.show_id, episode.id)
    }

    /// The seek bar's pictures of an episode, made the first time it's asked for.
    fn seek_sheet(&self, id: i64) -> Result<Value> {
        let episode = self.episode_or_err(id)?;
        let sheet = self.sheet_path(&episode);
        if !sheet.is_file() {
            let ffmpeg = self.tools().ffmpeg.ok_or_else(|| anyhow!("Telly needs ffmpeg for this. Choose it in Settings."))?;
            std::fs::create_dir_all(sheet.parent().expect("a folder"))?;
            media::make_sheet(&ffmpeg, Path::new(&episode.play_path), episode.duration, &sheet)?;
        }
        Ok(json!({
            "src": format!("/sheet/{id}"),
            "every": media::SHEET_EVERY,
            "columns": media::SHEET_COLUMNS,
            "rows": media::sheet_rows(episode.duration),
        }))
    }

    fn open_player(&self, id: i64) -> Result<Value> {
        let episode = self.db.episode(id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
        // How long since the show was last open, for "Previously on".
        let away = self.db.show(episode.show_id)?.and_then(|s| ask::away_ms(s.last_watched_at));
        let first_time = self.db.show(episode.show_id)?.is_some_and(|s| s.last_watched_at.is_none());
        self.db.touch_show(episode.show_id)?;
        // A show starting for the first time joins the character dictionary.
        if first_time {
            self.names.poke();
        }
        if let Ok(mut p) = self.playing.lock() {
            self.end_sitting(p.take());
            let now = Instant::now();
            *p = Some(Playing { episode_id: id, show_id: episode.show_id, position: episode.position, playing: false, at: now, saved_at: now, session: None });
        }
        if let Some(m) = self.mining.get() {
            m.watch(true);
        }
        self.jobs.poke();
        // Explain answers in a second or two from a Claude Code that's already started.
        if crate::claude::find().is_some() {
            crate::claude::warm_up(ask::model(self).as_deref(), Some(ask::EFFORT));
        }
        let mut view = self.episode_view(id)?;
        view["awayMs"] = json!(away);
        view["recap"] = ask::recap_state(self, &episode, None);
        // Picking up part-way through: a recap that runs up to there.
        if ask::resuming(&episode, episode.position) {
            view["resume"] = ask::recap_state(self, &episode, Some(episode.position));
        }
        Ok(view)
    }

    /// Plays the rip's audio track `track` in this episode from now on, and in
    /// the show's later episodes as they're got ready again. The Japanese
    /// track stays the default: picking it forgets the choice.
    fn set_audio(&self, id: i64, track: usize) -> Result<Value> {
        let episode = self.episode_or_err(id)?;
        let probe = jobs::stored_probe(self, id).ok_or_else(|| anyhow!("Telly hasn’t read this episode’s audio tracks yet"))?;
        if track >= probe.audio.len() {
            return Err(anyhow!("This episode has no audio track {}", track + 1));
        }
        let ffmpeg = self.tools().ffmpeg.ok_or_else(|| anyhow!("Telly needs ffmpeg for this. Choose it in Settings."))?;
        let chosen = if track == media::japanese_audio(&probe) { Value::Null } else { json!(track) };
        self.db.set_setting(&jobs::audio_setting(episode.show_id), &chosen)?;
        if jobs::playing_track(&episode, &probe) != track {
            jobs::change_audio(self, &ffmpeg, &episode, &probe, track)?;
        }
        // Later episodes that are ready with another track follow: rips that play
        // it as they are at once, Telly's copies when they're got ready again.
        for later in self.db.episodes(episode.show_id)?.into_iter().filter(|e| e.number > episode.number && e.media_state == "ready") {
            if let Some(p) = jobs::stored_probe(self, later.id) {
                jobs::follow_audio(self, &ffmpeg, &later, &p)?;
            }
        }
        self.jobs.poke();
        self.episode_view(id)
    }

    fn end_sitting(&self, p: Option<Playing>) {
        if let Some(p) = p {
            let _ = self.db.save_position(p.episode_id, p.position);
        }
    }

    fn close_player(self: &Arc<Self>, id: i64, position: Option<f64>) -> Result<Value> {
        if let Some(position) = position {
            self.db.save_position(id, position)?;
        }
        if let Ok(mut p) = self.playing.lock() {
            if p.as_ref().is_some_and(|p| p.episode_id == id) {
                let mut taken = p.take();
                if let (Some(t), Some(pos)) = (taken.as_mut(), position) {
                    t.position = pos;
                }
                self.end_sitting(taken);
            }
        }
        if let Some(m) = self.mining.get() {
            m.watch(false);
        }
        self.jobs.poke();
        // Leaving after most of an episode: write the recap for the next one now,
        // so it's ready when you come back. Leaving part-way: the recap up to here.
        if let Ok(e) = self.episode_or_err(id) {
            let seen = position.unwrap_or(e.position);
            let finished = e.watched_at.is_some() || (e.duration > 0.0 && seen >= e.duration * 0.8);
            ask::prepare_recaps(self, id, finished, ask::resuming(&e, seen).then_some(seen));
        }
        Ok(json!({}))
    }

    fn tick(&self, a: TickArgs) -> Result<Value> {
        let episode = self.db.episode(a.id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
        let show = self.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let now = Instant::now();
        let mut became_watched = false;
        {
            let mut guard = self.playing.lock().map_err(|_| anyhow!("busy"))?;
            if guard.as_ref().is_none_or(|p| p.episode_id != a.id) {
                self.end_sitting(guard.take());
                *guard = Some(Playing { episode_id: a.id, show_id: show.id, position: a.position, playing: false, at: now, saved_at: now, session: None });
            }
            let p = guard.as_mut().expect("just set");
            let gap = now.duration_since(p.at);
            let seconds = if a.playing && p.playing { gap.as_secs_f64().min(3.0) } else { 0.0 };
            // A new sitting after a long pause, so the log's times are real.
            if (a.playing || !a.lines.is_empty()) && (p.session.is_none() || gap > Duration::from_secs(600)) {
                let sitting = Sitting { show_id: show.id, title: &show.title, native: &show.title_native, anilist_id: show.anilist_id, episode: episode.number };
                p.session = self.log.start(&sitting, now_ms(), a.subtitles).ok();
            }
            if let Some(session) = p.session {
                let lines: Vec<(i64, i64)> = a.lines.iter().map(|l| (now_ms(), l.chars)).collect();
                if seconds > 0.0 || !lines.is_empty() {
                    let _ = self.log.extend(session, now_ms(), seconds, &lines, a.subtitles);
                }
            }
            p.position = a.position;
            p.playing = a.playing;
            p.at = now;
            if a.playing {
                if let Ok(mut l) = self.last_play.lock() {
                    *l = Some(now);
                }
            }
            if now.duration_since(p.saved_at) >= Duration::from_secs(5) || !a.playing {
                self.db.save_position(a.id, a.position)?;
                p.saved_at = now;
            }
        }
        let duration = if episode.duration > 0.0 { episode.duration } else { 0.0 };
        let watched_from = episode.ending_at.filter(|e| *e > duration * 0.5).unwrap_or(duration * 0.9);
        if episode.watched_at.is_none() && duration > 0.0 && a.position >= watched_from {
            self.db.set_watched(a.id, true)?;
            became_watched = true;
            self.jobs.poke();
        }
        Ok(json!({ "events": self.events_since(a.since), "watched": became_watched || episode.watched_at.is_some() }))
    }

    fn status(&self) -> Value {
        let mining = self.mining.get().map(|m| m.status());
        let url = cards::anki_url(self);
        let anki = crate::anki::version(&url).is_ok();
        json!({
            "tools": self.tools(),
            "mining": mining,
            "beaconAddress": mining::address(),
            "anki": anki,
            "ankiUrl": url,
            "dataDir": self.dirs.local,
            "mediaDir": self.media_dir(),
            "logPath": self.log.path,
            "spaceUsed": self.space_used(),
            "fields": {
                "sentence": cards::fields(self).sentence,
                "picture": cards::fields(self).picture,
                "audio": cards::fields(self).audio,
                "info": cards::fields(self).info,
            },
        })
    }

    fn set_setting(&self, key: &str, value: &Value) -> Result<Value> {
        self.db.set_setting(key, value)?;
        match key {
            "tools.ffmpeg" | "whisper.model" => self.refind_tools(),
            "jimaku.key" => {
                self.jobs.forget_jimaku();
                self.db.requeue_subs(None, &["nokey", "none", "failed"])?;
                self.jobs.poke();
            }
            "names.enabled" | "names.spoilers" => self.names.poke(),
            _ => self.jobs.poke(),
        }
        Ok(json!({}))
    }

    fn episode_or_err(&self, id: i64) -> Result<Episode> {
        self.db.episode(id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))
    }

    /// Answers one of the UI's requests.
    pub fn call(self: &Arc<Self>, cmd: &str, args: Value) -> Result<Value> {
        match cmd {
            "library" => self.library(),
            "activity" => to_value(self.activity()),
            "show" => self.show_view(arg(&args, "id")?),
            "scan" => self.scan(&arg::<String>(&args, "folder")?),
            "searchAnilist" => {
                let mut found = anilist::search(&arg::<String>(&args, "query")?)?;
                let count = arg::<Option<usize>>(&args, "count")?.unwrap_or(0);
                anilist::rank(&mut found, &arg::<String>(&args, "query")?, count);
                anilist::follow_sequels(&mut found, &arg::<String>(&args, "query")?)?;
                to_value(found)
            }
            "addShow" => self.add_show(&args),
            "rescan" => {
                let added = self.rescan(arg(&args, "id")?)?;
                Ok(json!({ "added": added }))
            }
            "rematch" => self.rematch(arg(&args, "id")?, arg(&args, "anilistId")?),
            "removeShow" => self.remove_show(arg(&args, "id")?),
            "renumber" => {
                let order: Vec<(i64, i64)> = arg(&args, "order")?;
                self.db.renumber(arg(&args, "id")?, &order)?;
                self.jobs.poke();
                self.show_view(arg(&args, "id")?)
            }
            "episode" => self.episode_view(arg(&args, "id")?),
            "openPlayer" => {
                let id = arg(&args, "id")?;
                let view = self.open_player(id)?;
                // Hachidori mines into this show's deck.
                if let Some(show) = self.db.episode(id)?.and_then(|e| self.db.show(e.show_id).ok().flatten()) {
                    own_hachidori::watching(self, &show.title);
                }
                Ok(view)
            }
            "closePlayer" => self.close_player(arg(&args, "id")?, arg(&args, "position")?),
            "tick" => self.tick(serde_json::from_value(args).context("the player’s tick")?),
            "markWatched" => {
                let id: i64 = arg(&args, "id")?;
                self.db.set_watched(id, arg(&args, "watched")?)?;
                self.jobs.poke();
                Ok(json!({}))
            }
            "savePosition" => {
                self.db.save_position(arg(&args, "id")?, arg(&args, "position")?)?;
                Ok(json!({}))
            }
            "events" => to_value(self.events_since(arg::<Option<u64>>(&args, "since")?.unwrap_or(0))),
            "settings" => Ok(Value::Object(self.db.settings()?)),
            "setSetting" => {
                let key: String = arg(&args, "key")?;
                let done = self.set_setting(&key, &args.get("value").cloned().unwrap_or(Value::Null))?;
                // Which cards count as known changed: every share is worked out again.
                if key.starts_with("known.") {
                    premine::start_shares(self, None);
                }
                Ok(done)
            }
            "knownSources" => known::sources(self),
            "status" => Ok(self.status()),
            "findTools" => {
                self.refind_tools();
                Ok(self.status())
            }
            "subtitleFiles" => to_value(jobs::read_tried(self, arg(&args, "id")?)),
            "useSubtitle" => {
                let episode = self.episode_or_err(arg(&args, "id")?)?;
                let path: String = arg(&args, "path")?;
                let name = arg::<Option<String>>(&args, "name")?
                    .unwrap_or_else(|| Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                jobs::use_file(self, &episode, Path::new(&path), &name)?;
                self.episode_view(episode.id)
            }
            "nudge" => {
                let id: i64 = arg(&args, "id")?;
                self.db.set_nudge(id, arg(&args, "ms")?)?;
                Ok(json!({}))
            }
            "lateFromHere" => {
                self.move_lines(arg(&args, "id")?, arg(&args, "from")?, arg(&args, "ms")?)
            }
            "retryMedia" => {
                let id: i64 = arg(&args, "id")?;
                // "Convert this episode": it looked playable but wasn't.
                if arg::<Option<bool>>(&args, "force")?.unwrap_or(false) {
                    self.db.set_setting(&format!("convert.force.{id}"), &Value::Bool(true))?;
                }
                let episode = self.episode_or_err(id)?;
                let _ = std::fs::remove_file(jobs::copy_path(self, episode.show_id, id));
                self.db.media_reset(id)?;
                self.jobs.poke();
                Ok(json!({}))
            }
            "retrySubs" => {
                let show: Option<i64> = arg(&args, "show")?;
                match arg::<Option<i64>>(&args, "id")? {
                    Some(id) => self.db.set_subs_state(id, "pending", "")?,
                    None => {
                        self.db.requeue_subs(show, &["none", "failed", "nokey", "check"])?;
                    }
                }
                self.jobs.forget_jimaku();
                self.jobs.poke();
                Ok(json!({}))
            }
            "freeSpace" => self.free_space(arg(&args, "show")?),
            "setMediaFolder" => self.set_media_folder(arg(&args, "folder")?),
            "whisperSubs" => {
                let n = self.db.queue_whisper(arg(&args, "show")?, arg(&args, "id")?)?;
                self.jobs.poke();
                Ok(json!({ "queued": n }))
            }
            "getWhisperModel" => {
                jobs::get_whisper_model(self.clone());
                Ok(json!({}))
            }
            "setAudio" => self.set_audio(arg(&args, "id")?, arg(&args, "track")?),
            "seekSheet" => self.seek_sheet(arg(&args, "id")?),
            "translation" => to_value(jobs::translation(self, &self.episode_or_err(arg(&args, "id")?)?)?),
            "mineLine" => {
                let job = cards::add_line_to_last_card(self, arg(&args, "id")?, arg(&args, "start")?, arg(&args, "end")?, &arg::<String>(&args, "text")?)?;
                Ok(json!({ "job": job }))
            }
            "retryCard" => {
                cards::retry(self, arg(&args, "job")?)?;
                Ok(json!({}))
            }
            "recentCards" => to_value(self.db.recent_mine_jobs(30)?),
            "claudeStatus" => to_value(crate::claude::status()),
            "explain" => ask::explain(self, arg(&args, "id")?, arg(&args, "index")?),
            "ask" => Ok(self.asks.progress(arg(&args, "ask")?)),
            "stopAsk" => {
                self.asks.stop(arg(&args, "ask")?);
                Ok(json!({}))
            }
            "recap" => ask::recap(self, arg(&args, "id")?, arg::<Option<bool>>(&args, "start")?.unwrap_or(false), arg(&args, "at")?),
            "recapAnswer" => ask::recap_answer(self, arg(&args, "id")?, arg(&args, "ask")?, arg(&args, "at")?),
            "namesStatus" => Ok(names::status(self)),
            "rebuildNames" => {
                // Fetches every cast again.
                let _ = self.db.set_setting("names.built", &Value::Null);
                for show in self.db.shows()? {
                    if let Some(id) = show.anilist_id {
                        let _ = std::fs::remove_file(names::dir(self).join(format!("{id}.json")));
                    }
                }
                self.names.poke();
                Ok(names::status(self))
            }
            "showNames" => {
                let show = self.db.show(arg(&args, "id")?)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
                to_value(names::show_names(self, &show))
            }
            "wordShares" => {
                premine::start_shares(self, arg(&args, "show")?);
                Ok(json!({}))
            }
            "followUp" => ask::follow_up(
                self,
                arg(&args, "id")?,
                arg(&args, "index")?,
                &arg::<String>(&args, "explanation")?,
                &arg::<Vec<ask::Exchange>>(&args, "thread")?,
                &arg::<String>(&args, "question")?,
            ),
            "searchLines" => search::find(self, &arg::<String>(&args, "q")?),
            "scene" => search::scene(self, arg(&args, "id")?, arg(&args, "line")?),
            "premine" => to_value(premine::start_list(self, arg(&args, "show")?)?),
            "premineStatus" => to_value(premine::status(self)),
            "ownHachidori" => to_value(own_hachidori::report(self)),
            "premineCards" => to_value(premine::start_cards(self, arg(&args, "words")?)?),
            "recommendations" => recommend::view(self),
            "anilistSignIn" => Ok(json!({ "user": anilist_list::sign_in(self, &arg::<String>(&args, "token")?)? })),
            "anilistSignOut" => {
                anilist_list::sign_out(self)?;
                Ok(json!({}))
            }
            "hideRecommendation" => {
                recommend::hide(self, arg(&args, "id")?)?;
                Ok(json!({}))
            }
            "ankiFields" => {
                let url = cards::anki_url(self);
                let model: String = arg(&args, "model")?;
                to_value(crate::anki::model_field_names(&url, &model)?)
            }
            _ => Err(anyhow!("Telly doesn’t know the request \"{cmd}\"")),
        }
    }

    /// What Hachidori's engine page posts: its answers to premining's questions.
    pub fn receive(&self, path: &str, body: &[u8]) -> Result<()> {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        match path {
            "/hd/api/answer" => self.own_hachidori.answer(query, body),
            _ => Err(anyhow!("nothing takes {path}")),
        }
    }

    /// A file the player or the library asks for: /ep/<id>, /cover/<show id> or /clip/<name>.
    pub fn serve(&self, path: &str, range: Option<&str>) -> Result<Served> {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let json_reply = |v: Value| Served { bytes: v.to_string().into_bytes(), mime: "application/json", range: None };
        // Hachidori's engine page asks for the character dictionary, your
        // dictionaries, the Anki setup and premining's questions, and says how
        // each went (own_hachidori.rs).
        match path {
            "/names/state" => return Ok(json_reply(names::state(self))),
            "/names/imported" => {
                names::imported(self, query)?;
                return Ok(json_reply(json!({})));
            }
            "/hd/dictionaries/next" => return Ok(json_reply(own_hachidori::next_dictionary(self))),
            "/hd/dictionaries/done" => {
                own_hachidori::dictionary_done(self, query)?;
                return Ok(json_reply(json!({})));
            }
            "/hd/anki" => return Ok(json_reply(own_hachidori::anki_setup(self))),
            "/hd/anki/done" => {
                own_hachidori::anki_done(self, query);
                return Ok(json_reply(json!({})));
            }
            "/hd/api/next" => return Ok(json_reply(self.own_hachidori.next_request(Duration::from_secs(20)))),
            _ => {}
        }
        let decoded = percent_encoding::percent_decode_str(path).decode_utf8_lossy().into_owned();
        let parts: Vec<&str> = decoded.trim_start_matches('/').split('/').collect();
        let file = match parts.as_slice() {
            ["ep", id] => {
                let e = self.episode_or_err(id.parse().map_err(|_| anyhow!("bad episode"))?)?;
                let video = if e.media_state == "ready" && !e.play_path.is_empty() { PathBuf::from(e.play_path) } else { PathBuf::from(e.path) };
                let from = range.and_then(|r| r.trim().strip_prefix("bytes=")).and_then(|r| r.split('-').next()?.trim().parse().ok());
                self.read_ahead(&video, from.unwrap_or(0));
                video
            }
            ["sheet", id] => self.sheet_path(&self.episode_or_err(id.parse().map_err(|_| anyhow!("bad episode"))?)?),
            ["picture", id, n] => return search::picture(self, id.parse().map_err(|_| anyhow!("bad episode"))?, n.parse().map_err(|_| anyhow!("bad picture"))?),
            ["cover", id] => {
                let id = id.parse().map_err(|_| anyhow!("bad show"))?;
                // The cover as fetched, for the moments before its small copy is made.
                Some(self.small_cover_path(id)).filter(|small| small.is_file()).unwrap_or_else(|| self.cover_path(id))
            }
            // A clip's file name alone: on Windows "C:\…" joined to the folder would be anywhere.
            ["clip", name] if !name.contains("..") && !name.contains(['/', '\\', ':']) => self.dirs.clips().join(name),
            ["names", "telly-characters.zip"] => names::zip_path(self),
            ["hd", "dictionaries", "file", name] => own_hachidori::dictionary_file(self, name)?,
            _ => return Err(anyhow!("nothing at {path}")),
        };
        read_range(&file, range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_film_is_what_anilist_calls_one_or_one_long_file_it_doesn_t_know() {
        assert!(is_movie("MOVIE", true, &[6000.0]));
        // A film kept as two versions is still a film.
        assert!(is_movie("MOVIE", true, &[5500.0, 5510.0]));
        // Live action: AniList has nothing, and it's one file of two hours.
        assert!(is_movie("", false, &[7200.0]));
        // One episode of a series, a folder of specials, and a series AniList knows.
        assert!(!is_movie("", false, &[1440.0]));
        assert!(!is_movie("", false, &[6060.0, 660.0, 1440.0]));
        assert!(!is_movie("TV", true, &[7200.0]));
    }

    #[test]
    fn a_removed_show_leaves_nothing_for_the_next_one() {
        let dir = std::env::temp_dir().join(format!("telly-remove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let add = |title: &str| {
            let episodes = vec![crate::db::NewEpisode { number: 1, path: format!("{title}.mkv"), size: 1, duration: 1400.0 }];
            let show = backend.db.add_show(&crate::db::NewShow { folder: title.into(), title: title.into(), episodes, ..Default::default() }).unwrap();
            (show, backend.db.episodes(show).unwrap()[0].id)
        };
        let (kept_show, kept_episode) = add("Kept");
        let (show, episode) = add("Removed");
        for key in [format!("audio.track.{show}"), format!("anilist.group.{show}.Season 2"), format!("jimaku.group.{show}.x"), format!("convert.force.{episode}")] {
            backend.db.set_setting(&key, &json!(1)).unwrap();
        }
        backend.db.set_setting(&format!("audio.track.{kept_show}"), &json!(1)).unwrap();
        let subs = backend.dirs.subs();
        std::fs::create_dir_all(&subs).unwrap();
        for name in [format!("{episode}.json"), format!("{episode}.tried.json"), format!("{episode}.track2.ass"), format!("{episode}.whisper.small.srt"), format!("{kept_episode}.json")] {
            std::fs::write(subs.join(name), b"[]").unwrap();
        }

        backend.remove_show(show).unwrap();
        let (again, again_episode) = add("Added after");
        // Its ids are new, and nothing of the removed show's is left.
        assert!(again > show && again_episode > episode, "{again} {again_episode}");
        let settings = backend.db.settings().unwrap();
        assert!(settings.keys().all(|k| !k.ends_with(&format!(".{show}")) && !k.contains(&format!(".{show}.")) && !k.ends_with(&format!(".{episode}"))), "{settings:?}");
        assert!(settings.contains_key(&format!("audio.track.{kept_show}")));
        let left: Vec<String> = std::fs::read_dir(&subs).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, vec![format!("{kept_episode}.json")]);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn earlier_from_here_moves_the_lines_from_where_the_player_is() {
        let dir = std::env::temp_dir().join(format!("telly-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = vec![crate::db::NewEpisode { number: 1, path: "1.mkv".into(), size: 1, duration: 1400.0 }];
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let id = backend.db.episodes(show).unwrap()[0].id;
        let line = |start: f64| Line { start, end: start + 0.2, text: "せりふ".into(), ruby: vec![], speaker: String::new(), sign: false };
        jobs::write_lines(&backend, id, &[line(9.0), line(10.4), line(12.0)]).unwrap();
        // The subtitles are nudged 1 s later, so the player is at 11.4 where the file says 10.4.
        backend.db.set_nudge(id, 1000).unwrap();
        backend.move_lines(id, 11.4, -500).unwrap();
        backend.move_lines(id, 11.4, -500).unwrap();
        let starts: Vec<f64> = jobs::read_lines(&backend, id).iter().map(|l| (l.start * 10.0).round() / 10.0).collect();
        assert_eq!(starts, vec![9.0, 9.4, 11.0]);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_release_of_an_episode_takes_the_place_of_one_that_s_gone() {
        let dir = std::env::temp_dir().join(format!("telly-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let Some(ffmpeg) = backend.tools().ffmpeg else { return }; // making rips to scan needs ffmpeg
        let folder = dir.join("Show");
        std::fs::create_dir_all(&folder).unwrap();
        let path = |name: &str| folder.join(name).to_string_lossy().into_owned();
        let rip = |name: &str| {
            let made = std::process::Command::new(&ffmpeg)
                .args(["-v", "quiet", "-f", "lavfi", "-i", "color=c=black:s=64x36:r=1", "-t", "400", "-c:v", "mpeg4", "-y"])
                .arg(path(name))
                .status()
                .unwrap();
            assert!(made.success());
        };
        rip("[Sub] Show - 01 [720p].mkv");
        rip("[Sub] Show - 02 [720p].mkv");
        let episodes = (1..=2).map(|n| crate::db::NewEpisode { number: n, path: path(&format!("[Sub] Show - 0{n} [720p].mkv")), size: 1, duration: 400.0 }).collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: folder.to_string_lossy().into_owned(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let two = backend.db.episodes(show).unwrap()[1].clone();
        backend.db.save_position(two.id, 600.0).unwrap();
        std::fs::create_dir_all(backend.dirs.subs()).unwrap();
        std::fs::write(backend.dirs.subs().join(format!("{}.tried.json", two.id)), b"[]").unwrap();

        // Episode 2 is replaced by a better release.
        std::fs::remove_file(path("[Sub] Show - 02 [720p].mkv")).unwrap();
        rip("[BD] Show - 02 [1080p].mkv");
        assert_eq!(backend.rescan(show).unwrap(), 1);
        let after = backend.db.episodes(show).unwrap();
        assert_eq!(after.len(), 2);
        let replaced = &after[1];
        assert_eq!((replaced.id, replaced.path.as_str()), (two.id, path("[BD] Show - 02 [1080p].mkv").as_str()));
        assert_eq!((replaced.media_state.as_str(), replaced.subs_state.as_str(), replaced.position), ("pending", "pending", 600.0));
        assert!(!backend.dirs.subs().join(format!("{}.tried.json", two.id)).exists());
        // A second copy of an episode whose rip is still there is left alone.
        rip("[BD] Show - 01 [1080p].mkv");
        assert_eq!(backend.rescan(show).unwrap(), 0);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_copies_folder_gets_the_seek_bar_pictures_too() {
        let dir = std::env::temp_dir().join(format!("telly-copies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Arc::new(Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        let episodes = vec![crate::db::NewEpisode { number: 1, path: "1.mkv".into(), size: 1, duration: 1400.0 }];
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let id = backend.db.episodes(show).unwrap()[0].id;
        // A rip that plays as it is has no copy, but its pictures are in the copies folder.
        let sheet = jobs::sheet_path(&backend, show, id);
        std::fs::create_dir_all(sheet.parent().unwrap()).unwrap();
        std::fs::write(&sheet, b"jpg").unwrap();
        backend.set_media_folder(Some(dir.join("elsewhere").to_string_lossy().into_owned())).unwrap();
        let moved = jobs::sheet_path(&backend, show, id);
        assert!(moved.starts_with(dir.join("elsewhere")));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !moved.is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(moved.is_file() && !sheet.exists());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_clip_is_served_from_the_clips_folder_only() {
        let dir = std::env::temp_dir().join(format!("telly-clip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        std::fs::create_dir_all(backend.dirs.clips()).unwrap();
        std::fs::write(backend.dirs.clips().join("a.webm"), b"clip").unwrap();
        assert_eq!(backend.serve("/clip/a.webm", None).unwrap().bytes, b"clip");
        for name in ["C:%5CWindows%5Cwin.ini", "..%2Ftelly.sqlite", "%2Fetc%2Fpasswd", "sub%5Ca.webm"] {
            assert!(backend.serve(&format!("/clip/{name}"), None).is_err(), "{name}");
        }
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn serves_ranges() {
        let dir = std::env::temp_dir().join(format!("telly-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.mp4");
        std::fs::write(&path, (0..100u8).collect::<Vec<_>>()).unwrap();
        let s = read_range(&path, Some("bytes=10-19")).unwrap();
        assert_eq!(s.bytes, (10..20u8).collect::<Vec<_>>());
        assert_eq!(s.range, Some((10, 19, 100)));
        let s = read_range(&path, Some("bytes=90-")).unwrap();
        assert_eq!(s.range, Some((90, 99, 100)));
        let s = read_range(&path, None).unwrap();
        assert_eq!(s.bytes.len(), 100);
        let _ = std::fs::remove_dir_all(dir);
    }
}
