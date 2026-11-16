//! Your libraries: the folders your shows are kept in, and where a download
//! goes when you've said which show it is. Settings → Libraries lists them,
//! each a name, a kind (series or movies) and its folders, one of which takes
//! downloads. The Library lists every show together whatever folder it's
//! in, and its Filter can leave a library's shows out.
//!
//! Each library's folders are read, and what's in them that isn't in Telly
//! yet is added: when the library is added (or a folder is added to one),
//! every 10 minutes after the look for new episodes, and when you press Look
//! for new shows.
//!
//! - Each folder in it is an anime, added as Add media… adds one when
//!   nothing in its sheet is changed (`add_as_read` in backend.rs). Of an
//!   anime some of whose seasons are in Telly, the seasons, movies and OVAs
//!   that turned up since are added.
//! - In a movies library, each video lying loose in it is a film.
//! - One at a time, in the background. The folders a library's adding or
//!   your pressing asked for are kept, so Telly carries on with them when it
//!   next starts, and waits a minute at a time while AniList can't be asked.
//!   A folder no library has any more isn't read any further.
//! - A video that's been looked at and isn't in Telly is passed over from
//!   then on: an extra, a second copy of an episode, a show you removed or
//!   left out in Add media…. So a look costs a listing of each folder, and
//!   only a folder with a video that's new is read (ffprobe) and matched
//!   (AniList). Add media… still adds what's passed over.
//! - A folder Telly is downloading into is the downloader's to add
//!   (torrents.rs), and one open in Add media… is yours.
//!
//! What's in a library's folders is what's in Telly, so a show leaves Telly
//! with its folder (as Remove… on its page removes it: your rips are never
//! touched, and Stats keeps what you watched):
//!
//! - A folder no library has any more (its library removed, or the folder
//!   taken out of it) takes its shows with it.
//! - A show whose folder is gone from a library's folder leaves at the next
//!   look, as does a film lying loose there whose file is. Only while the
//!   library's folder is there with something in it: an unplugged drive is
//!   no show gone. And a show whose rips are elsewhere in your libraries, by
//!   name and size, has moved: it's pointed there and keeps everything.
//!   When most of a folder's shows are gone at once, none leaves: that's a
//!   library moved, which Settings → Moved rips is for.
//!
//! The first-time setup reads the folders you choose the same way, but adds
//! nothing by itself: it lists what's in each (`chosen`), asks about each
//! folder and film in turn (`film`, and Add media's scan), and adds what you
//! leave ticked at its end. A folder you choose there may also be one
//! anime's own, which is told from a folder of them by what's in it
//! (`own_folder`). The folders of anime and of films chosen there become
//! your libraries, and what you unticked is passed over (`take_chosen`).
//!
//! Telly's downloader writes a release straight into its place in the
//! library you choose, so nothing is moved once it's down:
//!
//!   in a series library  <folder>\<anime>\Season NN
//!   in a movies library  <folder>\<title> (<year>)
//!
//! - The library at first is the one of its kind (a film's, or a series')
//!   that has the anime's other parts in its folders, else the first of its
//!   kind.
//! - The anime's folder is the one its seasons are in already, in any of the
//!   library's folders, whatever it's called (a release's own name, say); a
//!   new anime's is named as its card in the Library, in the downloads folder.
//! - NN is the season's place on AniList's line of sequels from the first:
//!   the line parts.rs follows to read season folders, so what's written
//!   here is read back the same.
//! - A part off that line (a spin-off, an OVA, a film put in a series
//!   library) goes by its own name, and so does a season whose folder is
//!   there already, holding another show's rips.
//! - A release of several parts (a folder for each season) goes in the
//!   anime's folder as it is.
//!
//! Files keep the names they were released with. Until you've changed them,
//! the libraries are a Series and a Movies library in the folders your series
//! and films are kept in already, saved the first time they're needed. With
//! no library, a download goes in the downloads folder as before
//! (torrents.rs).
//!
//! Settings: libraries (the list) and libraries.adding (the folders to read
//! that were asked for and aren't read yet). library.series and
//! library.movies, the two folders Telly had before, become the first
//! libraries. The videos passed over are in libraries-passed-over.json, each
//! with its length.

use crate::anilist;
use crate::backend::{shared_setting, Backend};
use crate::db::{NewEpisode, NewShow, Show};
use crate::relink::{self, below, folder_of, join, parts};
use crate::series::Graph;
use crate::util::{natural_cmp, safe_file_name};
use crate::{scan, torrents};
use anyhow::{anyhow, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// A library: its name, whether it holds series or movies, and its folders.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Library {
    pub name: String,
    /// "series" or "movies".
    pub kind: String,
    pub folders: Vec<String>,
    /// The one of `folders` that downloads go in.
    pub downloads: String,
}

/// A folder's name from a title: "Kaguya-sama: Love is War" is "Kaguya-sama - Love is War".
fn folder_name(title: &str) -> String {
    safe_file_name(&title.replace(": ", " - "))
}

/// The folder in `series` that the anime called after `root` is in: the one most of its shows in the library are below.
fn anime_folder(series: &str, graph: &Graph, shows: &[Show], root: i64) -> Option<String> {
    let mut tops: Vec<(&str, usize)> = Vec::new();
    for show in shows.iter().filter(|s| s.anilist_id.and_then(|id| graph.root(id)) == Some(root)) {
        let Some(top) = below(series, &show.folder).and_then(|rest| rest.first().copied()) else { continue };
        match tops.iter_mut().find(|(name, _)| name.to_lowercase() == top.to_lowercase()) {
            Some(found) => found.1 += 1,
            None => tops.push((top, 1)),
        }
    }
    // Of two with as many, the first found.
    let most = tops.iter().map(|(_, count)| *count).max()?;
    tops.iter().find(|(_, count)| *count == most).map(|(top, _)| join(series, &[top]))
}

/// The folder a download of the AniList entry `id` goes in, in `library`,
/// or none when Telly hasn't asked AniList about it. `whole`: the release
/// holds several parts. `taken` says whether a folder is there already with
/// something in it.
pub fn place(library: &Library, graph: &Graph, shows: &[Show], id: i64, whole: bool, taken: impl Fn(&str) -> bool) -> Option<String> {
    let entry = graph.entry(id)?;
    let title = entry.anime.short_title();
    if library.kind == "movies" && !whole {
        let named = entry.anime.year.map_or(title.clone(), |year| format!("{title} ({year})"));
        return Some(join(&library.downloads, &[&folder_name(&named)]));
    }
    let root = graph.root(id).unwrap_or(id);
    let anime = library.folders.iter().find_map(|folder| anime_folder(folder, graph, shows, root)).unwrap_or_else(|| {
        let named = graph.entry(root).map_or_else(|| title.clone(), |e| e.anime.short_title());
        join(&library.downloads, &[&folder_name(&named)])
    });
    if whole {
        return Some(anime);
    }
    let season = graph.seasons_from(root).iter().position(|e| e.anime.id == id).map(|at| join(&anime, &[&format!("Season {:02}", at + 1)]));
    Some(season.filter(|folder| !taken(folder)).unwrap_or_else(|| join(&anime, &[&folder_name(&title)])))
}

/// The folder most of your films (`movies`) or series are kept in: the one
/// the folders of the most anime's films, or of the most anime, are in. An
/// anime's folder is the one all its seasons are below; a folder that holds
/// other anime's too is where they're all kept, not one's own.
pub fn kept_in(graph: &Graph, shows: &[Show], movies: bool) -> Option<String> {
    let anime = |s: &Show| s.anilist_id.map(|id| graph.root(id).unwrap_or(id)).unwrap_or(-s.id);
    let kind: Vec<&Show> = shows.iter().filter(|s| if movies { s.format == "MOVIE" } else { !s.format.is_empty() && s.format != "MOVIE" }).collect();
    // A film on its own, a series' seasons together.
    let mut units: Vec<(i64, Vec<&Show>)> = Vec::new();
    for show in kind {
        let key = if movies { show.id } else { anime(show) };
        match units.iter_mut().find(|(k, _)| *k == key) {
            Some((_, unit)) => unit.push(show),
            None => units.push((key, vec![show])),
        }
    }
    // Each anime counts once for a folder, however many of its films are there.
    let mut counted: HashSet<(i64, String)> = HashSet::new();
    let mut tally: Vec<(String, usize)> = Vec::new();
    for (_, unit) in units {
        let mut common = unit[0].folder.trim_end_matches(['\\', '/']).to_string();
        while !common.is_empty() && !unit.iter().all(|s| below(&common, &s.folder).is_some()) {
            common = folder_of(&common).to_string();
        }
        let own = anime(unit[0]);
        let shared = shows.iter().any(|s| anime(s) != own && below(&common, &s.folder).is_some());
        let kept = if shared { common } else { folder_of(&common).to_string() };
        if kept.is_empty() || !counted.insert((own, kept.to_lowercase())) {
            continue;
        }
        match tally.iter_mut().find(|(folder, _)| folder.to_lowercase() == kept.to_lowercase()) {
            Some(found) => found.1 += 1,
            None => tally.push((kept, 1)),
        }
    }
    // Of two with as many, the first found.
    let most = tally.iter().map(|(_, count)| *count).max()?;
    tally.into_iter().find(|(_, count)| *count == most).map(|(folder, _)| folder)
}

