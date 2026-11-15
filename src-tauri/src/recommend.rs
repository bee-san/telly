//! Recommended: anime to watch next, from AniList's community recommendations
//! for the shows in the library and, once you've signed in to AniList, those
//! on your list there ("liked X, try Y", voted up or down). Telly asks about
//! each show by its AniList ID, weighs each show's votes by how much you liked
//! it (your score and status on your list, or else how much of it you've
//! watched, and less the longer ago that was) and adds them up, and leaves
//! out what you have or have started.
//! The seasons and movies you don't have of an anime you do are picks too,
//! by how much you liked the parts you have (series.rs has which they are).
//!
//! The page also lists rows from AniList's charts, asked for once a day and
//! kept in charts.json: the ten series and ten movies trending now, what's
//! popular this season, what has just finished airing, short series, hidden
//! gems, the best rated, and the most popular of each genre, the genres you
//! watch most first. Rows of "Because you watched" come from the
//! recommendations of the shows you watched last. A search finds any other
//! anime on AniList, so anything can be found to download.

use crate::anilist::{self, Anime, Listed};
use crate::backend::Backend;
use crate::series::{Graph, SAME_SERIES};
use crate::util::now_ms;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Setting: the AniList IDs of picks hidden from the page by an older Telly, which didn't ask why.
const HIDDEN: &str = "recommended.hidden";
/// Setting: what you said of the picks you left out, each a `Verdict`.
const VERDICTS: &str = "recommended.verdicts";
/// The status a pick you said wasn't for you has among the entries of your list (never one AniList has).
const NOT_FOR_ME: &str = "NOT_FOR_ME";
/// AniList is asked about a show again after a week.
const KEPT_MS: i64 = 7 * 24 * 3600 * 1000;
/// A pick's share of a show's votes is counted as if the show had this many
/// more, for none of its picks, so a show with only a handful of votes has a
/// smaller say.
const FEW_VOTES: f64 = 50.0;
/// A show started and not finished, with nothing of it watched for 30 days, was left.
const LEFT_MS: i64 = 30 * 24 * 3600 * 1000;
/// How much each further pick that one series says most for counts, of the one before it.
const SPREAD: f64 = 0.75;
/// A year (unix ms).
const YEAR_MS: i64 = 365 * 24 * 3600 * 1000;
/// What a kept answer has in it: one kept by an older Telly, which didn't
/// note sequels and what isn't out yet, is asked for again.
const KEPT_VERSION: u32 = 1;

/// What you said of a pick when you left it out: you'd seen it elsewhere and
/// liked it ("liked") or didn't ("disliked"), or it isn't for you ("notForMe").
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Verdict {
    pub id: i64,
    pub title: String,
    pub verdict: String,
}

/// What AniList says about one show in the library or on your list.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub id: i64,
    pub fetched_at: i64,
    #[serde(default)]
    pub version: u32,
    /// The rest of its series, in the library or not.
    pub related: Vec<i64>,
    /// Its 25 most voted recommendations.
    pub likes: Vec<Like>,
}

/// One recommendation: people who liked the show voted for this anime.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Like {
    pub votes: i64,
    pub anime: Anime,
    pub genres: Vec<String>,
    pub description: String,
    /// AniList's wide picture of it, for the top of the page ("" when it has none).
    #[serde(default)]
    pub banner: String,
    /// It carries on from another entry (AniList calls that its prequel): not one to start with.
    #[serde(default)]
    pub sequel: bool,
    /// It isn't out yet, so there's nothing of it to download.
    #[serde(default)]
    pub unreleased: bool,
}

/// A show in the library, as the ranking sees it.
pub struct Owned {
    pub anilist_id: i64,
    pub title: String,
    pub episodes: usize,
    pub watched: usize,
    /// When an episode of it was last watched (unix ms).
    pub last_watched: Option<i64>,
    /// How many episodes AniList's entry has, when it says.
    pub entry_episodes: Option<i64>,
}

/// A show in the library or on your list that a pick is recommended from.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct From {
    pub title: String,
    pub votes: i64,
}

/// An anime to watch next.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Pick {
    #[serde(flatten)]
    pub anime: Anime,
    pub title: String,
    pub genres: Vec<String>,
    pub description: String,
    /// The shows it's recommended from, the one that counts most first.
    pub from: Vec<From>,
    /// The anime it's another season or movie of, one you have ("" for a pick people recommend).
    pub more_of: String,
    /// The show in your library that it is, by its ID, when it's one that's trending or was searched for.
    pub show: Option<i64>,
    /// It isn't out yet, so there's nothing of it to download.
    pub unreleased: bool,
    /// AniList's wide picture of it, for the top of the page ("" when it has none).
    pub banner: String,
    /// Where it stands in a ranked row: "No. 3 of the series trending on AniList this week".
    pub standing: String,
    #[serde(skip)]
    score: f64,
}

fn query() -> String {
    format!(
        "query ($ids: [Int], $page: Int) {{ Page(page: $page, perPage: 50) {{ pageInfo {{ hasNextPage }}
  media(id_in: $ids, type: ANIME) {{ id
    relations {{ edges {{ relationType(version: 2) node {{ id type }} }} }}
    recommendations(sort: RATING_DESC, perPage: 25) {{ nodes {{ rating
      mediaRecommendation {{ type genres description(asHtml: false) bannerImage
        relations {{ edges {{ relationType(version: 2) }} }} {} }} }} }} }} }} }}",
        anilist::FIELDS
    )
}

fn list<'a>(v: &'a Value, pointer: &str) -> &'a [Value] {
    v.pointer(pointer).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// The shows in one page of AniList's answer.
fn sources(answer: &Value, now: i64) -> Vec<Source> {
    list(answer, "/data/Page/media")
        .iter()
        .filter_map(|media| {
            let related = list(media, "/relations/edges")
                .iter()
                .filter(|e| e.pointer("/node/type").and_then(Value::as_str) == Some("ANIME"))
                .filter(|e| e.get("relationType").and_then(Value::as_str).is_some_and(|t| SAME_SERIES.contains(&t)))
                .filter_map(|e| e.pointer("/node/id").and_then(Value::as_i64))
                .collect();
            let likes = list(media, "/recommendations/nodes")
                .iter()
                .filter_map(|node| {
                    // A recommendation can be a manga, or an entry AniList has since removed.
                    let to = node.get("mediaRecommendation").filter(|m| m.get("type").and_then(Value::as_str) == Some("ANIME"))?;
                    Some(Like {
                        votes: node.get("rating").and_then(Value::as_i64).unwrap_or(0),
                        anime: Anime::from_api(to)?,
                        genres: list(to, "/genres").iter().filter_map(Value::as_str).map(str::to_string).collect(),
                        description: to.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        banner: to.get("bannerImage").and_then(Value::as_str).unwrap_or("").to_string(),
                        sequel: list(to, "/relations/edges").iter().any(|e| e.get("relationType").and_then(Value::as_str) == Some("PREQUEL")),
                        unreleased: to.get("status").and_then(Value::as_str) == Some("NOT_YET_RELEASED"),
                    })
                })
                .collect();
            Some(Source { id: media.get("id")?.as_i64()?, fetched_at: now, version: KEPT_VERSION, related, likes })
        })
        .collect()
}

