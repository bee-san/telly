//! Everything the UI asks for goes through `Backend::call(cmd, args)`, from
//! the app (lib.rs) or from the development server (devserver.rs), so both
//! run the same code.

use crate::anilist::{self, Anime};
use crate::ask::{self, Asks};
use crate::cards::{self, Event};
use crate::db::{Db, Episode, NewShow, Show};
use crate::jobs::{self, Jobs, Task};
use crate::log::{Log, Sitting};
use crate::mining::{self, Mining};
use crate::names::{self, Names};
use crate::own_hachidori::{self, Own};
use crate::premine::{self, Premining};
use crate::scan;
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
}

/// What's being worked on, for the title bar.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub tasks: Vec<Task>,
    /// The show being got ready: its title, episodes with subtitles settled, and its episode count.
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
        let found = tools::find(&dirs.tools(), &dirs.models(), &db.setting_str("tools.ffmpeg"));
        Ok(Backend {
            db,
            log,
            dirs,
            tools: RwLock::new(found),
            jobs: Jobs::default(),
            cards: cards::Queue::default(),
            playing: Mutex::new(None),
            last_play: Mutex::new(None),
            events: Mutex::new((0, VecDeque::new())),
            mining: OnceLock::new(),
            asks: Asks::default(),
            names: Names::default(),
            premining: Premining::default(),
            own_hachidori: Own::default(),
        })
    }

    /// Starts the background work: getting shows ready and finishing cards.
    pub fn start(self: &Arc<Self>) {
        jobs::start(self.clone());
        let backend = self.clone();
        std::thread::Builder::new().name("telly-cards".into()).spawn(move || cards::worker(&backend)).ok();
        self.watch_folders();
        names::start(self.clone());
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

    pub fn tools(&self) -> Tools {
        self.tools.read().map(|t| t.clone()).unwrap_or_default()
    }

    pub fn refresh_tools(&self) {
        self.refind_tools();
    }

    fn refind_tools(&self) {
        let found = tools::find(&self.dirs.tools(), &self.dirs.models(), &self.db.setting_str("tools.ffmpeg"));
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

    fn cover_path(&self, show_id: i64) -> PathBuf {
        self.dirs.covers().join(format!("{show_id}.jpg"))
    }

    fn fetch_cover(self: &Arc<Self>, show_id: i64, url: String) {
        let backend = self.clone();
        std::thread::spawn(move || {
            let _ = anilist::download_cover(&url, &backend.cover_path(show_id));
        });
    }

    fn card(&self, show: Show, cards_by_show: &std::collections::HashMap<i64, i64>) -> Result<ShowCard> {
        let episodes = self.db.episodes(show.id)?;
        let watched = episodes.iter().filter(|e| e.watched_at.is_some()).count();
        let media_ready = episodes.iter().filter(|e| e.media_state == "ready").count();
        let subs_ready = episodes.iter().filter(|e| settled(&e.subs_state)).count();
        let subs_found = episodes.iter().filter(|e| matches!(e.subs_state.as_str(), "synced" | "check" | "manual")).count();
        let next = jobs::next_episode(&episodes).map(|e| NextUp {
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
        let cover = if self.cover_path(show.id).is_file() { format!("/cover/{}?v={}", show.id, show.anilist_id.unwrap_or(0)) } else { String::new() };
        Ok(ShowCard {
            cards: cards_by_show.get(&show.id).copied().unwrap_or(0),
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
        let mut tasks = Vec::new();
        for slot in [&self.jobs.media, &self.jobs.subs, &self.jobs.download, &self.names.task] {
            if let Some(t) = slot.lock().ok().and_then(|t| t.clone()) {
                tasks.push(t);
            }
        }
        let mut activity = Activity { tasks, ..Default::default() };
        if let Some(t) = activity.tasks.last().cloned() {
            if let Ok(eps) = self.db.episodes(t.show_id) {
                activity.show_id = Some(t.show_id);
                activity.title = t.title.clone();
                activity.total = eps.len();
                activity.done = eps.iter().filter(|e| settled(&e.subs_state)).count();
            }
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
        let shows = self.db.shows()?;
        let mut cards = Vec::with_capacity(shows.len());
        for s in shows {
            cards.push(self.card(s, &cards_by_show)?);
        }
        Ok(json!({ "shows": cards, "activity": self.activity(), "setup": self.setup() }))
    }

    fn show_view(&self, id: i64) -> Result<Value> {
        let show = self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let cards_by_show = self.db.cards_per_show().unwrap_or_default();
        let episodes = self.db.episodes(id)?;
        let next_id = jobs::next_episode(&episodes).map(|e| e.id);
        let rows: Vec<EpisodeRow> = episodes
            .into_iter()
            .map(|e| EpisodeRow {
                file: Path::new(&e.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                watched: e.watched_at.is_some(),
                is_next: Some(e.id) == next_id,
                episode: e,
            })
            .collect();
        let card = self.card(show, &cards_by_show)?;
        let tools = self.tools();
        let whisper = json!({ "cli": tools.whisper.is_some(), "model": tools.whisper_model.is_some() });
        Ok(json!({ "card": card, "episodes": rows, "activity": self.activity(), "whisper": whisper }))
    }

    fn scan(&self, folder: &str) -> Result<Value> {
        let tools = self.tools();
        let scan = scan::scan(Path::new(folder), |paths| scan::probe_durations(tools.ffprobe.as_deref(), paths))?;
        let count = scan.files.iter().filter(|f| f.number.is_some()).count();
        let (matches, error) = match anilist::search_loosely(&scan.guess) {
            Ok(mut found) => {
                anilist::rank(&mut found, &scan.guess, count);
                (found, None)
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
        let mut new = Vec::new();
        for f in scan.files.iter().filter(|f| f.reason.is_empty() && !known.contains_key(&f.path)) {
            let number = match f.parsed {
                Some(n) if scan.numbering == "names" => {
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
            self.jobs.poke();
        }
        Ok(new.len())
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
        if !new.cover_url.is_empty() {
            self.fetch_cover(id, new.cover_url.clone());
        }
        self.jobs.poke();
        Ok(json!({ "id": id }))
    }

    fn rematch(self: &Arc<Self>, id: i64, anilist_id: i64) -> Result<Value> {
        let show = self.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let anime = anilist::by_id(anilist_id)?;
        let new = Self::new_show_from(Some(&anime), NewShow { folder: show.folder.clone(), ..Default::default() });
        self.db.set_show_anilist(id, &new)?;
        let _ = std::fs::remove_file(self.cover_path(id));
        self.fetch_cover(id, new.cover_url.clone());
        // Subtitles were found under the old show's name.
        self.db.requeue_subs(Some(id), &["none", "failed", "check", "synced", "nokey"])?;
        self.jobs.forget_jimaku();
        self.jobs.poke();
        self.show_view(id)
    }

    fn remove_show(&self, id: i64) -> Result<Value> {
        self.jobs.cancel_show(id);
        let episodes = self.db.episodes(id)?;
        self.db.remove_show(id)?;
        // Only Telly's own files go; your rips stay where they are.
        let _ = std::fs::remove_dir_all(self.dirs.media().join(id.to_string()));
        for e in episodes {
            let _ = std::fs::remove_file(jobs::lines_path(self, e.id));
            let _ = std::fs::remove_file(premine::words_path(self, e.id));
        }
        let _ = std::fs::remove_file(self.cover_path(id));
        Ok(json!({}))
    }

    /// Removes Telly's copies of episodes you've watched (or of a whole finished show).
    fn free_space(&self, show_id: Option<i64>) -> Result<Value> {
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
                    count += 1;
                    self.db.media_reset(e.id)?;
                }
            }
        }
        Ok(json!({ "episodes": count, "bytes": freed }))
    }

    fn space_used(&self) -> u64 {
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
        size(&self.dirs.media())
    }

    fn episode_view(&self, id: i64) -> Result<Value> {
        let episode = self.db.episode(id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
        let show = self.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
        let episodes = self.db.episodes(show.id)?;
        let i = episodes.iter().position(|e| e.id == id).unwrap_or(0);
        let lite = |e: &Episode| json!({ "id": e.id, "number": e.number, "ready": e.media_state == "ready", "mediaState": e.media_state, "mediaProgress": e.media_progress });
        let lines: Vec<Line> = jobs::read_lines(self, id);
        Ok(json!({
            "show": show,
            "episode": episode,
            "src": format!("/ep/{id}?v={}", episode.media_plan),
            "lines": lines,
            "prev": if i > 0 { Some(lite(&episodes[i - 1])) } else { None },
            "next": episodes.get(i + 1).map(lite),
            "count": episodes.len(),
            "tried": jobs::read_tried(self, id),
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
            "tools.ffmpeg" => self.refind_tools(),
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
            "setSetting" => self.set_setting(&arg::<String>(&args, "key")?, &args.get("value").cloned().unwrap_or(Value::Null)),
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
                let id: i64 = arg(&args, "id")?;
                let mut lines = jobs::read_lines(self, id);
                sync::shift_from(&mut lines, arg(&args, "from")?, arg(&args, "ms")?);
                jobs::write_lines(self, id, &lines)?;
                to_value(lines)
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
            "whisperSubs" => {
                let n = self.db.queue_whisper(arg(&args, "show")?, arg(&args, "id")?)?;
                self.jobs.poke();
                Ok(json!({ "queued": n }))
            }
            "getWhisperModel" => {
                jobs::get_whisper_model(self.clone());
                Ok(json!({}))
            }
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
            "premine" => to_value(premine::start_list(self, arg(&args, "show")?)?),
            "premineStatus" => to_value(premine::status(self)),
            "ownHachidori" => to_value(own_hachidori::report(self)),
            "premineCards" => to_value(premine::start_cards(self, arg(&args, "words")?)?),
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
                if e.media_state == "ready" && !e.play_path.is_empty() { PathBuf::from(e.play_path) } else { PathBuf::from(e.path) }
            }
            ["cover", id] => self.cover_path(id.parse().map_err(|_| anyhow!("bad show"))?),
            ["clip", name] if !name.contains("..") => self.dirs.clips().join(name),
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
