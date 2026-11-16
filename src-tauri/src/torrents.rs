//! Telly's own torrent client (librqbit) does its downloading: you pick a
//! release on nyaa.si (nyaa.rs), and Telly downloads it, carrying on from
//! where it was whenever the connection drops, and when Telly opens again.
//!
//! Each download's files go straight into a show's folder. For a show in the
//! library, that's the folder its rips are in. For one that isn't, you say
//! which show it is before it starts (`plan`: the anime its name finds on
//! AniList, or another you choose), and it goes to its place in the library
//! you choose (library.rs), or, with none, a folder named for it in the
//! downloads folder. A file that isn't finished counts as
//! unreadable when Telly looks in a folder, so no episode is added half
//! there. When a file finishes in a show's folder, Telly looks in the folder
//! at once. A new show is added to the library when its first file finishes,
//! as the show you said it was; a release you didn't say, or one of several
//! parts (a folder for each season), is offered in the Library to add.
//!
//! Telly fetches the release's .torrent file from nyaa, asking again when the
//! connection drops, and falls back to the magnet link (whose list of files
//! has to come from the people sharing it) only when it can't. The client
//! keeps what it's downloading, how far each got and the DHT's nodes in
//! torrents/ in Telly's data folder. It writes down how far a download got
//! only every 16 MB, so when Telly opens, the downloads that weren't finished
//! check what's on disk instead (a few seconds an episode), and lose nothing.
//! A finished download is shared while Telly is open, until it's removed in
//! Processing; removing one leaves its files.
//!
//! What the client may do is yours to say, in Settings → Library and
//! downloads:
//!
//! - How fast it downloads and uploads, at most (no limit unless set).
//! - Whether a download goes on being shared once it's finished. When it
//!   doesn't, each is paused as it finishes; while it downloads it shares
//!   what it has, as a torrent does.
//! - Whether Telly asks your router to let others connect to it (UPnP).
//! - The one network connection to download through (a VPN's). While that
//!   connection isn't up the client isn't running at all: it's stopped
//!   within a second of the connection going, and started again when it's
//!   back. librqbit can't tie its sockets to a connection on Windows, so
//!   this is what Telly can do there.
//! - How much room to keep free on the drive: a download there isn't room
//!   for, counting what the others on that drive still need, isn't started.
//!
//! Settings: downloads.folder (where new shows go when no library folder
//! takes them; Downloads\Telly in your home folder unless set),
//! downloads.downLimit and downloads.upLimit (KB a second; none or 0 for no
//! limit), downloads.share (true unless set), downloads.upnp (true unless
//! set), downloads.adapter (the connection's name; empty for any),
//! downloads.keepFree (GB; 1 unless set), downloads.used (Telly has downloaded something, so it starts the client
//! when it opens), downloads.unfinished (the info hashes of the downloads
//! that weren't finished at the last look) and downloads.entries (the AniList
//! entry of each new show's download, by its folder, until it's added) and
//! downloads.left (the files of downloads removed before they finished).
//!
//! A release is never downloaded over a file that's there already, unless
//! the file is a download's own: the client writes over what it finds, so
//! Telly checks the release's files first, once they're known.

use crate::anilist::{self, Anime};
use crate::backend::Backend;
use crate::util::safe_file_name;
use crate::{library, scan, series};
use anyhow::{anyhow, Context, Result};
use librqbit::dht::DhtPersistenceConfig;
use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, ListenerOptions, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig, TorrentStatsState,
};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

/// Telly's downloads, as the client last had them.
#[derive(Default)]
pub struct Downloads {
    session: Mutex<Option<Arc<Session>>>,
    /// Releases added by magnet link, whose files are still being asked for.
    finding: Mutex<Vec<Finding>>,
    seen: Mutex<Seen>,
    /// The .torrent file last fetched and its address: a release's sheet reads it, then its download starts from it.
    fetched: Mutex<Option<(String, Vec<u8>)>>,
    /// The folders of new shows being added to the library now.
    adding: Mutex<HashSet<String>>,
    /// Wakes the watcher to look again at once.
    wake: (Mutex<bool>, Condvar),
}

#[derive(Clone)]
struct Finding {
    hash: String,
    name: String,
    folder: String,
    error: Option<String>,
}

#[derive(Default)]
struct Seen {
    torrents: Vec<Torrent>,
    /// The files that aren't finished, as `key` writes their paths.
    unfinished: HashSet<String>,
    /// How many files each download had finished at the last look.
    done: HashMap<String, usize>,
    looked: bool,
}

/// One of Telly's downloads.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Torrent {
    /// What Processing's buttons name it by; none while its files are being found.
    pub id: Option<usize>,
    pub hash: String,
    pub name: String,
    /// From 0 to 1.
    pub progress: f64,
    pub size: u64,
    /// Bytes a second.
    pub speed: u64,
    /// Seconds left, when there's a speed to tell by.
    pub eta: Option<u64>,
    /// "finding" (its files), "checking" (what's on disk), "downloading",
    /// "waiting" (for anyone sharing it), "paused", "done" or "error".
    pub state: String,
    pub error: Option<String>,
    /// The folder its files go in.
    pub folder: String,
    /// The people sharing it that Telly is connected to.
    pub peers: u32,
    /// The show whose folder it's in.
    pub show: Option<i64>,
    /// How many of its files are finished, and how many it has.
    pub files_done: usize,
    pub files: usize,
}

/// A folder of finished downloads that isn't a show in the library yet.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub folder: String,
    pub name: String,
    /// Whether some of it is still downloading.
    pub downloading: bool,
}

/// Where a release's files would go.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Destination {
    pub folder: String,
    /// The show in the library it's more of.
    pub show: Option<i64>,
    /// Whether that's its place in a library folder.
    pub sorted: bool,
    /// Whether it joins the library by itself as it finishes: a show's episodes, or the one new show you said it is.
    pub joins: bool,
    /// The library it goes in, by its place in the list.
    pub library: Option<usize>,
}

/// How the downloads are going, all together, for the title bar.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub count: usize,
    pub progress: f64,
    pub speed: u64,
}

/// The runtime the client runs on, apart from Tauri's.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("telly-torrents")
            .enable_all()
            .build()
            .expect("the downloads' runtime")
    })
}

/// Runs `work` on the client's runtime and waits for it, from any thread.
fn run<T: Send + 'static>(work: impl Future<Output = T> + Send + 'static) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    runtime().spawn(async move {
        let _ = tx.send(work.await);
    });
    rx.recv().map_err(|_| anyhow!("Telly’s downloader stopped"))
}

