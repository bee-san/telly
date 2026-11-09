//! telly.sqlite, in %LOCALAPPDATA%\Telly: your shows, their episodes, where
//! you are in each, subtitle and conversion state, settings and the queue of
//! cards waiting for their picture and audio.
//!
//! The schema is a list of migrations; PRAGMA user_version counts how many
//! have run. Add new ones to the end, never edit old ones.

use crate::util::now_ms;
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
"];

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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
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
  subs_file, subs_score, subs_offset_ms, subs_nudge_ms";

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

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = open_migrated(path, MIGRATIONS)?;
        // Work cut short when Telly last closed starts again.
        conn.execute_batch(
            "UPDATE episodes SET media_state = 'pending', media_progress = 0 WHERE media_state = 'working';
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
        let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
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
            "INSERT INTO shows (folder, title, title_native, title_english, title_romaji, anilist_id, anilist_episodes,
               format, year, cover_url, cover_color, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                "INSERT INTO episodes (show_id, number, path, size, duration) VALUES (?1, ?2, ?3, ?4, ?5)",
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
                "INSERT INTO episodes (show_id, number, path, size, duration) VALUES (?1, ?2, ?3, ?4, ?5)",
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
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], show_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn remove_show(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM mine_jobs WHERE episode_id IN (SELECT id FROM episodes WHERE show_id = ?1)", params![id])?;
        conn.execute("DELETE FROM shows WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_show_anilist(&self, id: i64, s: &NewShow) -> Result<()> {
        self.conn()?.execute(
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
        Ok(())
    }

    pub fn set_jimaku_id(&self, show_id: i64, jimaku_id: Option<i64>) -> Result<()> {
        self.conn()?.execute("UPDATE shows SET jimaku_id = ?2 WHERE id = ?1", params![show_id, jimaku_id])?;
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
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![show_id], episode_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn all_episodes(&self) -> Result<Vec<Episode>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {EPISODE_COLUMNS} FROM episodes ORDER BY show_id, number, id");
        let mut stmt = conn.prepare(&sql)?;
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

    pub fn media_ready(&self, id: i64, play_path: &str, audio_index: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET media_state = 'ready', media_progress = 1, media_error = '', play_path = ?2, audio_index = ?3
             WHERE id = ?1",
            params![id, play_path, audio_index],
        )?;
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
            "UPDATE episodes SET subs_state = ?2, subs_detail = ?3, subs_file = ?4, subs_score = ?5, subs_offset_ms = ?6
             WHERE id = ?1",
            params![id, state, detail, file, score, offset_ms],
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

    pub fn save_position(&self, id: i64, position: f64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET position = ?2, last_played_at = ?3 WHERE id = ?1",
            params![id, position, now_ms()],
        )?;
        Ok(())
    }

    pub fn set_watched(&self, id: i64, watched: bool) -> Result<()> {
        if watched {
            self.conn()?.execute(
                "UPDATE episodes SET watched_at = COALESCE(watched_at, ?2), position = 0 WHERE id = ?1",
                params![id, now_ms()],
            )?;
        } else {
            self.conn()?.execute("UPDATE episodes SET watched_at = NULL, position = 0 WHERE id = ?1", params![id])?;
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
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], mine_job_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn recent_mine_jobs(&self, limit: i64) -> Result<Vec<MineJob>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {MINE_COLUMNS} FROM mine_jobs ORDER BY id DESC LIMIT ?1");
        let mut stmt = conn.prepare(&sql)?;
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
    pub fn cards_per_show(&self) -> Result<std::collections::HashMap<i64, i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
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
        db.remove_show(id).unwrap();
        assert!(db.episodes(id).unwrap().is_empty());
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }
}
