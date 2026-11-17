//! TMDB (https://www.themoviedb.org), for what AniList doesn't know: Japanese
//! dramas, films and other live action. It tells Telly a show's titles,
//! poster and what it's about, and Jimaku finds its subtitles by TMDB's ID
//! ("tv:55925", "movie:505192"). Telly's own read token is built in, so
//! nobody needs a key.

use crate::anilist::likeness;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;

const API: &str = "https://api.themoviedb.org/3";
const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJlYjZiOTYzNTE4Y2IwMWNiZmQ3NThiNGI3YjViZTExMiIsIm5iZiI6MTc5NDkwNjQ1MC43Nzc5OTk5LCJzdWIiOiI2YWZjMTk1MjkyZTRlYjE4MWRmODBhNTciLCJzY29wZXMiOlsiYXBpX3JlYWQiXSwidmVyc2lvbiI6MX0.fNIwh8vu_kQflzfhdjz84fqKjrDXbdqEojnZaI-Oo3E";
const USER_AGENT: &str = concat!("Telly/", env!("CARGO_PKG_VERSION"), " (personal anime player)");
const POSTERS: &str = "https://image.tmdb.org/t/p/w500";
/// TMDB's genre for animation: an anime is AniList's to know, so one isn't taken for a folder unasked.
const ANIMATION: i64 = 16;

/// A series or film on TMDB.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Title {
    /// Its ID as Jimaku writes it: "tv:55925" or "movie:505192".
    pub id: String,
    /// Its own name when it's Japanese (半沢直樹), else its name in Japanese if TMDB has one.
    pub native: String,
    pub english: String,
    /// "TV" or "MOVIE", as AniList's formats are written.
    pub format: String,
    pub episodes: Option<i64>,
    pub year: Option<i64>,
    pub cover: String,
    /// Whether it was made in Japanese.
    pub japanese: bool,
    /// Whether TMDB calls it animation.
    pub animation: bool,
    /// How well it fits the folder (0 to 1), when it came from a search.
    pub score: f64,
}

impl Title {
    fn from_api(v: &Value, kind: &str) -> Option<Title> {
        let kind = v.get("media_type").and_then(Value::as_str).unwrap_or(kind);
        let (name, original, date, format) = match kind {
            "tv" => ("name", "original_name", "first_air_date", "TV"),
            "movie" => ("title", "original_title", "release_date", "MOVIE"),
            _ => return None,
        };
        let text = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string();
        let japanese = text("original_language") == "ja";
        let genres = v.get("genre_ids").and_then(Value::as_array).cloned().unwrap_or_default();
        let detail_genres = v.get("genres").and_then(Value::as_array).map(|g| g.iter().filter_map(|g| g.get("id").cloned()).collect::<Vec<_>>()).unwrap_or_default();
        let poster = text("poster_path");
        Some(Title {
            id: format!("{kind}:{}", v.get("id")?.as_i64()?),
            native: if japanese { text(original) } else { String::new() },
            english: text(name),
            format: format.to_string(),
            episodes: v.get("number_of_episodes").and_then(Value::as_i64).or((kind == "movie").then_some(1)),
            year: text(date).get(..4).and_then(|y| y.parse().ok()),
            cover: if poster.is_empty() { String::new() } else { format!("{POSTERS}{poster}") },
            japanese,
            animation: genres.iter().chain(&detail_genres).any(|g| g.as_i64() == Some(ANIMATION)),
            score: 0.0,
        })
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        [self.english.as_str(), self.native.as_str()].into_iter().filter(|n| !n.is_empty())
    }

