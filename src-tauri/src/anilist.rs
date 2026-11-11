//! AniList (https://anilist.co), to know which show a folder is: its titles,
//! episode count and cover. Free, and no key is needed. Jimaku finds
//! subtitles by AniList's ID, and Tidepool logs it.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

const API: &str = "https://graphql.anilist.co";
const USER_AGENT: &str = concat!("Telly/", env!("CARGO_PKG_VERSION"), " (personal anime player)");

pub(crate) const FIELDS: &str = "id format episodes seasonYear status synonyms
  title { romaji english native }
  coverImage { extraLarge large color }";

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

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Anime {
    pub id: i64,
    pub romaji: String,
    pub english: String,
    pub native: String,
    pub synonyms: Vec<String>,
    pub format: String,
    pub episodes: Option<i64>,
    pub year: Option<i64>,
    pub cover: String,
    pub color: String,
    /// How well it fits the folder (0 to 1), when it came from a search.
    pub score: f64,
}

impl Anime {
    pub(crate) fn from_api(v: &Value) -> Option<Anime> {
        let text = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
        Some(Anime {
            id: v.get("id")?.as_i64()?,
            romaji: text("/title/romaji"),
            english: text("/title/english"),
            native: text("/title/native"),
            synonyms: v
                .get("synonyms")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default(),
            format: text("/format"),
            episodes: v.get("episodes").and_then(Value::as_i64),
            year: v.get("seasonYear").and_then(Value::as_i64),
            cover: {
                let xl = text("/coverImage/extraLarge");
                if xl.is_empty() { text("/coverImage/large") } else { xl }
            },
            color: text("/coverImage/color"),
            score: 0.0,
        })
    }

    /// The name for cards and the window title: English if it has one and it's
    /// short, else romaji.
    pub fn short_title(&self) -> String {
        if !self.english.is_empty() && self.english.chars().count() <= 28 {
            self.english.clone()
        } else if !self.romaji.is_empty() {
            self.romaji.clone()
        } else {
            self.native.clone()
        }
    }
}

fn query(q: &str, variables: Value) -> Result<Value> {
    let body = json!({ "query": q, "variables": variables });
    let mut response = agent()
        .post(API)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json")
        .send_json(&body)
        .map_err(|e| anyhow!("AniList isn’t reachable ({e})"))?;
    let status = response.status().as_u16();
    if status == 429 {
        return Err(anyhow!("AniList asked Telly to slow down. Try again in a minute."));
    }
    let answer: Value = response.body_mut().with_config().limit(8 * 1024 * 1024).read_json().context("AniList sent something unexpected")?;
    if let Some(error) = answer.pointer("/errors/0/message").and_then(Value::as_str) {
        return Err(anyhow!("AniList: {error}"));
    }
    Ok(answer)
}

/// `query`, waiting out AniList's rate limit (90 requests a minute, less
/// when it's busy) for the many pages of a long show's cast.
pub fn query_patiently(q: &str, variables: Value) -> Result<Value> {
    let body = json!({ "query": q, "variables": variables });
    for attempt in 0..6u32 {
        let mut response = agent()
            .post(API)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json")
            .send_json(&body)
            .map_err(|e| anyhow!("AniList isn’t reachable ({e})"))?;
        if response.status().as_u16() == 429 {
            let wait = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
            std::thread::sleep(Duration::from_secs(wait.unwrap_or(5 << attempt).clamp(2, 90)));
            continue;
        }
        let answer: Value = response.body_mut().with_config().limit(16 * 1024 * 1024).read_json().context("AniList sent something unexpected")?;
        if let Some(error) = answer.pointer("/errors/0/message").and_then(Value::as_str) {
            return Err(anyhow!("AniList: {error}"));
        }
        return Ok(answer);
    }
    Err(anyhow!("AniList kept asking Telly to slow down. It will try again later."))
}

/// Anime matching `text`, AniList's best match first.
pub fn search(text: &str) -> Result<Vec<Anime>> {
    let q = format!("query ($search: String) {{ Page(perPage: 10) {{ media(search: $search, type: ANIME, sort: SEARCH_MATCH) {{ {FIELDS} }} }} }}");
    let answer = query(&q, json!({ "search": text }))?;
    Ok(answer
        .pointer("/data/Page/media")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Anime::from_api).collect())
        .unwrap_or_default())
}

pub fn by_id(id: i64) -> Result<Anime> {
    let q = format!("query ($id: Int) {{ Media(id: $id, type: ANIME) {{ {FIELDS} }} }}");
    let answer = query(&q, json!({ "id": id }))?;
    answer.pointer("/data/Media").and_then(Anime::from_api).ok_or_else(|| anyhow!("AniList has no anime {id}"))
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// How alike two titles are, 0 to 1: shared words over all words, with a bonus when one contains the other.
fn likeness(a: &str, b: &str) -> f64 {
    let (wa, wb) = (words(a), words(b));
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let shared = wa.iter().filter(|w| wb.contains(w)).count() as f64;
    let jaccard = shared / (wa.len() + wb.len()) as f64 * 2.0;
    let (ja, jb) = (wa.join(" "), wb.join(" "));
    if ja == jb {
        1.0
    } else if ja.contains(&jb) || jb.contains(&ja) {
        jaccard.max(0.8)
    } else {
        jaccard
    }
}

/// The format a folder's name asks for: "OVA", "MOVIE", "SPECIAL" or "ONA", else TV.
fn format_hint(title: &str) -> &'static str {
    let words = words(title);
    let has = |w: &[&str]| words.iter().any(|x| w.contains(&x.as_str()));
    if has(&["ova", "oad", "ovas", "oads"]) {
        "OVA"
    } else if has(&["movie", "film", "gekijouban", "劇場版"]) {
        "MOVIE"
    } else if has(&["special", "specials", "sp"]) {
        "SPECIAL"
    } else if has(&["ona"]) {
        "ONA"
    } else {
        "TV"
    }
}

