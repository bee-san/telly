//! New episodes of a show you follow download themselves. Following is turned
//! on from a show's page. Telly then looks on nyaa.si (nyaa.rs) for releases
//! named as the show's latest rip is (or the last release downloaded for it),
//! but for a later episode: the same
//! group, the same title and the same tags (resolution, codec), so episode 6
//! comes from where episode 5 did. Each one found downloads into the show's
//! folder (torrents.rs) and joins the show as it finishes.
//!
//! It looks when following is turned on, a couple of minutes after Telly
//! opens and every hour after that, and not while a game is running. A show
//! with every episode AniList lists isn't looked for. A rip whose name gives
//! no episode number, or names the episode's title, has nothing to go by, and
//! nothing is downloaded for it: better none than the wrong one.
//!
//! Settings: follow.<show> (the show is followed), follow.got.<show> (the
//! last episode Telly started a download of, so none is started twice) and
//! follow.like.<show> (the title of the last release downloaded for the show,
//! which is how nyaa names its episodes when the files inside are named
//! another way).

use crate::backend::Backend;
use crate::db::Show;
use crate::{games, nyaa, scan, torrents};
use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const FIRST_LOOK: Duration = Duration::from_secs(120);
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);
const BETWEEN_SHOWS: Duration = Duration::from_secs(5);

/// The setting that says a show is followed.
pub(crate) fn setting(show: i64) -> String {
    format!("follow.{show}")
}

/// The setting with the last episode of a show Telly started a download of.
pub(crate) fn got_setting(show: i64) -> String {
    format!("follow.got.{show}")
}

/// The setting with the title of the last release downloaded for a show, as nyaa lists it.
pub(crate) fn like_setting(show: i64) -> String {
    format!("follow.like.{show}")
}

/// How a release names an episode: its tags, its words with the episode's number taken out, and the number.
#[derive(Debug, Clone, PartialEq)]
struct Shape {
    tags: Vec<String>,
    words: Vec<String>,
    number: i64,
}

impl Shape {
    /// Whether `other` is an episode of the same release: named the same but for the number.
    fn same_release(&self, other: &Shape) -> bool {
        self.tags == other.tags && self.words == other.words
    }
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a valid pattern"))
}

/// How `name` (a rip's file name, or a release's title) names its episode, when it gives a number.
fn shape(name: &str) -> Option<Shape> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static CRC: OnceLock<Regex> = OnceLock::new();
    static EPISODE: OnceLock<Regex> = OnceLock::new();
    let stem = if scan::is_video(Path::new(name)) { name.rsplit_once('.').map_or(name, |(stem, _)| stem) } else { name };
    let (_, number) = scan::episode_number(stem)?;
    let tag = re(&TAG, r"\[([^\]]*)\]|\(([^)]*)\)");
    // A file's checksum ([1A2B3C4D]) is each episode's own.
    let tags = tag
        .captures_iter(stem)
        .map(|c| c.get(1).or(c.get(2)).map_or("", |m| m.as_str()).trim().to_lowercase())
        .filter(|t| !t.is_empty() && !re(&CRC, r"^[0-9a-f]{8}$").is_match(t))
        .collect();
    let bare = tag.replace_all(stem, " ").to_lowercase();
    let mut words: Vec<String> = bare.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    // "05", "05v2", "e05", "s02e05", "第05話": the last word that is the episode's number.
    let episode = re(&EPISODE, r"^(s[0-9]{1,2}e|ep?|episode|第)?([0-9]{1,4})(?:v[0-9])?[話话回]?$");
    let (at, marked) = words.iter().enumerate().rev().find_map(|(i, w)| {
        let c = episode.captures(w)?;
        (c[2].parse::<i64>().ok() == Some(number)).then(|| (i, format!("{}#", c.get(1).map_or("", |m| m.as_str()))))
    })?;
    words[at] = marked;
    Some(Shape { tags, words, number })
}

/// What to search nyaa for to find more of a release: its group, when its name opens with one, and its title.
fn query(name: &str, shape: &Shape) -> String {
    let group = if name.trim_start().starts_with('[') { shape.tags.first() } else { None };
    let title: Vec<&str> = shape.words.iter().take_while(|w| !w.ends_with('#')).map(String::as_str).collect();
    let title = if title.is_empty() { shape.words.iter().filter(|w| !w.ends_with('#')).map(String::as_str).collect() } else { title };
    group.map(String::as_str).into_iter().chain(title).collect::<Vec<_>>().join(" ")
}

