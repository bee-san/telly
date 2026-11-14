//! Telly's own torrent client (librqbit) does its downloading: you pick a
//! release on nyaa.si (nyaa.rs), and Telly downloads it, carrying on from
//! where it was whenever the connection drops, and when Telly opens again.
//!
//! Each download's files go straight into a show's folder: the folder of a
//! show in the library, or, for a show that isn't, a folder named for it in
//! the downloads folder. A file that isn't finished counts as unreadable when
//! Telly looks in a folder, so no episode is added half there. When a file
//! finishes in a show's folder, Telly looks in the folder at once; a new
//! show's folder is offered in the Library to add.
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
//! Settings: downloads.folder (where new shows go; Downloads\Telly in your
//! home folder unless set), downloads.used (Telly has downloaded something,
//! so it starts the client when it opens) and downloads.unfinished (the info
//! hashes of the downloads that weren't finished at the last look).

use crate::backend::Backend;
use crate::util::safe_file_name;
use anyhow::{anyhow, Context, Result};
use librqbit::dht::DhtPersistenceConfig;
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
fn magnet_hash(magnet: &str) -> Option<String> {
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

    /// How the downloads are going, all together, from the last look.
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

/// The client, started on first use with what it was downloading when Telly closed.
fn session(backend: &Backend) -> Result<Arc<Session>> {
    let mut slot = backend.downloads.session.lock().map_err(|_| anyhow!("Telly’s downloader stopped"))?;
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
        // Others can connect to it as well as it to them, through the router where it lets it.
        listen: Some(ListenerOptions { enable_upnp_port_forwarding: true, ..Default::default() }),
        ..Default::default()
    };
    let folder = downloads_folder(backend);
    let session = run(Session::new_with_opts(folder, options))?.context("Telly’s downloader didn’t start")?;
    *slot = Some(session.clone());
    Ok(session)
}

/// Hands a release to the client: into `show`'s folder, or a new show's
/// folder named from `title`. `torrent` is its .torrent file's address.
/// Returns the folder.
pub fn add(backend: &Arc<Backend>, magnet: &str, torrent: &str, title: &str, show: Option<i64>) -> Result<String> {
    if !magnet.starts_with("magnet:") {
        return Err(anyhow!("That isn’t a magnet link"));
    }
    let folder = match show {
        Some(id) => backend.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?.folder,
        None => downloads_folder(backend).join(folder_name(title)).to_string_lossy().into_owned(),
    };
    std::fs::create_dir_all(&folder).with_context(|| format!("Telly couldn’t make the folder {folder}"))?;
    let session = session(backend)?;
    let options = AddTorrentOptions { output_folder: Some(folder.clone()), overwrite: true, ..Default::default() };
    backend.db.set_setting("downloads.used", &Value::Bool(true))?;
    match crate::nyaa::torrent_file(torrent) {
        Ok(bytes) => {
            let added = run(async move { session.add_torrent(AddTorrent::from_bytes(bytes), Some(options)).await.map(|r| matches!(r, AddTorrentResponse::Added(..))) })?
                .context("Telly couldn’t start the download")?;
            if !added {
                return Err(anyhow!("Telly is downloading this one already"));
            }
        }
        // The magnet link's list of files comes from the people sharing it, which can take a while.
        Err(_) => {
            let hash = magnet_hash(magnet).ok_or_else(|| anyhow!("That magnet link has no info hash"))?;
            let finding = Finding { hash: hash.clone(), name: title.to_string(), folder: folder.clone(), error: None };
            backend.downloads.finding.lock().map_err(|_| anyhow!("Telly’s downloader stopped"))?.push(finding);
            let (magnet, backend) = (magnet.to_string(), backend.clone());
            runtime().spawn(async move {
                let added = session.add_torrent(AddTorrent::Url(Cow::Owned(magnet)), Some(options)).await;
                if let Ok(mut finding) = backend.downloads.finding.lock() {
                    match added {
                        Ok(_) => finding.retain(|f| f.hash != hash),
                        Err(e) => {
                            if let Some(f) = finding.iter_mut().find(|f| f.hash == hash) {
                                f.error = Some(format!("Its files couldn’t be found ({e:#})"));
                            }
                        }
                    }
                }
                backend.downloads.poke();
            });
        }
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
    run(async move { session.delete(id.into(), false).await })??;
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
    // While it checks what's on disk, none of its files counts as finished.
    let unfinished: Vec<String> = if matches!(stats.state, TorrentStatsState::Error) {
        Vec::new()
    } else {
        real.iter()
            .filter(|(i, f)| checking || !finished_file(*i, f.1))
            .map(|(_, f)| key(&Path::new(&folder).join(&f.0).to_string_lossy()))
            .collect()
    };
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

/// Looks at how the downloads are going. When a file has finished in a
/// show's folder since the last look, the show looks in its folder again.
pub fn look(backend: &Arc<Backend>) -> Result<()> {
    let session = session(backend)?;
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
    // Shows with a file that's finished since the last look.
    let mut look_in: Vec<i64> = Vec::new();
    {
        let mut seen = backend.downloads.seen.lock().map_err(|_| anyhow!("the downloads are being looked at already"))?;
        for t in &torrents {
            let before = seen.done.get(&t.hash).copied().unwrap_or(0);
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
    Ok(())
}

/// Starts the client if Telly has downloaded anything, so what it was
/// downloading carries on, and looks at the downloads now and then: every
/// 3 seconds while something is downloading, every minute otherwise, and at
/// once when woken.
pub fn start(backend: Arc<Backend>) {
    // Which files aren't finished is known before any folder is looked in.
    if backend.db.setting_bool("downloads.used", false) {
        let _ = look(&backend);
    }
    std::thread::Builder::new()
        .name("telly-downloads".into())
        .spawn(move || loop {
            let busy = backend.db.setting_bool("downloads.used", false) && look(&backend).is_ok() && backend.downloads.summary().is_some();
            backend.downloads.wait(Duration::from_secs(if busy { 3 } else { 60 }));
        })
        .ok();
}

/// Folders of finished downloads that aren't a show in the library, for it to offer.
pub fn offers(backend: &Backend) -> Vec<Offer> {
    let Ok(seen) = backend.downloads.seen.lock() else { return Vec::new() };
    if seen.torrents.is_empty() {
        return Vec::new();
    }
    let shows = backend.db.shows().unwrap_or_default();
    let mut offers: Vec<Offer> = Vec::new();
    for t in seen.torrents.iter().filter(|t| t.files_done > 0 && !shows.iter().any(|s| inside(&t.folder, &s.folder))) {
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
    fn reads_a_magnet_links_hash() {
        assert_eq!(magnet_hash("magnet:?xt=urn:btih:E738213EBA3CB50034E8C37955D7F58C7B9AE53F&dn=x").as_deref(), Some("e738213eba3cb50034e8c37955d7f58c7b9ae53f"));
        assert_eq!(magnet_hash("magnet:?dn=x"), None);
    }
}
