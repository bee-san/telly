//! Your AniList list: once you've signed in (Settings → AniList), each
//! episode you watch in Telly raises that anime's progress on your list.
//!
//! Only episodes watched since signing in count, and progress only ever goes
//! up: rewatching an earlier episode, or marking one unwatched, changes
//! nothing. An entry reaching its last episode is marked completed.
//!
//! A show added as one folder holding several seasons has an AniList entry
//! for each season folder. The first folder is the show's own entry; a later
//! one is looked up by its folder's name, and logs nothing unless AniList is
//! fairly sure which entry it is. A number past the entry's episode count
//! logs nothing either: better nothing than the wrong show.
//!
//! A copy of which anime are on your list (all but those you only plan to
//! watch) is kept in anilist-list.json, for Recommended to leave out. It's
//! fetched again at start-up, on signing in, and after Telly updates the list.
//!
//! Settings: anilist.token (the access token), anilist.user (its name),
//! anilist.since (unix ms: when you signed in), anilist.error (the last
//! update that failed), anilist.sent.<entry> (the progress last sent) and
//! anilist.group.<show>.<folder> (a season folder's entry, -1 for none).

use crate::anilist::{self, ListEntry};
use crate::backend::Backend;
use crate::db::{Episode, Show};
use crate::util::now_ms;
use crate::{matching, scan};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How sure a season folder's search must be before its entry is used.
const SURE: f64 = 0.5;

/// Signs in with an access token from AniList; returns the user's name.
pub fn sign_in(backend: &Backend, token: &str) -> Result<String> {
    let token = token.trim();
    let user = anilist::viewer(token)?;
    backend.db.set_setting("anilist.token", &json!(token))?;
    backend.db.set_setting("anilist.user", &json!(user))?;
    backend.db.set_setting("anilist.since", &json!(now_ms()))?;
    backend.db.set_setting("anilist.error", &Value::Null)?;
    backend.jobs.poke();
    Ok(user)
}

pub fn sign_out(backend: &Backend) -> Result<()> {
    for key in ["anilist.token", "anilist.user", "anilist.since", "anilist.error"] {
        backend.db.set_setting(key, &Value::Null)?;
    }
    let _ = std::fs::remove_file(list_path(backend));
    Ok(())
}

fn list_path(backend: &Backend) -> PathBuf {
    backend.dirs.local.join("anilist-list.json")
}

