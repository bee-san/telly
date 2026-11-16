//! Finding your rips again after they moved: a show's folder moved or
//! renamed, or a whole library now on another drive, or its drive's letter
//! changed. Each episode is matched to a file in the new place, by its path
//! below the show's folder first, then by its file name and size, and only
//! the show's folder and its episodes' paths change: everything Telly keeps by
//! an episode's id (where you were, what you watched, the subtitles, kept
//! explanations and recaps, the seek bar's pictures, your cards) stays with
//! it. No file is matched to two episodes, and an episode that isn't found is
//! left as it was. What's found is shown first, and changes only when you say
//! so. The rips are only looked at, never moved or changed.
//!
//! Paths are compared as Windows has them: split at \ or /, ignoring case.

use crate::backend::Backend;
use crate::db::{Episode, Show};
use crate::{jobs, scan};
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// An episode's rip found in a new place: where Telly had it, and where it is now.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    pub id: i64,
    pub number: i64,
    pub from: String,
    pub to: String,
}

/// An episode whose rip wasn't found in the new place: it's left where Telly had it.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Unfound {
    pub id: i64,
    pub number: i64,
    pub path: String,
}

/// What was found of a show's rips in the folder it's in now (`to`), which
/// Telly has as `from`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ShowMove {
    pub show_id: i64,
    pub title: String,
    pub from: String,
    pub to: String,
    pub found: Vec<Move>,
    pub missing: Vec<Unfound>,
}

/// A show whose rips aren't all where Telly has them: its folder isn't there
/// (its drive unplugged, or the folder moved or renamed), or some of its
/// episodes' files aren't.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MissingRips {
    pub show_id: i64,
    pub title: String,
    pub folder: String,
    pub folder_missing: bool,
    pub episodes: usize,
    /// How many of its episodes' rips aren't there: all of them when its folder isn't.
    pub missing: usize,
}

/// A path's folders and file name, split at \ or / as Windows splits them:
/// D:\Anime\Show is D:, Anime and Show.
pub fn parts(path: &str) -> Vec<&str> {
    path.split(['\\', '/']).filter(|p| !p.is_empty()).collect()
}

/// Whether two folder or file names are the same name, as Windows has them (ignoring case).
fn same(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// What's left of `path` below `folder`, by whole folders and ignoring case:
/// D:\Anime\Show\01.mkv below d:\anime is Show and 01.mkv, but D:\Anime2\Show
/// isn't below D:\Anime. Nothing is left of `folder` itself; None when `path`
/// isn't below it.
pub fn below<'a>(folder: &str, path: &'a str) -> Option<Vec<&'a str>> {
    let (folder, path) = (parts(folder), parts(path));
    (path.len() >= folder.len() && folder.iter().zip(&path).all(|(a, b)| same(a, b))).then(|| path[folder.len()..].to_vec())
}

/// `rest` (from `below`) put below `folder`, with the separator `folder` uses.
pub fn join(folder: &str, rest: &[&str]) -> String {
    if rest.is_empty() {
        return folder.to_string();
    }
    let separator = if folder.contains('\\') {
        '\\'
    } else if folder.contains('/') {
        '/'
    } else {
        std::path::MAIN_SEPARATOR
    };
    let mut out = folder.trim_end_matches(['\\', '/']).to_string();
    for part in rest {
        out.push(separator);
        out.push_str(part);
    }
    out
}

/// The folder a file is in: everything before its name.
pub fn folder_of(path: &str) -> &str {
    path.rfind(['\\', '/']).map_or("", |i| &path[..i])
}

