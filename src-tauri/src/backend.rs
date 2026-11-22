//! Everything the UI asks for goes through `Backend::call(cmd, args)`, from
//! the app (lib.rs) or from the development server (devserver.rs), so both
//! run the same code. Two parts of it are in modules of their own:
//! `backend/player.rs` (an episode in the player, its sittings and ticks) and
//! `backend/adding.rs` (reading folders, adding shows, looking for new episodes).

use crate::anilist::{self, Anime};
use crate::anilist_list;
use crate::ask::{self, Asks};
use crate::backup;
use crate::cards::{self, Event};
use crate::covers;
use crate::follow;
use crate::db::{Db, Episode, NewShow, Show};
use crate::jobs::{self, Jobs, Task};
use crate::known;
use crate::log::{Log, Sitting};
use crate::media;
use crate::mining::{self, Mining};
use crate::names::{self, Names};
use crate::own_hachidori::{self, Own};
use crate::parts;
use crate::nyaa;
use crate::premine::{self, Premining};
use crate::recommend;
use crate::relink;
use crate::scan;
use crate::search;
use crate::series;
use crate::stats;
use crate::subs::Line;
use crate::sync;
use crate::tmdb;
use crate::tools::{self, Tools};
use crate::torrents::{self, Downloads};
use crate::util::now_ms;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

mod adding;
mod player;

/// Where Telly keeps things. %LOCALAPPDATA%\Telly for the library, copies,
/// subtitles and clips; %APPDATA%\Telly for the log Tidepool reads.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub local: PathBuf,
    pub roaming: PathBuf,
}

/// The folder Telly's copies go in when you choose `chosen` for them: one of
/// its own inside it, as Telly deletes a show's copies by the folder, and a
/// folder you chose may hold your own files too (your rips, even).
pub fn copies_in(chosen: &Path) -> PathBuf {
    if chosen.file_name().is_some_and(|name| name == COPIES) {
        return chosen.to_path_buf();
    }
    chosen.join(COPIES)
}

const COPIES: &str = "Telly copies";
/// Setting: the copies in a folder you chose are in `copies_in` of it (they were in the folder itself once).
const COPIES_APART: &str = "media.apart";

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
/// Setting: whether the first-time setup is done (true), begun (false) or not yet shown (none).
const SETUP_DONE: &str = "setup.done";

/// The episode open in the player.
struct Playing {
    episode_id: i64,
    show_id: i64,
    position: f64,
    playing: bool,
    at: Instant,
    /// When a tick last said it was playing: the player ticks while paused too.
    played_at: Instant,
    saved_at: Instant,
    session: Option<i64>,
    /// The first reads of a resumed episode are for its index and the frame it was left at.
    starting_midway: bool,
    /// Where this sitting began playing (its first tick that plays: the player
    /// starts an episode you've watched from its beginning, wherever it was
    /// left), and whether it has reached the point that makes the episode
    /// watched: a rewatch counts once a sitting, played to.
    from: Option<f64>,
    reached_end: bool,
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
    /// The copies folder, once asked for (see `media_dir`).
    media_folder: RwLock<Option<PathBuf>>,
    /// What Telly is downloading (torrents.rs).
    pub downloads: Downloads,
    /// AniList's entries and the anime each belongs to (series.rs).
    pub series: series::Kept,
    /// The folder Add media last read, so choosing another anime for it doesn't read it again.
    scanned: Mutex<Option<scan::Scan>>,
    /// Held while a show's folder is looked in for new episodes, so two looks
    /// at once (a download finishing, the 10-minute look) don't both add one.
    rescanning: Mutex<()>,
    /// A newer Telly that's out, as GitHub last said (update.rs).
    newer: Mutex<Option<crate::update::Newer>>,
    /// The lengths of videos a look for new episodes read that aren't episodes (`lengths_once`).
    looked_at: Mutex<std::collections::HashMap<PathBuf, (u64, Option<std::time::SystemTime>, f64)>>,
    /// Whether your libraries' folders are being read for shows (library.rs).
    pub adding_libraries: Mutex<bool>,
    /// The libraries' folders to look in again for shows that aren't in Telly yet (library.rs).
    pub library_looks: Mutex<Vec<String>>,
    /// The videos in your libraries' folders that aren't in Telly and aren't looked at again, each with its length (library.rs).
    pub passed_over: Mutex<Option<std::collections::HashMap<String, f64>>>,
    /// The folder Add media's sheet is open on, and since when: the libraries' look leaves it to you.
    sheet: Mutex<Option<(String, std::time::Instant)>>,
    /// Episodes lent to your phone, or got from your PC (phone.rs).
    pub phone: crate::phone::Phone,
    /// Your dictionaries on the phone, open for its lookups (lookups.rs).
    pub lookups: crate::lookups::Lookups,
}

/// A folder Add media read, and what AniList makes of it.
struct Found {
    scan: scan::Scan,
    /// The anime its name finds on AniList, the likeliest first.
    matches: Vec<Anime>,
    /// Its parts, when it holds more than one show (parts.rs).
    parts: Vec<parts::Part>,
    /// Every season, movie and OVA of the anime.
    entries: Vec<Anime>,
    /// The anime its parts are listed under.
    anime: Option<Anime>,
    /// Why AniList couldn't be asked.
    error: Option<String>,
}

/// A show to add as Add shows takes it (`addShows`), with whether its folder holds other parts' rips too.
pub(crate) fn as_read(show: &NewShow, shared: bool) -> Value {
    json!({ "folder": show.folder, "title": show.title, "anilistId": show.anilist_id, "shared": shared, "episodes": show.episodes })
}

impl Found {
    /// The shows the folder is when nothing is changed in Add media's sheet,
    /// each with whether its folder holds other parts' rips too: its parts,
    /// or the folder as one show. What has no episodes isn't one.
    fn shows(&self) -> Vec<(NewShow, bool)> {
        let episodes = |files: &[scan::ScannedFile]| -> Vec<crate::db::NewEpisode> {
            files.iter().filter_map(|f| f.number.map(|number| crate::db::NewEpisode { number, path: f.path.clone(), size: f.size, duration: f.duration })).collect()
        };
        let mut shows: Vec<(NewShow, bool)> = self
            .parts
            .iter()
            .map(|p| {
                let title = if p.anime.is_some() { String::new() } else { p.title.clone() };
                (NewShow { folder: p.folder.clone(), title, anilist_id: p.anime.as_ref().map(|a| a.id), episodes: episodes(&p.files), ..Default::default() }, p.shared)
            })
            .collect();
        if self.parts.is_empty() {
            let mut episodes = episodes(&self.scan.files);
            // A folder with no episodes, but videos left out only for running past two hours, is a film (or one in parts).
            if episodes.is_empty() {
                let long = self.scan.files.iter().filter(|f| f.reason.starts_with("Over 2 hours"));
                episodes = long.zip(1..).map(|(f, number)| crate::db::NewEpisode { number, path: f.path.clone(), size: f.size, duration: f.duration }).collect();
            }
            let title = if self.matches.is_empty() { self.scan.guess.clone() } else { String::new() };
            shows.push((NewShow { folder: self.scan.folder.clone(), title, anilist_id: self.matches.first().map(|a| a.id), episodes, ..Default::default() }, false));
        }
        shows.retain(|(show, _)| !show.episodes.is_empty());
        shows
    }
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

/// One line that just played, for the log: as the player had it.
#[derive(Deserialize, Debug)]
struct Played {
    chars: i64,
    /// When it played (unix ms), and the video's time then.
    #[serde(default)]
    at: Option<i64>,
    #[serde(default)]
    position: Option<f64>,
    /// Its place among the episode's lines, its times in the file and what it said.
    #[serde(default)]
    line: Option<i64>,
    #[serde(default)]
    start: f64,
    #[serde(default)]
    end: f64,
    #[serde(default)]
    text: String,
    #[serde(default)]
    speaker: String,
    #[serde(default)]
    ruby: Value,
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
    #[serde(default)]
    rate: Option<f64>,
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
    /// The anime it's listed under in the Library, with its other seasons and movies.
    pub series: series::Place,
    /// The library its folder is in, by its place among your libraries; none for a show kept outside them all.
    pub library: Option<usize>,
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
    /// How the downloads are going, while any are.
    pub downloading: Option<torrents::Summary>,
    /// Downloads finished that wait for you to add them (Processing offers
    /// them), and downloads stopped on an error: the title bar says so.
    pub downloads_to_add: usize,
    pub downloads_stopped: usize,
    /// A newer Telly that's out (update.rs).
    pub newer: Option<crate::update::Newer>,
}

/// What the title bar's list shows: what each worker is doing, and the work
/// waiting, in the order the workers will take it.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    pub now: Vec<Task>,
    pub waiting: Vec<Waiting>,
}

