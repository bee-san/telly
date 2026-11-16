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
//! no episode number has nothing to go by, and nothing is downloaded for it:
//! better none than the wrong one. An episode's title in a name ("- 05 - The
//! Return") is each episode's own, so it's left out when names are compared.
//!
//! Settings: follow.<show> (the show is followed), follow.got.<show> (the
//! last episode Telly started a download of, so none is started twice),
//! follow.started.<show> (each episode it started and its download's info
//! hash, so one whose download is gone is started again) and
//! follow.like.<show> (the title of the last release downloaded for the show,
//! which is how nyaa names its episodes when the files inside are named
//! another way).
//!
//! Nobody is looking when these download, and anyone can upload under a
//! release's name, so a release is taken only when it's like the episodes
//! before it (`later`): the same kind of uploader, about the same size.

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

/// The setting with the downloads Telly started for a show: each episode's number and its info hash.
pub(crate) fn started_setting(show: i64) -> String {
    format!("follow.started.{show}")
}

/// The setting with the title of the last release downloaded for a show, as nyaa lists it.
pub(crate) fn like_setting(show: i64) -> String {
    format!("follow.like.{show}")
}

/// How a release names an episode: its tags, its words with the episode's number taken out, and the number.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Shape {
    pub(crate) tags: Vec<String>,
    pub(crate) words: Vec<String>,
    pub(crate) number: i64,
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
pub(crate) fn shape(name: &str) -> Option<Shape> {
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
    // An episode's title after its number is each episode's own: "- 05 - The Return", "05「帰還」",
    // and "S01E05 The Return 1080p", where the release's details come after it.
    static RESOLUTION: OnceLock<Regex> = OnceLock::new();
    static TITLED: OnceLock<Regex> = OnceLock::new();
    let details = words[at + 1..].iter().position(|w| re(&RESOLUTION, r"^[0-9]{3,4}p$").is_match(w));
    let titled = || re(&TITLED, r"([0-9]{1,4})(?:v[0-9])?[話话回]?(?:\s+-\s+\S|\s*[「『])").captures_iter(&bare).any(|c| c[1].parse::<i64>().ok() == Some(number));
    match details {
        Some(n) => drop(words.drain(at + 1..at + 1 + n)),
        None if titled() => words.truncate(at + 1),
        None => {}
    }
    Some(Shape { tags, words, number })
}

/// What to search nyaa for to find more of a release: its group, when its name opens with one, and its title.
fn query(name: &str, shape: &Shape) -> String {
    let group = if name.trim_start().starts_with('[') { shape.tags.first() } else { None };
    let title: Vec<&str> = shape.words.iter().take_while(|w| !w.ends_with('#')).map(String::as_str).collect();
    let title = if title.is_empty() { shape.words.iter().filter(|w| !w.ends_with('#')).map(String::as_str).collect() } else { title };
    group.map(String::as_str).into_iter().chain(title).collect::<Vec<_>>().join(" ")
}