    /// The name for cards and the window title, as an anime's is: English if it's short, else Japanese.
    pub fn short_title(&self) -> String {
        if !self.english.is_empty() && (self.english.chars().count() <= 28 || self.native.is_empty()) {
            self.english.clone()
        } else {
            self.native.clone()
        }
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

fn api() -> String {
    std::env::var("TELLY_TMDB_API").unwrap_or_else(|_| API.to_string())
}

fn get(path: &str) -> Result<Value> {
    let url = format!("{}{path}", api());
    let mut response = agent()
        .get(&url)
        .header("Authorization", &format!("Bearer {TOKEN}"))
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| anyhow!("TMDB isn’t reachable ({e})"))?;
    match response.status().as_u16() {
        200 => response.body_mut().read_json().context("TMDB sent something unexpected"),
        404 => Err(anyhow!("TMDB has no such show")),
        status => Err(anyhow!("TMDB didn’t answer (HTTP {status})")),
    }
}

fn encode(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text.trim(), percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The series and films TMDB finds for `text`, best first for a folder of that
/// name with `count` episodes.
pub fn search(text: &str, count: usize) -> Result<Vec<Title>> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let answer = get(&format!("/search/multi?query={}&language=en-US&include_adult=false", encode(&text)))?;
    let mut found: Vec<Title> = answer.get("results").and_then(Value::as_array).into_iter().flatten().filter_map(|v| Title::from_api(v, "")).collect();
    rank(&mut found, &text, count);
    Ok(found)
}

/// Orders search results by how well they fit a folder called `title` with
/// `count` episodes: the name first, then whether it's Japanese, then a film
/// for one video and a series for more.
fn rank(found: &mut [Title], title: &str, count: usize) {
    let n = found.len().max(1) as f64;
    for (i, t) in found.iter_mut().enumerate() {
        let title_fit = t.names().map(|name| likeness(title, name)).fold(0.0, f64::max);
        let format_fit = match (count, t.format.as_str()) {
            (0, _) => 0.5,
            (1, "MOVIE") => 1.0,
            (c, "TV") if c > 1 => 1.0,
            _ => 0.0,
        };
        let order = 1.0 - i as f64 / n;
        t.score = 0.6 * title_fit + 0.2 * if t.japanese { 1.0 } else { 0.0 } + 0.1 * format_fit + 0.1 * order;
    }
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
}

/// The Japanese series or film a folder called `title`, with `count`
/// episodes, fairly surely is: one of its names shares half its words or more
/// with it, as an anime's must (`Anime::is_called`). Animation isn't taken:
/// that's AniList's, and a folder of it AniList didn't know is better
/// matched by hand.
pub fn find(title: &str, count: usize) -> Result<Option<Title>> {
    let found = search(title, count)?;
    let Some(best) = found.into_iter().next() else { return Ok(None) };
    if !best.japanese || best.animation || !best.names().any(|name| likeness(title, name) >= 0.5) {
        return Ok(None);
    }
    by_id(&best.id).map(Some)
}

/// TMDB's path for an ID ("tv:55925" → "/tv/55925").
fn path_of(id: &str) -> Result<String> {
    match id.split_once(':') {
        Some((kind @ ("tv" | "movie"), number)) if number.parse::<i64>().is_ok() => Ok(format!("/{kind}/{number}")),
        _ => Err(anyhow!("“{id}” isn’t a TMDB ID")),
    }
}

/// A series or film by its ID, with its episode count and genres.
pub fn by_id(id: &str) -> Result<Title> {
    let path = path_of(id)?;
    let kind = id.split(':').next().unwrap_or("");
    let answer = get(&format!("{path}?language=en-US"))?;
    Title::from_api(&answer, kind).ok_or_else(|| anyhow!("TMDB has no show {id}"))
}

/// What TMDB says a series or film is about: in English, else in Japanese, else nothing.
pub fn description(id: &str) -> Result<String> {
    let path = path_of(id)?;
    for language in ["en-US", "ja-JP"] {
        let answer = get(&format!("{path}?language={language}"))?;
        let text = answer.get("overview").and_then(Value::as_str).unwrap_or("").trim();
        if !text.is_empty() {
            return Ok(text.to_string());
        }
    }
    Ok(String::new())
}

/// TMDB's page for an ID.
pub fn page(id: &str) -> String {
    let (kind, number) = id.split_once(':').unwrap_or(("tv", id));
    format!("https://www.themoviedb.org/{kind}/{number}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_japanese_drama_found_by_its_english_name_comes_first() {
        let results = json!([
            { "media_type": "movie", "id": 2, "title": "Hanzawa Naoki: Spin-off", "original_title": "半沢直樹Ⅱ エピソードゼロ", "original_language": "ja", "genre_ids": [10770], "release_date": "2020-01-03" },
            { "media_type": "tv", "id": 55925, "name": "Hanzawa Naoki", "original_name": "半沢直樹", "original_language": "ja", "genre_ids": [18, 35], "first_air_date": "2013-07-07", "poster_path": "/p.jpg" },
            { "media_type": "person", "id": 3, "name": "Hanzawa Naoki" },
        ]);
        let mut found: Vec<Title> = results.as_array().unwrap().iter().filter_map(|v| Title::from_api(v, "")).collect();
        rank(&mut found, "Hanzawa Naoki", 10);
        assert_eq!(found.len(), 2);
        let best = &found[0];
        assert_eq!(best.id, "tv:55925");
        assert_eq!(best.native, "半沢直樹");
        assert_eq!(best.format, "TV");
        assert_eq!(best.year, Some(2013));
        assert_eq!(best.cover, "https://image.tmdb.org/t/p/w500/p.jpg");
        assert!(best.japanese && !best.animation);
    }

    #[test]
    fn a_film_of_one_video_beats_a_remake_of_the_same_name() {
        let results = json!([
            { "media_type": "movie", "id": 1, "title": "Shoplifters", "original_title": "Shoplifters", "original_language": "en", "genre_ids": [35] },
            { "media_type": "movie", "id": 505192, "title": "Shoplifters", "original_title": "万引き家族", "original_language": "ja", "genre_ids": [18] },
        ]);
        let mut found: Vec<Title> = results.as_array().unwrap().iter().filter_map(|v| Title::from_api(v, "")).collect();
        rank(&mut found, "Shoplifters", 1);
        assert_eq!(found[0].id, "movie:505192");
        assert_eq!(found[0].episodes, Some(1));
        assert_eq!(found[0].short_title(), "Shoplifters");
    }

    #[test]
    fn ids_are_paths() {
        assert_eq!(path_of("tv:55925").unwrap(), "/tv/55925");
        assert_eq!(path_of("movie:1").unwrap(), "/movie/1");
        assert!(path_of("person:1").is_err());
        assert!(path_of("tv:x").is_err());
        assert_eq!(page("movie:505192"), "https://www.themoviedb.org/movie/505192");
    }
}
