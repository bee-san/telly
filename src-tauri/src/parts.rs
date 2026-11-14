//! A chosen folder's parts, and the AniList entry each belongs to. A folder
//! of rips often holds a whole anime: a folder for each season, one of
//! movies, a loose OVA. Each of those is an entry of its own on AniList, so
//! each is added as a show of its own (the Library lists them together,
//! series.rs), all from one sheet.
//!
//! - Season folders are parts when their episode numbers start again in each;
//!   folders that carry on from each other (discs, or a long run kept in
//!   season folders) are one part, as before.
//! - A folder of movies is a part for each file. A folder or loose file
//!   named OVA, OAD or Specials (or Season 00) is a part. A loose file as
//!   long as a film, beside season folders, is a movie whatever it's called.
//! - A season folder named for a series off the line of sequels (a spin-off)
//!   is that series.
//! - Seasons are matched along AniList's sequels from the first: a folder
//!   with as many episodes as the entry takes it; one with more, when the
//!   entries after it make up the rest exactly, is split between them (22
//!   episodes of a third season are its 12 and the 10 of its Part 2);
//!   several that add up to one entry (a long series kept in short season
//!   folders) share it.
//! - Movies go to the anime's films by the number in their names ("Movie 2"),
//!   else by title, else in the order they came out. OVAs and specials go to
//!   the entry of that kind with as many episodes.
//!
//! What can't be placed is left without an entry: better none than the wrong one.

use crate::anilist::{self, Anime};
use crate::scan::{self, Scan, ScannedFile};
use crate::series::{Entry, Graph};
use crate::util::natural_cmp;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

/// How long a film runs at least: the hour from which one file AniList doesn't know is listed as a movie (backend.rs).
const FILM_SECONDS: f64 = 3600.0;

/// What a part holds, from its name.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Season,
    Movie,
    Ova,
    Specials,
}

/// One part of a chosen folder, to be added as a show.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    /// What it is in the folder, for showing: a folder's name, or a movie's file's.
    pub name: String,
    /// The folder it's added with.
    pub folder: String,
    pub kind: Kind,
    /// Its files: episodes first, in order, then what's left out.
    pub files: Vec<ScannedFile>,
    /// "names" when the episode numbers come from the file names, "order" when Telly put them in order.
    pub numbering: String,
    /// Its folder holds other parts' rips too (one of several movies, a loose file, half a season folder), so the folder isn't looked in for new episodes.
    pub shared: bool,
    /// The AniList entry it is, when Telly could place it.
    pub anime: Option<Anime>,
    /// What it's called when added without an entry.
    pub title: String,
    /// Something to check before adding: its count doesn't fit the entry's.
    pub note: String,
    #[serde(skip)]
    season: Option<i64>,
    #[serde(skip)]
    cour: Option<i64>,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid pattern"))
}

/// What a folder or loose file holds, by its name.
fn kind_of(name: &str) -> Kind {
    static MOVIE: OnceLock<Regex> = OnceLock::new();
    static OVA: OnceLock<Regex> = OnceLock::new();
    static SPECIALS: OnceLock<Regex> = OnceLock::new();
    if re(&MOVIE, r"(?i)\b(movies?|films?|gekijouban)\b|劇場版").is_match(name) {
        Kind::Movie
    } else if re(&OVA, r"(?i)\b(ovas?|oads?)\b").is_match(name) {
        Kind::Ova
    } else if re(&SPECIALS, r"(?i)\b(specials?|season\s*0+|s0+)\b").is_match(name) {
        Kind::Specials
    } else {
        Kind::Season
    }
}

fn stem(path: &str) -> String {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Whether a file is an episode: nothing says to leave it out. A movie may run past two hours.
fn counts(f: &ScannedFile, kind: Kind) -> bool {
    f.reason.is_empty() || (kind == Kind::Movie && f.reason.starts_with("Over 2 hours"))
}

/// Numbers a part's files as a folder's are: by their names when every
/// episode has a number of its own, else in the order Explorer lists them.
/// Episodes come first, in order, then what's left out.
fn numbered(mut files: Vec<ScannedFile>, kind: Kind) -> (Vec<ScannedFile>, String) {
    files.sort_by(|a, b| scan::path_cmp(&a.rel, &b.rel));
    for f in files.iter_mut() {
        f.number = None;
        if kind == Kind::Movie && counts(f, kind) {
            f.reason.clear();
        }
    }
    let episodes: Vec<usize> = (0..files.len()).filter(|&i| files[i].reason.is_empty()).collect();
    let parsed: Vec<i64> = episodes.iter().filter_map(|&i| files[i].parsed).collect();
    // A movie is the one episode of its entry, whatever number its name has ("Movie 2").
    let by_names = kind != Kind::Movie && !episodes.is_empty() && parsed.len() == episodes.len() && parsed.iter().collect::<HashSet<_>>().len() == parsed.len();
    for (n, &i) in episodes.iter().enumerate() {
        files[i].number = if by_names { files[i].parsed } else { Some(n as i64 + 1) };
    }
    files.sort_by(|a, b| match (a.number, b.number) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => natural_cmp(&a.rel, &b.rel),
    });
    (files, if by_names { "names".into() } else { "order".into() })
}

