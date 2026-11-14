//! Which anime a show belongs to. AniList keeps each season, movie and side
//! story as an entry of its own and says how they're related (sequel, parent,
//! side story…). Telly follows those links out from the shows in your
//! library, through entries you don't have (your ten Pokémon movies meet at TV
//! series you don't own), and keeps what it finds in series.json. The Library
//! lists an anime once, with its seasons and movies inside in the order they
//! came out; the next-episode countdown and Previously on follow the sequel
//! and prequel links; Recommended offers the parts you don't have.
//!
//! AniList is asked at start-up and when a show is added, about entries it
//! hasn't been asked about, and about all of them again after a week. Pages
//! only read what's kept.
//!
//! Settings: series.of.<show> (the anime you moved a show to: an anime's
//! key, or 0 for on its own).

use crate::anilist::{self, Anime};
use crate::backend::Backend;
use crate::db::Show;
use crate::util::now_ms;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// AniList is asked about every entry again after a week.
const KEPT_MS: i64 = 7 * 24 * 3600 * 1000;
/// Relations that make two entries the same anime: its seasons, films and side stories.
pub const SAME_SERIES: &[&str] = &["PREQUEL", "SEQUEL", "PARENT", "SIDE_STORY", "SUMMARY", "ALTERNATIVE", "SPIN_OFF", "COMPILATION", "CONTAINS"];

/// One AniList entry and its links to the rest of its anime.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(flatten)]
    pub anime: Anime,
    /// The day it started, as 20150707 (0 when AniList doesn't say).
    pub start: i64,
    /// FINISHED, RELEASING, NOT_YET_RELEASED…
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// Each related anime entry: how it's related (SEQUEL, PARENT…) and its ID.
    pub links: Vec<(String, i64)>,
    pub fetched_at: i64,
}

impl Entry {
    /// A season: what a season folder holds.
    pub fn is_series(&self) -> bool {
        matches!(self.anime.format.as_str(), "TV" | "TV_SHORT" | "ONA")
    }

    /// The entries it's linked to by `relation`.
    fn linked<'a>(&'a self, relation: &'a str) -> impl Iterator<Item = i64> + 'a {
        self.links.iter().filter(move |(r, _)| r == relation).map(|(_, id)| *id)
    }
}

/// The entries Telly knows, and which anime each belongs to.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    entries: HashMap<i64, Entry>,
    /// Each entry's anime, named by the entry it's called after (its first season).
    roots: HashMap<i64, i64>,
}

impl Graph {
    pub fn new(entries: impl IntoIterator<Item = Entry>) -> Graph {
        let entries: HashMap<i64, Entry> = entries.into_iter().map(|e| (e.anime.id, e)).collect();
        // Entries linked either way are one anime.
        let mut group: HashMap<i64, i64> = entries.keys().map(|&id| (id, id)).collect();
        fn top(group: &mut HashMap<i64, i64>, id: i64) -> i64 {
            let mut at = id;
            while let Some(&up) = group.get(&at).filter(|up| **up != at) {
                at = up;
            }
            group.insert(id, at);
            at
        }
        for e in entries.values() {
            for (relation, to) in &e.links {
                if SAME_SERIES.contains(&relation.as_str()) && entries.contains_key(to) {
                    let (a, b) = (top(&mut group, e.anime.id), top(&mut group, *to));
                    group.insert(b, a);
                }
            }
        }
        let mut members: HashMap<i64, Vec<i64>> = HashMap::new();
        for &id in entries.keys() {
            members.entry(top(&mut group, id)).or_default().push(id);
        }
        // An anime is called after its first TV series, else its first series of any kind, else its first entry.
        let mut roots = HashMap::new();
        for ids in members.into_values() {
            let rank = |id: &i64| {
                let e = &entries[id];
                let kind = match e.anime.format.as_str() {
                    "TV" => 0,
                    "TV_SHORT" | "ONA" => 1,
                    _ => 2,
                };
                (kind, if e.start > 0 { e.start } else { i64::MAX }, *id)
            };
            let root = *ids.iter().min_by_key(|id| rank(id)).expect("a group has an entry");
            for id in ids {
                roots.insert(id, root);
            }
        }
        Graph { entries, roots }
    }

