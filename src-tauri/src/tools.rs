//! The programs Telly drives: ffmpeg and ffprobe (probing, converting, cutting
//! clips, reading audio for sync) and, when it's there, whisper.cpp.
//!
//! Telly doesn't ship ffmpeg. It uses the one you chose in Settings, else one
//! in Telly's own tools folder, else one on PATH, else the copy another
//! Japanese tool on this PC keeps (GameSentenceMiner, Fushi, Migaku).

use crate::util::command;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tools {
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    /// ffmpeg's version line.
    pub version: String,
    /// The H.264 encoder conversions use: a GPU's (h264_amf, h264_nvenc, h264_qsv) or libx264.
    pub encoder: String,
    /// It can make animated AVIF and Opus for cards.
    pub can_clip: bool,
    pub whisper: Option<PathBuf>,
    pub whisper_model: Option<PathBuf>,
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(exe(name))).find(|p| p.is_file())
}

/// Where to look for ffmpeg, best first.
fn ffmpeg_candidates(tools_dir: &Path, custom: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !custom.trim().is_empty() {
        let p = PathBuf::from(custom.trim());
        out.push(if p.is_dir() { p.join(exe("ffmpeg")) } else { p });
    }
    out.push(tools_dir.join(exe("ffmpeg")));
    out.push(tools_dir.join("ffmpeg").join("bin").join(exe("ffmpeg")));
    if let Some(p) = on_path("ffmpeg") {
        out.push(p);
    }
    if let Some(roaming) = env_dir("APPDATA") {
        out.push(roaming.join("GameSentenceMiner").join("ffmpeg").join(exe("ffmpeg")));
    }
    if let Some(local) = env_dir("LOCALAPPDATA") {
        out.push(local.join("Fushi").join(exe("ffmpeg")));
        out.push(local.join("Migaku").join("MigakuShared").join(exe("ffmpeg")));
    }
    out
}

fn whisper_candidates(tools_dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![tools_dir.join("whisper").join(exe("whisper-cli"))];
    if let Some(local) = env_dir("LOCALAPPDATA") {
        out.push(local.join("Tidepool").join("whisper").join("Release").join(exe("whisper-cli")));
    }
    out
}

/// A multilingual whisper.cpp model in Telly's models folder (English-only
/// ".en" models can't hear Japanese).
fn whisper_model(models_dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(models_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            name.starts_with("ggml-") && name.ends_with(".bin") && !name.contains(".en")
        })
        .collect();
    // Bigger models hear better; prefer them.
    found.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    found.into_iter().next()
}

/// Whether ffmpeg can encode a moment of video with `encoder` (a GPU encoder
/// is listed even when there's no such GPU).
fn encoder_works(ffmpeg: &Path, encoder: &str) -> bool {
    let format = if encoder == "libx264" { "yuv420p" } else { "nv12" };
    command(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-v", "error", "-f", "lavfi", "-i", "color=c=gray:s=320x240:r=24:d=0.5"])
        .args(["-vf", &format!("format={format}"), "-c:v", encoder, "-f", "null", "-"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn encoders(ffmpeg: &Path) -> String {
    command(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

pub fn find(tools_dir: &Path, models_dir: &Path, custom_ffmpeg: &str) -> Tools {
    let mut tools = Tools::default();
    for candidate in ffmpeg_candidates(tools_dir, custom_ffmpeg) {
        let probe = candidate.with_file_name(exe("ffprobe"));
        if candidate.is_file() && probe.is_file() {
            tools.ffmpeg = Some(candidate);
            tools.ffprobe = Some(probe);
            break;
        }
    }
    if let Some(ffmpeg) = &tools.ffmpeg {
        tools.version = command(ffmpeg)
            .arg("-version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string())
            .unwrap_or_default();
        let list = encoders(ffmpeg);
        tools.can_clip = list.contains("libsvtav1") && list.contains("libopus");
        tools.encoder = ["h264_nvenc", "h264_amf", "h264_qsv"]
            .into_iter()
            .find(|e| list.contains(e) && encoder_works(ffmpeg, e))
            .unwrap_or("libx264")
            .to_string();
    }
    tools.whisper = whisper_candidates(tools_dir).into_iter().find(|p| p.is_file());
    tools.whisper_model = whisper_model(models_dir);
    tools
}