fn part(name: String, folder: String, kind: Kind, files: Vec<ScannedFile>, shared: bool) -> Part {
    static COUR: OnceLock<Regex> = OnceLock::new();
    let (files, numbering) = numbered(files, kind);
    // The season its name gives, else the one its files all give ("S02E05").
    let named = anilist::season(&name).filter(|_| kind == Kind::Season);
    let in_files = || {
        let mut seasons = files.iter().filter(|f| f.number.is_some()).map(|f| scan::episode_number(&stem(&f.path)).and_then(|(season, _)| season));
        let first = seasons.next()??;
        seasons.all(|s| s == Some(first)).then_some(first).filter(|&s| s > 0)
    };
    let season = named.or_else(in_files);
    let cour = re(&COUR, r"(?i)\bpart\s*([0-9])\b").captures(&name).and_then(|c| c[1].parse().ok());
    Part { name, folder, kind, files, numbering, shared, anime: None, title: String::new(), note: String::new(), season, cour }
}

/// How many episodes a part has: its files that aren't left out.
pub fn episodes(p: &Part) -> usize {
    p.files.iter().filter(|f| f.number.is_some()).count()
}

/// The parts of a scanned folder, before any is matched: one when the folder is one show.
pub fn split(scan: &Scan) -> Vec<Part> {
    let root = Path::new(&scan.folder);
    let root_name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // What the chosen folder itself is, by its name without its tags ("[Judas] Overlord - Movies").
    let root_kind = kind_of(&scan::clean_title(&root_name));
    let mut dirs: BTreeMap<String, Vec<ScannedFile>> = BTreeMap::new();
    let mut loose: Vec<ScannedFile> = Vec::new();
    for f in &scan.files {
        match f.rel.split_once('/') {
            Some((top, _)) => dirs.entry(top.to_string()).or_default().push(f.clone()),
            None => loose.push(f.clone()),
        }
    }
    let kind_in = |name: &str, files: &[ScannedFile]| match kind_of(name) {
        // A folder whose episodes are all of season 0 holds specials, whatever it's called.
        Kind::Season if files.iter().any(|f| f.reason.is_empty()) && files.iter().filter(|f| f.reason.is_empty()).all(|f| matches!(scan::episode_number(&stem(&f.path)), Some((Some(0), _)))) => Kind::Specials,
        kind => kind,
    };
    let mut seasons: Vec<(String, Vec<ScannedFile>)> = Vec::new();
    let mut others: Vec<Part> = Vec::new();
    let movies = |name: &str, folder: String, files: Vec<ScannedFile>, others: &mut Vec<Part>| {
        let shared = files.iter().filter(|f| counts(f, Kind::Movie)).count() > 1 || name.is_empty();
        for f in files.into_iter().filter(|f| counts(f, Kind::Movie)) {
            others.push(part(file_name(&f.path), folder.clone(), Kind::Movie, vec![f], shared));
        }
    };
    for (name, files) in dirs {
        if !files.iter().any(|f| counts(f, kind_in(&name, &files))) {
            continue;
        }
        let folder = root.join(&name).to_string_lossy().into_owned();
        match kind_in(&name, &files) {
            Kind::Season => seasons.push((name, files)),
            Kind::Movie => movies(&name, folder, files, &mut others),
            kind => others.push(part(name, folder, kind, files, false)),
        }
    }
    // Loose files: one named as a movie, an OVA or specials is a part of its own; the rest are episodes together.
    let mut plain: Vec<ScannedFile> = Vec::new();
    for f in loose {
        let kind = match kind_of(&stem(&f.path)) {
            // Beside season folders, a file as long as a film is one, whatever it's called ("Jujutsu Kaisen 0").
            Kind::Season if root_kind == Kind::Season && !seasons.is_empty() && f.duration >= FILM_SECONDS => Kind::Movie,
            Kind::Season => root_kind,
            kind => kind,
        };
        if !counts(&f, kind) || kind == Kind::Season {
            plain.push(f);
        } else if kind == Kind::Movie {
            movies("", scan.folder.clone(), vec![f], &mut others);
        } else {
            others.push(part(file_name(&f.path), scan.folder.clone(), kind, vec![f], true));
        }
    }
    // Season folders whose numbers start again are a part each; ones that carry on from each other are one.
    let numbers = |files: &[ScannedFile]| files.iter().filter(|f| f.reason.is_empty()).filter_map(|f| f.parsed).collect::<HashSet<i64>>();
    let sets: Vec<HashSet<i64>> = seasons.iter().map(|(_, files)| numbers(files)).collect();
    let clash = sets.iter().enumerate().any(|(i, a)| sets.iter().skip(i + 1).any(|b| a.intersection(b).count() >= 2));
    let mut parts: Vec<Part> = Vec::new();
    if clash {
        for (name, files) in seasons {
            let folder = root.join(&name).to_string_lossy().into_owned();
            parts.push(part(name, folder, Kind::Season, files, false));
        }
        if plain.iter().any(|f| f.reason.is_empty()) {
            parts.push(part(root_name.clone(), scan.folder.clone(), Kind::Season, plain, true));
        }
    } else {
        let mut run: Vec<ScannedFile> = seasons.into_iter().flat_map(|(_, files)| files).collect();
        run.extend(plain);
        if run.iter().any(|f| f.reason.is_empty()) {
            let shared = !others.is_empty();
            parts.push(part(root_name.clone(), scan.folder.clone(), Kind::Season, run, shared));
        }
    }
    parts.extend(others);
    parts
}