/// Your libraries (setting libraries). The first time they're asked for,
/// they're made, and saved: a Series and a Movies library in the folders
/// Telly had for them before, else where your series and films are kept
/// already (`kept_in`). While neither can be told yet (nothing in the
/// library has a place), nothing is saved, so they're made once it can.
pub fn libraries(backend: &Backend) -> Result<Vec<Library>> {
    if let Some(kept) = backend.db.setting("libraries")? {
        return Ok(serde_json::from_value(kept)?);
    }
    let (graph, shows) = (crate::series::graph(backend), backend.db.shows()?);
    let mut made = Vec::new();
    for (key, name, kind) in [("library.series", "Series", "series"), ("library.movies", "Movies", "movies")] {
        let folder = match backend.db.setting(key)? {
            Some(chosen) => chosen.as_str().unwrap_or_default().trim().to_string(),
            None => kept_in(&graph, &shows, kind == "movies").unwrap_or_default(),
        };
        if !folder.is_empty() {
            made.push(Library { name: name.into(), kind: kind.into(), folders: vec![folder.clone()], downloads: folder });
        }
    }
    if made.is_empty() {
        return Ok(made);
    }
    for key in ["library.series", "library.movies"] {
        backend.db.set_setting(key, &serde_json::Value::Null)?;
    }
    backend.db.set_setting("libraries", &json!(made))?;
    Ok(made)
}

/// Your libraries, each with how many shows in the library are in its folders, for Settings.
pub fn view(backend: &Backend) -> Result<serde_json::Value> {
    let (shows, adding) = (backend.db.shows()?, adding(backend));
    let listed: Vec<serde_json::Value> = libraries(backend)?
        .into_iter()
        .map(|l| {
            let count = shows.iter().filter(|s| l.folders.iter().any(|f| below(f, &s.folder).is_some())).count();
            let adding = l.folders.iter().any(|f| adding.iter().any(|a| same(a, f)));
            json!({ "name": l.name, "kind": l.kind, "folders": l.folders, "downloads": l.downloads, "shows": count, "adding": adding })
        })
        .collect();
    Ok(json!(listed))
}

/// Which of `libraries` a show's folder is in, by its place among them. When
/// one library's folder is inside another's, it's the one whose folder is
/// nearest above the show's. None for a show kept outside them all.
pub fn of(libraries: &[Library], folder: &str) -> Option<usize> {
    let depth = |l: &Library| l.folders.iter().filter_map(|f| below(f, folder)).map(|rest| rest.len()).min();
    libraries.iter().enumerate().filter_map(|(i, l)| depth(l).map(|depth| (depth, i))).min().map(|(_, i)| i)
}

/// How long the adding waits before it asks AniList again.
const ASK_AGAIN: Duration = Duration::from_secs(60);

/// Whether two paths are the same folder or file, as Windows has them.
fn same(a: &str, b: &str) -> bool {
    below(a, b).is_some_and(|rest| rest.is_empty())
}

/// What a video is known by however its path is written: D:\Anime\a.mkv is d:/anime/a.mkv.
pub(crate) fn key(path: &str) -> String {
    parts(path).join("/").to_lowercase()
}

/// Reads, or with `change` changes and saves, the videos passed over: each by its `key`, with its length.
fn passed<R>(backend: &Backend, change: bool, with: impl FnOnce(&mut HashMap<String, f64>) -> R) -> R {
    let file = backend.dirs.local.join("libraries-passed-over.json");
    let mut kept = backend.passed_over.lock().unwrap_or_else(|e| e.into_inner());
    let videos = kept.get_or_insert_with(|| std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default());
    let out = with(videos);
    if change {
        let _ = crate::util::write_whole(&file, serde_json::to_vec(videos).unwrap_or_default());
    }
    out
}

/// Passes over `videos` (each with its length) from now on, those of them
/// in a library's folder: a look for new shows takes none of them for new.
/// What you removed from Telly, and what you left out in Add media….
pub fn pass_over(backend: &Backend, videos: &[(String, f64)]) {
    let folders: Vec<String> = libraries(backend).unwrap_or_default().into_iter().flat_map(|l| l.folders).collect();
    let inside: Vec<&(String, f64)> = videos.iter().filter(|(path, _)| folders.iter().any(|f| below(f, path).is_some())).collect();
    if !inside.is_empty() {
        passed(backend, true, |kept| kept.extend(inside.iter().map(|(path, length)| (key(path), *length))));
    }
}

/// The videos passed over that were below `from`: below `to` now, as the rips there are (relink.rs).
pub fn moved(backend: &Backend, from: &str, to: &str) {
    let (from, to) = (format!("{}/", key(from)), format!("{}/", key(to)));
    if passed(backend, false, |kept| kept.keys().any(|k| k.starts_with(&from))) {
        passed(backend, true, |kept| {
            *kept = kept.drain().map(|(k, length)| (k.strip_prefix(&from).map_or(k.clone(), |rest| format!("{to}{rest}")), length)).collect();
        });
    }
}

/// The folders to read that were asked for (a library's adding, Look for new shows) and aren't read yet.
fn adding(backend: &Backend) -> Vec<String> {
    backend.db.setting("libraries.adding").ok().flatten().and_then(|kept| serde_json::from_value(kept).ok()).unwrap_or_default()
}

/// Changes the folders still to be read, from one place at a time.
fn change_adding(backend: &Backend, change: impl FnOnce(&mut Vec<String>)) {
    let _one_at_a_time = backend.adding_libraries.lock().unwrap_or_else(|e| e.into_inner());
    let mut folders = adding(backend);
    change(&mut folders);
    let _ = backend.db.set_setting("libraries.adding", &json!(folders));
}

/// The shows in `before`'s folders that are in none of `now`'s: those that
/// leave Telly when `now` is saved over `before`.
pub fn leaving(backend: &Backend, before: &[Library], now: &[Library]) -> Result<Vec<Show>> {
    let inside = |libraries: &[Library], show: &Show| libraries.iter().flat_map(|l| &l.folders).any(|f| below(f, &show.folder).is_some());
    Ok(backend.db.shows()?.into_iter().filter(|show| inside(before, show) && !inside(now, show)).collect())
}

/// What saving the list of libraries over `before` starts and stops: a
/// folder no library had is read for shows, and one no library has any more
/// isn't read any further, and its shows leave Telly.
pub fn saved(backend: &Arc<Backend>, before: &[Library]) -> Result<()> {
    let now = libraries(backend)?;
    let has = |libraries: &[Library], folder: &str| libraries.iter().flat_map(|l| &l.folders).any(|f| same(f, folder));
    let leaving = leaving(backend, before, &now)?;
    // What was passed over there is forgotten with it: put back in a library, the folder is read afresh.
    let below_any = |libraries: &[Library], video: &str| libraries.iter().flat_map(|l| &l.folders).any(|f| video.starts_with(&format!("{}/", key(f))));
    let left = |video: &str| below_any(before, video) && !below_any(&now, video);
    if passed(backend, false, |kept| kept.keys().any(|video| left(video))) {
        passed(backend, true, |kept| kept.retain(|video, _| !left(video)));
    }
    if !leaving.is_empty() {
        let backend = backend.clone();
        // One after another, in the background: each has Telly's copies and subtitles to delete.
        std::thread::Builder::new()
            .name("telly-library-removed".into())
            .spawn(move || {
                for show in leaving {
                    // Unless its folder has been put back in a library meanwhile.
                    let back = libraries(&backend).is_ok_and(|all| all.iter().flat_map(|l| &l.folders).any(|f| below(f, &show.folder).is_some()));
                    if !back {
                        let _ = backend.remove_show(show.id);
                    }
                }
            })
            .ok();
    }
    change_adding(backend, |folders| {
        folders.retain(|f| !has(before, f) || has(&now, f));
        for folder in now.iter().flat_map(|l| &l.folders) {
            if !has(before, folder) && !folders.iter().any(|f| same(f, folder)) {
                folders.push(folder.clone());
            }
        }
    });
    carry_on(backend);
    Ok(())
}

/// Looks in every library's folders again for shows that aren't in Telly yet (every 10 minutes: backend.rs).
pub fn look(backend: &Arc<Backend>) {
    let folders: Vec<String> = libraries(backend).unwrap_or_default().into_iter().flat_map(|l| l.folders).collect();
    if let Ok(mut looks) = backend.library_looks.lock() {
        for folder in folders {
            if !looks.iter().any(|f| same(f, &folder)) {
                looks.push(folder);
            }
        }
    }
    carry_on(backend);
}

/// Looks in the folders of the library `at` in the list for new shows now (Look for new shows, in Settings).
pub fn look_now(backend: &Arc<Backend>, at: usize) -> Result<()> {
    let library = libraries(backend)?.into_iter().nth(at).ok_or_else(|| anyhow!("That library isn’t in Settings any more"))?;
    change_adding(backend, |folders| {
        for folder in &library.folders {
            if !folders.iter().any(|f| same(f, folder)) {
                folders.push(folder.clone());
            }
        }
    });
    carry_on(backend);
    Ok(())
}

