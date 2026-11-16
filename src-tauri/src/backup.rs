//! Backing Telly up to one file, and restoring it on a new computer or after
//! a reinstall. A backup is a .zip of what Telly can't make again: the library
//! (telly.sqlite: your shows, where you are in each, what you watched, your
//! settings and keys, subtitle choices and nudges), Tidepool's log, the
//! subtitles folder, your known words and your AniList list. What Telly makes
//! again by itself stays out: its copies of episodes, covers, the seek bar's
//! pictures, line search's index, recommendations, the character dictionary,
//! Jimaku's downloads and models. Your rips are never in it.
//!
//! Restoring never swaps files under a running Telly. Restore checks the
//! backup and unpacks it beside the library (backup-to-restore), and Telly
//! restarts; as it starts, before anything opens the library, the backup takes
//! its place (`finish_restore`). The library it replaces is set aside first and
//! put back if anything fails. What belongs to this computer rather than to
//! you is kept from the library replaced: the copies folder, the dictionaries
//! this computer's Hachidori has installed, ffmpeg and the dictionaries folder
//! (the backup's, when this computer has none chosen and the backup's is
//! here), and Telly's copies, pictures and covers of episodes and shows that
//! are the same in both.

use crate::backend::{Backend, Dirs};
use crate::db::{Db, Episode};
use crate::log::Log;
use crate::util::now_ms;
use crate::{names, own_hachidori};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// What a backup has in it: what it says about itself, the library, the log,
/// the subtitles folder and three files.
const MANIFEST: &str = "telly-backup.json";
const LIBRARY: &str = "telly.sqlite";
const LOG: &str = "telly-log.sqlite";
const SUBTITLES: &str = "subtitles";
const FILES: [&str; 3] = ["known-words.json", "anilist-list.json", "libraries-passed-over.json"];
/// In the library's folder: a backup being unpacked, and one unpacked and
/// waiting for Telly to start again.
const UNPACKING: &str = "backup-to-restore.part";
const STAGED: &str = "backup-to-restore";
/// The library a restore replaces, set aside until the backup is in its place.
const ASIDE: &str = "library-before-restore";
/// Settings: what was restored, or why it wasn't, for the page to say once.
pub const RESTORED: &str = "backup.restored";
pub const NOT_RESTORED: &str = "backup.restoreFailed";
/// Settings that are paths on the computer the library was on.
const MEDIA_FOLDER: &str = "media.folder";
/// Whether this computer's copies are in a folder of Telly's own in the one chosen (backend.rs).
const MEDIA_APART: &str = "media.apart";
const FFMPEG: &str = "tools.ffmpeg";

/// What a backup says about itself (telly-backup.json): the Telly that made
/// it, when (unix ms), and how many shows and episodes it has.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub telly: String,
    pub made_at: i64,
    pub shows: usize,
    pub episodes: usize,
}

// ---- Backing up ----

/// Writes a backup to `path` (a .zip), whole or not at all: it's written
/// beside it first, and put in its place once it's done.
pub fn back_up(backend: &Backend, path: &str) -> Result<Value> {
    let mut path = PathBuf::from(path.trim());
    let mut name = path.file_name().ok_or_else(|| anyhow!("Choose where to save the backup"))?.to_string_lossy().into_owned();
    if !name.to_lowercase().ends_with(".zip") {
        name.push_str(".zip");
        path.set_file_name(&name);
    }
    let part = path.with_file_name(format!("{name}.part"));
    let manifest = Manifest {
        telly: env!("CARGO_PKG_VERSION").into(),
        made_at: now_ms(),
        shows: backend.db.shows()?.len(),
        episodes: backend.db.all_episodes()?.len(),
    };
    let written = write(backend, &part, &manifest).and_then(|()| Ok(std::fs::rename(&part, &path)?));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&part);
        return Err(e.context(format!("Telly couldn’t save a backup to {}", path.display())));
    }
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut answer = json!(manifest);
    answer["path"] = json!(path);
    answer["bytes"] = json!(bytes);
    Ok(answer)
}

