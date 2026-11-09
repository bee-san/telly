//! Cutting a card's media from the episode: an animated AVIF of the line and
//! the line's audio as Opus, both from Telly's copy (or the rip when it plays
//! as it is).
//!
//! AVIF: SVT-AV1, preset 8. Anime is mostly drawn on twos, so 12 fps loses
//! little. Longer lines step down in frame rate, width and quality to keep the
//! file small, as Nagare does.
//!
//! Opus: mono, 48 kb/s, loudness-normalised. About 20 KB for a 3-second line.

use crate::util::command;
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Clips stop at this length.
pub const MAX_SECONDS: f64 = 12.0;

/// (fps, width, crf) for a clip `seconds` long.
pub fn tier(seconds: f64) -> (u32, u32, u32) {
    if seconds <= 5.0 {
        (12, 640, 40)
    } else if seconds <= 8.0 {
        (10, 560, 42)
    } else {
        (8, 480, 45)
    }
}

pub struct Clip {
    pub picture: PathBuf,
    pub audio: PathBuf,
}

fn run(ffmpeg: &Path, args: &[String]) -> Result<()> {
    let out = command(ffmpeg).args(args).stdin(Stdio::null()).output()?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("ffmpeg: {}", text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed")));
    }
    Ok(())
}

pub fn picture_args(video: &Path, start: f64, length: f64, out: &Path) -> Vec<String> {
    let (fps, width, crf) = tier(length);
    let filter = format!("fps={fps},scale='min({width},iw)':-2:flags=lanczos,format=yuv420p");
    [
        "-nostdin", "-hide_banner", "-v", "error", "-y", "-ss", &format!("{start:.3}"), "-i", &video.to_string_lossy(),
        "-t", &format!("{length:.3}"), "-map", "0:v:0", "-an", "-sn", "-vf", &filter, "-c:v", "libsvtav1", "-preset", "8",
        "-crf", &crf.to_string(), "-loop", "0", "-f", "avif", &out.to_string_lossy(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub fn audio_args(video: &Path, audio_index: i64, start: f64, length: f64, out: &Path) -> Vec<String> {
    [
        "-nostdin", "-hide_banner", "-v", "error", "-y", "-ss", &format!("{start:.3}"), "-i", &video.to_string_lossy(),
        "-t", &format!("{length:.3}"), "-map", &format!("0:a:{audio_index}"), "-vn", "-sn", "-ac", "1", "-ar", "48000",
        "-af", "loudnorm=I=-16:TP=-1.5:LRA=11", "-c:a", "libopus", "-b:a", "48k", "-f", "opus", &out.to_string_lossy(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Cuts `start`..`end` (seconds) of `video` into `dir`, as `<base>.avif` and `<base>.opus`.
pub fn cut(ffmpeg: &Path, video: &Path, audio_index: i64, start: f64, end: f64, dir: &Path, base: &str) -> Result<Clip> {
    std::fs::create_dir_all(dir)?;
    let start = start.max(0.0);
    let length = (end - start).clamp(0.3, MAX_SECONDS);
    let picture = dir.join(format!("{base}.avif"));
    let audio = dir.join(format!("{base}.opus"));
    let (p_args, a_args) = (picture_args(video, start, length, &picture), audio_args(video, audio_index, start, length, &audio));
    let (p, a) = std::thread::scope(|scope| {
        let p = scope.spawn(|| run(ffmpeg, &p_args));
        let a = scope.spawn(|| run(ffmpeg, &a_args));
        (p.join(), a.join())
    });
    p.map_err(|_| anyhow!("the picture cutter crashed"))??;
    a.map_err(|_| anyhow!("the audio cutter crashed"))??;
    Ok(Clip { picture, audio })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_down_for_long_lines() {
        assert_eq!(tier(3.0), (12, 640, 40));
        assert_eq!(tier(7.0), (10, 560, 42));
        assert_eq!(tier(11.0), (8, 480, 45));
        let args = picture_args(Path::new("ep.mp4"), 730.75, 3.5, Path::new("c.avif")).join(" ");
        assert!(args.contains("-ss 730.750") && args.contains("libsvtav1") && args.contains("fps=12"), "{args}");
    }
}
