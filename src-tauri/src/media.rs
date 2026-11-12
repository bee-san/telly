//! What's in a rip (ffprobe), what WebView2 can play, and making a copy it
//! can (ffmpeg).
//!
//! WebView2 plays H.264, HEVC, VP9 and AV1, 8-bit or 10-bit, with AAC, MP3,
//! Opus, FLAC, AC-3 or E-AC-3, in MP4 or MKV (one file of each kind played in
//! Edge 157's engine, HEVC on the graphics card). Rips straight from MakeMKV
//! are usually MPEG-2, which it can't. So each episode gets one of four plans:
//!
//!   direct  it plays as it is: the player loads the rip itself
//!   remux   right codecs, wrong container (AVI): copied into MP4, in seconds
//!   audio   the audio (DTS, TrueHD) is converted to AAC and the video copied
//!   video   converted to H.264, on the GPU's encoder when there is one, with
//!           the DVD's telecine undone or the picture deinterlaced, square
//!           pixels, and a keyframe every 2 seconds so seeking is instant
//!
//! The Japanese audio track is picked by its language tag, unless the player
//! picked another for the show. A rip that plays as it is keeps all its
//! tracks, and the player switches to that one; Telly's copy holds only it.

use crate::util::command;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub codec: String,
    pub profile: String,
    pub pix_fmt: String,
    pub width: i64,
    pub height: i64,
    /// Sample aspect ratio, width over height (DVDs aren't square pixels).
    pub sar: f64,
    pub field_order: String,
    pub fps: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioInfo {
    /// The stream's index in the file.
    pub index: i64,
    pub codec: String,
    pub channels: i64,
    pub language: String,
    pub title: String,
    pub default: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleInfo {
    pub index: i64,
    pub codec: String,
    pub language: String,
    pub title: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Chapter {
    pub start: f64,
    pub end: f64,
    pub title: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub format: String,
    pub duration: f64,
    pub video: Option<VideoInfo>,
    pub audio: Vec<AudioInfo>,
    pub subtitles: Vec<SubtitleInfo>,
    pub chapters: Vec<Chapter>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    Direct,
    Remux,
    Audio,
    Video,
}

impl Plan {
    pub fn name(self) -> &'static str {
        match self {
            Plan::Direct => "direct",
            Plan::Remux => "remux",
            Plan::Audio => "audio",
            Plan::Video => "video",
        }
    }
}

fn ratio(text: &str) -> f64 {
    let mut parts = text.split([':', '/']);
    match (parts.next().and_then(|a| a.parse::<f64>().ok()), parts.next().and_then(|b| b.parse::<f64>().ok())) {
        (Some(a), Some(b)) if b != 0.0 && a > 0.0 => a / b,
        _ => 0.0,
    }
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn tag(v: &Value, key: &str) -> String {
    v.get("tags")
        .and_then(|t| t.as_object())
        .and_then(|t| t.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn f(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::String(t)) => t.parse().unwrap_or(0.0),
        Some(n) => n.as_f64().unwrap_or(0.0),
        None => 0.0,
    }
}

/// Reads ffprobe's JSON.
pub fn parse_probe(json: &Value) -> Probe {
    let mut probe = Probe {
        format: json.pointer("/format/format_name").and_then(Value::as_str).unwrap_or("").to_string(),
        duration: json.get("format").map(|f_| f(f_, "duration")).unwrap_or(0.0),
        ..Default::default()
    };
    for st in json.get("streams").and_then(Value::as_array).into_iter().flatten() {
        let default = st.pointer("/disposition/default").and_then(Value::as_i64) == Some(1);
        match st.get("codec_type").and_then(Value::as_str) {
            Some("video") if probe.video.is_none() => {
                // Cover art is a "video" stream too.
                if st.pointer("/disposition/attached_pic").and_then(Value::as_i64) == Some(1) {
                    continue;
                }
                let fps = ratio(&s(st, "r_frame_rate"));
                let avg = ratio(&s(st, "avg_frame_rate"));
                probe.video = Some(VideoInfo {
                    codec: s(st, "codec_name"),
                    profile: s(st, "profile"),
                    pix_fmt: s(st, "pix_fmt"),
                    width: st.get("width").and_then(Value::as_i64).unwrap_or(0),
                    height: st.get("height").and_then(Value::as_i64).unwrap_or(0),
                    sar: match ratio(&s(st, "sample_aspect_ratio")) {
                        r if r > 0.0 => r,
                        _ => 1.0,
                    },
                    field_order: s(st, "field_order"),
                    fps: if avg > 0.0 && avg < 120.0 { avg } else { fps },
                });
            }
            Some("audio") => probe.audio.push(AudioInfo {
                index: st.get("index").and_then(Value::as_i64).unwrap_or(0),
                codec: s(st, "codec_name"),
                channels: st.get("channels").and_then(Value::as_i64).unwrap_or(0),
                language: tag(st, "language"),
                title: tag(st, "title"),
                default,
            }),
            Some("subtitle") => probe.subtitles.push(SubtitleInfo {
                index: st.get("index").and_then(Value::as_i64).unwrap_or(0),
                codec: s(st, "codec_name"),
                language: tag(st, "language"),
                title: tag(st, "title"),
            }),
            _ => {}
        }
    }
    for ch in json.get("chapters").and_then(Value::as_array).into_iter().flatten() {
        probe.chapters.push(Chapter { start: f(ch, "start_time"), end: f(ch, "end_time"), title: tag(ch, "title") });
    }
    probe
}

pub fn probe(ffprobe: &Path, path: &Path) -> Result<Probe> {
    let out = command(ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", "-show_chapters"])
        .arg(path)
        .output()
        .context("running ffprobe")?;
    if !out.status.success() {
        return Err(anyhow!("ffprobe couldn’t read {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    let json: Value = serde_json::from_slice(&out.stdout).context("reading ffprobe’s answer")?;
    Ok(parse_probe(&json))
}

pub fn duration(ffprobe: &Path, path: &Path) -> Result<f64> {
    let out = command(ffprobe)
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().map_err(|_| anyhow!("no duration"))
}

fn is_japanese(a: &AudioInfo) -> bool {
    let lang = a.language.to_lowercase();
    let title = a.title.to_lowercase();
    matches!(lang.as_str(), "jpn" | "ja" | "jp" | "japanese") || title.contains("japanese") || a.title.contains("日本語")
}

/// The Japanese track's position among the audio tracks: by language tag,
/// else the default track, else the first.
pub fn japanese_audio(probe: &Probe) -> usize {
    probe
        .audio
        .iter()
        .position(is_japanese)
        .or_else(|| probe.audio.iter().position(|a| a.default))
        .unwrap_or(0)
}

fn video_plays(v: &VideoInfo) -> bool {
    let eight_bit = matches!(v.pix_fmt.as_str(), "yuv420p" | "yuvj420p" | "nv12");
    match v.codec.as_str() {
        // 10-bit H.264 has no hardware decoder: the processor decodes it.
        "h264" | "hevc" | "av1" => eight_bit || v.pix_fmt == "yuv420p10le",
        "vp8" | "vp9" => eight_bit,
        _ => false,
    }
}

/// Audio that goes into Telly's MP4 copy as it is; the rest is converted to AAC.
fn audio_plays_in_mp4(a: &AudioInfo) -> bool {
    matches!(a.codec.as_str(), "aac" | "mp3" | "opus")
}

/// Audio the player plays from the rip itself. It doesn't list a track it
/// can't play (DTS, TrueHD) among the rip's audio tracks.
pub fn audio_plays(a: &AudioInfo) -> bool {
    audio_plays_in_mp4(a) || matches!(a.codec.as_str(), "flac" | "ac3" | "eac3")
}

fn container_plays(probe: &Probe, path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    (probe.format.contains("mp4") && matches!(ext.as_str(), "mp4" | "m4v" | "mov"))
        || (probe.format.contains("matroska") && matches!(ext.as_str(), "mkv" | "webm"))
}

/// How to get an episode playing with its audio track `audio` (a position
/// among the audio tracks).
pub fn plan(probe: &Probe, path: &Path, audio: usize) -> Plan {
    let Some(video) = &probe.video else { return Plan::Video };
    let track = probe.audio.get(audio);
    if video_plays(video) && track.is_some_and(audio_plays) && container_plays(probe, path) {
        return Plan::Direct;
    }
    match (video_plays(video), track.is_some_and(audio_plays_in_mp4)) {
        (true, true) => Plan::Remux,
        (true, false) => Plan::Audio,
        _ => Plan::Video,
    }
}

/// Where the ending starts (see `ending`).
pub fn ending_at(probe: &Probe) -> Option<f64> {
    ending(probe).map(|(start, _)| start)
}

/// The ending, as (start, end): a chapter called that, else the first chapter
/// after 70% of the episode that's an ending's length.
pub fn ending(probe: &Probe) -> Option<(f64, f64)> {
    let d = probe.duration;
    if d <= 0.0 || probe.chapters.len() < 3 {
        return None;
    }
    let named = probe.chapters.iter().find(|c| {
        let t = c.title.to_lowercase();
        t.contains("ending") || t == "ed" || t.starts_with("ed ") || c.title.contains("エンディング")
    });
    named
        .or_else(|| probe.chapters.iter().find(|c| c.start > 0.7 * d && (55.0..=150.0).contains(&(c.end - c.start))))
        .map(|c| (c.start, c.end))
}

/// The opening, as (start, end): a chapter called that, else the chapter in
/// the first 30% of the episode that's nearest an opening's 90 seconds.
pub fn opening(probe: &Probe) -> Option<(f64, f64)> {
    let d = probe.duration;
    if d <= 0.0 || probe.chapters.len() < 3 {
        return None;
    }
    let named = probe.chapters.iter().find(|c| {
        let t = c.title.to_lowercase();
        t.contains("opening") || t == "op" || t.starts_with("op ") || c.title.contains("オープニング")
    });
    named
        .or_else(|| {
            probe
                .chapters
                .iter()
                .filter(|c| c.start < 0.3 * d && (55.0..=150.0).contains(&(c.end - c.start)))
                .min_by(|a, b| (a.end - a.start - 90.0).abs().total_cmp(&(b.end - b.start - 90.0).abs()))
        })
        .map(|c| (c.start, c.end))
}

/// ffmpeg's video filter for a conversion: undo telecine or deinterlace, then square pixels.
pub fn video_filter(v: &VideoInfo, encoder: &str) -> String {
    let mut filters: Vec<String> = Vec::new();
    let interlaced = matches!(v.field_order.as_str(), "tt" | "bb" | "tb" | "bt");
    let ntsc = (v.fps - 29.97).abs() < 0.1;
    if interlaced && ntsc {
        // Inverse telecine: anime on NTSC DVDs is mostly 24 fps with fields repeated.
        filters.push("fieldmatch=order=auto:combmatch=full".into());
        filters.push("yadif=deint=interlaced".into());
        filters.push("decimate".into());
    } else if interlaced {
        filters.push("bwdif=mode=send_frame:deint=interlaced".into());
    }
    if (v.sar - 1.0).abs() > 0.01 {
        filters.push("scale=trunc(iw*sar/2)*2:trunc(ih/2)*2".into());
        filters.push("setsar=1".into());
    }
    filters.push(format!("format={}", if encoder == "libx264" { "yuv420p" } else { "nv12" }));
    filters.join(",")
}

fn encoder_args(encoder: &str) -> Vec<String> {
    let args: &[&str] = match encoder {
        "h264_nvenc" => &["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", "23", "-b:v", "0", "-profile:v", "high"],
        "h264_amf" => &["-c:v", "h264_amf", "-usage", "transcoding", "-quality", "quality", "-rc", "cqp", "-qp_i", "22", "-qp_p", "24", "-qp_b", "26", "-profile:v", "high"],
        "h264_qsv" => &["-c:v", "h264_qsv", "-preset", "medium", "-global_quality", "23", "-profile:v", "high"],
        _ => &["-c:v", "libx264", "-preset", "veryfast", "-crf", "21", "-tune", "animation", "-profile:v", "high"],
    };
    args.iter().map(|a| a.to_string()).collect()
}

/// ffmpeg's arguments for making `output` (an MP4) from `input`, with its audio track `audio`.
pub fn convert_args(probe: &Probe, plan: Plan, encoder: &str, input: &Path, output: &Path, audio: usize) -> Vec<String> {
    let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-y", "-v", "error", "-progress", "pipe:1", "-nostats", "-i"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    args.push(input.to_string_lossy().into_owned());
    args.extend(["-map".into(), "0:v:0".into()]);
    if !probe.audio.is_empty() {
        args.extend(["-map".into(), format!("0:a:{audio}")]);
    }
    args.extend(["-map_metadata".into(), "-1".into(), "-sn".into(), "-dn".into()]);
    match plan {
        Plan::Video => {
            let v = probe.video.clone().unwrap_or_default();
            args.extend(["-vf".into(), video_filter(&v, encoder)]);
            args.extend(encoder_args(encoder));
            args.extend(["-force_key_frames".into(), "expr:gte(t,n_forced*2)".into()]);
        }
        _ => args.extend(["-c:v".into(), "copy".into()]),
    }
    let audio_ok = probe.audio.get(audio).is_some_and(audio_plays_in_mp4);
    if audio_ok {
        args.extend(["-c:a".into(), "copy".into()]);
    } else {
        args.extend(["-c:a", "aac", "-b:a", "192k", "-ac", "2"].iter().map(|a| a.to_string()));
    }
    args.extend(["-movflags".into(), "+faststart".into(), "-f".into(), "mp4".into()]);
    args.push(output.to_string_lossy().into_owned());
    args
}

/// ffmpeg's arguments for making `output` (an MP4) from `video`'s picture as
/// it is (Telly's copy, or the rip when it plays as it is) and the rip's audio
/// track `audio`: changing the audio track without converting again.
pub fn swap_audio_args(probe: &Probe, video: &Path, rip: &Path, audio: usize, output: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-y", "-v", "error", "-progress", "pipe:1", "-nostats", "-i"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    args.push(video.to_string_lossy().into_owned());
    args.extend(["-i".into(), rip.to_string_lossy().into_owned()]);
    args.extend(["-map".into(), "0:v:0".into(), "-map".into(), format!("1:a:{audio}")]);
    // A copy's chapters point at a track that isn't carried over.
    args.extend(["-map_metadata".into(), "-1".into(), "-map_chapters".into(), "-1".into(), "-sn".into(), "-dn".into(), "-c:v".into(), "copy".into()]);
    if probe.audio.get(audio).is_some_and(audio_plays_in_mp4) {
        args.extend(["-c:a".into(), "copy".into()]);
    } else {
        args.extend(["-c:a", "aac", "-b:a", "192k", "-ac", "2"].iter().map(|a| a.to_string()));
    }
    args.extend(["-movflags".into(), "+faststart".into(), "-f".into(), "mp4".into()]);
    args.push(output.to_string_lossy().into_owned());
    args
}

/// Runs ffmpeg with `args`, reporting progress through the file (0 to 1)
/// from its -progress output. Stops it when `cancel` is set.
pub fn run_ffmpeg(
    ffmpeg: &Path,
    args: &[String],
    duration: f64,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<()> {
    let mut child = command(ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting ffmpeg")?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let (tx, rx) = std::sync::mpsc::channel::<f64>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(us) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")) {
                if let Ok(us) = us.trim().parse::<f64>() {
                    let _ = tx.send(us / 1_000_000.0);
                }
            }
        }
    });
    let status = loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(anyhow!("cancelled"));
        }
        while let Ok(t) = rx.try_recv() {
            if duration > 0.0 {
                progress((t / duration).clamp(0.0, 1.0));
            }
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let _ = reader.join();
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        let last = errors.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no message");
        return Err(anyhow!("ffmpeg stopped: {last}"));
    }
    Ok(())
}

/// The seek bar's pictures: one every this many seconds, this wide, tiled
/// this many across in one JPEG.
pub const SHEET_EVERY: f64 = 5.0;
pub const SHEET_COLUMNS: usize = 10;
const SHEET_WIDTH: u32 = 160;

/// How many rows of pictures a video `duration` seconds long has.
pub fn sheet_rows(duration: f64) -> usize {
    let pictures = (duration / SHEET_EVERY).ceil().max(1.0) as usize;
    pictures.div_ceil(SHEET_COLUMNS)
}

/// How ffmpeg decodes a video for the seek bar's pictures, said before the
/// input: keyframes only, each on several cores. Keyframes are seconds apart,
/// so ffmpeg's usual threads (a frame each) decode them one after another:
/// a 24-minute 1080p HEVC episode took 17.7 s that way and 4.6 s this way,
/// for the same pictures.
const SHEET_DECODING: [&str; 4] = ["-thread_type:v", "slice", "-skip_frame:v", "nokey"];

/// ffmpeg's arguments for the seek bar's pictures of the input, `duration`
/// seconds long, as one more thing it writes: a picture is the keyframe
/// nearest its moment.
fn sheet_output(duration: f64, output: &Path) -> Vec<String> {
    // The last picture repeats to the sheet's end: after the last keyframe there would be black.
    let filter = format!("fps=1/{SHEET_EVERY},scale={SHEET_WIDTH}:-2,tpad=stop=-1:stop_mode=clone,tile={SHEET_COLUMNS}x{}", sheet_rows(duration));
    ["-map", "0:v:0", "-an", "-sn", "-vf", &filter, "-frames:v", "1", "-q:v", "5", "-f", "mjpeg", &output.to_string_lossy()].iter().map(|s| s.to_string()).collect()
}

/// ffmpeg's arguments for making the seek bar's pictures of `video`. Only
/// keyframes are decoded, so it takes seconds.
pub fn sheet_args(video: &Path, duration: f64, output: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-v", "error", "-y"].iter().chain(&SHEET_DECODING).map(|s| s.to_string()).collect();
    args.extend(["-i".into(), video.to_string_lossy().into_owned()]);
    args.extend(sheet_output(duration, output));
    args
}

/// Makes the seek bar's pictures of `video` at `output`.
pub fn make_sheet(ffmpeg: &Path, video: &Path, duration: f64, output: &Path) -> Result<()> {
    let partial = output.with_extension("jpg.part");
    let out = command(ffmpeg).args(sheet_args(video, duration, &partial)).stdin(Stdio::null()).output().context("starting ffmpeg")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&partial);
        let text = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("ffmpeg couldn’t make the seek bar’s pictures: {}", text.lines().last().unwrap_or("")));
    }
    std::fs::rename(&partial, output).context("saving the seek bar’s pictures")
}