fn write(backend: &Backend, to: &Path, manifest: &Manifest) -> Result<()> {
    let mut zip = ZipWriter::new(std::io::BufWriter::new(File::create(to)?));
    zip.start_file(MANIFEST, SimpleFileOptions::default().compression_method(CompressionMethod::Deflated))?;
    zip.write_all(&serde_json::to_vec_pretty(manifest)?)?;
    for (name, db) in [(LIBRARY, backend.dirs.local.join(LIBRARY)), (LOG, backend.log.path.clone())] {
        let copy = backend.dirs.local.join(format!("{name}.backup-{}", std::process::id()));
        let added = snapshot(&db, &copy).and_then(|()| add(&mut zip, name, &copy));
        let _ = std::fs::remove_file(&copy);
        added?;
    }
    for path in subtitle_files(&backend.dirs.subs()) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        add(&mut zip, &format!("{SUBTITLES}/{name}"), &path)?;
    }
    for name in FILES {
        let path = backend.dirs.local.join(name);
        if path.is_file() {
            add(&mut zip, name, &path)?;
        }
    }
    let file = zip.finish()?.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    Ok(())
}

/// A copy of a database as it is now, made by SQLite (VACUUM INTO), so it's
/// whole however busy Telly is with it.
fn snapshot(db: &Path, to: &Path) -> Result<()> {
    let _ = std::fs::remove_file(to);
    let conn = Connection::open(db)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    conn.execute("VACUUM INTO ?1", [to.to_string_lossy()])?;
    Ok(())
}

fn add<W: Write + Seek>(zip: &mut ZipWriter<W>, name: &str, path: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).large_file(size >= u32::MAX as u64);
    zip.start_file(name, options)?;
    std::io::copy(&mut file, zip)?;
    Ok(())
}

/// The files in the subtitles folder that go in a backup: all but the pieces
/// of audio heard while matching and files half written.
fn subtitle_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            !name.ends_with(".wav") && !name.ends_with(".part") && !name.contains(".part-")
        })
        .collect();
    files.sort();
    files
}

// ---- Reading one ----

/// Whether `name` is something Telly puts in a backup: one of its files, or
/// a file in the subtitles folder (by its name alone, so it can't be put anywhere else).
fn expected(name: &str) -> bool {
    if [MANIFEST, LIBRARY, LOG].contains(&name) || FILES.contains(&name) || name == format!("{SUBTITLES}/") {
        return true;
    }
    name.strip_prefix(SUBTITLES).and_then(|rest| rest.strip_prefix('/')).is_some_and(|file| {
        !file.is_empty() && file != "." && file != ".." && !file.contains(['/', '\\', ':']) && !file.chars().any(char::is_control)
    })
}

/// Whether version `a` (0.4.0) is newer than `b` (0.3.4).
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect() };
    parts(a) > parts(b)
}

/// Opens a backup and checks it's one this Telly can restore: Telly made it
/// (it has telly-backup.json), this version or an older one, and it has
/// nothing in it but what Telly puts in a backup.
fn open(path: &str) -> Result<(ZipArchive<File>, Manifest)> {
    let path = path.trim();
    let file = File::open(path).with_context(|| format!("Telly can’t open {path}"))?;
    let not_one = || anyhow!("{path} isn’t a backup of Telly.");
    let mut zip = ZipArchive::new(file).map_err(|_| not_one())?;
    let mut manifest = None;
    let (mut library, mut log) = (false, false);
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name()?.to_string();
        if !expected(&name) {
            bail!("{path} isn’t a backup Telly made: it has {name} in it.");
        }
        match name.as_str() {
            MANIFEST => {
                let mut text = Vec::new();
                entry.by_ref().take(1 << 20).read_to_end(&mut text)?;
                manifest = serde_json::from_slice::<Manifest>(&text).ok();
            }
            LIBRARY => library = true,
            LOG => log = true,
            _ => {}
        }
    }
    let manifest = manifest.ok_or_else(not_one)?;
    if !library || !log {
        bail!("This backup is missing your library or Tidepool’s log, so it can’t be restored.");
    }
    let this = env!("CARGO_PKG_VERSION");
    if newer(&manifest.telly, this) {
        bail!("This backup was made by Telly {}, which is newer than this one ({this}). Update Telly, then restore it.", manifest.telly);
    }
    Ok((zip, manifest))
}

/// What's in a backup, before restoring it: what it says about itself, where
/// it is, and how many shows the library here has.
pub fn read(backend: &Backend, path: &str) -> Result<Value> {
    let (_, manifest) = open(path)?;
    let mut answer = json!(manifest);
    answer["path"] = json!(path.trim());
    answer["showsHere"] = json!(backend.db.shows()?.len());
    Ok(answer)
}

// ---- Restoring ----

