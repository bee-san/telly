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

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        [self.romaji.as_str(), self.english.as_str(), self.native.as_str()]
            .into_iter()
            .chain(self.synonyms.iter().map(String::as_str))
    }

    /// The season its titles name. A first season's name none.
    pub(crate) fn season(&self) -> i64 {
        self.names().find_map(season).unwrap_or(1)
    }

    /// A TV series: what a season folder holds.
    fn is_series(&self) -> bool {
        matches!(self.format.as_str(), "TV" | "TV_SHORT" | "ONA")
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
        // What it says of an ID it has no anime for.
        if answer.pointer("/errors/0/status").and_then(Value::as_i64) == Some(404) {
            return Err(NotFound(error.to_string()).into());
        }
        return Err(anyhow!("AniList: {error}"));
    }
    Ok(answer)
}

/// AniList answered "Not Found" (404) to a query.
#[derive(Debug)]
pub struct NotFound(String);

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AniList: {}", self.0)
    }
}

impl std::error::Error for NotFound {}

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
/// ("Season 2", "S2") isn't searched for: AniList finds every season by the
/// show's name, and the words throw it ("K-On! Season 1" finds other shows,
/// "Overlord S2" nothing). `rank` picks the season out.
pub fn search(text: &str) -> Result<Vec<Anime>> {
    static SEASON: OnceLock<Regex> = OnceLock::new();
    let season = SEASON.get_or_init(|| Regex::new(r"(?i)\b(?:season\s*\d+|s\d{1,2})\b").expect("valid pattern"));
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

/// How many of an anime's episodes are out: every one once it's finished, those before the
/// next to air while it's airing, and None when AniList doesn't say.
pub fn episodes_out(id: i64) -> Result<Option<i64>> {
    let q = "query ($id: Int) { Media(id: $id, type: ANIME) { episodes status nextAiringEpisode { episode } } }";
    let answer = query(q, json!({ "id": id }))?;
    let media = answer.pointer("/data/Media").ok_or_else(|| anyhow!("AniList has no anime {id}"))?;
    if let Some(next) = media.pointer("/nextAiringEpisode/episode").and_then(Value::as_i64) {
        return Ok(Some(next - 1));
    }
    if media.get("status").and_then(Value::as_str) == Some("RELEASING") {
        return Ok(None);
    }
    Ok(media.get("episodes").and_then(Value::as_i64))
}

/// The anime of each of `ids` that AniList has, asked for together.
pub fn by_ids(ids: &[i64]) -> Result<Vec<Anime>> {
    let q = format!("query ($ids: [Int]) {{ Page(perPage: 50) {{ media(id_in: $ids, type: ANIME) {{ {FIELDS} }} }} }}");
    let mut found = Vec::new();
    for ids in ids.chunks(50) {
        let answer = query_patiently(&q, json!({ "ids": ids }))?;
        found.extend(answer.pointer("/data/Page/media").and_then(Value::as_array).into_iter().flatten().filter_map(Anime::from_api));
    }
    Ok(found)
}

/// The name of the user whose access token `token` is.
pub fn viewer(token: &str) -> Result<String> {
    let answer = query_as("query { Viewer { name } }", json!({}), Some(token))?;
    answer.pointer("/data/Viewer/name").and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("AniList didn’t say who this token is for"))
}

/// An anime on the user's list, as Recommended reads it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Listed {
    pub id: i64,
    /// Its short title.
    pub title: String,
    /// CURRENT, COMPLETED, PAUSED, DROPPED or REPEATING ("" in a copy of the list kept before Telly read them).
    pub status: String,
    /// The user's score out of 100 (0 when they haven't scored it).
    pub score: f64,
    pub progress: i64,
    pub episodes: Option<i64>,
    /// When it was last watched, as near as the list says (unix ms; 0 when it doesn't):
    /// when the user finished it, or last changed the entry of one they're watching
    /// or one with no finishing date.
    #[serde(default)]
    pub at: i64,
}

/// The anime on the user's list that they've started (all but those they only
/// plan to watch), with each one's status and score.
pub fn list(token: &str) -> Result<Vec<Listed>> {
    let viewer = query_as("query { Viewer { id } }", json!({}), Some(token))?;
    let user = viewer.pointer("/data/Viewer/id").and_then(Value::as_i64).ok_or_else(|| anyhow!("AniList didn’t say who this token is for"))?;
    let q = "query ($user: Int, $chunk: Int) { MediaListCollection(userId: $user, type: ANIME, status_not: PLANNING, chunk: $chunk, perChunk: 500) {
      hasNextChunk lists { entries { mediaId status score(format: POINT_100) progress updatedAt completedAt { year month day } media { id episodes title { romaji english native } } } } } }";
    let mut found = Vec::new();
    for chunk in 1.. {
        let answer = query_as(q, json!({ "user": user, "chunk": chunk }), Some(token))?;
        found.extend(listed_in(&answer));
        if answer.pointer("/data/MediaListCollection/hasNextChunk").and_then(Value::as_bool) != Some(true) {
            break;
        }
    }
    // An anime in one of the user's own lists is in its status's list too.
    found.sort_by_key(|l| l.id);
    found.dedup_by_key(|l| l.id);
    Ok(found)
}

