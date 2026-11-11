//! Recommended: anime to watch next, from AniList's community recommendations
//! for the shows in the library ("liked X, try Y", voted up or down). No
//! sign-in is needed: Telly asks about each show by its AniList ID, adds the
//! votes up across the library, and leaves out what the library already has.

use crate::anilist::{self, Anime};
use crate::backend::Backend;
use crate::util::now_ms;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Setting: the AniList IDs of picks hidden from the page.
const HIDDEN: &str = "recommended.hidden";
/// AniList is asked about a show again after a week.
const KEPT_MS: i64 = 7 * 24 * 3600 * 1000;
/// Relations that make two entries the same series: its seasons, films and side stories.
const SAME_SERIES: &[&str] = &["PREQUEL", "SEQUEL", "PARENT", "SIDE_STORY", "SUMMARY", "ALTERNATIVE", "SPIN_OFF", "COMPILATION", "CONTAINS"];

/// What AniList says about one show in the library.
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
}

/// A show in the library that a pick is recommended from.
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
    /// The library's shows it's recommended from, the one that counts most first.
    pub from: Vec<From>,
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
                std::fs::write(&path, serde_json::to_vec(&kept)?)?;
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

/// The picks, best first. Each series in the library has one say on a pick:
/// the most votes any of its entries gives it, counted by their size
/// (log2(1 + votes)), so a pick liked from many shows beats one that a single
/// well-known show carries. A series watched to the end counts double one not
/// started. Left out: what's in the library, the rest of its series, and
/// what's hidden.
pub fn rank(owned: &[Owned], sources: &[Source], hidden: &HashSet<i64>) -> Vec<Pick> {
    let mut series: HashMap<i64, i64> = HashMap::new();
    for o in owned {
        series.entry(o.anilist_id).or_insert(o.anilist_id);
    }
    let sources: Vec<&Source> = sources.iter().filter(|s| owned.iter().any(|o| o.anilist_id == s.id)).collect();
    for s in &sources {
        for &r in &s.related {
            let (a, b) = (series_of(&mut series, s.id), series_of(&mut series, r));
            series.insert(b, a);
        }
    }
    let mut seen: HashMap<i64, (usize, usize)> = HashMap::new();
    for o in owned {
        let of = seen.entry(series_of(&mut series, o.anilist_id)).or_default();
        *of = (of.0 + o.episodes, of.1 + o.watched);
    }
    // For each pick, each series' say: the entry that gives it the most votes.
    let mut says: HashMap<i64, (&Like, HashMap<i64, (i64, &str)>)> = HashMap::new();
    for s in &sources {
        let from = series_of(&mut series, s.id);
        let title = owned.iter().find(|o| o.anilist_id == s.id).map_or("", |o| o.title.as_str());
        for like in s.likes.iter().filter(|l| l.votes > 0 && !series.contains_key(&l.anime.id) && !hidden.contains(&l.anime.id)) {
            let say = says.entry(like.anime.id).or_insert_with(|| (like, HashMap::new())).1.entry(from).or_insert((0, title));
            if like.votes > say.0 {
                *say = (like.votes, title);
            }
        }
    }
    let mut picks: Vec<Pick> = says
        .into_values()
        .map(|(like, by_series)| {
            let mut from: Vec<(f64, From)> = by_series
                .into_iter()
                .map(|(root, (votes, title))| {
                    let (episodes, watched) = seen.get(&root).copied().unwrap_or_default();
                    let weight = 1.0 + if episodes > 0 { watched as f64 / episodes as f64 } else { 0.0 };
                    (weight * (1.0 + votes as f64).log2(), From { title: title.to_string(), votes })
                })
                .collect();
            from.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
            Pick {
                title: like.anime.short_title(),
                anime: like.anime.clone(),
                genres: like.genres.clone(),
                description: like.description.clone(),
                score: from.iter().map(|f| f.0).sum(),
                from: from.into_iter().map(|f| f.1).collect(),
            }
        })
        .collect();
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

/// The Recommended page: the picks, and how many of the library's shows they come from.
pub fn view(backend: &Backend) -> Result<Value> {
    let mut owned = Vec::new();
    for show in backend.db.shows()? {
        if let Some(anilist_id) = show.anilist_id {
            let episodes = backend.db.episodes(show.id)?;
            owned.push(Owned { anilist_id, title: show.title, episodes: episodes.len(), watched: episodes.iter().filter(|e| e.watched_at.is_some()).count() });
        }
    }
    let mut ids: Vec<i64> = owned.iter().map(|o| o.anilist_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let sources = if ids.is_empty() { Vec::new() } else { sources_for(backend, &ids)? };
    Ok(json!({ "shows": owned.len(), "picks": rank(&owned, &sources, &hidden(backend)) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(anilist_id: i64, title: &str, episodes: usize, watched: usize) -> Owned {
        Owned { anilist_id, title: title.into(), episodes, watched }
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
    fn many_shows_beat_one_well_known_show() {
        let library = [owned(1, "Your Name.", 1, 0), owned(2, "Clannad", 23, 0), owned(3, "Angel Beats!", 13, 0), owned(4, "K-ON!", 13, 0)];
        let answers = [source(1, &[], &[(100, 6000), (101, 150)]), source(2, &[], &[(101, 120)]), source(3, &[], &[(101, 90)]), source(4, &[], &[(101, 60)])];
        let picks = rank(&library, &answers, &HashSet::new());
        assert_eq!(ids(&picks), [101, 100]);
        let from: Vec<(&str, i64)> = picks[0].from.iter().map(|f| (f.title.as_str(), f.votes)).collect();
        assert_eq!(from, [("Your Name.", 150), ("Clannad", 120), ("Angel Beats!", 90), ("K-ON!", 60)]);
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
            source(9, &[], &[(200, 500), (1, 40)]),
        ];
        let picks = rank(&library, &answers, &HashSet::new());
        // 100 has 900 votes from the one series, 200 has 500 from another show: one say each.
        assert_eq!(ids(&picks), [200, 100, 5]);
        assert_eq!(picks[1].from, [From { title: "Titan".into(), votes: 400 }]);
    }

    #[test]
    fn watched_shows_count_for_more() {
        let library = [owned(1, "Seen", 12, 12), owned(2, "Not started", 12, 0)];
        let answers = [source(1, &[], &[(100, 50)]), source(2, &[], &[(200, 300)])];
        // log2(51) doubled is 11.3; log2(301) is 8.2.
        assert_eq!(ids(&rank(&library, &answers, &HashSet::new())), [100, 200]);
        let unwatched = [owned(1, "Seen", 12, 0), owned(2, "Not started", 12, 0)];
        assert_eq!(ids(&rank(&unwatched, &answers, &HashSet::new())), [200, 100]);
    }

    #[test]
    fn leaves_out_hidden_and_voted_down_picks_and_shows_since_removed() {
        let library = [owned(1, "Death Note", 37, 0)];
        let answers = [source(1, &[], &[(100, 90), (101, 80), (102, -3), (103, 0)]), source(7, &[], &[(104, 999)])];
        assert_eq!(ids(&rank(&library, &answers, &HashSet::from([100]))), [101]);
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