/// A path as Telly compares them: forward slashes, no slash at the end, and
/// on Windows in lower case.
fn key(path: &str) -> String {
    let k = path.replace('\\', "/").trim_end_matches('/').to_string();
    if cfg!(windows) { k.to_lowercase() } else { k }
}

/// Whether `path` is the folder `folder` or in it.
fn inside(path: &str, folder: &str) -> bool {
    let (p, f) = (key(path), key(folder));
    !f.is_empty() && (p == f || p.strip_prefix(&f).is_some_and(|rest| rest.starts_with('/')))
}

/// The folder a new show's download goes in: the show's name from the
/// release's title. "[Erai-raws] Sousou no Frieren 2nd Season - 09 [1080p]"
/// goes in "Sousou no Frieren 2nd Season".
pub fn folder_name(title: &str) -> String {
    static BRACKETS: OnceLock<Regex> = OnceLock::new();
    static EPISODE: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    let brackets = BRACKETS.get_or_init(|| Regex::new(r"\[[^\]]*\]|\([^)]*\)|\{[^}]*\}|【[^】]*】").expect("a valid pattern"));
    // Where the episode (or the release's details) starts: " - 09", " - S01", "S02E06", " E05", " 1080p", "Batch".
    let episode = EPISODE.get_or_init(|| {
        Regex::new(r"(?i)\s+-\s+(?:s?\d|e\d|ep|batch|vol)|\bs\d{1,2}(?:e\d+|v\d|\b)|\be(?:p)?\d{1,4}\b|\b\d{3,4}p\b|\bbatch\b").expect("a valid pattern")
    });
    let bare = brackets.replace_all(title, " ");
    // "Mononoke.2007.1080p.BluRay" is written with dots for spaces.
    let bare = if title.contains(' ') { bare.replace('_', " ") } else { bare.replace(['_', '.'], " ") };
    let cut = episode.find(&bare).map_or(bare.as_str(), |m| &bare[..m.start()]);
    let name = SPACES.get_or_init(|| Regex::new(r"\s+").expect("a valid pattern")).replace_all(cut, " ");
    let name = name.trim().trim_matches(|c: char| c == '-' || c == '.' || c.is_whitespace());
    safe_file_name(if name.is_empty() { title.trim() } else { name })
}

/// Where new shows are downloaded: setting downloads.folder, or Downloads\Telly in your home folder.
fn downloads_folder(backend: &Backend) -> PathBuf {
    let chosen = backend.db.setting_str("downloads.folder");
    if !chosen.trim().is_empty() {
        return PathBuf::from(chosen.trim());
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    home.unwrap_or_else(|| backend.dirs.local.clone()).join("Downloads").join("Telly")
}

/// The magnet link's info hash, in lower case.
pub(crate) fn magnet_hash(magnet: &str) -> Option<String> {
    let at = magnet.find("btih:")? + 5;
    let hash: String = magnet[at..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (hash.len() == 40).then(|| hash.to_lowercase())
}

impl Downloads {
    /// Wakes the watcher to look at the downloads again now.
    pub fn poke(&self) {
        let (lock, cv) = &self.wake;
        if let Ok(mut woken) = lock.lock() {
            *woken = true;
            cv.notify_all();
        }
    }

    fn wait(&self, at_most: Duration) {
        let (lock, cv) = &self.wake;
        let Ok(woken) = lock.lock() else { return };
        if let Ok((mut woken, _)) = cv.wait_timeout_while(woken, at_most, |w| !*w) {
            *woken = false;
        }
    }

    /// `durations` for the files that are finished, and none (so they're
    /// left out, as still downloading) for the rest.
    pub fn unless_downloading(&self, paths: &[PathBuf], durations: impl Fn(&[PathBuf]) -> Vec<Option<f64>>) -> Vec<Option<f64>> {
        let unfinished = self.seen.lock().map(|s| s.unfinished.clone()).unwrap_or_default();
        let downloading = |p: &PathBuf| unfinished.contains(&key(&p.to_string_lossy()));
        if !paths.iter().any(downloading) {
            return durations(paths);
        }
        let ready: Vec<PathBuf> = paths.iter().filter(|p| !downloading(p)).cloned().collect();
        let mut lengths = durations(&ready).into_iter();
        paths.iter().map(|p| if downloading(p) { None } else { lengths.next().flatten() }).collect()
    }

    /// The client has stopped: nothing is downloading any more, as far as the last look goes.
    fn halted(&self) {
        if let Ok(mut seen) = self.seen.lock() {
            for t in seen.torrents.iter_mut().filter(|t| t.state != "done" && t.state != "error") {
                (t.state, t.speed, t.peers, t.eta) = ("paused".into(), 0, 0, None);
            }
        }
    }

    /// How many downloads have stopped on an error, at the last look.
    pub fn stopped(&self) -> usize {
        self.seen.lock().map(|s| s.torrents.iter().filter(|t| t.state == "error").count()).unwrap_or(0)
    }

    /// How the downloads are going, all together, from the last look.
    /// The info hashes of the downloads Telly has: those in the client, however they're
    /// doing, and those whose files are still being asked for. None until Telly has looked.
    pub fn there(&self) -> Option<HashSet<String>> {
        let seen = self.seen.lock().ok()?;
        seen.looked.then(|| seen.torrents.iter().filter(|t| t.id.is_some() || t.error.is_none()).map(|t| t.hash.clone()).collect())
    }

    pub fn summary(&self) -> Option<Summary> {
        let seen = self.seen.lock().ok()?;
        let going: Vec<&Torrent> = seen.torrents.iter().filter(|t| t.state != "done" && t.state != "error").collect();
        if going.is_empty() {
            return None;
        }
        let size: u64 = going.iter().map(|t| t.size).sum();
        let have: f64 = going.iter().map(|t| t.progress * t.size as f64).sum();
        Some(Summary { count: going.len(), progress: if size > 0 { have / size as f64 } else { 0.0 }, speed: going.iter().map(|t| t.speed).sum() })
    }
}

/// A speed limit from Settings (KB a second), in bytes a second: none when it's unset or 0.
fn limit(backend: &Backend, key: &str) -> Option<NonZeroU32> {
    NonZeroU32::new((backend.db.setting_i64(key, 0).clamp(0, 4_000_000) as u32) * 1024)
}

/// The computer's network connections by the names Windows gives them
/// ("Wi-Fi", "Mullvad"), each with whether it's connected now: it has an
/// address of its own. A VPN's tunnel is one of them, though it's no piece
/// of hardware; many VPNs make theirs when they connect and take it away
/// when they drop.
pub fn connections() -> Vec<(String, bool)> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};
    let own = |ip: std::net::IpAddr| match ip {
        std::net::IpAddr::V4(v4) => !v4.is_link_local() && !v4.is_unspecified(),
        std::net::IpAddr::V6(v6) => !v6.is_unicast_link_local() && !v6.is_unspecified(),
    };
    let mut all: Vec<(String, bool)> = Vec::new();
    // The computer's connection to itself isn't one to download through.
    for interface in NetworkInterface::show().unwrap_or_default().into_iter().filter(|i| !i.internal && !i.addr.iter().any(|a| a.ip().is_loopback())) {
        let connected = interface.addr.iter().any(|a| own(a.ip()));
        // Listed once, though some systems list a connection once for each of its addresses.
        match all.iter_mut().find(|(name, _)| *name == interface.name) {
            Some(listed) => listed.1 |= connected,
            None => all.push((interface.name, connected)),
        }
    }
    all.sort();
    all
}

