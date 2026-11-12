//! Reading a folder of rips: which files are episodes, in what order, and what
//! the show is probably called. Nothing is moved or renamed.
//!
//! Episode numbers come from the file names when every episode has one
//! ("Naruto - 04", "S01E04", "第04話", "[DVD] Naruto 004"). MakeMKV's own names
//! (title_t03.mkv, one folder per disc) and anything else are put in order by
//! folder, then name, the way Explorer sorts them, and numbered from 1.
//!
//! Creditless openings and endings, previews and the like (under 5 minutes),
//! and play-all titles (over 2 hours) are left out, as are files whose names
//! say they're extras.

use crate::media;
use crate::util::natural_cmp;
use anyhow::{anyhow, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "webm", "ts", "m2ts", "mts", "mov", "wmv", "flv", "ogm", "mpg", "mpeg",
];
const MIN_SECONDS: f64 = 5.0 * 60.0;
const MAX_SECONDS: f64 = 2.0 * 3600.0;
/// How deep to look below the chosen folder.
const MAX_DEPTH: usize = 4;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScannedFile {
    pub path: String,
    /// The path below the chosen folder, for showing.
    pub rel: String,
    pub size: i64,
    pub duration: f64,
    /// The episode number the name gives, if any.
    pub parsed: Option<i64>,
    /// The episode it becomes, or None when it's left out.
    pub number: Option<i64>,
    /// Why it's left out.
    pub reason: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Scan {
    pub folder: String,
    /// The show's name as the folder (or the files) give it, cleaned up for searching.
    pub guess: String,
    /// Episodes first, in order, then what's left out.
    pub files: Vec<ScannedFile>,
    /// "names" when the episode numbers come from the file names, "order" when Telly put them in order.
    pub numbering: String,
    /// Subfolders that each look like a show of their own (seasons in one folder).
    pub groups: Vec<Group>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub folder: String,
    pub name: String,
    pub videos: usize,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid pattern"))
}

/// Removes [group tags], (2009), {hashes} and similar from a name.
fn strip_brackets(name: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\[[^\]]*\]|\([^)]*\)|\{[^}]*\}|【[^】]*】|（[^）]*）").replace_all(name, " ").into_owned()
}

/// The episode number in a file name (without its extension), and the season if it gives one.
pub fn episode_number(stem: &str) -> Option<(Option<i64>, i64)> {
    static SXE: OnceLock<Regex> = OnceLock::new();
    static JA: OnceLock<Regex> = OnceLock::new();
    static EP: OnceLock<Regex> = OnceLock::new();
    static DASH: OnceLock<Regex> = OnceLock::new();
    static NUM: OnceLock<Regex> = OnceLock::new();
    if let Some(c) = re(&SXE, r"(?i)\bS(\d{1,2})\s*[ ._-]?\s*E(\d{1,4})").captures(stem) {
        return Some((c[1].parse().ok(), c[2].parse().ok()?));
    }
    if let Some(c) = re(&JA, r"第\s*(\d{1,4})\s*[話话回]").captures(stem) {
        return Some((None, c[1].parse().ok()?));
    }
    let bare = strip_brackets(stem).replace('_', " ");
    if let Some(c) = re(&EP, r"(?i)\b(?:ep|episode|e)\s*\.?\s*(\d{1,4})(?:v\d)?\b").captures(&bare) {
        return Some((None, c[1].parse().ok()?));
    }
    if let Some(c) = re(&DASH, r"\s-\s*(\d{1,4})(?:v\d)?(?:\s|$)").captures(&bare) {
        return Some((None, c[1].parse().ok()?));
    }
    // The last number that isn't a resolution, a codec or a year.
    let mut found = None;
    for c in re(&NUM, r"(?i)(?:^|[\s.#-])(\d{1,4})(?:v\d)?(?:$|[\s.-])").captures_iter(&bare) {
        let text = &c[1];
        let n: i64 = text.parse().ok()?;
        let resolution = matches!(n, 480 | 576 | 720 | 1080 | 2160);
        let year = text.len() == 4 && (1950..=2099).contains(&n);
        if !resolution && !year && !(text.len() == 3 && (n == 264 || n == 265)) {
            found = Some(n);
        }
    }
    found.map(|n| (None, n))
}