/// The releases among `found` that are episodes `wanted` of the release
/// `like` is of: one for each episode, earliest episode first.
///
/// Anyone can upload under a release's name, and Telly downloads these with
/// nobody looking, so a release is only taken when it's like the episodes of
/// it that came before (those in `found` that aren't `wanted`):
/// - from a trusted uploader, when every one of those was, and not a remake when none of them was;
/// - between a third of and three times their usual size, or the size `rip` of your latest rip;
/// - with someone sharing it.
///
/// Of two uploads of an episode, the later version is taken (a v2 over the
/// first); of two of the same version, the first one up, which a copy of its
/// name comes after.
fn later<'a>(like: &Shape, wanted: impl Fn(i64) -> bool, rip: Option<u64>, found: &'a [nyaa::Release]) -> Vec<(i64, &'a nyaa::Release)> {
    let of_it: Vec<(Shape, &nyaa::Release)> = found.iter().filter_map(|r| Some((shape(&r.title)?, r))).filter(|(named, _)| named.same_release(like)).collect();
    let before: Vec<&nyaa::Release> = of_it.iter().filter(|(named, _)| !wanted(named.number)).map(|(_, r)| *r).collect();
    let mut sizes: Vec<u64> = before.iter().map(|r| r.bytes).filter(|b| *b > 0).collect();
    sizes.sort_unstable();
    let usual = sizes.get(sizes.len() / 2).copied().or(rip).filter(|b| *b > 0);
    let trusted = !before.is_empty() && before.iter().all(|r| r.trusted);
    let no_remakes = !before.is_empty() && before.iter().all(|r| !r.remake);
    let like_them = |r: &nyaa::Release| {
        let size = usual.is_none_or(|usual| r.bytes == 0 || (r.bytes >= usual / 3 && r.bytes <= usual.saturating_mul(3)));
        r.seeders > 0 && size && (r.trusted || !trusted) && (!r.remake || !no_remakes)
    };
    let mut picked: Vec<(i64, &nyaa::Release)> = Vec::new();
    for (named, release) in of_it.iter().filter(|(named, r)| wanted(named.number) && like_them(r)) {
        let better = |other: &nyaa::Release| (scan::version(&other.title), -other.id) >= (scan::version(&release.title), -release.id);
        if !picked.iter().any(|(n, r)| *n == named.number && better(r)) {
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

/// The downloads Telly started for a show that haven't been seen to finish: each episode's number and its info hash.
fn started(backend: &Backend, show: i64) -> Vec<(i64, String)> {
    backend.db.setting(&started_setting(show)).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// The episodes among `started` to download again: those whose download isn't
/// among the downloads `there` any more (removed in Processing, or nobody
/// ever answered for its magnet link). One that finished and was then
/// removed is found on disk when it's tried, and left at that.
fn to_retry(started: &[(i64, String)], there: Option<&std::collections::HashSet<String>>) -> Vec<i64> {
    let Some(there) = there else { return Vec::new() };
    started.iter().filter(|(_, hash)| !there.contains(hash)).map(|(number, _)| *number).collect()
}

/// Looks on nyaa for episodes of `show` after its latest, from the same
/// release, and starts downloading each; and again those it started before
/// whose download is gone without the episode.
pub fn look(backend: &Arc<Backend>, show: &Show) -> Result<Looked> {
    let Some((file, like)) = latest(backend, show.id)? else { return Ok(Looked::default()) };
    let after = like.number.max(backend.db.setting_i64(&got_setting(show.id), 0));
    let mut going = started(backend, show.id);
    let retry = to_retry(&going, backend.downloads.there().as_ref());
    let rip = backend.db.episodes(show.id)?.iter().map(|e| e.size.max(0) as u64).max();
    let found = nyaa::search(&backend.db.setting_str("nyaa.site"), &query(&file, &like), "all", false, "newest", 1)?;
    let mut begun = Vec::new();
    let mut failed = None;
    for (number, release) in later(&like, |n| n > after || retry.contains(&n), rip, &found.releases) {
        going.retain(|(n, _)| *n != number);
        match torrents::add(backend, &release.magnet, &release.torrent, &release.title, Some(show.id), None, None) {
            Ok(_) => {
                begun.push(number);
                going.extend(torrents::magnet_hash(&release.magnet).map(|hash| (number, hash)));
            }
            // It's downloading already, or its file is here: there's nothing more to start.
            Err(e) if e.to_string().contains("already") => {}
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
        backend.db.set_setting(&got_setting(show.id), &json!(number.max(after)))?;
    }
    backend.db.set_setting(&started_setting(show.id), &json!(going))?;
    match failed {
        Some(e) => Err(e),
        None => Ok(Looked { like: Some(file), started: begun }),
    }
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
        backend.db.set_setting(&started_setting(show), &Value::Null)?;
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
            seeders: 10,
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
        later(&shape(like).unwrap(), |n| n > after, None, &found).into_iter().map(|(n, r)| (n, r.id)).collect()
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
    fn an_episode_s_title_is_left_out_of_its_shape() {
        let five = shape("[Group] Show - 05 - The Return [1080p].mkv").unwrap();
        assert_eq!(five.words, ["show", "#"]);
        assert!(five.same_release(&shape("[Group] Show - 06 - A Second Title, With 2 Parts [1080p].mkv").unwrap()));
        assert!(five.same_release(&shape("[Group] Show - 06「帰還」[1080p].mkv").unwrap()));
        let scene = shape("Sousou no Frieren S01E05 Phantoms of the Dead 1080p CR WEB-DL AAC2.0 H 264-VARYG").unwrap();
        assert_eq!(scene.words, ["sousou", "no", "frieren", "s01e#", "1080p", "cr", "web", "dl", "aac2", "0", "h", "264", "varyg"]);
        assert!(scene.same_release(&shape("Sousou no Frieren S01E06 A Privileged Wish 1080p CR WEB-DL AAC2.0 H 264-VARYG").unwrap()));
        // What comes after the number without a title's dash, or details after it, stays: it may be the release's.
        assert_eq!(shape("[Group] Show - 05 END [1080p].mkv").unwrap().words, ["show", "#", "end"]);
        // A number in the title is the title's own.
        assert_eq!(shape("[Group] 86 - 05 - Title [720p].mkv").unwrap().words, ["86", "#"]);
    }

    #[test]
    fn later_episodes_with_their_own_titles_come_from_the_same_release() {
        let like = "Sousou no Frieren S01E05 Phantoms of the Dead 1080p CR WEB-DL AAC2.0 H 264-VARYG";
        let found = [
            "Sousou no Frieren S01E07 Like a Fairy-tale 1080p CR WEB-DL AAC2.0 H 264-VARYG",
            "Sousou no Frieren S01E06 A Privileged Wish 720p CR WEB-DL AAC2.0 H 264-VARYG",
            "Sousou no Frieren S01E06 A Privileged Wish 1080p CR WEB-DL AAC2.0 H 264-VARYG",
        ];
        assert_eq!(numbers(like, 5, &found), [(6, 98), (7, 100)]);
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
        // Episode 6 is its v2, the later version.
        assert_eq!(numbers(like, 5, &found), [(6, 97), (7, 100)]);
        // One already started isn't started again.
        assert_eq!(numbers(like, 6, &found), [(7, 100)]);
        assert_eq!(numbers(like, 7, &found), []);
    }

    #[test]
    fn a_release_under_the_same_name_is_only_taken_when_it_s_like_the_ones_before() {
        let like = shape("[SubsPlease] Sousou no Frieren - 05 (1080p) [1A2B3C4D].mkv").unwrap();
        let mb = 1024 * 1024;
        let upload = |id: i64, number: &str, trusted: bool, bytes: u64, seeders: i64| nyaa::Release {
            trusted,
            bytes,
            seeders,
            ..release(id, &format!("[SubsPlease] Sousou no Frieren - {number} (1080p) [AAAAAAAA].mkv"))
        };
        let before = [upload(1, "04", true, 1400 * mb, 300), upload(2, "05", true, 1380 * mb, 400)];
        let picked = |sixes: &[nyaa::Release]| -> Vec<i64> {
            let found: Vec<nyaa::Release> = sixes.iter().chain(&before).cloned().collect();
            later(&like, |n| n > 5, None, &found).into_iter().map(|(_, r)| r.id).collect()
        };
        // The group's own upload, and someone's copy of its name uploaded after: the first one up is taken.
        assert_eq!(picked(&[upload(4, "06", true, 1390 * mb, 80), upload(3, "06", true, 1390 * mb, 500)]), [3]);
        // Not one from an uploader nyaa doesn't trust, when the episodes before were from one it does.
        assert_eq!(picked(&[upload(3, "06", false, 1390 * mb, 500)]), [] as [i64; 0]);
        // Nor one nothing like an episode's size, nor one nobody shares.
        assert_eq!(picked(&[upload(3, "06", true, 3 * mb, 500)]), [] as [i64; 0]);
        assert_eq!(picked(&[upload(3, "06", true, 1390 * mb, 0)]), [] as [i64; 0]);
        // A v2 is taken over the first upload, though it came later.
        assert_eq!(picked(&[upload(4, "06v2", true, 1390 * mb, 80), upload(3, "06", true, 1390 * mb, 500)]), [4]);
        // With no earlier episode listed, your latest rip's size is what an episode's is.
        let alone = [upload(3, "06", false, 3 * mb, 500), upload(4, "07", false, 1300 * mb, 500)];
        assert_eq!(later(&like, |n| n > 5, Some(1400 * mb), &alone).into_iter().map(|(_, r)| r.id).collect::<Vec<_>>(), [4]);
    }

    #[test]
    fn an_episode_whose_download_is_gone_is_started_again() {
        let started = [(6, "aaaa".to_string()), (7, "bbbb".to_string())];
        let there: std::collections::HashSet<String> = ["bbbb".to_string()].into();
        // Episode 6's download was removed, or nobody answered for it; 7's is still there.
        assert_eq!(to_retry(&started, Some(&there)), [6]);
        // Before Telly has looked at its downloads, none counts as gone.
        assert_eq!(to_retry(&started, None), [] as [i64; 0]);
        let like = "[SubsPlease] Sousou no Frieren - 07 (1080p) [1A2B3C4D].mkv";
        let found = ["[SubsPlease] Sousou no Frieren - 08 (1080p)", "[SubsPlease] Sousou no Frieren - 07 (1080p)", "[SubsPlease] Sousou no Frieren - 06 (1080p)"];
        let found: Vec<nyaa::Release> = found.iter().enumerate().map(|(i, t)| release(100 - i as i64, t)).collect();
        let numbers: Vec<i64> = later(&shape(like).unwrap(), |n| n > 7 || n == 6, None, &found).into_iter().map(|(n, _)| n).collect();
        assert_eq!(numbers, [6, 8]);
    }

    #[test]
    fn a_season_is_not_another_seasons_episodes() {
        let like = "Show.S02E05.1080p.WEB.H264-GRP.mkv";
        let found = ["Show.S02E06.1080p.WEB.H264-GRP", "Show.S03E06.1080p.WEB.H264-GRP", "Show.S02E07.720p.WEB.H264-GRP"];
        assert_eq!(numbers(like, 5, &found), [(6, 100)]);
    }
}