/// The connection Settings has Telly download through, when it isn't connected now.
fn waiting_for(backend: &Backend) -> Option<String> {
    let chosen = backend.db.setting_str("downloads.adapter");
    let chosen = chosen.trim();
    (!chosen.is_empty() && !connections().iter().any(|(name, connected)| name == chosen && *connected)).then(|| chosen.to_string())
}

/// The drive `folder` is on (or will be), and how much room it has free, when the computer says.
fn drive_of(folder: &Path) -> Option<(PathBuf, u64)> {
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    disks.iter().filter(|d| folder.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len()).map(|d| (d.mount_point().to_path_buf(), d.available_space()))
}

/// "14.2 GB", "310 MB".
fn size_text(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB { format!("{:.1} GB", b / GB) } else { format!("{:.0} MB", (b / (1024.0 * 1024.0)).ceil()) }
}

/// How much of a release still has to be written into `folder`: its files' sizes, less what's there of each already.
fn still_to_write(torrent: &[u8], folder: &str) -> Option<u64> {
    let meta = librqbit::torrent_from_bytes(torrent).ok()?;
    let info = meta.info.data.validate().ok()?;
    let sizes = info.iter_file_details().filter(|f| !f.attrs().padding).map(|f| {
        let there = std::fs::metadata(Path::new(folder).join(f.filename.to_vec().join("/"))).map_or(0, |m| m.len());
        f.len.saturating_sub(there)
    });
    Some(sizes.sum())
}

/// Why a release that needs `needed` more bytes in `folder` isn't started:
/// its drive hasn't the room, with what Settings keeps free and what the
/// downloads `others` (each its folder and what it still needs) are still
/// to write there. None when there's room, or the drive doesn't say.
fn no_room(backend: &Backend, folder: &str, needed: u64, others: &[(String, u64)]) -> Option<String> {
    let (drive, free) = drive_of(Path::new(folder))?;
    let keep = backend.db.setting_i64("downloads.keepFree", 1).clamp(0, 10_000) as u64 * 1024 * 1024 * 1024;
    let queued: u64 = others.iter().filter(|(f, _)| drive_of(Path::new(f)).is_some_and(|(d, _)| d == drive)).map(|(_, left)| *left).sum();
    if needed.saturating_add(keep).saturating_add(queued) <= free {
        return None;
    }
    let mut why = format!("There isn’t room for this on {}: it needs {} and {} is free", drive.display(), size_text(needed), size_text(free));
    if queued > 0 {
        why.push_str(&format!(", {} of which your other downloads still need", size_text(queued)));
    }
    if keep > 0 {
        why.push_str(&format!(". Settings keeps {} free", size_text(keep)));
    }
    Some(why + ".")
}

/// The client, started on first use with what it was downloading when Telly closed.
fn session(backend: &Backend) -> Result<Arc<Session>> {
    let mut slot = backend.downloads.session.lock().map_err(|_| anyhow!("Telly’s downloader stopped"))?;
    // The connection it downloads through isn't up: it doesn't run until it is.
    if let Some(connection) = waiting_for(backend) {
        if let Some(running) = slot.take() {
            let _ = run(async move { running.stop().await });
            backend.downloads.halted();
        }
        return Err(anyhow!("Downloads are waiting for {connection} to connect: Settings → Library and downloads has Telly download only through it."));
    }
    if let Some(session) = slot.as_ref() {
        return Ok(session.clone());
    }
    let dir = backend.dirs.local.join("torrents");
    std::fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
    // Without what it last wrote down, an unfinished download checks its files, and finds all it got.
    let unfinished: Vec<String> = serde_json::from_value(backend.db.setting("downloads.unfinished").ok().flatten().unwrap_or_default()).unwrap_or_default();
    for hash in unfinished.iter().filter(|h| h.len() == 40 && h.chars().all(|c| c.is_ascii_hexdigit())) {
        let _ = std::fs::remove_file(dir.join(format!("{hash}.bitv")));
    }
    let options = SessionOptions {
        persistence: Some(SessionPersistenceConfig::Json { folder: Some(dir.clone()) }),
        fastresume: true,
        dht: Some(DhtSessionConfig {
            persistence: Some(DhtPersistenceConfig { config_filename: Some(dir.join("dht.json")), ..Default::default() }),
            ..Default::default()
        }),
        // Others can connect to it as well as it to them, through the router where it lets it and you do.
        listen: Some(ListenerOptions { enable_upnp_port_forwarding: backend.db.setting_bool("downloads.upnp", true), ..Default::default() }),
        ratelimits: LimitsConfig { upload_bps: limit(backend, "downloads.upLimit"), download_bps: limit(backend, "downloads.downLimit") },
        ..Default::default()
    };
    let folder = downloads_folder(backend);
    let session = run(Session::new_with_opts(folder, options))?.context("Telly’s downloader didn’t start")?;
    *slot = Some(session.clone());
    Ok(session)
}

/// A release's .torrent file, kept from when its sheet asked where it would go.
fn torrent_bytes(backend: &Backend, url: &str) -> Result<Vec<u8>> {
    let kept = backend.downloads.fetched.lock().ok().and_then(|f| f.as_ref().filter(|(from, _)| from == url).map(|(_, bytes)| bytes.clone()));
    if let Some(bytes) = kept {
        return Ok(bytes);
    }
    let bytes = crate::nyaa::torrent_file(url)?;
    if let Ok(mut fetched) = backend.downloads.fetched.lock() {
        *fetched = Some((url.to_string(), bytes.clone()));
    }
    Ok(bytes)
}

