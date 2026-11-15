//! nyaa.si: finding releases of a show to download. Telly reads nyaa's search
//! page (its RSS feed lists only the newest, whatever the order asked for),
//! best-seeded first unless you ask for the newest, and downloads the one you
//! pick from its .torrent file (torrents.rs).
//!
//! An anime nyaa has only as a release for each episode, with no one release
//! of all of them, can still be downloaded whole: `every_episode` reads a few
//! pages of what nyaa finds and puts together the episodes one group released
//! at one resolution, named alike (as Follow reads them, follow.rs), one
//! release for each.
//!
//!   GET /?q=frieren&c=1_0&f=0&s=seeders&o=desc&p=1
//!   GET /download/2088335.torrent
//!
//! `c` is the category (1_0 all anime, 1_2 English-translated, 1_4 raw) and
//! f=2 keeps to trusted uploaders. Setting nyaa.site is another address for
//! nyaa, for where nyaa.si itself is blocked; TELLY_NYAA is a stand-in for tests.

use anyhow::{anyhow, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;

const SITE: &str = "https://nyaa.si";
const USER_AGENT: &str = concat!("Telly/", env!("CARGO_PKG_VERSION"), " (personal anime player)");

/// One release on nyaa.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub id: i64,
    pub title: String,
    pub magnet: String,
    /// Its .torrent file.
    pub torrent: String,
    /// As nyaa shows it: "1.4 GiB".
    pub size: String,
    pub bytes: u64,
    pub seeders: i64,
    pub leechers: i64,
    /// How many times it's been downloaded.
    pub completed: i64,
    /// When it was uploaded (unix seconds).
    pub date: i64,
    /// From one of nyaa's trusted uploaders (its green rows).
    pub trusted: bool,
    /// Someone else's release uploaded again (its red rows).
    pub remake: bool,
    /// Its page on nyaa.
    pub page: String,
}

/// A page of what nyaa found, and whether it has another.
#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub releases: Vec<Release>,
    pub more: bool,
}

/// Where nyaa is: setting nyaa.site, TELLY_NYAA, or nyaa.si.
fn site(chosen: &str) -> String {
    let chosen = chosen.trim().trim_end_matches('/');
    if !chosen.is_empty() {
        return if chosen.contains("://") { chosen.to_string() } else { format!("https://{chosen}") };
    }
    std::env::var("TELLY_NYAA").unwrap_or_else(|_| SITE.to_string())
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(45)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// Searches nyaa for `query`: `category` is "all", "translated" or "raw",
/// `sort` "seeders" or "newest", and `page` counts from 1.
pub fn search(chosen_site: &str, query: &str, category: &str, trusted: bool, sort: &str, page: u32) -> Result<Found> {
    let site = site(chosen_site);
    let category = match category {
        "translated" => "1_2",
        "raw" => "1_4",
        _ => "1_0",
    };
    let sort = if sort == "newest" { "id" } else { "seeders" };
    let page = page.max(1).to_string();
    let params = [("q", query.trim()), ("c", category), ("f", if trusted { "2" } else { "0" }), ("s", sort), ("o", "desc"), ("p", page.as_str())];
    let html = fetch(&format!("{site}/"), &params)?;
    parse(&String::from_utf8_lossy(&html), &site)
}

/// How many of nyaa's pages of releases `every_episode` reads, at most: 75 releases each.
const PAGES: u32 = 4;

/// One release for each episode of an anime, from the same group at the same resolution,
/// named alike: what downloads all of it when no one release has every episode.
#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeSet {
    /// One release for each episode, the first episode first.
    pub releases: Vec<Release>,
    /// The episodes that are out but nyaa has none of, from this group.
    pub missing: Vec<i64>,
    /// The fewest people sharing any one of them.
    pub seeders: i64,
    pub bytes: u64,
    /// Whether nyaa has a release of several episodes in one that people are sharing.
    pub batch: bool,
}