/// The number a movie's name gives it: "Movie 2", "Film 03".
fn movie_number(name: &str) -> Option<usize> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)\b(?:movie|film)\s*0*([0-9]{1,2})\b").captures(name).and_then(|c| c[1].parse().ok()).filter(|&n| n > 0)
}

fn released(e: &Entry) -> bool {
    e.status != "NOT_YET_RELEASED"
}

/// A part cut from `whole`: `files` of its episodes, numbered from 1 in their order.
fn cut(whole: &Part, files: Vec<ScannedFile>, range: (usize, usize)) -> Part {
    let mut files = files;
    for (n, f) in files.iter_mut().enumerate() {
        f.number = Some(n as i64 + 1);
    }
    Part { name: format!("{} · {}–{}", whole.name, range.0, range.1), files, numbering: "order".into(), shared: true, anime: None, note: String::new(), ..whole.clone() }
}

/// Splits `whole`'s episodes between `entries` in order, each taking as many as it has episodes.
fn split_between(whole: &Part, entries: &[&Entry]) -> Vec<Part> {
    let mut rest: Vec<ScannedFile> = whole.files.iter().filter(|f| f.number.is_some()).cloned().collect();
    let mut from = 1;
    let mut out = Vec::new();
    for e in entries {
        let n = e.anime.episodes.unwrap_or(0) as usize;
        let files: Vec<ScannedFile> = rest.drain(..n.min(rest.len())).collect();
        let mut p = cut(whole, files, (from, from + n - 1));
        p.anime = Some(e.anime.clone());
        from += n;
        out.push(p);
    }
    // What's left out stays with the first.
    if let Some(first) = out.first_mut() {
        first.files.extend(whole.files.iter().filter(|f| f.number.is_none()).cloned());
    }
    out
}