/// MakeMKV's title number: "title_t03" or "NARUTO_DISC1_t03" gives 3.
pub fn makemkv_title(stem: &str) -> Option<i64> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)(?:^|_)t(\d{2,3})$").captures(stem).and_then(|c| c[1].parse().ok())
}

/// Names that say a file is an extra, not an episode.
fn extra_reason(rel: &str) -> Option<&'static str> {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(
        &R,
        r"(?i)\b(NCOP|NCED|creditless|non[- ]?credit|clean (?:opening|ending)|trailers?|teasers?|previews?|promos?|PV|CM|menus?|extras?|bonus|special features|making of|interviews?)\b",
    );
    r.is_match(rel).then_some("Its name says it’s an extra")
}

/// "NARUTO DVD Box 1" becomes "NARUTO"; "[Judas] Fairy Tail (2009)" becomes "Fairy Tail".
pub fn clean_title(name: &str) -> String {
    static FORMATS: OnceLock<Regex> = OnceLock::new();
    static PARTS: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    let bare = strip_brackets(name).replace(['_', '.'], " ");
    // Words about the release, then words that come with a number ("Box 1", "Season 2").
    let formats = re(
        &FORMATS,
        r"(?i)\b(dvd|bd|bdrip|dvdrip|blu-?ray|remux|rip|makemkv|complete|collection|\d{3,4}p|x26[45]|hevc|avc|h\.?26[45]|10-?bit|8-?bit|dual[- ]audio|multi[- ]subs?|eng(?:lish)?[- ]subs?)\b",
    );
    let parts = re(&PARTS, r"(?i)\b(box(?:\s*set)?|set|disc|disk|vol(?:ume)?|part|season|series)\b\s*\d*");
    let cleaned = formats.replace_all(&bare, " ");
    let cleaned = parts.replace_all(&cleaned, " ");
    // "Fairy Tail - OVAs" searches better as "Fairy Tail OVA".
    let cleaned = cleaned.replace(" - ", " ").replace("OVAs", "OVA").replace("OADs", "OAD");
    let cleaned = re(&SPACES, r"\s+").replace_all(cleaned.trim(), " ");
    cleaned.trim_matches(|c: char| c == '-' || c.is_whitespace()).to_string()
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with('$') {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() && depth < MAX_DEPTH {
            walk(&path, depth + 1, out);
        } else if kind.is_file() && is_video(&path) {
            out.push(path);
        }
    }
}

fn relative(folder: &Path, path: &Path) -> String {
    path.strip_prefix(folder).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// The name to search AniList for: the folder's, unless it's a disc folder or
/// says nothing, then its parent's, then the files' common start. A season
/// folder's number is kept ("Show Season 2"): seasons often have the same
/// number of episodes, so only the number tells them apart.
fn guess_title(folder: &Path, stems: &[String]) -> String {
    static DISC: OnceLock<Regex> = OnceLock::new();
    static SEASON: OnceLock<Regex> = OnceLock::new();
    let disc = re(&DISC, r"(?i)^(disc|disk|dvd|bd|vol(ume)?|season)\s*\d+$");
    let season_folder = re(&SEASON, r"(?i)^season\s*(\d+)$");
    let mut season: Option<i64> = None;
    let mut dir = Some(folder);
    while let Some(d) = dir {
        let name = d.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let cleaned = clean_title(&name);
        let generic = ["anime", "videos", "video", "tv", "shows", "rips", "makemkv", "downloads"]
            .contains(&cleaned.to_lowercase().as_str());
        if !cleaned.is_empty() && !disc.is_match(name.trim()) && !generic && cleaned.chars().any(char::is_alphabetic) {
            return match season {
                Some(n) => format!("{cleaned} Season {n}"),
                None => cleaned,
            };
        }
        // Season 0 holds the specials, which AniList keeps apart from the seasons.
        if season.is_none() {
            season = season_folder.captures(name.trim()).and_then(|c| c[1].parse().ok()).filter(|&n| n > 0);
        }
        dir = d.parent();
    }
    // The longest start every file name shares, without its episode number.
    let Some(first) = stems.first() else { return String::new() };
    let mut prefix: String = first.clone();
    for s in stems {
        while !s.starts_with(&prefix) {
            prefix.pop();
        }
    }
    clean_title(prefix.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '-' || c == '_'))
}