/// Unpacks a backup beside the library, for Telly to put in its place when it
/// next starts (`finish_restore`). Its library is checked and brought up to
/// this Telly's version now, so starting into it can't fail on that.
pub fn stage(backend: &Backend, path: &str) -> Result<Value> {
    let (mut zip, manifest) = open(path)?;
    let (part, staged) = (backend.dirs.local.join(UNPACKING), backend.dirs.local.join(STAGED));
    let _ = std::fs::remove_dir_all(&part);
    let _ = std::fs::remove_dir_all(&staged);
    let unpacked = (|| -> Result<()> {
        std::fs::create_dir_all(part.join(SUBTITLES))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            // Only the names `open` let through, each put where Telly keeps it.
            let name = entry.name()?.to_string();
            let to = match name.split_once('/') {
                Some((_, file)) => part.join(SUBTITLES).join(file),
                None => part.join(&name),
            };
            std::io::copy(&mut entry, &mut File::create(&to)?)?;
        }
        let library = part.join(LIBRARY);
        let version: i64 = Connection::open(&library)?
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|_| anyhow!("The library in this backup can’t be read."))?;
        if version < 1 {
            bail!("The library in this backup is empty.");
        }
        if version > crate::db::VERSION {
            bail!("This backup’s library is from a newer Telly than this one. Update Telly, then restore it.");
        }
        drop(Db::open(&library).context("The library in this backup can’t be read")?);
        drop(Log::open(&part.join(LOG)).context("Tidepool’s log in this backup can’t be read")?);
        Ok(())
    })();
    if let Err(e) = unpacked.and_then(|()| Ok(std::fs::rename(&part, &staged)?)) {
        let _ = std::fs::remove_dir_all(&part);
        return Err(e);
    }
    Ok(json!(manifest))
}

/// Puts a backup `stage` unpacked in the library's place, as Telly starts and
/// before anything opens the library: what was restored, or None when there
/// was nothing to restore. When it fails, the library is as it was.
pub fn finish_restore(dirs: &Dirs) -> Result<Option<Manifest>> {
    let _ = std::fs::remove_dir_all(dirs.local.join(UNPACKING));
    let staged = dirs.local.join(STAGED);
    if !staged.is_dir() {
        return Ok(None);
    }
    let restored = restore(dirs, &staged);
    let _ = std::fs::remove_dir_all(&staged);
    restored.map(Some)
}