/// Gives each part its AniList entry: `base` is the entry the folder's name
/// found, and `graph` has the rest of its anime. `only` is true when the
/// folder is one part, whose entry `base` already is.
pub fn assign(parts: Vec<Part>, graph: &Graph, base: &Anime) -> Vec<Part> {
    let root = graph.root(base.id).unwrap_or(base.id);
    let entries: Vec<&Entry> = graph.parts(root).into_iter().filter(|e| released(e)).collect();
    let anime_title = graph.entry(root).map_or_else(|| base.short_title(), |e| e.anime.short_title());
    let only = parts.len() == 1;
    let (seasons, others): (Vec<Part>, Vec<Part>) = parts.into_iter().partition(|p| p.kind == Kind::Season);
    let mut out: Vec<Part> = Vec::new();
    let mut taken: HashSet<i64> = HashSet::new();

    // A season folder named for a series that isn't one of the sequels (a spin-off) is that series:
    // "An Explosion on This Wonderful World" beside KonoSuba's Season 1, 2 and 3.
    let line: HashSet<i64> = graph.sequels_from(graph.entry(root).filter(|e| e.is_series()).map_or(base.id, |e| e.anime.id)).iter().map(|e| e.anime.id).collect();
    let mut on_line: Vec<Part> = Vec::new();
    for mut p in seasons {
        let name = scan::clean_title(&p.name);
        let fit = |e: &Entry| e.anime.names().map(|t| anilist::likeness(&name, t)).fold(0.0, f64::max);
        let sequel = entries.iter().filter(|e| line.contains(&e.anime.id)).map(|e| fit(e)).fold(0.0, f64::max);
        let aside = entries.iter().filter(|e| e.is_series() && !line.contains(&e.anime.id) && !taken.contains(&e.anime.id)).map(|e| (fit(e), *e)).max_by(|a, b| a.0.total_cmp(&b.0));
        match aside {
            Some((fit, e)) if !only && fit >= 0.6 && fit > sequel => {
                taken.insert(e.anime.id);
                p.anime = Some(e.anime.clone());
                out.push(p);
            }
            _ => on_line.push(p),
        }
    }
    let mut seasons = on_line;
    seasons.sort_by(|a, b| (a.season.unwrap_or(0), a.cour.unwrap_or(0)).cmp(&(b.season.unwrap_or(0), b.cour.unwrap_or(0))).then_with(|| natural_cmp(&a.name, &b.name)));

    // Seasons, along the sequels from the first of them.
    let first_season = seasons.first().and_then(|p| p.season).unwrap_or(1);
    let from_root = graph.sequels_from(graph.entry(root).filter(|e| e.is_series()).map_or(base.id, |e| e.anime.id));
    let start = if only || seasons.len() <= 1 && others.is_empty() {
        base.id
    } else if first_season <= 1 {
        from_root.first().map_or(base.id, |e| e.anime.id)
    } else {
        // The season its first folder names: the entry whose title says so, else that many seasons along.
        let named = from_root.iter().find(|e| e.is_series() && e.anime.season() == first_season);
        let counted = from_root.iter().filter(|e| e.is_series()).nth(first_season as usize - 1);
        named.or(counted).map_or(base.id, |e| e.anime.id)
    };
    let chain: Vec<&Entry> = graph.sequels_from(start).into_iter().filter(|e| e.anime.format != "MOVIE" && released(e)).collect();
    let mut at = 0;
    let mut i = 0;
    while i < seasons.len() {
        let n = episodes(&seasons[i]);
        // Entries that can't be this folder's are passed over: a special between two seasons.
        let mut placed = false;
        while at < chain.len() && !placed {
            let e = chain[at];
            let takes = e.anime.episodes.map(|c| c as usize);
            match takes {
                None => {
                    // An entry still airing, or of a length AniList doesn't say, takes this folder and the season folders after it.
                    let mut whole = seasons[i].clone();
                    for more in &seasons[i + 1..] {
                        whole.files.extend(more.files.iter().cloned());
                    }
                    if seasons.len() - i > 1 {
                        let folder = Path::new(&whole.folder).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                        // In folder order, each folder after the one before.
                        let mut files: Vec<ScannedFile> = Vec::new();
                        let mut n = 0;
                        for p in &seasons[i..] {
                            for f in &p.files {
                                let mut f = f.clone();
                                if f.number.is_some() {
                                    n += 1;
                                    f.number = Some(n);
                                }
                                files.push(f);
                            }
                        }
                        files.sort_by_key(|f| f.number.unwrap_or(i64::MAX));
                        whole = Part { name: format!("{} to {}", seasons[i].name, seasons[seasons.len() - 1].name), folder, files, numbering: "order".into(), shared: true, ..whole };
                    }
                    whole.anime = Some(e.anime.clone());
                    out.push(whole);
                    i = seasons.len();
                    at += 1;
                    placed = true;
                }
                Some(c) if n == c => {
                    let mut p = seasons[i].clone();
                    p.anime = Some(e.anime.clone());
                    out.push(p);
                    i += 1;
                    at += 1;
                    placed = true;
                }
                Some(c) if n < c => {
                    // Several folders that add up to the entry share it (a series kept in short season folders); else it's a season with episodes missing.
                    let mut sum = n;
                    let mut k = i + 1;
                    while k < seasons.len() && sum < c {
                        sum += episodes(&seasons[k]);
                        k += 1;
                    }
                    if sum == c && k > i + 1 {
                        let folder = Path::new(&seasons[i].folder).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                        let mut files: Vec<ScannedFile> = Vec::new();
                        let mut number = 0;
                        for p in &seasons[i..k] {
                            for f in &p.files {
                                let mut f = f.clone();
                                if f.number.is_some() {
                                    number += 1;
                                    f.number = Some(number);
                                }
                                files.push(f);
                            }
                        }
                        files.sort_by_key(|f| f.number.unwrap_or(i64::MAX));
                        out.push(Part { name: format!("{} to {}", seasons[i].name, seasons[k - 1].name), folder, files, numbering: "order".into(), shared: true, anime: Some(e.anime.clone()), ..seasons[i].clone() });
                        i = k;
                    } else if e.is_series() {
                        let mut p = seasons[i].clone();
                        p.anime = Some(e.anime.clone());
                        out.push(p);
                        i += 1;
                    } else {
                        at += 1;
                        continue;
                    }
                    at += 1;
                    placed = true;
                }
                Some(c) => {
                    // More episodes than the entry has: when the entries after it make up the rest exactly, the folder is split between them.
                    let mut sum = c;
                    let mut k = at + 1;
                    while k < chain.len() && sum < n {
                        match chain[k].anime.episodes {
                            Some(more) => sum += more as usize,
                            None => break,
                        }
                        k += 1;
                    }
                    if sum == n && k > at + 1 {
                        out.extend(split_between(&seasons[i], &chain[at..k]));
                        i += 1;
                        at = k;
                        placed = true;
                    } else if e.is_series() {
                        let mut p = seasons[i].clone();
                        p.note = format!("{n} episodes here, and AniList’s entry has {c}.");
                        p.anime = Some(e.anime.clone());
                        out.push(p);
                        i += 1;
                        at += 1;
                        placed = true;
                    } else {
                        at += 1;
                    }
                }
            }
        }
        if !placed {
            // Past the last entry AniList has: nothing to place it on.
            let mut p = seasons[i].clone();
            p.title = scan::clean_title(&p.name);
            if p.title.is_empty() || p.title.chars().all(|c| !c.is_alphabetic()) {
                p.title = match p.season {
                    Some(s) => format!("{anime_title} Season {s}"),
                    None => anime_title.clone(),
                };
            }
            out.push(p);
            i += 1;
        }
    }

    // Movies: by the number in the name, else by title, else in the order they came out.
    let films: Vec<&Entry> = entries.iter().copied().filter(|e| e.anime.format == "MOVIE").collect();
    let (movies, others): (Vec<Part>, Vec<Part>) = others.into_iter().partition(|p| p.kind == Kind::Movie);
    let by_order = movies.len() == films.len();
    let single = movies.len() == 1 && films.len() == 1;
    for (index, mut p) in movies.into_iter().enumerate() {
        let name = stem(&p.files[0].path);
        let by_number = movie_number(&name).and_then(|n| films.get(n - 1).copied());
        let by_title = || {
            let fit = |e: &Entry| e.anime.names().map(|t| anilist::likeness(&scan::clean_title(&name), t)).fold(0.0, f64::max);
            let mut best: Vec<(f64, &Entry)> = films.iter().map(|e| (fit(e), *e)).filter(|(fit, _)| *fit >= 0.5).collect();
            best.sort_by(|a, b| b.0.total_cmp(&a.0));
            match best.as_slice() {
                [one] => Some(one.1),
                [first, second, ..] if first.0 > second.0 => Some(first.1),
                _ => None,
            }
        };
        let found = by_number.or_else(by_title).or_else(|| (by_order || single).then(|| films.get(index).copied()).flatten());
        match found.filter(|e| taken.insert(e.anime.id)) {
            Some(e) => p.anime = Some(e.anime.clone()),
            None => p.title = scan::clean_title(&name),
        }
        out.push(p);
    }

    // OVAs and specials: the entry of that kind with as many episodes.
    for mut p in others {
        let (format, word) = if p.kind == Kind::Ova { ("OVA", "OVA") } else { ("SPECIAL", "specials") };
        let n = episodes(&p);
        let of_kind: Vec<&Entry> = entries.iter().copied().filter(|e| e.anime.format == format && !taken.contains(&e.anime.id)).collect();
        let exact: Vec<&Entry> = of_kind.iter().copied().filter(|e| e.anime.episodes == Some(n as i64)).collect();
        let sum: i64 = of_kind.iter().map(|e| e.anime.episodes.unwrap_or(i64::MAX / 1024)).sum();
        if let [one] = exact.as_slice() {
            taken.insert(one.anime.id);
            p.anime = Some(one.anime.clone());
            out.push(p);
        } else if of_kind.len() > 1 && sum == n as i64 {
            // Every entry of the kind, one after another: three OVA series in one folder.
            taken.extend(of_kind.iter().map(|e| e.anime.id));
            out.extend(split_between(&p, &of_kind));
        } else if let ([one], true) = (of_kind.as_slice(), exact.is_empty()) {
            taken.insert(one.anime.id);
            if let Some(c) = one.anime.episodes.filter(|&c| c != n as i64) {
                p.note = format!("{n} here, and AniList’s entry has {c}.");
            }
            p.anime = Some(one.anime.clone());
            out.push(p);
        } else {
            p.title = format!("{anime_title} {word}");
            out.push(p);
        }
    }

    // In the order they came out, what has no entry last.
    let start_of = |p: &Part| p.anime.as_ref().and_then(|a| graph.entry(a.id)).map(|e| e.start).filter(|&d| d > 0).unwrap_or(i64::MAX);
    out.sort_by_key(|p| start_of(p));
    out
}