fn fetch(ids: &[i64]) -> Result<Vec<Source>> {
    let now = now_ms();
    let mut found = Vec::new();
    for page in 1.. {
        let answer = anilist::query_patiently(&query(), json!({ "ids": ids, "page": page }))?;
        found.extend(sources(&answer, now));
        if answer.pointer("/data/Page/pageInfo/hasNextPage").and_then(Value::as_bool) != Some(true) {
            break;
        }
    }
    // An ID AniList no longer has is kept too, empty, so it isn't asked about every time the page opens.
    for id in ids {
        if !found.iter().any(|s| s.id == *id) {
            found.push(Source { id: *id, fetched_at: now, version: KEPT_VERSION, ..Default::default() });
        }
    }
    Ok(found)
}

fn kept_path(backend: &Backend) -> PathBuf {
    backend.dirs.local.join("recommended.json")
}

/// What AniList says about each of `ids`: kept answers, and fresh ones for
/// shows it hasn't been asked about this week (or only by an older Telly).
fn sources_for(backend: &Backend, ids: &[i64]) -> Result<Vec<Source>> {
    // One asking at a time: a page opened while `gather` is asking waits for its answers rather than asking again.
    static ASKING: Mutex<()> = Mutex::new(());
    let _asking = ASKING.lock();
    let path = kept_path(backend);
    let mut kept: Vec<Source> = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    kept.retain(|s| ids.contains(&s.id));
    let now = now_ms();
    let old: Vec<i64> = ids.iter().copied().filter(|id| !kept.iter().any(|s| s.id == *id && s.version == KEPT_VERSION && now - s.fetched_at < KEPT_MS)).collect();
    if !old.is_empty() {
        match fetch(&old) {
            Ok(fresh) => {
                kept.retain(|s| !old.contains(&s.id));
                kept.extend(fresh);
                crate::util::write_whole(&path, serde_json::to_vec(&kept)?)?;
            }
            // Offline: last week's answers still stand, when there's one for every show.
            Err(_) if ids.iter().all(|id| kept.iter().any(|s| s.id == *id)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(kept)
}

/// The series `id` belongs to, named by one of its entries.
fn series_of(series: &mut HashMap<i64, i64>, id: i64) -> i64 {
    let mut root = id;
    while let Some(&up) = series.get(&root).filter(|up| **up != root) {
        root = up;
    }
    series.insert(id, root);
    root
}

/// How much your liking a show counts for, from how much of it you've
/// watched: 2 watched to the end, 1 to 2 as you watch it, 0.5 not started
/// (having it only says you're curious) and 0.25 left part-way.
fn liking(episodes: usize, watched: usize, last_watched: Option<i64>, now: i64) -> f64 {
    if episodes > 0 && watched >= episodes {
        2.0
    } else if watched == 0 {
        0.5
    } else if last_watched.is_some_and(|at| now - at > LEFT_MS) {
        0.25
    } else {
        1.0 + watched as f64 / episodes as f64
    }
}

/// Tastes change: how much what you watched at `at` (unix ms, 0 when it isn't
/// known) still says about you. Whole for the last year, then half as much for
/// every two years more, down to a quarter.
fn freshness(at: i64, now: i64) -> f64 {
    if at <= 0 {
        return 1.0;
    }
    let years = (now - at) as f64 / YEAR_MS as f64;
    0.5f64.powf((years - 1.0).max(0.0) / 2.0).max(0.25)
}

/// How much your liking an anime on your AniList list counts for, from its
/// status and your score, or None when the list doesn't say: dropped -1;
/// scored, 2 for your average score and 1 more or less for each 10 points
/// above or below it (-2 to 4); else completed 2, watching 1 to 2 by how far
/// you are, and paused 0.25. A pick you said wasn't for you counts -0.5.
fn list_liking(entry: &Listed, average_score: f64) -> Option<f64> {
    match entry.status.as_str() {
        "DROPPED" => Some(-1.0),
        NOT_FOR_ME => Some(-0.5),
        "" => None,
        _ if entry.score > 0.0 => Some((2.0 + (entry.score - average_score) / 10.0).clamp(-2.0, 4.0)),
        "COMPLETED" | "REPEATING" => Some(2.0),
        "CURRENT" => Some(1.0 + entry.episodes.filter(|&e| e > 0).map_or(0.0, |e| (entry.progress as f64 / e as f64).min(1.0))),
        "PAUSED" => Some(0.25),
        _ => None,
    }
}

/// The picks, best first. Each series in the library or on your list has one
/// say on a pick: the biggest any of its entries gives it, which is the square
/// root of the pick's share of that entry's votes, times how much your liking
/// the entry counts (less the longer ago you watched it). So a little-known show's favourite counts as much as a
/// famous show's, and a pick liked from many shows still beats one show's
/// favourite. Each further pick that one series says most for counts three
/// quarters as much as the one before it, so its recommendations don't fill
/// the page. A show you dropped or scored low says against its picks, and a
/// pick it says more against than the rest say for is left out. What you said
/// of the picks you left out is among `listed` (see `said`). Also left
/// out: what's in the library or on your list, what's hidden, sequels (not
/// somewhere to start) and what isn't out yet.
///
/// The rest of an anime you have is picked too (`graph` has which entries
/// those are): its seasons and movies you don't have, each as that anime's
/// own favourite, with the whole say of the part you liked most (however long
/// ago you watched it). Of several,
/// the earliest out has that say, the next half of it, the third a third, so
/// what comes next leads and thirty films of one anime don't fill the page.
/// Recap films, specials, OVAs and shorts aren't picked. Nor is any of an
/// anime with a show that holds more episodes than its entry has: seasons
/// added as one show, so what looks missing may be in it.
pub fn rank(owned: &[Owned], listed: &[Listed], sources: &[Source], hidden: &HashSet<i64>, now: i64, graph: &Graph) -> Vec<Pick> {
    // Each show's episodes, those watched, when it was last watched and its title, across the library's copies of it.
    let mut watching: HashMap<i64, (usize, usize, Option<i64>, &str)> = HashMap::new();
    for o in owned {
        let show = watching.entry(o.anilist_id).or_insert((0, 0, None, o.title.as_str()));
        *show = (show.0 + o.episodes, show.1 + o.watched, show.2.max(o.last_watched), show.3);
    }
    let last_watched: HashMap<i64, i64> = watching.iter().filter_map(|(&id, show)| Some((id, show.2?))).collect();
    // How much your liking each show counts for, how much of that it still says about you (less the longer ago you
    // watched it), and its title: as your list says, or else by how much of it you've watched.
    let mut shows: HashMap<i64, (f64, f64, &str)> = watching
        .into_iter()
        .map(|(id, (episodes, watched, last_watched, title))| (id, (liking(episodes, watched, last_watched, now), freshness(last_watched.unwrap_or(0), now), title)))
        .collect();
    let scores: Vec<f64> = listed.iter().map(|l| l.score).filter(|&s| s > 0.0).collect();
    let average_score = scores.iter().sum::<f64>() / scores.len().max(1) as f64;
    for entry in listed {
        if let Some(liking) = list_liking(entry, average_score) {
            let title = shows.get(&entry.id).map_or(entry.title.as_str(), |s| s.2);
            let at = entry.at.max(last_watched.get(&entry.id).copied().unwrap_or(0));
            shows.insert(entry.id, (liking, freshness(at, now), title));
        }
    }
    let mut series: HashMap<i64, i64> = shows.keys().map(|&id| (id, id)).collect();
    let sources: Vec<&Source> = sources.iter().filter(|s| shows.contains_key(&s.id)).collect();
    for s in &sources {
        for &r in &s.related {
            let (a, b) = (series_of(&mut series, s.id), series_of(&mut series, r));
            series.insert(b, a);
        }
    }
    // Entries tied through others you don't have are one series too: ten films, each of a TV series that isn't here.
    let yours: HashSet<i64> = shows.keys().filter_map(|&id| graph.root(id)).collect();
    for (id, root) in graph.roots().filter(|(_, root)| yours.contains(root)) {
        let (a, b) = (series_of(&mut series, root), series_of(&mut series, id));
        series.insert(b, a);
    }
    // For each pick, each series' say, and the votes and title of the entry that gives it.
    let mut says: HashMap<i64, (&Like, HashMap<i64, (f64, i64, &str)>)> = HashMap::new();
    for s in &sources {
        let from = series_of(&mut series, s.id);
        let (liking, fresh, title) = shows[&s.id];
        let liking = liking * fresh;
        // Its votes, but for the rest of its own series: those aren't recommendations.
        let mut votes = 0;
        for like in s.likes.iter().filter(|l| l.votes > 0) {
            if !series.contains_key(&like.anime.id) || series_of(&mut series, like.anime.id) != from {
                votes += like.votes;
            }
        }
        // A sequel isn't somewhere to start, and what isn't out yet can't be had.
        for like in s.likes.iter().filter(|l| l.votes > 0 && !l.sequel && !l.unreleased && !series.contains_key(&l.anime.id) && !hidden.contains(&l.anime.id)) {
            let say = liking * (like.votes as f64 / (votes as f64 + FEW_VOTES)).sqrt();
            let best = says.entry(like.anime.id).or_insert_with(|| (like, HashMap::new())).1.entry(from).or_insert((f64::NEG_INFINITY, 0, title));
            if say > best.0 {
                *best = (say, like.votes, title);
            }
        }
    }
    // Each pick, and the series that says most for it.
    let mut led: Vec<(Pick, i64)> = says
        .into_values()
        .map(|(like, by_series)| {
            let mut from: Vec<(f64, i64, From)> =
                by_series.into_iter().map(|(series, (say, votes, title))| (say, series, From { title: title.to_string(), votes })).collect();
            from.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.2.title.cmp(&b.2.title)));
            let lead = from.first().map_or(0, |f| f.1);
            let pick = Pick {
                title: like.anime.short_title(),
                anime: like.anime.clone(),
                genres: like.genres.clone(),
                description: like.description.clone(),
                score: from.iter().map(|f| f.0).sum(),
                // Only the shows that say for it: it isn't recommended from one you dropped.
                from: from.into_iter().filter(|f| f.0 > 0.0).map(|f| f.2).collect(),
                more_of: String::new(),
                show: None,
                unreleased: false,
                banner: like.banner.clone(),
                standing: String::new(),
            };
            (pick, lead)
        })
        .filter(|(p, _)| p.score > 0.0)
        .collect();
    // So one show's recommendations don't fill the page, each pick a series says most for counts
    // three quarters as much as the one before it.
    led.sort_by(|a, b| b.0.score.total_cmp(&a.0.score).then_with(|| a.0.anime.id.cmp(&b.0.anime.id)));
    let mut before: HashMap<i64, i32> = HashMap::new();
    let mut picks: Vec<Pick> = led
        .into_iter()
        .map(|(mut pick, lead)| {
            let n = before.entry(lead).or_default();
            pick.score *= SPREAD.powi(*n);
            *n += 1;
            pick
        })
        .collect();
    // The seasons and movies you don't have of each anime you do.
    let holds_more: HashSet<i64> = owned.iter().filter(|o| o.entry_episodes.is_some_and(|e| o.episodes as i64 > e)).filter_map(|o| graph.root(o.anilist_id)).collect();
    for &root in yours.iter().filter(|root| !holds_more.contains(root)) {
        let parts = graph.parts(root);
        // The part of it you liked most says how much the rest is worth to you, however long ago you watched it:
        // wanting to see how a story goes on doesn't fade as tastes do.
        let Some((liking, _, title)) = parts.iter().filter_map(|e| shows.get(&e.anime.id)).max_by(|a, b| a.0.total_cmp(&b.0)).copied() else { continue };
        if liking <= 0.0 {
            continue;
        }
        let retold = graph.retellings(root);
        let missing = parts.iter().filter(|e| {
            let whole = matches!(e.anime.format.as_str(), "TV" | "ONA" | "MOVIE") && e.minutes.is_none_or(|m| m >= 15);
            whole && e.status != "NOT_YET_RELEASED" && !shows.contains_key(&e.anime.id) && !hidden.contains(&e.anime.id) && !retold.contains(&e.anime.id)
        });
        let name = graph.entry(root).map_or_else(|| title.to_string(), |e| e.anime.short_title());
        for (nth, e) in missing.enumerate() {
            picks.push(Pick {
                title: e.anime.short_title(),
                anime: e.anime.clone(),
                genres: e.genres.clone(),
                description: e.description.clone(),
                from: vec![From { title: title.to_string(), votes: 0 }],
                more_of: name.clone(),
                show: None,
                unreleased: false,
                banner: String::new(),
                standing: String::new(),
                score: liking / (nth + 1) as f64,
            });
        }
    }
    picks.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.anime.id.cmp(&b.anime.id)));
    picks
}

