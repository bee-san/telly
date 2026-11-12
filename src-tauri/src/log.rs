//! telly-log.sqlite, in %APPDATA%\Telly: what you watched, for Tidepool's
//! Japanese log, which reads it read-only (WAL mode lets it read while Telly
//! writes). Tidepool's tables hold counts only; lines_seen keeps each line
//! whole, its text too, in case it's wanted (Telly makes no statistics of it).
//!
//!   episodes_played  one row per sitting with an episode: start, end, seconds
//!                    of playback, and the lines and characters that played
//!   lines_played     each line as it plays: when, and its character count
//!   lines_seen       the same lines whole: text, speaker, furigana, where in
//!                    the episode and its subtitle file, the nudge, the video's
//!                    time and speed (as Rubi's history.csv keeps a game's lines);
//!                    the sitting's row has the episode's ID and subtitle file
//!   cards_mined      each card Telly finished, with the show and episode
//!
//! lines_seen_exstatic is lines_seen as GameSentenceMiner's ExStatic lines
//! CSV has them (uuid, given_identifier, name, line, time), for its importer.
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
", "
-- Each line as it played, whole: kept in case it's wanted, as Rubi keeps a
-- game's lines (lines_played has its count, as Tidepool reads it). What's the
-- same for every line of a sitting is the sitting's: the episode, and the
-- subtitle file its lines came from and how they were synced.
ALTER TABLE episodes_played ADD COLUMN episode_id INTEGER;
ALTER TABLE episodes_played ADD COLUMN subs_file TEXT NOT NULL DEFAULT '';
ALTER TABLE episodes_played ADD COLUMN subs_detail TEXT NOT NULL DEFAULT '';
CREATE TABLE lines_seen (
  id           INTEGER PRIMARY KEY,
  line_played  INTEGER NOT NULL REFERENCES lines_played (id),
  session_id   INTEGER NOT NULL REFERENCES episodes_played (id), -- its show, AniList ID and episode number
  seen_at      INTEGER NOT NULL,   -- when playback passed its middle
  episode_id   INTEGER NOT NULL,   -- Telly's own episode ID
  line         INTEGER,            -- its place among the episode's subtitle lines
  start        REAL NOT NULL,      -- seconds, as its subtitle file has it
  end          REAL NOT NULL,
  nudge_ms     INTEGER NOT NULL,   -- your nudge (Z and X): it showed at start + nudge
  position     REAL,               -- the video's time then, seconds
  rate         REAL,               -- the playback speed
  text         TEXT NOT NULL,      -- as shown, its line breaks kept
  speaker      TEXT NOT NULL,
  ruby         TEXT NOT NULL,      -- furigana, as JSON: [[first character, characters, reading]]
  chars        INTEGER NOT NULL,   -- counted as lines_played counts
  subtitles    INTEGER NOT NULL    -- 1: Japanese subtitles were on screen
);
CREATE INDEX lines_seen_by_time ON lines_seen (seen_at);
-- lines_seen as GameSentenceMiner's ExStatic lines CSV has them (Rubi writes
-- the same first columns): GSM's /api/import-exstatic takes them.
CREATE VIEW lines_seen_exstatic AS
  SELECT 'telly-' || COALESCE('anilist-' || p.anilist_id, 'show-' || p.show_id) AS uuid,
         'telly-' || s.id AS given_identifier,
         p.show_title AS name,
         s.text AS line,
         printf('%.6f', s.seen_at / 1000.0) AS time
  FROM lines_seen s JOIN episodes_played p ON p.id = s.session_id;
UPDATE meta SET value = value || ' lines_seen keeps each line whole, its text too.' WHERE key = 'about';
"];