/// Each of `episodes`' rips among `files` (each file's path and size), the
/// video files of the folder `to`, which Telly has as `from`: the file at the
/// same place below the folder, of the same size when Telly knows the
/// episode's; else the one file there of the same name and size. An episode
/// whose size Telly doesn't know is found by its place alone, and one whose
/// name and size two files there have (or two episodes do) isn't found. No
/// file is matched to two episodes. In `episodes`' order.
pub fn find(from: &str, to: &str, episodes: &[Episode], files: &[(String, i64)]) -> Vec<Move> {
    let place = |rest: &[&str]| rest.iter().map(|p| p.to_lowercase()).collect::<Vec<_>>().join("/");
    let name = |path: &str| parts(path).last().map(|n| n.to_lowercase()).unwrap_or_default();
    let by_place: HashMap<String, usize> = files.iter().enumerate().filter_map(|(i, (path, _))| Some((place(&below(to, path)?), i))).collect();
    let mut taken: HashSet<usize> = HashSet::new();
    let mut found: HashMap<i64, usize> = HashMap::new();
    for e in episodes {
        let Some(&i) = below(from, &e.path).and_then(|rest| by_place.get(&place(&rest))) else { continue };
        if (e.size <= 0 || files[i].1 == e.size) && taken.insert(i) {
            found.insert(e.id, i);
        }
    }
    // Then by name and size, where one episode and one file left have them.
    let mut files_by: HashMap<(String, i64), Vec<usize>> = HashMap::new();
    for (i, (path, size)) in files.iter().enumerate().filter(|(i, _)| !taken.contains(i)) {
        files_by.entry((name(path), *size)).or_default().push(i);
    }
    let mut episodes_by: HashMap<(String, i64), Vec<i64>> = HashMap::new();
    for e in episodes.iter().filter(|e| e.size > 0 && !found.contains_key(&e.id)) {
        episodes_by.entry((name(&e.path), e.size)).or_default().push(e.id);
    }
    for (key, ids) in episodes_by {
        if let (&[id], Some(&[i])) = (ids.as_slice(), files_by.get(&key).map(Vec::as_slice)) {
            found.insert(id, i);
        }
    }
    episodes
        .iter()
        .filter_map(|e| found.get(&e.id).map(|&i| Move { id: e.id, number: e.number, from: e.path.clone(), to: files[i].0.clone() }))
        .collect()
}

/// Whether a show's rips are all where Telly has them; None when they are.
/// Its folder is looked at, and, when it's there, each episode's file.
pub fn missing(show: &Show, episodes: &[Episode]) -> Option<MissingRips> {
    let folder_missing = !Path::new(&show.folder).is_dir();
    let missing = if folder_missing { episodes.len() } else { episodes.iter().filter(|e| !Path::new(&e.path).is_file()).count() };
    (folder_missing || missing > 0).then(|| MissingRips {
        show_id: show.id,
        title: show.title.clone(),
        folder: show.folder.clone(),
        folder_missing,
        episodes: episodes.len(),
        missing,
    })
}

/// Every show whose rips aren't all where Telly has them.
pub fn missing_shows(backend: &Backend) -> Result<Vec<MissingRips>> {
    let mut by_show: HashMap<i64, Vec<Episode>> = HashMap::new();
    for e in backend.db.all_episodes()? {
        by_show.entry(e.show_id).or_default().push(e);
    }
    Ok(backend.db.shows()?.iter().filter_map(|show| missing(show, by_show.get(&show.id).map_or(&[], Vec::as_slice))).collect())
}

/// The video files in `folder` and the folders below it, as a show's folder
/// is read when it's added (scan.rs), with their sizes.
fn files_in(folder: &str) -> Vec<(String, i64)> {
    let mut paths: Vec<PathBuf> = Vec::new();
    scan::walk(Path::new(folder), 0, &mut paths);
    paths
        .into_iter()
        .map(|p| {
            let size = std::fs::metadata(&p).map(|m| m.len() as i64).unwrap_or(0);
            (p.to_string_lossy().into_owned(), size)
        })
        .collect()
}

/// What's found of `show`'s rips in `to`, the folder it's in now.
fn look(backend: &Backend, show: &Show, to: &str) -> Result<ShowMove> {
    let episodes = backend.db.episodes(show.id)?;
    let found = find(&show.folder, to, &episodes, &files_in(to));
    let ids: HashSet<i64> = found.iter().map(|m| m.id).collect();
    let missing = episodes.iter().filter(|e| !ids.contains(&e.id)).map(|e| Unfound { id: e.id, number: e.number, path: e.path.clone() }).collect();
    Ok(ShowMove { show_id: show.id, title: show.title.clone(), from: show.folder.clone(), to: to.to_string(), found, missing })
}

fn a_folder(folder: &str) -> Result<String> {
    let folder = folder.trim();
    if folder.is_empty() || !Path::new(folder).is_dir() {
        return Err(anyhow!("Telly can’t open the folder {folder}"));
    }
    Ok(folder.to_string())
}