/// The picks left out from now on: those you said something of, and those hidden before Telly asked why.
fn hidden(backend: &Backend) -> HashSet<i64> {
    let ids: HashSet<i64> = backend.db.setting(HIDDEN).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    ids.into_iter().chain(verdicts(backend).into_iter().map(|v| v.id)).collect()
}

fn verdicts(backend: &Backend) -> Vec<Verdict> {
    backend.db.setting(VERDICTS).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// Leaves a pick out from now on, with what you said of it: it then counts as
/// an anime on your list would, for or against the picks like it.
pub fn hide(backend: &Backend, verdict: Verdict) -> Result<()> {
    anyhow::ensure!(matches!(verdict.verdict.as_str(), "liked" | "disliked" | "notForMe"), "No such verdict: {}", verdict.verdict);
    let mut all = verdicts(backend);
    all.retain(|v| v.id != verdict.id);
    all.push(verdict);
    backend.db.set_setting(VERDICTS, &json!(all))
}

/// What you said of the picks you left out, as entries of your list: one you
/// liked as one completed, one you didn't as one dropped, and one that isn't
/// for you as `NOT_FOR_ME`. Those since in the library or on your list are
/// left to say for themselves.
fn said(verdicts: Vec<Verdict>, owned: &[Owned], listed: &[Listed]) -> Vec<Listed> {
    verdicts
        .into_iter()
        .filter(|v| !owned.iter().any(|o| o.anilist_id == v.id) && !listed.iter().any(|l| l.id == v.id))
        .map(|v| {
            let status = match v.verdict.as_str() {
                "liked" => "COMPLETED",
                "disliked" => "DROPPED",
                _ => NOT_FOR_ME,
            };
            Listed { id: v.id, title: v.title, status: status.into(), ..Default::default() }
        })
        .collect()
}

/// Asks AniList, in the background, about the shows it hasn't been asked about
/// this week: at start-up and when a show is added, so the Recommended page
/// has its answers when it opens.
pub fn gather(backend: &Arc<Backend>) {
    let backend = backend.clone();
    std::thread::spawn(move || {
        let _ = view(&backend);
    });
}

/// AniList is asked for its charts again after a day.
const CHARTS_MS: i64 = 24 * 3600 * 1000;
/// How many anime a row holds.
const ROW: usize = 20;
/// AniList's genres, each a row of its most popular. Ecchi shows only when it's one you watch most.
const GENRES: &[&str] = &[
    "Action", "Adventure", "Comedy", "Drama", "Ecchi", "Fantasy", "Horror", "Mahou Shoujo", "Mecha", "Music", "Mystery",
    "Psychological", "Romance", "Sci-Fi", "Slice of Life", "Sports", "Supernatural", "Thriller",
];

/// AniList's charts, as they were when last asked: each list by its key ("series", "genre:Romance"…).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Charts {
    fetched_at: i64,
    /// The season "Popular this season" is of: "Autumn 2026".
    season: String,
    lists: Vec<(String, Vec<Like>)>,
}

