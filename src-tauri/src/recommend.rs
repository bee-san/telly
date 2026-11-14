//! Recommended: anime to watch next, from AniList's community recommendations
//! for the shows in the library and, once you've signed in to AniList, those
//! on your list there ("liked X, try Y", voted up or down). Telly asks about
//! each show by its AniList ID, weighs each show's votes by how much you liked
//! it (your score and status on your list, or else how much of it you've
//! watched) and adds them up, and leaves out what you have or have started.
//! The seasons and movies you don't have of an anime you do are picks too,
//! by how much you liked the parts you have (series.rs has which they are).
//!
//! The page also lists what's trending on AniList, the ten series and the
//! ten movies most watched and talked about there now (asked once a day, kept
//! in trending.json), and searches AniList for an anime that's neither a pick
//! nor trending, so anything can be found to download.

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

/// Setting: the AniList IDs of picks hidden from the page.
const HIDDEN: &str = "recommended.hidden";
/// AniList is asked about a show again after a week.
const KEPT_MS: i64 = 7 * 24 * 3600 * 1000;
/// A pick's share of a show's votes is counted as if the show had this many
/// more, for none of its picks, so a show with only a handful of votes has a
/// smaller say.
const FEW_VOTES: f64 = 50.0;
/// A show started and not finished, with nothing of it watched for 30 days, was left.
const LEFT_MS: i64 = 30 * 24 * 3600 * 1000;

/// What AniList says about one show in the library or on your list.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub id: i64,
    pub fetched_at: i64,
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
    #[serde(skip)]
    score: f64,
}

fn query() -> String {
    format!(
        "query ($ids: [Int], $page: Int) {{ Page(page: $page, perPage: 50) {{ pageInfo {{ hasNextPage }}
  media(id_in: $ids, type: ANIME) {{ id
    relations {{ edges {{ relationType(version: 2) node {{ id type }} }} }}
    recommendations(sort: RATING_DESC, perPage: 25) {{ nodes {{ rating
      mediaRecommendation {{ type genres description(asHtml: false) {} }} }} }} }} }} }}",
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
                    })
                })
                .collect();
            Some(Source { id: media.get("id")?.as_i64()?, fetched_at: now, related, likes })
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
            found.push(Source { id: *id, fetched_at: now, ..Default::default() });
        }
    }
    Ok(found)
}

fn kept_path(backend: &Backend) -> PathBuf {
    backend.dirs.local.join("recommended.json")
}

/// What AniList says about each of `ids`: kept answers, and fresh ones for
/// shows it hasn't been asked about this week.
fn sources_for(backend: &Backend, ids: &[i64]) -> Result<Vec<Source>> {
    // One asking at a time: a page opened while `gather` is asking waits for its answers rather than asking again.
    static ASKING: Mutex<()> = Mutex::new(());
    let _asking = ASKING.lock();
    let path = kept_path(backend);
    let mut kept: Vec<Source> = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    kept.retain(|s| ids.contains(&s.id));
    let now = now_ms();
    let old: Vec<i64> = ids.iter().copied().filter(|id| !kept.iter().any(|s| s.id == *id && now - s.fetched_at < KEPT_MS)).collect();
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

/// How much your liking an anime on your AniList list counts for, from its
/// status and your score, or None when the list doesn't say: dropped -1;
/// scored, 2 for your average score and 1 more or less for each 10 points
/// above or below it (-2 to 4); else completed 2, watching 1 to 2 by how far
/// you are, and paused 0.25.
fn list_liking(entry: &Listed, average_score: f64) -> Option<f64> {
    match entry.status.as_str() {
        "DROPPED" => Some(-1.0),
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
/// the entry counts. So a little-known show's favourite counts as much as a
/// famous show's, and a pick liked from many shows still beats one show's
/// favourite. A show you dropped or scored low says against its picks, and a
/// pick it says more against than the rest say for is left out. Also left
/// out: what's in the library or on your list, and what's hidden.
///
/// The rest of an anime you have is picked too (`graph` has which entries
/// those are): its seasons and movies you don't have, each as that anime's
/// own favourite, with the whole say of the part you liked most. Of several,
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
    // How much your liking each show counts for, and its title: as your list says, or else by how much of it you've watched.
    let mut shows: HashMap<i64, (f64, &str)> =
        watching.into_iter().map(|(id, (episodes, watched, last_watched, title))| (id, (liking(episodes, watched, last_watched, now), title))).collect();
    let scores: Vec<f64> = listed.iter().map(|l| l.score).filter(|&s| s > 0.0).collect();
    let average_score = scores.iter().sum::<f64>() / scores.len().max(1) as f64;
    for entry in listed {
        if let Some(liking) = list_liking(entry, average_score) {
            let title = shows.get(&entry.id).map_or(entry.title.as_str(), |s| s.1);
            shows.insert(entry.id, (liking, title));
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
        let (liking, title) = shows[&s.id];
        // Its votes, but for the rest of its own series: those aren't recommendations.
        let mut votes = 0;
        for like in s.likes.iter().filter(|l| l.votes > 0) {
            if !series.contains_key(&like.anime.id) || series_of(&mut series, like.anime.id) != from {
                votes += like.votes;
            }
        }
        for like in s.likes.iter().filter(|l| l.votes > 0 && !series.contains_key(&l.anime.id) && !hidden.contains(&l.anime.id)) {
            let say = liking * (like.votes as f64 / (votes as f64 + FEW_VOTES)).sqrt();
            let best = says.entry(like.anime.id).or_insert_with(|| (like, HashMap::new())).1.entry(from).or_insert((f64::NEG_INFINITY, 0, title));
            if say > best.0 {
                *best = (say, like.votes, title);
            }
        }
    }
    let mut picks: Vec<Pick> = says
        .into_values()
        .map(|(like, by_series)| {
            let mut from: Vec<(f64, From)> = by_series.into_values().map(|(say, votes, title)| (say, From { title: title.to_string(), votes })).collect();
            from.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
            Pick {
                title: like.anime.short_title(),
                anime: like.anime.clone(),
                genres: like.genres.clone(),
                description: like.description.clone(),
                score: from.iter().map(|f| f.0).sum(),
                // Only the shows that say for it: it isn't recommended from one you dropped.
                from: from.into_iter().filter(|f| f.0 > 0.0).map(|f| f.1).collect(),
                more_of: String::new(),
                show: None,
                unreleased: false,
            }
        })
        .filter(|p| p.score > 0.0)
        .collect();
    // The seasons and movies you don't have of each anime you do.
    let holds_more: HashSet<i64> = owned.iter().filter(|o| o.entry_episodes.is_some_and(|e| o.episodes as i64 > e)).filter_map(|o| graph.root(o.anilist_id)).collect();
    for &root in yours.iter().filter(|root| !holds_more.contains(root)) {
        let parts = graph.parts(root);
        // The part of it you liked most says how much the rest is worth to you.
        let Some((liking, title)) = parts.iter().filter_map(|e| shows.get(&e.anime.id)).max_by(|a, b| a.0.total_cmp(&b.0)).copied() else { continue };
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
                score: liking / (nth + 1) as f64,
            });
        }
    }
    picks.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.anime.id.cmp(&b.anime.id)));
    picks
}