/// Searches, and if nothing comes back, searches again with fewer words
/// ("Fairy Tail OVA Collection" → "Fairy Tail OVA" → "Fairy Tail").
pub fn search_loosely(text: &str) -> Result<Vec<Anime>> {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    loop {
        let found = search(&words.join(" "))?;
        if !found.is_empty() || words.len() <= 1 {
            return Ok(found);
        }
        words.pop();
    }
}

/// Orders search results by how well they fit a folder called `title` with `count` episodes.
pub fn rank(results: &mut [Anime], title: &str, count: usize) {
    let n = results.len().max(1) as f64;
    let wanted = format_hint(title);
    for (i, a) in results.iter_mut().enumerate() {
        let names = [a.romaji.as_str(), a.english.as_str(), a.native.as_str()]
            .into_iter()
            .chain(a.synonyms.iter().map(String::as_str));
        let title_fit = names.map(|name| likeness(title, name)).fold(0.0, f64::max);
        let format_fit = match (wanted, a.format.as_str()) {
            (w, f) if w == f => 1.0,
            ("TV", "TV_SHORT" | "ONA") => 0.6,
            ("TV", "OVA" | "SPECIAL") => 0.3,
            ("OVA", "SPECIAL" | "ONA") | ("SPECIAL", "OVA" | "ONA") => 0.6,
            _ => 0.0,
        };
        let episode_fit = match a.episodes {
            Some(e) if count > 0 && e as usize == count => 1.0,
            Some(e) if count > 0 && e as usize > count => 0.6,
            Some(e) if count > 0 => (e as f64 / count as f64).min(1.0) * 0.5,
            _ => 0.3,
        };
        let order = 1.0 - i as f64 / n;
        a.score = 0.55 * title_fit + 0.15 * format_fit + 0.2 * episode_fit + 0.1 * order;
    }
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
}

/// An image from AniList's CDN (a cover or a character's portrait).
pub fn image(url: &str) -> Result<Vec<u8>> {
    let mut response = agent().get(url).header("User-Agent", USER_AGENT).call().map_err(|e| anyhow!("couldn’t get the image ({e})"))?;
    if !response.status().is_success() {
        return Err(anyhow!("AniList’s image server answered {}", response.status()));
    }
    Ok(response.body_mut().with_config().limit(16 * 1024 * 1024).read_to_vec()?)
}

/// Saves a cover image (for the library and the show page).
pub fn download_cover(url: &str, path: &Path) -> Result<()> {
    if url.is_empty() {
        return Err(anyhow!("no cover"));
    }
    let bytes = image(url)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anime(id: i64, romaji: &str, english: &str, format: &str, episodes: Option<i64>) -> Anime {
        Anime { id, romaji: romaji.into(), english: english.into(), format: format.into(), episodes, ..Default::default() }
    }

    #[test]
    fn ranks_by_title_format_and_episodes() {
        let mut results = vec![
            anime(1, "NARUTO: Shippuuden", "Naruto Shippuden", "TV", Some(500)),
            anime(2, "NARUTO", "Naruto", "TV", Some(220)),
            anime(3, "NARUTO: Akaki Yotsuba no Clover wo Sagase", "", "SPECIAL", Some(1)),
        ];
        rank(&mut results, "NARUTO", 26);
        assert_eq!(results[0].id, 2);
        let mut ft = vec![anime(9, "FAIRY TAIL (2014)", "Fairy Tail Series 2", "TV", Some(102)), anime(6702, "FAIRY TAIL", "Fairy Tail", "TV", Some(175))];
        rank(&mut ft, "Fairy Tail", 175);
        assert_eq!(ft[0].id, 6702);
        let mut ova = vec![anime(6702, "FAIRY TAIL", "Fairy Tail", "TV", Some(175)), anime(10087, "FAIRY TAIL OVA", "Fairy Tail OVA", "OVA", Some(9))];
        rank(&mut ova, "Fairy Tail OVA", 9);
        assert_eq!(ova[0].id, 10087);
    }

    #[test]
    fn picks_a_short_title() {
        let a = anime(1, "Shirokuma Cafe", "Polar Bear Cafe", "TV", Some(50));
        assert_eq!(a.short_title(), "Polar Bear Cafe");
        let b = anime(2, "Sousou no Frieren", "Frieren: Beyond Journey’s End", "TV", Some(28));
        assert_eq!(b.short_title(), "Sousou no Frieren");
    }
}