/// A line that played: when, its character count, and the rest of it for lines_seen.
#[derive(Debug, Default)]
pub struct Seen {
    pub at: i64,
    pub chars: i64,
    pub episode_id: i64,
    pub line: Option<i64>,
    pub start: f64,
    pub end: f64,
    pub nudge_ms: i64,
    pub position: Option<f64>,
    pub rate: Option<f64>,
    pub text: String,
    pub speaker: String,
    /// Furigana as JSON.
    pub ruby: String,
    pub subtitles: bool,
}

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
    /// For lines_seen: Telly's own episode ID, the subtitle file its lines
    /// came from and how they were synced.
    pub episode_id: i64,
    pub subs_file: &'a str,
    pub subs_detail: &'a str,
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
            "INSERT INTO episodes_played (show_id, show_title, show_title_native, anilist_id, episode, started_at, ended_at, subtitles,
               episode_id, subs_file, subs_detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10)",
            params![s.show_id, s.title, s.native, s.anilist_id, s.episode, at, subtitles as i64, s.episode_id, s.subs_file, s.subs_detail],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Adds `seconds` of playback and the lines that played to a sitting, which now ends at `at`.
    pub fn extend(&self, session: i64, at: i64, seconds: f64, lines: &[Seen], subtitles: bool) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        for l in lines {
            tx.prepare_cached("INSERT INTO lines_played (session_id, played_at, chars) VALUES (?1, ?2, ?3)")?.execute(params![session, l.at, l.chars])?;
            let played = tx.last_insert_rowid();
            tx.prepare_cached(
                "INSERT INTO lines_seen (line_played, session_id, seen_at, episode_id, line, start, end, nudge_ms, position, rate,
                   text, speaker, ruby, chars, subtitles)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?
            .execute(params![
                played,
                session,
                l.at,
                l.episode_id,
                l.line,
                l.start,
                l.end,
                l.nudge_ms,
                l.position,
                l.rate,
                l.text,
                l.speaker,
                l.ruby,
                l.chars,
                l.subtitles as i64
            ])?;
        }
        let chars: i64 = lines.iter().map(|l| l.chars).sum();
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

    /// cargo test --profile measure --lib log::tests::lines_seen_cost -- --ignored --nocapture
    #[test]
    #[ignore]
    fn lines_seen_cost() {
        let dir = std::env::temp_dir().join(format!("telly-log-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ticks = 50_000;
        let line = |i: i64| Seen {
            at: 1_000_000 + i * 1500,
            chars: 18,
            episode_id: 42,
            line: Some(i % 500),
            start: i as f64 * 1.5,
            end: i as f64 * 1.5 + 1.2,
            nudge_ms: 0,
            position: Some(i as f64 * 1.5 + 0.6),
            rate: Some(1.0),
            text: "大丈夫だよ、俺がついてるから\nもう泣くな".into(),
            speaker: String::new(),
            ruby: "[[0,3,\"だいじょうぶ\"],[6,1,\"おれ\"],[15,1,\"な\"]]".into(),
            subtitles: true,
        };
        let time = |name: &str, whole: bool| {
            let path = dir.join(format!("{name}.sqlite"));
            let log = Log::open(&path).unwrap();
            let s = Sitting { show_id: 1, title: "Cyberpunk: Edgerunners", native: "サイバーパンク エッジランナーズ", anilist_id: Some(120377), episode: 4, episode_id: 42,
                subs_file: "[Kitsunekko] Cyberpunk Edgerunners - 04.ja.srt", subs_detail: "shifted +1.2 s · matched to the rip’s English subtitles" };
            let id = log.start(&s, 0, true).unwrap();
            let started = std::time::Instant::now();
            for t in 0..ticks {
                let lines = [line(2 * t), line(2 * t + 1)];
                if whole {
                    log.extend(id, t * 1000, 1.0, &lines, true).unwrap();
                } else {
                    // As before lines_seen: the counts alone.
                    let mut conn = log.conn().unwrap();
                    let tx = conn.transaction().unwrap();
                    for l in &lines {
                        tx.prepare_cached("INSERT INTO lines_played (session_id, played_at, chars) VALUES (?1, ?2, ?3)").unwrap().execute(params![id, l.at, l.chars]).unwrap();
                    }
                    tx.execute("UPDATE episodes_played SET ended_at = MAX(ended_at, ?2), seconds = seconds + 1, lines = lines + 2, chars = chars + 36 WHERE id = ?1", params![id, t * 1000]).unwrap();
                    tx.commit().unwrap();
                }
            }
            let per_tick = started.elapsed().as_secs_f64() * 1e6 / ticks as f64;
            let started = std::time::Instant::now();
            let exported: i64 = log.conn().unwrap().query_row("SELECT COUNT(*), SUM(LENGTH(line)) FROM lines_seen_exstatic", [], |r| r.get(0)).unwrap();
            let export_ms = started.elapsed().as_secs_f64() * 1000.0;
            log.conn().unwrap().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
            drop(log);
            let bytes = std::fs::metadata(&path).unwrap().len();
            println!("{name}: {per_tick:.1} µs a tick of 2 lines, {:.0} bytes a line on disk, GSM view over {exported} lines {export_ms:.0} ms", bytes as f64 / (2 * ticks) as f64);
        };
        time("counts only", false);
        time("whole lines", true);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn logs_a_sitting() {
        let dir = std::env::temp_dir().join(format!("telly-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Log::open(&dir.join("telly-log.sqlite")).unwrap();
        let s = Sitting { show_id: 1, title: "NARUTO", native: "NARUTO -ナルト-", anilist_id: Some(20), episode: 4, episode_id: 7, subs_file: "", subs_detail: "" };
        let id = log.start(&s, 1000, true).unwrap();
        let seen = |at: i64, text: &str, chars: i64| Seen { at, chars, episode_id: 7, line: Some(3), text: text.into(), subtitles: true, ..Default::default() };
        log.extend(id, 2000, 1.0, &[seen(1500, "影分身の術だってばよ", 10), seen(1900, "うん", 2)], true).unwrap();
        log.extend(id, 3000, 1.0, &[], true).unwrap();
        log.card(99, "NARUTO", Some(20), 4, 2500).unwrap();
        drop(log);
        // Tidepool opens it read-only, as here.
        let conn = Connection::open_with_flags(dir.join("telly-log.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (seconds, lines, chars, ended): (f64, i64, i64, i64) = conn
            .query_row("SELECT seconds, lines, chars, ended_at FROM episodes_played WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        assert_eq!((seconds, lines, chars, ended), (2.0, 2, 12, 3000));
        let cards: i64 = conn.query_row("SELECT COUNT(*) FROM cards_mined", [], |r| r.get(0)).unwrap();
        assert_eq!(cards, 1);
        // Each line whole, and as GameSentenceMiner's importer takes it.
        let text: String = conn.query_row("SELECT text FROM lines_seen WHERE seen_at = 1500", [], |r| r.get(0)).unwrap();
        assert_eq!(text, "影分身の術だってばよ");
        let row: (String, String, String, String, String) = conn
            .query_row("SELECT uuid, given_identifier, name, line, time FROM lines_seen_exstatic ORDER BY time LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap();
        assert_eq!(row.0, "telly-anilist-20");
        assert_eq!((row.2.as_str(), row.3.as_str(), row.4.as_str()), ("NARUTO", "影分身の術だってばよ", "1.500000"));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}
