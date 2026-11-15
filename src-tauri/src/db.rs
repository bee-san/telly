//! telly.sqlite, in %LOCALAPPDATA%\Telly: your shows, their episodes, where
//! you are in each, when you finished or rewatched a show, subtitle and conversion
//! state, settings and the queue of cards waiting for their picture and audio.
//!
//! The schema is a list of migrations; PRAGMA user_version counts how many
//! have run. Add new ones to the end, never edit old ones.

use crate::util::now_ms;
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

const MIGRATIONS: &[&str] = &["
CREATE TABLE shows (
  id               INTEGER PRIMARY KEY,
  folder           TEXT NOT NULL,
  title            TEXT NOT NULL,              -- short title for the window and cards (romaji or English)
  title_native     TEXT NOT NULL DEFAULT '',
  title_english    TEXT NOT NULL DEFAULT '',
  title_romaji     TEXT NOT NULL DEFAULT '',
  anilist_id       INTEGER,
  anilist_episodes INTEGER,
  format           TEXT NOT NULL DEFAULT '',
  year             INTEGER,
  cover_url        TEXT NOT NULL DEFAULT '',
  cover_color      TEXT NOT NULL DEFAULT '',
  jimaku_id        INTEGER,                    -- Jimaku's entry, once found
  added_at         INTEGER NOT NULL,           -- unix ms
  last_watched_at  INTEGER
);
CREATE TABLE episodes (
  id               INTEGER PRIMARY KEY,
  show_id          INTEGER NOT NULL REFERENCES shows (id) ON DELETE CASCADE,
  number           INTEGER NOT NULL,
  path             TEXT NOT NULL,              -- the rip, never moved or changed
  size             INTEGER NOT NULL DEFAULT 0,
  duration         REAL NOT NULL DEFAULT 0,    -- seconds
  media_state      TEXT NOT NULL DEFAULT 'pending',  -- pending, working, ready, failed
  media_plan       TEXT NOT NULL DEFAULT '',   -- direct, remux, audio or video (see media.rs)
  media_progress   REAL NOT NULL DEFAULT 0,
  media_error      TEXT NOT NULL DEFAULT '',
  play_path        TEXT NOT NULL DEFAULT '',   -- what the player loads: the rip or Telly's copy
  audio_index      INTEGER NOT NULL DEFAULT 0, -- the Japanese track among play_path's audio tracks
  probe            TEXT NOT NULL DEFAULT '',   -- JSON: what ffprobe found in the rip
  ending_at        REAL,                       -- where the ending starts, from the chapters
  position         REAL NOT NULL DEFAULT 0,    -- seconds in
  watched_at       INTEGER,
  last_played_at   INTEGER,
  subs_state       TEXT NOT NULL DEFAULT 'pending',  -- pending, working, synced, check, none, nokey, failed, manual
  subs_detail      TEXT NOT NULL DEFAULT '',
  subs_file        TEXT NOT NULL DEFAULT '',   -- the source file's name
  subs_score       REAL,                       -- 0 to 1
  subs_offset_ms   INTEGER,                    -- the sync's main shift
  subs_nudge_ms    INTEGER NOT NULL DEFAULT 0  -- your own nudges (Z and X)
);
CREATE INDEX episodes_by_show ON episodes (show_id, number);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE mine_jobs (
  id          INTEGER PRIMARY KEY,
  note_id     INTEGER NOT NULL,
  episode_id  INTEGER NOT NULL,
  start       REAL NOT NULL,                   -- the clip, in seconds
  end         REAL NOT NULL,
  sentence    TEXT NOT NULL DEFAULT '',
  state       TEXT NOT NULL DEFAULT 'pending', -- pending, done, failed, skipped
  detail      TEXT NOT NULL DEFAULT '',
  tries       INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);
", "
-- What Claude said: a line explained, or the recap shown before an episode.
CREATE TABLE answers (
  episode_id  INTEGER NOT NULL REFERENCES episodes (id) ON DELETE CASCADE,
  kind        TEXT NOT NULL,                 -- line or recap
  line        INTEGER NOT NULL DEFAULT -1,   -- the line's place in the subtitles, for line
  about       TEXT NOT NULL DEFAULT '',      -- the line's text, or what the recap was made from
  text        TEXT NOT NULL,
  extra       TEXT NOT NULL DEFAULT '',      -- JSON: the recap's moments
  created_at  INTEGER NOT NULL,
  PRIMARY KEY (episode_id, kind, line)
);
", "
-- The rip's audio track that play_path plays, as a position among its audio
-- tracks. NULL: the Japanese one, as it was picked when the episode was got ready.
ALTER TABLE episodes ADD COLUMN audio_track INTEGER;
", "
-- How much of an episode's words you know (premine.rs): the uses of words in
-- it, and how many are of known words. Kept so the Library and a show's page
-- have it at once, and with Anki closed.
CREATE TABLE word_scores (
  episode_id  INTEGER PRIMARY KEY REFERENCES episodes (id) ON DELETE CASCADE,
  uses        INTEGER NOT NULL,
  known       INTEGER NOT NULL,
  stamp       TEXT NOT NULL            -- the state of your known words it was worked out from (known.rs)
);
", "
-- Where AniSkip says an episode's opening is (aniskip.rs), asked for when the
-- rip's chapters don't say. NULL start and end: AniSkip has none for it.
CREATE TABLE openings (
  episode_id  INTEGER PRIMARY KEY REFERENCES episodes (id) ON DELETE CASCADE,
  start       REAL,
  end         REAL
);
", "
-- When AniSkip was asked (unix ms). An answer of none is asked again a week
-- on: people mark an episode's opening in the days after it airs.
ALTER TABLE openings ADD COLUMN asked_at INTEGER NOT NULL DEFAULT 0;
", "
-- The last show and episode ids given out. A removed show's ids aren't given
-- out again: what's kept by id (settings, the files in subtitles, line
-- search) would otherwise go to the show added next.
CREATE TABLE last_ids (name TEXT PRIMARY KEY, id INTEGER NOT NULL);
INSERT INTO last_ids (name, id) SELECT 'shows', IFNULL(MAX(id), 0) FROM shows;
INSERT INTO last_ids (name, id) SELECT 'episodes', IFNULL(MAX(id), 0) FROM episodes;
CREATE TRIGGER shows_last_id AFTER INSERT ON shows
  BEGIN UPDATE last_ids SET id = MAX(id, NEW.id) WHERE name = 'shows'; END;
CREATE TRIGGER episodes_last_id AFTER INSERT ON episodes
  BEGIN UPDATE last_ids SET id = MAX(id, NEW.id) WHERE name = 'episodes'; END;