/// The set of single-episode releases nyaa has for `query` that gets the most
/// of the anime: every one of its `episodes` that are out (or, when AniList
/// doesn't say, every one up to the last nyaa has), from one group.
pub fn every_episode(chosen_site: &str, query: &str, category: &str, trusted: bool, episodes: Option<i64>) -> Result<Option<EpisodeSet>> {
    let mut found = Vec::new();
    for page in 1..=PAGES {
        match search(chosen_site, query, category, trusted, "seeders", page) {
            Ok(Found { releases, more }) => {
                found.extend(releases);
                if !more {
                    break;
                }
            }
            // The pages already read are enough to go on.
            Err(_) if page > 1 => break,
            Err(e) => return Err(e),
        }
    }
    Ok(episode_set(&found, query, episodes))
}

/// The resolution a tag or word says, "1080p".
fn resolution(text: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b([0-9]{3,4}p)\b").expect("a valid pattern")).captures(text).map(|c| c[1].to_lowercase())
}

/// Whether a release's title says it has several episodes: "(01-28)", "01 ~ 12", "E01-E12", "Batch", "Complete".
fn several_episodes(title: &str) -> bool {
    static WORD: OnceLock<Regex> = OnceLock::new();
    static RANGE: OnceLock<Regex> = OnceLock::new();
    if WORD.get_or_init(|| Regex::new(r"(?i)\b(?:batch|complete)\b").expect("a valid pattern")).is_match(title) {
        return true;
    }
    let range = RANGE.get_or_init(|| {
        Regex::new(r"(?i)(?:^|[^a-z0-9.]|s[0-9]{1,2})(?:ep?)?([0-9]{1,4})(?:-|\s*~\s*)(?:ep?)?([0-9]{1,4})(?:[^a-z0-9.]|$)").expect("a valid pattern")
    });
    range.captures_iter(title).any(|c| {
        let (first, last) = (c[1].parse::<i64>().unwrap_or(0), c[2].parse::<i64>().unwrap_or(0));
        first < last && last < 1900
    })
}

/// Of the releases `found` for `query`, the best set of single episodes: each
/// episode's release from one group, at one resolution, titled alike. The set
/// with every episode comes first, then the one whose title has the fewest
/// words `query` hasn't (a sequel's "2nd Season" when the first is asked for),
/// then the one with the most episodes, then the one most shared.
fn episode_set(found: &[Release], query: &str, episodes: Option<i64>) -> Option<EpisodeSet> {
    let asked: HashSet<String> = query.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    let mut sets: HashMap<(String, String, Vec<String>), BTreeMap<i64, &Release>> = HashMap::new();
    let mut batch = false;
    for release in found {
        let shape = if several_episodes(&release.title) { None } else { crate::follow::shape(&release.title) };
        let Some(shape) = shape else {
            batch |= release.seeders > 0;
            continue;
        };
        if shape.number < 1 || episodes.is_some_and(|out| shape.number > out) {
            continue;
        }
        let at = shape.tags.iter().chain(&shape.words).find_map(|t| resolution(t)).unwrap_or_default();
        let group = if release.title.trim_start().starts_with('[') { shape.tags.first().cloned().unwrap_or_default() } else { String::new() };
        let words = shape.words.into_iter().filter(|w| resolution(w).is_none()).collect();
        let picked = sets.entry((group, at, words)).or_default().entry(shape.number).or_insert(release);
        // Of two of an episode, the one people share, then the newer upload (a v2 over the first).
        if (release.seeders > 0, release.id) > (picked.seeders > 0, picked.id) {
            *picked = release;
        }
    }
    let best = sets
        .into_iter()
        .filter(|(_, by_number)| by_number.len() >= 2)
        .map(|((_, _, words), by_number)| {
            let last = episodes.or_else(|| by_number.keys().next_back().copied()).unwrap_or(0);
            let missing: Vec<i64> = (1..=last).filter(|n| !by_number.contains_key(n)).collect();
            let extra = words.iter().take_while(|w| !w.ends_with('#')).filter(|w| !asked.contains(*w)).count();
            let releases: Vec<Release> = by_number.into_values().cloned().collect();
            let seeders = releases.iter().map(|r| r.seeders).min().unwrap_or(0);
            let shared: i64 = releases.iter().map(|r| r.seeders).sum();
            let first = std::cmp::Reverse(releases[0].id);
            ((missing.is_empty(), std::cmp::Reverse(extra), releases.len(), seeders > 0, seeders, shared, first), (releases, missing, seeders))
        })
        .max_by(|a, b| a.0.cmp(&b.0))?;
    let (releases, missing, seeders) = best.1;
    let bytes = releases.iter().map(|r| r.bytes).sum();
    Some(EpisodeSet { releases, missing, seeders, bytes, batch })
}