/// The files a .torrent holds, each as its path below the release's folder ("Season 1/01.mkv").
fn files_in(torrent: &[u8]) -> Result<Vec<String>> {
    let meta = librqbit::torrent_from_bytes(torrent).map_err(|e| anyhow!("That .torrent file can’t be read ({e:?})"))?;
    let info = meta.info.data.validate().map_err(|e| anyhow!("That .torrent file can’t be read ({e:?})"))?;
    Ok(info.iter_file_details().filter(|f| !f.attrs().padding).map(|f| f.filename.to_vec().join("/")).collect())
}

/// The files Telly's downloads write, and those of a download removed before it finished, as `key` writes their paths.
fn own_files(backend: &Backend, session: &Session) -> HashSet<String> {
    let mut own: HashSet<String> = session.with_torrents(|all| {
        all.flat_map(|(_, handle)| {
            let folder = handle.output_folder().to_path_buf();
            handle.with_metadata(|m| m.file_infos.iter().map(|f| key(&folder.join(&f.relative_filename).to_string_lossy())).collect::<Vec<_>>()).unwrap_or_default()
        })
        .collect()
    });
    own.extend(left(backend));
    own
}

/// The files of downloads removed before they finished, as `key` writes their paths: a release downloaded again carries on with them.
fn left(backend: &Backend) -> Vec<String> {
    backend.db.setting(LEFT).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

const LEFT: &str = "downloads.left";

/// The first of a release's `files` that's in `folder` already and isn't a
/// download's `own`: a rip, an extra or a subtitle file of yours, which the
/// client would write the release's file over.
fn over_a_file<'a>(folder: &str, files: &'a [String], own: &HashSet<String>) -> Option<&'a String> {
    files.iter().find(|f| {
        let path = Path::new(folder).join(f);
        path.symlink_metadata().is_ok() && !own.contains(&key(&path.to_string_lossy()))
    })
}

/// What Processing says of a magnet link nobody answered for.
fn found_none(e: anyhow::Error) -> String {
    format!("Its files couldn’t be found ({e:#})")
}

/// What Telly says rather than download over `file`.
fn would_write_over(file: &str, folder: &str) -> anyhow::Error {
    anyhow!("{file} is in {folder} already, and downloading this would write over it")
}

/// A release's videos that aren't extras.
fn episodes_in(files: &[String]) -> impl Iterator<Item = &String> {
    let videos = || files.iter().filter(|f| scan::is_video(Path::new(f)));
    let title_words = scan::title_words(videos().map(String::as_str));
    videos().filter(move |f| scan::extra_reason(f, &title_words).is_none())
}

/// Whether a release is more than one show: its videos, extras apart, are in
/// more than one folder (a folder for each season, one of movies).
fn several_parts(files: &[String]) -> bool {
    let folders: HashSet<String> = episodes_in(files).map(|f| f.rsplit_once('/').map_or("", |(folder, _)| folder).to_lowercase()).collect();
    folders.len() > 1
}

/// The AniList entry each new show's download is, by its folder, until the show is added.
fn entries(backend: &Backend) -> Vec<(String, i64)> {
    let kept: HashMap<String, i64> = backend.db.setting("downloads.entries").ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    kept.into_iter().collect()
}

/// Says which AniList entry the download in `folder` is, or (None) that it's been added or isn't one show.
fn set_entry(backend: &Backend, folder: &str, anilist: Option<i64>) -> Result<()> {
    let mut kept: HashMap<String, i64> = entries(backend).into_iter().filter(|(f, _)| key(f) != key(folder)).collect();
    if let Some(id) = anilist {
        kept.insert(folder.to_string(), id);
    }
    backend.db.set_setting("downloads.entries", &json!(kept))
}

/// Whether `folder` is there with something in it.
fn in_use(folder: &str) -> bool {
    std::fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_some())
}

/// Where a release goes: the folder `show`'s rips are in (or those of the
/// show in the library that is `anilist`), else `anilist`'s place in the
/// library `library` (or its usual one), else a folder named from `title`
/// in the downloads folder. `files` are the release's, when its .torrent
/// file could be read.
fn destination(backend: &Backend, title: &str, files: Option<&[String]>, show: Option<i64>, anilist: Option<i64>, library: Option<usize>) -> Result<Destination> {
    let shows = backend.db.shows()?;
    let whole = files.is_some_and(several_parts);
    let show = match show {
        Some(id) => Some(shows.iter().find(|s| s.id == id).ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?),
        // A release of several parts isn't more of the one you have.
        None if whole => None,
        None => anilist.and_then(|id| shows.iter().find(|s| s.anilist_id == Some(id))),
    };
    if let Some(show) = show {
        let rips: Vec<String> = backend.db.episodes(show.id)?.into_iter().map(|e| e.path).collect();
        return Ok(Destination { folder: library::rips_folder(&show.folder, &rips), show: Some(show.id), sorted: false, joins: true, library: None });
    }
    let unsorted = downloads_folder(backend).join(folder_name(title)).to_string_lossy().into_owned();
    let Some(id) = anilist else { return Ok(Destination { folder: unsorted, show: None, sorted: false, joins: false, library: None }) };
    let graph = series::around(backend, id)?;
    let libraries = library::libraries(backend)?;
    let library = library.or_else(|| library::usual(&libraries, &graph, &shows, id));
    // A season's folder with something in it is another show's, unless it's this entry's own download so far.
    let waiting = entries(backend);
    let taken = |folder: &str| in_use(folder) && !waiting.iter().any(|(f, entry)| key(f) == key(folder) && *entry == id);
    let placed = library.and_then(|i| libraries.get(i)).and_then(|chosen| library::place(chosen, &graph, &shows, id, whole, taken));
    Ok(Destination { sorted: placed.is_some(), folder: placed.unwrap_or(unsorted), show: None, joins: !whole, library })
}

/// The anime a release's name finds on AniList, the likeliest first. `count`
/// is how many episodes the release holds, when that's known.
fn find(title: &str, count: usize) -> Result<Vec<Anime>> {
    let name = folder_name(title);
    // "(Season 02)" and "S02E06" are cut from the name, but say which season is meant.
    let season = anilist::season(title).or_else(|| scan::episode_number(title).and_then(|(season, _)| season)).filter(|s| *s > 1);
    let asked = match season {
        Some(s) if anilist::season(&name).is_none() => format!("{name} Season {s}"),
        _ => name.clone(),
    };
    let mut found = anilist::search_loosely(&name)?;
    anilist::rank(&mut found, &asked, count);
    anilist::follow_sequels(&mut found, &asked)?;
    Ok(found)
}