", "
-- Where AniSkip says the ending is, asked with the opening. ed_asked 0: asked
-- before endings were, so asked again once.
ALTER TABLE openings ADD COLUMN ed_start REAL;
ALTER TABLE openings ADD COLUMN ed_end REAL;
ALTER TABLE openings ADD COLUMN ed_asked INTEGER NOT NULL DEFAULT 0;
", "
-- Each time an episode already watched is watched to its end again: a
-- rewatch, for your AniList list (anilist_list.rs).
CREATE TABLE rewatches (
  episode_id  INTEGER NOT NULL REFERENCES episodes (id) ON DELETE CASCADE,
  at          INTEGER NOT NULL
);
CREATE INDEX rewatches_by_episode ON rewatches (episode_id);
", "
-- Give the episode watches a stable order for counting complete passes through a show.
ALTER TABLE rewatches RENAME TO old_rewatches;
CREATE TABLE rewatches (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  episode_id   INTEGER NOT NULL REFERENCES episodes (id) ON DELETE CASCADE,
  at           INTEGER NOT NULL,
  first_watch  INTEGER NOT NULL DEFAULT 0 -- a newly watched episode is not a rewatch
);
INSERT INTO rewatches (id, episode_id, at) SELECT rowid, episode_id, at FROM old_rewatches ORDER BY rowid;
DROP TABLE old_rewatches;
CREATE INDEX rewatches_by_episode ON rewatches (episode_id);
CREATE TABLE show_completions (
  show_id   INTEGER NOT NULL REFERENCES shows (id) ON DELETE CASCADE,
  watch_id  INTEGER NOT NULL, -- last episode watch included in this pass, or zero for older marks
  at        INTEGER NOT NULL,
  PRIMARY KEY (show_id, watch_id)
);
", "
-- Removing a show leaves Stats as it was: its episodes played to their end
-- and its finishes stay (ids aren't given out again), and so does the show
-- as Stats shows it: its card, and its episodes' shares of known words.
ALTER TABLE rewatches RENAME TO old_rewatches;
CREATE TABLE rewatches (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  episode_id   INTEGER NOT NULL,
  at           INTEGER NOT NULL,
  first_watch  INTEGER NOT NULL DEFAULT 0
);
INSERT INTO rewatches (id, episode_id, at, first_watch) SELECT id, episode_id, at, first_watch FROM old_rewatches ORDER BY id;
DROP TABLE old_rewatches;
CREATE INDEX rewatches_by_episode ON rewatches (episode_id);
ALTER TABLE show_completions RENAME TO old_show_completions;
CREATE TABLE show_completions (
  show_id   INTEGER NOT NULL,
  watch_id  INTEGER NOT NULL,
  at        INTEGER NOT NULL,
  PRIMARY KEY (show_id, watch_id)
);
INSERT INTO show_completions SELECT show_id, watch_id, at FROM old_show_completions;
DROP TABLE old_show_completions;
CREATE TABLE removed_shows (
  id      INTEGER PRIMARY KEY,
  card    TEXT NOT NULL, -- its card as it was (JSON), for the shows finished
  scores  TEXT NOT NULL  -- [[episode number, uses, known]], for the share known in what you watched
);
", "
-- How far into an episode you've ever played it (seconds), so Previously on
-- tells what's after its ending (an after-credits scene, the next episode's
-- preview) only once you've played that far.
ALTER TABLE episodes ADD COLUMN reached REAL NOT NULL DEFAULT 0;
"];

/// How many migrations this Telly has: a library of a higher version is from a newer Telly.
pub const VERSION: i64 = MIGRATIONS.len() as i64;

/// The next id for a show or an episode: after every one given out, removed or not.
const NEXT_SHOW_ID: &str = "(SELECT id + 1 FROM last_ids WHERE name = 'shows')";
const NEXT_EPISODE_ID: &str = "(SELECT id + 1 FROM last_ids WHERE name = 'episodes')";

/// Something Claude said, kept so it's there instantly next time.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub about: String,
    pub text: String,
    pub extra: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Show {
    pub id: i64,
    pub folder: String,
    pub title: String,
    pub title_native: String,
    pub title_english: String,
    pub title_romaji: String,
    pub anilist_id: Option<i64>,
    pub anilist_episodes: Option<i64>,
    pub format: String,
    pub year: Option<i64>,
    pub cover_url: String,
    pub cover_color: String,
    pub jimaku_id: Option<i64>,
    pub added_at: i64,
    pub last_watched_at: Option<i64>,
}

const SHOW_COLUMNS: &str = "id, folder, title, title_native, title_english, title_romaji, anilist_id, anilist_episodes,
  format, year, cover_url, cover_color, jimaku_id, added_at, last_watched_at";

fn show_from_row(r: &Row) -> rusqlite::Result<Show> {
    Ok(Show {
        id: r.get(0)?,
        folder: r.get(1)?,
        title: r.get(2)?,
        title_native: r.get(3)?,
        title_english: r.get(4)?,
        title_romaji: r.get(5)?,
        anilist_id: r.get(6)?,
        anilist_episodes: r.get(7)?,
        format: r.get(8)?,
        year: r.get(9)?,
        cover_url: r.get(10)?,
        cover_color: r.get(11)?,
        jimaku_id: r.get(12)?,
        added_at: r.get(13)?,
        last_watched_at: r.get(14)?,
    })
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: i64,
    pub show_id: i64,
    pub number: i64,
    pub path: String,
    pub size: i64,
    pub duration: f64,
    pub media_state: String,
    pub media_plan: String,
    pub media_progress: f64,
    pub media_error: String,
    pub play_path: String,
    pub audio_index: i64,
    pub audio_track: Option<i64>,
    pub ending_at: Option<f64>,
    pub position: f64,
    pub watched_at: Option<i64>,
    pub last_played_at: Option<i64>,
    pub subs_state: String,
    pub subs_detail: String,
    pub subs_file: String,
    pub subs_score: Option<f64>,
    pub subs_offset_ms: Option<i64>,
    pub subs_nudge_ms: i64,
}

const EPISODE_COLUMNS: &str = "id, show_id, number, path, size, duration, media_state, media_plan, media_progress,
  media_error, play_path, audio_index, ending_at, position, watched_at, last_played_at, subs_state, subs_detail,
  subs_file, subs_score, subs_offset_ms, subs_nudge_ms, audio_track";

fn episode_from_row(r: &Row) -> rusqlite::Result<Episode> {
    Ok(Episode {
        id: r.get(0)?,
        show_id: r.get(1)?,
        number: r.get(2)?,
        path: r.get(3)?,
        size: r.get(4)?,
        duration: r.get(5)?,
        media_state: r.get(6)?,
        media_plan: r.get(7)?,
        media_progress: r.get(8)?,
        media_error: r.get(9)?,
        play_path: r.get(10)?,
        audio_index: r.get(11)?,
        ending_at: r.get(12)?,
        position: r.get(13)?,
        watched_at: r.get(14)?,
        last_played_at: r.get(15)?,
        subs_state: r.get(16)?,
        subs_detail: r.get(17)?,
        subs_file: r.get(18)?,
        subs_score: r.get(19)?,
        subs_offset_ms: r.get(20)?,
        subs_nudge_ms: r.get(21)?,
        audio_track: r.get(22)?,
    })
}

/// A show to add, as confirmed in the Add media sheet.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewShow {
    pub folder: String,
    pub title: String,
    #[serde(default)]
    pub title_native: String,
    #[serde(default)]
    pub title_english: String,
    #[serde(default)]
    pub title_romaji: String,
    pub anilist_id: Option<i64>,
    pub anilist_episodes: Option<i64>,
    #[serde(default)]
    pub format: String,
    pub year: Option<i64>,
    #[serde(default)]
    pub cover_url: String,
    #[serde(default)]
    pub cover_color: String,
    pub episodes: Vec<NewEpisode>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NewEpisode {
    pub number: i64,
    pub path: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub duration: f64,
}

/// A queued card enhancement (see mining.rs).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MineJob {
    pub id: i64,
    pub note_id: i64,
    pub episode_id: i64,
    pub start: f64,
    pub end: f64,
    pub sentence: String,
    pub state: String,
    pub detail: String,
    pub tries: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