/// Reads the folders to read for shows (those asked for first, then those
/// to look in again), one after another, unless that's going on already.
pub fn carry_on(backend: &Arc<Backend>) {
    fn waiting(backend: &Backend) -> Vec<String> {
        let mut folders = adding(backend);
        folders.extend(backend.library_looks.lock().map(|looks| looks.clone()).unwrap_or_default());
        folders
    }
    {
        let mut going = backend.adding_libraries.lock().unwrap_or_else(|e| e.into_inner());
        if *going || waiting(backend).is_empty() {
            return;
        }
        *going = true;
    }
    let backend = backend.clone();
    std::thread::Builder::new()
        .name("telly-libraries".into())
        .spawn(move || {
            // A folder that isn't there (its drive is unplugged) waits for the next look.
            let mut away: Vec<String> = Vec::new();
            loop {
                let next = {
                    let mut going = backend.adding_libraries.lock().unwrap_or_else(|e| e.into_inner());
                    let next = waiting(&backend).into_iter().find(|f| !away.contains(f));
                    *going = next.is_some();
                    next
                };
                let Some(folder) = next else { break };
                let read = add_folder(&backend, &folder);
                if let Ok(mut looks) = backend.library_looks.lock() {
                    looks.retain(|f| f != &folder);
                }
                if read {
                    change_adding(&backend, |folders| folders.retain(|f| f != &folder));
                } else {
                    away.push(folder);
                }
            }
        })
        .ok();
}

/// Whether a library has `folder`: one no library has any more isn't read any further.
fn wanted(backend: &Backend, folder: &str) -> bool {
    libraries(backend).is_ok_and(|all| all.iter().flat_map(|l| &l.folders).any(|f| same(f, folder)))
}

/// Every episode's rip in Telly, by its `key`: its length and its show.
fn in_telly(backend: &Backend) -> Result<HashMap<String, (f64, i64)>> {
    Ok(backend.db.all_episodes()?.into_iter().map(|e| (key(&e.path), (e.duration, e.show_id))).collect())
}

/// The folder a show whose folder was `from` is in now, its rips found at
/// `moves`: where each rip's place in the old folder puts it, when they
/// agree, else the folder they're all in.
fn folder_now(from: &str, moves: &[relink::Move]) -> String {
    let by_place = |m: &relink::Move| {
        let depth = below(from, &m.from)?.len();
        Some((0..depth).fold(m.to.as_str(), |path, _| folder_of(path)).to_string()).filter(|folder| !folder.is_empty())
    };
    let first = moves.first().and_then(by_place);
    if first.is_some() && moves.iter().all(|m| by_place(m) == first) {
        return first.unwrap_or_default();
    }
    let mut common = moves.first().map(|m| folder_of(&m.to).to_string()).unwrap_or_default();
    while !common.is_empty() && !moves.iter().all(|m| below(&common, &m.to).is_some()) {
        common = folder_of(&common).to_string();
    }
    common
}

/// Sees to the shows in `folder`, a library's, whose rips are gone from it:
/// a show whose own folder isn't there any more, and one in a folder it
/// shares (a film lying loose in the library) none of whose rips are.
///
/// - One whose rips are elsewhere in your libraries, by name and size, has
///   moved (its folder renamed, or put in another library): it's pointed
///   there, and keeps everything (relink.rs).
/// - Any other leaves Telly, as its folder left the library.
/// - Nothing is done unless `folder` is there with something in it: with
///   its drive unplugged, or the share not answering, no show is gone.
/// - Nor does any leave when most of the folder's shows would at once (five
///   or more, and over half): that's a library that moved, or that Telly was
///   restored beside, which Moved rips is for.
fn gone(backend: &Arc<Backend>, folder: &str) -> Result<()> {
    if !std::fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_some()) {
        return Ok(());
    }
    let shows: Vec<Show> = backend.db.shows()?.into_iter().filter(|show| below(folder, &show.folder).is_some()).collect();
    // Every video in your libraries that isn't a show's rip, with its size: read when a show is found gone.
    let mut elsewhere: Option<Vec<(String, i64)>> = None;
    let mut lost: Vec<i64> = Vec::new();
    for show in &shows {
        // A show whose own folder is there is there: its rips are for its own look for new episodes.
        if Path::new(&show.folder).is_dir() && !backend.db.setting_bool(&shared_setting(show.id), false) {
            continue;
        }
        let episodes = backend.db.episodes(show.id)?;
        if episodes.is_empty() || episodes.iter().any(|e| Path::new(&e.path).is_file()) {
            continue;
        }
        if elsewhere.is_none() {
            let rips = in_telly(backend)?;
            let mut videos = Vec::new();
            for library in libraries(backend)?.iter().flat_map(|l| &l.folders) {
                scan::walk(Path::new(library), 0, &mut videos);
            }
            let sized = videos.iter().map(|path| (path.to_string_lossy().into_owned(), std::fs::metadata(path).map_or(0, |m| m.len() as i64)));
            elsewhere = Some(sized.filter(|(path, _)| !rips.contains_key(&key(path))).collect());
        }
        let videos = elsewhere.as_mut().expect("just read");
        let found = relink::find(&show.folder, &show.folder, &episodes, videos);
        if found.is_empty() {
            lost.push(show.id);
            continue;
        }
        // Each is this show's rip now, and no other's.
        videos.retain(|(path, _)| !found.iter().any(|m| m.to == *path));
        let to = folder_now(&show.folder, &found);
        relink::relink(backend, &[relink::ShowMove { show_id: show.id, title: show.title.clone(), from: show.folder.clone(), to, found, missing: Vec::new() }])?;
    }
    if lost.len() >= 5 && lost.len() * 2 > shows.len() {
        return Ok(());
    }
    for show in lost {
        backend.remove_show(show)?;
    }
    Ok(())
}

/// Adds what's in a library's folder that isn't in Telly yet: each folder as
/// an anime and, in a movies library, each loose video as a film. Before
/// that, the shows whose rips are gone from it are seen to (`gone`). False
/// when the folder can't be read.
fn add_folder(backend: &Arc<Backend>, folder: &str) -> bool {
    if !wanted(backend, folder) {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(folder) else { return false };
    let _ = gone(backend, folder);
    let films = libraries(backend).unwrap_or_default().iter().any(|l| l.kind == "movies" && l.folders.iter().any(|f| same(f, folder)));
    let (mut folders, mut videos): (Vec<PathBuf>, Vec<PathBuf>) = (Vec::new(), Vec::new());
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // As when a show's folder is read (scan.rs): nothing hidden, nor the Recycle Bin.
        if name.starts_with('.') || name.starts_with('$') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            folders.push(path);
        } else if films && scan::is_video(&path) {
            videos.push(path);
        }
    }
    let by_name = |a: &PathBuf, b: &PathBuf| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy());
    folders.sort_by(by_name);
    videos.sort_by(by_name);
    let Ok(mut rips) = in_telly(backend) else { return false };
    for path in &folders {
        if !wanted(backend, folder) {
            return true;
        }
        if add_anime(backend, folder, &path.to_string_lossy(), &rips) {
            let Ok(now) = in_telly(backend) else { return false };
            rips = now;
        }
    }
    for path in &videos {
        if !wanted(backend, folder) {
            return true;
        }
        add_film(backend, folder, path, &rips);
    }
    true
}

/// The folders in `folder` and the videos lying loose in it, each in the
/// order Explorer lists them, for the first-time setup. None when the folder
/// can't be read.
fn contents(folder: &str) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
    let entries = std::fs::read_dir(folder).ok()?;
    let (mut folders, mut videos): (Vec<PathBuf>, Vec<PathBuf>) = (Vec::new(), Vec::new());
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // As when a show's folder is read (scan.rs): nothing hidden, nor the Recycle Bin.
        if name.starts_with('.') || name.starts_with('$') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            folders.push(path);
        } else if scan::is_video(&path) {
            videos.push(path);
        }
    }
    let by_name = |a: &PathBuf, b: &PathBuf| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy());
    folders.sort_by(by_name);
    videos.sort_by(by_name);
    Some((folders, videos))
}

/// A video shorter than this isn't taken for a film when telling a folder of films from a show's own.
const FILM_SECONDS: f64 = 40.0 * 60.0;

/// Whether a folder's name is that of a part of an anime, not of an anime: "Season 2", "S01", "OVA", "Specials".
fn named_as_a_part(name: &str) -> bool {
    static PART: OnceLock<Regex> = OnceLock::new();
    let pattern = r"(?i)^(?:(?:season|series|part|cour|disc|disk|volume|vol|cd|dvd|bd|s)[\s._-]*\d{1,3}|\d{1,2}(?:st|nd|rd|th)\s+season|ovas?|oads?|onas?|specials?|movies?|films?|extras?)$";
    // Without its tags: "Season 1 [BD 1080p]".
    let bare = name.split(['[', '(']).next().unwrap_or(name).trim();
    PART.get_or_init(|| Regex::new(pattern).expect("a valid pattern")).is_match(bare)
}

/// Whether a folder you chose is one anime's own, not a folder of them: most
/// of the folders in it are named as an anime's parts, or most of the videos
/// lying loose in it are too short to be films, so they're its episodes.
fn own_folder(folders: &[PathBuf], lengths: &[Option<f64>]) -> bool {
    let parts = folders.iter().filter(|f| f.file_name().is_some_and(|n| named_as_a_part(&n.to_string_lossy()))).count();
    let read: Vec<f64> = lengths.iter().flatten().copied().collect();
    parts * 2 > folders.len() || read.iter().filter(|d| **d < FILM_SECONDS).count() * 2 > read.len()
}