/// What a release's sheet shows before it downloads: the anime its name
/// finds on AniList (`matches`, none when `anilist` says which it is or it's
/// for `show`), the one it's taken to be, where that one's download goes (in
/// `library`, or its usual one) and the libraries it could go in.
pub fn plan(backend: &Backend, title: &str, torrent: &str, show: Option<i64>, anilist: Option<i64>, library: Option<usize>) -> Result<Value> {
    let files = torrent_bytes(backend, torrent).ok().and_then(|bytes| files_in(&bytes).ok());
    let (matches, error) = match (show, anilist) {
        (None, None) => match find(title, files.as_deref().map_or(0, |f| episodes_in(f).count())) {
            Ok(found) => (found, None),
            Err(e) => (Vec::new(), Some(format!("{e:#}"))),
        },
        _ => (Vec::new(), None),
    };
    let anime = match anilist {
        Some(id) => Some(anilist::by_id(id)?),
        None => matches.first().cloned(),
    };
    let to = destination(backend, title, files.as_deref(), show, anime.as_ref().map(|a| a.id), library)?;
    Ok(json!({ "matches": matches, "anime": anime, "to": to, "libraries": library::libraries(backend)?, "error": error }))
}

/// Hands a release to the client: into `show`'s folder, the place in
/// `library` (or its usual one) of the AniList entry `anilist`, or a new
/// show's folder named from `title` (`destination`). `torrent` is its
/// .torrent file's address. Returns the folder.
pub fn add(backend: &Arc<Backend>, magnet: &str, torrent: &str, title: &str, show: Option<i64>, anilist: Option<i64>, library: Option<usize>) -> Result<String> {
    if !magnet.starts_with("magnet:") {
        return Err(anyhow!("That isn’t a magnet link"));
    }
    // Without its .torrent file, the release's list of files comes later, from the people sharing it.
    let fetched = torrent_bytes(backend, torrent);
    let files = fetched.as_ref().ok().and_then(|bytes| files_in(bytes).ok());
    let to = destination(backend, title, files.as_deref(), show, anilist, library)?;
    let folder = to.folder.clone();
    let session = session(backend)?;
    // The client writes a release's files over any of the same name there: never over what's yours.
    let own = own_files(backend, &session);
    if let Some(file) = files.as_deref().and_then(|files| over_a_file(&folder, files, &own)) {
        return Err(would_write_over(file, &folder));
    }
    // A magnet link's size isn't known until the people sharing it say.
    if let Some(needed) = fetched.as_ref().ok().and_then(|bytes| still_to_write(bytes, &folder)) {
        let others: Vec<(String, u64)> = session.with_torrents(|all| {
            all.map(|(_, handle)| (handle.output_folder().to_string_lossy().into_owned(), handle.stats())).filter(|(_, s)| !s.finished).map(|(f, s)| (f, s.total_bytes.saturating_sub(s.progress_bytes))).collect()
        });
        if let Some(why) = no_room(backend, &folder, needed, &others) {
            return Err(anyhow!("{why}"));
        }
    }
    std::fs::create_dir_all(&folder).with_context(|| format!("Telly couldn’t make the folder {folder}"))?;
    if to.sorted {
        library::make_room(backend, &folder)?;
    }
    if let (None, true, Some(id)) = (to.show, to.joins, anilist) {
        set_entry(backend, &folder, Some(id))?;
    }
    let options = |list_only: bool| AddTorrentOptions { output_folder: Some(folder.clone()), overwrite: true, list_only, ..Default::default() };
    backend.db.set_setting("downloads.used", &Value::Bool(true))?;
    match fetched {
        Ok(bytes) => {
            let options = options(false);
            let added = run(async move { session.add_torrent(AddTorrent::from_bytes(bytes), Some(options)).await.map(|r| matches!(r, AddTorrentResponse::Added(..))) })?
                .context("Telly couldn’t start the download")?;
            if !added {
                return Err(anyhow!("Telly is downloading this one already"));
            }
        }
        // The magnet link's list of files comes from the people sharing it, which can take a while.
        // Nothing is written until the list is here and none of its files is one of yours.
        Err(_) => {
            let hash = magnet_hash(magnet).ok_or_else(|| anyhow!("That magnet link has no info hash"))?;
            let finding = Finding { hash: hash.clone(), name: title.to_string(), folder: folder.clone(), error: None };
            let mut waiting = backend.downloads.finding.lock().map_err(|_| anyhow!("Telly’s downloader stopped"))?;
            // Asked for again after nobody answered: the one that failed gives way.
            waiting.retain(|f| f.hash != hash);
            waiting.push(finding);
            drop(waiting);
            let (magnet, backend, folder) = (magnet.to_string(), backend.clone(), folder.clone());
            let (list, options) = (options(true), options(false));
            runtime().spawn(async move {
                let added = match session.add_torrent(AddTorrent::Url(Cow::Owned(magnet)), Some(list)).await {
                    Ok(AddTorrentResponse::ListOnly(listed)) => {
                        let files: Vec<String> = listed.info.iter_file_details().filter(|f| !f.attrs().padding).map(|f| f.filename.to_vec().join("/")).collect();
                        match over_a_file(&folder, &files, &own) {
                            Some(file) => Err(format!("{:#}", would_write_over(file, &folder))),
                            None => session.add_torrent(AddTorrent::from_bytes(listed.torrent_bytes), Some(options)).await.map(|_| ()).map_err(found_none),
                        }
                    }
                    Ok(_) => Ok(()),
                    Err(e) => Err(found_none(e)),
                };
                if let Ok(mut finding) = backend.downloads.finding.lock() {
                    match added {
                        Ok(_) => finding.retain(|f| f.hash != hash),
                        Err(e) => {
                            if let Some(f) = finding.iter_mut().find(|f| f.hash == hash) {
                                f.error = Some(e);
                            }
                        }
                    }
                }
                backend.downloads.poke();
            });
        }
    }
    // A show you follow gets its next episodes from this release (follow.rs), unless it's several episodes in one.
    if let Some(show) = to.show.filter(|_| !crate::nyaa::several_episodes(title)) {
        backend.db.set_setting(&crate::follow::like_setting(show), &json!(title))?;
    }
    backend.downloads.poke();
    Ok(folder)
}

/// Pauses a download, or carries on with one.
pub fn pause(backend: &Backend, id: usize, paused: bool) -> Result<()> {
    let session = session(backend)?;
    let handle = session.get(id.into()).ok_or_else(|| anyhow!("That download isn’t there any more"))?;
    run(async move { if paused { session.pause(&handle).await } else { session.unpause(&handle).await } })??;
    backend.downloads.poke();
    Ok(())
}