/// The anime on your list, from the copy kept on disk (none when signed out).
pub fn on_list(backend: &Backend) -> HashSet<i64> {
    std::fs::read(list_path(backend)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn fetch_list(backend: &Backend, token: &str) -> Result<()> {
    let ids = anilist::watched_ids(token)?;
    crate::util::write_whole(&list_path(backend), serde_json::to_vec(&ids)?)?;
    Ok(())
}

/// Updates your list whenever an episode may have been watched, and every 10 minutes (for one that failed).
pub fn start(backend: Arc<Backend>) {
    std::thread::Builder::new()
        .name("telly-anilist".into())
        .spawn(move || {
            let mut seen = 0;
            // The token the copy of the list was fetched with: fetched again at start-up and on signing in.
            let mut listed = String::new();
            loop {
                let token = backend.db.setting_str("anilist.token");
                if !token.is_empty() {
                    let result = update(&backend, &token).and_then(|sent| {
                        if sent || listed != token || !list_path(&backend).is_file() {
                            fetch_list(&backend, &token)?;
                            listed = token.clone();
                        }
                        Ok(())
                    });
                    let error = result.err().map(|e| json!(format!("{e:#}"))).unwrap_or(Value::Null);
                    let _ = backend.db.set_setting("anilist.error", &error);
                }
                backend.jobs.wait(&mut seen, Duration::from_secs(600));
            }
        })
        .ok();
}

/// Sends each entry's progress that's higher than what was last sent; true if the list changed.
fn update(backend: &Backend, token: &str) -> Result<bool> {
    let since = backend.db.setting_i64("anilist.since", i64::MAX);
    let mut wanted: BTreeMap<i64, i64> = BTreeMap::new();
    for show in backend.db.shows()?.into_iter().filter(|s| s.anilist_id.is_some()) {
        let episodes = backend.db.episodes(show.id)?;
        for e in episodes.iter().filter(|e| e.watched_at.is_some_and(|at| at >= since)) {
            if let Some((entry, progress)) = place(&show, &episodes, e, |group, count| group_entry(backend, &show, group, count))? {
                let best = wanted.entry(entry).or_insert(0);
                *best = (*best).max(progress);
            }
        }
    }
    let mut sent_any = false;
    for (entry, progress) in wanted {
        let sent = format!("anilist.sent.{entry}");
        if progress <= backend.db.setting_i64(&sent, 0) {
            continue;
        }
        if let Some(status) = change(&anilist::list_entry(token, entry)?, progress) {
            anilist::save_progress(token, entry, progress, status)?;
            sent_any = true;
        }
        backend.db.set_setting(&sent, &json!(progress))?;
    }
    Ok(sent_any)
}

/// The AniList entry an episode is of, and its number there.
/// `group_entry` finds a later season folder's entry from its name and episode count.
pub(crate) fn place(show: &Show, episodes: &[Episode], episode: &Episode, group_entry: impl FnOnce(&str, usize) -> Result<Option<i64>>) -> Result<Option<(i64, i64)>> {
    let Some(own) = show.anilist_id else { return Ok(None) };
    let listing: Vec<(i64, String)> = episodes.iter().map(|e| (e.number, e.path.clone())).collect();
    let groups = matching::groups(&show.folder, &listing);
    if groups.len() <= 1 {
        return Ok(Some((own, from_one(show.anilist_episodes, &listing, episode.number))));
    }
    let group = matching::group_of(&show.folder, &episode.path);
    let first = listing.iter().filter(|(_, p)| matching::group_of(&show.folder, p) == group).map(|(n, _)| *n).min().unwrap_or(episode.number);
    let number = episode.number - first + 1;
    if groups.first().is_some_and(|(g, _)| *g == group) {
        return Ok(Some((own, number)));
    }
    let count = groups.iter().find(|(g, _)| *g == group).map_or(0, |(_, c)| *c);
    Ok(group_entry(&group, count)?.map(|entry| (entry, number)))
}

/// An episode's number in a show's one entry. Files that go on from an
/// earlier season's numbers (Spy x Family 13–25, matched to the 13 episodes of
/// Part 2) are numbered from 1 there, when that's what makes them fit:
/// otherwise the first would be logged as the last, and the season completed.
fn from_one(entry_episodes: Option<i64>, listing: &[(i64, String)], number: i64) -> i64 {
    let first = listing.iter().map(|(n, _)| *n).min().unwrap_or(number);
    let last = listing.iter().map(|(n, _)| *n).max().unwrap_or(number);
    match entry_episodes {
        Some(count) if first > 1 && last > count && last - (first - 1) <= count => number - (first - 1),
        _ => number,
    }
}

/// Kept for a season folder AniList has no entry for. -1 was kept before a
/// folder named only by its season was searched by the show's title, so
/// it's asked again.
pub(crate) const NO_ENTRY: i64 = -2;
pub(crate) const NO_ENTRY_BEFORE: i64 = -1;

/// What a later season folder is searched for on AniList: its name without
/// the release's words, with the season it names ("Fairy Tail Season 2"). A
/// folder named by its season alone ("Season 2", "2nd Season", "Final
/// Season") goes by the show's title; "Season 0" is the show's specials.
pub(crate) fn group_title(show: &Show, group: &str) -> String {
    let folder = group.rsplit('/').next().unwrap_or(group);
    let cleaned = scan::clean_title(folder);
    let ordinal = |w: &str| {
        let w = w.to_lowercase();
        matches!(w.as_str(), "first" | "second" | "third" | "fourth" | "fifth" | "final" | "ii" | "iii" | "iv" | "v")
            || w.trim_end_matches(|c: char| c.is_ascii_alphabetic()).chars().all(|c| c.is_ascii_digit())
                && w.chars().next().is_some_and(|c| c.is_ascii_digit())
    };
    let named = cleaned.split_whitespace().any(|w| !ordinal(w));
    let show_title = if show.title_romaji.trim().is_empty() { show.title.clone() } else { show.title_romaji.clone() };
    let base = if named { cleaned } else { show_title };
    let lower = folder.to_lowercase();
    match anilist::season(folder) {
        Some(0) => format!("{base} Specials"),
        Some(s) if s > 1 && anilist::season(&base) != Some(s) => format!("{base} Season {s}"),
        _ if !named && lower.contains("final season") => format!("{base} Final Season"),
        _ => base,
    }
}

/// A later season folder's own AniList entry, worked out once and kept.
pub(crate) fn group_entry(backend: &Backend, show: &Show, group: &str, count: usize) -> Result<Option<i64>> {
    let setting = format!("anilist.group.{}.{group}", show.id);
    let id = match backend.db.setting(&setting)?.and_then(|v| v.as_i64()) {
        Some(id) if id != NO_ENTRY_BEFORE => id,
        _ => {
            let title = group_title(show, group);
            let mut found = anilist::search_loosely(&title)?;
            anilist::rank(&mut found, &title, count);
            anilist::follow_sequels(&mut found, &title)?;
            // The show's own entry again would mean the folders aren't seasons of their own.
            let id = found.first().filter(|a| a.score >= SURE && Some(a.id) != show.anilist_id).map_or(NO_ENTRY, |a| a.id);
            backend.db.set_setting(&setting, &json!(id))?;
            id
        }
    };
    Ok((id > 0).then_some(id))
}

/// The status to send with `progress`, or None when the list needs no change
/// (it's that far already, or the number is past the entry's last episode).
fn change(entry: &ListEntry, progress: i64) -> Option<&'static str> {
    if progress <= entry.progress || entry.episodes.is_some_and(|n| progress > n) {
        return None;
    }
    Some(if entry.episodes == Some(progress) {
        "COMPLETED"
    } else if entry.status.as_deref() == Some("REPEATING") {
        "REPEATING"
    } else {
        "CURRENT"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show() -> Show {
        Show {
            id: 1,
            folder: "D:\\Anime\\Fairy Tail".into(),
            title: "Fairy Tail".into(),
            title_native: String::new(),
            title_english: String::new(),
            title_romaji: String::new(),
            anilist_id: Some(6702),
            anilist_episodes: Some(175),
            format: "TV".into(),
            year: None,
            cover_url: String::new(),
            cover_color: String::new(),
            jimaku_id: None,
            added_at: 0,
            last_watched_at: None,
        }
    }

    fn ep(number: i64, path: &str) -> Episode {
        Episode { number, path: path.into(), ..Default::default() }
    }

    #[test]
    fn a_season_folder_logs_to_its_own_entry_numbered_from_one() {
        let s = show();
        let one = vec![ep(1, "D:\\Anime\\Fairy Tail\\01.mkv"), ep(2, "D:\\Anime\\Fairy Tail\\02.mkv")];
        assert_eq!(place(&s, &one, &one[1], |_, _| panic!("one folder needs no search")).unwrap(), Some((6702, 2)));
        let seasons = vec![
            ep(1, "D:\\Anime\\Fairy Tail\\Fairy Tail (2009)\\01.mkv"),
            ep(2, "D:\\Anime\\Fairy Tail\\Fairy Tail (2009)\\02.mkv"),
            ep(3, "D:\\Anime\\Fairy Tail\\Fairy Tail 2014\\01.mkv"),
            ep(4, "D:\\Anime\\Fairy Tail\\Fairy Tail 2014\\02.mkv"),
        ];
        assert_eq!(place(&s, &seasons, &seasons[1], |_, _| panic!("the first folder is the show's own")).unwrap(), Some((6702, 2)));
        let found = place(&s, &seasons, &seasons[3], |group, count| {
            assert_eq!((group, count), ("Fairy Tail 2014", 2));
            Ok(Some(20626))
        });
        assert_eq!(found.unwrap(), Some((20626, 2)));
        // A folder AniList isn't sure about logs nothing.
        assert_eq!(place(&s, &seasons, &seasons[2], |_, _| Ok(None)).unwrap(), None);
    }

    #[test]
    fn files_numbered_on_from_the_last_season_log_from_one() {
        // Spy x Family 13–25 in one folder, matched to the 13 episodes of Part 2.
        let s = Show { anilist_id: Some(158927), anilist_episodes: Some(13), ..show() };
        let files: Vec<Episode> = (13..=25).map(|n| ep(n, &format!("Spy x Family - {n}.mkv"))).collect();
        let none = |_: &str, _| panic!("one folder needs no search");
        assert_eq!(place(&s, &files, &files[0], none).unwrap(), Some((158927, 1)));
        assert_eq!(place(&s, &files, &files[12], none).unwrap(), Some((158927, 13)));
        // Numbers that already fit, or a show AniList hasn't counted, stay as they are.
        let late: Vec<Episode> = (12..=13).map(|n| ep(n, &format!("Show - {n}.mkv"))).collect();
        assert_eq!(place(&s, &late, &late[1], none).unwrap(), Some((158927, 13)));
        let airing = Show { anilist_episodes: None, ..s.clone() };
        assert_eq!(place(&airing, &files, &files[0], none).unwrap(), Some((158927, 13)));
        // Two cours in one folder against the first's 13 episodes: the second's don't fit either way.
        let both: Vec<Episode> = (1..=25).map(|n| ep(n, &format!("Spy x Family - {n}.mkv"))).collect();
        assert_eq!(place(&s, &both, &both[13], none).unwrap(), Some((158927, 14)));
    }

    #[test]
    fn a_folder_named_by_its_season_alone_is_searched_by_the_show_s_title() {
        let s = Show { title_romaji: "Fairy Tail".into(), ..show() };
        assert_eq!(group_title(&s, "Season 2"), "Fairy Tail Season 2");
        assert_eq!(group_title(&s, "Fairy Tail/2nd Season"), "Fairy Tail Season 2");
        assert_eq!(group_title(&s, "Season 03 [BD 1080p]"), "Fairy Tail Season 3");
        assert_eq!(group_title(&s, "Final Season"), "Fairy Tail Final Season");
        assert_eq!(group_title(&s, "Season 00"), "Fairy Tail Specials");
        // A folder with a name of its own keeps it, and its season.
        assert_eq!(group_title(&s, "Fairy Tail 2014"), "Fairy Tail 2014");
        assert_eq!(group_title(&s, "Fairy Tail Season 2 [BD]"), "Fairy Tail Season 2");
        assert_eq!(group_title(&s, "Konosuba 2"), "Konosuba 2");
    }

    #[test]
    fn progress_only_goes_up_and_never_past_the_last_episode() {
        let entry = |episodes, status: Option<&str>, progress| ListEntry { episodes, status: status.map(str::to_string), progress };
        assert_eq!(change(&entry(Some(12), None, 0), 1), Some("CURRENT"));
        assert_eq!(change(&entry(Some(12), Some("PAUSED"), 4), 5), Some("CURRENT"));
        assert_eq!(change(&entry(Some(12), Some("CURRENT"), 11), 12), Some("COMPLETED"));
        assert_eq!(change(&entry(Some(12), Some("REPEATING"), 2), 3), Some("REPEATING"));
        assert_eq!(change(&entry(None, Some("CURRENT"), 500), 501), Some("CURRENT"));
        // Rewatching an earlier episode, or one past the entry's end.
        assert_eq!(change(&entry(Some(12), Some("CURRENT"), 8), 3), None);
        assert_eq!(change(&entry(Some(12), None, 0), 13), None);
    }
}