/// What the first-time setup reads in a folder you chose, before anything is
/// added. `show` says it's one anime's own folder, read whole; otherwise
/// `folders` are the folders in it, an anime each, and `videos` the videos
/// lying loose in it, a film each. What's in Telly already is left out, and
/// counted in `have`.
pub fn chosen(backend: &Backend, folder: &str) -> Result<serde_json::Value> {
    let (folders, videos) = contents(folder).ok_or_else(|| anyhow!("Telly can’t read {folder}. Is it a folder?"))?;
    let rips = in_telly(backend)?;
    let has = |path: &Path| {
        let below_it = format!("{}/", key(&path.to_string_lossy()));
        rips.keys().any(|rip| rip.starts_with(&below_it))
    };
    let text = |paths: Vec<&PathBuf>| paths.into_iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>();
    if own_folder(&folders, &backend.lengths(&videos)) {
        let have = has(Path::new(folder));
        return Ok(json!({ "show": true, "folders": if have { vec![] } else { vec![folder] }, "videos": [], "have": have as usize }));
    }
    let (had, new): (Vec<&PathBuf>, Vec<&PathBuf>) = folders.iter().partition(|f| has(f));
    let (had_films, films): (Vec<&PathBuf>, Vec<&PathBuf>) = videos.iter().partition(|v| rips.contains_key(&key(&v.to_string_lossy())));
    Ok(json!({ "show": false, "folders": text(new), "videos": text(films), "have": had.len() + had_films.len() }))
}

/// Makes the folders of anime and of films chosen in the first-time setup
/// (`chosen`: each with whether it holds films) your libraries, beside those
/// you have, and passes over `out`, the videos of what you unticked there.
/// They aren't read for shows now: the setup has just read them, and adds
/// what you left ticked itself. The libraries' look comes to them in its turn.
pub fn take_chosen(backend: &Backend, chosen: &[(String, bool)], out: &[(String, f64)]) -> Result<()> {
    let mut all = libraries(backend)?;
    for (folder, films) in chosen {
        if all.iter().flat_map(|l| &l.folders).any(|f| below(f, folder).is_some()) {
            continue;
        }
        let kind = if *films { "movies" } else { "series" };
        match all.iter_mut().find(|l| l.kind == kind) {
            Some(library) => library.folders.push(folder.clone()),
            None => all.push(Library { name: if *films { "Movies" } else { "Series" }.into(), kind: kind.into(), folders: vec![folder.clone()], downloads: folder.clone() }),
        }
    }
    backend.db.set_setting("libraries", &json!(all))?;
    pass_over(backend, out);
    Ok(())
}