    pub fn entry(&self, id: i64) -> Option<&Entry> {
        self.entries.get(&id)
    }

    /// The anime `id` belongs to, named by its first season's ID.
    pub fn root(&self, id: i64) -> Option<i64> {
        self.roots.get(&id).copied()
    }

    /// Every entry and the anime it belongs to.
    pub fn roots(&self) -> impl Iterator<Item = (i64, i64)> + '_ {
        self.roots.iter().map(|(id, root)| (*id, *root))
    }

    /// The entries of the anime called after `root`, in the order they came out.
    pub fn parts(&self, root: i64) -> Vec<&Entry> {
        let mut parts: Vec<&Entry> = self.entries.values().filter(|e| self.roots.get(&e.anime.id) == Some(&root)).collect();
        parts.sort_by_key(|e| (if e.start > 0 { e.start } else { i64::MAX }, e.anime.id));
        parts
    }

    /// The entry that carries on from `id`: a show can go on in more than one
    /// way (a series and a special), so the series, then the earliest.
    pub fn sequel(&self, id: i64) -> Option<&Entry> {
        let entry = self.entries.get(&id)?;
        entry.linked("SEQUEL").filter_map(|to| self.entries.get(&to)).min_by_key(|e| (!e.is_series(), if e.start > 0 { e.start } else { i64::MAX }, e.anime.id))
    }

    /// `id` and what carries on from it, following the sequels: seasons, and the films and specials between them.
    pub fn sequels_from(&self, id: i64) -> Vec<&Entry> {
        let mut seen = HashSet::from([id]);
        let mut out = Vec::new();
        let mut at = self.entries.get(&id);
        while let Some(e) = at {
            out.push(e);
            at = self.sequel(e.anime.id).filter(|next| seen.insert(next.anime.id));
        }
        out
    }

    /// The seasons from `id` on, following the sequels: `id` itself first.
    /// Films and specials on the way are passed through and left out.
    pub fn seasons_from(&self, id: i64) -> Vec<&Entry> {
        self.sequels_from(id).into_iter().filter(|e| e.is_series() || e.anime.id == id).collect()
    }

    /// The season `id` carries on from: its prequel that's a series, the latest of them.
    pub fn prequel(&self, id: i64) -> Option<&Entry> {
        let entry = self.entries.get(&id)?;
        entry.linked("PREQUEL").filter_map(|to| self.entries.get(&to)).filter(|e| e.is_series()).max_by_key(|e| (e.start, e.anime.id))
    }
}

/// Where a show sits in the Library: the anime it's listed under and its place among that anime's parts.
#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    /// The anime's key: its first season's AniList ID, or, for a show on its own that AniList doesn't know, minus the show's ID.
    pub id: i64,
    /// The anime's name, for its card.
    pub title: String,
    pub native: String,
    /// Where the show comes among the anime's parts: the day it started, as 20150707.
    pub order: i64,
    /// How many shows in the library the anime has.
    pub parts: usize,
}

fn override_key(show: i64) -> String {
    format!("series.of.{show}")
}

/// Puts a show under another anime (`key`: an anime's key), or on its own (0), or back where AniList has it (None).
pub fn move_show(backend: &Backend, show: i64, key: Option<i64>) -> Result<()> {
    backend.db.set_setting(&override_key(show), &key.map_or(Value::Null, |k| json!(k)))
}

