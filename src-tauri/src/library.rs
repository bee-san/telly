//! Your library folders: where a download goes when you've said which show
//! it is. Settings → Library names a folder for series and one for movies,
//! and Telly's downloader writes a release straight into its place there, so
//! nothing is moved once it's down:
//!
//!   a season  <series folder>\<anime>\Season NN
//!   a film    <movies folder>\<title> (<year>)
//!
//! - The anime's folder is the one its seasons are in already, whatever it's
//!   called (a release's own name, say); a new anime's is named as its card
//!   in the Library.
//! - NN is the season's place on AniList's line of sequels from the first:
//!   the line parts.rs follows to read season folders, so what's written
//!   here is read back the same.
//! - A part off that line (a spin-off, an OVA) goes by its own name, and so
//!   does a season whose folder is there already, holding another show's rips.
//! - A film goes with the movies even when it belongs to an anime with
//!   seasons: the Library groups by AniList's relations, not by folder.
//! - A release of several parts (a folder for each season) goes in the
//!   anime's folder as it is.
//!
//! Files keep the names they were released with. A folder you haven't chosen
//! is the one your series or films are kept in already, saved the first time
//! a download needs it. With no folder for its kind (nothing of that kind in
//! the library yet, or the folder set empty in Settings), a download goes in
//! the downloads folder as before (torrents.rs).
//!
//! Settings: library.series and library.movies (the two folders; empty for
//! the downloads folder).

use crate::backend::{shared_setting, Backend};
use crate::db::Show;
use crate::relink::{below, folder_of, join};
use crate::series::Graph;
use crate::util::safe_file_name;
use anyhow::Result;
use serde_json::json;
use std::collections::HashSet;

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

/// The folder a download of the AniList entry `id` goes in, or none when no
/// folder is chosen for its kind. `whole`: the release holds several parts.
/// `taken` says whether a folder is there already with something in it.
pub fn place(series: &str, movies: &str, graph: &Graph, shows: &[Show], id: i64, whole: bool, taken: impl Fn(&str) -> bool) -> Option<String> {
    let entry = graph.entry(id)?;
    let title = entry.anime.short_title();
    if entry.anime.format == "MOVIE" && !whole {
        let named = entry.anime.year.map_or(title.clone(), |year| format!("{title} ({year})"));
        return (!movies.is_empty()).then(|| join(movies, &[&folder_name(&named)]));
    }
    if series.is_empty() {
        return None;
    }
    let root = graph.root(id).unwrap_or(id);
    let anime = anime_folder(series, graph, shows, root).unwrap_or_else(|| {
        let named = graph.entry(root).map_or_else(|| title.clone(), |e| e.anime.short_title());
        join(series, &[&folder_name(&named)])
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

/// The series and movies folders (settings library.series and
/// library.movies). One never chosen is the folder your series or films are
/// kept in already (`kept_in`), saved as if you'd chosen it.
pub fn folders(backend: &Backend, graph: &Graph, shows: &[Show]) -> Result<(String, String)> {
    let folder = |key: &str, movies: bool| -> Result<String> {
        if let Some(chosen) = backend.db.setting(key)? {
            return Ok(chosen.as_str().unwrap_or_default().trim().to_string());
        }
        let Some(found) = kept_in(graph, shows, movies) else { return Ok(String::new()) };
        backend.db.set_setting(key, &json!(found))?;
        Ok(found)
    };
    Ok((folder("library.series", false)?, folder("library.movies", true)?))
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

    const SERIES: &str = r"D:\Media\Anime\Series";
    const MOVIES: &str = r"D:\Media\Anime\Movies";

    #[test]
    fn a_new_anime_gets_a_folder_and_a_season_folder() {
        let graph = Graph::new(overlord());
        let free = |_: &str| false;
        assert_eq!(place(SERIES, MOVIES, &graph, &[], 20832, false, free).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Season 01"));
        // The season's number is its place on the line of sequels, past the films between.
        assert_eq!(place(SERIES, MOVIES, &graph, &[], 133844, false, free).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Season 04"));
    }

    #[test]
    fn the_next_season_goes_where_the_anime_s_seasons_are() {
        let graph = Graph::new(overlord());
        let pack = r"D:\Media\Anime\Series\[Judas] Overlord (Seasons 1-3) [BD 1080p]";
        let shows = [show(1, 20832, &format!(r"{pack}\[Judas] Overlord - S1")), show(2, 98437, &format!(r"{pack}\[Judas] Overlord - S2"))];
        assert_eq!(place(SERIES, MOVIES, &graph, &shows, 133844, false, |_| false), Some(format!(r"{pack}\Season 04")));
        // A show elsewhere says nothing of where the anime is here.
        let elsewhere = [show(1, 20832, r"C:\Videos\Overlord")];
        assert_eq!(place(SERIES, MOVIES, &graph, &elsewhere, 98437, false, |_| false).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Season 02"));
    }

    #[test]
    fn a_film_goes_with_the_movies_and_a_side_story_by_its_own_name() {
        let graph = Graph::new(overlord());
        let shows = [show(1, 20832, r"D:\Media\Anime\Series\Overlord\Season 01")];
        assert_eq!(place(SERIES, MOVIES, &graph, &shows, 133845, false, |_| false).as_deref(), Some(r"D:\Media\Anime\Movies\Overlord - The Sacred Kingdom (2024)"));
        assert_eq!(place(SERIES, MOVIES, &graph, &shows, 21305, false, |_| false).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Overlord - Ple Ple Pleiades"));
    }

    #[test]
    fn a_season_folder_that_s_another_show_s_is_left_alone() {
        let graph = Graph::new(overlord());
        let taken = |folder: &str| folder.ends_with("Season 02");
        assert_eq!(place(SERIES, MOVIES, &graph, &[], 98437, false, taken).as_deref(), Some(r"D:\Media\Anime\Series\Overlord\Overlord II"));
    }

    #[test]
    fn a_release_of_several_parts_goes_in_the_anime_s_folder() {
        let graph = Graph::new(overlord());
        assert_eq!(place(SERIES, MOVIES, &graph, &[], 20832, true, |_| false).as_deref(), Some(r"D:\Media\Anime\Series\Overlord"));
    }

    #[test]
    fn nothing_is_placed_without_a_folder_for_its_kind() {
        let graph = Graph::new(overlord());
        assert_eq!(place("", MOVIES, &graph, &[], 20832, false, |_| false), None);
        assert_eq!(place(SERIES, "", &graph, &[], 133845, false, |_| false), None);
        // Nor an entry Telly hasn't asked AniList about.
        assert_eq!(place(SERIES, MOVIES, &Graph::new([entry(1, "Other", "TV", 12, 20200101, &[])]), &[], 2, false, |_| false), None);
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
    fn a_folder_never_chosen_is_saved_and_one_cleared_stays_clear() {
        use crate::backend::Dirs;
        let dir = std::env::temp_dir().join(format!("telly-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap();
        let graph = Graph::new(overlord());
        let mut film = show(1, 98873, r"D:\Films\Overlord - The Undead King (2017)");
        film.format = "MOVIE".into();
        backend.db.set_setting("library.series", &json!("")).unwrap();
        assert_eq!(folders(&backend, &graph, &[film]).unwrap(), (String::new(), r"D:\Films".to_string()));
        assert_eq!(backend.db.setting_str("library.movies"), r"D:\Films");
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