/// Takes a download off the list, leaving whatever it downloaded where it is.
pub fn remove(backend: &Backend, id: usize) -> Result<()> {
    let session = session(backend)?;
    let folder = session.get(id.into()).map(|handle| handle.output_folder().to_string_lossy().into_owned());
    // What it leaves half there is still a download's: the release downloaded again carries on with it.
    if let Some(handle) = session.get(id.into()) {
        let mut kept = left(backend);
        kept.extend(torrent_now(id, &handle).1);
        kept.retain(|f| Path::new(f).symlink_metadata().is_ok());
        kept.sort();
        kept.dedup();
        backend.db.set_setting(LEFT, &json!(kept))?;
    }
    run(async move { session.delete(id.into(), false).await })??;
    // A new show's download that's gone has no show to become.
    if let Some(folder) = folder {
        set_entry(backend, &folder, None)?;
    }
    backend.downloads.poke();
    Ok(())
}

/// One download as it is now, and its files that aren't finished.
fn torrent_now(id: usize, handle: &ManagedTorrent) -> (Torrent, Vec<String>) {
    let stats = handle.stats();
    let folder = handle.output_folder().to_string_lossy().into_owned();
    let files: Vec<(PathBuf, u64, bool)> = handle
        .with_metadata(|m| m.file_infos.iter().map(|f| (f.relative_filename.clone(), f.len, f.attrs.padding)).collect())
        .unwrap_or_default();
    let real: Vec<(usize, &(PathBuf, u64, bool))> = files.iter().enumerate().filter(|(_, f)| !f.2).collect();
    let checking = matches!(stats.state, TorrentStatsState::Initializing { .. });
    let have = |i: usize| stats.file_progress.get(i).copied();
    let finished_file = |i: usize, len: u64| stats.finished || have(i).is_some_and(|h| h >= len);
    // While it checks what's on disk, none of its files counts as finished; nor
    // while it's stopped on an error (the client then says nothing of each file),
    // unless it had finished: a half-written episode isn't added.
    let stopped = matches!(stats.state, TorrentStatsState::Error) && !stats.finished;
    let unfinished: Vec<String> = real
        .iter()
        .filter(|(i, f)| checking || stopped || !finished_file(*i, f.1))
        .map(|(_, f)| key(&Path::new(&folder).join(&f.0).to_string_lossy()))
        .collect();
    let files_done = if checking { 0 } else { real.iter().filter(|(i, f)| finished_file(*i, f.1)).count() };
    let speed = stats.live.as_ref().map_or(0, |l| (l.download_speed.mbps * 1024.0 * 1024.0) as u64);
    let peers = stats.live.as_ref().map_or(0, |l| l.snapshot.peer_stats.live);
    let left = stats.total_bytes.saturating_sub(stats.progress_bytes);
    let state = match stats.state {
        TorrentStatsState::Error => "error",
        _ if stats.finished => "done",
        TorrentStatsState::Initializing { .. } => "checking",
        TorrentStatsState::Paused => "paused",
        TorrentStatsState::Live if peers == 0 => "waiting",
        TorrentStatsState::Live => "downloading",
    };
    let torrent = Torrent {
        id: Some(id),
        hash: handle.info_hash().as_string(),
        name: handle.name().unwrap_or_else(|| handle.info_hash().as_string()),
        progress: if stats.total_bytes > 0 { (stats.progress_bytes as f64 / stats.total_bytes as f64).min(1.0) } else { 0.0 },
        size: stats.total_bytes,
        speed,
        eta: (speed > 0 && left > 0 && state == "downloading").then(|| left / speed),
        state: state.into(),
        error: stats.error.clone(),
        folder,
        peers,
        show: None,
        files_done,
        files: real.len(),
    };
    (torrent, unfinished)
}

/// The finished downloads that are being shared (`live`), or that aren't (paused).
fn finished(session: &Session, live: bool) -> Vec<Arc<ManagedTorrent>> {
    let is = |state: &TorrentStatsState| if live { matches!(state, TorrentStatsState::Live) } else { matches!(state, TorrentStatsState::Paused) };
    session.with_torrents(|all| all.filter(|(_, handle)| handle.stats().finished && is(&handle.stats().state)).map(|(_, handle)| handle.clone()).collect())
}

/// What changing one of the downloads' settings does at once: a speed limit
/// holds from now, finished downloads are shared again, and the client is
/// stopped to start again with or without asking the router.
pub fn setting_changed(backend: &Backend, key: &str) {
    let running = backend.downloads.session.lock().ok().and_then(|slot| slot.clone());
    match (key, running) {
        ("downloads.downLimit" | "downloads.upLimit", Some(session)) => {
            session.ratelimits.set_download_bps(limit(backend, "downloads.downLimit"));
            session.ratelimits.set_upload_bps(limit(backend, "downloads.upLimit"));
        }
        ("downloads.share", Some(session)) if backend.db.setting_bool("downloads.share", true) => {
            for handle in finished(&session, false) {
                let session = session.clone();
                let _ = run(async move { session.unpause(&handle).await });
            }
        }
        ("downloads.upnp", Some(session)) => {
            if let Ok(mut slot) = backend.downloads.session.lock() {
                *slot = None;
            }
            let _ = run(async move { session.stop().await });
        }
        _ => {}
    }
    backend.downloads.poke();
}