/// Reads one audio track as mono 16-bit samples at `rate` Hz (for syncing and Whisper).
pub fn read_audio(ffmpeg: &Path, path: &Path, audio: usize, rate: u32, start: Option<f64>, length: Option<f64>) -> Result<Vec<i16>> {
    read_audio_filtered(ffmpeg, path, audio, rate, start, length, None)
}

/// As `read_audio`, through an ffmpeg audio filter first (a voice band-pass, say).
pub fn read_audio_filtered(
    ffmpeg: &Path,
    path: &Path,
    audio: usize,
    rate: u32,
    start: Option<f64>,
    length: Option<f64>,
    filter: Option<&str>,
) -> Result<Vec<i16>> {
    let mut cmd = command(ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-v", "error"]);
    if let Some(start) = start {
        cmd.args(["-ss", &format!("{start:.3}")]);
    }
    cmd.arg("-i").arg(path);
    if let Some(length) = length {
        cmd.args(["-t", &format!("{length:.3}")]);
    }
    if let Some(filter) = filter {
        cmd.args(["-af", filter]);
    }
    cmd.args(["-map", &format!("0:a:{audio}"), "-vn", "-sn", "-ac", "1", "-ar", &rate.to_string(), "-f", "s16le", "-"]);
    let out = cmd.stdin(Stdio::null()).output().context("starting ffmpeg")?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("ffmpeg couldn’t read the audio: {}", text.lines().last().unwrap_or("")));
    }
    Ok(out.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}

