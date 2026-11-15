//! Your libraries: where a download goes when you've said which show it is.
//! Settings → Libraries lists them, each a name, a kind (series or movies)
//! and its folders, one of which takes downloads. They live only in
//! Settings: the Library lists every show together whatever folder it's in.
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
//! Settings: libraries (the list). library.series and library.movies, the
//! two folders Telly had before, become the first libraries.

use crate::backend::{shared_setting, Backend};
use crate::db::Show;
use crate::relink::{below, folder_of, join};
use crate::series::Graph;
use crate::util::safe_file_name;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;

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
/// already (`kept_in`).
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
        backend.db.set_setting(key, &serde_json::Value::Null)?;
    }
    backend.db.set_setting("libraries", &json!(made))?;
    Ok(made)
}

/// Your libraries, each with how many shows in the library are in its folders, for Settings.
pub fn view(backend: &Backend) -> Result<serde_json::Value> {
    let shows = backend.db.shows()?;
    let listed: Vec<serde_json::Value> = libraries(backend)?
        .into_iter()
        .map(|l| {
            let count = shows.iter().filter(|s| l.folders.iter().any(|f| below(f, &s.folder).is_some())).count();
            json!({ "name": l.name, "kind": l.kind, "folders": l.folders, "downloads": l.downloads, "shows": count })
        })
        .collect();
    Ok(json!(listed))
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
    fn the_first_libraries_are_the_folders_telly_had_else_where_shows_are() {
        use crate::backend::Dirs;
        use crate::db::NewShow;
        let dir = std::env::temp_dir().join(format!("telly-libraries-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
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