/// Looks at how the downloads are going. When a file has finished in a
/// show's folder since the last look, the show looks in its folder again.
pub fn look(backend: &Arc<Backend>) -> Result<()> {
    let session = session(backend)?;
    // A finished download stops being shared, when Settings says not to share them.
    if !backend.db.setting_bool("downloads.share", true) {
        for handle in finished(&session, true) {
            let session = session.clone();
            let _ = run(async move { session.pause(&handle).await });
        }
    }
    let shows = backend.db.shows()?;
    let mut unfinished = HashSet::new();
    let mut torrents: Vec<Torrent> = session.with_torrents(|all| {
        all.map(|(id, handle)| {
            let (torrent, files) = torrent_now(id, handle);
            (torrent, files)
        })
        .collect::<Vec<_>>()
    })
    .into_iter()
    .map(|(torrent, files)| {
        unfinished.extend(files);
        torrent
    })
    .collect();
    // Newest first, and those whose files are being found before them.
    torrents.sort_by(|a, b| b.id.cmp(&a.id));
    let finding = backend.downloads.finding.lock().map(|f| f.clone()).unwrap_or_default();
    let found: Vec<Torrent> = finding
        .into_iter()
        .filter(|f| !torrents.iter().any(|t| t.hash == f.hash))
        .map(|f| Torrent {
            id: None,
            state: (if f.error.is_some() { "error" } else { "finding" }).into(),
            hash: f.hash,
            name: f.name,
            error: f.error,
            folder: f.folder,
            progress: 0.0,
            size: 0,
            speed: 0,
            eta: None,
            peers: 0,
            show: None,
            files_done: 0,
            files: 0,
        })
        .collect();
    torrents.splice(0..0, found);
    for t in &mut torrents {
        t.show = shows.iter().filter(|s| inside(&t.folder, &s.folder)).max_by_key(|s| s.folder.len()).map(|s| s.id);
    }
    let waiting = entries(backend);
    // Shows with a file that's finished since the last look, and new shows with their first.
    let mut look_in: Vec<i64> = Vec::new();
    let mut new_shows: Vec<(String, i64, bool)> = Vec::new();
    {
        let mut seen = backend.downloads.seen.lock().map_err(|_| anyhow!("the downloads are being looked at already"))?;
        for t in &torrents {
            let before = seen.done.get(&t.hash).copied().unwrap_or(0);
            // A new show that wasn't added when Telly closed is tried again as it opens.
            if t.files_done > before || (!seen.looked && t.files_done > 0) {
                if let Some((folder, entry)) = waiting.iter().find(|(f, _)| key(f) == key(&t.folder)) {
                    new_shows.push((folder.clone(), *entry, t.state == "done"));
                }
            }
            if seen.looked && t.files_done > before {
                if let Some(show) = t.show.filter(|s| !look_in.contains(s)) {
                    look_in.push(show);
                }
            }
        }
        seen.done = torrents.iter().map(|t| (t.hash.clone(), t.files_done)).collect();
        let going: Vec<&str> = torrents.iter().filter(|t| t.id.is_some() && t.state != "done").map(|t| t.hash.as_str()).collect();
        let before: Vec<&str> = seen.torrents.iter().filter(|t| t.id.is_some() && t.state != "done").map(|t| t.hash.as_str()).collect();
        if going != before || !seen.looked {
            backend.db.set_setting("downloads.unfinished", &json!(going))?;
        }
        seen.torrents = torrents;
        seen.unfinished = unfinished;
        seen.looked = true;
    }
    if !look_in.is_empty() {
        let backend = backend.clone();
        std::thread::spawn(move || {
            for show in look_in {
                let _ = backend.rescan(show);
            }
        });
    }
    for (folder, entry, finished) in new_shows {
        // One go at a folder at a time.
        if !backend.downloads.adding.lock().is_ok_and(|mut adding| adding.insert(key(&folder))) {
            continue;
        }
        let is_show = shows.iter().any(|s| key(&s.folder) == key(&folder));
        let backend = backend.clone();
        std::thread::spawn(move || {
            let added = is_show || backend.add_download(&folder, entry).unwrap_or(false);
            // A finished download that isn't one show is offered in the Library instead.
            if added || finished {
                let _ = set_entry(&backend, &folder, None);
            }
            if let Ok(mut adding) = backend.downloads.adding.lock() {
                adding.remove(&key(&folder));
            }
        });
    }
    Ok(())
}

/// Starts the client if Telly has downloaded anything, so what it was
/// downloading carries on, and looks at the downloads now and then: every
/// 3 seconds while something is downloading, every minute otherwise, every
/// second while there's one connection to download through, and at once
/// when woken.
pub fn start(backend: Arc<Backend>) {
    // Which files aren't finished is known before any folder is looked in.
    if backend.db.setting_bool("downloads.used", false) {
        let _ = look(&backend);
    }
    std::thread::Builder::new()
        .name("telly-downloads".into())
        .spawn(move || loop {
            let busy = backend.db.setting_bool("downloads.used", false) && look(&backend).is_ok() && backend.downloads.summary().is_some();
            // With one connection to download through, it's looked for every second, so the client stops within one of its going.
            let watched = backend.db.setting_bool("downloads.used", false) && !backend.db.setting_str("downloads.adapter").trim().is_empty();
            backend.downloads.wait(Duration::from_secs(if watched { 1 } else if busy { 3 } else { 60 }));
        })
        .ok();
}

/// Whether a download that isn't finished, or a new show's that hasn't
/// joined the library yet, is in `folder` or has `folder` in it. Such a
/// folder is the downloader's to add from (`look`), not a library's.
pub fn going_in(backend: &Backend, folder: &str) -> bool {
    let overlaps = |other: &str| inside(other, folder) || inside(folder, other);
    let downloading = backend.downloads.seen.lock().is_ok_and(|seen| seen.torrents.iter().any(|t| t.state != "done" && overlaps(&t.folder)));
    downloading || entries(backend).iter().any(|(waiting, _)| overlaps(waiting))
}

/// Folders of finished downloads that aren't a show in the library, for it to offer.
pub fn offers(backend: &Backend) -> Vec<Offer> {
    let Ok(seen) = backend.downloads.seen.lock() else { return Vec::new() };
    if seen.torrents.is_empty() {
        return Vec::new();
    }
    let shows = backend.db.shows().unwrap_or_default();
    // A download you said the show of adds itself.
    let waiting = entries(backend);
    let adds_itself = |t: &Torrent| waiting.iter().any(|(f, _)| key(f) == key(&t.folder));
    let mut offers: Vec<Offer> = Vec::new();
    for t in seen.torrents.iter().filter(|t| t.files_done > 0 && !adds_itself(t) && !shows.iter().any(|s| inside(&t.folder, &s.folder))) {
        let downloading = t.state != "done";
        match offers.iter_mut().find(|o| key(&o.folder) == key(&t.folder)) {
            Some(o) => o.downloading |= downloading,
            None => offers.push(Offer {
                name: Path::new(&t.folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| t.folder.clone()),
                folder: t.folder.clone(),
                downloading,
            }),
        }
    }
    offers
}

