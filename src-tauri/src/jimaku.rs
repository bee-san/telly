//! Jimaku (https://jimaku.cc): Japanese subtitles for anime, found by AniList ID.
//! It needs a free account's API key, sent as the Authorization header.
//!
//!   GET /api/entries/search?anilist_id=20&anime=true   the show's entries
//!   GET /api/entries/{id}/files                         its files (zips too)
//!
//! Jimaku's own episode filter is a guess from file names, so Telly reads the
//! names itself, and looks inside zips (season packs) too.

use crate::scan::episode_number;
use crate::util::safe_file_name;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

const API: &str = "https://jimaku.cc/api";
const USER_AGENT: &str = concat!("Telly/", env!("CARGO_PKG_VERSION"), " (personal anime player)");
const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "vtt"];
/// Season packs bigger than this aren't downloaded.
const MAX_ZIP: u64 = 80 * 1024 * 1024;

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: i64,
    pub name: String,
    pub japanese_name: String,
    pub anilist_id: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFile {
    pub name: String,
    pub url: String,
    pub size: u64,
}

fn get_json(key: &str, url: &str) -> Result<Value> {
    let mut response = agent()
        .get(url)
        .header("Authorization", key)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json")
        .call()
        .map_err(|e| anyhow!("Jimaku isn’t reachable ({e})"))?;
    match response.status().as_u16() {
        200 => {}
        401 | 403 => return Err(anyhow!("Jimaku didn’t accept the API key. Check it in Settings.")),
        429 => return Err(anyhow!("Jimaku asked Telly to slow down")),
        code => return Err(anyhow!("Jimaku answered {code}")),
    }
    response.body_mut().with_config().limit(16 * 1024 * 1024).read_json().context("Jimaku sent something unexpected")
}

fn entries_from(v: &Value) -> Vec<Entry> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some(Entry {
                id: e.get("id")?.as_i64()?,
                name: e.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                japanese_name: e.get("japanese_name").and_then(Value::as_str).unwrap_or("").to_string(),
                anilist_id: e.get("anilist_id").and_then(Value::as_i64),
            })
        })
        .collect()
}

/// The show's entries on Jimaku: by AniList ID, else by name.
pub fn find_entries(key: &str, anilist_id: Option<i64>, title: &str) -> Result<Vec<Entry>> {
    if let Some(id) = anilist_id {
        let found = entries_from(&get_json(key, &format!("{API}/entries/search?anilist_id={id}&anime=true"))?);
        if !found.is_empty() {
            return Ok(found);
        }
    }
    if title.trim().is_empty() {
        return Ok(Vec::new());
    }
    let q = percent_encoding::utf8_percent_encode(title.trim(), percent_encoding::NON_ALPHANUMERIC).to_string();
    let found = entries_from(&get_json(key, &format!("{API}/entries/search?query={q}&anime=true"))?);
    // A name search can find other shows; keep only ones without a different AniList ID.
    Ok(found.into_iter().filter(|e| anilist_id.is_none() || e.anilist_id.is_none() || e.anilist_id == anilist_id).collect())
}

pub fn files(key: &str, entry_id: i64) -> Result<Vec<RemoteFile>> {
    let v = get_json(key, &format!("{API}/entries/{entry_id}/files"))?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some(RemoteFile {
                name: f.get("name")?.as_str()?.to_string(),
                url: f.get("url")?.as_str()?.to_string(),
                size: f.get("size").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect())
}

fn download(key: &str, url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut response = agent()
        .get(url)
        .header("Authorization", key)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| anyhow!("Jimaku isn’t reachable ({e})"))?;
    if response.status().as_u16() != 200 {
        return Err(anyhow!("Jimaku answered {} for {url}", response.status().as_u16()));
    }
    Ok(response.body_mut().with_config().limit(limit).read_to_vec()?)
}

