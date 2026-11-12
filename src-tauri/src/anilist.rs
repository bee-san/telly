//! AniList (https://anilist.co), to know which show a folder is: its titles,
//! episode count and cover. Free, and no key is needed. Jimaku finds
//! subtitles by AniList's ID, and Tidepool logs it. Signed in, Telly also
//! puts what you watch on your AniList list (anilist_list.rs).

use anyhow::{anyhow, Context, Result};
use regex::Regex;
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
    query_as(q, variables, None)
}

/// `query`, as the user whose access token `token` is.
fn query_as(q: &str, variables: Value, token: Option<&str>) -> Result<Value> {
    let body = json!({ "query": q, "variables": variables });
    let mut request = agent().post(API).header("User-Agent", USER_AGENT).header("Accept", "application/json");
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let mut response = request.send_json(&body).map_err(|e| anyhow!("AniList isn’t reachable ({e})"))?;
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

/// Anime matching `text`, AniList's best match first. A season in it
/// ("Season 2") isn't searched for: AniList finds every season by the show's
/// name, and the words throw it ("K-On! Season 1" finds other shows). `rank`
/// picks the season out.
pub fn search(text: &str) -> Result<Vec<Anime>> {
    static SEASON: OnceLock<Regex> = OnceLock::new();
    let season = SEASON.get_or_init(|| Regex::new(r"(?i)\bseason\s*\d+\b").expect("valid pattern"));
    let text = season.replace_all(text, " ");
    let q = format!("query ($search: String) {{ Page(perPage: 10) {{ media(search: $search, type: ANIME, sort: SEARCH_MATCH) {{ {FIELDS} }} }} }}");
    let answer = query(&q, json!({ "search": text.split_whitespace().collect::<Vec<_>>().join(" ") }))?;
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

/// The name of the user whose access token `token` is.
pub fn viewer(token: &str) -> Result<String> {
    let answer = query_as("query { Viewer { name } }", json!({}), Some(token))?;
    answer.pointer("/data/Viewer/name").and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("AniList didn’t say who this token is for"))
}

/// The anime on the user's list that they've started: all but those they only plan to watch.
pub fn watched_ids(token: &str) -> Result<Vec<i64>> {
    let viewer = query_as("query { Viewer { id } }", json!({}), Some(token))?;
    let user = viewer.pointer("/data/Viewer/id").and_then(Value::as_i64).ok_or_else(|| anyhow!("AniList didn’t say who this token is for"))?;
    let q = "query ($user: Int, $chunk: Int) { MediaListCollection(userId: $user, type: ANIME, status_not: PLANNING, chunk: $chunk, perChunk: 500) {
      hasNextChunk lists { entries { mediaId } } } }";
    let mut ids = Vec::new();
    for chunk in 1.. {
        let answer = query_as(q, json!({ "user": user, "chunk": chunk }), Some(token))?;
        for list in answer.pointer("/data/MediaListCollection/lists").and_then(Value::as_array).into_iter().flatten() {
            ids.extend(list.get("entries").and_then(Value::as_array).into_iter().flatten().filter_map(|e| e.get("mediaId").and_then(Value::as_i64)));
        }
        if answer.pointer("/data/MediaListCollection/hasNextChunk").and_then(Value::as_bool) != Some(true) {
            break;
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// An anime on the user's list: its episode count (if AniList knows it),
/// and the entry's status and progress (None when it isn't on the list).
pub struct ListEntry {
    pub episodes: Option<i64>,
    pub status: Option<String>,
    pub progress: i64,
}

pub fn list_entry(token: &str, id: i64) -> Result<ListEntry> {
    let q = "query ($id: Int) { Media(id: $id, type: ANIME) { episodes mediaListEntry { status progress } } }";
    let answer = query_as(q, json!({ "id": id }), Some(token))?;
    let media = answer.pointer("/data/Media").ok_or_else(|| anyhow!("AniList has no anime {id}"))?;
    Ok(ListEntry {
        episodes: media.get("episodes").and_then(Value::as_i64),
        status: media.pointer("/mediaListEntry/status").and_then(Value::as_str).map(str::to_string),
        progress: media.pointer("/mediaListEntry/progress").and_then(Value::as_i64).unwrap_or(0),
    })
}

/// Sets how many episodes of anime `id` the user has watched, and its status
/// (CURRENT, COMPLETED, REPEATING…).
pub fn save_progress(token: &str, id: i64, progress: i64, status: &str) -> Result<()> {
    let q = "mutation ($id: Int, $progress: Int, $status: MediaListStatus) {
      SaveMediaListEntry(mediaId: $id, progress: $progress, status: $status) { id } }";
    query_as(q, json!({ "id": id, "progress": progress, "status": status }), Some(token))?;
    Ok(())
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

/// The season a title names: "Season 2", "2nd Season", "Second Season", or a
/// number ending it ("Konosuba 2", "Overlord II", but not "Part 2").
fn season(title: &str) -> Option<i64> {
    let words = words(title);
    let ordinal = |w: &str| -> Option<i64> {
        match w {
            "second" => Some(2),
            "third" => Some(3),
            "fourth" => Some(4),
            "fifth" => Some(5),
            _ => w.strip_suffix("st").or(w.strip_suffix("nd")).or(w.strip_suffix("rd")).or(w.strip_suffix("th"))?.parse().ok(),
        }
    };
    for i in (0..words.len()).filter(|&i| words[i] == "season") {
        if let Some(n) = words.get(i + 1).and_then(|n| n.parse().ok()) {
            return Some(n);
        }
        if let Some(n) = i.checked_sub(1).and_then(|p| ordinal(&words[p])) {
            return Some(n);
        }
    }
    let (last, before) = (words.last()?, words.len().checked_sub(2).map(|i| words[i].as_str()));
    match last.as_str() {
        _ if before == Some("part") => None,
        "ii" => Some(2),
        "iii" => Some(3),
        "iv" => Some(4),
        n if n.len() <= 2 => n.parse().ok(),
        _ => None,
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
    let wanted_season = season(title);
    for (i, a) in results.iter_mut().enumerate() {
        let names = || {
            [a.romaji.as_str(), a.english.as_str(), a.native.as_str()]
                .into_iter()
                .chain(a.synonyms.iter().map(String::as_str))
        };
        let title_fit = names().map(|name| likeness(title, name)).fold(0.0, f64::max);
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
        let score = 0.55 * title_fit + 0.15 * format_fit + 0.2 * episode_fit + 0.1 * order;
        // A folder that names its season wants the entry for that season. A
        // first season's titles rarely name it.
        a.score = match wanted_season {
            Some(s) => 0.8 * score + if names().find_map(season).unwrap_or(1) == s { 0.2 } else { 0.0 },
            None => score,
        };
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
    fn a_season_folder_gets_that_season() {
        // AniList's order for "KonoSuba": both seasons are TV with 10 episodes.
        let konosuba = || {
            vec![
                anime(21202, "Kono Subarashii Sekai ni Shukufuku wo!", "KONOSUBA -God's blessing on this wonderful world!", "TV", Some(10)),
                anime(21699, "Kono Subarashii Sekai ni Shukufuku wo! 2", "KONOSUBA -God's blessing on this wonderful world! 2", "TV", Some(10)),
                anime(136804, "Kono Subarashii Sekai ni Shukufuku wo! 3", "KONOSUBA -God's blessing on this wonderful world! 3", "TV", Some(11)),
            ]
        };
        for (folder, id) in [("Season 1", 21202), ("Season 2", 21699), ("Season 3", 136804)] {
            let mut found = konosuba();
            rank(&mut found, &format!("KonoSuba – God’s Blessing On This Wonderful World {folder}"), if id == 136804 { 11 } else { 10 });
            assert_eq!(found[0].id, id, "{folder}");
        }
        assert_eq!(season("Yuru Camp△ SEASON 2"), Some(2));
        assert_eq!(season("Re:Zero kara Hajimeru Isekai Seikatsu 2nd Season"), Some(2));
        assert_eq!(season("Haikyuu!! Second Season"), Some(2));
        assert_eq!(season("Sword Art Online II"), Some(2));
        assert_eq!(season("Pokémon Concierge Part 2"), None);
        assert_eq!(season("FAIRY TAIL (2014)"), None);
    }

    #[test]
    fn picks_a_short_title() {
        let a = anime(1, "Shirokuma Cafe", "Polar Bear Cafe", "TV", Some(50));
        assert_eq!(a.short_title(), "Polar Bear Cafe");
        let b = anime(2, "Sousou no Frieren", "Frieren: Beyond Journey’s End", "TV", Some(28));
        assert_eq!(b.short_title(), "Sousou no Frieren");
    }
}