/// Writes `length` seconds of one audio track from `start` as WAV, mono at 16 kHz (what speech models hear).
pub fn write_wav(ffmpeg: &Path, path: &Path, audio: usize, start: f64, length: f64, output: &Path) -> Result<()> {
    let out = command(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-v", "error", "-y", "-ss", &format!("{start:.3}"), "-i"])
        .arg(path)
        .args(["-t", &format!("{length:.3}"), "-map", &format!("0:a:{audio}"), "-vn", "-sn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(output)
        .stdin(Stdio::null())
        .output()
        .context("starting ffmpeg")?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("ffmpeg couldn’t read the audio: {}", text.lines().last().unwrap_or("")));
    }
    Ok(())
}

/// A subtitle track inside the rip, as text (for rips that carry Japanese subtitles).
/// Subtitle codecs that are pictures: no text, but each shows at an exact time.
pub fn picture_codec(codec: &str) -> bool {
    matches!(codec, "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub")
}

/// Subtitle codecs that are text.
pub fn text_codec(codec: &str) -> bool {
    matches!(codec, "ass" | "ssa" | "subrip" | "srt" | "webvtt" | "mov_text")
}

/// Whether a subtitle track's title says it's only signs, songs or forced lines.
fn partial_title(title: &str) -> bool {
    let title = title.to_lowercase();
    ["sign", "song", "forced", "karaoke", "lyric"].iter().any(|w| title.contains(w)) && !title.contains("full") && !title.contains("dialog")
}

/// The rip's English subtitles as text, if it has them: the first track that
/// isn't only signs and songs, else the first.
pub fn english_text_track(probe: &Probe) -> Option<&SubtitleInfo> {
    let english = |s: &&SubtitleInfo| {
        text_codec(&s.codec) && (matches!(s.language.to_lowercase().as_str(), "eng" | "en") || s.title.to_lowercase().contains("english"))
    };
    probe.subtitles.iter().filter(english).find(|s| !partial_title(&s.title)).or_else(|| probe.subtitles.iter().find(english))
}

/// One of the rip's own subtitle tracks, by when its subtitles show.
#[derive(Clone, Debug, Default)]
pub struct TrackTiming {
    pub index: i64,
    pub title: String,
    pub language: String,
    pub codec: String,
    /// A text track as a subtitle file (`text_format` says which kind); empty for a picture track.
    pub text: String,
    /// When each subtitle shows, in seconds, in order.
    pub events: Vec<(f64, f64)>,
}

/// The kind of subtitle file ffmpeg makes of a text track: ASS of ASS, else SRT.
pub fn text_format(codec: &str) -> &'static str {
    if matches!(codec, "ass" | "ssa") { "ass" } else { "srt" }
}