/// A release's .torrent file.
pub fn torrent_file(url: &str) -> Result<Vec<u8>> {
    if !url.starts_with("http") {
        return Err(anyhow!("There’s no .torrent file to fetch"));
    }
    let bytes = fetch(url, &[])?;
    // A .torrent file is a bencoded dictionary.
    if bytes.first() != Some(&b'd') {
        return Err(anyhow!("nyaa didn’t send a .torrent file"));
    }
    Ok(bytes)
}

/// Asks nyaa for `url`. A dropped connection is asked again, twice, before giving up.
fn fetch(url: &str, params: &[(&str, &str)]) -> Result<Vec<u8>> {
    let mut failed = anyhow!("nyaa isn’t reachable");
    for attempt in 0..3u64 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(2 * attempt));
        }
        let mut response = match agent().get(url).query_pairs(params.iter().copied()).header("User-Agent", USER_AGENT).call() {
            Ok(r) => r,
            Err(e) => {
                failed = anyhow!("nyaa isn’t reachable ({e})");
                continue;
            }
        };
        match response.status().as_u16() {
            200 => {}
            429 => return Err(anyhow!("nyaa asked Telly to slow down. Try again in a minute.")),
            code if code >= 500 => {
                failed = anyhow!("nyaa answered {code}");
                continue;
            }
            code => return Err(anyhow!("nyaa answered {code}")),
        }
        match response.body_mut().with_config().limit(8 * 1024 * 1024).read_to_vec() {
            Ok(bytes) => return Ok(bytes),
            Err(e) => failed = anyhow!("nyaa’s answer was cut off ({e})"),
        }
    }
    Err(failed)
}