impl Charts {
    fn list(&self, key: &str) -> &[Like] {
        self.lists.iter().find(|(k, _)| k == key).map_or(&[], |(_, likes)| likes.as_slice())
    }
}

/// One anime of a list AniList answers with.
fn listed_anime(media: &Value) -> Option<Like> {
    Some(Like {
        votes: 0,
        anime: Anime::from_api(media)?,
        genres: list(media, "/genres").iter().filter_map(Value::as_str).map(str::to_string).collect(),
        description: media.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
        banner: media.get("bannerImage").and_then(Value::as_str).unwrap_or("").to_string(),
        sequel: list(media, "/relations/edges").iter().any(|e| e.get("relationType").and_then(Value::as_str) == Some("PREQUEL")),
        unreleased: media.get("status").and_then(Value::as_str) == Some("NOT_YET_RELEASED"),
    })
}

/// The anime of one of the answer's lists that are out: one still to come has nothing to download.
fn out_now(answer: &Value, name: &str) -> Vec<Like> {
    list(answer, &format!("/data/{name}/media")).iter().filter_map(listed_anime).filter(|like| !like.unreleased).collect()
}

/// The year, month and day of a time (unix ms), in UTC.
fn date_of(ms: i64) -> (i64, i64, i64) {
    // Howard Hinnant's days-to-civil.
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// AniList's season at a time, its year, and its name: December is the next year's winter.
fn season_at(ms: i64) -> (&'static str, i64, &'static str) {
    let (year, month, _) = date_of(ms);
    match month {
        12 => ("WINTER", year + 1, "Winter"),
        1 | 2 => ("WINTER", year, "Winter"),
        3..=5 => ("SPRING", year, "Spring"),
        6..=8 => ("SUMMER", year, "Summer"),
        _ => ("FALL", year, "Autumn"),
    }
}

/// Each chart's key and what AniList is asked for it, at `now`.
fn chart_queries(now: i64) -> Vec<(String, String)> {
    let (season, year, _) = season_at(now);
    let (y, m, d) = date_of(now - 90 * 86_400_000);
    let mut out = vec![
        ("series".to_string(), "format: TV, sort: TRENDING_DESC".to_string()),
        ("movies".to_string(), "format: MOVIE, sort: TRENDING_DESC".to_string()),
        ("season".to_string(), format!("season: {season}, seasonYear: {year}, format_in: [TV, ONA], sort: POPULARITY_DESC")),
        ("finished".to_string(), format!("format: TV, status: FINISHED, endDate_greater: {}, sort: POPULARITY_DESC", y * 10_000 + m * 100 + d)),
        ("short".to_string(), "format: TV, status: FINISHED, episodes_lesser: 14, sort: POPULARITY_DESC".to_string()),
        ("gems".to_string(), "format_in: [TV, MOVIE], averageScore_greater: 77, popularity_lesser: 40000, sort: SCORE_DESC".to_string()),
        ("top".to_string(), "format_in: [TV, MOVIE], sort: SCORE_DESC".to_string()),
    ];
    for genre in GENRES {
        out.push((format!("genre:{genre}"), format!("genre: \"{genre}\", format_in: [TV, MOVIE, ONA], sort: POPULARITY_DESC")));
    }
    out
}

fn fetch_charts(now: i64) -> Result<Charts> {
    let mut lists = Vec::new();
    // A few lists to a request, so none is too much for AniList at once.
    for batch in chart_queries(now).chunks(5) {
        let pages: Vec<String> = batch
            .iter()
            .enumerate()
            .map(|(i, (_, filter))| {
                format!(
                    "l{i}: Page(perPage: 50) {{ media(type: ANIME, isAdult: false, {filter}) {{ genres description(asHtml: false) bannerImage relations {{ edges {{ relationType }} }} {} }} }}",
                    anilist::FIELDS
                )
            })
            .collect();
        let answer = anilist::query_patiently(&format!("query {{ {} }}", pages.join(" ")), json!({}))?;
        for (i, (key, _)) in batch.iter().enumerate() {
            lists.push((key.clone(), out_now(&answer, &format!("l{i}"))));
        }
    }
    let (_, year, name) = season_at(now);
    Ok(Charts { fetched_at: now, season: format!("{name} {year}"), lists })
}

/// AniList's charts: the kept answer while it's under a day old, else a fresh one, else (offline) the kept one however old.
fn charts(backend: &Backend) -> Charts {
    static ASKING: Mutex<()> = Mutex::new(());
    let _asking = ASKING.lock();
    let path = backend.dirs.local.join("charts.json");
    let kept: Charts = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    if now_ms() - kept.fetched_at < CHARTS_MS {
        return kept;
    }
    match fetch_charts(now_ms()) {
        Ok(fresh) => {
            if let Ok(bytes) = serde_json::to_vec(&fresh) {
                let _ = crate::util::write_whole(&path, bytes);
                // What Telly 0.6.1 kept before these rows: only the two top tens.
                let _ = std::fs::remove_file(backend.dirs.local.join("trending.json"));
            }
            fresh
        }
        Err(_) => kept,
    }
}

/// The show in the library that each AniList entry is.
fn in_library(backend: &Backend) -> Result<HashMap<i64, i64>> {
    Ok(backend.db.shows()?.iter().filter_map(|s| Some((s.anilist_id?, s.id))).collect())
}

/// A pick that's listed, not recommended: one from a chart, or found by a search.
fn listed_pick(like: &Like, unreleased: bool, have: &HashMap<i64, i64>) -> Pick {
    Pick {
        title: like.anime.short_title(),
        anime: like.anime.clone(),
        genres: like.genres.clone(),
        description: like.description.clone(),
        from: Vec::new(),
        more_of: String::new(),
        show: have.get(&like.anime.id).copied(),
        unreleased,
        banner: like.banner.clone(),
        standing: String::new(),
        score: 0.0,
    }
}

/// The anime AniList finds for `text`, for downloading one that's in no row:
/// each with the show in your library it is, when you have it, and whether
/// it's still to come out.
pub fn search(backend: &Backend, text: &str) -> Result<Vec<Pick>> {
    let q = format!("query ($search: String) {{ Page(perPage: 20) {{ media(search: $search, type: ANIME, sort: SEARCH_MATCH) {{ genres description(asHtml: false) bannerImage {} }} }} }}", anilist::FIELDS);
    let answer = anilist::query_patiently(&q, json!({ "search": text.split_whitespace().collect::<Vec<_>>().join(" ") }))?;
    let have = in_library(backend)?;
    Ok(list(&answer, "/data/Page/media").iter().filter_map(listed_anime).map(|like| listed_pick(&like, like.unreleased, &have)).collect())
}

/// One row of the page.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    /// What the row is: "suggested" (its Show all lists every pick), "more", "because", "series", "genre:Romance"…
    pub key: String,
    pub title: String,
    /// A few words beside the title: "Trending on AniList".
    pub note: String,
    /// Its anime are numbered, as a top ten.
    pub ranked: bool,
    pub picks: Vec<Pick>,
}