/// Where each show (by its ID) sits in the Library.
pub fn places(graph: &Graph, shows: &[Show], settings: &Map<String, Value>) -> HashMap<i64, Place> {
    let alone = |s: &Show| -s.id;
    // Each show's anime, as you moved it or as AniList has it.
    let mut key: HashMap<i64, i64> = HashMap::new();
    for s in shows {
        let moved = settings.get(&override_key(s.id)).and_then(Value::as_i64);
        let k = match moved {
            Some(0) => alone(s),
            Some(k) => k,
            None => s.anilist_id.and_then(|id| graph.root(id)).unwrap_or_else(|| alone(s)),
        };
        key.insert(s.id, k);
    }
    // A show AniList doesn't know (a folder of specials) joins the anime whose folders its own sits beside.
    for s in shows.iter().filter(|s| s.anilist_id.is_none() && !settings.contains_key(&override_key(s.id))) {
        let beside = Path::new(&s.folder).parent();
        let mut found: HashSet<i64> = HashSet::new();
        for other in shows.iter().filter(|o| o.anilist_id.is_some() && o.id != s.id) {
            if beside.is_some() && Path::new(&other.folder).parent() == beside {
                found.insert(key[&other.id]);
            }
        }
        if found.len() == 1 {
            key.insert(s.id, found.into_iter().next().expect("one anime"));
        }
    }
    let mut count: HashMap<i64, usize> = HashMap::new();
    for k in key.values() {
        *count.entry(*k).or_default() += 1;
    }
    // An anime AniList doesn't know is called after the show its key names.
    let by_id: HashMap<i64, &Show> = shows.iter().map(|s| (s.id, s)).collect();
    shows
        .iter()
        .map(|s| {
            let k = key[&s.id];
            let (title, native) = match graph.entry(k) {
                Some(e) if count[&k] > 1 => (e.anime.short_title(), e.anime.native.clone()),
                _ => {
                    let named = if count[&k] > 1 { by_id.get(&-k).copied().unwrap_or(s) } else { s };
                    (named.title.clone(), named.title_native.clone())
                }
            };
            let order = s.anilist_id.and_then(|id| graph.entry(id)).map(|e| e.start).filter(|&d| d > 0).or(s.year.map(|y| y * 10000)).unwrap_or(i64::MAX);
            (s.id, Place { id: k, title, native, order, parts: count[&k] })
        })
        .collect()
}

/// The library's shows listed under the anime `key`, in the order they came out.
pub fn shows_of(backend: &Backend, key: i64) -> Result<Vec<Show>> {
    let shows = backend.db.shows()?;
    let places = places(&graph(backend), &shows, &backend.db.settings()?);
    let mut parts: Vec<Show> = shows.into_iter().filter(|s| places.get(&s.id).is_some_and(|p| p.id == key)).collect();
    parts.sort_by_key(|s| (places[&s.id].order, s.id));
    Ok(parts)
}

fn query() -> String {
    format!(
        "query ($ids: [Int], $page: Int) {{ Page(page: $page, perPage: 50) {{ pageInfo {{ hasNextPage }}
  media(id_in: $ids, type: ANIME) {{ {} startDate {{ year month day }} genres description(asHtml: false)
    relations {{ edges {{ relationType(version: 2) node {{ id type }} }} }} }} }} }}",
        anilist::FIELDS
    )
}

fn entry_from(media: &Value, now: i64) -> Option<Entry> {
    let date = |part: &str| media.pointer(&format!("/startDate/{part}")).and_then(Value::as_i64).unwrap_or(0);
    let links = media
        .pointer("/relations/edges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e.pointer("/node/type").and_then(Value::as_str) == Some("ANIME"))
        .filter_map(|e| Some((e.get("relationType")?.as_str()?.to_string(), e.pointer("/node/id")?.as_i64()?)))
        .collect();
    Some(Entry {
        anime: Anime::from_api(media)?,
        start: if date("year") > 0 { date("year") * 10000 + date("month") * 100 + date("day") } else { 0 },
        status: media.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
        genres: media.get("genres").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),
        description: media.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
        links,
        fetched_at: now,
    })
}

fn fetch(ids: &[i64]) -> Result<Vec<Entry>> {
    let now = now_ms();
    let mut found = Vec::new();
    for ids in ids.chunks(50) {
        for page in 1.. {
            let answer = anilist::query_patiently(&query(), json!({ "ids": ids, "page": page }))?;
            found.extend(answer.pointer("/data/Page/media").and_then(Value::as_array).into_iter().flatten().filter_map(|m| entry_from(m, now)));
            if answer.pointer("/data/Page/pageInfo/hasNextPage").and_then(Value::as_bool) != Some(true) {
                break;
            }
        }
    }
    Ok(found)
}