/// Work of one kind waiting.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Waiting {
    /// "media", "subs", "whisper", "catch up" or "words".
    pub kind: String,
    /// What the work is: "Finding subtitles".
    pub what: String,
    pub count: usize,
    /// The first few, the next to be taken first.
    pub first: Vec<Queued>,
}

/// An episode waiting.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Queued {
    pub show_id: i64,
    pub episode_id: i64,
    pub title: String,
    pub number: i64,
    /// How many episodes its show has: a film's one goes by the film's title.
    pub episodes: usize,
    /// What's done to it next, where that's worth saying: "To convert".
    pub step: String,
}

/// How many of each kind of work waiting the title bar's list names.
const QUEUE_SHOWN: usize = 10;

/// What's done next to an episode waiting for its video.
fn video_step(e: &Episode) -> &'static str {
    match e.media_plan.as_str() {
        // A rip nobody has looked at yet: it may play as it is.
        "" => "To read",
        "remux" | "audio" => "To copy",
        _ => "To convert",
    }
}

fn arg<T: for<'de> Deserialize<'de>>(args: &Value, key: &str) -> Result<T> {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null)).with_context(|| format!("the \"{key}\" argument"))
}

fn to_value<T: Serialize>(v: T) -> Result<Value> {
    Ok(serde_json::to_value(v)?)
}

/// Whether a show is a film: AniList says so (`format`), or AniList doesn't
/// know it (`matched` is false) and it's one file, an hour or more long.
pub(crate) fn is_movie(format: &str, matched: bool, lengths: &[f64]) -> bool {
    format == "MOVIE" || (!matched && lengths.len() == 1 && lengths[0] >= 3600.0)
}

/// The setting that says a show's folder holds other shows' rips too, so Look for new episodes adds none to it.
pub(crate) fn shared_setting(show_id: i64) -> String {
    format!("folder.shared.{show_id}")
}

/// The setting holding the files of a show's folder you left out when you
/// added it, by their paths in its folder (so they're the same once it's moved).
fn left_out_setting(show_id: i64) -> String {
    format!("folder.left.{show_id}")
}