/// The genres you watch most, most first: each of your shows' genres, by how
/// many of its episodes you've watched (one more for having it), and Ecchi only
/// when it's among your first three.
fn your_genres(owned: &[Owned], graph: &Graph) -> Vec<String> {
    let mut weight: HashMap<&str, f64> = HashMap::new();
    for o in owned {
        let Some(entry) = graph.entry(o.anilist_id) else { continue };
        for genre in &entry.genres {
            *weight.entry(genre.as_str()).or_default() += o.watched as f64 + 1.0;
        }
    }
    let mut genres: Vec<(&str, f64)> = GENRES.iter().map(|g| (*g, weight.get(g).copied().unwrap_or(0.0))).collect();
    genres.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    genres.into_iter().enumerate().filter(|(i, (g, _))| *g != "Ecchi" || *i < 3).map(|(_, (g, _))| g.to_string()).collect()
}

/// A genre's name as the page writes it: "Slice of life".
fn genre_name(genre: &str) -> String {
    match genre {
        "Mahou Shoujo" => "Magical girls".into(),
        "Sci-Fi" => "Sci-fi".into(),
        "Slice of Life" => "Slice of life".into(),
        g => g.into(),
    }
}

/// The page's rows, in the order they're shown. `picks` are the
/// recommendations, best first; `left_out` what's on your AniList list or
/// hidden; `have` the library's shows by their AniList ID.
fn rows(picks: &[Pick], charts: &Charts, owned: &[Owned], sources: &[Source], graph: &Graph, have: &HashMap<i64, i64>, left_out: &HashSet<i64>) -> Vec<Row> {
    let row = |key: &str, title: String, note: &str, ranked: bool, picks: Vec<Pick>| Row { key: key.into(), title, note: note.into(), ranked, picks };
    // A chart's row: what you don't have or have on your list, and that isn't a sequel you can't start with.
    let fresh = |key: &str| -> Vec<Pick> {
        charts.list(key).iter().filter(|l| !l.sequel && !have.contains_key(&l.anime.id) && !left_out.contains(&l.anime.id)).take(ROW).map(|l| listed_pick(l, false, have)).collect()
    };
    let top_ten = |key: &str, what: &str| -> Vec<Pick> {
        charts.list(key).iter().take(10).enumerate().map(|(i, l)| Pick { standing: format!("No. {} of the {what} trending on AniList this week", i + 1), ..listed_pick(l, false, have) }).collect()
    };

    // Because you watched: the shows you watched last, a different anime each, by AniList's users' recommendations for them.
    let yours: HashSet<i64> = owned.iter().filter_map(|o| graph.root(o.anilist_id)).collect();
    let mut watched: Vec<&Owned> = owned.iter().filter(|o| o.last_watched.is_some()).collect();
    watched.sort_by_key(|o| std::cmp::Reverse(o.last_watched));
    let mut because: Vec<Row> = Vec::new();
    let mut seen_anime: HashSet<i64> = HashSet::new();
    for o in watched {
        if because.len() == 2 {
            break;
        }
        if !seen_anime.insert(graph.root(o.anilist_id).unwrap_or(o.anilist_id)) {
            continue;
        }
        let Some(source) = sources.iter().find(|s| s.id == o.anilist_id) else { continue };
        let mut likes: Vec<&Like> = source
            .likes
            .iter()
            .filter(|l| l.votes > 0 && !l.sequel && !l.unreleased && !have.contains_key(&l.anime.id) && !left_out.contains(&l.anime.id))
            .filter(|l| graph.root(l.anime.id).is_none_or(|root| !yours.contains(&root)))
            .collect();
        likes.sort_by_key(|l| std::cmp::Reverse(l.votes));
        if likes.len() < 5 {
            continue;
        }
        let picks = likes.into_iter().take(ROW).map(|l| Pick { from: vec![From { title: o.title.clone(), votes: l.votes }], ..listed_pick(l, false, have) }).collect();
        because.push(row("because", format!("Because you watched {}", o.title), "", false, picks));
    }
    let mut because = because.into_iter();

    let genres = your_genres(owned, graph);
    let genre_row = |genre: &str, yours: bool| row(&format!("genre:{genre}"), genre_name(genre), if yours { "One you watch a lot" } else { "Most popular on AniList" }, false, fresh(&format!("genre:{genre}")));
    let mut out = vec![
        row("suggested", "Suggested for you".into(), "", false, picks.iter().filter(|p| p.more_of.is_empty()).take(ROW).cloned().collect()),
        row("more", "More of your anime".into(), "Seasons and movies you don’t have", false, picks.iter().filter(|p| !p.more_of.is_empty()).take(ROW).cloned().collect()),
        row("series", "Top 10 series this week".into(), "Trending on AniList", true, top_ten("series", "series")),
    ];
    out.extend(because.next());
    out.extend(genres.first().map(|g| genre_row(g, true)));
    out.push(row("movies", "Top 10 movies this week".into(), "Trending on AniList", true, top_ten("movies", "movies")));
    out.push(row("season", "Popular this season".into(), &charts.season, false, fresh("season")));
    out.extend(genres.get(1).map(|g| genre_row(g, true)));
    out.extend(because.next());
    out.push(row("finished", "Just finished".into(), "Every episode out", false, fresh("finished")));
    out.extend(genres.get(2).map(|g| genre_row(g, true)));
    out.push(row("gems", "Hidden gems".into(), "Loved by the few who’ve seen them", false, fresh("gems")));
    out.push(row("short", "Short series".into(), "13 episodes or fewer", false, fresh("short")));
    out.push(row("top", "Best rated of all time".into(), "By AniList’s users", false, fresh("top")));
    out.extend(genres.iter().skip(3).filter(|g| g.as_str() != "Ecchi").map(|g| genre_row(g, false)));
    out.retain(|r| !r.picks.is_empty());
    out
}