/// Adds to `known` the entries `from` belong with: each entry's related
/// entries are asked about in turn until none is new. An ID AniList no longer
/// has is kept empty, so it isn't asked about again.
fn walk(known: &mut HashMap<i64, Entry>, from: &[i64], fetch: &dyn Fn(&[i64]) -> Result<Vec<Entry>>) -> Result<()> {
    let mut wanted: Vec<i64> = from.iter().copied().filter(|id| !known.contains_key(id)).collect();
    while !wanted.is_empty() {
        wanted.sort_unstable();
        wanted.dedup();
        let found = fetch(&wanted)?;
        let now = found.first().map_or_else(now_ms, |e| e.fetched_at);
        for id in &wanted {
            known.entry(*id).or_insert_with(|| Entry { anime: Anime { id: *id, ..Default::default() }, fetched_at: now, ..Default::default() });
        }
        let mut next = Vec::new();
        for e in found {
            next.extend(e.links.iter().filter(|(relation, to)| SAME_SERIES.contains(&relation.as_str()) && !known.contains_key(to)).map(|(_, to)| *to));
            known.insert(e.anime.id, e);
        }
        wanted = next;
    }
    Ok(())
}

/// The kept entries, read once and replaced when AniList has been asked again.
#[derive(Default)]
pub struct Kept(Mutex<Option<Arc<Graph>>>);

fn kept_path(backend: &Backend) -> PathBuf {
    backend.dirs.local.join("series.json")
}

/// What Telly knows of AniList's entries and their anime.
pub fn graph(backend: &Backend) -> Arc<Graph> {
    let mut kept = backend.series.0.lock().unwrap_or_else(|e| e.into_inner());
    kept.get_or_insert_with(|| {
        let entries: Vec<Entry> = std::fs::read(kept_path(backend)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Arc::new(Graph::new(entries))
    })
    .clone()
}

/// Asks AniList about the library's shows it hasn't been asked about, and
/// about every entry again once the oldest answer is a week old.
pub fn refresh(backend: &Backend) -> Result<()> {
    // One asking at a time.
    static ASKING: Mutex<()> = Mutex::new(());
    let _asking = ASKING.lock();
    let ids: Vec<i64> = backend.db.shows()?.iter().filter_map(|s| s.anilist_id).collect();
    let now = now_ms();
    let have = graph(backend);
    let stale = ids.iter().filter_map(|id| have.entry(*id)).any(|e| now - e.fetched_at > KEPT_MS);
    if !stale && ids.iter().all(|id| have.entry(*id).is_some()) {
        return Ok(());
    }
    // Asked again, the walk starts from nothing: an entry's links can have changed.
    let mut known = if stale { HashMap::new() } else { have.entries.clone() };
    walk(&mut known, &ids, &fetch)?;
    let entries: Vec<&Entry> = known.values().collect();
    crate::util::write_whole(&kept_path(backend), serde_json::to_vec(&entries)?)?;
    *backend.series.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Graph::new(known.into_values())));
    Ok(())
}

/// `refresh`, in the background: at start-up and when a show is added.
pub fn gather(backend: &Arc<Backend>) {
    let backend = backend.clone();
    std::thread::spawn(move || {
        let _ = refresh(&backend);
    });
}