fn restore(dirs: &Dirs, staged: &Path) -> Result<Manifest> {
    let manifest: Manifest = std::fs::read(staged.join(MANIFEST)).ok().and_then(|b| serde_json::from_slice(&b).ok()).ok_or_else(|| anyhow!("The backup to restore wasn’t all there."))?;
    if !staged.join(LIBRARY).is_file() || !staged.join(LOG).is_file() {
        bail!("The backup to restore wasn’t all there.");
    }
    let mut moves = Moves::default();
    if let Err(e) = swap(dirs, staged, &mut moves) {
        moves.undo();
        return Err(e);
    }
    match fit(dirs) {
        Ok(forget) => {
            for path in forget {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::remove_dir_all(dirs.local.join(ASIDE));
            let _ = std::fs::remove_dir_all(dirs.roaming.join(ASIDE));
            Ok(manifest)
        }
        Err(e) => {
            moves.undo();
            Err(e)
        }
    }
}

/// What a restore replaces, each with the name the backup has it by: None
/// for what isn't in a backup (line search's index, made again from the
/// subtitles). A database's -wal and -shm go with it. Tidepool's log comes
/// last: it's in another folder, maybe on another drive.
fn replaced(dirs: &Dirs) -> Vec<(PathBuf, Option<String>)> {
    let database = |dir: &Path, name: &str, restored: bool| {
        ["", "-wal", "-shm"].map(|suffix| {
            let file = format!("{name}{suffix}");
            (dir.join(&file), restored.then_some(file))
        })
    };
    let mut out = Vec::new();
    out.extend(database(&dirs.local, LIBRARY, true));
    out.extend(database(&dirs.local, "telly-lines.sqlite", false));
    out.push((dirs.subs(), Some(SUBTITLES.into())));
    for name in FILES {
        out.push((dirs.local.join(name), Some(name.into())));
    }
    out.extend(database(&dirs.roaming, LOG, true));
    out
}

/// Sets aside what the backup replaces, then moves the backup's in.
fn swap(dirs: &Dirs, staged: &Path, moves: &mut Moves) -> Result<()> {
    for dir in [&dirs.local, &dirs.roaming] {
        let _ = std::fs::remove_dir_all(dir.join(ASIDE));
    }
    let items = replaced(dirs);
    // The Telly that just closed to restart may hold its files for a moment yet.
    let patience = Instant::now() + Duration::from_secs(10);
    for (place, _) in &items {
        if std::fs::symlink_metadata(place).is_ok() {
            let aside = place.parent().expect("a folder").join(ASIDE);
            std::fs::create_dir_all(&aside)?;
            moves.go(place, &aside.join(place.file_name().expect("a name")), Some(patience))?;
        }
    }
    for (place, name) in &items {
        let Some(from) = name.as_ref().map(|name| staged.join(name)).filter(|from| from.exists()) else { continue };
        moves.go(&from, place, None)?;
    }
    Ok(())
}

/// Files and folders moved by a restore, to put back if it can't finish.
#[derive(Default)]
struct Moves(Vec<(PathBuf, PathBuf)>);

impl Moves {
    /// Moves `from` to `to`, trying again until `patience` runs out while something holds it.
    fn go(&mut self, from: &Path, to: &Path, patience: Option<Instant>) -> Result<()> {
        loop {
            match move_path(from, to) {
                Ok(()) => {
                    self.0.push((from.to_path_buf(), to.to_path_buf()));
                    return Ok(());
                }
                Err(e) if e.kind() != std::io::ErrorKind::NotFound && patience.is_some_and(|until| Instant::now() < until) => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => return Err(anyhow!("Telly couldn’t move {} to {}: {e}", from.display(), to.display())),
            }
        }
    }

    /// Puts everything back where it was, last moved first. The folders it
    /// was set aside in go only once they're empty.
    fn undo(self) {
        let mut asides = Vec::new();
        for (from, to) in self.0.into_iter().rev() {
            let _ = move_path(&to, &from);
            asides.extend(to.parent().filter(|dir| dir.ends_with(ASIDE)).map(Path::to_path_buf));
        }
        for dir in asides {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Moves a file or folder. A file goes to another drive by copying, as the
/// log's folder may be on another drive than the library's.
fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices && from.is_file() => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        moved => moved,
    }
}

/// Makes the restored library, now in place, this computer's. What belongs to
/// this computer is taken from the library it replaced (set aside): where its
/// copies go and what its Hachidori has installed, ffmpeg and the
/// dictionaries folder (else the backup's, when they're here too). Telly's
/// copies of episodes are kept only for episodes the same in both (the same
/// show and rip); the rest are made again. Says which of the replaced
/// library's files to remove once the restore is done: Telly's copies, seek
/// bar pictures and covers named by ids that are now another episode's or
/// show's, and the character dictionary, made again for the restored shows.
/// An error here means the restored library can't be opened, and the restore is undone.
fn fit(dirs: &Dirs) -> Result<Vec<PathBuf>> {
    let new = Db::open(&dirs.local.join(LIBRARY)).context("Telly can’t open the restored library")?;
    drop(Log::open(&dirs.roaming.join(LOG)).context("Telly can’t open the restored log")?);
    let aside = dirs.local.join(ASIDE).join(LIBRARY);
    let old = if aside.is_file() { Db::open(&aside).ok() } else { None };
    let mine = |key: &str| old.as_ref().and_then(|o| o.setting(key).ok().flatten());
    let installed_error = format!("{}.error", own_hachidori::INSTALLED);
    // A restore that failed on the computer the backup was made on isn't this one's to tell.
    new.set_setting(NOT_RESTORED, &Value::Null)?;
    for key in [MEDIA_FOLDER, MEDIA_APART, own_hachidori::INSTALLED, installed_error.as_str()] {
        new.set_setting(key, &mine(key).unwrap_or(Value::Null))?;
    }
    let file: fn(&Path) -> bool = Path::is_file;
    let folder: fn(&Path) -> bool = Path::is_dir;
    for (key, here) in [(FFMPEG, file), (own_hachidori::FOLDER, folder)] {
        let chosen = |v: &Value| v.as_str().is_some_and(|p| !p.trim().is_empty());
        let value = match mine(key).filter(chosen) {
            Some(value) => value,
            None => new.setting(key)?.filter(|v| chosen(v) && v.as_str().is_some_and(|p| here(Path::new(p.trim())))).unwrap_or(Value::Null),
        };
        new.set_setting(key, &value)?;
    }

    let old_episodes: HashMap<i64, Episode> = old.as_ref().and_then(|o| o.all_episodes().ok()).unwrap_or_default().into_iter().map(|e| (e.id, e)).collect();
    let new_episodes: HashMap<i64, Episode> = new.all_episodes()?.into_iter().map(|e| (e.id, e)).collect();
    let same = |e: &Episode| new_episodes.get(&e.id).is_some_and(|n| n.show_id == e.show_id && n.path == e.path);
    let copy = |e: &Episode| !e.play_path.is_empty() && e.play_path != e.path;
    for e in new_episodes.values().filter(|e| copy(e)) {
        match old_episodes.get(&e.id) {
            Some(o) if same(o) && copy(o) && Path::new(&o.play_path).is_file() => new.set_play_path(e.id, &o.play_path)?,
            _ => new.media_reset(e.id)?,
        }
    }
    let media = mine(MEDIA_FOLDER).and_then(|v| v.as_str().map(str::to_string)).filter(|f| !f.is_empty()).map(|f| crate::backend::copies_in(Path::new(&f))).unwrap_or_else(|| dirs.media());
    let mut forget = Vec::new();
    for o in old_episodes.values().filter(|o| !same(o)) {
        forget.extend(named_by(&media.join(o.show_id.to_string()), o.id));
    }
    let new_shows = new.shows()?;
    for o in old.as_ref().and_then(|o| o.shows().ok()).unwrap_or_default() {
        if !new_shows.iter().any(|n| n.id == o.id && n.folder == o.folder && n.anilist_id == o.anilist_id && n.cover_url == o.cover_url) {
            forget.extend(named_by(&dirs.covers(), o.id));
        }
    }
    forget.push(dirs.local.join("names").join(names::ZIP));
    Ok(forget)
}

/// The files in `dir` named by an id: 12.mp4, 12.jpg, 12.small.jpg.
fn named_by(dir: &Path, id: i64) -> Vec<PathBuf> {
    let id = id.to_string();
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().split('.').next() == Some(id.as_str()))
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{NewEpisode, NewShow};

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("telly-backup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn library(dir: &Path) -> Backend {
        Backend::open(Dirs { local: dir.join("local"), roaming: dir.join("roaming") }).unwrap()
    }

    /// Adds a show of `count` episodes, whose rips are in `rips`.
    fn add_show(backend: &Backend, title: &str, rips: &Path, count: i64) -> i64 {
        std::fs::create_dir_all(rips).unwrap();
        let episodes = (1..=count)
            .map(|n| {
                let path = rips.join(format!("{title} - {n:02}.mkv"));
                std::fs::write(&path, b"rip").unwrap();
                NewEpisode { number: n, path: path.to_string_lossy().into_owned(), size: 3, duration: 1400.0 }
            })
            .collect();
        backend.db.add_show(&NewShow { folder: rips.to_string_lossy().into_owned(), title: title.into(), episodes, ..Default::default() }).unwrap()
    }

    fn names_in(zip: &Path) -> Vec<String> {
        let archive = ZipArchive::new(File::open(zip).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort();
        names
    }

    fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn a_backup_has_what_telly_cant_make_again() {
        let dir = folder("what");
        let backend = library(&dir);
        let show = add_show(&backend, "Show", &dir.join("rips"), 1);
        let episode = backend.db.episodes(show).unwrap()[0].id;
        backend.db.set_setting("jimaku.key", &json!("key")).unwrap();
        let local = &backend.dirs.local;
        let subs = backend.dirs.subs();
        std::fs::create_dir_all(&subs).unwrap();
        for name in [format!("{episode}.json"), format!("{episode}.tried.json"), format!("{episode}.timing.json"), format!("{episode}.whisper.small.srt")] {
            std::fs::write(subs.join(name), b"[]").unwrap();
        }
        // Pieces of audio being heard, and a file half written: not wanted.
        for name in [format!("{episode}.piece.wav"), format!("{episode}.json.part-7-0")] {
            std::fs::write(subs.join(name), b"x").unwrap();
        }
        std::fs::write(local.join("known-words.json"), b"{}").unwrap();
        std::fs::write(local.join("anilist-list.json"), b"[]").unwrap();
        // What Telly makes again.
        std::fs::write(local.join("recommended.json"), b"[]").unwrap();
        for (folder, name) in [("covers", format!("{show}.jpg")), ("media", format!("{show}/{episode}.mp4")), ("names", "telly-characters.zip".into())] {
            let path = local.join(folder).join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"made").unwrap();
        }

        // Saved as a .zip, whatever name it was given.
        let zip = dir.join("Telly backup.zip");
        let made = back_up(&backend, &dir.join("Telly backup").to_string_lossy()).unwrap();
        assert_eq!((made["shows"].clone(), made["path"].clone()), (json!(1), json!(zip)));
        assert!(made["bytes"].as_u64().unwrap() > 0);
        assert!(!dir.join("Telly backup.zip.part").exists());
        let mut expected: Vec<String> = vec!["anilist-list.json".into(), "known-words.json".into(), "telly-backup.json".into(), "telly-log.sqlite".into(), "telly.sqlite".into()];
        for name in [format!("{episode}.json"), format!("{episode}.timing.json"), format!("{episode}.tried.json"), format!("{episode}.whisper.small.srt")] {
            expected.push(format!("subtitles/{name}"));
        }
        expected.sort();
        assert_eq!(names_in(&zip), expected);
        // The library in it is whole, and the snapshots it was made from are gone.
        let mut archive = ZipArchive::new(File::open(&zip).unwrap()).unwrap();
        let mut bytes = Vec::new();
        archive.by_name(LIBRARY).unwrap().read_to_end(&mut bytes).unwrap();
        let copy = dir.join("copy.sqlite");
        std::fs::write(&copy, bytes).unwrap();
        let conn = Connection::open(&copy).unwrap();
        assert_eq!(conn.query_row("SELECT title FROM shows", [], |r| r.get::<_, String>(0)).unwrap(), "Show");
        assert_eq!(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), crate::db::VERSION);
        assert!(std::fs::read_dir(local).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains(".backup-")));
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reading_a_backup_says_what_s_in_it() {
        let dir = folder("read");
        let backend = library(&dir);
        add_show(&backend, "One", &dir.join("one"), 3);
        add_show(&backend, "Two", &dir.join("two"), 2);
        let zip = dir.join("backup.zip");
        let before = now_ms();
        back_up(&backend, &zip.to_string_lossy()).unwrap();
        let read = read(&backend, &format!(" {} ", zip.display())).unwrap();
        assert_eq!(read["telly"], json!(env!("CARGO_PKG_VERSION")));
        assert_eq!((read["shows"].clone(), read["episodes"].clone(), read["showsHere"].clone()), (json!(2), json!(5), json!(2)));
        assert!(read["madeAt"].as_i64().unwrap() >= before);
        assert_eq!(read["path"], json!(zip.to_string_lossy()));
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_backup_telly_made_is_restored() {
        let dir = folder("refuse");
        let backend = library(&dir);
        let manifest = |version: &str| serde_json::to_vec(&Manifest { telly: version.into(), made_at: 1, shows: 0, episodes: 0 }).unwrap();
        let this = manifest(env!("CARGO_PKG_VERSION"));
        let later = manifest("99.0.0");
        // A zip's name, what's in it, and what the refusal says.
        type Case<'a> = (&'a str, Vec<(&'a str, &'a [u8])>, &'a str);
        let cases: Vec<Case> = vec![
            ("other.zip", vec![("photo.jpg", b"jpg")], "it has photo.jpg in it"),
            ("climbs.zip", vec![(MANIFEST, &this), (LIBRARY, b""), (LOG, b""), ("subtitles/../../evil.json", b"[]")], "evil.json"),
            ("absolute.zip", vec![(MANIFEST, &this), (LIBRARY, b""), (LOG, b""), ("/etc/evil", b"x")], "/etc/evil"),
            ("deeper.zip", vec![(MANIFEST, &this), (LIBRARY, b""), (LOG, b""), ("subtitles/a/b.json", b"x")], "subtitles/a/b.json"),
            ("unsaid.zip", vec![(LIBRARY, b""), (LOG, b"")], "isn’t a backup of Telly"),
            ("empty.zip", vec![(MANIFEST, &this), (LOG, b"")], "missing your library"),
            ("newer.zip", vec![(MANIFEST, &later), (LIBRARY, b""), (LOG, b"")], "newer than this one"),
        ];
        for (name, files, says) in cases {
            let path = dir.join(name);
            write_zip(&path, &files);
            let error = read(&backend, &path.to_string_lossy()).unwrap_err().to_string();
            assert!(error.contains(says), "{name}: {error}");
            assert!(stage(&backend, &path.to_string_lossy()).is_err(), "{name}");
        }
        // Not a zip at all.
        let text = dir.join("notes.zip");
        std::fs::write(&text, b"not a zip").unwrap();
        assert!(read(&backend, &text.to_string_lossy()).unwrap_err().to_string().contains("isn’t a backup of Telly"));
        // A library that isn't one, with a manifest: refused as it's unpacked.
        let fake = dir.join("fake.zip");
        write_zip(&fake, &[(MANIFEST, &this), (LIBRARY, b"not a database"), (LOG, b"")]);
        assert!(stage(&backend, &fake.to_string_lossy()).unwrap_err().to_string().contains("can’t be read"));
        // Nothing waits to be restored, and nothing was written outside.
        assert!(!backend.dirs.local.join(STAGED).exists() && !backend.dirs.local.join(UNPACKING).exists());
        assert!(!dir.join("evil.json").exists());
        assert!(newer("0.10.0", "0.9.9") && !newer("0.3.4", "0.3.4") && !newer("0.3.3", "0.3.4"));
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restore_brings_the_library_back_as_telly_starts() {
        let dir = folder("restore");
        // The library backed up, on the old computer.
        let old = library(&dir.join("old"));
        let show = add_show(&old, "Show", &dir.join("old-rips"), 2);
        let eps = old.db.episodes(show).unwrap();
        old.db.save_position(eps[0].id, 612.0).unwrap();
        old.db.set_nudge(eps[0].id, -250).unwrap();
        old.db.set_setting("jimaku.key", &json!("old key")).unwrap();
        old.db.set_setting("setup.done", &json!(true)).unwrap();
        old.db.set_setting(MEDIA_FOLDER, &json!(dir.join("old-copies").to_string_lossy())).unwrap();
        old.db.set_setting(FFMPEG, &json!(dir.join("nowhere").join("ffmpeg.exe").to_string_lossy())).unwrap();
        old.db.set_setting(own_hachidori::INSTALLED, &json!({ "jitendex.zip": "1:1" })).unwrap();
        old.db.media_ready(eps[1].id, &dir.join("old-copies").join(format!("{show}/{}.mp4", eps[1].id)).to_string_lossy(), 0, 0).unwrap();
        std::fs::create_dir_all(old.dirs.subs()).unwrap();
        std::fs::write(old.dirs.subs().join(format!("{}.json", eps[0].id)), b"[\"old line\"]").unwrap();
        let zip = dir.join("backup.zip");
        back_up(&old, &zip.to_string_lossy()).unwrap();
        drop(old);

        // A new computer, where Telly was tried first: a show of its own, with
        // the same ids, a copy, a cover and a line index.
        let here = dir.join("new");
        let new = library(&here);
        let other = add_show(&new, "Other", &dir.join("other-rips"), 2);
        let theirs = new.db.episodes(other).unwrap();
        assert_eq!((other, theirs[1].id), (show, eps[1].id));
        let copy = new.dirs.media().join(format!("{other}/{}.mp4", theirs[1].id));
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::write(&copy, b"another show").unwrap();
        new.db.media_ready(theirs[1].id, &copy.to_string_lossy(), 0, 0).unwrap();
        std::fs::create_dir_all(new.dirs.covers()).unwrap();
        std::fs::write(new.dirs.covers().join(format!("{other}.jpg")), b"cover").unwrap();
        std::fs::create_dir_all(new.dirs.subs()).unwrap();
        std::fs::write(new.dirs.subs().join(format!("{}.json", theirs[0].id)), b"[\"other line\"]").unwrap();
        new.db.set_setting(own_hachidori::INSTALLED, &json!({ "jmdict.zip": "2:2" })).unwrap();
        let staged = stage(&new, &zip.to_string_lossy()).unwrap();
        assert_eq!(staged["shows"], json!(1));
        // Nothing changes until Telly starts again.
        assert_eq!(new.db.shows().unwrap()[0].title, "Other");
        drop(new);

        let restored = library(&here);
        let shows = restored.db.shows().unwrap();
        assert_eq!(shows.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["Show"]);
        let back = restored.db.episodes(show).unwrap();
        assert_eq!((back[0].position, back[0].subs_nudge_ms), (612.0, -250));
        assert_eq!(restored.db.setting_str("jimaku.key"), "old key");
        assert_eq!(restored.db.setting(RESTORED).unwrap().unwrap()["shows"], json!(1));
        assert_eq!(std::fs::read(restored.dirs.subs().join(format!("{}.json", eps[0].id))).unwrap(), b"[\"old line\"]");
        // What belongs to this computer: its own copies folder (none chosen)
        // and what its Hachidori has; an ffmpeg that isn't here is forgotten.
        assert_eq!(restored.db.setting(MEDIA_FOLDER).unwrap(), None);
        assert_eq!(restored.db.setting(own_hachidori::INSTALLED).unwrap(), Some(json!({ "jmdict.zip": "2:2" })));
        assert_eq!(restored.db.setting(FFMPEG).unwrap(), None);
        // The copy wasn't in the backup: the episode is converted again. The
        // other show's copy and cover, named by the same ids, are gone.
        assert_eq!((back[1].media_state.as_str(), back[1].play_path.as_str()), ("pending", ""));
        assert!(!copy.exists() && !restored.dirs.covers().join(format!("{other}.jpg")).exists());
        // Nothing is left over from the restore.
        for name in [STAGED, UNPACKING, ASIDE] {
            assert!(!here.join("local").join(name).exists() && !here.join("roaming").join(name).exists(), "{name}");
        }
        drop(restored);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restoring_onto_the_same_library_keeps_its_copies() {
        let dir = folder("same");
        let backend = library(&dir);
        let show = add_show(&backend, "Show", &dir.join("rips"), 1);
        let episode = backend.db.episodes(show).unwrap()[0].id;
        let copy = backend.dirs.media().join(format!("{show}/{episode}.mp4"));
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::write(&copy, b"copy").unwrap();
        std::fs::write(copy.with_extension("jpg"), b"pictures").unwrap();
        backend.db.media_ready(episode, &copy.to_string_lossy(), 0, 0).unwrap();
        std::fs::create_dir_all(backend.dirs.covers()).unwrap();
        std::fs::write(backend.dirs.covers().join(format!("{show}.jpg")), b"cover").unwrap();
        let zip = dir.join("backup.zip");
        back_up(&backend, &zip.to_string_lossy()).unwrap();
        backend.db.save_position(episode, 100.0).unwrap();
        stage(&backend, &zip.to_string_lossy()).unwrap();
        drop(backend);

        let restored = library(&dir);
        let e = restored.db.episode(episode).unwrap().unwrap();
        assert_eq!((e.position, e.media_state.as_str(), e.play_path.as_str()), (0.0, "ready", copy.to_string_lossy().as_ref()));
        assert!(copy.is_file() && copy.with_extension("jpg").is_file() && restored.dirs.covers().join(format!("{show}.jpg")).is_file());
        drop(restored);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restore_that_fails_leaves_the_library_as_it_was() {
        let dir = folder("fails");
        let backup = library(&dir.join("backed-up"));
        add_show(&backup, "Backed up", &dir.join("rips"), 1);
        let zip = dir.join("backup.zip");
        back_up(&backup, &zip.to_string_lossy()).unwrap();
        drop(backup);

        let here = dir.join("here");
        let backend = library(&here);
        let show = add_show(&backend, "Here", &dir.join("here-rips"), 1);
        let episode = backend.db.episodes(show).unwrap()[0].id;
        std::fs::create_dir_all(backend.dirs.subs()).unwrap();
        std::fs::write(backend.dirs.subs().join(format!("{episode}.json")), b"[\"here\"]").unwrap();
        std::fs::write(backend.dirs.local.join("known-words.json"), b"{\"here\":1}").unwrap();
        stage(&backend, &zip.to_string_lossy()).unwrap();
        drop(backend);

        // The log's folder can't be written to (it's below a file), so the
        // log, put in place last, can't be.
        std::fs::write(dir.join("a file"), b"").unwrap();
        let broken = Dirs { local: here.join("local"), roaming: dir.join("a file").join("roaming") };
        let error = finish_restore(&broken).unwrap_err().to_string();
        assert!(error.contains("telly-log.sqlite"), "{error}");
        assert!(!here.join("local").join(STAGED).exists() && !here.join("local").join(ASIDE).exists());

        let after = library(&here);
        assert_eq!(after.db.shows().unwrap().iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["Here"]);
        assert_eq!(std::fs::read(after.dirs.subs().join(format!("{episode}.json"))).unwrap(), b"[\"here\"]");
        assert_eq!(std::fs::read(after.dirs.local.join("known-words.json")).unwrap(), b"{\"here\":1}");
        assert_eq!(after.db.setting(RESTORED).unwrap(), None);
        drop(after);

        // A restored library that can't be opened is put back too.
        let backend = library(&here);
        stage(&backend, &zip.to_string_lossy()).unwrap();
        drop(backend);
        std::fs::write(here.join("local").join(STAGED).join(LIBRARY), b"not a database any more").unwrap();
        assert!(finish_restore(&Dirs { local: here.join("local"), roaming: here.join("roaming") }).is_err());
        let after = library(&here);
        assert_eq!(after.db.shows().unwrap()[0].title, "Here");
        drop(after);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