fn extension(name: &str) -> String {
    Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn is_subtitle(name: &str) -> bool {
    SUBTITLE_EXTENSIONS.contains(&extension(name).as_str())
}

/// A subtitle file on this PC, from Jimaku (or inside one of its zips).
#[derive(Clone, Debug, PartialEq)]
pub struct Local {
    pub name: String,
    pub path: PathBuf,
    pub episode: Option<i64>,
}

fn episode_of(name: &str) -> Option<i64> {
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    // "Show.S01E04.ja" and "Show - 04.ja" keep their language tag in the stem.
    let stem = stem.trim_end_matches(".ja").trim_end_matches(".jp").trim_end_matches(".jpn");
    episode_number(stem).map(|(_, e)| e)
}

/// Downloads (once) every subtitle file of an entry into `dir`, unpacking zips.
/// Returns them all, with the episode each name gives.
pub fn fetch_all(key: &str, entry_id: i64, dir: &Path) -> Result<Vec<Local>> {
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::new();
    for file in files(key, entry_id)? {
        let ext = extension(&file.name);
        if is_subtitle(&file.name) {
            let path = dir.join(safe_file_name(&file.name));
            if !path.is_file() {
                let bytes = download(key, &file.url, 16 * 1024 * 1024)?;
                std::fs::write(&path, bytes)?;
            }
            out.push(Local { episode: episode_of(&file.name), name: file.name, path });
        } else if ext == "zip" && file.size <= MAX_ZIP {
            let unpacked = dir.join(format!("{}.d", safe_file_name(&file.name)));
            if !unpacked.is_dir() {
                let bytes = download(key, &file.url, MAX_ZIP)?;
                unzip_subtitles(&bytes, &unpacked).with_context(|| format!("unpacking {}", file.name))?;
            }
            for entry in walk(&unpacked) {
                let name = entry.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                out.push(Local { episode: episode_of(&name), name: format!("{} › {name}", file.name), path: entry });
            }
        }
    }
    Ok(out)
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else if p.file_name().map(|n| is_subtitle(&n.to_string_lossy())).unwrap_or(false) {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// Writes the subtitle files in a zip into `dir` (flat, with safe names).
pub fn unzip_subtitles(bytes: &[u8], dir: &Path) -> Result<()> {
    let staging = dir.with_extension("part");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name()?.replace('\\', "/");
        let base = name.rsplit('/').next().unwrap_or(&name).to_string();
        if !is_subtitle(&base) || entry.size() > 16 * 1024 * 1024 {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        // Folders in the zip (one per season, say) are kept in the name.
        let flat = safe_file_name(&name.replace('/', " - "));
        std::fs::write(staging.join(flat), data)?;
    }
    let _ = std::fs::remove_dir_all(dir);
    std::fs::rename(&staging, dir)?;
    Ok(())
}

/// The files for `episode`, best guess first: names that say this episode,
/// Japanese-marked names before others, SRT before ASS (ASS often carries signs).
pub fn candidates(all: &[Local], episode: i64, episodes_in_show: usize) -> Vec<Local> {
    let mut found: Vec<Local> = all.iter().filter(|l| l.episode == Some(episode)).cloned().collect();
    // A single-episode show (a film) can take a file without a number.
    if found.is_empty() && episodes_in_show == 1 {
        found = all.iter().filter(|l| l.episode.is_none()).cloned().collect();
    }
    let rank = |l: &Local| {
        let lower = l.name.to_lowercase();
        let marked = lower.contains(".ja.") || lower.contains(".jp.") || lower.contains("jpn") || lower.contains("japanese") || l.name.contains("日本語");
        let other_language = [".en.", ".eng.", "english", ".zh", "chinese", ".ko."].iter().any(|m| lower.contains(m));
        (other_language, !marked, extension(&l.name) != "srt", lower.len())
    };
    found.sort_by_key(rank);
    found.dedup_by(|a, b| std::fs::metadata(&a.path).map(|m| m.len()).ok() == std::fs::metadata(&b.path).map(|m| m.len()).ok() && extension(&a.name) == extension(&b.name));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_episode_numbers_from_jimaku_names() {
        assert_eq!(episode_of("[Netflix] Fairy Tail - 004.ja.srt"), Some(4));
        assert_eq!(episode_of("FAIRY TAIL S01E12.srt"), Some(12));
        assert_eq!(episode_of("フェアリーテイル 第12話.ass"), Some(12));
        assert_eq!(episode_of("Fairy Tail 175.srt"), Some(175));
    }

    #[test]
    fn unzips_season_packs() {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buffer);
            let options: zip::write::SimpleFileOptions = Default::default();
            for (name, body) in [("Season 1/Show - 01.srt", "a"), ("Season 1/Show - 02.ass", "b"), ("readme.txt", "c")] {
                zip.start_file(name, options).unwrap();
                std::io::Write::write_all(&mut zip, body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let dir = std::env::temp_dir().join(format!("telly-jimaku-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unzip_subtitles(buffer.get_ref(), &dir.join("pack.zip.d")).unwrap();
        let files = walk(&dir.join("pack.zip.d"));
        let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(names, ["Season 1 - Show - 01.srt", "Season 1 - Show - 02.ass"]);
        let locals: Vec<Local> = files.into_iter().map(|p| Local { name: p.file_name().unwrap().to_string_lossy().to_string(), episode: episode_of(&p.file_name().unwrap().to_string_lossy()), path: p }).collect();
        assert_eq!(candidates(&locals, 2, 12).len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
