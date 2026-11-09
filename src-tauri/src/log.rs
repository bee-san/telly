//! telly-log.sqlite, in %APPDATA%\Telly: what you watched, for Tidepool's
//! Japanese log, which reads it read-only (WAL mode lets it read while Telly
//! writes). Never the subtitle text, only counts.
//!
//!   episodes_played  one row per sitting with an episode: start, end, seconds
//!                    of playback, and the lines and characters that played
//!   lines_played     each line as it plays: when, and its character count
//!   cards_mined      each card Telly finished, with the show and episode
//!
//! A line counts once playback passes its middle at ½× to 2× speed without a
//! seek over it (the player decides that). Characters are counted as
//! GameSentenceMiner and Tidepool count them: letters and numbers only.

use crate::db::open_migrated;
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const MIGRATIONS: &[&str] = &["
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO meta (key, value) VALUES
  ('writer', 'Telly'),
  ('about', 'What was watched in Telly, for Tidepool. Times are unix milliseconds. chars counts subtitle text without punctuation, symbols or spaces, as GameSentenceMiner and Tidepool count. A line counts once playback passes its middle at 0.5x to 2x without a seek over it.');
CREATE TABLE episodes_played (
  id                INTEGER PRIMARY KEY,
  show_id           INTEGER NOT NULL,        -- Telly's own show ID
  show_title        TEXT NOT NULL,           -- romaji or English
  show_title_native TEXT NOT NULL,
  anilist_id        INTEGER,
  episode           INTEGER NOT NULL,
  started_at        INTEGER NOT NULL,
  ended_at          INTEGER NOT NULL,
  seconds           REAL NOT NULL DEFAULT 0, -- time spent playing (not paused)
  lines             INTEGER NOT NULL DEFAULT 0,
  chars             INTEGER NOT NULL DEFAULT 0,
  subtitles         INTEGER NOT NULL DEFAULT 1  -- Japanese subtitles were on screen
);
CREATE INDEX episodes_played_by_time ON episodes_played (ended_at);
CREATE TABLE lines_played (
  id          INTEGER PRIMARY KEY,
  session_id  INTEGER NOT NULL REFERENCES episodes_played (id),
  played_at   INTEGER NOT NULL,
  chars       INTEGER NOT NULL
);
CREATE INDEX lines_played_by_time ON lines_played (played_at);
CREATE TABLE cards_mined (
  id          INTEGER PRIMARY KEY,
  note_id     INTEGER NOT NULL,
  show_title  TEXT NOT NULL,
  anilist_id  INTEGER,
  episode     INTEGER NOT NULL,
  mined_at    INTEGER NOT NULL
);
"];

pub struct Log {
    conn: Mutex<Connection>,
    pub path: PathBuf,
}

/// Who and what a sitting is about.
pub struct Sitting<'a> {
    pub show_id: i64,
    pub title: &'a str,
    pub native: &'a str,
    pub anilist_id: Option<i64>,
    pub episode: i64,
}

impl Log {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(Self { conn: Mutex::new(open_migrated(path, MIGRATIONS)?), path: path.to_path_buf() })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| anyhow!("the watching log is unavailable"))
    }

    pub fn start(&self, s: &Sitting, at: i64, subtitles: bool) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO episodes_played (show_id, show_title, show_title_native, anilist_id, episode, started_at, ended_at, subtitles)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            params![s.show_id, s.title, s.native, s.anilist_id, s.episode, at, subtitles as i64],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Adds `seconds` of playback and the lines that played to a sitting, which now ends at `at`.
    pub fn extend(&self, session: i64, at: i64, seconds: f64, lines: &[(i64, i64)], subtitles: bool) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        for (played_at, chars) in lines {
            tx.execute(
                "INSERT INTO lines_played (session_id, played_at, chars) VALUES (?1, ?2, ?3)",
                params![session, played_at, chars],
            )?;
        }
        let chars: i64 = lines.iter().map(|(_, c)| c).sum();
        tx.execute(
            "UPDATE episodes_played SET ended_at = MAX(ended_at, ?2), seconds = seconds + ?3, lines = lines + ?4, chars = chars + ?5,
               subtitles = MAX(subtitles, ?6) WHERE id = ?1",
            params![session, at, seconds, lines.len() as i64, chars, subtitles as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn card(&self, note_id: i64, title: &str, anilist_id: Option<i64>, episode: i64, at: i64) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO cards_mined (note_id, show_title, anilist_id, episode, mined_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![note_id, title, anilist_id, episode, at],
        )?;
        Ok(())
    }

    /// Sittings shorter than 20 seconds of playback, with no lines, are noise (opening the wrong episode).
    pub fn tidy(&self) -> Result<()> {
        self.conn()?.execute(
            "DELETE FROM episodes_played WHERE seconds < 20 AND lines = 0 AND ended_at < ?1
               AND id NOT IN (SELECT session_id FROM lines_played)",
            params![crate::util::now_ms() - 3_600_000],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_a_sitting() {
        let dir = std::env::temp_dir().join(format!("telly-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Log::open(&dir.join("telly-log.sqlite")).unwrap();
        let s = Sitting { show_id: 1, title: "NARUTO", native: "NARUTO -ナルト-", anilist_id: Some(20), episode: 4 };
        let id = log.start(&s, 1000, true).unwrap();
        log.extend(id, 2000, 1.0, &[(1500, 12), (1900, 3)], true).unwrap();
        log.extend(id, 3000, 1.0, &[], true).unwrap();
        log.card(99, "NARUTO", Some(20), 4, 2500).unwrap();
        drop(log);
        // Tidepool opens it read-only, as here.
        let conn = Connection::open_with_flags(dir.join("telly-log.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (seconds, lines, chars, ended): (f64, i64, i64, i64) = conn
            .query_row("SELECT seconds, lines, chars, ended_at FROM episodes_played WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        assert_eq!((seconds, lines, chars, ended), (2.0, 2, 15, 3000));
        let cards: i64 = conn.query_row("SELECT COUNT(*) FROM cards_mined", [], |r| r.get(0)).unwrap();
        assert_eq!(cards, 1);
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}