/// The downloads as they are now, for Processing.
pub fn view(backend: &Arc<Backend>) -> Value {
    let used = backend.db.setting_bool("downloads.used", false);
    let error = if used { look(backend).err().map(|e| format!("{e:#}")) } else { None };
    let torrents = backend.downloads.seen.lock().map(|s| s.torrents.clone()).unwrap_or_default();
    json!({
        "torrents": torrents,
        "offers": offers(backend),
        "error": error,
        "folder": downloads_folder(backend),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_new_shows_folder_from_the_release() {
        assert_eq!(folder_name("[Erai-raws] Sousou no Frieren 2nd Season - 09 [1080p CR WEB-DL AVC AAC][MultiSub][15B7C196]"), "Sousou no Frieren 2nd Season");
        assert_eq!(folder_name("[ToonsHub] Frieren Beyond Journeys End S02E06 1080p BILI WEB-DL AAC2.0 H.265 (Sousou no Frieren, Multi-Subs)"), "Frieren Beyond Journeys End");
        assert_eq!(folder_name("[Judas] Sousou no Frieren (Season 02) [1080p][HEVC x265 10bit][Dual-Audio][Multi-Subs] (Batch)"), "Sousou no Frieren");
        assert_eq!(folder_name("[AUTISM] Sousou no Frieren - S01v2 (BD Remux 1080p AVC FLAC)"), "Sousou no Frieren");
        assert_eq!(folder_name("[SubsPlease] Kusuriya no Hitorigoto - 30 (1080p) [A1B2C3D4].mkv"), "Kusuriya no Hitorigoto");
        assert_eq!(folder_name("Mononoke.2007.1080p.BluRay.FLAC.x264"), "Mononoke 2007");
        assert_eq!(folder_name("Re:Zero kara Hajimeru Isekai Seikatsu - 01"), "Re_Zero kara Hajimeru Isekai Seikatsu");
        assert_eq!(folder_name("[Group] [1080p]"), "[Group] [1080p]");
    }

    #[test]
    fn a_folder_holds_what_is_in_it() {
        assert!(inside("/anime/Frieren", "/anime/Frieren/"));
        assert!(inside("/anime/Frieren/Season 2", "/anime/Frieren"));
        assert!(!inside("/anime/Frieren 2", "/anime/Frieren"));
        assert!(inside(r"D:\Anime\Frieren\ep 01.mkv", "D:/Anime/Frieren"));
        assert!(!inside("/anime", ""));
    }

    #[test]
    fn a_release_with_a_folder_for_each_season_is_several_parts() {
        let files = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert!(several_parts(&files(&["S1/Overlord - S01E01.mkv", "S2/Overlord - S02E01.mkv"])));
        assert!(several_parts(&files(&["Season 1/01.mkv", "Movies/The Undead King.mkv"])));
        // Extras, and what isn't a video, aren't a part.
        assert!(!several_parts(&files(&["Film.mkv", "Film - NCOP.mkv", "Extras/Interview.mkv", "Scans/cover.jpg"])));
        assert!(!several_parts(&files(&["Show - 01.mkv", "Show - 02.mkv"])));
        assert!(!several_parts(&files(&["season 1/01.mkv", "Season 1/02.mkv"])));
    }

    #[test]
    fn a_release_isn_t_downloaded_over_what_s_there() {
        let dir = std::env::temp_dir().join(format!("telly-over-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Extras")).unwrap();
        // A rip, and an extra and a subtitle file Telly has no episode for.
        for name in ["[Judas] Show - S01E01.mkv", "Extras/NCOP.mkv", "[Judas] Show - S01E03.ja.srt", "[Judas] Show - S01E04.mkv"] {
            std::fs::write(dir.join(name), b"yours").unwrap();
        }
        let folder = dir.to_string_lossy().into_owned();
        let files = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let none = HashSet::new();
        // Their BD batch, named as the WEB one you have.
        assert_eq!(over_a_file(&folder, &files(&["[Judas] Show - S01E02.mkv", "[Judas] Show - S01E01.mkv"]), &none).map(String::as_str), Some("[Judas] Show - S01E01.mkv"));
        assert_eq!(over_a_file(&folder, &files(&["Extras/NCOP.mkv"]), &none).map(String::as_str), Some("Extras/NCOP.mkv"));
        assert_eq!(over_a_file(&folder, &files(&["[Judas] Show - S01E03.ja.srt"]), &none).map(String::as_str), Some("[Judas] Show - S01E03.ja.srt"));
        assert_eq!(over_a_file(&folder, &files(&["[Judas] Show - S01E02.mkv"]), &none), None);
        // What a download of Telly's left half there is carried on with.
        let own: HashSet<String> = [key(&dir.join("[Judas] Show - S01E04.mkv").to_string_lossy())].into();
        assert_eq!(over_a_file(&folder, &files(&["[Judas] Show - S01E04.mkv"]), &own), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_download_there_isn_t_room_for_isn_t_started() {
        let dir = std::env::temp_dir().join(format!("telly-room-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let folder = dir.join("Show").to_string_lossy().into_owned();
        let (_, free) = drive_of(Path::new(&folder)).expect("the drive the temp folder is on");
        backend.db.set_setting("downloads.keepFree", &json!(0)).unwrap();
        assert_eq!(no_room(&backend, &folder, free / 2, &[]), None);
        assert!(no_room(&backend, &folder, free.saturating_add(1 << 30), &[]).unwrap().starts_with("There isn’t room for this on "));
        // What the other downloads on the drive still need counts, and so does what Settings keeps free.
        let others = [(dir.join("Other").to_string_lossy().into_owned(), free)];
        assert!(no_room(&backend, &folder, free / 2, &others).unwrap().contains("your other downloads still need"));
        backend.db.set_setting("downloads.keepFree", &json!(10_000)).unwrap();
        assert!(no_room(&backend, &folder, 1, &[]).unwrap().ends_with("Settings keeps 10000.0 GB free."));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_speed_limit_is_none_until_set() {
        let dir = std::env::temp_dir().join(format!("telly-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        assert_eq!(limit(&backend, "downloads.downLimit"), None);
        backend.db.set_setting("downloads.downLimit", &json!(500)).unwrap();
        assert_eq!(limit(&backend, "downloads.downLimit").map(NonZeroU32::get), Some(500 * 1024));
        backend.db.set_setting("downloads.downLimit", &json!(0)).unwrap();
        assert_eq!(limit(&backend, "downloads.downLimit"), None);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn downloads_wait_for_the_connection_chosen() {
        let dir = std::env::temp_dir().join(format!("telly-connection-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        assert_eq!(waiting_for(&backend), None);
        backend.db.set_setting("downloads.adapter", &json!("a connection this computer hasn’t")).unwrap();
        assert_eq!(waiting_for(&backend).as_deref(), Some("a connection this computer hasn’t"));
        // The client doesn't start without it.
        assert!(session(&backend).err().is_some_and(|e| e.to_string().starts_with("Downloads are waiting for ")));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_a_magnet_links_hash() {
        assert_eq!(magnet_hash("magnet:?xt=urn:btih:E738213EBA3CB50034E8C37955D7F58C7B9AE53F&dn=x").as_deref(), Some("e738213eba3cb50034e8c37955d7f58c7b9ae53f"));
        assert_eq!(magnet_hash("magnet:?dn=x"), None);
    }
}