/// The entries around `id`, for matching a folder's parts before any of it
/// is in the library: what's kept, and AniList's answers for the rest.
pub fn around(backend: &Backend, id: i64) -> Result<Arc<Graph>> {
    let have = graph(backend);
    if have.entry(id).is_some() {
        return Ok(have);
    }
    let mut known = have.entries.clone();
    walk(&mut known, &[id], &fetch)?;
    // Kept, so the Library lists the folder's shows together the moment they're added.
    let entries: Vec<&Entry> = known.values().collect();
    crate::util::write_whole(&kept_path(backend), serde_json::to_vec(&entries)?)?;
    let graph = Arc::new(Graph::new(known.into_values()));
    *backend.series.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(graph.clone());
    Ok(graph)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn entry(id: i64, title: &str, format: &str, episodes: i64, start: i64, links: &[(&str, i64)]) -> Entry {
        Entry {
            anime: Anime { id, romaji: title.into(), english: title.into(), format: format.into(), episodes: Some(episodes), year: Some(start / 10000), ..Default::default() },
            start,
            status: "FINISHED".into(),
            links: links.iter().map(|(r, to)| (r.to_string(), *to)).collect(),
            ..Default::default()
        }
    }

    /// Overlord as AniList had it on 14 November 2026, without the Ple Ple Pleiades shorts after the first.
    pub(crate) fn overlord() -> Vec<Entry> {
        vec![
            entry(20832, "Overlord", "TV", 13, 20150707, &[("SIDE_STORY", 87489), ("SEQUEL", 98437), ("SUMMARY", 98873), ("SUMMARY", 98874), ("SIDE_STORY", 21305), ("CHARACTER", 104454)]),
            entry(21305, "Overlord: Ple Ple Pleiades", "SPECIAL", 8, 20150925, &[("PARENT", 20832)]),
            entry(87489, "Overlord: Ple Ple Pleiades - Nazarick Saidai no Kiki", "OVA", 1, 20160930, &[("PARENT", 20832)]),
            entry(98873, "Overlord: The Undead King", "MOVIE", 1, 20170225, &[("PARENT", 20832), ("SEQUEL", 98874)]),
            entry(98874, "Overlord: The Dark Hero", "MOVIE", 1, 20170311, &[("PARENT", 20832), ("PREQUEL", 98873), ("SEQUEL", 98437)]),
            entry(98437, "Overlord II", "TV", 13, 20180109, &[("PREQUEL", 20832), ("SEQUEL", 101474), ("PREQUEL", 98874)]),
            entry(101474, "Overlord III", "TV", 13, 20180710, &[("PREQUEL", 98437), ("SEQUEL", 133844)]),
            entry(133844, "Overlord IV", "TV", 13, 20220705, &[("PREQUEL", 101474), ("SEQUEL", 133845)]),
            entry(133845, "Overlord: The Sacred Kingdom", "MOVIE", 1, 20240920, &[("PREQUEL", 133844)]),
            entry(104454, "Isekai Quartet", "TV_SHORT", 12, 20190410, &[("CHARACTER", 20832)]),
        ]
    }

    fn show(id: i64, title: &str, anilist: Option<i64>, folder: &str) -> Show {
        Show {
            id,
            folder: folder.into(),
            title: title.into(),
            title_native: String::new(),
            title_english: String::new(),
            title_romaji: String::new(),
            anilist_id: anilist,
            anilist_episodes: None,
            format: String::new(),
            year: None,
            cover_url: String::new(),
            cover_color: String::new(),
            jimaku_id: None,
            added_at: 0,
            last_watched_at: None,
        }
    }

    #[test]
    fn an_anime_is_its_seasons_movies_and_side_stories_called_after_the_first_season() {
        let graph = Graph::new(overlord());
        for id in [20832, 21305, 87489, 98873, 98874, 98437, 101474, 133844, 133845] {
            assert_eq!(graph.root(id), Some(20832), "{id}");
        }
        // Another show its characters turn up in isn't the same anime.
        assert_eq!(graph.root(104454), Some(104454));
        let order: Vec<i64> = graph.parts(20832).iter().map(|e| e.anime.id).collect();
        assert_eq!(order, [20832, 21305, 87489, 98873, 98874, 98437, 101474, 133844, 133845]);
        // The seasons run on past the recap movies, and a season's sequel is the next season.
        assert_eq!(graph.seasons_from(20832).iter().map(|e| e.anime.id).collect::<Vec<_>>(), [20832, 98437, 101474, 133844]);
        assert_eq!(graph.sequel(20832).map(|e| e.anime.id), Some(98437));
        assert_eq!(graph.prequel(98437).map(|e| e.anime.id), Some(20832));
        assert_eq!(graph.prequel(20832).map(|e| e.anime.id), None);
    }

    #[test]
    fn shows_are_listed_under_their_anime_in_the_order_they_came_out() {
        let graph = Graph::new(overlord());
        let shows = [
            show(1, "Overlord III", Some(101474), r"D:\Anime\Overlord\S3"),
            show(2, "Overlord", Some(20832), r"D:\Anime\Overlord\S1"),
            show(3, "The Undead King", Some(98873), r"D:\Anime\Overlord\Movies"),
            show(4, "Overlord specials", None, r"D:\Anime\Overlord\Specials"),
            show(5, "Shin Godzilla", None, r"D:\Movies\Shin Godzilla"),
            show(6, "Bocchi the Rock!", Some(130003), r"D:\Anime\Bocchi the Rock!"),
        ];
        let places = places(&graph, &shows, &Map::new());
        for id in [1, 2, 3, 4] {
            assert_eq!((places[&id].id, places[&id].title.as_str(), places[&id].parts), (20832, "Overlord", 4), "show {id}");
        }
        assert!(places[&2].order < places[&3].order && places[&3].order < places[&1].order && places[&1].order < places[&4].order);
        // A show AniList doesn't know, and one Telly hasn't asked about yet, are on their own.
        assert_eq!((places[&5].id, places[&5].title.as_str(), places[&5].parts), (-5, "Shin Godzilla", 1));
        assert_eq!((places[&6].id, places[&6].title.as_str(), places[&6].parts), (-6, "Bocchi the Rock!", 1));
    }

    #[test]
    fn a_show_you_moved_stays_where_you_put_it() {
        let graph = Graph::new(overlord());
        let shows = [show(1, "Overlord", Some(20832), "a"), show(2, "Overlord II", Some(98437), "b"), show(3, "Shin Godzilla", None, "c"), show(4, "Godzilla Minus One", None, "d")];
        let mut settings = Map::new();
        settings.insert("series.of.2".into(), json!(0));
        settings.insert("series.of.4".into(), json!(-3));
        let places = places(&graph, &shows, &settings);
        assert_eq!((places[&1].parts, places[&2].id, places[&2].parts), (1, -2, 1));
        // Two live-action films put together are called after the one the other was moved to.
        assert_eq!((places[&3].id, places[&4].id, places[&4].title.as_str(), places[&4].parts), (-3, -3, "Shin Godzilla", 2));
    }

    #[test]
    fn the_walk_follows_links_through_entries_you_don_t_have() {
        // Two Pokémon movies, each tied to a TV series that isn't in the library; the series are each other's sequel.
        let all = vec![
            entry(528, "Mewtwo Strikes Back", "MOVIE", 1, 19980718, &[("PARENT", 527)]),
            entry(527, "Pokémon", "TV", 276, 19970401, &[("SIDE_STORY", 528), ("SEQUEL", 1564)]),
            entry(1564, "Pokémon Advanced", "TV", 192, 20021121, &[("PREQUEL", 527), ("SIDE_STORY", 1526), ("OTHER", 9)]),
            entry(1526, "Lucario and the Mystery of Mew", "MOVIE", 1, 20050716, &[("PARENT", 1564)]),
            entry(9, "Something else", "TV", 12, 20000101, &[]),
        ];
        let asked = std::cell::RefCell::new(Vec::new());
        let fetch = |ids: &[i64]| -> Result<Vec<Entry>> {
            asked.borrow_mut().push(ids.to_vec());
            Ok(all.iter().filter(|e| ids.contains(&e.anime.id)).cloned().collect())
        };
        let mut known = HashMap::new();
        walk(&mut known, &[528, 1526, 404], &fetch).unwrap();
        assert_eq!(*asked.borrow(), [vec![404, 528, 1526], vec![527, 1564]]);
        let graph = Graph::new(known.into_values());
        assert_eq!((graph.root(528), graph.root(1526)), (Some(527), Some(527)));
        assert_eq!(graph.entry(527).map(|e| e.anime.short_title()), Some("Pokémon".to_string()));
        // An entry AniList doesn't have is kept, on its own, and one only loosely related isn't asked about.
        assert_eq!(graph.root(404), Some(404));
        assert!(graph.entry(9).is_none());
    }
}