/// The parts with no entry found for any: AniList couldn't be asked, or doesn't know the anime.
pub fn unassigned(parts: Vec<Part>, guess: &str) -> Vec<Part> {
    parts
        .into_iter()
        .map(|mut p| {
            p.title = match p.kind {
                Kind::Movie => scan::clean_title(&stem(&p.files[0].path)),
                Kind::Ova => format!("{guess} OVA"),
                Kind::Specials => format!("{guess} specials"),
                Kind::Season => match p.season {
                    Some(s) => format!("{guess} Season {s}"),
                    None => guess.to_string(),
                },
            };
            p
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::series::tests::{entry, overlord};

    fn file(rel: &str, minutes: f64) -> ScannedFile {
        let name = rel.rsplit('/').next().unwrap();
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
        let extra = rel.contains("Extras/");
        ScannedFile {
            path: format!("D:/Anime/Root/{rel}"),
            rel: rel.into(),
            size: 1,
            duration: minutes * 60.0,
            parsed: scan::episode_number(stem).map(|(_, n)| n),
            number: None,
            reason: if extra { "Its name says it’s an extra".into() } else if minutes > 120.0 { "Over 2 hours (a play-all title)".into() } else { String::new() },
        }
    }

    fn scanned(files: Vec<ScannedFile>) -> Scan {
        Scan { folder: "D:/Anime/Root".into(), guess: "Root".into(), files, numbering: "order".into() }
    }

    fn season(dir: &str, tag: &str, count: usize) -> Vec<ScannedFile> {
        (1..=count).map(|n| file(&format!("{dir}/Show - {tag}E{n:02}.mkv"), 24.0)).collect()
    }

    /// Each part as its name, its entry's ID (0 for none) and its episodes' numbers.
    fn summary(parts: &[Part]) -> Vec<(String, i64, Vec<i64>)> {
        parts.iter().map(|p| (p.name.clone(), p.anime.as_ref().map_or(0, |a| a.id), p.files.iter().filter_map(|f| f.number).collect())).collect()
    }

    #[test]
    fn a_folder_of_seasons_movies_and_an_ova_is_a_part_for_each() {
        let mut files = Vec::new();
        for s in 1..=3 {
            files.extend(season(&format!("[Judas] Overlord - S{s}"), &format!("S0{s}"), 13));
            files.push(file(&format!("[Judas] Overlord - S{s}/S{s} Extras/[Judas] Overlord - S0{s} NCOP.mkv"), 1.5));
        }
        files.push(file("[Judas] Overlord - Movies/[Judas] Overlord - Movie 1.mkv", 121.0));
        files.push(file("[Judas] Overlord - Movies/[Judas] Overlord - Movie 2.mkv", 93.0));
        files.push(file("[Judas] Overlord - OVA.mkv", 30.0));
        let parts = split(&scanned(files));
        assert_eq!(parts.len(), 6);
        let graph = Graph::new(overlord());
        let base = graph.entry(20832).unwrap().anime.clone();
        let parts = assign(parts, &graph, &base);
        let all: Vec<i64> = (1..=13).collect();
        assert_eq!(
            summary(&parts),
            [
                ("[Judas] Overlord - S1".to_string(), 20832, all.clone()),
                ("[Judas] Overlord - OVA.mkv".to_string(), 87489, vec![1]),
                // A movie may run past two hours, and "Movie 2" is the second of the anime's films.
                ("[Judas] Overlord - Movie 1.mkv".to_string(), 98873, vec![1]),
                ("[Judas] Overlord - Movie 2.mkv".to_string(), 98874, vec![1]),
                ("[Judas] Overlord - S2".to_string(), 98437, all.clone()),
                ("[Judas] Overlord - S3".to_string(), 101474, all),
            ]
        );
        // A season's folder is its own; a movie's and the loose OVA's hold other parts' rips too.
        assert_eq!(parts.iter().map(|p| p.shared).collect::<Vec<_>>(), [false, true, true, true, false, false]);
        // What was left out stays with its season.
        assert_eq!(parts[0].files.len(), 14);
    }

    #[test]
    fn a_folder_longer_than_its_entry_is_split_at_anilists_seasons() {
        // Attack on Titan as AniList has it: its third season is two entries, and its end two specials.
        let graph = Graph::new(vec![
            entry(1, "Shingeki no Kyojin", "TV", 25, 20130407, &[("SEQUEL", 2), ("SIDE_STORY", 10), ("SIDE_STORY", 11), ("SIDE_STORY", 12)]),
            entry(2, "Shingeki no Kyojin Season 2", "TV", 12, 20170401, &[("PREQUEL", 1), ("SEQUEL", 3)]),
            entry(3, "Shingeki no Kyojin Season 3", "TV", 12, 20180723, &[("PREQUEL", 2), ("SEQUEL", 4)]),
            entry(4, "Shingeki no Kyojin Season 3 Part 2", "TV", 10, 20190429, &[("PREQUEL", 3), ("SEQUEL", 5)]),
            entry(5, "Shingeki no Kyojin: The Final Season", "TV", 16, 20201207, &[("PREQUEL", 4), ("SEQUEL", 6)]),
            entry(6, "Shingeki no Kyojin: The Final Season Part 2", "TV", 12, 20220110, &[("PREQUEL", 5), ("SEQUEL", 7)]),
            entry(7, "Kanketsu-hen Zenpen", "SPECIAL", 1, 20230304, &[("PREQUEL", 6), ("SEQUEL", 8)]),
            entry(8, "Kanketsu-hen Kouhen", "SPECIAL", 1, 20231105, &[("PREQUEL", 7)]),
            entry(10, "Shingeki no Kyojin OVA", "OVA", 3, 20131209, &[("PARENT", 1)]),
            entry(11, "Kuinaki Sentaku", "OVA", 2, 20141209, &[("PARENT", 1)]),
            entry(12, "Lost Girls", "OVA", 3, 20171208, &[("PARENT", 1)]),
        ]);
        let mut files = Vec::new();
        for (dir, count) in [("AoT (Season 01)", 25), ("AoT (Season 02)", 12), ("AoT (Season 03)", 22), ("AoT (Season 4 Part 1)", 16), ("AoT (Season 4 Part 2)", 12)] {
            files.extend((1..=count).map(|n| file(&format!("{dir}/Attack on Titan - {n:02}.mkv"), 24.0)));
        }
        files.push(file("AoT (Season 4 Part 3)/Attack On Titan - 88-91.mkv", 61.0));
        files.push(file("AoT (Season 4 Part 3)/Attack On Titan - 92-94.mkv", 85.0));
        files.extend((1..=8).map(|n| file(&format!("AoT OAD-OVA/Attack On Titan OAD - {n:02}.mkv"), 24.0)));
        let base = graph.entry(1).unwrap().anime.clone();
        let parts = assign(split(&scanned(files)), &graph, &base);
        let got: Vec<(i64, usize)> = parts.iter().map(|p| (p.anime.as_ref().map_or(0, |a| a.id), episodes(p))).collect();
        assert_eq!(got, [(1, 25), (10, 3), (11, 2), (2, 12), (12, 3), (3, 12), (4, 10), (5, 16), (6, 12), (7, 1), (8, 1)]);
        // The second half is numbered from 1, as its entry numbers it.
        let part_2 = parts.iter().find(|p| p.anime.as_ref().is_some_and(|a| a.id == 4)).unwrap();
        assert_eq!(part_2.files.iter().filter_map(|f| f.number).collect::<Vec<_>>(), (1..=10).collect::<Vec<_>>());
        assert!(part_2.files[0].rel.ends_with("- 13.mkv") && part_2.shared);
    }

    #[test]
    fn folders_that_carry_on_from_each_other_are_one_part() {
        // Discs of one season, and a long series kept in season folders, numbered straight through.
        let discs = scanned(vec![file("Disc 1/title_t00.mkv", 24.0), file("Disc 1/title_t01.mkv", 24.0), file("Disc 2/title_t00.mkv", 24.0)]);
        assert_eq!(split(&discs).len(), 1);
        let mut files: Vec<ScannedFile> = (1..=8).map(|n| file(&format!("Season 1/One Piece - {n:03}.mkv"), 24.0)).collect();
        files.extend((9..=30).map(|n| file(&format!("Season 2/One Piece - {n:03}.mkv"), 24.0)));
        let parts = split(&scanned(files));
        assert_eq!((parts.len(), episodes(&parts[0]), parts[0].numbering.as_str(), parts[0].shared), (1, 30, "names", false));
    }

    #[test]
    fn season_folders_of_a_series_still_airing_share_its_one_entry() {
        // Numbered from 1 in each folder, as Sonarr names them; AniList has the series as one entry of no set length.
        let mut one_piece = entry(21, "ONE PIECE", "TV", 0, 19991020, &[]);
        one_piece.anime.episodes = None;
        let graph = Graph::new(vec![one_piece]);
        let mut files = season("Season 1", "S01", 8);
        files.extend(season("Season 2", "S02", 22));
        files.extend(season("Season 00", "S00", 3));
        let base = graph.entry(21).unwrap().anime.clone();
        let parts = assign(split(&scanned(files)), &graph, &base);
        assert_eq!(parts.iter().map(|p| (p.anime.as_ref().map_or(0, |a| a.id), episodes(p), p.title.as_str())).collect::<Vec<_>>(), [(21, 30, ""), (0, 3, "ONE PIECE specials")]);
        assert_eq!(parts[0].files.iter().filter_map(|f| f.number).collect::<Vec<_>>(), (1..=30).collect::<Vec<_>>());
        assert_eq!(parts[0].folder, "D:/Anime/Root");
    }

    #[test]
    fn a_movie_is_placed_by_its_title_and_a_stray_one_is_left_without_an_entry() {
        let graph = Graph::new(vec![
            entry(1, "Kono Subarashii Sekai ni Shukufuku wo!", "TV", 10, 20160114, &[("SEQUEL", 2)]),
            entry(2, "Kono Subarashii Sekai ni Shukufuku wo! 2", "TV", 10, 20170112, &[("PREQUEL", 1), ("SEQUEL", 3)]),
            entry(3, "KONOSUBA Legend of Crimson", "MOVIE", 1, 20190830, &[("PREQUEL", 2)]),
            entry(4, "KONOSUBA An Explosion on This Wonderful World", "TV", 12, 20230406, &[("PARENT", 1)]),
        ]);
        let mut files = season("Season 1", "S01", 10);
        files.extend(season("Season 2", "S02", 10));
        // A spin-off's folder, named for it, with its own episodes numbered from S01E01.
        files.extend(season("KonoSuba - An Explosion On This Wonderful World", "S01", 12));
        files.push(file("KonoSuba - S00E05 - Movie Legend of Crimson.mkv", 90.0));
        files.push(file("Films/Something Else Entirely.mkv", 100.0));
        files.push(file("Films/Another Film.mkv", 100.0));
        let base = graph.entry(1).unwrap().anime.clone();
        let parts = assign(split(&scanned(files)), &graph, &base);
        let got: Vec<(i64, &str)> = parts.iter().map(|p| (p.anime.as_ref().map_or(0, |a| a.id), p.title.as_str())).collect();
        assert_eq!(got, [(1, ""), (2, ""), (3, ""), (4, ""), (0, "Something Else Entirely"), (0, "Another Film")]);
        assert_eq!(parts.iter().map(episodes).collect::<Vec<_>>(), [10, 10, 1, 12, 1, 1]);
    }

    #[test]
    fn a_film_lying_loose_beside_the_season_folders_is_a_movie_not_a_season() {
        // Jujutsu Kaisen: the film is the first season's prequel, and its file is named for it alone.
        let graph = Graph::new(vec![
            entry(1, "Jujutsu Kaisen", "TV", 24, 20201003, &[("PREQUEL", 4), ("SEQUEL", 2)]),
            entry(2, "Jujutsu Kaisen 2nd Season", "TV", 23, 20230706, &[("PREQUEL", 1), ("SEQUEL", 3)]),
            entry(3, "Jujutsu Kaisen: Shimetsu Kaiyuu", "TV", 12, 20260108, &[("PREQUEL", 2)]),
            entry(4, "Jujutsu Kaisen 0", "MOVIE", 1, 20211224, &[("SEQUEL", 1)]),
        ]);
        let mut files = season("Season 1", "S01", 24);
        files.extend(season("Season 2", "S02", 23));
        files.extend(season("Season 3", "S03", 12));
        files.push(file("Jujutsu Kaisen 0.mkv", 105.0));
        let base = graph.entry(1).unwrap().anime.clone();
        let parts = assign(split(&scanned(files)), &graph, &base);
        assert_eq!(
            summary(&parts),
            [
                ("Season 1".to_string(), 1, (1..=24).collect::<Vec<i64>>()),
                // Listed where it came out, between the first two seasons.
                ("Jujutsu Kaisen 0.mkv".to_string(), 4, vec![1]),
                ("Season 2".to_string(), 2, (1..=23).collect()),
                ("Season 3".to_string(), 3, (1..=12).collect()),
            ]
        );
        assert_eq!(parts[1].kind, Kind::Movie);
        // Without season folders, long files lying together are one show's episodes, as before.
        assert_eq!(split(&scanned(vec![file("Part 1.mkv", 100.0), file("Part 2.mkv", 100.0)])).len(), 1);
    }

    #[test]
    fn one_season_folder_chosen_by_itself_is_split_too() {
        let graph = Graph::new(vec![
            entry(3, "Shingeki no Kyojin Season 3", "TV", 12, 20180723, &[("SEQUEL", 4)]),
            entry(4, "Shingeki no Kyojin Season 3 Part 2", "TV", 10, 20190429, &[("PREQUEL", 3)]),
        ]);
        let files: Vec<ScannedFile> = (1..=22).map(|n| file(&format!("Attack on Titan S3 - {n:02}.mkv"), 24.0)).collect();
        let base = graph.entry(3).unwrap().anime.clone();
        let parts = assign(split(&scanned(files)), &graph, &base);
        assert_eq!(parts.iter().map(|p| (p.anime.as_ref().map_or(0, |a| a.id), episodes(p))).collect::<Vec<_>>(), [(3, 12), (4, 10)]);
        // A folder that fits its entry stays whole, and one over it that the next entry doesn't finish is kept whole with a word of it.
        let k_on = Graph::new(vec![entry(5, "K-ON!", "TV", 13, 20090403, &[("SEQUEL", 6)]), entry(6, "K-ON!!", "TV", 26, 20100407, &[("PREQUEL", 5)])]);
        let files: Vec<ScannedFile> = (1..=14).map(|n| file(&format!("K-On! - S01E{n:02}.mkv"), 24.0)).collect();
        let parts = assign(split(&scanned(files)), &k_on, &k_on.entry(5).unwrap().anime.clone());
        assert_eq!((parts.len(), parts[0].anime.as_ref().map(|a| a.id), parts[0].note.as_str()), (1, Some(5), "14 episodes here, and AniList’s entry has 13."));
    }
}