/// The anime in one chunk of the user's list.
fn listed_in(answer: &Value) -> Vec<Listed> {
    let lists = answer.pointer("/data/MediaListCollection/lists").and_then(Value::as_array).into_iter().flatten();
    lists
        .flat_map(|list| list.get("entries").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|e| {
            let anime = e.get("media").and_then(Anime::from_api);
            let status = e.get("status").and_then(Value::as_str).unwrap_or("").to_string();
            let updated = e.get("updatedAt").and_then(Value::as_i64).map_or(0, |s| s * 1000);
            let date = |part: &str| e.pointer(&format!("/completedAt/{part}")).and_then(Value::as_i64);
            let completed = date("year").map(|year| day_ms(year, date("month").unwrap_or(1), date("day").unwrap_or(1)));
            Some(Listed {
                id: e.get("mediaId")?.as_i64()?,
                title: anime.as_ref().map(Anime::short_title).unwrap_or_default(),
                // One being watched (again) is as recent as its last change; a list moved from elsewhere
                // was changed all at once, so the date it was finished says more when there is one.
                at: if matches!(status.as_str(), "CURRENT" | "REPEATING") { updated } else { completed.unwrap_or(updated) },
                status,
                score: e.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                progress: e.get("progress").and_then(Value::as_i64).unwrap_or(0),
                episodes: anime.and_then(|a| a.episodes),
            })
        })
        .collect()
}

/// Midnight at the start of a day, in UTC (unix ms).
fn day_ms(year: i64, month: i64, day: i64) -> i64 {
    // Howard Hinnant's days-from-civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400_000
}

/// An anime on the user's list: its episode count (if AniList knows it),
/// and the entry's status and progress (None when it isn't on the list).
pub struct ListEntry {
    pub episodes: Option<i64>,
    pub status: Option<String>,
    pub progress: i64,
    /// How many times it's been rewatched to the end.
    pub repeat: i64,
}

/// AniList has no anime of this ID (any more: entries get merged).
#[derive(Debug)]
pub struct NoAnime(pub i64);

impl std::fmt::Display for NoAnime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AniList has no anime {}", self.0)
    }
}

impl std::error::Error for NoAnime {}

pub fn list_entry(token: &str, id: i64) -> Result<ListEntry> {
    let q = "query ($id: Int) { Media(id: $id, type: ANIME) { episodes mediaListEntry { status progress repeat } } }";
    let answer = match query_as(q, json!({ "id": id }), Some(token)) {
        Err(e) if e.downcast_ref::<NotFound>().is_some() => return Err(NoAnime(id).into()),
        answer => answer?,
    };
    let media = answer.pointer("/data/Media").filter(|m| !m.is_null()).ok_or(NoAnime(id))?;
    Ok(ListEntry {
        episodes: media.get("episodes").and_then(Value::as_i64),
        status: media.pointer("/mediaListEntry/status").and_then(Value::as_str).map(str::to_string),
        progress: media.pointer("/mediaListEntry/progress").and_then(Value::as_i64).unwrap_or(0),
        repeat: media.pointer("/mediaListEntry/repeat").and_then(Value::as_i64).unwrap_or(0),
    })
}

/// Sets how many episodes of anime `id` the user has watched, and its status
/// (CURRENT, COMPLETED, REPEATING…), and how many times it's been rewatched
/// when that changes.
pub fn save_progress(token: &str, id: i64, progress: i64, status: &str, repeat: Option<i64>) -> Result<()> {
    match repeat {
        Some(repeat) => {
            let q = "mutation ($id: Int, $progress: Int, $status: MediaListStatus, $repeat: Int) {
              SaveMediaListEntry(mediaId: $id, progress: $progress, status: $status, repeat: $repeat) { id } }";
            query_as(q, json!({ "id": id, "progress": progress, "status": status, "repeat": repeat }), Some(token))?;
        }
        None => {
            let q = "mutation ($id: Int, $progress: Int, $status: MediaListStatus) {
              SaveMediaListEntry(mediaId: $id, progress: $progress, status: $status) { id } }";
            query_as(q, json!({ "id": id, "progress": progress, "status": status }), Some(token))?;
        }
    }
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
pub(crate) fn likeness(a: &str, b: &str) -> f64 {
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
    } else if has(&["movie", "movies", "film", "films", "gekijouban", "劇場版"]) {
        "MOVIE"
    } else if has(&["special", "specials", "sp"]) {
        "SPECIAL"
    } else if has(&["ona"]) {
        "ONA"
    } else {
        "TV"
    }
}