/// Subfolders whose videos number from 1 again: seasons or shows of their own.
fn groups(folder: &Path, files: &[ScannedFile]) -> Vec<Group> {
    let mut by_dir: BTreeMap<String, Vec<&ScannedFile>> = BTreeMap::new();
    for f in files.iter().filter(|f| f.reason.is_empty()) {
        let top = f.rel.split('/').next().unwrap_or("").to_string();
        if f.rel.contains('/') {
            by_dir.entry(top).or_default().push(f);
        }
    }
    let numbered: Vec<(&String, HashSet<i64>)> = by_dir
        .iter()
        .filter(|(_, fs)| fs.len() >= 3)
        .map(|(d, fs)| (d, fs.iter().filter_map(|f| f.parsed).collect::<HashSet<_>>()))
        .collect();
    let clash = numbered.iter().enumerate().any(|(i, (_, a))| {
        numbered.iter().skip(i + 1).any(|(_, b)| a.intersection(b).count() >= 2)
    });
    // Disc folders of one season carry on from each other; seasons start again.
    if !clash {
        return Vec::new();
    }
    by_dir
        .into_iter()
        .filter(|(_, fs)| fs.len() >= 3)
        .map(|(dir, fs)| Group { folder: folder.join(&dir).to_string_lossy().into_owned(), name: dir, videos: fs.len() })
        .collect()
}

/// Orders paths (with /) folder by folder, as Explorer does: "Show" comes
/// before "Show 2nd Season", where comparing the whole paths would put the
/// space before the slash.
fn path_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.split('/'), b.split('/'));
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) => match natural_cmp(x, y) {
                std::cmp::Ordering::Equal => continue,
                order => return order,
            },
            (x, y) => return x.is_some().cmp(&y.is_some()),
        }
    }
}

/// Reads `folder`. `durations` gets the length of each video (ffprobe) and is
/// asked for all of them at once, so it can work in parallel.
pub fn scan(folder: &Path, durations: impl Fn(&[PathBuf]) -> Vec<Option<f64>>) -> Result<Scan> {
    if !folder.is_dir() {
        return Err(anyhow!("{} isn’t a folder", folder.display()));
    }
    let mut paths = Vec::new();
    walk(folder, 0, &mut paths);
    if paths.is_empty() {
        return Err(anyhow!("There are no videos in {}.", folder.display()));
    }
    paths.sort_by(|a, b| path_cmp(&relative(folder, a), &relative(folder, b)));
    let lengths = durations(&paths);

    let mut files: Vec<ScannedFile> = paths
        .iter()
        .zip(lengths)
        .map(|(path, duration)| {
            let rel = relative(folder, path);
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let duration = duration.unwrap_or(0.0);
            let reason = if let Some(why) = extra_reason(&rel) {
                why.to_string()
            } else if duration <= 0.0 {
                "Telly couldn’t read it (still downloading?)".to_string()
            } else if duration < MIN_SECONDS {
                "Under 5 minutes".to_string()
            } else if duration > MAX_SECONDS {
                "Over 2 hours (a play-all title)".to_string()
            } else {
                String::new()
            };
            ScannedFile {
                path: path.to_string_lossy().into_owned(),
                rel,
                size: std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0),
                duration,
                parsed: if makemkv_title(&stem).is_some() { None } else { episode_number(&stem).map(|(_, n)| n) },
                number: None,
                reason,
            }
        })
        .collect();

    let groups = groups(folder, &files);
    let episodes: Vec<usize> = (0..files.len()).filter(|&i| files[i].reason.is_empty()).collect();
    let parsed: Vec<i64> = episodes.iter().filter_map(|&i| files[i].parsed).collect();
    let unique: HashSet<i64> = parsed.iter().copied().collect();
    let by_names = !episodes.is_empty() && parsed.len() == episodes.len() && unique.len() == parsed.len();
    if by_names {
        for &i in &episodes {
            files[i].number = files[i].parsed;
        }
    } else {
        for (n, &i) in episodes.iter().enumerate() {
            files[i].number = Some(n as i64 + 1);
        }
    }
    files.sort_by(|a, b| match (a.number, b.number) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => natural_cmp(&a.rel, &b.rel),
    });

    let stems: Vec<String> = episodes
        .iter()
        .map(|&i| Path::new(&paths[i]).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
        .collect();
    Ok(Scan {
        folder: folder.to_string_lossy().into_owned(),
        guess: guess_title(folder, &stems),
        files,
        numbering: if by_names { "names".into() } else { "order".into() },
        groups,
    })
}