fn mine_job_from_row(r: &Row) -> rusqlite::Result<MineJob> {
    Ok(MineJob {
        id: r.get(0)?,
        note_id: r.get(1)?,
        episode_id: r.get(2)?,
        start: r.get(3)?,
        end: r.get(4)?,
        sentence: r.get(5)?,
        state: r.get(6)?,
        detail: r.get(7)?,
        tries: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

const MINE_COLUMNS: &str = "id, note_id, episode_id, start, end, sentence, state, detail, tries, created_at, updated_at";

pub fn open_migrated(path: &Path, migrations: &[&str]) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, migration) in migrations.iter().enumerate().skip(version.max(0) as usize) {
        conn.execute_batch(&format!("BEGIN; {migration} PRAGMA user_version = {}; COMMIT;", i + 1))
            .map_err(|e| anyhow!("updating {} to version {}: {e}", path.display(), i + 1))?;
    }
    Ok(conn)
}

/// The first finish and later full rewatches of a show, newest rewatch first.
#[derive(Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowCompletion {
    pub finished_at: Option<i64>,
    pub rewatches: Vec<i64>,
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = open_migrated(path, MIGRATIONS)?;
        Self::backfill_show_completions(&mut conn)?;
        // The Library, the workers' choice of work and every page ask the same
        // few queries again and again: each is parsed once.
        conn.set_prepared_statement_cache_capacity(64);
        // Work cut short when Telly last closed starts again: subtitles Whisper
        // was making (see claim_whisper) wait for Whisper again.
        conn.execute_batch(
            "UPDATE episodes SET media_state = 'pending', media_progress = 0 WHERE media_state = 'working';
             UPDATE episodes SET subs_state = 'whisper', subs_detail = '' WHERE subs_state = 'working' AND subs_detail LIKE '% is listening';
             UPDATE episodes SET subs_state = 'pending' WHERE subs_state = 'working';",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| anyhow!("the library database is unavailable"))
    }

    // ---- settings ----------------------------------------------------------

    pub fn setting(&self, key: &str) -> Result<Option<Value>> {
        let text: Option<String> = self
            .conn()?
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?;
        Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
    }

    pub fn setting_str(&self, key: &str) -> String {
        self.setting(key).ok().flatten().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }

    pub fn setting_bool(&self, key: &str, default: bool) -> bool {
        self.setting(key).ok().flatten().and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn setting_i64(&self, key: &str, default: i64) -> i64 {
        self.setting(key).ok().flatten().and_then(|v| v.as_i64()).unwrap_or(default)
    }

    pub fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        if value.is_null() {
            self.conn()?.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        } else {
            self.conn()?.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value.to_string()],
            )?;
        }
        Ok(())
    }

    pub fn settings(&self) -> Result<serde_json::Map<String, Value>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut map = serde_json::Map::new();
        for row in rows {
            let (key, text) = row?;
            if let Ok(value) = serde_json::from_str(&text) {
                map.insert(key, value);
            }
        }
        Ok(map)
    }

    // ---- shows -------------------------------------------------------------

    pub fn add_show(&self, s: &NewShow) -> Result<i64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO shows (id, folder, title, title_native, title_english, title_romaji, anilist_id, anilist_episodes,
                   format, year, cover_url, cover_color, added_at)
                 VALUES ({NEXT_SHOW_ID}, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
            ),
            params![
                s.folder,
                s.title,
                s.title_native,
                s.title_english,
                s.title_romaji,
                s.anilist_id,
                s.anilist_episodes,
                s.format,
                s.year,
                s.cover_url,
                s.cover_color,
                now_ms()
            ],
        )?;
        let id = tx.last_insert_rowid();
        for e in &s.episodes {
            tx.execute(
                &format!("INSERT INTO episodes (id, show_id, number, path, size, duration) VALUES ({NEXT_EPISODE_ID}, ?1, ?2, ?3, ?4, ?5)"),
                params![id, e.number, e.path, e.size, e.duration],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Episodes that turned up in a show's folder after it was added.
    pub fn add_episodes(&self, show_id: i64, episodes: &[NewEpisode]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        for e in episodes {
            tx.execute(
                &format!("INSERT INTO episodes (id, show_id, number, path, size, duration) VALUES ({NEXT_EPISODE_ID}, ?1, ?2, ?3, ?4, ?5)"),
                params![show_id, e.number, e.path, e.size, e.duration],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn show(&self, id: i64) -> Result<Option<Show>> {
        let sql = format!("SELECT {SHOW_COLUMNS} FROM shows WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, params![id], show_from_row).optional()?)
    }

    pub fn shows(&self) -> Result<Vec<Show>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {SHOW_COLUMNS} FROM shows ORDER BY last_watched_at IS NULL, last_watched_at DESC, added_at DESC");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], show_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Removes a show and what Telly kept for it, but what Stats counts (see
    /// the migration that made removed_shows): `kept` is the show as Stats
    /// shows it from now on, its card and its episodes' shares of known words.
    pub fn remove_show(&self, id: i64, kept: Option<(&str, &str)>) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        if let Some((card, scores)) = kept {
            tx.execute("INSERT OR REPLACE INTO removed_shows (id, card, scores) VALUES (?1, ?2, ?3)", params![id, card, scores])?;
        }
        // Settings kept by the show's id, and by its episodes' (backend.rs,
        // jobs.rs, anilist_list.rs, the player's picture filter).
        for key in ["audio.track.{id}", "cover.frame.{id}", "player.filter.{id}", "player.filters.{id}", "series.of.{id}", "folder.shared.{id}", "folder.left.{id}", "follow.{id}", "follow.got.{id}", "follow.like.{id}"] {
            tx.execute("DELETE FROM settings WHERE key = ?1", params![key.replace("{id}", &id.to_string())])?;
        }
        for prefix in ["anilist.group", "jimaku.group", "subs.numbering"] {
            tx.execute("DELETE FROM settings WHERE key LIKE ?1", params![format!("{prefix}.{id}.%")])?;
        }
        tx.execute(
            "DELETE FROM settings WHERE key IN (SELECT 'convert.force.' || id FROM episodes WHERE show_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM mine_jobs WHERE episode_id IN (SELECT id FROM episodes WHERE show_id = ?1)", params![id])?;
        tx.execute("DELETE FROM shows WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_show_anilist(&self, id: i64, s: &NewShow) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        // AniSkip's openings were for the anime it was before.
        tx.execute("DELETE FROM openings WHERE episode_id IN (SELECT id FROM episodes WHERE show_id = ?1)", params![id])?;
        // So were its season folders' own AniList and Jimaku entries, found by
        // its title, and the numbering its Jimaku files taught them.
        for prefix in ["anilist.group", "jimaku.group", "subs.numbering"] {
            tx.execute("DELETE FROM settings WHERE key LIKE ?1", params![format!("{prefix}.{id}.%")])?;
        }
        tx.execute(
            "UPDATE shows SET title = ?2, title_native = ?3, title_english = ?4, title_romaji = ?5, anilist_id = ?6,
               anilist_episodes = ?7, format = ?8, year = ?9, cover_url = ?10, cover_color = ?11, jimaku_id = NULL
             WHERE id = ?1",
            params![
                id,
                s.title,
                s.title_native,
                s.title_english,
                s.title_romaji,
                s.anilist_id,
                s.anilist_episodes,
                s.format,
                s.year,
                s.cover_url,
                s.cover_color
            ],
        )?;
        if Self::show_completion_cursor(&tx, id)?.is_none() {
            Self::record_show_completions(&tx, id, true)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_jimaku_id(&self, show_id: i64, jimaku_id: Option<i64>) -> Result<()> {
        self.conn()?.execute("UPDATE shows SET jimaku_id = ?2 WHERE id = ?1", params![show_id, jimaku_id])?;
        Ok(())
    }

    pub fn answer(&self, episode_id: i64, kind: &str, line: i64) -> Result<Option<Answer>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT about, text, extra, created_at FROM answers WHERE episode_id = ?1 AND kind = ?2 AND line = ?3",
                params![episode_id, kind, line],
                |r| Ok(Answer { about: r.get(0)?, text: r.get(1)?, extra: r.get(2)?, created_at: r.get(3)? }),
            )
            .optional()?)
    }

    pub fn save_answer(&self, episode_id: i64, kind: &str, line: i64, about: &str, text: &str, extra: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT OR REPLACE INTO answers (episode_id, kind, line, about, text, extra, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![episode_id, kind, line, about, text, extra, now_ms()],
        )?;
        Ok(())
    }

    pub fn touch_show(&self, id: i64) -> Result<()> {
        self.conn()?.execute("UPDATE shows SET last_watched_at = ?2 WHERE id = ?1", params![id, now_ms()])?;
        Ok(())
    }

    // ---- episodes ----------------------------------------------------------

    pub fn episode(&self, id: i64) -> Result<Option<Episode>> {
        let sql = format!("SELECT {EPISODE_COLUMNS} FROM episodes WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, params![id], episode_from_row).optional()?)
    }

    pub fn episodes(&self, show_id: i64) -> Result<Vec<Episode>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {EPISODE_COLUMNS} FROM episodes WHERE show_id = ?1 ORDER BY number, id");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![show_id], episode_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn all_episodes(&self) -> Result<Vec<Episode>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {EPISODE_COLUMNS} FROM episodes ORDER BY show_id, number, id");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], episode_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Starts media work on an episode; false if another worker got there first.
    pub fn claim_media(&self, id: i64) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE episodes SET media_state = 'working', media_progress = 0, media_error = ''
             WHERE id = ?1 AND media_state = 'pending'",
            params![id],
        )?;
        Ok(n == 1)
    }

    pub fn set_media_progress(&self, id: i64, progress: f64) -> Result<()> {
        self.conn()?.execute("UPDATE episodes SET media_progress = ?2 WHERE id = ?1", params![id, progress])?;
        Ok(())
    }

    pub fn set_probe(&self, id: i64, probe: &str, duration: f64, ending_at: Option<f64>, plan: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET probe = ?2, duration = CASE WHEN ?3 > 0 THEN ?3 ELSE duration END, ending_at = ?4, media_plan = ?5
             WHERE id = ?1",
            params![id, probe, duration, ending_at, plan],
        )?;
        Ok(())
    }

    /// `audio_index` is the track among `play_path`'s; `audio_track`, the one of the rip's it is.
    pub fn media_ready(&self, id: i64, play_path: &str, audio_index: i64, audio_track: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET media_state = 'ready', media_progress = 1, media_error = '', play_path = ?2, audio_index = ?3,
               audio_track = ?4
             WHERE id = ?1",
            params![id, play_path, audio_index, audio_track],
        )?;
        Ok(())
    }

    /// What ffprobe found in the rip, as JSON ("" before the episode was first got ready).
    pub fn probe(&self, id: i64) -> Result<String> {
        Ok(self.conn()?.query_row("SELECT probe FROM episodes WHERE id = ?1", params![id], |r| r.get(0)).optional()?.unwrap_or_default())
    }

    /// Sends a ready episode back to the conversion queue, keeping Telly's copy (for a change of audio track).
    pub fn media_requeue(&self, id: i64) -> Result<()> {
        self.conn()?.execute("UPDATE episodes SET media_state = 'pending', media_progress = 0 WHERE id = ?1 AND media_state = 'ready'", params![id])?;
        Ok(())
    }

    /// Telly's copy of an episode was moved (to another copies folder).
    pub fn set_play_path(&self, id: i64, play_path: &str) -> Result<()> {
        self.conn()?.execute("UPDATE episodes SET play_path = ?2 WHERE id = ?1", params![id, play_path])?;
        Ok(())
    }

    pub fn media_failed(&self, id: i64, error: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET media_state = 'failed', media_error = ?2 WHERE id = ?1",
            params![id, error],
        )?;
        Ok(())
    }

    /// Sends an episode back to the conversion queue (Free up space, or Try again).
    /// A new rip in place of an episode's that's gone (a better release of
    /// it): it's read, got ready and its subtitles synced again, as if new,
    /// but where you were in it and whether it's watched stay. Subtitles you
    /// chose by hand stay chosen.
    pub fn replace_rip(&self, id: i64, path: &str, size: i64, duration: f64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET path = ?2, size = ?3, duration = ?4,
               media_state = 'pending', media_plan = '', media_progress = 0, media_error = '', play_path = '',
               audio_index = 0, audio_track = NULL, probe = '', ending_at = NULL,
               subs_state = CASE WHEN subs_state = 'manual' THEN 'manual' ELSE 'pending' END,
               subs_detail = CASE WHEN subs_state = 'manual' THEN subs_detail ELSE '' END,
               subs_offset_ms = NULL, subs_nudge_ms = 0
             WHERE id = ?1",
            params![id, path, size, duration],
        )?;
        Ok(())
    }

    /// Points shows at the folders they're in now, and episodes at where
    /// their rips are now (relink.rs), in one go. Nothing else of theirs
    /// changes: a rip that plays as it is plays from its new place, and Telly's
    /// copies stay where they are. Work given up on because the rip wasn't
    /// there (`jobs::GONE`), and subtitles that failed, are tried again.
    pub fn relink(&self, shows: &[(i64, String)], episodes: &[(i64, String)]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        for (id, folder) in shows {
            tx.execute("UPDATE shows SET folder = ?2 WHERE id = ?1", params![id, folder])?;
        }
        for (id, path) in episodes {
            tx.execute(
                "UPDATE episodes SET path = ?2, play_path = CASE WHEN play_path = path THEN ?2 ELSE play_path END,
                   media_progress = CASE WHEN media_state = 'failed' AND media_error LIKE ?3 || '%' THEN 0 ELSE media_progress END,
                   media_error = CASE WHEN media_state = 'failed' AND media_error LIKE ?3 || '%' THEN '' ELSE media_error END,
                   media_state = CASE WHEN media_state = 'failed' AND media_error LIKE ?3 || '%' THEN 'pending' ELSE media_state END,
                   subs_detail = CASE WHEN subs_state = 'failed' THEN '' ELSE subs_detail END,
                   subs_state = CASE WHEN subs_state = 'failed' THEN 'pending' ELSE subs_state END
                 WHERE id = ?1",
                params![id, path, crate::jobs::GONE],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn media_reset(&self, id: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET media_state = 'pending', media_progress = 0, media_error = '', play_path = '' WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn claim_subs(&self, id: i64) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE episodes SET subs_state = 'working', subs_detail = '' WHERE id = ?1 AND subs_state = 'pending'",
            params![id],
        )?;
        Ok(n == 1)
    }

    /// Starts making subtitles for an episode queued for it ("whisper"), with
    /// `detail` saying who is listening; false if it isn't queued.
    pub fn claim_whisper(&self, id: i64, detail: &str) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE episodes SET subs_state = 'working', subs_detail = ?2 WHERE id = ?1 AND subs_state = 'whisper'",
            params![id, detail],
        )?;
        Ok(n == 1)
    }

    /// Queues Whisper for an episode, or for every episode of a show without subtitles.
    pub fn queue_whisper(&self, show_id: Option<i64>, episode_id: Option<i64>) -> Result<usize> {
        Ok(match episode_id {
            Some(id) => self.conn()?.execute(
                "UPDATE episodes SET subs_state = 'whisper', subs_detail = '' WHERE id = ?1 AND subs_state != 'working'",
                params![id],
            )?,
            None => self.conn()?.execute(
                "UPDATE episodes SET subs_state = 'whisper', subs_detail = ''
                 WHERE subs_state IN ('none', 'nokey', 'failed') AND (?1 IS NULL OR show_id = ?1)",
                params![show_id],
            )?,
        })
    }

    /// An episode's subtitles, found or not. Your nudge (Z and X) was made
    /// for the lines it had: it stays only when they're the same file, moved
    /// the same, as before.
    pub fn set_subs(
        &self,
        id: i64,
        state: &str,
        detail: &str,
        file: &str,
        score: Option<f64>,
        offset_ms: Option<i64>,
    ) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET subs_state = ?2, subs_detail = ?3, subs_file = ?4, subs_score = ?5, subs_offset_ms = ?6,
                subs_nudge_ms = CASE WHEN subs_file = ?4 AND subs_offset_ms IS ?6 THEN subs_nudge_ms ELSE 0 END
             WHERE id = ?1",
            params![id, state, detail, file, score, offset_ms],
        )?;
        Ok(())
    }

    /// What a subtitle worker found for an episode it took ("working"), kept
    /// only if it's still working on it: a file you picked meanwhile (Use a
    /// subtitle file) isn't replaced by what the worker found. Whether it was kept.
    pub fn settle_subs(&self, id: i64, state: &str, detail: &str, file: &str, score: Option<f64>, offset_ms: Option<i64>) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE episodes SET subs_state = ?2, subs_detail = ?3, subs_file = ?4, subs_score = ?5, subs_offset_ms = ?6,
                subs_nudge_ms = CASE WHEN subs_file = ?4 AND subs_offset_ms IS ?6 THEN subs_nudge_ms ELSE 0 END
             WHERE id = ?1 AND subs_state = 'working'",
            params![id, state, detail, file, score, offset_ms],
        )?;
        Ok(n == 1)
    }

    /// A subtitle worker couldn't finish an episode it took: failed, unless
    /// you picked a file for it meanwhile.
    pub fn fail_subs(&self, id: i64, detail: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET subs_state = 'failed', subs_detail = ?2 WHERE id = ?1 AND subs_state = 'working'",
            params![id, detail],
        )?;
        Ok(())
    }

    /// Whisper couldn't listen to an episode it took now (OpenRouter is busy):
    /// it waits for Whisper again, unless you picked a file for it meanwhile.
    pub fn listen_later(&self, id: i64, detail: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET subs_state = 'whisper', subs_detail = ?2 WHERE id = ?1 AND subs_state = 'working'",
            params![id, detail],
        )?;
        Ok(())
    }

    pub fn set_subs_state(&self, id: i64, state: &str, detail: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET subs_state = ?2, subs_detail = ?3 WHERE id = ?1",
            params![id, state, detail],
        )?;
        Ok(())
    }

    /// Queues subtitles again for every episode of a show in one of `states`.
    /// Sends back to waiting the episodes (of `show_id`, or all) a subtitle
    /// worker is finding subtitles for now, and returns them: what that worker
    /// finds is let go, as it can only keep it while they're its own. Whisper's aren't.
    pub fn requeue_working_subs(&self, show_id: Option<i64>) -> Result<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "UPDATE episodes SET subs_state = 'pending', subs_detail = ''
             WHERE subs_state = 'working' AND subs_detail NOT LIKE '% is listening' AND (?1 IS NULL OR show_id = ?1) RETURNING id",
        )?;
        let ids = stmt.query_map(params![show_id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    pub fn requeue_subs(&self, show_id: Option<i64>, states: &[&str]) -> Result<usize> {
        let list = states.iter().map(|s| format!("'{}'", s.replace('\'', ""))).collect::<Vec<_>>().join(", ");
        let sql = format!(
            "UPDATE episodes SET subs_state = 'pending', subs_detail = '' WHERE subs_state IN ({list}) AND (?1 IS NULL OR show_id = ?1)"
        );
        Ok(self.conn()?.execute(&sql, params![show_id])?)
    }

    pub fn set_nudge(&self, id: i64, nudge_ms: i64) -> Result<()> {
        self.conn()?.execute("UPDATE episodes SET subs_nudge_ms = ?2 WHERE id = ?1", params![id, nudge_ms])?;
        Ok(())
    }

    /// You played an episode to `position`: how far you've ever got in it goes up to there.
    pub fn reach(&self, id: i64, position: f64) -> Result<()> {
        self.conn()?.execute("UPDATE episodes SET reached = MAX(reached, ?2) WHERE id = ?1", params![id, position])?;
        Ok(())
    }

    /// How far into an episode you've ever played it, in seconds.
    pub fn reached(&self, id: i64) -> Result<f64> {
        Ok(self.conn()?.query_row("SELECT reached FROM episodes WHERE id = ?1", params![id], |r| r.get(0)).optional()?.unwrap_or(0.0))
    }

    pub fn save_position(&self, id: i64, position: f64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET position = ?2, last_played_at = ?3 WHERE id = ?1",
            params![id, position, now_ms()],
        )?;
        Ok(())
    }

    /// Keeps an episode finish and any resulting show finish together.
    pub fn finish_episode(&self, id: i64, played_from_before_end: bool) -> Result<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let at = now_ms();
        let first = Self::mark_watched(&tx, id, at)?;
        let played = first || played_from_before_end;
        if played {
            tx.execute(
                "INSERT INTO rewatches (episode_id, at, first_watch)
                 VALUES (?1, ?2, ?3 AND NOT EXISTS (SELECT 1 FROM rewatches WHERE episode_id = ?1))",
                params![id, at, first],
            )?;
            let show = tx.query_row("SELECT show_id FROM episodes WHERE id = ?1", [id], |r| r.get(0))?;
            Self::record_show_completions(&tx, show, false)?;
        }
        tx.commit()?;
        Ok(played)
    }

    /// Every time one of a show's episodes was played to its end since `since`
    /// (unix ms): the first time and each time again, as (episode, when).
    pub fn watches_since(&self, show_id: i64, since: i64) -> Result<Vec<(i64, i64)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT r.episode_id, r.at FROM rewatches r JOIN episodes e ON e.id = r.episode_id WHERE e.show_id = ?1 AND r.at >= ?2",
        )?;
        let rows = stmt.query_map(params![show_id, since], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn mark_watched(conn: &Connection, id: i64, at: i64) -> Result<bool> {
        Ok(conn.execute("UPDATE episodes SET watched_at = ?2, position = 0 WHERE id = ?1 AND watched_at IS NULL", params![id, at])? == 1)
    }

    pub fn set_watched(&self, id: i64, watched: bool) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        if watched {
            if Self::mark_watched(&tx, id, now_ms())? {
                let show = tx.query_row("SELECT show_id FROM episodes WHERE id = ?1", [id], |r| r.get(0))?;
                Self::record_show_completions(&tx, show, false)?;
            } else {
                tx.execute("UPDATE episodes SET position = 0 WHERE id = ?1", [id])?;
            }
        } else {
            tx.execute("UPDATE episodes SET watched_at = NULL, position = 0 WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The dates of a show's first finish and each full rewatch.
    pub fn show_completion(&self, show: i64) -> Result<ShowCompletion> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached("SELECT at FROM show_completions WHERE show_id = ?1 ORDER BY watch_id")?;
        let mut dates = stmt.query_map([show], |r| r.get(0))?;
        let finished_at = dates.next().transpose()?;
        let mut rewatches = dates.collect::<rusqlite::Result<Vec<_>>>()?;
        rewatches.reverse();
        Ok(ShowCompletion { finished_at, rewatches })
    }

    /// The shows removed, as Stats shows them: id, card (JSON) and episodes' shares of known words (JSON).
    pub fn removed_shows(&self) -> Result<Vec<(i64, String, String)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached("SELECT id, card, scores FROM removed_shows")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every show's finishes and full rewatches, as (show, when, whether it's a rewatch). For Stats.
    pub fn all_show_completions(&self) -> Result<Vec<(i64, i64, bool)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT show_id, at, watch_id > (SELECT MIN(watch_id) FROM show_completions f WHERE f.show_id = c.show_id)
             FROM show_completions c ORDER BY at",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// How many episodes were played to their end each day (local time), the
    /// first time or again. For Stats.
    pub fn episodes_finished_by_day(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached("SELECT date(at / 1000, 'unixepoch', 'localtime') AS day, COUNT(*) FROM rewatches GROUP BY day")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Recovers show dates once from the episode marks and playback history older Telly kept.
    fn backfill_show_completions(conn: &mut Connection) -> Result<()> {
        let tx = conn.transaction()?;
        let done = tx.query_row("SELECT 1 FROM settings WHERE key = 'show.completions'", [], |_| Ok(())).optional()?.is_some();
        if !done {
            let shows = {
                let mut stmt = tx.prepare("SELECT DISTINCT show_id FROM episodes WHERE watched_at IS NOT NULL")?;
                stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?
            };
            for show in shows {
                Self::record_show_completions(&tx, show, true)?;
            }
            tx.execute("INSERT INTO settings (key, value) VALUES ('show.completions', 'true')", [])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn show_completion_cursor(conn: &Connection, show: i64) -> Result<Option<i64>> {
        Ok(conn.query_row("SELECT MAX(watch_id) FROM show_completions WHERE show_id = ?1", [show], |r| r.get(0))?)
    }

    /// Keeps one date for each pass through every episode, or each finish of a film.
    fn record_show_completions(conn: &Connection, show: i64, backfill: bool) -> Result<()> {
        let (movie, count, watched, first, last, missing): (bool, i64, i64, Option<i64>, Option<i64>, bool) = conn.query_row(
            "SELECT s.format = 'MOVIE', COUNT(e.id), COUNT(e.watched_at), MIN(e.watched_at), MAX(e.watched_at),
                    COUNT(DISTINCT e.number) < COALESCE(s.anilist_episodes, 0)
               FROM shows s LEFT JOIN episodes e ON e.show_id = s.id WHERE s.id = ?1 GROUP BY s.id",
            [show],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
        if count == 0 || (!movie && missing) {
            return Ok(());
        }
        let insert = |watch_id, at| conn.execute("INSERT INTO show_completions (show_id, watch_id, at) VALUES (?1, ?2, ?3)", params![show, watch_id, at]);
        let after = if let Some(previous) = Self::show_completion_cursor(conn, show)? {
            previous
        } else {
            let Some(finished) = (if movie { first } else { last.filter(|_| watched == count) }) else { return Ok(()) };
            // Before show history, the watched mark and its episode watch were written separately.
            // The first watch just after that episode's mark belongs to the first pass too.
            let cursor = conn.query_row(
                "SELECT COALESCE(MAX(r.id), 0) FROM rewatches r JOIN episodes e ON e.id = r.episode_id
                  WHERE e.show_id = ?1 AND (NOT ?3 OR r.at <= ?2 OR
                    (e.watched_at <= ?2 AND r.at BETWEEN e.watched_at AND e.watched_at + 1000
                     AND r.id = (SELECT MIN(id) FROM rewatches WHERE episode_id = e.id)))",
                params![show, finished, backfill],
                |r| r.get(0),
            )?;
            insert(cursor, finished)?;
            cursor
        };
        let mut stmt = conn.prepare_cached(
            "SELECT r.id, r.episode_id, r.at FROM rewatches r JOIN episodes e ON e.id = r.episode_id
              WHERE e.show_id = ?1 AND r.id > ?2 AND (?3 OR NOT r.first_watch) ORDER BY r.id",
        )?;
        let mut seen = HashSet::new();
        for row in stmt.query_map(params![show, after, movie], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
            let (id, episode, at) = row?;
            seen.insert(episode);
            if movie || seen.len() as i64 == count {
                insert(id, at)?;
                seen.clear();
            }
        }
        Ok(())
    }

    /// Puts episodes in a new order (the Fix order list on a show's page).
    pub fn renumber(&self, show_id: i64, order: &[(i64, i64)]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        for (episode_id, number) in order {
            tx.execute(
                "UPDATE episodes SET number = ?3 WHERE id = ?1 AND show_id = ?2",
                params![episode_id, show_id, number],
            )?;
        }
        // AniSkip's openings were for the episodes' old numbers.
        tx.execute("DELETE FROM openings WHERE episode_id IN (SELECT id FROM episodes WHERE show_id = ?1)", params![show_id])?;
        tx.commit()?;
        Ok(())
    }

    // ---- mining queue --------------------------------------------------------

    pub fn add_mine_job(&self, note_id: i64, episode_id: i64, start: f64, end: f64, sentence: &str) -> Result<i64> {
        let conn = self.conn()?;
        let now = now_ms();
        conn.execute(
            "INSERT INTO mine_jobs (note_id, episode_id, start, end, sentence, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![note_id, episode_id, start, end, sentence, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn mine_job(&self, id: i64) -> Result<Option<MineJob>> {
        let sql = format!("SELECT {MINE_COLUMNS} FROM mine_jobs WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, params![id], mine_job_from_row).optional()?)
    }

    pub fn pending_mine_jobs(&self) -> Result<Vec<MineJob>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {MINE_COLUMNS} FROM mine_jobs WHERE state = 'pending' ORDER BY id");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], mine_job_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn recent_mine_jobs(&self, limit: i64) -> Result<Vec<MineJob>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {MINE_COLUMNS} FROM mine_jobs ORDER BY id DESC LIMIT ?1");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![limit], mine_job_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn has_mine_job_for(&self, note_id: i64) -> Result<bool> {
        Ok(self.conn()?.query_row(
            "SELECT EXISTS (SELECT 1 FROM mine_jobs WHERE note_id = ?1 AND state != 'failed')",
            params![note_id],
            |r| r.get(0),
        )?)
    }

    pub fn update_mine_job(&self, id: i64, state: &str, detail: &str, tries: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE mine_jobs SET state = ?2, detail = ?3, tries = ?4, updated_at = ?5 WHERE id = ?1",
            params![id, state, detail, tries, now_ms()],
        )?;
        Ok(())
    }

    /// Every card mined from a show, for the Finished line on its card.
    /// Each scored episode's uses of words, how many are of known words, and the stamp it was scored at.
    pub fn word_scores(&self) -> Result<std::collections::HashMap<i64, (i64, i64, String)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached("SELECT episode_id, uses, known, stamp FROM word_scores")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().collect())
    }

    /// An episode with no lines any more has no share of known words.
    pub fn forget_word_score(&self, episode_id: i64) -> Result<()> {
        self.conn()?.execute("DELETE FROM word_scores WHERE episode_id = ?1", params![episode_id])?;
        Ok(())
    }

    pub fn set_word_score(&self, episode_id: i64, uses: i64, known: i64, stamp: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO word_scores (episode_id, uses, known, stamp) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(episode_id) DO UPDATE SET uses = excluded.uses, known = excluded.known, stamp = excluded.stamp",
            params![episode_id, uses, known, stamp],
        )?;
        Ok(())
    }

    /// Where AniSkip says an episode's opening is, as (start, end).
    pub fn opening(&self, episode_id: i64) -> Result<Option<(f64, f64)>> {
        let found: Option<(Option<f64>, Option<f64>)> =
            self.conn()?.query_row("SELECT start, end FROM openings WHERE episode_id = ?1", params![episode_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        Ok(found.and_then(|(start, end)| Some((start?, end?))))
    }

    /// Where AniSkip says an episode's ending is.
    pub fn ending(&self, episode_id: i64) -> Result<Option<(f64, f64)>> {
        let found: Option<(Option<f64>, Option<f64>)> = self
            .conn()?
            .query_row("SELECT ed_start, ed_end FROM openings WHERE episode_id = ?1", params![episode_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        Ok(found.and_then(|(start, end)| Some((start?, end?))))
    }

    /// A show's episodes that ffprobe has read and AniSkip is to be asked
    /// about: never asked, or it had none for them when asked before `again` (unix ms).
    pub fn unasked_openings(&self, show_id: i64, again: i64) -> Result<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id FROM episodes e WHERE show_id = ?1 AND probe != ''
               AND NOT EXISTS (SELECT 1 FROM openings o WHERE o.episode_id = e.id AND o.ed_asked = 1
                               AND (o.start IS NOT NULL OR o.ed_start IS NOT NULL OR o.asked_at >= ?2))
             ORDER BY number",
        )?;
        let rows = stmt.query_map(params![show_id, again], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// What AniSkip said about an episode's opening just now; None: it has none.
    pub fn set_opening(&self, episode_id: i64, opening: Option<(f64, f64)>) -> Result<()> {
        self.conn()?.execute(
            "INSERT OR REPLACE INTO openings (episode_id, start, end, asked_at, ed_asked) VALUES (?1, ?2, ?3, ?4, 1)",
            params![episode_id, opening.map(|o| o.0), opening.map(|o| o.1), now_ms()],
        )?;
        Ok(())
    }

    /// AniSkip's answer for an episode, kept only if the episode still has the
    /// number, and its show the AniList match, it was asked about: Change
    /// match or Fix order meanwhile makes it another episode's answer.
    pub fn set_opening_if(&self, episode_id: i64, number: i64, anilist_id: i64, skips: crate::aniskip::Skips) -> Result<bool> {
        let (op, ed) = (skips.opening, skips.ending);
        let kept = self.conn()?.execute(
            "INSERT OR REPLACE INTO openings (episode_id, start, end, asked_at, ed_start, ed_end, ed_asked)
             SELECT e.id, ?2, ?3, ?4, ?7, ?8, 1 FROM episodes e JOIN shows s ON s.id = e.show_id
             WHERE e.id = ?1 AND e.number = ?5 AND s.anilist_id = ?6",
            params![episode_id, op.map(|o| o.0), op.map(|o| o.1), now_ms(), number, anilist_id, ed.map(|o| o.0), ed.map(|o| o.1)],
        )?;
        Ok(kept > 0)
    }

    /// The cards mined from a show, once each, as `cards_per_show` counts them:
    /// the note, its episode's number and its sentence, in the show's order.
    pub fn show_cards(&self, show_id: i64) -> Result<Vec<(i64, i64, String)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT m.note_id, e.number, m.sentence, MIN(m.start) AS start FROM mine_jobs m JOIN episodes e ON e.id = m.episode_id
             WHERE e.show_id = ?1 AND m.state IN ('done', 'pending') GROUP BY m.note_id ORDER BY e.number, start",
        )?;
        let rows = stmt.query_map([show_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn cards_per_show(&self) -> Result<std::collections::HashMap<i64, i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT e.show_id, COUNT(DISTINCT m.note_id) FROM mine_jobs m JOIN episodes e ON e.id = m.episode_id
             WHERE m.state IN ('done', 'pending') GROUP BY e.show_id",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> (Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("telly-db-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Db::open(&dir.join("telly.sqlite")).unwrap(), dir)
    }

    #[test]
    fn keeps_a_show_rewatch_only_after_every_episode_finishes_again() {
        let (db, dir) = temp_db("show-completions");
        let episode = |number| NewEpisode { number, path: format!("{number}.mkv"), size: 1, duration: 1400.0 };
        let show = db.add_show(&NewShow { folder: "Series".into(), episodes: vec![episode(1), episode(2)], ..Default::default() }).unwrap();
        let episodes = db.episodes(show).unwrap();
        let (first, second) = (episodes[0].id, episodes[1].id);
        db.finish_episode(second, true).unwrap();
        assert_eq!(db.show_completion(show).unwrap(), ShowCompletion::default());
        db.finish_episode(first, true).unwrap();
        let finished = db.show_completion(show).unwrap().finished_at.unwrap();

        for _ in 0..2 {
            db.finish_episode(second, true).unwrap();
        }
        assert!(db.show_completion(show).unwrap().rewatches.is_empty());
        db.set_watched(first, false).unwrap();
        db.set_watched(first, true).unwrap();
        assert!(db.show_completion(show).unwrap().rewatches.is_empty());
        db.finish_episode(first, true).unwrap();
        assert_eq!(db.show_completion(show).unwrap().rewatches.len(), 1);

        db.add_episodes(show, &[episode(3)]).unwrap();
        let third = db.episodes(show).unwrap()[2].id;
        for id in [first, second, third] {
            db.finish_episode(id, true).unwrap();
        }
        assert_eq!(db.show_completion(show).unwrap().rewatches.len(), 1);
        db.finish_episode(third, true).unwrap();
        let history = db.show_completion(show).unwrap();
        assert_eq!(history.finished_at, Some(finished));
        assert_eq!(history.rewatches.len(), 2);
        drop(db);

        let db = Db::open(&dir.join("telly.sqlite")).unwrap();
        assert_eq!(db.show_completion(show).unwrap(), history);
        // Removed, the show's finishes and its episodes played to their end stay, for Stats.
        let days = db.episodes_finished_by_day().unwrap();
        db.remove_show(show, Some(("{}", "[]"))).unwrap();
        assert_eq!(db.show_completion(show).unwrap(), history);
        assert_eq!(db.episodes_finished_by_day().unwrap(), days);
        assert_eq!(db.removed_shows().unwrap(), [(show, "{}".to_string(), "[]".to_string())]);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn subtitles_whisper_was_making_when_telly_closed_wait_for_whisper_again() {
        let (db, dir) = temp_db("whisper-closed");
        let episode = |number| NewEpisode { number, path: format!("{number}.mkv"), size: 1, duration: 1400.0 };
        let show = db.add_show(&NewShow { folder: "Series".into(), episodes: vec![episode(1), episode(2)], ..Default::default() }).unwrap();
        let ids: Vec<i64> = db.episodes(show).unwrap().iter().map(|e| e.id).collect();
        db.queue_whisper(None, Some(ids[0])).unwrap();
        assert!(db.claim_whisper(ids[0], "Whisper is listening").unwrap());
        assert!(db.claim_subs(ids[1]).unwrap());
        drop(db);

        let db = Db::open(&dir.join("telly.sqlite")).unwrap();
        let states: Vec<String> = db.episodes(show).unwrap().into_iter().map(|e| e.subs_state).collect();
        assert_eq!(states, ["whisper", "pending"]);
        // A file you pick while Whisper listens stays, whether Whisper is turned away or fails.
        assert!(db.claim_whisper(ids[0], "Whisper is listening").unwrap());
        db.set_subs(ids[0], "synced", "chosen by hand", "mine.srt", None, Some(0)).unwrap();
        db.listen_later(ids[0], "OpenRouter is busy").unwrap();
        db.fail_subs(ids[0], "Whisper: stopped").unwrap();
        let e = db.episodes(show).unwrap().remove(0);
        assert_eq!((e.subs_state.as_str(), e.subs_detail.as_str()), ("synced", "chosen by hand"));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recovers_a_films_finish_and_real_rewatch_from_an_older_database() {
        let dir = std::env::temp_dir().join(format!("telly-old-film-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("telly.sqlite");
        let old = open_migrated(&path, &MIGRATIONS[..MIGRATIONS.len() - 1]).unwrap();
        old.execute_batch(
            "INSERT INTO shows (id, folder, title, format, added_at) VALUES (1, 'Film', 'Film', 'MOVIE', 0);
             INSERT INTO episodes (id, show_id, number, path, watched_at) VALUES (1, 1, 1, 'a.mkv', 10000), (2, 1, 2, 'b.mkv', NULL);
             INSERT INTO rewatches (episode_id, at) VALUES (1, 10001), (1, 30000);",
        ).unwrap();
        drop(old);

        let db = Db::open(&path).unwrap();
        assert_eq!(db.show_completion(1).unwrap(), ShowCompletion { finished_at: Some(10000), rewatches: vec![30000] });
        db.finish_episode(2, true).unwrap();
        let history = db.show_completion(1).unwrap();
        assert_eq!(history.finished_at, Some(10000));
        assert_eq!(history.rewatches.len(), 2);
        assert_eq!(history.rewatches[1], 30000);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_opening_found_before_endings_were_asked_for_is_asked_again_once() {
        let (db, dir) = temp_db("ending");
        let episodes = vec![NewEpisode { number: 1, path: "1.mkv".into(), size: 1, duration: 1400.0 }];
        let show = db.add_show(&NewShow { folder: "Show".into(), title: "Show".into(), anilist_id: Some(100), episodes, ..Default::default() }).unwrap();
        let id = db.episodes(show).unwrap()[0].id;
        db.set_probe(id, "{}", 1400.0, None, "direct").unwrap();
        // Kept as it was before: the opening, no ending, not asked about one.
        db.conn().unwrap().execute("INSERT INTO openings (episode_id, start, end, asked_at) VALUES (?1, 90, 180, 1)", params![id]).unwrap();
        assert_eq!(db.unasked_openings(show, 0).unwrap(), vec![id]);
        let skips = crate::aniskip::Skips { opening: Some((90.0, 180.0)), ending: Some((1300.0, 1390.0)) };
        assert!(db.set_opening_if(id, 1, 100, skips).unwrap());
        assert!(db.unasked_openings(show, 0).unwrap().is_empty());
        assert_eq!(db.ending(id).unwrap(), Some((1300.0, 1390.0)));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn aniskip_s_answer_is_kept_only_for_the_episode_it_was_asked_about() {
        let (db, dir) = temp_db("opening");
        let episodes = vec![NewEpisode { number: 2, path: "2.mkv".into(), size: 1, duration: 1400.0 }];
        let show = db.add_show(&NewShow { folder: "Show".into(), title: "Show".into(), anilist_id: Some(100), episodes, ..Default::default() }).unwrap();
        let id = db.episodes(show).unwrap()[0].id;
        assert!(db.set_opening_if(id, 2, 100, crate::aniskip::Skips { opening: Some((90.0, 180.0)), ending: Some((1300.0, 1390.0)) }).unwrap());
        // Fix order made it episode 3 while AniSkip was asked about episode 2.
        db.renumber(show, &[(id, 3)]).unwrap();
        assert!(!db.set_opening_if(id, 2, 100, Default::default()).unwrap());
        // Change match made it another show's.
        db.set_show_anilist(show, &NewShow { folder: "Show".into(), title: "Other".into(), anilist_id: Some(200), ..Default::default() }).unwrap();
        assert!(!db.set_opening_if(id, 3, 100, Default::default()).unwrap());
        assert!(db.set_opening_if(id, 3, 200, Default::default()).unwrap());
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn another_match_forgets_the_season_folders_entries_found_for_the_last() {
        let (db, dir) = temp_db("rematch");
        let add = |title: &str| {
            let episodes = vec![NewEpisode { number: 1, path: format!("{title}/Season 2/01.mkv"), size: 1, duration: 1400.0 }];
            db.add_show(&NewShow { folder: title.into(), title: title.into(), anilist_id: Some(100), episodes, ..Default::default() }).unwrap()
        };
        let (show, other) = (add("Show"), add("Other"));
        // Found while it was matched to anime 100: "Season 2" searched as that anime's second season.
        for of in [show, other] {
            for key in [format!("anilist.group.{of}.Season 2"), format!("jimaku.group.{of}.Season 2"), format!("subs.numbering.{of}.Season 2")] {
                db.set_setting(&key, &serde_json::json!(555)).unwrap();
            }
        }
        db.set_show_anilist(show, &NewShow { title: "Show".into(), anilist_id: Some(200), ..Default::default() }).unwrap();
        let settings = db.settings().unwrap();
        assert!(settings.keys().all(|k| !k.contains(&format!(".{show}."))), "{settings:?}");
        assert_eq!(settings.keys().filter(|k| k.contains(&format!(".{other}."))).count(), 3);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_nudge_goes_with_the_subtitles_it_was_made_for() {
        let (db, dir) = temp_db("nudge");
        let episodes = vec![NewEpisode { number: 1, path: "1.mkv".into(), size: 1, duration: 1400.0 }];
        let show = db.add_show(&NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let id = db.episodes(show).unwrap()[0].id;
        let nudge = || db.episode(id).unwrap().unwrap().subs_nudge_ms;
        // A file synced 3 s off, nudged back by hand.
        db.set_subs(id, "check", "shifted +4.2 s · matched to the speech", "A.srt", Some(0.3), Some(4200)).unwrap();
        db.set_nudge(id, -3000).unwrap();
        // Found again just as it was: the nudge still fits it.
        db.set_subs(id, "check", "shifted +4.2 s · matched to the speech", "A.srt", Some(0.3), Some(4200)).unwrap();
        assert_eq!(nudge(), -3000);
        // Synced right this time, or another file: the nudge would throw it 3 s out.
        db.set_subs(id, "synced", "shifted +1.2 s · matched to the speech", "A.srt", Some(0.9), Some(1200)).unwrap();
        assert_eq!(nudge(), 0);
        db.set_nudge(id, 400).unwrap();
        db.set_subs(id, "synced", "beside the rip · as it is", "Show - 01.ja.srt", None, Some(0)).unwrap();
        assert_eq!(nudge(), 0);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adds_a_show_with_episodes_and_keeps_your_place() {
        let (db, dir) = temp_db("add");
        let id = db
            .add_show(&NewShow {
                folder: "D:\\Anime\\NARUTO".into(),
                title: "NARUTO".into(),
                episodes: vec![
                    NewEpisode { number: 2, path: "b.mkv".into(), size: 1, duration: 1400.0 },
                    NewEpisode { number: 1, path: "a.mkv".into(), size: 1, duration: 1400.0 },
                ],
                ..Default::default()
            })
            .unwrap();
        let eps = db.episodes(id).unwrap();
        assert_eq!(eps.iter().map(|e| e.number).collect::<Vec<_>>(), [1, 2]);
        assert!(db.claim_media(eps[0].id).unwrap());
        assert!(!db.claim_media(eps[0].id).unwrap());
        db.save_position(eps[0].id, 120.5).unwrap();
        db.set_watched(eps[1].id, true).unwrap();
        let eps = db.episodes(id).unwrap();
        assert_eq!(eps[0].position, 120.5);
        assert!(eps[1].watched_at.is_some());
        db.set_setting("jimaku.key", &Value::String("k".into())).unwrap();
        assert_eq!(db.setting_str("jimaku.key"), "k");
        db.remove_show(id, None).unwrap();
        assert!(db.episodes(id).unwrap().is_empty());
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn asks_aniskip_again_after_a_renumber_or_rematch_and_a_week_after_none() {
        let (db, dir) = temp_db("openings");
        let id = db
            .add_show(&NewShow {
                folder: "D:\\Anime\\Frieren".into(),
                title: "Frieren".into(),
                episodes: vec![NewEpisode { number: 1, path: "a.mkv".into(), size: 1, duration: 1500.0 }],
                ..Default::default()
            })
            .unwrap();
        let ep = db.episodes(id).unwrap()[0].id;
        let week_ago = now_ms() - 7 * 86_400_000;
        // Not read by ffprobe yet.
        assert!(db.unasked_openings(id, week_ago).unwrap().is_empty());
        db.set_probe(ep, "{}", 1500.0, None, "direct").unwrap();
        assert_eq!(db.unasked_openings(id, week_ago).unwrap(), [ep]);
        db.set_opening(ep, Some((116.7, 206.7))).unwrap();
        assert_eq!(db.opening(ep).unwrap(), Some((116.7, 206.7)));
        assert!(db.unasked_openings(id, week_ago).unwrap().is_empty());
        // An opening found stands, however long ago it was asked.
        assert!(db.unasked_openings(id, now_ms() + 1).unwrap().is_empty());
        db.renumber(id, &[(ep, 2)]).unwrap();
        assert_eq!(db.opening(ep).unwrap(), None);
        assert_eq!(db.unasked_openings(id, week_ago).unwrap(), [ep]);
        db.set_opening(ep, None).unwrap();
        assert!(db.unasked_openings(id, week_ago).unwrap().is_empty());
        // None is asked about again once it's older than `again`.
        assert_eq!(db.unasked_openings(id, now_ms() + 1).unwrap(), [ep]);
        db.set_show_anilist(id, &NewShow { title: "Frieren".into(), ..Default::default() }).unwrap();
        assert_eq!(db.unasked_openings(id, week_ago).unwrap(), [ep]);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }
}
