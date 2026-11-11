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
use std::collections::BTreeMap;
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
    Ok(())
}

/// Updates your list whenever an episode may have been watched, and every 10 minutes (for one that failed).
pub fn start(backend: Arc<Backend>) {
    std::thread::Builder::new()
        .name("telly-anilist".into())
        .spawn(move || {
            let mut seen = 0;
            loop {
                let token = backend.db.setting_str("anilist.token");
                if !token.is_empty() {
                    let error = update(&backend, &token).err().map(|e| json!(format!("{e:#}"))).unwrap_or(Value::Null);
                    let _ = backend.db.set_setting("anilist.error", &error);
                }
                backend.jobs.wait(&mut seen, Duration::from_secs(600));
            }
        })
        .ok();
}

/// Sends each entry's progress that's higher than what was last sent.
fn update(backend: &Backend, token: &str) -> Result<()> {
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
    for (entry, progress) in wanted {
        let sent = format!("anilist.sent.{entry}");
        if progress <= backend.db.setting_i64(&sent, 0) {
            continue;
        }
        if let Some(status) = change(&anilist::list_entry(token, entry)?, progress) {
            anilist::save_progress(token, entry, progress, status)?;
        }
        backend.db.set_setting(&sent, &json!(progress))?;
    }
    Ok(())
}

/// The AniList entry an episode is of, and its number there.
/// `group_entry` finds a later season folder's entry from its name and episode count.
fn place(show: &Show, episodes: &[Episode], episode: &Episode, group_entry: impl FnOnce(&str, usize) -> Result<Option<i64>>) -> Result<Option<(i64, i64)>> {
    let Some(own) = show.anilist_id else { return Ok(None) };
    let listing: Vec<(i64, String)> = episodes.iter().map(|e| (e.number, e.path.clone())).collect();
    let groups = matching::groups(&show.folder, &listing);
    if groups.len() <= 1 {
        return Ok(Some((own, episode.number)));
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

/// A later season folder's own AniList entry, worked out once and kept.
fn group_entry(backend: &Backend, show: &Show, group: &str, count: usize) -> Result<Option<i64>> {
    let setting = format!("anilist.group.{}.{group}", show.id);
    let id = match backend.db.setting(&setting)?.and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => {
            let title = scan::clean_title(group.rsplit('/').next().unwrap_or(group));
            let mut found = anilist::search_loosely(&title)?;
            anilist::rank(&mut found, &title, count);
            // The show's own entry again would mean the folders aren't seasons of their own.
            let id = found.first().filter(|a| a.score >= SURE && Some(a.id) != show.anilist_id).map_or(-1, |a| a.id);
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