/// Reads a search page's table of releases.
fn parse(html: &str, site: &str) -> Result<Found> {
    static ROW: OnceLock<Regex> = OnceLock::new();
    static VIEW: OnceLock<Regex> = OnceLock::new();
    static MAGNET: OnceLock<Regex> = OnceLock::new();
    static SIZE: OnceLock<Regex> = OnceLock::new();
    static DATE: OnceLock<Regex> = OnceLock::new();
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    let re = |cell: &'static OnceLock<Regex>, pattern: &str| cell.get_or_init(|| Regex::new(pattern).expect("a valid pattern"));
    // An internet provider's page saying nyaa is blocked has no table of torrents.
    if !html.contains("torrent-list") {
        return Err(anyhow!("That isn’t nyaa’s search page. If your internet provider blocks nyaa.si, give another address for it in Settings → Downloads."));
    }
    let row = re(&ROW, r#"(?s)<tr class="(default|success|danger)">(.*?)</tr>"#);
    let view = re(&VIEW, r#"<a href="/view/(\d+)" title="([^"]*)""#);
    let magnet = re(&MAGNET, r#"href="(magnet:\?[^"]+)""#);
    let size = re(&SIZE, r#"<td class="text-center">\s*([\d.]+\s*[KMGT]?i?B)\s*</td>"#);
    let date = re(&DATE, r#"data-timestamp="(\d+)""#);
    let number = re(&NUMBER, r#"<td class="text-center">\s*(\d+)\s*</td>"#);
    let mut releases = Vec::new();
    for r in row.captures_iter(html) {
        let (kind, cells) = (&r[1], &r[2]);
        let (Some(v), Some(m)) = (view.captures(cells), magnet.captures(cells)) else { continue };
        let Ok(id) = v[1].parse::<i64>() else { continue };
        let size = size.captures(cells).map(|s| s[1].to_string()).unwrap_or_default();
        let date_at = date.captures(cells);
        // Seeders, leechers and downloads are the three numbers after the date.
        let after = date_at.as_ref().and_then(|d| d.get(0)).map_or(0, |d| d.end());
        let counts: Vec<i64> = number.captures_iter(&cells[after..]).filter_map(|n| n[1].parse().ok()).collect();
        releases.push(Release {
            id,
            title: unescape(&v[2]),
            magnet: unescape(&m[1]),
            torrent: format!("{site}/download/{id}.torrent"),
            bytes: size_bytes(&size),
            size,
            seeders: counts.first().copied().unwrap_or(0),
            leechers: counts.get(1).copied().unwrap_or(0),
            completed: counts.get(2).copied().unwrap_or(0),
            date: date_at.and_then(|d| d[1].parse().ok()).unwrap_or(0),
            trusted: kind == "success",
            remake: kind == "danger",
            page: format!("{site}/view/{id}"),
        });
    }
    Ok(Found { releases, more: html.contains(r#"<li class="next">"#) })
}

/// "1.4 GiB" in bytes.
fn size_bytes(text: &str) -> u64 {
    let text = text.trim();
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(text.len());
    let value: f64 = text[..split].parse().unwrap_or(0.0);
    let power = match text[split..].trim().to_ascii_uppercase().as_str() {
        "KIB" | "KB" => 1,
        "MIB" | "MB" => 2,
        "GIB" | "GB" => 3,
        "TIB" | "TB" => 4,
        _ => 0,
    };
    (value * 1024f64.powi(power)).round() as u64
}

/// HTML's escapes in an attribute: &amp;, &quot;, &#39; and the like.
fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.char_indices().take(12).find(|&(_, c)| c == ';').map(|(i, _)| i) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let c = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|n| n.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three rows of nyaa's search page, as it sends them: trusted, ordinary and a remake.
    const PAGE: &str = r#"<table class="table table-bordered table-hover table-striped torrent-list"><tbody>
			<tr class="success">
				<td><a href="/?c=1_2" title="Anime - English-translated"><img src="/static/img/icons/nyaa/1_2.png" alt="Anime - English-translated" class="category-icon"></a></td>
				<td colspan="2">
					<a href="/view/2088335#comments" class="comments" title="1 comment"><i class="fa fa-comments-o"></i>1</a>
					<a href="/view/2088335" title="[Erai-raws] Sousou no Frieren 2nd Season - 09 [1080p CR WEB-DL AVC AAC][MultiSub][15B7C196]">[Erai-raws] Sousou no Frieren 2nd Season - 09 [1080p CR WEB-DL AVC AAC][MultiSub][15B7C196]</a>
				</td>
				<td class="text-center">
					<a href="/download/2088335.torrent"><i class="fa fa-fw fa-download"></i></a>
					<a href="magnet:?xt=urn:btih:e738213eba3cb50034e8c37955d7f58c7b9ae53f&amp;dn=%5BErai-raws%5D%20Sousou%20no%20Frieren&amp;tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce"><i class="fa fa-fw fa-magnet"></i></a>
				</td>
				<td class="text-center">1.4 GiB</td>
				<td class="text-center" data-timestamp="1774018847">2026-03-20 15:00</td>

				<td class="text-center">657</td>
				<td class="text-center">7</td>
				<td class="text-center">37623</td>
			</tr>
			<tr class="default">
				<td><a href="/?c=1_2" title="Anime - English-translated"><img src="/static/img/icons/nyaa/1_2.png" alt="Anime - English-translated" class="category-icon"></a></td>
				<td colspan="2">
					<a href="/view/2080580" title="[ToonsHub] Frieren Beyond Journeys End S02E06 1080p BILI WEB-DL AAC2.0 H.265 (Sousou no Frieren, Multi-Subs)">[ToonsHub] Frieren Beyond Journeys End S02E06 1080p BILI WEB-DL AAC2.0 H.265 (Sousou no Frieren, Multi-Subs)</a>
				</td>
				<td class="text-center">
					<a href="/download/2080580.torrent"><i class="fa fa-fw fa-download"></i></a>
					<a href="magnet:?xt=urn:btih:dafe600371d2231bd08dec9e0ec166846aef2029&amp;dn=%5BToonsHub%5D"><i class="fa fa-fw fa-magnet"></i></a>
				</td>
				<td class="text-center">376.5 MiB</td>
				<td class="text-center" data-timestamp="1772204450">2026-02-27 15:00</td>

				<td class="text-center">414</td>
				<td class="text-center">1</td>
				<td class="text-center">14538</td>
			</tr>
			<tr class="danger">
				<td><a href="/?c=1_2" title="Anime - English-translated"><img src="/static/img/icons/nyaa/1_2.png" alt="Anime - English-translated" class="category-icon"></a></td>
				<td colspan="2">
					<a href="/view/2097365#comments" class="comments" title="3 comments"><i class="fa fa-comments-o"></i>3</a>
					<a href="/view/2097365" title="[Judas] Sousou no Frieren (Frieren Beyond Journey`s End) &amp; &quot;more&quot; &#39;Batch&#39;">[Judas] Sousou no Frieren</a>
				</td>
				<td class="text-center">
					<a href="/download/2097365.torrent"><i class="fa fa-fw fa-download"></i></a>
					<a href="magnet:?xt=urn:btih:cc7e35dc12f72d204abcc956a2f38e3096f80d3b&amp;dn=%5BJudas%5D"><i class="fa fa-fw fa-magnet"></i></a>
				</td>
				<td class="text-center">5.5 GiB</td>
				<td class="text-center" data-timestamp="1776007892">2026-04-12 15:31</td>

				<td class="text-center">385</td>
				<td class="text-center">8</td>
				<td class="text-center">21205</td>
			</tr>
		</tbody></table>
	<ul class="pagination"><li class="previous disabled unavailable"><a> &laquo; </a></li><li class="active"><a>1</a></li><li class="next"><a href="/?q=frieren&p=2">&raquo;</a></li></ul>"#;

    #[test]
    fn reads_each_release_on_the_page() {
        let found = parse(PAGE, "https://nyaa.si").unwrap();
        assert!(found.more);
        assert_eq!(found.releases.len(), 3);
        let first = &found.releases[0];
        assert_eq!(first.id, 2088335);
        assert_eq!(first.title, "[Erai-raws] Sousou no Frieren 2nd Season - 09 [1080p CR WEB-DL AVC AAC][MultiSub][15B7C196]");
        assert_eq!(
            first.magnet,
            "magnet:?xt=urn:btih:e738213eba3cb50034e8c37955d7f58c7b9ae53f&dn=%5BErai-raws%5D%20Sousou%20no%20Frieren&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce"
        );
        assert_eq!((first.seeders, first.leechers, first.completed), (657, 7, 37623));
        assert_eq!(first.size, "1.4 GiB");
        assert_eq!(first.bytes, 1_503_238_554);
        assert_eq!(first.date, 1774018847);
        assert!(first.trusted && !first.remake);
        assert_eq!(first.page, "https://nyaa.si/view/2088335");
        assert_eq!(first.torrent, "https://nyaa.si/download/2088335.torrent");
        let second = &found.releases[1];
        assert_eq!((second.seeders, second.bytes), (414, 394_788_864));
        assert!(!second.trusted && !second.remake);
        let third = &found.releases[2];
        assert!(third.remake && !third.trusted);
        assert_eq!(third.title, r#"[Judas] Sousou no Frieren (Frieren Beyond Journey`s End) & "more" 'Batch'"#);
    }

    #[test]
    fn the_last_page_has_no_more() {
        let last = PAGE.replace(r#"<li class="next">"#, r#"<li class="next disabled">"#);
        assert!(!parse(&last, SITE).unwrap().more);
    }

    #[test]
    fn a_page_that_isnt_nyaas_is_an_error() {
        assert!(parse("<html><body>This site has been blocked</body></html>", SITE).is_err());
        let empty = parse(r#"<table class="torrent-list"><tbody></tbody></table>"#, SITE).unwrap();
        assert!(empty.releases.is_empty() && !empty.more);
    }

    #[test]
    fn another_address_for_nyaa() {
        assert_eq!(site("nyaa.land/"), "https://nyaa.land");
        assert_eq!(site("http://192.168.1.5:8000"), "http://192.168.1.5:8000");
    }

    #[test]
    fn sizes_and_escapes() {
        assert_eq!(size_bytes("980 KiB"), 1_003_520);
        assert_eq!(size_bytes("123 Bytes"), 123);
        assert_eq!(unescape("a &amp; b &#x27;c&#x27; &#34;d&#34; & e &unknown;"), "a & b 'c' \"d\" & e &unknown;");
        // Chinese fansub groups joined by "&" right beside wide characters.
        assert_eq!(unescape("[千夏字幕组&amp;喵萌奶茶屋] 葬送的芙莉莲"), "[千夏字幕组&喵萌奶茶屋] 葬送的芙莉莲");
        assert_eq!(unescape("[豌豆字幕组&风之圣殿字幕组]"), "[豌豆字幕组&风之圣殿字幕组]");
    }

    fn release(id: i64, title: &str, seeders: i64) -> Release {
        Release {
            id,
            title: title.into(),
            magnet: String::new(),
            torrent: String::new(),
            size: String::new(),
            bytes: 100,
            seeders,
            leechers: 0,
            completed: 0,
            date: 0,
            trusted: false,
            remake: false,
            page: String::new(),
        }
    }

    /// The ids of the releases `episode_set` picks from `found` (id, title, seeders), and the episodes it misses.
    fn picked(found: &[(i64, &str, i64)], query: &str, episodes: Option<i64>) -> Option<(Vec<i64>, Vec<i64>, bool)> {
        let found: Vec<Release> = found.iter().map(|&(id, title, seeders)| release(id, title, seeders)).collect();
        episode_set(&found, query, episodes).map(|set| (set.releases.iter().map(|r| r.id).collect(), set.missing, set.batch))
    }

    #[test]
    fn a_release_for_each_episode_makes_a_set() {
        let found = [
            (11, "[SubsPlease] Show - 01 (1080p) [AAAAAAAA].mkv", 50),
            (12, "[SubsPlease] Show - 02 (1080p) [BBBBBBBB].mkv", 40),
            (13, "[SubsPlease] Show - 03 (1080p) [CCCCCCCC].mkv", 30),
            (14, "[SubsPlease] Show - 03v2 (1080p) [DDDDDDDD].mkv", 5),
            (21, "[SubsPlease] Show - 01 (720p) [EEEEEEEE].mkv", 90),
            (22, "[SubsPlease] Show - 02 (720p) [FFFFFFFF].mkv", 90),
            (31, "[Erai-raws] Show - 01 [1080p][Multiple Subtitle]", 99),
            (32, "[Erai-raws] Show - 02 [1080p][Multiple Subtitle][ENG]", 99),
            (33, "[Erai-raws] Show - 03 [1080p][Multiple Subtitle]", 0),
            (41, "[SubsPlease] Show 2nd Season - 01 (1080p) [11111111].mkv", 500),
            (42, "[SubsPlease] Show 2nd Season - 02 (1080p) [22222222].mkv", 500),
            (43, "[SubsPlease] Show 2nd Season - 03 (1080p) [33333333].mkv", 500),
        ];
        // SubsPlease's 1080p has all three (episode 3's v2); the 720p has two, Erai-raws' episode 3
        // no one shares, and the second season isn't the one asked for.
        assert_eq!(picked(&found, "Show", Some(3)), Some((vec![11, 12, 14], vec![], false)));
        // Asked for, the second season is.
        assert_eq!(picked(&found, "Show 2nd Season", Some(3)), Some((vec![41, 42, 43], vec![], false)));
        // Without AniList's count, the set that goes furthest without a gap.
        assert_eq!(picked(&found, "Show", None), Some((vec![11, 12, 14], vec![], false)));
    }

    #[test]
    fn a_set_with_an_episode_missing_says_which() {
        let found = [
            (1, "Show.S01E01.1080p.WEB.H264-GRP", 9),
            (3, "Show.S01E03.1080p.WEB.H264-GRP", 9),
            (4, "[Judas] Show (Season 1) [BD 1080p][HEVC x265 10bit]", 300),
            (5, "[Group] Show (01-12) [1080p] [Batch]", 0),
        ];
        // The batch no one shares doesn't count, Judas' whole season does.
        assert_eq!(picked(&found, "Show", Some(3)), Some((vec![1, 3], vec![2], true)));
        assert_eq!(picked(&found[..1], "Show", Some(3)), None);
    }

    #[test]
    fn several_episodes_in_one() {
        for title in ["[SubsPlease] Frieren (01-28) (1080p) [Batch]", "[Erai-raws] Show - 01 ~ 12 [1080p]", "Show S01E01-E12 1080p", "Show - 01-12 [720p]", "Show Complete Series"] {
            assert!(several_episodes(title), "{title}");
        }
        for title in ["[SubsPlease] Spy x Family Season 2 - 05 (1080p)", "[Group] Show - 05 [1080p x264-10bit]", "Show (2019-2020) - 05", "86 - 05", "Show S02E05 AAC2.0-GRP"] {
            assert!(!several_episodes(title), "{title}");
        }
    }
}