/// The Recommended page: the picks, how many shows (in the library or on
/// your list) they come from, and the page's rows.
pub fn view(backend: &Backend) -> Result<Value> {
    let mut owned = Vec::new();
    for show in backend.db.shows()? {
        if let Some(anilist_id) = show.anilist_id {
            let episodes = backend.db.episodes(show.id)?;
            owned.push(Owned {
                anilist_id,
                title: show.title,
                episodes: episodes.len(),
                watched: episodes.iter().filter(|e| e.watched_at.is_some()).count(),
                last_watched: episodes.iter().filter_map(|e| e.watched_at).max(),
                entry_episodes: show.anilist_episodes,
            });
        }
    }
    let mut listed = crate::anilist_list::listed(backend);
    listed.extend(said(verdicts(backend), &owned, &listed));
    // A copy of the list kept before Telly read its statuses only leaves its anime out.
    let mut ids: Vec<i64> = owned.iter().map(|o| o.anilist_id).chain(listed.iter().filter(|l| !l.status.is_empty()).map(|l| l.id)).collect();
    ids.sort_unstable();
    ids.dedup();
    let sources = if ids.is_empty() { Vec::new() } else { sources_for(backend, &ids)? };
    let mut left_out = hidden(backend);
    left_out.extend(listed.iter().map(|l| l.id));
    let graph = crate::series::graph(backend);
    let picks = rank(&owned, &listed, &sources, &left_out, now_ms(), &graph);
    let rows = rows(&picks, &charts(backend), &owned, &sources, &graph, &in_library(backend)?, &left_out);
    Ok(json!({ "shows": ids.len(), "picks": picks, "rows": rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;
    const DAY: i64 = 24 * 3600 * 1000;

    /// A show in the library, its last episode watched today when any is.
    fn owned(anilist_id: i64, title: &str, episodes: usize, watched: usize) -> Owned {
        Owned { anilist_id, title: title.into(), episodes, watched, last_watched: (watched > 0).then_some(NOW), entry_episodes: None }
    }

    fn source(id: i64, related: &[i64], likes: &[(i64, i64)]) -> Source {
        Source {
            id,
            fetched_at: 0,
            version: KEPT_VERSION,
            related: related.to_vec(),
            likes: likes.iter().map(|&(to, votes)| Like { votes, anime: Anime { id: to, romaji: format!("Anime {to}"), ..Default::default() }, ..Default::default() }).collect(),
        }
    }

    fn ids(picks: &[Pick]) -> Vec<i64> {
        picks.iter().map(|p| p.anime.id).collect()
    }

    #[test]
    fn a_chart_s_row_leaves_out_what_you_have_sequels_and_what_isn_t_out() {
        let anime = |id: i64, status: &str, prequel: bool| {
            let edges = if prequel { json!([{ "relationType": "PREQUEL" }]) } else { json!([{ "relationType": "SEQUEL" }]) };
            json!({ "id": id, "format": "TV", "status": status, "title": { "romaji": format!("Anime {id}") }, "genres": ["Drama"], "description": "About it.", "bannerImage": format!("https://b/{id}.jpg"), "relations": { "edges": edges } })
        };
        let media: Vec<Value> = (1..=14).map(|id| anime(id, if id == 2 { "NOT_YET_RELEASED" } else { "FINISHED" }, id == 4)).collect();
        let answer = json!({ "data": { "l0": { "media": media } } });
        let likes = out_now(&answer, "l0");
        assert_eq!(likes.len(), 13);
        assert_eq!((likes[0].banner.as_str(), likes[2].sequel, likes[0].sequel), ("https://b/1.jpg", true, false));
        let charts = Charts { fetched_at: 0, season: "Autumn 2026".into(), lists: vec![("series".into(), likes.clone()), ("genre:Drama".into(), likes)] };
        let have = HashMap::from([(3, 42)]);
        let rows = rows(&[], &charts, &[], &[], &Graph::default(), &have, &HashSet::from([5]));
        // The top ten keeps what you have, marked; a genre's row leaves out what you have, the hidden, and the sequel.
        assert_eq!(rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["series", "genre:Drama"]);
        assert_eq!(ids(&rows[0].picks), [1, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!((rows[0].picks[1].show, rows[0].picks[1].standing.as_str()), (Some(42), "No. 2 of the series trending on AniList this week"));
        assert_eq!(ids(&rows[1].picks), [1, 6, 7, 8, 9, 10, 11, 12, 13, 14]);
        assert_eq!((rows[1].title.as_str(), rows[1].note.as_str()), ("Drama", "Most popular on AniList"));
    }

    #[test]
    fn seasons_and_dates_are_anilist_s() {
        // 14 November 2026, 1 December 2026 and 10 February 2027, at noon UTC.
        assert_eq!(date_of(1_794_657_600_000), (2026, 11, 14));
        assert_eq!(season_at(1_794_657_600_000), ("FALL", 2026, "Autumn"));
        assert_eq!(season_at(1_796_126_400_000), ("WINTER", 2027, "Winter"));
        assert_eq!(season_at(1_802_260_800_000), ("WINTER", 2027, "Winter"));
        let queries = chart_queries(1_794_657_600_000);
        assert!(queries.iter().any(|(k, q)| k == "finished" && q.contains("endDate_greater: 20260816")));
        assert!(queries.iter().any(|(k, q)| k == "genre:Slice of Life" && q.contains("genre: \"Slice of Life\"")));
    }

    #[test]
    fn the_seasons_and_movies_you_don_t_have_of_an_anime_you_do_are_picks() {
        let graph = Graph::new(crate::series::tests::overlord());
        // Overlord's three seasons, the first two watched; another show recommends something as strongly as a show can.
        let library = [owned(20832, "Overlord", 13, 13), owned(98437, "Overlord II", 13, 13), owned(101474, "Overlord III", 13, 0), owned(1, "Clannad", 23, 23)];
        let answers = [source(20832, &[98437], &[(133844, 500), (300, 5)]), source(1, &[], &[(200, 900)])];
        let picks = rank(&library, &[], &answers, &HashSet::new(), NOW, &graph);
        // The next season leads, then the film after it at half its say; the recap films, the OVA and the specials aren't picked,
        // and AniList's users recommending the fourth season from the first doesn't make it a pick twice.
        assert_eq!(ids(&picks), [133844, 200, 133845, 300]);
        assert_eq!((picks[0].more_of.as_str(), picks[0].from[0].title.as_str(), picks[1].more_of.as_str()), ("Overlord", "Overlord II", ""));
        assert!((picks[0].score - 2.0).abs() < 1e-9 && (picks[2].score - 1.0).abs() < 1e-9);
        // Hidden, it's left out, and the film takes its place; an anime you dropped offers nothing more.
        assert_eq!(ids(&rank(&library, &[], &answers, &HashSet::from([133844]), NOW, &graph))[..2], [133845, 200]);
        // Two seasons added as one show: the second may be in it, so nothing more of the anime is offered.
        let one_show = [Owned { entry_episodes: Some(13), ..owned(20832, "Overlord", 26, 26) }];
        assert_eq!(ids(&rank(&one_show, &[], &[], &HashSet::new(), NOW, &graph)), [0i64; 0]);
        let dropped = [Listed { id: 20832, title: "Overlord".into(), status: "DROPPED".into(), score: 0.0, progress: 3, episodes: Some(13), at: 0 }];
        assert_eq!(ids(&rank(&[], &dropped, &[], &HashSet::new(), NOW, &graph)), [0i64; 0]);
    }

    #[test]
    fn many_shows_beat_one_well_known_show() {
        let library = [owned(1, "Your Name.", 1, 0), owned(2, "Clannad", 23, 0), owned(3, "Angel Beats!", 13, 0), owned(4, "K-ON!", 13, 0)];
        let answers = [source(1, &[], &[(100, 6000), (101, 150)]), source(2, &[], &[(101, 120)]), source(3, &[], &[(101, 90)]), source(4, &[], &[(101, 60)])];
        let picks = rank(&library, &[], &answers, &HashSet::new(), NOW, &Graph::default());
        assert_eq!(ids(&picks), [101, 100]);
        // 150 is a sliver of Your Name.'s votes, and each other show's only pick.
        let from: Vec<(&str, i64)> = picks[0].from.iter().map(|f| (f.title.as_str(), f.votes)).collect();
        assert_eq!(from, [("Clannad", 120), ("Angel Beats!", 90), ("K-ON!", 60), ("Your Name.", 150)]);
    }

    #[test]
    fn a_little_known_shows_favourite_counts_as_much_as_a_famous_ones() {
        let library = [owned(1, "Famous", 12, 0), owned(2, "Little known", 12, 0)];
        let answers = [source(1, &[], &[(100, 6000), (102, 5000), (101, 4000)]), source(2, &[], &[(200, 40), (201, 20)])];
        // 40 of 60 votes is about as big a share as 6,000 of 15,000, and a bigger one than 5,000 of them.
        // Famous's third pick, at a little over half its say, falls behind Little known's second.
        assert_eq!(ids(&rank(&library, &[], &answers, &HashSet::new(), NOW, &Graph::default())), [100, 200, 102, 201, 101]);
    }

    #[test]
    fn one_show_s_recommendations_don_t_fill_the_page() {
        // Seen to the end, Loved has ten picks of about 0.62 each; Curious, not started, two of about 0.3.
        let loved: Vec<(i64, i64)> = (100..110).map(|id| (id, 100)).collect();
        let answers = [source(1, &[], &loved), source(2, &[], &[(200, 60), (201, 60)])];
        let library = [owned(1, "Loved", 12, 12), owned(2, "Curious", 12, 0)];
        let picks = rank(&library, &[], &answers, &HashSet::new(), NOW, &Graph::default());
        // Loved's fourth pick counts 0.26, behind Curious's first.
        assert_eq!(ids(&picks)[..6], [100, 101, 102, 200, 103, 201]);
    }

    #[test]
    fn a_series_has_one_say_and_none_of_it_is_a_pick() {
        // Three seasons in the library; the second and the fourth are joined
        // through the third season's second part, which the library lacks.
        let library = [owned(1, "Titan", 25, 0), owned(2, "Titan Season 2", 12, 0), owned(4, "Titan Final Season", 16, 0), owned(9, "Monster", 74, 0)];
        let answers = [
            source(1, &[2], &[(100, 400), (5, 50), (4, 30)]),
            source(2, &[1, 3], &[(100, 300), (3, 80)]),
            source(4, &[3], &[(100, 200)]),
            source(9, &[], &[(200, 500), (201, 400), (1, 40)]),
        ];
        let picks = rank(&library, &[], &answers, &HashSet::new(), NOW, &Graph::default());
        assert_eq!(ids(&picks), [100, 200, 201, 5]);
        // Its say is Season 2's alone: 300 of its 300 votes, as Season 3's 80 aren't a recommendation.
        assert_eq!(picks[0].from, [From { title: "Titan Season 2".into(), votes: 300 }]);
        assert!((picks[0].score - 0.5 * (300.0 / (300.0 + FEW_VOTES)).sqrt()).abs() < 1e-9);
    }

    #[test]
    fn how_much_of_a_show_you_watched_counts() {
        assert_eq!(liking(12, 12, Some(NOW - 400 * DAY), NOW), 2.0);
        assert_eq!(liking(12, 6, Some(NOW - DAY), NOW), 1.5);
        assert_eq!(liking(12, 0, None, NOW), 0.5);
        assert_eq!(liking(12, 3, Some(NOW - 31 * DAY), NOW), 0.25);
        let left = Owned { last_watched: Some(NOW - 31 * DAY), ..owned(3, "Left", 12, 3) };
        let library = [owned(1, "Seen", 12, 12), owned(2, "Not started", 12, 0), left];
        let answers = [source(1, &[], &[(100, 50)]), source(2, &[], &[(200, 300)]), source(3, &[], &[(300, 300)])];
        assert_eq!(ids(&rank(&library, &[], &answers, &HashSet::new(), NOW, &Graph::default())), [100, 200, 300]);
        let unwatched = [owned(1, "Seen", 12, 0), owned(2, "Not started", 12, 0)];
        assert_eq!(ids(&rank(&unwatched, &[], &answers, &HashSet::new(), NOW, &Graph::default())), [200, 100]);
    }

    #[test]
    fn what_you_watched_long_ago_counts_less() {
        assert_eq!([0, 6, 12, 36, 60, 120].map(|months| freshness(NOW - months * YEAR_MS / 12, NOW)), [1.0, 1.0, 1.0, 0.5, 0.25, 0.25]);
        assert_eq!(freshness(0, NOW), 1.0);
        // Two shows finished, one this week and one five years ago: the recent one's favourite leads.
        let old = Owned { last_watched: Some(NOW - 5 * YEAR_MS), ..owned(1, "Old", 12, 12) };
        let answers = [source(1, &[], &[(100, 300)]), source(2, &[], &[(200, 100)])];
        let library = [old, owned(2, "New", 12, 12)];
        assert_eq!(ids(&rank(&library, &[], &answers, &HashSet::new(), NOW, &Graph::default())), [200, 100]);
        // On your list, it's when you finished it, or watched it last in the library if that was later.
        let list = [Listed { at: NOW - 5 * YEAR_MS, ..listed(1, "Old", "COMPLETED", 0.0) }, listed(2, "New", "COMPLETED", 0.0)];
        assert_eq!(ids(&rank(&[], &list, &answers, &HashSet::new(), NOW, &Graph::default())), [200, 100]);
        // The next season of an anime you finished long ago has the whole say of it all the same.
        let graph = Graph::new(crate::series::tests::overlord());
        let long_ago = [Owned { last_watched: Some(NOW - 5 * YEAR_MS), ..owned(20832, "Overlord", 13, 13) }];
        let picks = rank(&long_ago, &[], &[], &HashSet::new(), NOW, &graph);
        assert_eq!((picks[0].more_of.as_str(), picks[0].score), ("Overlord", 2.0));
        let rewatched = [owned(1, "Old", 12, 12)];
        assert_eq!(ids(&rank(&rewatched, &list, &answers, &HashSet::new(), NOW, &Graph::default())), [100, 200]);
    }

    fn listed(id: i64, title: &str, status: &str, score: f64) -> Listed {
        Listed { id, title: title.into(), status: status.into(), score, progress: 0, episodes: Some(12), at: 0 }
    }

    #[test]
    fn your_score_and_status_say_how_much_a_show_on_your_list_counts() {
        let at = |score| list_liking(&listed(1, "", "COMPLETED", score), 75.0);
        assert_eq!([at(95.0), at(85.0), at(75.0), at(55.0), at(40.0), at(10.0)], [4.0, 3.0, 2.0, 0.0, -1.5, -2.0].map(Some));
        assert_eq!(at(0.0), Some(2.0));
        assert_eq!(list_liking(&listed(1, "", "DROPPED", 90.0), 75.0), Some(-1.0));
        assert_eq!(list_liking(&Listed { progress: 6, ..listed(1, "", "CURRENT", 0.0) }, 75.0), Some(1.5));
        assert_eq!(list_liking(&Listed { episodes: None, ..listed(1, "", "CURRENT", 0.0) }, 75.0), Some(1.0));
        assert_eq!(list_liking(&listed(1, "", "PAUSED", 0.0), 75.0), Some(0.25));
        // A copy of the list kept before Telly read statuses.
        assert_eq!(list_liking(&listed(1, "", "", 0.0), 75.0), None);
    }

    #[test]
    fn your_anilist_list_says_for_and_against_picks() {
        // Nothing in the library: the picks come from the list alone.
        let list = [listed(1, "Loved", "COMPLETED", 90.0), listed(2, "Fine", "COMPLETED", 60.0), listed(3, "Dropped", "DROPPED", 0.0)];
        let answers = [source(1, &[], &[(100, 50)]), source(2, &[], &[(200, 100), (300, 100)]), source(3, &[], &[(300, 300), (400, 300), (2, 50)])];
        let picks = rank(&[], &list, &answers, &HashSet::new(), NOW, &Graph::default());
        // Your average is 75: Loved counts 3.5, Fine 0.5 and Dropped -1. 300 has
        // Fine's say for it but more of Dropped's against, and 400 only
        // Dropped's: both are left out.
        assert_eq!(ids(&picks), [100, 200]);
        assert_eq!(picks[0].from, [From { title: "Loved".into(), votes: 50 }]);
        // A show in the library and on your list counts as the list says (2, at
        // your average score), by the library's name for it.
        let library = [owned(1, "Loved (in the library)", 12, 0)];
        let picks = rank(&library, &list[..1], &answers, &HashSet::new(), NOW, &Graph::default());
        assert_eq!(picks[0].from, [From { title: "Loved (in the library)".into(), votes: 50 }]);
        assert!((picks[0].score - 2.0 * (50.0 / (50.0 + FEW_VOTES)).sqrt()).abs() < 1e-9);
    }

    #[test]
    fn leaves_out_hidden_and_voted_down_picks_and_shows_since_removed() {
        let library = [owned(1, "Death Note", 37, 0)];
        let answers = [source(1, &[], &[(100, 90), (101, 80), (102, -3), (103, 0)]), source(7, &[], &[(104, 999)])];
        assert_eq!(ids(&rank(&library, &[], &answers, &HashSet::from([100]), NOW, &Graph::default())), [101]);
    }

    #[test]
    fn leaves_out_sequels_and_what_isn_t_out_yet() {
        let mut answer = source(1, &[], &[(100, 90), (101, 80), (102, 70), (103, 60)]);
        answer.likes[0].sequel = true;
        answer.likes[1].unreleased = true;
        let library = [owned(1, "Death Note", 37, 37)];
        assert_eq!(ids(&rank(&library, &[], &[answer.clone()], &HashSet::new(), NOW, &Graph::default())), [102, 103]);
        // Their votes still say how much of Death Note's the rest have.
        let picks = rank(&library, &[], &[answer], &HashSet::new(), NOW, &Graph::default());
        assert!((picks[0].score - 2.0 * (70.0 / (300.0 + FEW_VOTES)).sqrt()).abs() < 1e-9);
    }

    #[test]
    fn what_you_say_of_a_pick_you_leave_out_counts_for_or_against_what_s_like_it() {
        let verdict = |id: i64, verdict: &str| Verdict { id, title: format!("Said {id}"), verdict: verdict.into() };
        let library = [owned(1, "Watched", 12, 12)];
        let answers = [
            source(1, &[], &[(100, 50), (200, 50), (300, 50)]),
            source(10, &[], &[(200, 100), (400, 100)]),
            source(11, &[], &[(300, 100), (500, 100)]),
            source(12, &[], &[(100, 100), (600, 100)]),
        ];
        let listed = said(vec![verdict(10, "liked"), verdict(11, "disliked"), verdict(12, "notForMe"), verdict(1, "disliked")], &library, &[]);
        // The one since in the library says for itself.
        assert_eq!(listed.iter().map(|l| (l.id, l.status.as_str())).collect::<Vec<_>>(), [(10, "COMPLETED"), (11, "DROPPED"), (12, NOT_FOR_ME)]);
        let picks = rank(&library, &listed, &answers, &HashSet::from([10, 11, 12]), NOW, &Graph::default());
        // The pick of the one you liked leads, from both. The one you didn't like says against 300, and
        // Not for me a little against 100, so they come last; what those two recommend alone is left out.
        assert_eq!(ids(&picks), [200, 400, 100, 300]);
        assert_eq!(picks[0].from.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(), ["Said 10", "Watched"]);
        assert_eq!(picks[3].from.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(), ["Watched"]);
        assert!(picks[2].score < 2.0 * (50.0 / (150.0 + FEW_VOTES)).sqrt());
    }

    #[test]
    fn reads_anilists_answer() {
        let answer = json!({ "data": { "Page": { "pageInfo": { "hasNextPage": false }, "media": [{
            "id": 21519,
            "relations": { "edges": [
                { "relationType": "SIDE_STORY", "node": { "id": 97789, "type": "MANGA" } },
                { "relationType": "CHARACTER", "node": { "id": 16782, "type": "ANIME" } },
                { "relationType": "SEQUEL", "node": { "id": 5, "type": "ANIME" } },
            ] },
            "recommendations": { "nodes": [
                { "rating": 6187, "mediaRecommendation": { "id": 20954, "type": "ANIME", "format": "MOVIE", "episodes": 1, "seasonYear": 2016,
                    "genres": ["Drama", "Romance"], "description": "A boy<br>meets a girl.",
                    "title": { "romaji": "Koe no Katachi", "english": "A Silent Voice", "native": "聲の形" },
                    "coverImage": { "extraLarge": "https://s4.anilist.co/a.jpg", "large": "", "color": "#5dbbe4" } } },
                { "rating": 12, "mediaRecommendation": { "id": 30001, "type": "MANGA", "title": { "romaji": "A manga" } } },
                { "rating": 9, "mediaRecommendation": { "id": 20955, "type": "ANIME", "status": "FINISHED", "title": { "romaji": "Season 2" },
                    "relations": { "edges": [{ "relationType": "PREQUEL" }, { "relationType": "SEQUEL" }] } } },
                { "rating": 8, "mediaRecommendation": { "id": 20956, "type": "ANIME", "status": "NOT_YET_RELEASED", "title": { "romaji": "To come" },
                    "relations": { "edges": [{ "relationType": "SEQUEL" }] } } },
                { "rating": 3, "mediaRecommendation": null },
            ] },
        }] } } });
        let found = sources(&answer, 7);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].id, found[0].fetched_at, found[0].related.as_slice()), (21519, 7, &[5][..]));
        assert_eq!(found[0].likes.len(), 3);
        let flags: Vec<(bool, bool)> = found[0].likes.iter().map(|l| (l.sequel, l.unreleased)).collect();
        assert_eq!(flags, [(false, false), (true, false), (false, true)]);
        assert_eq!(found[0].version, KEPT_VERSION);
        let like = &found[0].likes[0];
        assert_eq!((like.votes, like.anime.id, like.anime.short_title().as_str()), (6187, 20954, "A Silent Voice"));
        assert_eq!((like.genres.as_slice(), like.description.as_str()), (&["Drama".to_string(), "Romance".to_string()][..], "A boy<br>meets a girl."));
        // What's kept reads back as it was written.
        let again: Vec<Source> = serde_json::from_slice(&serde_json::to_vec(&found).unwrap()).unwrap();
        assert_eq!(again[0].likes[0].anime, like.anime);
    }
}