/// The releases among `found` that are later episodes of the release `like` is of, after episode
/// `after`: one for each episode, the newest upload of it (a v2 over the first), earliest episode first.
fn later<'a>(like: &Shape, after: i64, found: &'a [nyaa::Release]) -> Vec<(i64, &'a nyaa::Release)> {
    let mut picked: Vec<(i64, &nyaa::Release)> = Vec::new();
    for release in found {
        let Some(named) = shape(&release.title) else { continue };
        if named.same_release(like) && named.number > after && !picked.iter().any(|(n, r)| *n == named.number && r.id >= release.id) {
            picked.retain(|(n, _)| *n != named.number);
            picked.push((named.number, release));
        }
    }
    picked.sort_by_key(|(n, _)| *n);
    picked
}

/// What a look for a show's new episodes came to.
#[derive(Serialize, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Looked {
    /// The rip whose release is followed: the show's latest whose name gives its episode number.
    pub like: Option<String>,
    /// The episodes Telly started downloading.
    pub started: Vec<i64>,
}

/// The show's latest episode whose name says how its release names episodes, with that: a rip's
/// file name, or the title of the last release Telly downloaded for it, which is how nyaa lists it.
fn latest(backend: &Backend, show: i64) -> Result<Option<(String, Shape)>> {
    let files = backend.db.episodes(show)?.into_iter().filter_map(|e| Some(Path::new(&e.path).file_name()?.to_string_lossy().into_owned()));
    let downloaded = backend.db.setting_str(&like_setting(show));
    // Of two for the same episode, the release's title is the one to go by.
    let named = files.chain([downloaded]).filter_map(|name| Some((shape(&name)?, name)));
    Ok(named.max_by_key(|(shape, _)| shape.number).map(|(shape, name)| (name, shape)))
}

/// Looks on nyaa for episodes of `show` after its latest, from the same release, and starts downloading each.
pub fn look(backend: &Arc<Backend>, show: &Show) -> Result<Looked> {
    let Some((file, like)) = latest(backend, show.id)? else { return Ok(Looked::default()) };
    let after = like.number.max(backend.db.setting_i64(&got_setting(show.id), 0));
    let found = nyaa::search(&backend.db.setting_str("nyaa.site"), &query(&file, &like), "all", false, "newest", 1)?;
    let mut started = Vec::new();
    for (number, release) in later(&like, after, &found.releases) {
        match torrents::add(backend, &release.magnet, &release.torrent, &release.title, Some(show.id), None, None) {
            Ok(_) => started.push(number),
            // It's downloading already, or its file is here under another episode's number.
            Err(e) if e.to_string().contains("already") => {}
            Err(e) => return Err(e),
        }
        backend.db.set_setting(&got_setting(show.id), &json!(number))?;
    }
    Ok(Looked { like: Some(file), started })
}

/// Whether Telly has every episode AniList lists for a show, so there are none to come.
fn complete(backend: &Backend, show: &Show) -> bool {
    let have = backend.db.episodes(show.id).map_or(0, |e| e.len() as i64);
    show.anilist_episodes.is_some_and(|all| have >= all)
}

/// Follows a show, or stops. Following looks for its new episodes at once, and returns what that came to.
pub fn set(backend: &Arc<Backend>, show: i64, on: bool) -> Result<Value> {
    let found = backend.db.show(show)?.ok_or_else(|| anyhow::anyhow!("That show isn’t in Telly any more"))?;
    if !on {
        backend.db.set_setting(&setting(show), &Value::Null)?;
        backend.db.set_setting(&got_setting(show), &Value::Null)?;
        return Ok(json!({ "following": false }));
    }
    backend.db.set_setting(&setting(show), &Value::Bool(true))?;
    let (looked, error) = match look(backend, &found) {
        Ok(looked) => (looked, None),
        Err(e) => (Looked::default(), Some(format!("{e:#}"))),
    };
    Ok(json!({ "following": true, "looked": looked, "error": error }))
}