/// A file's path in a show's folder ("Season 2/03.mkv"), if it's in it.
fn in_folder(folder: &str, path: &str) -> Option<String> {
    Path::new(path).strip_prefix(folder).ok().map(|p| p.to_string_lossy().replace('\\', "/"))
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

/// A page's answer with a tag that stands for it, or {"unchanged": true}
/// when the page has that answer already (it sends its tag): a show's page
/// asks every few seconds, and a show of hundreds of episodes is a big answer.
fn tagged(mut answer: Value, args: &Value) -> Value {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    answer.to_string().hash(&mut hasher);
    let tag = format!("{:016x}", hasher.finish());
    if args.get("tag").and_then(Value::as_str) == Some(tag.as_str()) {
        return json!({ "unchanged": true });
    }
    if let Some(fields) = answer.as_object_mut() {
        fields.insert("tag".into(), json!(tag));
    }
    answer
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
        // A backup that Restore unpacked takes the library's place before anything opens it.
        let restored = backup::finish_restore(&dirs);
        let db = Db::open(&dirs.local.join("telly.sqlite"))?;
        // For the page to say once: what was restored, or why it wasn't.
        match restored {
            Ok(Some(made)) => {
                let _ = db.set_setting(backup::RESTORED, &json!(made));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = db.set_setting(backup::NOT_RESTORED, &json!(format!("{e:#}")));
            }
        }
        let log = Log::open(&dirs.roaming.join("telly-log.sqlite"))?;
        let _ = log.tidy();
        let lines = search::Index::open(&dirs.local.join("telly-lines.sqlite"))?;
        // Failed subtitles get another go each time Telly starts (Jimaku may
        // have been down), but not a show whose folder isn't there: its drive
        // may be unplugged, and they'd only fail again.
        for show in db.shows().unwrap_or_default() {
            if Path::new(&show.folder).is_dir() {
                let _ = db.requeue_subs(Some(show.id), &["failed"]);
            }
        }
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
        // A library from before the first-time setup was set up without it, so
        // it doesn't go through it. One that's begun the setup has it false.
        if db.setting(SETUP_DONE)?.is_none() && (!db.shows()?.is_empty() || !db.setting_str("jimaku.key").trim().is_empty()) {
            let _ = db.set_setting(SETUP_DONE, &json!(true));
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
            media_folder: RwLock::new(None),
            downloads: Downloads::default(),
            series: series::Kept::default(),
            scanned: Mutex::new(None),
            rescanning: Mutex::new(()),
            newer: Mutex::new(None),
            looked_at: Mutex::new(std::collections::HashMap::new()),
            adding_libraries: Mutex::new(false),
            library_looks: Mutex::new(Vec::new()),
            passed_over: Mutex::new(None),
            sheet: Mutex::new(None),
            phone: Default::default(),
            lookups: Default::default(),
        })
    }

    /// Starts the background work: getting shows ready and finishing cards.
    pub fn start(self: &Arc<Self>) {
        // The phone plays what it got from the PC: the PC does the rest.
        #[cfg(target_os = "android")]
        {
            crate::phone::start_getting(self.clone());
            crate::lookups::start(self.clone());
            search::start(self.clone());
            crate::update::start(self.clone());
        }
        #[cfg(not(target_os = "android"))]
        let backend = self.clone();
        // The work starts once ffmpeg has said what it can do, so none of it sees half an answer.
        #[cfg(not(target_os = "android"))]
        std::thread::spawn(move || {
            backend.refind_tools();
            jobs::clear_parts(&backend);
            backend.forget_removed_files();
            jobs::start(backend.clone());
            let for_cards = backend.clone();
            std::thread::Builder::new().name("telly-cards".into()).spawn(move || cards::worker(&for_cards)).ok();
            torrents::start(backend.clone());
            follow::start(backend.clone());
            crate::update::start(backend.clone());
            backend.watch_folders();
            backend.fill_covers();
            series::gather(&backend);
            recommend::gather(&backend);
            premine::start_shares(&backend, None);
            premine::start_daily_shares(&backend);
            names::start(backend.clone());
            anilist_list::start(backend.clone());
            search::start(backend.clone());
            crate::phone::start_sharing(backend.clone());
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

    /// Where Telly's copies of episodes go: a folder of Telly's own
    /// (`copies_in`) in the one Settings chose ("media.folder", on an external
    /// drive, say), else Telly's own. Kept once asked for: the workers ask
    /// for it for each episode they rank, thousands of times a pass, which
    /// took a database query each.
    pub fn media_dir(&self) -> PathBuf {
        if let Some(dir) = self.media_folder.read().ok().and_then(|d| d.clone()) {
            return dir;
        }
        let mut kept = self.media_folder.write().unwrap_or_else(|e| e.into_inner());
        let dir = match self.db.setting_str("media.folder") {
            folder if folder.is_empty() => self.dirs.media(),
            folder => copies_in(Path::new(&folder)),
        };
        *kept = Some(dir.clone());
        dir
    }

    /// The newer Telly that's out, if GitHub has said there's one.
    pub fn newer(&self) -> Option<crate::update::Newer> {
        self.newer.lock().ok().and_then(|n| n.clone())
    }

    /// Keeps what GitHub said of a newer Telly, for the title bar and Settings.
    pub fn set_newer(&self, newer: Option<crate::update::Newer>) {
        *self.newer.lock().unwrap_or_else(|e| e.into_inner()) = newer;
    }

    /// Moves copies made in the very folder you chose into Telly's own folder
    /// inside it, once: Telly used to keep them in the folder itself.
    pub fn set_copies_apart(self: &Arc<Self>) {
        let chosen = self.db.setting_str("media.folder");
        if chosen.is_empty() || self.db.setting_bool(COPIES_APART, false) {
            return;
        }
        if self.db.set_setting(COPIES_APART, &json!(true)).is_ok() && PathBuf::from(&chosen) != self.media_dir() {
            jobs::move_copies(self.clone(), PathBuf::from(chosen));
        }
    }

    /// Sets the copies folder, and forgets the one kept.
    fn set_media_dir(&self, folder: &Value) -> Result<()> {
        let mut kept = self.media_folder.write().unwrap_or_else(|e| e.into_inner());
        self.db.set_setting("media.folder", folder)?;
        *kept = None;
        Ok(())
    }

    /// Chooses the copies folder (None: Telly's own again) and moves the copies there.
    fn set_media_folder(self: &Arc<Self>, folder: Option<String>) -> Result<Value> {
        let folder = folder.map(|f| f.trim().to_string()).filter(|f| !f.is_empty());
        if let Some(folder) = &folder {
            std::fs::create_dir_all(folder).map_err(|e| anyhow!("Telly can’t use {folder}: {e}. Choose another folder."))?;
        }
        let before = self.media_dir();
        forget_space_used();
        self.set_media_dir(&folder.map(Value::String).unwrap_or(Value::Null))?;
        self.db.set_setting(COPIES_APART, &json!(true))?;
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

    /// Looks a show AniList doesn't know up on TMDB, once: a Japanese drama or
    /// film whose name is much like the show's becomes what it is, with its
    /// titles and poster (tmdb.rs). Returns its poster's address when it's found.
    fn find_on_tmdb(&self, show_id: i64) -> Option<String> {
        let show = self.db.show(show_id).ok()??;
        let asked = format!("tmdb.asked.{show_id}");
        if show.anilist_id.is_some() || show.tmdb_id.is_some() || self.db.setting(&asked).ok()?.is_some() {
            return None;
        }
        let count = self.db.episodes(show_id).map(|e| e.len()).unwrap_or(0);
        // TMDB can't be asked now: it's asked again the next time Telly starts.
        let found = tmdb::find(&show.title, count).ok()?;
        let _ = self.db.set_setting(&asked, &Value::Bool(true));
        let title = found?;
        self.db.set_show_tmdb(show_id, &title).ok()?;
        self.jobs.forget_jimaku();
        // Subtitles were looked for, and maybe queued to be made, under its folder's name alone.
        let _ = self.db.requeue_subs(Some(show_id), &["none", "nokey", "whisper"]);
        self.jobs.poke();
        Some(title.cover)
    }

    /// Gets a show its cover: AniList's or TMDB's (`url`) when it has one;
    /// else TMDB's poster, for a Japanese drama or film it finds of that
    /// name; else the poster from Wikipedia's article about a film or series
    /// of that name; else a frame from its first episode.
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
        if let Some(poster) = self.find_on_tmdb(show_id).filter(|p| !p.is_empty()) {
            if anilist::download_cover(&poster, &path).is_ok() {
                framed(false);
                return;
            }
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
                    // A show added before Telly asked TMDB, AniList not knowing it, is asked about once.
                    if let Some(poster) = backend.find_on_tmdb(show.id).filter(|p| !p.is_empty()) {
                        backend.remove_cover(show.id);
                        backend.get_cover(show.id, &poster);
                    } else if !backend.cover_path(show.id).is_file() {
                        backend.get_cover(show.id, &show.cover_url);
                    } else if !backend.small_cover_path(show.id).is_file() {
                        let _ = covers::make_small(&backend.cover_path(show.id), &backend.small_cover_path(show.id));
                    }
                }
            })
            .ok();
    }

    /// A show's card, from its episodes (in order, as `Db::episodes` has them).
    pub(crate) fn card(&self, show: Show, episodes: &[Episode], cards_by_show: &std::collections::HashMap<i64, i64>, scores: &std::collections::HashMap<i64, (i64, i64, String)>) -> Result<ShowCard> {
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
            // On its own, until the Library says which anime it's part of (`library`).
            series: series::Place { id: -show.id, title: show.title.clone(), native: show.title_native.clone(), order: 0, parts: 1, next: None },
            // Which library it's in is the Library's to say too.
            library: None,
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

    /// What the workers are doing now, for the title bar.
    fn tasks(&self) -> Vec<Task> {
        let mut tasks = self.jobs.tasks();
        tasks.extend(self.names.task.lock().ok().and_then(|t| t.clone()));
        tasks
    }

    fn activity(&self) -> Activity {
        let mut activity = Activity {
            tasks: self.tasks(),
            downloading: self.downloads.summary(),
            downloads_to_add: torrents::offers(self).iter().filter(|o| !o.downloading).count(),
            downloads_stopped: self.downloads.stopped(),
            newer: self.newer.lock().ok().and_then(|n| n.clone()),
            ..Default::default()
        };
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

    /// What each worker is doing and what's waiting, for the title bar's
    /// list. The library is read once for all of it, and what's waiting comes
    /// from the ranking the workers take their work by (`jobs::waiting`).
    fn queue(&self) -> Queue {
        let mut now = self.tasks();
        now.extend(self.jobs.working_out.lock().ok().and_then(|t| t.clone()));
        now.extend(premine::shares_task(self));
        let library = jobs::library(self);
        let queued = |show: &Show, e: &Episode, episodes: usize, step: &str| Queued {
            show_id: show.id,
            episode_id: e.id,
            title: show.title.clone(),
            number: e.number,
            episodes,
            step: step.into(),
        };
        let maker = jobs::listener(self, &self.tools()).map_or("Whisper", |l| l.name());
        let kinds = [
            ("media", "Getting the video ready".to_string()),
            ("subs", "Finding subtitles".into()),
            ("whisper", format!("Making subtitles with {maker}")),
            ("catch up", "Catching up the seek bar’s pictures and English lines".into()),
        ];
        let mut waiting: Vec<Waiting> = kinds
            .into_iter()
            .map(|(kind, what)| {
                let found = jobs::waiting(self, kind, &library);
                let first = found.iter().take(QUEUE_SHOWN).map(|&(show, e, n)| queued(show, e, n, if kind == "media" { video_step(e) } else { "" })).collect();
                Waiting { kind: kind.into(), what, count: found.len(), first }
            })
            .collect();
        // The words to work out, first in first.
        let words = self.jobs.words_waiting();
        if !words.is_empty() {
            let by_id: std::collections::HashMap<i64, (&Show, &Episode, usize)> =
                library.iter().flat_map(|(show, episodes)| episodes.iter().map(move |e| (e.id, (show, e, episodes.len())))).collect();
            let found: Vec<_> = words.iter().filter_map(|id| by_id.get(id)).collect();
            let first = found.iter().take(QUEUE_SHOWN).map(|&&(show, e, n)| queued(show, e, n, "")).collect();
            waiting.push(Waiting { kind: "words".into(), what: "Working out the words".into(), count: found.len(), first });
        }
        waiting.retain(|w| w.count > 0);
        Queue { now, waiting }
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
        let mut places = series::places(&series::graph(self), &shows, &self.db.settings()?);
        // Your libraries, for the Library's Filter to leave one out.
        let libraries = crate::library::libraries(self).unwrap_or_default();
        let mut cards = Vec::with_capacity(shows.len());
        for s in shows {
            let episodes = by_show.remove(&s.id).unwrap_or_default();
            let place = places.remove(&s.id);
            let mut card = self.card(s, &episodes, &cards_by_show, &scores)?;
            card.series = place.unwrap_or(card.series);
            card.library = crate::library::of(&libraries, &card.show.folder);
            cards.push(card);
        }
        let libraries: Vec<&str> = libraries.iter().map(|l| l.name.as_str()).collect();
        Ok(json!({ "shows": cards, "libraries": libraries, "activity": self.activity(), "setup": self.setup() }))
    }

    /// What AniList (or TMDB, for a show AniList doesn't know) says a show is
    /// about, asked for once per entry and kept ("" for a show without one).
    fn show_about(&self, id: i64) -> Result<String> {
        let show = self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        if let (None, Some(tmdb_id)) = (show.anilist_id, &show.tmdb_id) {
            let key = format!("tmdb.about.{tmdb_id}");
            if let Some(Value::String(text)) = self.db.setting(&key)? {
                return Ok(text);
            }
            let text = tmdb::description(tmdb_id)?;
            self.db.set_setting(&key, &Value::String(text.clone()))?;
            return Ok(text);
        }
        let Some(anilist_id) = show.anilist_id else { return Ok(String::new()) };
        let key = format!("anilist.about.{anilist_id}");
        if let Some(Value::String(text)) = self.db.setting(&key)? {
            return Ok(text);
        }
        let text = anilist::description(anilist_id)?;
        self.db.set_setting(&key, &Value::String(text.clone()))?;
        Ok(text)
    }

    fn show_view(&self, id: i64) -> Result<Value> {
        let show = self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let cards_by_show = self.db.cards_per_show().unwrap_or_default();
        let episodes = self.db.episodes(id)?;
        let next_id = jobs::next_episode(&episodes).map(|e| e.id);
        // Find its folder… is offered when its rips aren't where Telly has them.
        let rips_missing = relink::missing(&show, &episodes);
        let mut card = self.card(show, &episodes, &cards_by_show, &self.db.word_scores().unwrap_or_default())?;
        card.series = series::places(&series::graph(self), &self.db.shows()?, &self.db.settings()?).remove(&id).unwrap_or(card.series);
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
        Ok(json!({ "card": card, "episodes": rows, "activity": self.activity(), "whisper": whisper, "wordsError": words_error, "completion": self.db.show_completion(id)?, "ripsMissing": rips_missing, "seriesMoved": self.db.setting(&format!("series.of.{id}"))?.is_some(), "following": self.db.setting_bool(&follow::setting(id), false) }))
    }

    /// Files in subtitles of episodes no longer in Telly: a worker may write
    /// one just after its show is removed.
    fn forget_removed_files(&self) {
        let Ok(here) = self.db.all_episodes() else { return };
        let here: std::collections::HashSet<i64> = here.iter().map(|e| e.id).collect();
        for entry in std::fs::read_dir(self.dirs.subs()).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.split('.').next().and_then(|id| id.parse::<i64>().ok());
            if id.is_some_and(|id| !here.contains(&id)) && entry.path().is_file() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Removes everything kept in subtitles by these episodes' ids: their
    /// lines, translation, words, the files tried, the rip's timing, its text
    /// tracks and Whisper's transcripts.
    fn forget_episode_files(&self, ids: &[i64]) {
        let prefixes: Vec<String> = ids.iter().map(|id| format!("{id}.")).collect();
        for entry in std::fs::read_dir(self.dirs.subs()).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefixes.iter().any(|prefix| name.starts_with(prefix.as_str())) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
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

    pub(crate) fn remove_show(&self, id: i64) -> Result<Value> {
        self.jobs.cancel_show(id);
        let episodes = self.db.episodes(id)?;
        let removed = self.db.show(id)?;
        let kept = match removed.clone() {
            Some(show) => self.kept_for_stats(show, &episodes).ok(),
            None => None,
        };
        // Its rips in another show's folder (a season of its own inside the
        // first's) don't become that show's episodes once it's gone.
        if let Some(removed) = &removed {
            let rips: Vec<String> = episodes.iter().map(|e| e.path.clone()).collect();
            for other in self.db.shows()?.iter().filter(|s| s.id != id && s.folder != removed.folder) {
                if rips.iter().any(|p| Path::new(p).starts_with(&other.folder)) {
                    self.leave_out(other.id, &other.folder, &rips);
                }
            }
        }
        self.db.remove_show(id, kept.as_ref().map(|(card, scores)| (card.as_str(), scores.as_str())))?;
        // Only Telly's own files go; your rips stay where they are.
        let _ = std::fs::remove_dir_all(self.media_dir().join(id.to_string()));
        self.forget_episode_files(&episodes.iter().map(|e| e.id).collect::<Vec<_>>());
        for e in &episodes {
            let _ = self.lines.forget(e.id);
        }
        self.remove_cover(id);
        Ok(json!({}))
    }

    /// What Stats keeps of a show being removed, so removing it changes nothing
    /// there: its card (with its cover, kept apart, when it's among the shows
    /// finished) and its episodes' shares of known words, by number.
    fn kept_for_stats(&self, show: Show, episodes: &[Episode]) -> Result<(String, String)> {
        let id = show.id;
        let scores = self.db.word_scores().unwrap_or_default();
        let mut card = self.card(show, episodes, &self.db.cards_per_show().unwrap_or_default(), &scores)?;
        card.cover = String::new();
        if self.db.show_completion(id)?.finished_at.is_some() {
            if let Some(cover) = [self.small_cover_path(id), self.cover_path(id)].into_iter().find(|p| p.is_file()) {
                std::fs::create_dir_all(self.removed_cover_path(id).parent().unwrap_or(Path::new(".")))?;
                if std::fs::copy(&cover, self.removed_cover_path(id)).is_ok() {
                    card.cover = format!("/removed-cover/{id}");
                }
            }
        }
        let by_number: Vec<(i64, i64, i64)> = episodes.iter().filter_map(|e| scores.get(&e.id).map(|s| (e.number, s.0, s.1))).collect();
        Ok((serde_json::to_string(&card)?, serde_json::to_string(&by_number)?))
    }

    /// The cover Stats keeps of a show that was removed.
    fn removed_cover_path(&self, show_id: i64) -> PathBuf {
        self.dirs.covers().join("removed").join(format!("{show_id}.jpg"))
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
            "version": env!("CARGO_PKG_VERSION"),
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
        if key == "media.folder" {
            self.set_media_dir(value)?;
        } else {
            self.db.set_setting(key, value)?;
        }
        match key {
            "tools.ffmpeg" | "whisper.model" => self.refind_tools(),
            "jimaku.key" => {
                self.jobs.forget_jimaku();
                self.db.requeue_subs(None, &["nokey", "none", "failed"])?;
                // And those being worked on without it.
                self.jobs.find_again(&self.db, None);
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
            "library" => self.library().map(|v| tagged(v, &args)),
            "activity" => to_value(self.activity()),
            "queue" => to_value(self.queue()),
            "show" => self.show_view(arg(&args, "id")?).map(|v| tagged(v, &args)),
            "showAbout" => self.show_about(arg(&args, "id")?).map(|text| json!({ "text": text })),
            "scan" => self.scan(&arg::<String>(&args, "folder")?, arg(&args, "anilistId")?, arg::<Option<bool>>(&args, "sure")?.unwrap_or(false)),
            "chosenFolder" => crate::library::chosen(self, &arg::<String>(&args, "folder")?),
            "scanFilm" => crate::library::film(self, &arg::<String>(&args, "folder")?, &arg::<String>(&args, "path")?),
            "setupLibraries" => {
                // The folders chosen in the first-time setup become libraries, and what was unticked there stays out.
                #[derive(Deserialize)]
                struct Folder {
                    folder: String,
                    films: bool,
                }
                #[derive(Deserialize)]
                struct Video {
                    path: String,
                    duration: f64,
                }
                let (folders, out): (Vec<Folder>, Vec<Video>) = (arg(&args, "folders")?, arg(&args, "out")?);
                let chosen: Vec<(String, bool)> = folders.into_iter().map(|f| (f.folder, f.films)).collect();
                crate::library::take_chosen(self, &chosen, &out.into_iter().map(|v| (v.path, v.duration)).collect::<Vec<_>>())?;
                Ok(json!({}))
            }
            "searchAnilist" => {
                let mut found = anilist::search_loosely(&arg::<String>(&args, "query")?)?;
                let count = arg::<Option<usize>>(&args, "count")?.unwrap_or(0);
                anilist::rank(&mut found, &arg::<String>(&args, "query")?, count);
                anilist::follow_sequels(&mut found, &arg::<String>(&args, "query")?)?;
                to_value(found)
            }
            "addShow" => self.add_show(&args),
            "addShows" => self.add_shows(&args),
            "rescan" => {
                let added = self.rescan(arg(&args, "id")?)?;
                Ok(json!({ "added": added }))
            }
            "searchTmdb" => {
                let count = arg::<Option<usize>>(&args, "count")?.unwrap_or(0);
                to_value(tmdb::search(&arg::<String>(&args, "query")?, count)?)
            }
            "rematch" => match arg::<Option<String>>(&args, "tmdbId")? {
                Some(tmdb_id) => self.rematch_tmdb(arg(&args, "id")?, &tmdb_id),
                None => self.rematch(arg(&args, "id")?, arg(&args, "anilistId")?),
            },
            "missingRips" => to_value(relink::missing_shows(self)?),
            "relinkShow" => {
                let apply = arg::<Option<bool>>(&args, "apply")?.unwrap_or(false);
                to_value(relink::show(self, arg(&args, "id")?, &arg::<String>(&args, "folder")?, apply)?)
            }
            "relinkLibrary" => {
                let apply = arg::<Option<bool>>(&args, "apply")?.unwrap_or(false);
                to_value(relink::library(self, &arg::<String>(&args, "from")?, &arg::<String>(&args, "to")?, apply)?)
            }
            "backUp" => backup::back_up(self, &arg::<String>(&args, "path")?),
            "readBackup" => backup::read(self, &arg::<String>(&args, "path")?),
            "restoreBackup" => backup::stage(self, &arg::<String>(&args, "path")?),
            "removeShow" => {
                let id: i64 = arg(&args, "id")?;
                // What you remove stays out of Telly, though its rips stay in a library's folder.
                let rips: Vec<(String, f64)> = self.db.episodes(id)?.into_iter().map(|e| (e.path, e.duration)).collect();
                let done = self.remove_show(id)?;
                crate::library::pass_over(self, &rips);
                Ok(done)
            }
            "moveShow" => {
                series::move_show(self, arg(&args, "id")?, arg(&args, "to")?)?;
                Ok(json!({}))
            }
            "renumber" => {
                let order: Vec<(i64, i64)> = arg(&args, "order")?;
                self.db.renumber(arg(&args, "id")?, &order)?;
                self.jobs.poke();
                self.show_view(arg(&args, "id")?)
            }
            "episode" => self.episode_view(arg(&args, "id")?),
            "openPlayer" => {
                let id = arg(&args, "id")?;
                let mut view = self.open_player(id)?;
                // The player shows what happens to cards from now on, not what happened before it opened.
                view["lastEvent"] = json!(self.events.lock().map(|e| e.0).unwrap_or(0));
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
                let libraries = if key == "libraries" { crate::library::libraries(self)? } else { Vec::new() };
                let done = self.set_setting(&key, &args.get("value").cloned().unwrap_or(Value::Null))?;
                // Which cards count as known changed: every share is worked out again.
                if key.starts_with("known.") {
                    premine::start_shares(self, None);
                }
                // A folder new to your libraries is read for shows.
                if key == "libraries" {
                    crate::library::saved(self, &libraries)?;
                }
                if key.starts_with("downloads.") {
                    torrents::setting_changed(self, &key);
                }
                Ok(done)
            }
            "knownSources" => known::sources(self),
            "status" => Ok(self.status()),
            "installUpdate" => crate::update::install(self.clone()),
            "newer" => to_value(self.newer()),
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
                self.jobs.ask_for(id);
                Ok(json!({}))
            }
            "retrySubs" => {
                let show: Option<i64> = arg(&args, "show")?;
                match arg::<Option<i64>>(&args, "id")? {
                    // One being found now would get a second worker.
                    Some(id) if self.db.episode(id)?.is_some_and(|e| e.subs_state == "working") => {
                        return Err(anyhow!("Telly is finding this episode’s subtitles right now. Ask again once it’s done."));
                    }
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
            "claudeStatus" => Ok(ask::status(self)),
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
            "explainMore" => ask::explain_more(
                self,
                arg(&args, "id")?,
                arg(&args, "index")?,
                &arg::<String>(&args, "explanation")?,
                &arg::<String>(&args, "kind")?,
            ),
            "searchLines" => search::find(self, &arg::<String>(&args, "q")?),
            "searchShows" => search::matching_shows(self, &arg::<String>(&args, "q")?),
            "scene" => search::scene(self, arg(&args, "id")?, arg(&args, "line")?),
            "premine" => match arg::<Option<i64>>(&args, "anime")? {
                Some(key) => to_value(premine::start_anime_list(self, key)?),
                None => to_value(premine::start_list(self, arg(&args, "show")?)?),
            },
            "premineStatus" => to_value(premine::status(self)),
            "ownHachidori" => to_value(own_hachidori::report(self)),
            "premineCards" => to_value(premine::start_cards(self, arg(&args, "words")?)?),
            "recommendations" => recommend::view(self),
            "searchPicks" => to_value(recommend::search(self, &arg::<String>(&args, "q")?)?),
            "chartPage" => recommend::chart_page(self, &arg::<String>(&args, "key")?, arg::<Option<u32>>(&args, "page")?.unwrap_or(1)),
            "stats" => stats::overview(self),
            "showStats" => stats::show(self, arg(&args, "id")?),
            "anilistSignIn" => Ok(json!({ "user": anilist_list::sign_in(self, &arg::<String>(&args, "token")?)? })),
            "anilistSignOut" => {
                anilist_list::sign_out(self)?;
                Ok(json!({}))
            }
            "hideRecommendation" => {
                recommend::hide(self, recommend::Verdict { id: arg(&args, "id")?, title: arg(&args, "title")?, verdict: arg(&args, "verdict")? })?;
                Ok(json!({}))
            }
            "unhideRecommendation" => {
                recommend::unhide(self, arg(&args, "id")?)?;
                Ok(json!({}))
            }
            "ankiFields" => {
                let url = cards::anki_url(self);
                let model: String = arg(&args, "model")?;
                to_value(crate::anki::model_field_names(&url, &model)?)
            }
            "ankiNames" => {
                let (decks, note_types) = crate::anki::deck_and_model_names(&cards::anki_url(self))?;
                Ok(json!({ "decks": decks, "noteTypes": note_types }))
            }
            "checkJimaku" => {
                crate::jimaku::check_key(&self.db.setting_str("jimaku.key"))?;
                Ok(json!({}))
            }
            "checkOpenRouter" => {
                crate::openrouter::check_key(&self.db.setting_str("openrouter.key"))?;
                Ok(json!({}))
            }
            "testClaude" => Ok(ask::test(self)),
            "searchNyaa" => to_value(nyaa::search(
                &self.db.setting_str("nyaa.site"),
                &arg::<String>(&args, "q")?,
                &arg::<Option<String>>(&args, "category")?.unwrap_or_default(),
                arg::<Option<bool>>(&args, "trusted")?.unwrap_or(false),
                &arg::<Option<String>>(&args, "sort")?.unwrap_or_default(),
                arg::<Option<u32>>(&args, "page")?.unwrap_or(1),
            )?),
            "everyEpisode" => {
                // Without AniList's count, the episodes nyaa has from the first on are taken to be all there is.
                let wanted = match arg::<Option<i64>>(&args, "anilistId")? {
                    Some(id) => {
                        let out = crate::anilist::episodes_out(id).ok().flatten();
                        crate::series::around(self, id).map_or_else(|_| nyaa::Wanted::alone(out), |graph| nyaa::wanted(&graph, id, out))
                    }
                    None => nyaa::Wanted::alone(None),
                };
                to_value(nyaa::every_episode(
                    &self.db.setting_str("nyaa.site"),
                    &arg::<String>(&args, "q")?,
                    &arg::<Option<String>>(&args, "category")?.unwrap_or_default(),
                    arg::<Option<bool>>(&args, "trusted")?.unwrap_or(false),
                    &wanted,
                )?)
            }
            "placeDownload" => {
                let torrent = arg::<Option<String>>(&args, "torrent")?.unwrap_or_default();
                torrents::plan(self, &arg::<String>(&args, "title")?, &torrent, arg(&args, "show")?, arg(&args, "anilistId")?, arg(&args, "library")?)
            }
            "download" => {
                let torrent = arg::<Option<String>>(&args, "torrent")?.unwrap_or_default();
                let folder = torrents::add(self, &arg::<String>(&args, "magnet")?, &torrent, &arg::<String>(&args, "title")?, arg(&args, "show")?, arg(&args, "anilistId")?, arg(&args, "library")?)?;
                Ok(json!({ "folder": folder }))
            }
            "follow" => follow::set(self, arg(&args, "show")?, arg(&args, "on")?),
            "downloads" => Ok(torrents::view(self)),
            "connections" => Ok(json!(torrents::connections().into_iter().map(|(name, connected)| json!({ "name": name, "connected": connected })).collect::<Vec<_>>())),
            "libraries" => crate::library::view(self),
            "showsLeaving" => {
                let now: Vec<crate::library::Library> = arg(&args, "libraries")?;
                Ok(json!(crate::library::leaving(self, &crate::library::libraries(self)?, &now)?.len()))
            }
            "lookInLibrary" => {
                crate::library::look_now(self, arg(&args, "at")?)?;
                crate::library::view(self)
            }
            "phoneSharing" => Ok(crate::phone::sharing_state(self)),
            "setPhoneSharing" => crate::phone::set_sharing(self, arg(&args, "on")?),
            "forgetPhones" => crate::phone::forget_phones(self),
            "phoneState" => Ok(crate::phone::state(self)),
            "phoneFind" => crate::phone::find(),
            "phonePair" => crate::phone::pair_with(self, &arg::<String>(&args, "address")?, &arg::<String>(&args, "code")?),
            "phoneUnpair" => crate::phone::unpair(self),
            "phonePcLibrary" => crate::phone::pc_library(self),
            "phoneGet" => {
                let got = crate::phone::get(self, &arg::<Vec<i64>>(&args, "ids")?, arg(&args, "plays")?);
                crate::lookups::start(self.clone());
                got
            }
            "phoneRemove" => crate::phone::remove(self, &arg::<Vec<i64>>(&args, "ids")?),
            "phoneSync" => {
                // Whatever dictionaries it got are installed once it's done.
                let synced = crate::phone::sync(self);
                crate::lookups::start(self.clone());
                synced
            }
            "phoneLibrary" => crate::phone::phone_library(self),
            "phoneCardPlan" => crate::phone::card_plan(self, arg(&args, "episodeId")?, arg(&args, "start")?, arg(&args, "end")?, &arg::<String>(&args, "text")?),
            "phoneLookUp" => crate::lookups::look_up(self, &arg::<String>(&args, "text")?, arg::<Option<usize>>(&args, "max")?.unwrap_or(6)),
            "pauseDownload" => {
                torrents::pause(self, arg(&args, "id")?, arg(&args, "paused")?)?;
                Ok(torrents::view(self))
            }
            "removeDownload" => {
                torrents::remove(self, arg(&args, "id")?)?;
                Ok(torrents::view(self))
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
                let can_read_ahead = self.playing.try_lock().is_ok_and(|p| {
                    !p.as_ref().is_some_and(|p| p.episode_id == e.id && p.starting_midway && p.from.is_none())
                });
                if can_read_ahead {
                    let from = range.and_then(|r| r.trim().strip_prefix("bytes=")).and_then(|r| r.split('-').next()?.trim().parse().ok());
                    self.read_ahead(&video, from.unwrap_or(0));
                }
                video
            }
            ["sheet", id] => self.sheet_path(&self.episode_or_err(id.parse().map_err(|_| anyhow!("bad episode"))?)?),
            ["picture", id, n] => return search::picture(self, id.parse().map_err(|_| anyhow!("bad episode"))?, n.parse().map_err(|_| anyhow!("bad picture"))?),
            ["cover", id] => {
                let id = id.parse().map_err(|_| anyhow!("bad show"))?;
                // The cover as fetched, for the moments before its small copy is made.
                Some(self.small_cover_path(id)).filter(|small| small.is_file()).unwrap_or_else(|| self.cover_path(id))
            }
            ["removed-cover", id] => self.removed_cover_path(id.parse().map_err(|_| anyhow!("bad show"))?),
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
    fn reads_ahead_only_after_a_resumed_episode_starts_playing() {
        let dir = std::env::temp_dir().join(format!("telly-resume-ahead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let path = dir.join("episode.mkv");
        std::fs::write(&path, b"0123456789").unwrap();
        let episodes = vec![crate::db::NewEpisode { number: 1, path: path.to_string_lossy().into_owned(), size: 10, duration: 1400.0 }];
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let id = backend.db.episodes(show).unwrap()[0].id;
        let src = format!("/ep/{id}");
        let read = |range| backend.serve(&src, Some(range)).unwrap().bytes;

        assert_eq!(read("bytes=0-1"), b"01");
        assert_eq!(backend.ahead.at.lock().unwrap().0, path);
        backend.db.save_position(id, 700.0).unwrap();
        backend.begin_sitting(&backend.db.episode(id).unwrap().unwrap());

        let tick = |position, playing| {
            backend.tick(serde_json::from_value(json!({ "id": id, "position": position, "playing": playing })).unwrap()).unwrap();
        };
        assert_eq!(read("bytes=0-1"), b"01");
        tick(0.0, false);
        assert_eq!(read("bytes=5-6"), b"56");
        assert!(backend.ahead.at.lock().unwrap().0.as_os_str().is_empty());

        tick(701.0, true);
        assert_eq!(read("bytes=6-7"), b"67");
        assert_eq!(backend.ahead.at.lock().unwrap().0, path);
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_library_set_up_before_the_setup_existed_skips_it() {
        let setup = |name: &str, prepare: &dyn Fn(&Backend)| {
            let dir = std::env::temp_dir().join(format!("telly-setup-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let dirs = Dirs { local: dir.clone(), roaming: dir.clone() };
            prepare(&Backend::open(dirs.clone()).unwrap());
            let done = Backend::open(dirs).unwrap().db.setting(SETUP_DONE).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            done
        };
        // A new library: the setup shows.
        assert_eq!(setup("new", &|_| {}), None);
        // A library with shows, or a Jimaku key: set up already.
        let show = |b: &Backend| {
            let episodes = vec![crate::db::NewEpisode { number: 1, path: "a.mkv".into(), size: 10, duration: 1400.0 }];
            b.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        };
        assert_eq!(setup("shows", &show), Some(json!(true)));
        assert_eq!(setup("key", &|b| b.db.set_setting("jimaku.key", &json!("key")).unwrap()), Some(json!(true)));
        // Begun, and a show added on its first screen: it still shows.
        let begun = |b: &Backend| {
            b.db.set_setting(SETUP_DONE, &json!(false)).unwrap();
            show(b);
        };
        assert_eq!(setup("begun", &begun), Some(json!(false)));
    }

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
    fn removing_a_show_leaves_stats_as_they_were() {
        let dir = std::env::temp_dir().join(format!("telly-remove-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = (1..=2).map(|n| crate::db::NewEpisode { number: n, path: format!("{n}.mkv"), size: 1, duration: 1400.0 }).collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Finished Show".into(), episodes, ..Default::default() }).unwrap();
        for e in backend.db.episodes(show).unwrap() {
            let sitting = crate::log::Sitting { show_id: show, title: "Finished Show", native: "", anilist_id: None, episode: e.number, episode_id: e.id, subs_file: "", subs_detail: "" };
            let session = backend.log.start(&sitting, crate::util::now_ms(), true).unwrap();
            backend.log.extend(session, crate::util::now_ms(), 1300.0, &[], true).unwrap();
            backend.db.finish_episode(e.id, true).unwrap();
            backend.db.set_word_score(e.id, 100, 80, "").unwrap();
        }
        std::fs::create_dir_all(backend.dirs.covers()).unwrap();
        std::fs::write(backend.cover_path(show), b"cover").unwrap();
        let before = crate::stats::overview(&backend).unwrap();
        assert_eq!((before["days"][0]["uses"].as_i64(), before["days"][0]["episodes"].as_i64()), (Some(200), Some(2)));

        backend.remove_show(show).unwrap();
        let after = crate::stats::overview(&backend).unwrap();
        assert_eq!(after["days"], before["days"]);
        assert_eq!(after["finished"], before["finished"]);
        let card = &after["shows"][show.to_string()];
        assert_eq!((card["title"].as_str(), card["removed"].as_bool()), (Some("Finished Show"), Some(true)));
        // Its cover is kept apart for Stats; the show's own is gone.
        assert_eq!(card["cover"], format!("/removed-cover/{show}"));
        assert_eq!(backend.serve(&format!("/removed-cover/{show}"), None).unwrap().bytes, b"cover");
        assert!(!backend.cover_path(show).exists());
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
        // A new episode and a file without a number arrive together, the one without sorting first
        // ("[A]" before "[Sub]"): episode 3 keeps its number, and the other comes after it.
        rip("[A] Show Recap.mkv");
        rip("[Sub] Show - 03 [720p].mkv");
        assert_eq!(backend.rescan(show).unwrap(), 2);
        let numbers: Vec<(i64, String)> = backend.db.episodes(show).unwrap().into_iter().map(|e| (e.number, e.path)).collect();
        assert_eq!(numbers[2..], [(3, path("[Sub] Show - 03 [720p].mkv")), (4, path("[A] Show Recap.mkv"))]);

        // Another show's folder inside this one (its second season, added on its own) isn't this show's.
        std::fs::create_dir_all(folder.join("Season 2")).unwrap();
        rip("Season 2/[Sub] Show S2 - 01 [720p].mkv");
        let second = vec![crate::db::NewEpisode { number: 1, path: path("Season 2/[Sub] Show S2 - 01 [720p].mkv"), size: 1, duration: 400.0 }];
        let season_two = backend.db.add_show(&crate::db::NewShow { folder: path("Season 2"), title: "Show 2".into(), episodes: second, ..Default::default() }).unwrap();
        rip("Season 2/[Sub] Show S2 - 02 [720p].mkv");
        assert_eq!(backend.rescan(show).unwrap(), 0, "{:?}", backend.db.episodes(show).unwrap().iter().map(|e| (e.number, e.path.clone())).collect::<Vec<_>>());

        // Nor is a file you left out when adding it.
        let other = dir.join("Other");
        std::fs::create_dir_all(&other).unwrap();
        let in_other = |name: &str| other.join(name).to_string_lossy().into_owned();
        for name in ["Other - 01.mkv", "Other - 02.mkv"] {
            std::fs::rename(path("[BD] Show - 01 [1080p].mkv"), in_other(name)).unwrap();
            rip("[BD] Show - 01 [1080p].mkv");
        }
        let read = scan::scan(&other, |paths| vec![Some(400.0); paths.len()]).unwrap();
        let chosen = crate::db::NewShow {
            folder: other.to_string_lossy().into_owned(),
            title: "Other".into(),
            episodes: vec![crate::db::NewEpisode { number: 1, path: in_other("Other - 01.mkv"), size: 1, duration: 400.0 }],
            ..Default::default()
        };
        let other_id = backend.db.add_show(&chosen).unwrap();
        backend.keep_left_out(other_id, &chosen, Some(&read));
        assert_eq!(backend.rescan(other_id).unwrap(), 0);
        // Kept by its place in the show's folder, so it's left out wherever the folder goes.
        assert_eq!(backend.db.setting(&left_out_setting(other_id)).unwrap(), Some(json!(["Other - 02.mkv"])));

        // The second season, removed, doesn't hand its rips to the first.
        backend.remove_show(season_two).unwrap();
        assert_eq!(backend.rescan(show).unwrap(), 0);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_season_s_folder_isn_t_more_of_the_season_before() {
        let dir = std::env::temp_dir().join(format!("telly-new-season-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let Some(ffmpeg) = backend.tools().ffmpeg else { return }; // making rips to scan needs ffmpeg
        let folder = dir.join("K-On!");
        let path = |name: &str| folder.join(name).to_string_lossy().into_owned();
        let rip = |name: &str| {
            std::fs::create_dir_all(Path::new(&path(name)).parent().unwrap()).unwrap();
            let made = std::process::Command::new(&ffmpeg)
                .args(["-v", "quiet", "-f", "lavfi", "-i", "color=c=black:s=64x36:r=1", "-t", "400", "-c:v", "mpeg4", "-y"])
                .arg(path(name))
                .status()
                .unwrap();
            assert!(made.success());
        };
        let episodes = (1..=3)
            .map(|n| {
                rip(&format!("Season 01/K-On! - S01E0{n}.mkv"));
                crate::db::NewEpisode { number: n, path: path(&format!("Season 01/K-On! - S01E0{n}.mkv")), size: 1, duration: 400.0 }
            })
            .collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: folder.to_string_lossy().into_owned(), title: "K-On!".into(), episodes, ..Default::default() }).unwrap();
        // The second season's numbers start again: its fourth and fifth episodes aren't the first season's.
        for n in 1..=5 {
            rip(&format!("Season 02/K-On! - S02E0{n}.mkv"));
        }
        assert_eq!(backend.rescan(show).unwrap(), 0);
        assert_eq!(backend.db.episodes(show).unwrap().len(), 3);
        // A folder that carries on from the show's last episode is more of it.
        rip("Part 2/K-On! - 04.mkv");
        rip("Part 2/K-On! - 05.mkv");
        assert_eq!(backend.rescan(show).unwrap(), 2);
        let numbers: Vec<(i64, String)> = backend.db.episodes(show).unwrap().into_iter().map(|e| (e.number, e.path)).collect();
        assert_eq!(numbers[3..], [(4, path("Part 2/K-On! - 04.mkv")), (5, path("Part 2/K-On! - 05.mkv"))]);
        // A new release of episodes whose rips are gone, in a folder of its own, still takes their place.
        for n in 1..=3 {
            std::fs::remove_file(path(&format!("Season 01/K-On! - S01E0{n}.mkv"))).unwrap();
            rip(&format!("[BD] K-On!/K-On! - 0{n}.mkv"));
        }
        assert_eq!(backend.rescan(show).unwrap(), 3);
        assert_eq!(backend.db.episodes(show).unwrap()[0].path, path("[BD] K-On!/K-On! - 01.mkv"));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_film_s_new_rip_takes_the_place_of_the_one_that_s_gone_whatever_it_s_called() {
        let dir = std::env::temp_dir().join(format!("telly-film-rip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let Some(ffmpeg) = backend.tools().ffmpeg else { return }; // making rips to scan needs ffmpeg
        let folder = dir.join("Paprika (2006)");
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
        rip("Paprika.mkv");
        let episodes = vec![crate::db::NewEpisode { number: 1, path: path("Paprika.mkv"), size: 1, duration: 5400.0 }];
        let film = crate::db::NewShow { folder: folder.to_string_lossy().into_owned(), title: "Paprika".into(), format: "MOVIE".into(), anilist_id: Some(1943), anilist_episodes: Some(1), episodes, ..Default::default() };
        let show = backend.db.add_show(&film).unwrap();
        let one = backend.db.episodes(show).unwrap()[0].clone();
        backend.db.save_position(one.id, 600.0).unwrap();
        // A better rip under another name, as Radarr puts one in.
        std::fs::remove_file(path("Paprika.mkv")).unwrap();
        rip("Paprika (2006) [Bluray-1080p].mkv");
        assert_eq!(backend.rescan(show).unwrap(), 1);
        let after = backend.db.episodes(show).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!((after[0].id, after[0].number, after[0].path.as_str(), after[0].position), (one.id, 1, path("Paprika (2006) [Bluray-1080p].mkv").as_str(), 600.0));
        // Two new videos don't say which is the film: it's left as it is.
        std::fs::remove_file(path("Paprika (2006) [Bluray-1080p].mkv")).unwrap();
        rip("Paprika CD1.mkv");
        rip("Paprika CD2.mkv");
        backend.rescan(show).unwrap();
        assert_eq!(backend.db.episodes(show).unwrap()[0].path, path("Paprika (2006) [Bluray-1080p].mkv"));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_film_is_its_entry_s_one_episode_whatever_number_its_name_has() {
        let episode = |number: i64| vec![crate::db::NewEpisode { number, path: "Jujutsu Kaisen 0.mkv".into(), size: 1, duration: 6000.0 }];
        let film = Anime { id: 131573, format: "MOVIE".into(), episodes: Some(1), ..Default::default() };
        assert_eq!(Backend::new_show_from(Some(&film), NewShow { episodes: episode(0), ..Default::default() }).episodes[0].number, 1);
        // A film released in parts keeps the number of the part.
        let parts = Anime { episodes: Some(6), ..film.clone() };
        assert_eq!(Backend::new_show_from(Some(&parts), NewShow { episodes: episode(3), ..Default::default() }).episodes[0].number, 3);
        // So does a series you have one episode of.
        let series = Anime { format: "TV".into(), ..film };
        assert_eq!(Backend::new_show_from(Some(&series), NewShow { episodes: episode(5), ..Default::default() }).episodes[0].number, 5);
    }

    #[test]
    fn a_new_show_s_download_is_the_show_you_said_once_something_of_it_is_down() {
        let dir = std::env::temp_dir().join(format!("telly-download-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let Some(ffmpeg) = backend.tools().ffmpeg else { return }; // making rips to scan needs ffmpeg
        let rip = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let made = std::process::Command::new(&ffmpeg)
                .args(["-v", "quiet", "-f", "lavfi", "-i", "color=c=black:s=64x36:r=1", "-t", "400", "-c:v", "mpeg4", "-y"])
                .arg(path)
                .status()
                .unwrap();
            assert!(made.success());
        };
        let film = Anime { id: 99, romaji: "Film".into(), english: "Film".into(), format: "MOVIE".into(), episodes: Some(1), year: Some(2018), ..Default::default() };

        // A film and its creditless opening: one show, the extra left out.
        let folder = dir.join("Movies").join("Film (2018)");
        rip(&folder.join("[Sub] Film.mkv"));
        rip(&folder.join("[Sub] Film - NCOP.mkv"));
        let new = backend.show_in_download(&folder.to_string_lossy(), &film).unwrap().expect("one show");
        assert_eq!((new.anilist_id, new.format.as_str(), new.title.as_str()), (Some(99), "MOVIE", "Film"));
        assert_eq!(new.episodes.iter().map(|e| (e.number, e.path.clone())).collect::<Vec<_>>(), [(1, folder.join("[Sub] Film.mkv").to_string_lossy().into_owned())]);

        // A folder for each season is several shows: Add media's sheet is for those.
        let seasons = dir.join("Series").join("Show");
        rip(&seasons.join("S1").join("Show - S01E01.mkv"));
        rip(&seasons.join("S1").join("Show - S01E02.mkv"));
        rip(&seasons.join("S2").join("Show - S02E01.mkv"));
        rip(&seasons.join("S2").join("Show - S02E02.mkv"));
        assert!(backend.show_in_download(&seasons.to_string_lossy(), &film).unwrap().is_none());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_video_that_isn_t_an_episode_is_read_once_not_at_every_look() {
        let dir = std::env::temp_dir().join(format!("telly-looked-at-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let (extra, copying) = (dir.join("NCOP.mkv"), dir.join("Show - 02.mkv"));
        std::fs::write(&extra, b"an opening").unwrap();
        std::fs::write(&copying, b"half").unwrap();
        let paths = [extra.clone(), copying.clone()];
        let reads = std::cell::RefCell::new(Vec::new());
        let look = |lengths: &[Option<f64>]| {
            backend.lengths_once_read(&paths, |unread| {
                reads.borrow_mut().push(unread.to_vec());
                lengths.to_vec()
            })
        };
        // The first look reads both; the one still being copied can't be read yet.
        assert_eq!(look(&[Some(90.0), None]), [Some(90.0), None]);
        // The next reads only that one again.
        assert_eq!(look(&[Some(1400.0)]), [Some(90.0), Some(1400.0)]);
        // And the one after, neither.
        assert_eq!(look(&[]), [Some(90.0), Some(1400.0)]);
        // A file that changed is read again.
        std::fs::write(&extra, b"another opening, longer").unwrap();
        assert_eq!(look(&[Some(95.0)]), [Some(95.0), Some(1400.0)]);
        assert_eq!(*reads.borrow(), [vec![extra.clone(), copying.clone()], vec![copying], vec![extra]]);
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
        // In a folder of Telly's own there: the folder you choose may hold your files too.
        assert!(moved.starts_with(dir.join("elsewhere").join("Telly copies")));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !moved.is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(moved.is_file() && !sheet.exists());
        // The folder is kept once asked for, and a change to the setting is seen at once.
        backend.call("setSetting", json!({ "key": "media.folder", "value": null })).unwrap();
        assert_eq!(backend.media_dir(), backend.dirs.media());
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
    fn a_page_that_has_the_answer_already_is_told_it_s_unchanged() {
        let first = tagged(json!({ "episodes": [1, 2] }), &json!({ "id": 1 }));
        let tag = first["tag"].as_str().unwrap().to_string();
        assert_eq!(tagged(json!({ "episodes": [1, 2] }), &json!({ "id": 1, "tag": tag })), json!({ "unchanged": true }));
        let changed = tagged(json!({ "episodes": [1, 2, 3] }), &json!({ "id": 1, "tag": tag }));
        assert_eq!(changed["episodes"], json!([1, 2, 3]));
        assert_ne!(changed["tag"], json!(tag));
    }

    #[test]
    fn the_library_says_which_library_each_show_is_in() {
        let dir = std::env::temp_dir().join(format!("telly-library-of-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Arc::new(Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        for (folder, title) in [(r"D:\Dramas\Quartet", "Quartet"), (r"D:\Anime\K-ON!\Season 01", "K-ON!"), (r"C:\Rips\Paprika", "Paprika")] {
            backend.db.add_show(&crate::db::NewShow { folder: folder.into(), title: title.into(), ..Default::default() }).unwrap();
        }
        let library = |name: &str, folder: &str| json!({ "name": name, "kind": "series", "folders": [folder], "downloads": folder });
        backend.db.set_setting("libraries", &json!([library("Anime", r"D:\Anime"), library("Dramas", r"D:\Dramas")])).unwrap();
        let answer = backend.call("library", json!({})).unwrap();
        assert_eq!(answer["libraries"], json!(["Anime", "Dramas"]));
        let of = |title: &str| answer["shows"].as_array().unwrap().iter().find(|s| s["title"] == title).unwrap()["library"].clone();
        assert_eq!(of("K-ON!"), json!(0));
        assert_eq!(of("Quartet"), json!(1));
        // One kept outside them all.
        assert_eq!(of("Paprika"), json!(null));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn files_of_episodes_no_longer_in_telly_are_swept_away() {
        let dir = std::env::temp_dir().join(format!("telly-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = vec![crate::db::NewEpisode { number: 1, path: "1.mkv".into(), size: 1, duration: 1400.0 }];
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let id = backend.db.episodes(show).unwrap()[0].id;
        let subs = backend.dirs.subs();
        std::fs::create_dir_all(&subs).unwrap();
        for name in [format!("{id}.json"), format!("{}.json", id + 100), format!("{}.words.json", id + 100), "notes.txt".into()] {
            std::fs::write(subs.join(name), b"[]").unwrap();
        }
        backend.forget_removed_files();
        let mut left: Vec<String> = std::fs::read_dir(&subs).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec![format!("{id}.json"), "notes.txt".to_string()]);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_episode_watched_to_its_end_again_is_a_rewatch_once_a_sitting() {
        let dir = std::env::temp_dir().join(format!("telly-rewatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = (1..=2).map(|number| crate::db::NewEpisode { number, path: format!("{number}.mkv"), size: 1, duration: 1400.0 }).collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let eps = backend.db.episodes(show).unwrap();
        let tick = |id: i64, position: f64| {
            backend.tick(serde_json::from_value(json!({ "id": id, "position": position, "playing": true })).unwrap()).unwrap();
        };
        // Each watched once.
        for e in &eps {
            tick(e.id, 10.0);
            tick(e.id, 1300.0);
        }
        // Played from the start to past its end again: one rewatch, however many ticks after.
        tick(eps[0].id, 10.0);
        tick(eps[0].id, 1300.0);
        tick(eps[0].id, 1350.0);
        // Opened at its end, it isn't rewatched.
        tick(eps[1].id, 1300.0);
        let watches = backend.db.watches_since(show, 0).unwrap();
        let again: Vec<i64> = watches.iter().map(|w| w.0).filter(|id| *id == eps[0].id).collect();
        assert_eq!(again.len(), 2, "the first watch and one rewatch: {watches:?}");
        assert_eq!(watches.iter().filter(|w| w.0 == eps[1].id).count(), 1);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_episode_played_to_where_aniskip_says_its_ending_starts_is_watched() {
        let dir = std::env::temp_dir().join(format!("telly-aniskip-end-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = vec![crate::db::NewEpisode { number: 1, path: "1.mkv".into(), size: 1, duration: 1420.0 }];
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), anilist_id: Some(5), episodes, ..Default::default() }).unwrap();
        let id = backend.db.episodes(show).unwrap()[0].id;
        // No chapters; AniSkip has the ending at 21:00, 89% of the way in: Skip endings stops there.
        let skips = crate::aniskip::Skips { opening: None, ending: Some((1260.0, 1350.0)) };
        assert!(backend.db.set_opening_if(id, 1, 5, skips).unwrap());
        let tick = |position: f64| {
            backend.tick(serde_json::from_value(json!({ "id": id, "position": position, "playing": true })).unwrap()).unwrap();
        };
        tick(10.0);
        tick(1259.0);
        assert!(backend.db.episode(id).unwrap().unwrap().watched_at.is_none());
        tick(1260.5);
        assert!(backend.db.episode(id).unwrap().unwrap().watched_at.is_some());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_episode_left_at_its_end_and_played_again_from_the_start_is_a_rewatch() {
        let dir = std::env::temp_dir().join(format!("telly-rewatch-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let episodes = vec![crate::db::NewEpisode { number: 1, path: "1.mkv".into(), size: 1, duration: 1400.0 }];
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let id = backend.db.episodes(show).unwrap()[0].id;
        let tick = |position: f64, playing: bool| {
            backend.tick(serde_json::from_value(json!({ "id": id, "position": position, "playing": playing })).unwrap()).unwrap();
        };
        // Watched to its end, and left there.
        tick(1.0, true);
        tick(1300.0, true);
        tick(1395.0, false);
        // Opened again: the player starts a watched episode from the beginning, and plays it to its end.
        backend.begin_sitting(&backend.db.episode(id).unwrap().unwrap());
        for position in [0.0, 1.0, 600.0, 1300.0] {
            tick(position, position > 0.0);
        }
        let watches = backend.db.watches_since(show, 0).unwrap();
        assert_eq!(watches.len(), 2, "the first watch and the rewatch: {watches:?}");
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_episode_marked_watched_by_hand_isn_t_a_watch_for_a_rewatch() {
        let dir = std::env::temp_dir().join(format!("telly-rewatch-marked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Arc::new(Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        let episodes = vec![crate::db::NewEpisode { number: 1, path: "1.mkv".into(), size: 1, duration: 1400.0 }];
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let id = backend.db.episodes(show).unwrap()[0].id;
        // Marked watched by hand, unwatched, and watched again (on a show AniList has as completed, say).
        for watched in [true, false, true] {
            backend.call("markWatched", json!({ "id": id, "watched": watched })).unwrap();
        }
        assert!(backend.db.watches_since(show, 0).unwrap().is_empty());
        assert!(backend.db.episode(id).unwrap().unwrap().watched_at.is_some());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_added_unchecked_is_its_episodes_or_the_film_that_s_all_it_holds() {
        let file = |name: &str, number: Option<i64>, reason: &str| scan::ScannedFile {
            path: format!(r"D:\Films\Film\{name}"),
            rel: name.into(),
            size: 1,
            duration: 9000.0,
            parsed: None,
            number,
            reason: reason.into(),
        };
        let found = |files: Vec<scan::ScannedFile>| Found {
            scan: scan::Scan { folder: r"D:\Films\Film".into(), guess: "Film".into(), files, numbering: "order".into() },
            matches: Vec::new(),
            parts: Vec::new(),
            entries: Vec::new(),
            anime: None,
            error: None,
        };
        let paths = |found: &Found| found.shows().iter().map(|(show, _)| show.episodes.iter().map(|e| (e.number, e.path.rsplit('\\').next().unwrap_or_default().to_string())).collect::<Vec<_>>()).collect::<Vec<_>>();
        // A film past two hours is left out of a show's episodes, but it's the show when it's all there is.
        let film = found(vec![file("Film.mkv", None, "Over 2 hours (a play-all title)"), file("Trailer.mkv", None, "Under 5 minutes")]);
        assert_eq!(paths(&film), [[(1, "Film.mkv".to_string())]]);
        assert_eq!(film.shows()[0].0.title, "Film");
        // Beside episodes, it's the disc's play-all title, left out as ever.
        let disc = found(vec![file("01.mkv", Some(1), ""), file("02.mkv", Some(2), ""), file("All.mkv", None, "Over 2 hours (a play-all title)")]);
        assert_eq!(paths(&disc), [[(1, "01.mkv".to_string()), (2, "02.mkv".to_string())]]);
        // A folder with nothing to watch is no show.
        assert!(found(vec![file("Trailer.mkv", None, "Under 5 minutes")]).shows().is_empty());
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