/// Video lengths through ffprobe, eight at a time.
pub fn probe_durations(ffprobe: Option<&Path>, paths: &[PathBuf]) -> Vec<Option<f64>> {
    let Some(ffprobe) = ffprobe else { return vec![None; paths.len()] };
    let mut out = vec![None; paths.len()];
    let size = paths.len().div_ceil(8).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .chunks(size)
            .map(|chunk| scope.spawn(move || chunk.iter().map(|p| media::duration(ffprobe, p).ok()).collect::<Vec<_>>()))
            .collect();
        for (chunk_index, handle) in handles.into_iter().enumerate() {
            if let Ok(found) = handle.join() {
                for (i, d) in found.into_iter().enumerate() {
                    out[chunk_index * size + i] = d;
                }
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_episode_numbers() {
        let n = |s: &str| episode_number(s).map(|(_, e)| e);
        assert_eq!(n("[Judas] Fairy Tail (2009) - 001"), Some(1));
        assert_eq!(n("Naruto - 04"), Some(4));
        assert_eq!(n("Naruto - 04v2 [1080p]"), Some(4));
        assert_eq!(n("Show.S01E04.1080p.WEB"), Some(4));
        assert_eq!(episode_number("Show S02E11"), Some((Some(2), 11)));
        assert_eq!(n("ナルト 第04話"), Some(4));
        assert_eq!(n("[DVD] Naruto 004"), Some(4));
        assert_eq!(n("Naruto Episode 12"), Some(12));
        assert_eq!(n("Naruto EP12 x264"), Some(12));
        assert_eq!(n("Show 1080p"), None);
        assert_eq!(n("[Group] Show (2009) [720p] [ABCD1234]"), None);
        assert_eq!(makemkv_title("title_t03"), Some(3));
        assert_eq!(makemkv_title("NARUTO_DISC1_t12"), Some(12));
        assert_eq!(makemkv_title("Naruto - 03"), None);
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("NARUTO DVD Box 1"), "NARUTO");
        assert_eq!(clean_title("[Judas] Fairy Tail (2009)"), "Fairy Tail");
        assert_eq!(clean_title("Shirokuma_Cafe_BD_1080p"), "Shirokuma Cafe");
        assert_eq!(clean_title("Haikyuu!! Season 2 Complete"), "Haikyuu!!");
        assert_eq!(clean_title("[Judas] Fairy Tail (2011) - OVAs"), "Fairy Tail OVA");
    }

    #[test]
    fn spots_extras() {
        assert!(extra_reason("Extras/[Judas] Fairy Tail - NCOP - 14.mkv").is_some());
        assert!(extra_reason("Fairy Tail - 014.mkv").is_none());
    }

    fn fake_folder(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("telly-scan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in files {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        }
        dir
    }

    #[test]
    fn orders_makemkv_discs_and_leaves_out_extras() {
        let dir = fake_folder(
            "makemkv",
            &["NARUTO DVD Box 1/Disc 2/title_t00.mkv", "NARUTO DVD Box 1/Disc 1/title_t01.mkv", "NARUTO DVD Box 1/Disc 1/title_t00.mkv", "NARUTO DVD Box 1/Disc 1/title_t02.mkv", "NARUTO DVD Box 1/Disc 10/title_t00.mkv"],
        );
        let folder = dir.join("NARUTO DVD Box 1");
        let scan = scan(&folder, |paths| {
            paths
                .iter()
                .map(|p| if p.ends_with("Disc 1/title_t02.mkv") { Some(90.0) } else if p.to_string_lossy().contains("Disc 10") { Some(9000.0) } else { Some(1400.0) })
                .collect()
        })
        .unwrap();
        assert_eq!(scan.guess, "NARUTO");
        assert_eq!(scan.numbering, "order");
        let eps: Vec<(&str, Option<i64>)> = scan.files.iter().map(|f| (f.rel.as_str(), f.number)).collect();
        assert_eq!(
            eps,
            [
                ("Disc 1/title_t00.mkv", Some(1)),
                ("Disc 1/title_t01.mkv", Some(2)),
                ("Disc 2/title_t00.mkv", Some(3)),
                ("Disc 1/title_t02.mkv", None),
                ("Disc 10/title_t00.mkv", None)
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn uses_names_when_every_episode_has_one_and_spots_seasons() {
        let dir = fake_folder(
            "names",
            &["Show/S1/Show - 01.mkv", "Show/S1/Show - 02.mkv", "Show/S1/Show - 03.mkv", "Show/S2/Show S2 - 01.mkv", "Show/S2/Show S2 - 02.mkv", "Show/S2/Show S2 - 03.mkv"],
        );
        let scan = scan(&dir.join("Show"), |paths| vec![Some(1400.0); paths.len()]).unwrap();
        assert_eq!(scan.groups.len(), 2);
        let one = scan_folder_with(&dir.join("Show").join("S1"));
        assert_eq!(one.numbering, "names");
        assert_eq!(one.files.iter().map(|f| f.number).collect::<Vec<_>>(), [Some(1), Some(2), Some(3)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_season_folder_keeps_its_number_in_the_guess() {
        let dir = fake_folder("season", &["KonoSuba – God’s Blessing On This Wonderful World/Season 2/KonoSuba - S02E01.mkv", "Show/Season 00/Show - S00E01.mkv"]);
        assert_eq!(scan_folder_with(&dir.join("KonoSuba – God’s Blessing On This Wonderful World").join("Season 2")).guess, "KonoSuba – God’s Blessing On This Wonderful World Season 2");
        assert_eq!(scan_folder_with(&dir.join("Show").join("Season 00")).guess, "Show");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_season_folder_named_as_the_show_comes_before_its_sequel() {
        let dir = fake_folder(
            "sequel",
            &[
                "Frieren/Sousou no Frieren 2nd Season/Sousou no Frieren 2nd Season - 01.mkv",
                "Frieren/Sousou no Frieren 2nd Season/Sousou no Frieren 2nd Season - 02.mkv",
                "Frieren/Sousou no Frieren/Sousou no Frieren - 01.mkv",
                "Frieren/Sousou no Frieren/Sousou no Frieren - 02.mkv",
            ],
        );
        let scan = scan_folder_with(&dir.join("Frieren"));
        let order: Vec<(Option<i64>, &str)> = scan.files.iter().map(|f| (f.number, f.rel.as_str())).collect();
        assert_eq!(
            order,
            [
                (Some(1), "Sousou no Frieren/Sousou no Frieren - 01.mkv"),
                (Some(2), "Sousou no Frieren/Sousou no Frieren - 02.mkv"),
                (Some(3), "Sousou no Frieren 2nd Season/Sousou no Frieren 2nd Season - 01.mkv"),
                (Some(4), "Sousou no Frieren 2nd Season/Sousou no Frieren 2nd Season - 02.mkv"),
            ]
        );
        assert_eq!(path_cmp("Show/01.mkv", "Show/Season 2/01.mkv"), std::cmp::Ordering::Less);
        assert_eq!(path_cmp("Show/Disc 2/a.mkv", "Show/Disc 10/a.mkv"), std::cmp::Ordering::Less);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn scan_folder_with(folder: &Path) -> Scan {
        scan(folder, |paths| vec![Some(1400.0); paths.len()]).unwrap()
    }
}