/// Looks for the new episodes of every show you follow, a couple of minutes after Telly opens and every hour after.
pub fn start(backend: Arc<Backend>) {
    let looking = move || {
        std::thread::sleep(FIRST_LOOK);
        loop {
            let followed = backend.db.shows().unwrap_or_default().into_iter().filter(|s| backend.db.setting_bool(&setting(s.id), false));
            for show in followed {
                // A download would take the connection from the game.
                if games::running(&backend).is_some() {
                    break;
                }
                if complete(&backend, &show) {
                    continue;
                }
                if let Err(e) = look(&backend, &show) {
                    eprintln!("follow: {}: {e:#}; looking again in an hour", show.title);
                }
                std::thread::sleep(BETWEEN_SHOWS);
            }
            std::thread::sleep(LOOK_EVERY);
        }
    };
    std::thread::Builder::new().name("telly-follow".into()).spawn(looking).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(id: i64, title: &str) -> nyaa::Release {
        nyaa::Release {
            id,
            title: title.into(),
            magnet: String::new(),
            torrent: String::new(),
            size: String::new(),
            bytes: 0,
            seeders: 0,
            leechers: 0,
            completed: 0,
            date: 0,
            trusted: false,
            remake: false,
            page: String::new(),
        }
    }

    fn numbers(like: &str, after: i64, titles: &[&str]) -> Vec<(i64, i64)> {
        let found: Vec<nyaa::Release> = titles.iter().enumerate().map(|(i, t)| release(100 - i as i64, t)).collect();
        later(&shape(like).unwrap(), after, &found).into_iter().map(|(n, r)| (n, r.id)).collect()
    }

    #[test]
    fn a_shape_is_the_name_without_its_number_and_checksum() {
        let five = shape("[SubsPlease] Sousou no Frieren - 05 (1080p) [1A2B3C4D].mkv").unwrap();
        assert_eq!(five.tags, ["subsplease", "1080p"]);
        assert_eq!(five.words, ["sousou", "no", "frieren", "#"]);
        assert_eq!(five.number, 5);
        let six = shape("[SubsPlease] Sousou no Frieren - 06v2 (1080p) [FFEE0011].mkv").unwrap();
        assert!(five.same_release(&six));
        assert_eq!(six.number, 6);
        assert_eq!(shape("Show.S02E05.1080p.WEB.H264-GRP.mkv").unwrap().words, ["show", "s02e#", "1080p", "web", "h264", "grp"]);
        // A title with a number in it keeps it: only the episode's is taken out.
        assert_eq!(shape("[Group] 86 - 05 [720p].mkv").unwrap().words, ["86", "#"]);
        assert_eq!(shape("[Group] Show (BD 1080p).mkv"), None);
    }

    #[test]
    fn the_search_is_for_the_group_and_the_title() {
        let name = "[SubsPlease] Sousou no Frieren - 05 (1080p) [1A2B3C4D].mkv";
        assert_eq!(query(name, &shape(name).unwrap()), "subsplease sousou no frieren");
        let scene = "Show.S02E05.1080p.WEB.H264-GRP.mkv";
        assert_eq!(query(scene, &shape(scene).unwrap()), "show");
    }

    #[test]
    fn later_episodes_come_from_the_same_release_only() {
        let like = "[SubsPlease] Sousou no Frieren - 05 (1080p) [1A2B3C4D].mkv";
        let found = [
            "[SubsPlease] Sousou no Frieren - 07 (1080p) [AAAAAAAA].mkv",
            "[SubsPlease] Sousou no Frieren - 07 (720p) [BBBBBBBB].mkv",
            "[Erai-raws] Sousou no Frieren - 07 [1080p][Multiple Subtitle]",
            "[SubsPlease] Sousou no Frieren - 06v2 (1080p) [CCCCCCCC].mkv",
            "[SubsPlease] Sousou no Frieren - 06 (1080p) [DDDDDDDD].mkv",
            "[SubsPlease] Sousou no Frieren (01-05) (1080p) [Batch]",
            "[SubsPlease] Sousou no Frieren - 05 (1080p) [1A2B3C4D].mkv",
            "[SubsPlease] Sousou no Frieren 2nd Season - 01 (1080p) [EEEEEEEE].mkv",
        ];
        // Episode 6 is its v2, the newer upload.
        assert_eq!(numbers(like, 5, &found), [(6, 97), (7, 100)]);
        // One already started isn't started again.
        assert_eq!(numbers(like, 6, &found), [(7, 100)]);
        assert_eq!(numbers(like, 7, &found), []);
    }

    #[test]
    fn a_season_is_not_another_seasons_episodes() {
        let like = "Show.S02E05.1080p.WEB.H264-GRP.mkv";
        let found = ["Show.S02E06.1080p.WEB.H264-GRP", "Show.S03E06.1080p.WEB.H264-GRP", "Show.S02E07.720p.WEB.H264-GRP"];
        assert_eq!(numbers(like, 5, &found), [(6, 100)]);
    }
}