/// A video lying loose in a folder you chose, as the film the first-time
/// setup would add: what it's called, the anime AniList has it as, and the
/// show it's added as, as a library's look adds one (`add_film`). Null for a
/// video that's no film: one Telly can't read, or shorter than an episode.
pub fn film(backend: &Backend, folder: &str, path: &str) -> Result<serde_json::Value> {
    let file = Path::new(path);
    let Some(duration) = backend.lengths(&[file.to_path_buf()]).pop().flatten().filter(|length| *length >= scan::MIN_SECONDS) else { return Ok(serde_json::Value::Null) };
    let name = scan::clean_title(&file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    let mut found = anilist::search_loosely(&name)?;
    anilist::rank(&mut found, &name, 1);
    // Nobody has checked it yet, so it's the film AniList finds only when their names are much alike.
    let entry = found.into_iter().next().filter(|a| a.is_called(&name));
    let show = NewShow {
        folder: folder.to_string(),
        title: if entry.is_some() { String::new() } else { name.clone() },
        anilist_id: entry.as_ref().map(|a| a.id),
        episodes: vec![NewEpisode { number: 1, path: path.to_string(), size: std::fs::metadata(file).map_or(0, |m| m.len() as i64), duration }],
        ..Default::default()
    };
    // Its folder holds the other films too.
    Ok(json!({ "title": name, "anime": entry, "asRead": [crate::backend::as_read(&show, true)] }))
}

/// Does `add` until AniList can be asked, for as long as a library has
/// `library` (its folder). What `add` gave; or none when the library went,
/// or `add` failed though AniList can be asked (see `tried`).
fn patiently<T>(backend: &Backend, library: &str, add: impl Fn() -> Result<T>) -> Option<T> {
    tried(add, anilist::answers, || {
        std::thread::sleep(ASK_AGAIN);
        wanted(backend, library)
    })
}

/// What `add` gives, tried again after each `wait` (which says whether to go
/// on) for as long as AniList `answers` nothing. An `add` that fails while
/// AniList answers has trouble of its own (an entry AniList no longer has,
/// a library that can't be written): it's given up on, with none, so what's
/// after it isn't kept waiting. The next look tries it again.
fn tried<T>(add: impl Fn() -> Result<T>, answers: impl Fn() -> bool, wait: impl Fn() -> bool) -> Option<T> {
    loop {
        match add() {
            Ok(done) => return Some(done),
            Err(e) if answers() => {
                eprintln!("library: passed over for now: {e:#}");
                return None;
            }
            Err(_) => {}
        }
        if !wait() {
            return None;
        }
    }
}

/// Reads `folder` as Add media… does, asking ffprobe only for the videos
/// whose length isn't `known` (by `key`): the episodes in Telly and the
/// videos passed over were read before.
fn read(backend: &Backend, folder: &str, known: &HashMap<String, f64>) -> Result<scan::Scan> {
    scan::scan(Path::new(folder), |paths| {
        let length = |path: &PathBuf| known.get(&key(&path.to_string_lossy())).copied();
        let unknown: Vec<PathBuf> = paths.iter().filter(|p| length(p).is_none()).cloned().collect();
        let mut probed = backend.lengths(&unknown).into_iter();
        paths
            .iter()
            .map(|p| match length(p) {
                // An episode in Telly counts as readable whatever its length, as when its show's folder is looked in.
                Some(known) => Some(if known > 0.0 { known } else { 600.0 }),
                None => probed.next().flatten(),
            })
            .collect()
    })
}

/// Adds what's new in `folder`, one of the folders in `library`: the anime
/// in it or, when some of it is in Telly, the seasons, movies and OVAs of it
/// that aren't. `rips` are the episodes in Telly (`in_telly`). True when it
/// added a show.
fn add_anime(backend: &Arc<Backend>, library: &str, folder: &str, rips: &HashMap<String, (f64, i64)>) -> bool {
    let mut videos = Vec::new();
    scan::walk(Path::new(folder), 0, &mut videos);
    let videos: Vec<String> = videos.iter().map(|path| key(&path.to_string_lossy())).collect();
    let mut known: HashMap<String, f64> = passed(backend, false, |kept| videos.iter().filter_map(|v| kept.get(v).map(|length| (v.clone(), *length))).collect());
    let new: HashSet<&String> = videos.iter().filter(|v| !rips.contains_key(*v) && !known.contains_key(*v)).collect();
    // Nothing there Telly hasn't looked at. And a folder Telly is downloading into joins the library by itself
    // (torrents.rs), and one open in Add media… is yours to match.
    if new.is_empty() || torrents::going_in(backend, folder) || backend.sheet_open(folder) {
        return false;
    }
    known.extend(videos.iter().filter_map(|v| rips.get(v).map(|(length, _)| (v.clone(), *length))));
    let Ok(scan) = read(backend, folder, &known) else { return false };
    let is_new = |path: &str| new.contains(&key(path));
    // An episode's length, or a film's: an extra, or a second copy, says nothing of a show to add.
    let an_episode = |f: &scan::ScannedFile| f.reason.is_empty() || f.reason.starts_with("Over 2 hours");
    let mut added: HashSet<String> = HashSet::new();
    if scan.files.iter().any(|f| is_new(&f.path) && an_episode(f)) {
        // The anime its seasons in Telly are (the one you chose, when you did), else what its name finds.
        let here: HashSet<i64> = videos.iter().filter_map(|v| rips.get(v).map(|(_, show)| *show)).collect();
        let mut shows: Vec<Show> = backend.db.shows().unwrap_or_default().into_iter().filter(|s| here.contains(&s.id) && s.anilist_id.is_some()).collect();
        shows.sort_by(|a, b| natural_cmp(&a.folder, &b.folder));
        let base = shows.first().and_then(|s| s.anilist_id);
        let Some(ids) = patiently(backend, library, || backend.add_as_read(&scan, base, &is_new)) else { return false };
        added = ids.iter().flat_map(|id| backend.db.episodes(*id).unwrap_or_default()).map(|e| key(&e.path)).collect();
    }
    // What's new and wasn't added is looked at now, and not again. A video Telly can't read yet (still being
    // copied there) is; one that's gone is forgotten.
    let looked: Vec<(String, f64)> =
        scan.files.iter().filter(|f| is_new(&f.path) && f.duration > 0.0 && !added.contains(&key(&f.path))).map(|f| (key(&f.path), f.duration)).collect();
    let below_here = format!("{}/", key(folder));
    passed(backend, true, |kept| {
        kept.retain(|video, _| !video.starts_with(&below_here) || videos.contains(video));
        kept.extend(looked);
    });
    !added.is_empty()
}

/// Adds a video lying loose in `library`, a movies library's folder, as a
/// film, unless it's a show's episode already (`rips`) or was passed over.
/// One Telly can't read (still being copied) is looked at again; one shorter
/// than an episode isn't a film.
fn add_film(backend: &Arc<Backend>, library: &str, path: &Path, rips: &HashMap<String, (f64, i64)>) {
    let file = path.to_string_lossy().into_owned();
    let video = key(&file);
    if rips.contains_key(&video) || passed(backend, false, |kept| kept.contains_key(&video)) || backend.sheet_open(&file) {
        return;
    }
    let Some(duration) = backend.lengths(&[path.to_path_buf()]).pop().flatten().filter(|length| *length > 0.0) else { return };
    if duration < scan::MIN_SECONDS {
        passed(backend, true, |kept| kept.insert(video, duration));
        return;
    }
    let name = scan::clean_title(&path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    let size = std::fs::metadata(path).map_or(0, |m| m.len() as i64);
    patiently(backend, library, || {
        let mut found = anilist::search_loosely(&name)?;
        anilist::rank(&mut found, &name, 1);
        // Nobody checks it, so it's the film AniList finds only when their names are much alike.
        let entry = found.first().filter(|a| a.is_called(&name));
        let show = NewShow {
            folder: library.to_string(),
            title: if entry.is_some() { String::new() } else { name.clone() },
            anilist_id: entry.map(|a| a.id),
            episodes: vec![NewEpisode { number: 1, path: file.clone(), size, duration }],
            ..Default::default()
        };
        // Its folder holds the library's other films too.
        backend.add_parts(vec![(show, true)], None, None, true)
    });
}

/// The library a download of the AniList entry `id` goes in unless you
/// choose another: of the libraries of its kind (a film's, or a series'),
/// the one with the anime's other parts in its folders, else the first; of
/// all of them when none is of its kind.
pub fn usual(libraries: &[Library], graph: &Graph, shows: &[Show], id: i64) -> Option<usize> {
    let kind = if graph.entry(id).is_some_and(|e| e.anime.format == "MOVIE") { "movies" } else { "series" };
    let root = graph.root(id).unwrap_or(id);
    let holds = |library: &Library| {
        shows.iter().any(|s| s.anilist_id.map(|a| graph.root(a).unwrap_or(a)) == Some(root) && library.folders.iter().any(|f| below(f, &s.folder).is_some()))
    };
    let of_kind: Vec<usize> = (0..libraries.len()).filter(|&i| libraries[i].kind == kind).collect();
    let choices = if of_kind.is_empty() { (0..libraries.len()).collect() } else { of_kind };
    choices.iter().copied().find(|&i| holds(&libraries[i])).or(choices.first().copied())
}

/// The folder a show's rips are in: the deepest one below its own folder
/// that holds them all ("Season 3" in "One-Punch Man"), else its own.
pub fn rips_folder(folder: &str, rips: &[String]) -> String {
    let mut common: Option<Vec<&str>> = None;
    for rip in rips {
        let Some(rest) = below(folder, folder_of(rip)) else { return folder.to_string() };
        common = Some(match common {
            Some(shared) => shared.into_iter().zip(rest).take_while(|(a, b)| a.to_lowercase() == b.to_lowercase()).map(|(a, _)| a).collect(),
            None => rest,
        });
    }
    join(folder, &common.unwrap_or_default())
}

/// Makes room for a download in `folder`, where it's inside a show's folder
/// (the anime's own folder, the show's rips a season folder down): that show
/// is pointed at the folder its rips are in. Where its rips aren't apart
/// from `folder`, it's no longer looked in for new episodes, as any show
/// whose folder holds other parts' rips too.
pub fn make_room(backend: &Backend, folder: &str) -> Result<()> {
    for show in backend.db.shows()? {
        if below(&show.folder, folder).is_none_or(|rest| rest.is_empty()) {
            continue;
        }
        let rips: Vec<String> = backend.db.episodes(show.id)?.into_iter().map(|e| e.path).collect();
        let narrowed = rips_folder(&show.folder, &rips);
        if below(&narrowed, folder).is_none() {
            backend.db.relink(&[(show.id, narrowed)], &[])?;
        } else {
            backend.db.set_setting(&shared_setting(show.id), &json!(true))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::series::tests::{entry, overlord};

    fn show(id: i64, anilist: i64, folder: &str) -> Show {
        crate::series::tests::show(id, "", Some(anilist), folder)
    }

    #[test]
    fn a_chosen_folder_is_one_anime_s_own_or_a_folder_of_them() {
        for name in ["Season 2", "season 01 [BD 1080p]", "S1", "S03", "2nd Season", "Specials", "OVA", "Movies", "Disc 2", "Part 1"] {
            assert!(named_as_a_part(name), "{name}");
        }
        for name in ["Overlord", "Overlord II", "Mob Psycho 100", "Sousou no Frieren 2nd Season", "Movie 3 - The Last", "Steins;Gate 0"] {
            assert!(!named_as_a_part(name), "{name}");
        }
        let folders = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        // Season folders, with a film beside them: one anime.
        assert!(own_folder(&folders(&["Season 1", "Season 2"]), &[Some(6300.0)]));
        // Episodes lying in it: one show, extras folder or no.
        assert!(own_folder(&folders(&["Extras"]), &[Some(1420.0), Some(1420.0), Some(1380.0)]));
        // A folder for each anime, one of them a folder of movies.
        assert!(!own_folder(&folders(&["Frieren", "Overlord", "Movies"]), &[]));
        // Films, one of them short, and a folder for one of them.
        assert!(!own_folder(&folders(&["Your Name (2016)"]), &[Some(6400.0), Some(7500.0), Some(1500.0)]));
        // Nothing Telly can read yet says nothing.
        assert!(!own_folder(&[], &[None, None]));
    }

    #[test]
    fn a_chosen_folder_lists_what_is_new_in_it() {
        let dir = std::env::temp_dir().join(format!("telly-chosen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(crate::backend::Dirs { local: dir.join("data"), roaming: dir.join("data") }).unwrap();
        let anime = dir.join("Anime");
        for folder in ["Frieren", "Overlord/Season 1", ".hidden"] {
            std::fs::create_dir_all(anime.join(folder)).unwrap();
        }
        let path = |name: &str| anime.join(name).to_string_lossy().into_owned();
        let episodes = vec![NewEpisode { number: 1, path: path("Frieren/01.mkv"), size: 1, duration: 1400.0 }];
        backend.db.add_show(&NewShow { folder: path("Frieren"), title: "Frieren".into(), episodes, ..Default::default() }).unwrap();
        // A folder of anime: the one in Telly already is counted, not listed.
        assert_eq!(chosen(&backend, &path("")).unwrap(), json!({ "show": false, "folders": [path("Overlord")], "videos": [], "have": 1 }));
        // One anime's own folder is read whole.
        assert_eq!(chosen(&backend, &path("Overlord")).unwrap(), json!({ "show": true, "folders": [path("Overlord")], "videos": [], "have": 0 }));
        assert!(chosen(&backend, &path("Nowhere")).is_err());
        // Chosen, they're libraries, beside what's there, and what was unticked in them is passed over.
        let films = dir.join("Films").to_string_lossy().into_owned();
        take_chosen(&backend, &[(path(""), false), (films.clone(), true), (path("Overlord"), false)], &[(path("Overlord/Season 1/01.mkv"), 1400.0), ("/elsewhere/01.mkv".into(), 1400.0)]).unwrap();
        let now = libraries(&backend).unwrap();
        assert_eq!(now.iter().map(|l| (l.kind.as_str(), l.folders.clone())).collect::<Vec<_>>(), vec![("series", vec![path("")]), ("movies", vec![films])]);
        assert!(passed(&backend, false, |kept| kept.len() == 1 && kept.contains_key(&key(&path("Overlord/Season 1/01.mkv")))));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A library of `kind` whose first folder takes downloads.
    fn library(kind: &str, folders: &[&str]) -> Library {
        Library { name: kind.into(), kind: kind.into(), folders: folders.iter().map(|f| f.to_string()).collect(), downloads: folders[0].into() }
    }

    fn series() -> Library {
        library("series", &[r"D:\Media\Anime\Series"])
    }

    fn movies() -> Library {
        library("movies", &[r"D:\Media\Anime\Movies"])
    }

    #[test]
    fn a_new_anime_gets_a_folder_and_a_season_folder() {
        let graph = Graph::new(overlord());
        let free = |_: &str| false;
        assert_eq!(place(&series(), &graph, &[], 20832, false, free).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Season 01"));
        // The season's number is its place on the line of sequels, past the films between.
        assert_eq!(place(&series(), &graph, &[], 133844, false, free).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Season 04"));
    }

    #[test]
    fn the_next_season_goes_where_the_anime_s_seasons_are() {
        let graph = Graph::new(overlord());
        let pack = r"D:\Media\Anime\Series\[Judas] Overlord (Seasons 1-3) [BD 1080p]";
        let shows = [show(1, 20832, &format!(r"{pack}\[Judas] Overlord - S1")), show(2, 98437, &format!(r"{pack}\[Judas] Overlord - S2"))];
        assert_eq!(place(&series(), &graph, &shows, 133844, false, |_| false), Some(format!(r"{pack}\Season 04")));
        // A show elsewhere says nothing of where the anime is here.
        let elsewhere = [show(1, 20832, r"C:\Videos\Overlord")];
        assert_eq!(place(&series(), &graph, &elsewhere, 98437, false, |_| false).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Season 02"));
        // In a library's other folder, it's found there.
        let two = library("series", &[r"D:\New", r"E:\Old"]);
        let old = [show(1, 20832, r"E:\Old\Overlord\Season 01")];
        assert_eq!(place(&two, &graph, &old, 98437, false, |_| false).as_deref(), Some(r"E:\Old\Overlord\Season 02"));
    }

    #[test]
    fn a_film_goes_in_its_own_folder_and_a_side_story_by_its_own_name() {
        let graph = Graph::new(overlord());
        let shows = [show(1, 20832, r"D:\Media\Anime\Series\Overlord\Season 01")];
        assert_eq!(place(&movies(), &graph, &shows, 133845, false, |_| false).as_deref(), Some(r"D:\Media\Anime\Movies\Overlord - The Sacred Kingdom (2024)"));
        assert_eq!(place(&series(), &graph, &shows, 21305, false, |_| false).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Overlord - Ple Ple Pleiades"));
        // A film put in a series library goes in its anime's folder by its own name.
        assert_eq!(place(&series(), &graph, &shows, 133845, false, |_| false).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Overlord - The Sacred Kingdom"));
        // Nor is an entry Telly hasn't asked AniList about placed.
        assert_eq!(place(&series(), &Graph::new([entry(1, "Other", "TV", 12, 20200101, &[])]), &[], 2, false, |_| false), None);
    }

    #[test]
    fn a_season_folder_that_s_another_show_s_is_left_alone() {
        let graph = Graph::new(overlord());
        let taken = |folder: &str| folder.ends_with("Season 02");
        assert_eq!(place(&series(), &graph, &[], 98437, false, taken).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Overlord II"));
    }

    #[test]
    fn a_release_of_several_parts_goes_in_the_anime_s_folder() {
        let graph = Graph::new(overlord());
        assert_eq!(place(&series(), &graph, &[], 20832, true, |_| false).as_deref(), Some(r"D:\Media\Anime\Series\Overlord"));
    }

    #[test]
    fn a_download_goes_in_the_library_of_its_kind_that_has_the_anime() {
        let graph = Graph::new(overlord());
        let elsewhere = library("series", &[r"E:\Anime"]);
        let libraries = [series(), movies(), elsewhere];
        let shows = [show(1, 20832, r"E:\Anime\Overlord\Season 01")];
        // A film goes with the movies, though its anime's seasons are in a series library.
        assert_eq!(usual(&libraries, &graph, &shows, 133845), Some(1));
        // A season goes in the series library its anime is in, else the first.
        assert_eq!(usual(&libraries, &graph, &shows, 98437), Some(2));
        assert_eq!(usual(&libraries, &graph, &[], 98437), Some(0));
        // With none of its kind, any.
        assert_eq!(usual(&[movies()], &graph, &[], 98437), Some(0));
        assert_eq!(usual(&[], &graph, &[], 98437), None);
    }

    #[test]
    fn the_folders_not_chosen_are_where_the_films_and_series_are_kept() {
        let mut graph = overlord();
        graph.extend([
            entry(1, "Your Name.", "MOVIE", 1, 20160826, &[]),
            entry(2, "Spirited Away", "MOVIE", 1, 20010720, &[]),
            entry(3, "K-On!", "TV", 13, 20090403, &[("SEQUEL", 4)]),
            entry(4, "K-On!!", "TV", 26, 20100407, &[("PREQUEL", 3)]),
            entry(5, "Baki Hanma Season 2", "ONA", 27, 20230726, &[]),
            entry(6, "Laid-Back Camp", "TV", 12, 20180104, &[]),
            entry(7, "Paprika", "MOVIE", 1, 20061125, &[]),
            entry(8, "ONE PIECE: The Movie", "MOVIE", 1, 20000304, &[("SEQUEL", 9)]),
            entry(9, "ONE PIECE: Nejimaki-jima no Bouken", "MOVIE", 1, 20010303, &[("PREQUEL", 8)]),
        ]);
        let graph = Graph::new(graph);
        let kind = |mut s: Show, format: &str| {
            s.format = format.into();
            s
        };
        let pack = r"D:\Media\Anime\Series\[Judas] Overlord (Seasons 1-3 + Movies)";
        let films = r"D:\Media\Anime\Movies";
        let shows = [
            kind(show(1, 1, &format!(r"{films}\Your Name (2016)\Player Ready")), "MOVIE"),
            kind(show(2, 2, &format!(r"{films}\Spirited Away (2001)")), "MOVIE"),
            kind(show(3, 7, &format!(r"{films}\Paprika")), "MOVIE"),
            kind(show(4, 8, &format!(r"{films}\One Piece Movie 01 (2000)")), "MOVIE"),
            kind(show(5, 9, &format!(r"{films}\One Piece Movie 02 (2001)")), "MOVIE"),
            // An anime's films in its own folder, beside its seasons.
            kind(show(6, 98873, &format!(r"{pack}\[Judas] Overlord - Movies")), "MOVIE"),
            kind(show(7, 98874, &format!(r"{pack}\[Judas] Overlord - Movies")), "MOVIE"),
            kind(show(8, 20832, &format!(r"{pack}\[Judas] Overlord - S1")), "TV"),
            kind(show(9, 98437, &format!(r"{pack}\[Judas] Overlord - S2")), "TV"),
            kind(show(10, 3, r"D:\Media\Anime\Series\K-On!\Season 01"), "TV"),
            kind(show(11, 4, r"D:\Media\Anime\Series\K-On!\Season 02"), "TV"),
            kind(show(12, 5, r"D:\Media\Anime\Series\Baki Hanma\Season 02"), "ONA"),
            kind(show(13, 6, r"D:\Media\Anime\Series\Laid-Back Camp"), "TV"),
            // Not on AniList: neither a film nor a series.
            crate::series::tests::show(14, "", None, r"D:\Media\Movies\Shin Godzilla"),
            crate::series::tests::show(15, "", None, r"D:\Media\Movies\Godzilla Minus One"),
            crate::series::tests::show(16, "", None, r"D:\Media\Movies\Call Me Chihiro (2023)"),
            crate::series::tests::show(17, "", None, r"D:\Media\Movies\Perfect Days (2023)"),
        ];
        assert_eq!(kept_in(&graph, &shows, true).as_deref(), Some(r"D:\Media\Anime\Movies"));
        assert_eq!(kept_in(&graph, &shows, false).as_deref(), Some(r"D:\Media\Anime\Series"));
        // An anime's seasons side by side among the other anime count for the folder they're in.
        let loose = [
            kind(show(1, 20832, r"D:\Anime\Overlord S1"), "TV"),
            kind(show(2, 98437, r"D:\Anime\Overlord S2"), "TV"),
            kind(show(3, 3, r"D:\Anime\K-On!\Season 01"), "TV"),
            kind(show(4, 4, r"D:\Anime\K-On!\Season 02"), "TV"),
        ];
        assert_eq!(kept_in(&graph, &loose, false).as_deref(), Some(r"D:\Anime"));
        assert_eq!(kept_in(&graph, &loose, true), None);
    }

    #[test]
    fn a_show_is_in_the_library_whose_folder_is_nearest_above_it() {
        let dramas = library("series", &[r"D:\Media\Anime\Series\Dramas", r"E:\Dramas"]);
        let libraries = [series(), movies(), dramas];
        assert_eq!(of(&libraries, r"D:\Media\Anime\Series\Overlord\Season 01"), Some(0));
        assert_eq!(of(&libraries, r"d:\media\anime\movies\Paprika (2006)"), Some(1));
        // A library inside another's folder has what's in its own.
        assert_eq!(of(&libraries, r"D:\Media\Anime\Series\Dramas\Quartet"), Some(2));
        assert_eq!(of(&libraries, r"E:\Dramas\Quartet"), Some(2));
        // Not D:\Media\Anime\Series2, and not a folder above a library's.
        assert_eq!(of(&libraries, r"D:\Media\Anime\Series2\Overlord"), None);
        assert_eq!(of(&libraries, r"D:\Media\Anime"), None);
    }

    #[test]
    fn the_first_libraries_are_the_folders_telly_had_else_where_shows_are() {
        use crate::backend::Dirs;
        use crate::db::NewShow;
        let dir = std::env::temp_dir().join(format!("telly-libraries-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        // Asked before anything in the library has a place (Settings opened first): none yet, and none kept.
        assert_eq!(libraries(&backend).unwrap(), vec![]);
        assert_eq!(backend.db.setting("libraries").unwrap(), None);
        let film = NewShow { folder: r"D:\Films\Paprika".into(), title: "Paprika".into(), anilist_id: Some(1), format: "MOVIE".into(), ..Default::default() };
        backend.db.add_show(&film).unwrap();
        // The series folder was set to the downloads folder: there's no series library.
        backend.db.set_setting("library.series", &json!("")).unwrap();
        assert_eq!(libraries(&backend).unwrap(), vec![Library { name: "Movies".into(), kind: "movies".into(), folders: vec![r"D:\Films".into()], downloads: r"D:\Films".into() }]);
        assert_eq!(backend.db.setting("library.series").unwrap(), None);
        // Once saved, they're yours: none is made again.
        backend.db.set_setting("libraries", &json!([])).unwrap();
        assert_eq!(libraries(&backend).unwrap(), vec![]);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn room_is_made_beside_a_show_whose_folder_is_the_anime_s_own() {
        use crate::backend::Dirs;
        use crate::db::{NewEpisode, NewShow};
        let dir = std::env::temp_dir().join(format!("telly-room-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let add = |folder: &str, rips: &[&str]| {
            let episodes = rips.iter().enumerate().map(|(i, rip)| NewEpisode { number: i as i64 + 1, path: format!(r"{folder}\{rip}"), size: 1, duration: 1400.0 }).collect();
            backend.db.add_show(&NewShow { folder: folder.into(), title: "Show".into(), episodes, ..Default::default() }).unwrap()
        };
        let folder_of = |id: i64| backend.db.show(id).unwrap().unwrap().folder;
        let shared = |id: i64| backend.db.setting_bool(&shared_setting(id), false);
        // Its rips are a season folder down: it's pointed there.
        let apart = add(r"D:\Anime\One-Punch Man", &[r"Season 3\02.mkv", r"Season 3\03.mkv"]);
        // Its rips are in the anime's folder itself: it's no longer looked in for new episodes.
        let within = add(r"D:\Anime\Mushishi", &["01.mkv", "02.mkv"]);
        let other = add(r"D:\Anime\Monster", &[r"Season 1\01.mkv"]);
        make_room(&backend, r"D:\Anime\One-Punch Man\Season 01").unwrap();
        make_room(&backend, r"D:\Anime\Mushishi\Season 02").unwrap();
        assert_eq!((folder_of(apart).as_str(), shared(apart)), (r"D:\Anime\One-Punch Man\Season 3", false));
        assert_eq!((folder_of(within).as_str(), shared(within)), (r"D:\Anime\Mushishi", true));
        assert_eq!((folder_of(other).as_str(), shared(other)), (r"D:\Anime\Monster", false));
        // A show's own folder needs no room made in it.
        make_room(&backend, r"D:\Anime\Monster").unwrap();
        assert_eq!((folder_of(other).as_str(), shared(other)), (r"D:\Anime\Monster", false));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_new_to_the_libraries_is_read_for_shows_and_a_removed_one_isn_t() {
        let dir = std::env::temp_dir().join(format!("telly-library-adding-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Arc::new(Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        let save = |libraries: &[Library]| backend.call("setSetting", json!({ "key": "libraries", "value": libraries })).unwrap();
        // Folders that aren't there (an unplugged drive) stay to be read when Telly next starts.
        let name = |folder: &str| dir.join(folder).to_string_lossy().into_owned();
        let (old, new, other) = (name("old"), name("new"), name("other"));
        save(&[library("series", &[old.as_str()])]);
        assert_eq!(adding(&backend), [old.clone()]);
        // A folder added to a library is read too; one it had already isn't read again.
        backend.db.set_setting("libraries.adding", &json!([])).unwrap();
        save(&[library("series", &[old.as_str(), new.as_str()]), library("movies", &[other.as_str()])]);
        assert_eq!(adding(&backend), [new.clone(), other.clone()]);
        assert_eq!(view(&backend).unwrap()[0]["adding"], true);
        // Removing a library stops its folder being read; the others go on.
        save(&[library("series", &[old.as_str(), new.as_str()])]);
        assert_eq!(adding(&backend), [new.clone()]);
        // A folder with nothing in it is read, and done with (once the look that found it missing is over).
        let wait_until = |done: &dyn Fn() -> bool| (0..400).any(|_| done() || { std::thread::sleep(Duration::from_millis(25)); false });
        assert!(wait_until(&|| !*backend.adding_libraries.lock().unwrap()));
        std::fs::create_dir_all(&new).unwrap();
        save(&[library("series", &[new.as_str()]), library("movies", &[other.as_str()])]);
        assert!(wait_until(&|| adding(&backend) == [other.clone()]), "{:?}", adding(&backend));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_show_you_remove_is_passed_over_where_its_rips_are_in_a_library() {
        let dir = std::env::temp_dir().join(format!("telly-library-passed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let open = || Arc::new(Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        let backend = open();
        let name = |folder: &str| dir.join(folder).to_string_lossy().into_owned();
        let (series, elsewhere) = (name("Series"), name("Elsewhere"));
        backend.db.set_setting("libraries", &json!([library("series", &[series.as_str()])])).unwrap();
        let add = |folder: &str| {
            let rip = join(folder, &["Show", "Show - 01.mkv"]);
            let episodes = vec![NewEpisode { number: 1, path: rip.clone(), size: 1, duration: 1400.0 }];
            (backend.db.add_show(&NewShow { folder: join(folder, &["Show"]), title: "Show".into(), episodes, ..Default::default() }).unwrap(), rip)
        };
        let ((inside, in_library), (outside, not_in_one)) = (add(&series), add(&elsewhere));
        backend.call("removeShow", json!({ "id": inside })).unwrap();
        backend.call("removeShow", json!({ "id": outside })).unwrap();
        let length = |backend: &Backend, path: &str| passed(backend, false, |kept| kept.get(&key(path)).copied());
        assert_eq!((length(&backend, &in_library), length(&backend, &not_in_one)), (Some(1400.0), None));
        // However the path is written.
        assert_eq!(length(&backend, &in_library.to_uppercase().replace('/', "\\")), Some(1400.0));
        // Kept for when Telly next starts, and it moves with your rips.
        drop(backend);
        let backend = open();
        assert_eq!(length(&backend, &in_library), Some(1400.0));
        moved(&backend, &series, &elsewhere);
        assert_eq!((length(&backend, &in_library), length(&backend, &not_in_one)), (None, Some(1400.0)));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_look_passes_over_what_isn_t_an_episode_and_doesn_t_read_it_again() {
        let dir = std::env::temp_dir().join(format!("telly-library-look-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Arc::new(Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        let Some(ffmpeg) = backend.tools().ffmpeg else { return }; // making a video to read needs ffmpeg
        let series = dir.join("Series");
        let extra = series.join("Show").join("Extras").join("Show - NCOP.mkv");
        std::fs::create_dir_all(extra.parent().unwrap()).unwrap();
        let made = std::process::Command::new(&ffmpeg).args(["-v", "quiet", "-f", "lavfi", "-i", "color=c=black:s=64x36:r=1", "-t", "90", "-c:v", "mpeg4", "-y"]).arg(&extra).status().unwrap();
        assert!(made.success());
        let extra = extra.to_string_lossy().into_owned();
        backend.db.set_setting("libraries", &json!([library("series", &[&series.to_string_lossy()])])).unwrap();
        let wait_until = |done: &dyn Fn() -> bool| (0..400).any(|_| done() || { std::thread::sleep(Duration::from_millis(25)); false });
        let looked = |backend: &Backend| backend.library_looks.lock().unwrap().is_empty() && !*backend.adding_libraries.lock().unwrap();
        // The 10-minute look: an extra is no show, so nothing is added (and AniList isn't asked).
        look(&backend);
        assert!(wait_until(&|| looked(&backend) && passed(&backend, false, |kept| kept.contains_key(&key(&extra)))));
        assert!(backend.db.shows().unwrap().is_empty());
        // Its length is kept, so the folder is read again without ffprobe; gone, it's forgotten.
        assert_eq!(passed(&backend, false, |kept| kept.get(&key(&extra)).map(|length| length.round())), Some(90.0));
        std::fs::remove_file(&extra).unwrap();
        std::fs::write(series.join("Show").join("Extras").join("Show - Menu.mkv"), b"not a video").unwrap();
        look(&backend);
        assert!(wait_until(&|| looked(&backend) && passed(&backend, false, |kept| kept.is_empty())));
        // Look for new shows says the library is being read until it has been.
        look_now(&backend, 0).unwrap();
        assert!(wait_until(&|| adding(&backend).is_empty()));
        assert!(look_now(&backend, 1).is_err());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A backend in a folder of its own, a library folder in it, and what adds a show there: its folder, its rips
    /// (each a file of its own size) and whether it shares the folder.
    fn with_library(name: &str) -> (Arc<Backend>, PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("telly-library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Arc::new(Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        let series = dir.join("Series").to_string_lossy().into_owned();
        std::fs::create_dir_all(&series).unwrap();
        backend.db.set_setting("libraries", &json!([library("series", &[series.as_str()])])).unwrap();
        (backend, dir, series)
    }

    fn add_show(backend: &Backend, folder: &str, rips: &[&str], shares: bool) -> i64 {
        let episodes = rips
            .iter()
            .enumerate()
            .map(|(i, rip)| {
                let path = join(folder, &parts(rip));
                std::fs::create_dir_all(folder_of(&path)).unwrap();
                // Sizes no two rips share, as real ones don't.
                let size = 1000 + path.len() * 7 + i;
                std::fs::write(&path, vec![0u8; size]).unwrap();
                NewEpisode { number: i as i64 + 1, path, size: size as i64, duration: 1400.0 }
            })
            .collect();
        let id = backend.db.add_show(&NewShow { folder: folder.into(), title: folder_of(&join(folder, &["x"])).rsplit(['/', '\\']).next().unwrap_or_default().into(), episodes, ..Default::default() }).unwrap();
        if shares {
            backend.db.set_setting(&shared_setting(id), &json!(true)).unwrap();
        }
        id
    }

    #[test]
    fn a_show_whose_folder_is_deleted_from_a_library_leaves_telly_and_one_renamed_is_found() {
        let (backend, dir, series) = with_library("gone");
        let there = |id: i64| backend.db.show(id).unwrap().is_some();
        let kept = add_show(&backend, &join(&series, &["Kept"]), &["Kept - 01.mkv", "Kept - 02.mkv"], false);
        let deleted = add_show(&backend, &join(&series, &["Deleted"]), &["Deleted - 01.mkv"], false);
        let renamed = add_show(&backend, &join(&series, &["[Group] Renamed (01-02)"]), &["Season 01/Renamed - 01.mkv", "Season 01/Renamed - 02.mkv"], false);
        let film = add_show(&backend, &series, &["Film (2001).mkv"], true);
        let boxed = add_show(&backend, &series, &["Boxed (2002).mkv"], true);
        // Not in a library: its folder gone is for Find its folder…, as before.
        let outside = add_show(&backend, &dir.join("Elsewhere").join("Outside").to_string_lossy(), &["Outside - 01.mkv"], false);
        let before = backend.db.episodes(renamed).unwrap();
        backend.db.save_position(before[1].id, 600.0).unwrap();

        std::fs::remove_dir_all(join(&series, &["Deleted"])).unwrap();
        std::fs::rename(join(&series, &["[Group] Renamed (01-02)"]), join(&series, &["Renamed"])).unwrap();
        std::fs::remove_file(join(&series, &["Film (2001).mkv"])).unwrap();
        // A loose film put in a folder of its own.
        std::fs::create_dir_all(join(&series, &["Boxed (2002)"])).unwrap();
        std::fs::rename(join(&series, &["Boxed (2002).mkv"]), join(&series, &["Boxed (2002)", "Boxed (2002).mkv"])).unwrap();
        std::fs::remove_dir_all(dir.join("Elsewhere")).unwrap();
        gone(&backend, &series).unwrap();

        assert_eq!((there(kept), there(deleted), there(film), there(outside)), (true, false, false, true));
        // Renamed, it's the same show where it is now, with where you were in it.
        assert_eq!(backend.db.show(renamed).unwrap().unwrap().folder, join(&series, &["Renamed"]));
        let after = backend.db.episodes(renamed).unwrap();
        assert_eq!((after[1].id, after[1].path.as_str(), after[1].position), (before[1].id, join(&series, &["Renamed", "Season 01", "Renamed - 02.mkv"]).as_str(), 600.0));
        assert_eq!(backend.db.show(boxed).unwrap().unwrap().folder, join(&series, &["Boxed (2002)"]));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_show_leaves_while_its_library_s_folder_is_away_or_most_of_it_is_gone_at_once() {
        let (backend, dir, series) = with_library("away");
        let shows: Vec<i64> = (1..=8).map(|n| add_show(&backend, &join(&series, &[&format!("Show {n}")]), &[&format!("Show {n} - 01.mkv")], false)).collect();
        let left = || backend.db.shows().unwrap().len();
        // Five of eight gone at once: the library moved, more likely than that you deleted them.
        for n in 1..=5 {
            std::fs::remove_dir_all(join(&series, &[&format!("Show {n}")])).unwrap();
        }
        gone(&backend, &series).unwrap();
        assert_eq!(left(), 8);
        // The folder there with nothing in it (a drive's folder with the drive away), or not there.
        for n in 6..=8 {
            std::fs::remove_dir_all(join(&series, &[&format!("Show {n}")])).unwrap();
        }
        gone(&backend, &series).unwrap();
        std::fs::remove_dir_all(&series).unwrap();
        gone(&backend, &series).unwrap();
        assert_eq!(left(), 8);
        // Fewer than five, or not most of them: they were deleted.
        std::fs::create_dir_all(&series).unwrap();
        for n in 5..=8 {
            std::fs::create_dir_all(join(&series, &[&format!("Show {n}")])).unwrap();
            std::fs::write(join(&series, &[&format!("Show {n}"), &format!("Show {n} - 01.mkv")]), b"back").unwrap();
        }
        gone(&backend, &series).unwrap();
        assert_eq!(backend.db.shows().unwrap().iter().map(|s| s.id).collect::<HashSet<i64>>(), shows[4..].iter().copied().collect::<HashSet<i64>>());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_taken_out_of_your_libraries_takes_its_shows_out_of_telly() {
        let (backend, dir, series) = with_library("removed");
        let films = dir.join("Films").to_string_lossy().into_owned();
        let save = |libraries: &[Library]| backend.call("setSetting", json!({ "key": "libraries", "value": libraries })).unwrap();
        save(&[library("series", &[series.as_str()]), library("movies", &[films.as_str()])]);
        let show = add_show(&backend, &join(&series, &["Show"]), &["Show - 01.mkv"], false);
        let film = add_show(&backend, &join(&films, &["Film"]), &["Film.mkv"], false);
        let outside = add_show(&backend, &dir.join("Elsewhere").join("Outside").to_string_lossy(), &["Outside - 01.mkv"], false);
        pass_over(&backend, &[(join(&films, &["Film", "Extras", "NCOP.mkv"]), 90.0), (join(&series, &["Show", "NCOP.mkv"]), 90.0)]);
        // Asked first: how many would leave.
        let asked = |libraries: &[Library]| backend.call("showsLeaving", json!({ "libraries": libraries })).unwrap();
        assert_eq!(asked(&[library("series", &[series.as_str()])]), json!(1));
        assert_eq!(asked(&[library("series", &[series.as_str()]), library("movies", &[dir.to_string_lossy().as_ref()])]), json!(0));
        assert_eq!(backend.db.shows().unwrap().len(), 3);
        save(&[library("series", &[series.as_str()])]);
        let wait_until = |done: &dyn Fn() -> bool| (0..400).any(|_| done() || { std::thread::sleep(Duration::from_millis(25)); false });
        assert!(wait_until(&|| backend.db.show(film).unwrap().is_none()));
        assert!(backend.db.show(show).unwrap().is_some() && backend.db.show(outside).unwrap().is_some());
        // Your rips stay, and what was passed over there is forgotten; what's passed over in a folder still in a library isn't.
        assert!(Path::new(&join(&films, &["Film", "Film.mkv"])).is_file());
        assert_eq!(passed(&backend, false, |kept| kept.keys().cloned().collect::<Vec<_>>()), [key(&join(&series, &["Show", "NCOP.mkv"]))]);
        save(&[library("series", &[join(&series, &["Show"]).as_str()])]);
        assert_eq!(passed(&backend, false, |kept| kept.len()), 1);
        assert!(backend.db.show(show).unwrap().is_some());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_that_can_t_be_added_is_given_up_on_only_while_anilist_answers() {
        use std::cell::Cell;
        let (tries, waits) = (Cell::new(0), Cell::new(0));
        let count = |cell: &Cell<i32>| cell.set(cell.get() + 1);
        // AniList can't be asked: tried again after each wait, until it can.
        let add = || {
            count(&tries);
            if tries.get() < 3 { Err(anyhow!("AniList isn’t reachable")) } else { Ok("added") }
        };
        let wait = || {
            count(&waits);
            true
        };
        assert_eq!(tried(add, || false, wait), Some("added"));
        assert_eq!((tries.get(), waits.get()), (3, 2));
        // It answers, and the folder still can't be added: given up on at once, without a wait.
        let fails = || -> Result<()> {
            count(&tries);
            Err(anyhow!("AniList has no anime 5"))
        };
        assert_eq!(tried(fails, || true, wait), None);
        assert_eq!((tries.get(), waits.get()), (4, 2));
        // Its library removed while AniList can't be asked: no more of it.
        assert_eq!(tried(fails, || false, || false), None);
        assert_eq!(tries.get(), 5);
    }

    #[test]
    fn a_show_s_rips_are_in_the_deepest_folder_that_holds_them_all() {
        let folder = r"D:\Media\Anime\Series\One-Punch Man";
        let rips = |names: &[&str]| names.iter().map(|n| format!(r"{folder}\{n}")).collect::<Vec<_>>();
        assert_eq!(rips_folder(folder, &rips(&[r"Season 3\02.mkv", r"season 3\03.mkv"])), format!(r"{folder}\Season 3"));
        assert_eq!(rips_folder(folder, &rips(&[r"Season 01\Player Ready\01.mkv", r"Season 01\Player Ready\02.mkv"])), format!(r"{folder}\Season 01\Player Ready"));
        assert_eq!(rips_folder(folder, &rips(&[r"Disc 1\01.mkv", r"Disc 2\02.mkv"])), folder);
        assert_eq!(rips_folder(folder, &rips(&["01.mkv"])), folder);
        assert_eq!(rips_folder(folder, &[]), folder);
        // A rip that isn't below the show's folder at all.
        assert_eq!(rips_folder(folder, &[r"E:\Elsewhere\01.mkv".to_string()]), folder);
    }
}