fn hidden(backend: &Backend) -> HashSet<i64> {
    backend.db.setting(HIDDEN).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// Leaves a pick out from now on (one seen elsewhere, say).
pub fn hide(backend: &Backend, id: i64) -> Result<()> {
    let mut ids = hidden(backend);
    ids.insert(id);
    backend.db.set_setting(HIDDEN, &json!(ids))
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

/// AniList is asked what's trending again after a day.
const TRENDING_MS: i64 = 24 * 3600 * 1000;

/// What's trending on AniList: the ten series and the ten movies most watched and talked about there now.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Trending {
    fetched_at: i64,
    series: Vec<Like>,
    movies: Vec<Like>,
}

/// One anime of a list AniList answers with, and whether it's still to come out.
fn listed_anime(media: &Value) -> Option<(Like, bool)> {
    let like = Like {
        votes: 0,
        anime: Anime::from_api(media)?,
        genres: list(media, "/genres").iter().filter_map(Value::as_str).map(str::to_string).collect(),
        description: media.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
    };
    Some((like, media.get("status").and_then(Value::as_str) == Some("NOT_YET_RELEASED")))
}

/// The ten most trending of one of the answer's lists that are out: one still to come has nothing to download.
fn top_ten(answer: &Value, name: &str) -> Vec<Like> {
    list(answer, &format!("/data/{name}/media")).iter().filter_map(listed_anime).filter(|(_, unreleased)| !unreleased).map(|(like, _)| like).take(10).collect()
}

fn fetch_trending() -> Result<Trending> {
    let page = |format: &str| format!("Page(perPage: 25) {{ media(type: ANIME, format: {format}, sort: TRENDING_DESC, isAdult: false) {{ genres description(asHtml: false) {} }} }}", anilist::FIELDS);
    let q = format!("query {{ series: {} movies: {} }}", page("TV"), page("MOVIE"));
    let answer = anilist::query_patiently(&q, json!({}))?;
    Ok(Trending { fetched_at: now_ms(), series: top_ten(&answer, "series"), movies: top_ten(&answer, "movies") })
}

/// What's trending: the kept answer while it's under a day old, else a fresh one, else (offline) the kept one however old.
fn trending(backend: &Backend) -> Trending {
    static ASKING: Mutex<()> = Mutex::new(());
    let _asking = ASKING.lock();
    let path = backend.dirs.local.join("trending.json");
    let kept: Trending = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    if now_ms() - kept.fetched_at < TRENDING_MS {
        return kept;
    }
    match fetch_trending() {
        Ok(fresh) => {
            if let Ok(bytes) = serde_json::to_vec(&fresh) {
                let _ = crate::util::write_whole(&path, bytes);
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

/// A pick that's listed, not recommended: one that's trending, or found by a search.
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
        score: 0.0,
    }
}

/// The anime AniList finds for `text`, for downloading one that's neither a
/// pick nor trending: each with the show in your library it is, when you
/// have it, and whether it's still to come out.
pub fn search(backend: &Backend, text: &str) -> Result<Vec<Pick>> {
    let q = format!("query ($search: String) {{ Page(perPage: 20) {{ media(search: $search, type: ANIME, sort: SEARCH_MATCH) {{ genres description(asHtml: false) {} }} }} }}", anilist::FIELDS);
    let answer = anilist::query_patiently(&q, json!({ "search": text.split_whitespace().collect::<Vec<_>>().join(" ") }))?;
    let have = in_library(backend)?;
    Ok(list(&answer, "/data/Page/media").iter().filter_map(listed_anime).map(|(like, unreleased)| listed_pick(&like, unreleased, &have)).collect())
}

/// The Recommended page: the picks, and how many shows, in the library or on
/// your list, they come from; and the series and movies trending on AniList.
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
    let listed = crate::anilist_list::listed(backend);
    // A copy of the list kept before Telly read its statuses only leaves its anime out.
    let mut ids: Vec<i64> = owned.iter().map(|o| o.anilist_id).chain(listed.iter().filter(|l| !l.status.is_empty()).map(|l| l.id)).collect();
    ids.sort_unstable();
    ids.dedup();
    let sources = if ids.is_empty() { Vec::new() } else { sources_for(backend, &ids)? };
    let mut left_out = hidden(backend);
    left_out.extend(listed.iter().map(|l| l.id));
    let trending = trending(backend);
    let have = in_library(backend)?;
    let listed_picks = |likes: &[Like]| likes.iter().map(|like| listed_pick(like, false, &have)).collect::<Vec<_>>();
    Ok(json!({
        "shows": ids.len(),
        "picks": rank(&owned, &listed, &sources, &left_out, now_ms(), &crate::series::graph(backend)),
        "series": listed_picks(&trending.series),
        "movies": listed_picks(&trending.movies),
    }))
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
            related: related.to_vec(),
            likes: likes.iter().map(|&(to, votes)| Like { votes, anime: Anime { id: to, romaji: format!("Anime {to}"), ..Default::default() }, ..Default::default() }).collect(),
        }
    }

    fn ids(picks: &[Pick]) -> Vec<i64> {
        picks.iter().map(|p| p.anime.id).collect()
    }

    #[test]
    fn the_top_ten_trending_are_the_first_ten_that_are_out() {
        let media: Vec<Value> = (1..=14)
            .map(|id| json!({ "id": id, "format": "TV", "status": if id == 2 || id == 5 { "NOT_YET_RELEASED" } else { "RELEASING" }, "title": { "romaji": format!("Anime {id}") }, "genres": ["Drama"], "description": "About it." }))
            .collect();
        let answer = json!({ "data": { "series": { "media": media }, "movies": { "media": [] } } });
        let top = top_ten(&answer, "series");
        assert_eq!(top.iter().map(|l| l.anime.id).collect::<Vec<_>>(), [1, 3, 4, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!((top[0].genres.as_slice(), top[0].description.as_str()), (&["Drama".to_string()][..], "About it."));
        assert!(top_ten(&answer, "movies").is_empty());
        // One that's trending and in the library says which show it is.
        let pick = listed_pick(&top[1], false, &HashMap::from([(3, 42)]));
        assert_eq!((pick.show, pick.unreleased, pick.title.as_str()), (Some(42), false, "Anime 3"));
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
        let dropped = [Listed { id: 20832, title: "Overlord".into(), status: "DROPPED".into(), score: 0.0, progress: 3, episodes: Some(13) }];
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
        assert_eq!(ids(&rank(&library, &[], &answers, &HashSet::new(), NOW, &Graph::default())), [100, 200, 102, 101, 201]);
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

    fn listed(id: i64, title: &str, status: &str, score: f64) -> Listed {
        Listed { id, title: title.into(), status: status.into(), score, progress: 0, episodes: Some(12) }
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
                { "rating": 3, "mediaRecommendation": null },
            ] },
        }] } } });
        let found = sources(&answer, 7);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].id, found[0].fetched_at, found[0].related.as_slice()), (21519, 7, &[5][..]));
        assert_eq!(found[0].likes.len(), 1);
        let like = &found[0].likes[0];
        assert_eq!((like.votes, like.anime.id, like.anime.short_title().as_str()), (6187, 20954, "A Silent Voice"));
        assert_eq!((like.genres.as_slice(), like.description.as_str()), (&["Drama".to_string(), "Romance".to_string()][..], "A boy<br>meets a girl."));
        // What's kept reads back as it was written.
        let again: Vec<Source> = serde_json::from_slice(&serde_json::to_vec(&found).unwrap()).unwrap();
        assert_eq!(again[0].likes[0].anime, like.anime);
    }
}