/// The season a title names: "Season 2", "2nd Season", "Second Season", "S2",
/// or a number ending it ("Konosuba 2", "Overlord II", but not "Part 2").
pub(crate) fn season(title: &str) -> Option<i64> {
    let words = words(title);
    // "S2" and "S02", as release groups name season folders.
    let short = |w: &String| w.strip_prefix('s').filter(|n| (1..=2).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())).and_then(|n| n.parse::<i64>().ok());
    if let Some(n) = words.iter().find_map(short).filter(|&n| n > 0) {
        return Some(n);
    }
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
        let title_fit = a.names().map(|name| likeness(title, name)).fold(0.0, f64::max);
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
        // A folder that names its season wants the entry for that season.
        a.score = match wanted_season {
            Some(s) => 0.8 * score + if a.season() == s { 0.2 } else { 0.0 },
            None => score,
        };
    }
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
}

/// The entries AniList says a show goes on in ("SEQUEL").
fn sequels(id: i64) -> Result<Vec<Anime>> {
    let q = format!("query ($id: Int) {{ Media(id: $id, type: ANIME) {{ relations {{ edges {{ relationType node {{ type {FIELDS} }} }} }} }} }}");
    let answer = query(&q, json!({ "id": id }))?;
    Ok(answer
        .pointer("/data/Media/relations/edges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e["relationType"] == "SEQUEL" && e["node"]["type"] == "ANIME")
        .filter_map(|e| Anime::from_api(&e["node"]))
        .collect())
}

/// When `title` names a later season ("Season 4") that no result's title
/// names, puts first the series that many seasons along AniList's sequels
/// from the first season. Its title often doesn't say ("Sword Art Online:
/// Alicization - War of Underworld"), and films can come between (Rascal Does
/// Not Dream of Bunny Girl Senpai goes on in three before its second series).
pub fn follow_sequels(results: &mut Vec<Anime>, title: &str) -> Result<()> {
    walk_sequels(results, title, sequels)
}

fn walk_sequels(results: &mut Vec<Anime>, title: &str, sequels: impl Fn(i64) -> Result<Vec<Anime>>) -> Result<()> {
    let Some(wanted) = season(title).filter(|&s| s > 1) else { return Ok(()) };
    if results.iter().any(|a| a.season() == wanted) {
        return Ok(());
    }
    let Some(mut current) = results.iter().find(|a| a.season() == 1 && a.is_series()).cloned() else { return Ok(()) };
    let mut seen = std::collections::HashSet::from([current.id]);
    let mut reached = 1;
    while reached < wanted {
        // A show can go on in more than one way (a series and a special): the series, then the earliest.
        let next = sequels(current.id)?.into_iter().filter(|a| !seen.contains(&a.id)).min_by_key(|a| (!a.is_series(), a.year.unwrap_or(i64::MAX), a.id));
        let Some(next) = next else { return Ok(()) };
        seen.insert(next.id);
        if next.is_series() {
            reached += 1;
        }
        current = next;
    }
    current.score = 1.0;
    results.retain(|a| a.id != current.id);
    results.insert(0, current);
    Ok(())
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
    // Written whole or not at all: a half cover would count as had for good.
    crate::util::write_whole(path, bytes)?;
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
        assert_eq!(season("Overlord S2"), Some(2));
        assert_eq!(season("Clannad S02"), Some(2));
        assert_eq!(season("Overlord S00"), None);
    }

    #[test]
    fn a_folder_named_s2_or_movies_gets_that_season_or_a_film() {
        // AniList's order for "Overlord": every season is TV with 13 episodes.
        let overlord = || {
            vec![
                anime(20832, "Overlord", "Overlord", "TV", Some(13)),
                anime(98437, "Overlord II", "Overlord II", "TV", Some(13)),
                anime(101474, "Overlord III", "Overlord III", "TV", Some(13)),
                anime(133844, "Overlord IV", "Overlord IV", "TV", Some(13)),
            ]
        };
        for (folder, id) in [("Overlord S1", 20832), ("Overlord S2", 98437), ("Overlord S3", 101474)] {
            let mut found = overlord();
            rank(&mut found, folder, 13);
            assert_eq!(found[0].id, id, "{folder}");
        }
        assert_eq!(format_hint("Overlord Movies"), "MOVIE");
        assert_eq!(format_hint("Ghibli Films"), "MOVIE");
    }

    #[test]
    fn a_season_no_title_names_is_found_along_the_sequels() {
        let dated = |id, romaji: &str, format: &str, year| Anime { year: Some(year), ..anime(id, romaji, "", format, None) };
        // AniList's sequels, as it gave them on 12 November 2026.
        let sequels = |id: i64| -> Result<Vec<Anime>> {
            Ok(match id {
                11757 => vec![dated(20021, "Sword Art Online: Extra Edition", "SPECIAL", 2013), dated(20594, "Sword Art Online II", "TV", 2014)],
                20594 => vec![dated(21403, "Sword Art Online: Ordinal Scale", "MOVIE", 2017)],
                21403 => vec![dated(100182, "Sword Art Online: Alicization", "TV", 2018)],
                100182 => vec![dated(108759, "Sword Art Online: Alicization - War of Underworld", "TV", 2019)],
                101291 => vec![dated(104157, "Seishun Buta Yarou wa Yumemiru Shoujo no Yume wo Minai", "MOVIE", 2019)],
                104157 => vec![dated(154967, "Seishun Buta Yarou wa Odekake Sister no Yume wo Minai", "MOVIE", 2023)],
                154967 => vec![dated(161474, "Seishun Buta Yarou wa Randoseru Girl no Yume wo Minai", "MOVIE", 2023)],
                161474 => vec![dated(171046, "Seishun Buta Yarou wa Santa Claus no Yume wo Minai", "TV", 2025)],
                _ => Vec::new(),
            })
        };
        let mut sao = vec![anime(11757, "Sword Art Online", "", "TV", Some(25)), anime(20594, "Sword Art Online II", "", "TV", Some(24))];
        walk_sequels(&mut sao, "Sword Art Online Season 4", sequels).unwrap();
        assert_eq!(sao[0].id, 108759);
        let mut rascal = vec![anime(101291, "Seishun Buta Yarou wa Bunny Girl Senpai no Yume wo Minai", "", "TV", Some(13))];
        walk_sequels(&mut rascal, "Rascal Does Not Dream of Bunny Girl Senpai Season 2", sequels).unwrap();
        assert_eq!(rascal[0].id, 171046);
        // A season some title names needs no walk.
        let mut konosuba = vec![anime(21699, "Kono Subarashii Sekai ni Shukufuku wo! 2", "", "TV", Some(10))];
        walk_sequels(&mut konosuba, "KonoSuba Season 2", |_| panic!("asked AniList")).unwrap();
    }

    #[test]
    fn picks_a_short_title() {
        let a = anime(1, "Shirokuma Cafe", "Polar Bear Cafe", "TV", Some(50));
        assert_eq!(a.short_title(), "Polar Bear Cafe");
        let b = anime(2, "Sousou no Frieren", "Frieren: Beyond Journey’s End", "TV", Some(28));
        assert_eq!(b.short_title(), "Sousou no Frieren");
    }

    #[test]
    fn reads_the_users_list() {
        let answer = json!({ "data": { "MediaListCollection": { "hasNextChunk": false, "lists": [
            { "entries": [
                { "mediaId": 154587, "status": "COMPLETED", "score": 90, "progress": 28, "updatedAt": 1_790_000_000, "completedAt": { "year": 2024, "month": 3, "day": 22 },
                  "media": { "id": 154587, "episodes": 28, "title": { "romaji": "Sousou no Frieren", "english": "Frieren: Beyond Journey’s End", "native": "葬送のフリーレン" } } },
                { "mediaId": 21, "status": "CURRENT", "score": 0, "progress": 400, "updatedAt": 1_790_000_000, "completedAt": { "year": 2020, "month": null, "day": null }, "media": { "id": 21, "episodes": null, "title": { "romaji": "ONE PIECE" } } },
            ] },
            { "entries": [{ "mediaId": 30, "status": "DROPPED", "score": 35.5, "progress": 3, "media": null }] },
        ] } } });
        let frieren = Listed { id: 154587, title: "Sousou no Frieren".into(), status: "COMPLETED".into(), score: 90.0, progress: 28, episodes: Some(28), at: 1_711_065_600_000 };
        let one_piece = Listed { id: 21, title: "ONE PIECE".into(), status: "CURRENT".into(), score: 0.0, progress: 400, episodes: None, at: 1_790_000_000_000 };
        let dropped = Listed { id: 30, title: "".into(), status: "DROPPED".into(), score: 35.5, progress: 3, episodes: None, at: 0 };
        assert_eq!(listed_in(&answer), [frieren, one_piece, dropped]);
        // Finished in a year AniList knows only the year of.
        assert_eq!(day_ms(2020, 1, 1), 1_577_836_800_000);
    }
}