/// The packets ffmpeg's framecrc muxer listed for one track, as (when it
/// shows, how long, its size in bytes), in seconds. A line is "stream, dts,
/// pts, duration, size, checksum", in the units "#tb 0:" gives.
fn listed_packets(listing: &str) -> Vec<(f64, f64, f64)> {
    // How many of the units make a second.
    let mut second = 1000.0;
    let mut out = Vec::new();
    for line in listing.lines() {
        if let Some(base) = line.strip_prefix("#tb 0:") {
            second = 1.0 / ratio(base.trim());
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let (Some(pts), Some(duration), Some(size)) = (fields.get(2), fields.get(3), fields.get(4)) else { continue };
        // A packet without a time is listed at the smallest number there is.
        let (Ok(pts), Ok(duration), Ok(size)) = (pts.parse::<i64>(), duration.parse::<i64>(), size.parse::<f64>()) else { continue };
        if pts != i64::MIN {
            out.push((pts as f64 / second, duration as f64 / second, size));
        }
    }
    out
}

/// When a picture track's subtitles show, from its packets: a packet with a
/// picture starts a subtitle, which lasts its duration or until the next
/// packet (the one that clears it).
fn picture_events(mut packets: Vec<(f64, f64, f64)>) -> Vec<(f64, f64)> {
    packets.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut events = Vec::new();
    let mut open: Option<f64> = None;
    for (pts, duration, size) in packets {
        if let Some(start) = open.take() {
            events.push((start, pts.min(start + 15.0)));
        }
        // A clearing packet is a few dozen bytes; a picture, thousands.
        if size >= 100.0 {
            if duration >= 0.05 {
                events.push((pts, pts + duration.min(15.0)));
            } else {
                open = Some(pts);
            }
        }
    }
    events
}

/// ffmpeg's arguments for reading every subtitle track of `path` in one pass
/// over it, each into its own file in `folder`, named by its place in
/// `tracks`: a text track as a subtitle file, a picture track as a list of
/// its packets (nothing decoded). With `sheet` (how long the video is, and
/// where to), the same pass makes the seek bar's pictures. None when there's
/// nothing for it to write.
fn rip_args(path: &Path, tracks: &[TrackTiming], folder: &Path, sheet: Option<(f64, &Path)>) -> Option<Vec<String>> {
    let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-v", "error", "-y"].iter().map(|a| a.to_string()).collect();
    if sheet.is_some() {
        args.extend(SHEET_DECODING.iter().map(|a| a.to_string()));
    }
    args.extend(["-i".into(), path.to_string_lossy().into_owned()]);
    let before = args.len();
    for (i, track) in tracks.iter().enumerate() {
        let format = if picture_codec(&track.codec) {
            "framecrc"
        } else if text_codec(&track.codec) {
            text_format(&track.codec)
        } else {
            continue;
        };
        args.extend(["-map".into(), format!("0:{}", track.index)]);
        if format == "framecrc" {
            args.extend(["-c".into(), "copy".into()]);
        }
        args.extend(["-f".into(), format.into(), folder.join(i.to_string()).to_string_lossy().into_owned()]);
    }
    if let Some((duration, output)) = sheet {
        args.extend(sheet_output(duration, output));
    }
    (args.len() > before).then_some(args)
}

/// Reads a rip from end to end, once, for everything Telly wants of it.
///
/// When the subtitles of each of its subtitle tracks show, whatever their
/// language, pictures included: the timing a release's own subtitles give its
/// dialogue. Every track is read in the one ffmpeg pass: a film on a USB hard
/// drive takes minutes to read through, and a rip can carry 40 tracks. Text
/// tracks are decoded, so their signs can be left out, and their text is
/// kept; picture tracks are timed by their packets.
///
/// With `sheet`, the same pass makes the seek bar's pictures there, as
/// `make_sheet` would of this rip.
pub fn read_rip(ffmpeg: &Path, path: &Path, probe: &Probe, sheet: Option<&Path>) -> Result<Vec<TrackTiming>> {
    static RUNS: AtomicU64 = AtomicU64::new(0);
    let mut out: Vec<TrackTiming> = probe
        .subtitles
        .iter()
        .map(|s| TrackTiming { index: s.index, title: s.title.clone(), language: s.language.clone(), codec: s.codec.clone(), ..Default::default() })
        .collect();
    let folder = std::env::temp_dir().join(format!("telly-tracks-{}-{}", std::process::id(), RUNS.fetch_add(1, Ordering::SeqCst)));
    // Not `make_sheet`'s name for it: the player may ask for the pictures meanwhile.
    let partial = sheet.map(|s| s.with_extension("jpg.reading"));
    let Some(args) = rip_args(path, &out, &folder, partial.as_deref().map(|p| (probe.duration, p))) else { return Ok(out) };
    std::fs::create_dir_all(&folder)?;
    let ran = command(ffmpeg).args(args).stdin(Stdio::null()).output();
    let written: Vec<Option<Vec<u8>>> = (0..out.len()).map(|i| std::fs::read(folder.join(i.to_string())).ok()).collect();
    let _ = std::fs::remove_dir_all(&folder);
    if !ran.context("starting ffmpeg")?.status.success() {
        if let Some(partial) = &partial {
            let _ = std::fs::remove_file(partial);
        }
        return Err(anyhow!("ffmpeg couldn’t read {}", path.display()));
    }
    if let (Some(partial), Some(sheet)) = (&partial, sheet) {
        std::fs::rename(partial, sheet).context("saving the seek bar’s pictures")?;
    }
    for (track, bytes) in out.iter_mut().zip(written) {
        let Some(bytes) = bytes else { continue };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if picture_codec(&track.codec) {
            track.events = picture_events(listed_packets(&text));
            continue;
        }
        let format = text_format(&track.codec);
        if let Ok(lines) = crate::subs::parse(&format!("track.{format}"), text.as_bytes()) {
            track.events = lines.iter().filter(|l| !l.sign).map(|l| (l.start, l.end)).collect();
            track.events.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        track.text = text;
    }
    Ok(out)
}

/// The track whose timing follows the dialogue best: not one only for signs,
/// songs or forced lines, and of those the one with the most subtitles.
pub fn dialogue_track(timings: &[TrackTiming]) -> Option<&TrackTiming> {
    let partial = |t: &TrackTiming| partial_title(&t.title);
    let enough = |t: &&TrackTiming| t.events.len() >= 60;
    timings
        .iter()
        .filter(enough)
        .filter(|t| !partial(t))
        .max_by_key(|t| t.events.len())
        .or_else(|| timings.iter().filter(enough).max_by_key(|t| t.events.len()))
}

pub fn read_subtitle_track(ffmpeg: &Path, path: &Path, stream_index: i64, format: &str) -> Result<String> {
    let out = command(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-v", "error", "-i"])
        .arg(path)
        .args(["-map", &format!("0:{stream_index}"), "-f", format, "-"])
        .stdin(Stdio::null())
        .output()
        .context("starting ffmpeg")?;
    if !out.status.success() {
        return Err(anyhow!("ffmpeg couldn’t read subtitle track {stream_index}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dvd() -> Value {
        json!({
          "format": {"format_name": "matroska,webm", "duration": "1421.5"},
          "streams": [
            {"index": 0, "codec_type": "video", "codec_name": "mpeg2video", "pix_fmt": "yuv420p", "width": 720, "height": 480,
             "sample_aspect_ratio": "8:9", "field_order": "tt", "r_frame_rate": "30000/1001", "avg_frame_rate": "30000/1001"},
            {"index": 1, "codec_type": "audio", "codec_name": "ac3", "channels": 2, "tags": {"language": "eng"}, "disposition": {"default": 1}},
            {"index": 2, "codec_type": "audio", "codec_name": "ac3", "channels": 2, "tags": {"language": "jpn"}, "disposition": {"default": 0}},
            {"index": 3, "codec_type": "subtitle", "codec_name": "dvd_subtitle", "tags": {"language": "eng"}}
          ],
          "chapters": [
            {"start_time": "0.0", "end_time": "90.0", "tags": {"title": "Chapter 01"}},
            {"start_time": "90.0", "end_time": "700.0", "tags": {"title": "Chapter 02"}},
            {"start_time": "700.0", "end_time": "1300.0", "tags": {"title": "Chapter 03"}},
            {"start_time": "1300.0", "end_time": "1390.0", "tags": {"title": "Chapter 04"}},
            {"start_time": "1390.0", "end_time": "1421.5", "tags": {"title": "Chapter 05"}}
          ]
        })
    }

    #[test]
    fn plans_a_dvd_rip() {
        let p = parse_probe(&dvd());
        assert_eq!(p.duration, 1421.5);
        assert_eq!(japanese_audio(&p), 1);
        assert_eq!(plan(&p, Path::new("title_t00.mkv"), 1), Plan::Video);
        assert_eq!(ending_at(&p), Some(1300.0));
        assert_eq!(opening(&p), Some((0.0, 90.0)));
        let v = p.video.as_ref().unwrap();
        let filter = video_filter(v, "h264_amf");
        assert!(filter.starts_with("fieldmatch"), "{filter}");
        assert!(filter.contains("setsar=1") && filter.ends_with("format=nv12"), "{filter}");
        let args = convert_args(&p, Plan::Video, "h264_amf", Path::new("in.mkv"), Path::new("out.mp4"), 1);
        let joined = args.join(" ");
        assert!(joined.contains("-map 0:a:1") && joined.contains("-c:a aac") && joined.contains("h264_amf"), "{joined}");
    }

    #[test]
    fn tiles_a_picture_every_five_seconds() {
        // 24 minutes: 288 pictures, 29 rows of 10.
        assert_eq!(sheet_rows(1437.0), 29);
        assert_eq!(sheet_rows(50.0), 1);
        assert_eq!(sheet_rows(50.1), 2);
        let args = sheet_args(Path::new("ep.mp4"), 1437.0, Path::new("ep.jpg")).join(" ");
        assert!(args.contains("-thread_type:v slice -skip_frame:v nokey -i ep.mp4") && args.contains("fps=1/5,scale=160:-2,tpad=stop=-1:stop_mode=clone,tile=10x29"), "{args}");
    }

    #[test]
    fn plans_other_files() {
        let mut j = dvd();
        j["streams"][0]["codec_name"] = json!("h264");
        j["streams"][0]["profile"] = json!("High");
        j["streams"][2]["codec_name"] = json!("aac");
        // An MKV plays as it is, whichever of its audio tracks is the Japanese one.
        let p = parse_probe(&j);
        assert_eq!(plan(&p, Path::new("a.mkv"), 1), Plan::Direct);
        j["streams"][2]["codec_name"] = json!("flac");
        assert_eq!(plan(&parse_probe(&j), Path::new("a.mkv"), 1), Plan::Direct);
        // So does 10-bit HEVC, and 10-bit H.264.
        j["streams"][0]["codec_name"] = json!("hevc");
        j["streams"][0]["pix_fmt"] = json!("yuv420p10le");
        assert_eq!(plan(&parse_probe(&j), Path::new("a.mkv"), 1), Plan::Direct);
        j["streams"][0]["codec_name"] = json!("h264");
        j["streams"][0]["profile"] = json!("High 10");
        assert_eq!(plan(&parse_probe(&j), Path::new("a.mkv"), 1), Plan::Direct);
        // Audio the player can't play is converted, and the picture copied.
        j["streams"][2]["codec_name"] = json!("dts");
        assert_eq!(plan(&parse_probe(&j), Path::new("a.mkv"), 1), Plan::Audio);
        // 4:4:4 hasn't been seen to play.
        j["streams"][2]["codec_name"] = json!("aac");
        j["streams"][0]["pix_fmt"] = json!("yuv444p10le");
        assert_eq!(plan(&parse_probe(&j), Path::new("a.mkv"), 1), Plan::Video);
        // The right codecs in an AVI are copied into MP4.
        j["streams"][0]["pix_fmt"] = json!("yuv420p");
        j["format"]["format_name"] = json!("avi");
        assert_eq!(plan(&parse_probe(&j), Path::new("a.avi"), 1), Plan::Remux);
        let mp4 = json!({"format": {"format_name": "mov,mp4,m4a,3gp,3g2,mj2", "duration": "1400"},
          "streams": [{"index": 0, "codec_type": "video", "codec_name": "h264", "pix_fmt": "yuv420p", "profile": "High"},
                      {"index": 1, "codec_type": "audio", "codec_name": "aac", "tags": {"language": "jpn"}}]});
        assert_eq!(plan(&parse_probe(&mp4), Path::new("a.mp4"), 0), Plan::Direct);
    }

    #[test]
    fn finds_the_opening_after_a_cold_open() {
        let mut j = dvd();
        j["chapters"] = json!([
            {"start_time": "0.0", "end_time": "130.0", "tags": {"title": "Chapter 01"}},
            {"start_time": "130.0", "end_time": "220.0", "tags": {"title": "Chapter 02"}},
            {"start_time": "220.0", "end_time": "1300.0", "tags": {"title": "Chapter 03"}},
            {"start_time": "1300.0", "end_time": "1421.5", "tags": {"title": "Chapter 04"}}
        ]);
        assert_eq!(opening(&parse_probe(&j)), Some((130.0, 220.0)));
        j["chapters"][0]["tags"]["title"] = json!("Opening");
        assert_eq!(opening(&parse_probe(&j)), Some((0.0, 130.0)));
    }

    #[test]
    fn changes_the_audio_track_without_converting() {
        let p = parse_probe(&dvd());
        let joined = swap_audio_args(&p, Path::new("copy.mp4"), Path::new("in.mkv"), 0, Path::new("out.part")).join(" ");
        assert!(joined.contains("-i copy.mp4 -i in.mkv -map 0:v:0 -map 1:a:0") && joined.contains("-c:v copy -c:a aac"), "{joined}");
    }

    #[test]
    fn reads_every_subtitle_track_in_one_pass_over_the_rip() {
        let track = |index: i64, codec: &str| TrackTiming { index, codec: codec.into(), ..Default::default() };
        let tracks = [track(2, "ass"), track(3, "subrip"), track(4, "hdmv_pgs_subtitle"), track(5, "eia_608")];
        let args = rip_args(Path::new("rip.mkv"), &tracks, Path::new("out"), None).unwrap();
        let file = |n: &str| Path::new("out").join(n).to_string_lossy().into_owned();
        // The rip is opened once. Text comes out as a subtitle file, pictures as a list of their packets.
        let wanted = ["-i", "rip.mkv", "-map", "0:2", "-f", "ass", &file("0"), "-map", "0:3", "-f", "srt", &file("1"), "-map", "0:4", "-c", "copy", "-f", "framecrc", &file("2")];
        assert_eq!(args[args.len() - wanted.len()..], wanted);
        assert_eq!(args.iter().filter(|a| *a == "-i").count(), 1);
        // Captions ffmpeg can't write as text or list are left alone.
        assert_eq!(rip_args(Path::new("rip.mkv"), &tracks[3..], Path::new("out"), None), None);
        // The seek bar's pictures are made in the same pass, as `sheet_args` makes them on their own.
        let with_sheet = rip_args(Path::new("rip.mkv"), &tracks, Path::new("out"), Some((1437.0, Path::new("ep.jpg")))).unwrap().join(" ");
        let alone = sheet_args(Path::new("rip.mkv"), 1437.0, Path::new("ep.jpg")).join(" ");
        let (decoding, pictures) = alone.split_once(" -i rip.mkv ").unwrap();
        assert!(with_sheet.starts_with(&format!("{decoding} -i rip.mkv -map 0:2 ")) && with_sheet.ends_with(&format!("framecrc {} {pictures}", file("2"))), "{with_sheet}");
        // A rip without subtitles is still read for them.
        assert!(rip_args(Path::new("rip.mkv"), &[], Path::new("out"), Some((1437.0, Path::new("ep.jpg")))).is_some());
    }

    #[test]
    fn times_picture_subtitles_by_their_packets() {
        // Blu-ray pictures, as ffmpeg lists them: a picture, then the few bytes that clear it.
        let listing = "#software: Lavf62.12.101
#tb 0: 1/1000
#media_type 0: subtitle
#codec_id 0: hdmv_pgs_subtitle
            0,       9214,       9214,        0,    14880, 0x4aa272a7
            0,      10966,      10966,        0,       35, 0x3dad039d
            0,      17598,      17598,        0,    14850, 0x3142283b
            0,      19934,      19934,        0,       35, 0x3e3f03ad
";
        assert_eq!(picture_events(listed_packets(listing)), [(9.214, 10.966), (17.598, 19.934)]);
        // DVD pictures say how long they show, here in 90 000ths of a second.
        let listing = "#tb 0: 1/90000
0, 180000, 180000, 270000, 4096, 0x00000000
0, 900000, 900000, 135000, 2048, 0x00000000
";
        assert_eq!(picture_events(listed_packets(listing)), [(2.0, 5.0), (10.0, 11.5)]);
    }

    #[test]
    fn picks_the_english_dialogue_track_of_the_rip() {
        let mut j = dvd();
        j["streams"] = json!([
            {"index": 3, "codec_type": "subtitle", "codec_name": "ass", "tags": {"language": "eng", "title": "Signs & Songs"}},
            {"index": 4, "codec_type": "subtitle", "codec_name": "ass", "tags": {"language": "eng", "title": "Full"}},
            {"index": 5, "codec_type": "subtitle", "codec_name": "hdmv_pgs_subtitle", "tags": {"language": "eng"}}
        ]);
        assert_eq!(english_text_track(&parse_probe(&j)).map(|s| s.index), Some(4));
        j["streams"] = json!([{"index": 5, "codec_type": "subtitle", "codec_name": "hdmv_pgs_subtitle", "tags": {"language": "eng"}}]);
        assert_eq!(english_text_track(&parse_probe(&j)), None);
    }
}