/// Finds a show's rips in `folder`, the folder it's in now (Find its folder…
/// on its page). With `apply`, the show is pointed there and its episodes
/// found at their files; without, nothing changes.
pub fn show(backend: &Backend, id: i64, folder: &str, apply: bool) -> Result<ShowMove> {
    let show = backend.db.show(id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let found = look(backend, &show, &a_folder(folder)?)?;
    if apply {
        relink(backend, std::slice::from_ref(&found))?;
    }
    Ok(found)
}

/// Finds the rips of every show whose folder is below `from` below `to`
/// instead ("everything in D:\Anime is now in E:\Anime", in Settings). With
/// `apply`, each show any of whose episodes were found is pointed there, all
/// in one go, and so are your libraries' folders and the downloads folder
/// that were below `from`; without, nothing changes.
pub fn library(backend: &Backend, from: &str, to: &str, apply: bool) -> Result<Vec<ShowMove>> {
    let (from, to) = (from.trim(), a_folder(to)?);
    if from.is_empty() {
        return Err(anyhow!("Say which folder your rips were in"));
    }
    let mut moves = Vec::new();
    for show in backend.db.shows()? {
        if let Some(rest) = below(from, &show.folder) {
            moves.push(look(backend, &show, &join(&to, &rest))?);
        }
    }
    if moves.is_empty() {
        return Err(anyhow!("No show in Telly is in {from}"));
    }
    moves.sort_by(|a, b| a.from.cmp(&b.from));
    if apply {
        relink(backend, &moves)?;
        move_folders(backend, from, &to)?;
    }
    Ok(moves)
}

/// Your libraries' folders (those still to be read for shows too), and the
/// folder new shows are downloaded to, that were below `from`: below `to`
/// now, as the rips there are.
fn move_folders(backend: &Backend, from: &str, to: &str) -> Result<()> {
    let moved = |folder: &str| below(from, folder).map(|rest| join(to, &rest));
    if backend.db.setting("libraries")?.is_some() {
        let mut libraries = crate::library::libraries(backend)?;
        let mut changed = false;
        for l in &mut libraries {
            for f in l.folders.iter_mut().chain(std::iter::once(&mut l.downloads)) {
                if let Some(now) = moved(f) {
                    *f = now;
                    changed = true;
                }
            }
        }
        if changed {
            backend.db.set_setting("libraries", &serde_json::to_value(&libraries)?)?;
        }
    }
    // A library's folder still to be read for shows (its drive was away) is read where it is now.
    let adding: Vec<String> = backend.db.setting("libraries.adding")?.and_then(|kept| serde_json::from_value(kept).ok()).unwrap_or_default();
    if adding.iter().any(|f| moved(f).is_some()) {
        let now: Vec<String> = adding.iter().map(|f| moved(f).unwrap_or_else(|| f.clone())).collect();
        backend.db.set_setting("libraries.adding", &serde_json::json!(now))?;
    }
    if let Some(now) = moved(&backend.db.setting_str("downloads.folder")) {
        backend.db.set_setting("downloads.folder", &serde_json::json!(now))?;
    }
    Ok(())
}

/// Points shows at the folders they're in now and their episodes at where
/// their rips were found (shows none of whose episodes were found stay as
/// they are), in one go. What else Telly keeps by where a rip is goes with
/// it, and what was given up on while it wasn't there is tried again.
pub fn relink(backend: &Backend, moves: &[ShowMove]) -> Result<()> {
    let moves: Vec<&ShowMove> = moves.iter().filter(|m| !m.found.is_empty()).collect();
    let shows: Vec<(i64, String)> = moves.iter().map(|m| (m.show_id, m.to.clone())).collect();
    let episodes: Vec<&Move> = moves.iter().flat_map(|m| &m.found).collect();
    relink_episodes(backend, &shows, &episodes)
}

/// Points shows at new folders and episodes at where their rips are now (see `relink`).
pub fn relink_episodes(backend: &Backend, shows: &[(i64, String)], episodes: &[&Move]) -> Result<()> {
    let paths: Vec<(i64, String)> = episodes.iter().map(|m| (m.id, m.to.clone())).collect();
    backend.db.relink(shows, &paths)?;
    for m in episodes {
        jobs::rip_moved(backend, m.id, &m.from, &m.to);
    }
    backend.jobs.look_again(&paths.iter().map(|(id, _)| *id).collect::<Vec<_>>());
    backend.jobs.poke();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode(id: i64, path: &str, size: i64) -> Episode {
        Episode { id, number: id, path: path.into(), size, ..Default::default() }
    }

    fn file(path: &str, size: i64) -> (String, i64) {
        (path.into(), size)
    }

    #[test]
    fn a_path_is_below_a_folder_by_whole_folders_ignoring_case() {
        assert_eq!(below(r"D:\Anime", r"D:\Anime\Show\01.mkv"), Some(vec!["Show", "01.mkv"]));
        assert_eq!(below(r"d:\anime\", r"D:\ANIME\Show"), Some(vec!["Show"]));
        assert_eq!(below(r"D:\Anime", r"D:\Anime2\Show"), None);
        assert_eq!(below(r"D:\Anime\Show", r"D:\Anime"), None);
        assert_eq!(below(r"D:\Anime", r"D:\Anime"), Some(vec![]));
        assert_eq!(below("/media/me/Anime", "/media/me/Anime/Show/01.mkv"), Some(vec!["Show", "01.mkv"]));
        assert_eq!(below("/media/me/Anime", "/media/me/Anime 2/Show"), None);
        assert_eq!(below(r"\\nas\anime", r"\\NAS\Anime\Show"), Some(vec!["Show"]));
        // Put below another folder, with that folder's separator.
        assert_eq!(join(r"E:\Anime", &["Show", "01.mkv"]), r"E:\Anime\Show\01.mkv");
        assert_eq!(join(r"E:\", &["Show"]), r"E:\Show");
        assert_eq!(join("/mnt/anime/", &["Show"]), "/mnt/anime/Show");
        assert_eq!(join(r"E:\", &[]), r"E:\");
        assert_eq!(folder_of(r"E:\Anime\Show\01.mkv"), r"E:\Anime\Show");
        assert_eq!(folder_of("/mnt/Show/01.mkv"), "/mnt/Show");
    }

    #[test]
    fn episodes_are_found_by_their_place_then_by_name_and_size() {
        let episodes = [
            episode(1, r"D:\Anime\Show\Season 1\Show - 01.mkv", 100),
            episode(2, r"D:\Anime\Show\Season 1\Show - 02.mkv", 200),
            // Moved into another folder of the show's: found by its name and size.
            episode(3, r"D:\Anime\Show\Show - 03.mkv", 300),
            // In its place, but another file (another size): a new release, not found.
            episode(4, r"D:\Anime\Show\Season 1\Show - 04.mkv", 400),
            // Nowhere.
            episode(5, r"D:\Anime\Show\Season 1\Show - 05.mkv", 500),
        ];
        let files = [
            file(r"E:\Anime\Show\season 1\SHOW - 01.mkv", 100),
            file(r"E:\Anime\Show\Season 1\Show - 02.mkv", 200),
            file(r"E:\Anime\Show\Extras\Show - 03.mkv", 300),
            file(r"E:\Anime\Show\Season 1\Show - 04.mkv", 444),
        ];
        let found = find(r"D:\Anime\Show", r"E:\Anime\Show", &episodes, &files);
        let to: Vec<(i64, &str)> = found.iter().map(|m| (m.id, m.to.as_str())).collect();
        assert_eq!(to, [(1, files[0].0.as_str()), (2, files[1].0.as_str()), (3, files[2].0.as_str())]);
        assert_eq!(found[0].from, episodes[0].path);
        // Unix-like paths, and an episode whose size Telly doesn't know: found by its place alone.
        let episodes = [episode(1, "/media/old/Show/01.mkv", 0), episode(2, "/media/old/Show/02.mkv", 0)];
        let files = [file("/media/new/Show/01.mkv", 7), file("/media/new/Show/Other/02.mkv", 8)];
        let found = find("/media/old/Show", "/media/new/Show", &episodes, &files);
        assert_eq!(found.iter().map(|m| m.to.as_str()).collect::<Vec<_>>(), ["/media/new/Show/01.mkv"]);
    }

    #[test]
    fn no_file_is_matched_to_two_episodes() {
        // Two episodes of one name and size (copies of one rip in two season
        // folders), and two files of it where neither is in its place: neither is found.
        let episodes = [episode(1, r"D:\Show\A\01.mkv", 100), episode(2, r"D:\Show\B\01.mkv", 100)];
        let files = [file(r"E:\Show\X\01.mkv", 100), file(r"E:\Show\Y\01.mkv", 100)];
        assert!(find(r"D:\Show", r"E:\Show", &episodes, &files).is_empty());
        // One file of that name and size: it's neither's, since either could be it.
        assert!(find(r"D:\Show", r"E:\Show", &episodes, &files[..1]).is_empty());
        // One episode in its place takes its file, and the other isn't given it too.
        let files = [file(r"E:\Show\A\01.mkv", 100)];
        let found = find(r"D:\Show", r"E:\Show", &episodes, &files);
        assert_eq!(found.iter().map(|m| m.id).collect::<Vec<_>>(), [1]);
        // Two episodes whose paths differ only in case are one place: the first takes it.
        let episodes = [episode(1, r"D:\Show\01.mkv", 0), episode(2, r"D:\Show\01.MKV", 0)];
        let found = find(r"D:\Show", r"E:\Show", &episodes, &[file(r"E:\Show\01.mkv", 5)]);
        assert_eq!(found.iter().map(|m| m.id).collect::<Vec<_>>(), [1]);
    }

    fn temp_backend(name: &str) -> (Backend, PathBuf) {
        let dir = std::env::temp_dir().join(format!("telly-relink-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data");
        (Backend::open(crate::backend::Dirs { local: data.clone(), roaming: data }).unwrap(), dir)
    }

    /// A show in `folder` of `count` rips in its folder `within` (each its number times 10 bytes), added to Telly.
    fn add_show(backend: &Backend, folder: &Path, within: &str, count: i64) -> i64 {
        std::fs::create_dir_all(folder.join(within)).unwrap();
        let episodes = (1..=count)
            .map(|number| {
                let path = folder.join(within).join(format!("Show - {number:02}.mkv"));
                std::fs::write(&path, vec![0u8; number as usize * 10]).unwrap();
                crate::db::NewEpisode { number, path: path.to_string_lossy().into_owned(), size: number * 10, duration: 1400.0 }
            })
            .collect();
        let title = folder.file_name().unwrap().to_string_lossy().into_owned();
        backend.db.add_show(&crate::db::NewShow { folder: folder.to_string_lossy().into_owned(), title, episodes, ..Default::default() }).unwrap()
    }

    #[test]
    fn a_show_found_in_its_new_folder_keeps_everything_of_its_episodes() {
        let (backend, dir) = temp_backend("show");
        let old = dir.join("D").join("Anime").join("Show");
        let id = add_show(&backend, &old, "Season 1", 3);
        let eps = backend.db.episodes(id).unwrap();
        let text = |p: &Path| p.to_string_lossy().into_owned();
        // Episode 1 plays as it is and is watched, its subtitles a file of yours beside it.
        backend.db.media_ready(eps[0].id, &eps[0].path, 0, 0).unwrap();
        backend.db.set_watched(eps[0].id, true).unwrap();
        let beside = old.join("Season 1").join("Show - 01.ja.srt");
        std::fs::write(&beside, "1\n00:00:01,000 --> 00:00:02,000\nせりふ\n").unwrap();
        let jimaku = text(&backend.dirs.jimaku().join("1").join("Show - 01.srt"));
        let tried = [
            jobs::Tried { name: "Show - 01.ja.srt".into(), path: text(&beside), score: 0.0, offset_ms: 0, error: String::new(), beside: true },
            jobs::Tried { name: "Show - 01.srt".into(), path: jimaku.clone(), score: 0.9, offset_ms: 0, error: String::new(), beside: false },
        ];
        std::fs::create_dir_all(backend.dirs.subs()).unwrap();
        std::fs::write(backend.dirs.subs().join(format!("{}.tried.json", eps[0].id)), serde_json::to_vec(&tried).unwrap()).unwrap();
        let timing = backend.dirs.subs().join(format!("{}.timing.json", eps[0].id));
        let kept = serde_json::json!({ "rip": eps[0].path, "size": 10, "spans": [], "length": 1400.0, "against": "the speech", "lead": 0.0, "calibration": [0.0, 1.0, 0.0] });
        std::fs::write(&timing, kept.to_string()).unwrap();
        // Episode 2 is half watched, given up on while its rip wasn't there, and its subtitles failed.
        backend.db.save_position(eps[1].id, 600.0).unwrap();
        backend.db.media_failed(eps[1].id, &format!("{}: {}", jobs::GONE, eps[1].path)).unwrap();
        backend.db.set_subs_state(eps[1].id, "failed", "reading the episode’s timing").unwrap();
        // Episode 3 couldn't be converted for another reason: that stands.
        backend.db.media_failed(eps[2].id, "ffmpeg couldn’t read it").unwrap();

        // The folder moves to another drive and is renamed.
        let new = dir.join("E").join("Anime").join("Show (2019)");
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::rename(&old, &new).unwrap();
        let before = backend.db.episodes(id).unwrap();
        let away = missing_shows(&backend).unwrap();
        assert_eq!(away.iter().map(|m| (m.show_id, m.folder_missing, m.missing)).collect::<Vec<_>>(), [(id, true, 3)]);
        // A folder that isn't one is said so.
        assert!(show(&backend, id, &text(&dir.join("nowhere")), false).is_err());

        // What's found is shown first, and nothing changes.
        let looked = show(&backend, id, &text(&new), false).unwrap();
        assert_eq!((looked.from.as_str(), looked.to.as_str()), (text(&old).as_str(), text(&new).as_str()));
        assert_eq!(looked.found.iter().map(|m| m.number).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(backend.db.episodes(id).unwrap(), before);
        // Episode 3 goes before it's used: it's left as it was.
        std::fs::remove_file(new.join("Season 1").join("Show - 03.mkv")).unwrap();
        let used = show(&backend, id, &text(&new), true).unwrap();
        assert_eq!(used.missing, [Unfound { id: eps[2].id, number: 3, path: eps[2].path.clone() }]);

        let after = backend.db.episodes(id).unwrap();
        assert_eq!(backend.db.show(id).unwrap().unwrap().folder, text(&new));
        assert_eq!(after.iter().map(|e| e.id).collect::<Vec<_>>(), eps.iter().map(|e| e.id).collect::<Vec<_>>());
        let one = text(&new.join("Season 1").join("Show - 01.mkv"));
        assert_eq!((after[0].path.as_str(), after[0].play_path.as_str(), after[0].media_state.as_str()), (one.as_str(), one.as_str(), "ready"));
        assert!(after[0].watched_at.is_some());
        assert_eq!((after[1].position, after[1].media_state.as_str(), after[1].subs_state.as_str()), (600.0, "pending", "pending"));
        assert_eq!((after[2].path.as_str(), after[2].media_state.as_str()), (eps[2].path.as_str(), "failed"));
        // What's kept by where the rip is went with it.
        let tried: Vec<jobs::Tried> = jobs::read_tried(&backend, eps[0].id);
        assert_eq!(tried.iter().map(|t| t.path.as_str()).collect::<Vec<_>>(), [text(&new.join("Season 1").join("Show - 01.ja.srt")).as_str(), jimaku.as_str()]);
        let kept: serde_json::Value = serde_json::from_slice(&std::fs::read(&timing).unwrap()).unwrap();
        assert_eq!(kept["rip"], serde_json::json!(one));
        // Only the episode still not found is missing now.
        let away = missing_shows(&backend).unwrap();
        assert_eq!(away.iter().map(|m| (m.folder_missing, m.missing)).collect::<Vec<_>>(), [(false, 1)]);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_library_moved_to_another_drive_is_found_show_by_show() {
        let (backend, dir) = temp_backend("library");
        let (a, b) = (add_show(&backend, &dir.join("D").join("Anime").join("A"), "", 2), add_show(&backend, &dir.join("D").join("Anime").join("B"), "", 1));
        // Not in that folder, though its name starts the same.
        let c = add_show(&backend, &dir.join("D").join("Anime2").join("C"), "", 1);
        std::fs::create_dir_all(dir.join("E")).unwrap();
        std::fs::rename(dir.join("D").join("Anime"), dir.join("E").join("Anime")).unwrap();
        let text = |p: &Path| p.to_string_lossy().into_owned();
        // "D/anime": a folder is the same folder whatever its case.
        let from = text(&dir.join("D").join("anime"));
        let to = text(&dir.join("E").join("Anime"));
        let looked = library(&backend, &from, &to, false).unwrap();
        let shows: Vec<(i64, usize, usize)> = looked.iter().map(|m| (m.show_id, m.found.len(), m.missing.len())).collect();
        assert_eq!(shows, [(a, 2, 0), (b, 1, 0)]);
        assert_eq!(looked[0].to, text(&dir.join("E").join("Anime").join("A")));
        assert_eq!(backend.db.show(a).unwrap().unwrap().folder, text(&dir.join("D").join("Anime").join("A")));
        // A library and the downloads folder in there go with them.
        let series = text(&dir.join("D").join("Anime").join("Series"));
        let lib = crate::library::Library { name: "Series".into(), kind: "series".into(), folders: vec![series.clone()], downloads: series };
        backend.db.set_setting("libraries", &serde_json::json!([lib])).unwrap();
        backend.db.set_setting("downloads.folder", &serde_json::json!(text(&dir.join("D").join("Anime").join("New")))).unwrap();
        // So does a library's folder that was still to be read for shows; one elsewhere stays.
        let (waiting, elsewhere) = (text(&dir.join("D").join("Anime").join("More")), text(&dir.join("G").join("Films")));
        backend.db.set_setting("libraries.adding", &serde_json::json!([waiting, elsewhere])).unwrap();
        library(&backend, &from, &to, true).unwrap();
        assert_eq!(backend.db.setting("libraries.adding").unwrap(), Some(serde_json::json!([text(&dir.join("E").join("Anime").join("More")), elsewhere])));
        let moved = crate::library::libraries(&backend).unwrap();
        assert_eq!((moved[0].folders[0].as_str(), moved[0].downloads.as_str()), (text(&dir.join("E").join("Anime").join("Series")).as_str(), text(&dir.join("E").join("Anime").join("Series")).as_str()));
        assert_eq!(backend.db.setting_str("downloads.folder"), text(&dir.join("E").join("Anime").join("New")));
        assert_eq!(backend.db.show(a).unwrap().unwrap().folder, looked[0].to);
        assert_eq!(backend.db.episodes(b).unwrap()[0].path, text(&dir.join("E").join("Anime").join("B").join("Show - 01.mkv")));
        assert_eq!(backend.db.show(c).unwrap().unwrap().folder, text(&dir.join("D").join("Anime2").join("C")));
        assert!(missing_shows(&backend).unwrap().is_empty());
        // A folder no show is in is said so.
        assert!(library(&backend, &text(&dir.join("F")), &to, false).is_err());
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_rip_moved_within_its_show_s_folder_is_found_there_when_the_folder_is_looked_at_again() {
        let (backend, dir) = temp_backend("rescan");
        let folder = dir.join("Show");
        let id = add_show(&backend, &folder, "", 2);
        let one = backend.db.episodes(id).unwrap().remove(0);
        jobs::write_lines(&backend, one.id, &[crate::subs::Line { start: 1.0, end: 2.0, text: "せりふ".into(), ruby: vec![], speaker: String::new(), sign: false }]).unwrap();
        backend.db.set_subs(one.id, "synced", "shifted +0.0 s", "Show - 01.srt", Some(0.9), Some(0)).unwrap();
        std::fs::create_dir_all(folder.join("Season 1")).unwrap();
        let moved = folder.join("Season 1").join("Show - 01.mkv");
        std::fs::rename(&one.path, &moved).unwrap();
        // Nothing added: it's the same episode, with its subtitles, where its rip is now.
        assert_eq!(backend.rescan(id).unwrap(), 0);
        let after = backend.db.episode(one.id).unwrap().unwrap();
        assert_eq!((after.path.as_str(), after.subs_state.as_str()), (moved.to_string_lossy().as_ref(), "synced"));
        assert_eq!(jobs::read_lines(&backend, one.id).len(), 1);
        assert_eq!(backend.db.episodes(id).unwrap().len(), 2);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
