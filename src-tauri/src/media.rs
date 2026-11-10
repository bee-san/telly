//! What's in a rip (ffprobe), what WebView2 can play, and making a copy it
//! can (ffmpeg).
//!
//! WebView2 plays H.264, VP9 and AV1 with AAC, MP3 or Opus. Rips straight from
//! MakeMKV are usually MPEG-2 with AC-3, and Blu-ray encodes are often 10-bit
//! HEVC, which it can't. So each episode gets one of four plans:
//!
//!   direct  it plays as it is
//!   remux   right codecs, wrong container (MKV): copied into MP4, in seconds
//!   audio   the audio is converted to AAC and the video copied
//!   video   converted to H.264, on the GPU's encoder when there is one, with
//!           the DVD's telecine undone or the picture deinterlaced, square
//!           pixels, and a keyframe every 2 seconds so seeking is instant
//!
//! The Japanese audio track is picked by its language tag and is the only one
//! in Telly's copy, so it's the one that plays.

use crate::util::command;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
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
        "h264" => eight_bit && !v.profile.to_lowercase().contains("10"),
        "vp8" | "vp9" => eight_bit,
        "av1" => eight_bit || v.pix_fmt == "yuv420p10le",
        _ => false,
    }
}

fn audio_plays_in_mp4(a: &AudioInfo) -> bool {
    matches!(a.codec.as_str(), "aac" | "mp3" | "opus")
}

fn container_plays(probe: &Probe, path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    probe.format.contains("mp4") && matches!(ext.as_str(), "mp4" | "m4v" | "mov")
}

/// How to get an episode playing.
pub fn plan(probe: &Probe, path: &Path) -> Plan {
    let Some(video) = &probe.video else { return Plan::Video };
    let jp = japanese_audio(probe);
    let audio_ok = probe.audio.get(jp).is_some_and(audio_plays_in_mp4);
    match (video_plays(video), audio_ok) {
        (true, true) if container_plays(probe, path) && jp == 0 => Plan::Direct,
        (true, true) => Plan::Remux,
        (true, false) => Plan::Audio,
        _ => Plan::Video,
    }
}

/// Where the ending starts: a chapter called that, else the first chapter
/// after 70% of the episode that's an ending's length.
pub fn ending_at(probe: &Probe) -> Option<f64> {
    let d = probe.duration;
    if d <= 0.0 || probe.chapters.len() < 3 {
        return None;
    }
    let named = probe.chapters.iter().find(|c| {
        let t = c.title.to_lowercase();
        t.contains("ending") || t == "ed" || t.starts_with("ed ") || c.title.contains("エンディング")
    });
    if let Some(c) = named {
        return Some(c.start);
    }
    probe
        .chapters
        .iter()
        .find(|c| c.start > 0.7 * d && (55.0..=150.0).contains(&(c.end - c.start)))
        .map(|c| c.start)
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

/// ffmpeg's arguments for making `output` (an MP4) from `input`.
pub fn convert_args(probe: &Probe, plan: Plan, encoder: &str, input: &Path, output: &Path) -> Vec<String> {
    let jp = japanese_audio(probe);
    let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-y", "-v", "error", "-progress", "pipe:1", "-nostats", "-i"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    args.push(input.to_string_lossy().into_owned());
    args.extend(["-map".into(), "0:v:0".into()]);
    if !probe.audio.is_empty() {
        args.extend(["-map".into(), format!("0:a:{jp}")]);
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
    let audio_ok = probe.audio.get(jp).is_some_and(audio_plays_in_mp4);
    if audio_ok {
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

/// A subtitle track inside the rip, as text (for rips that carry Japanese subtitles).
/// Subtitle codecs that are pictures: no text, but each shows at an exact time.
pub fn picture_codec(codec: &str) -> bool {
    matches!(codec, "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub")
}

/// One of the rip's own subtitle tracks, by when its subtitles show.
#[derive(Clone, Debug, Default)]
pub struct TrackTiming {
    pub index: i64,
    pub title: String,
    pub language: String,
    pub codec: String,
    /// When each subtitle shows, in seconds, in order.
    pub events: Vec<(f64, f64)>,
}

/// When the subtitles of each of the rip's subtitle tracks show, whatever
/// their language, pictures included: the timing a release's own subtitles
/// give its dialogue. Picture tracks are read from the container's packets
/// (one ffprobe pass, nothing decoded): a packet with a picture starts a
/// subtitle, which lasts its duration or until the next packet (the one that
/// clears it). Text tracks are decoded, so their signs can be left out.
pub fn subtitle_timings(ffmpeg: &Path, ffprobe: &Path, path: &Path, probe: &Probe) -> Result<Vec<TrackTiming>> {
    let mut out: Vec<TrackTiming> = probe
        .subtitles
        .iter()
        .map(|s| TrackTiming { index: s.index, title: s.title.clone(), language: s.language.clone(), codec: s.codec.clone(), events: Vec::new() })
        .collect();
    if out.iter().any(|t| picture_codec(&t.codec)) {
        let answer = command(ffprobe)
            .args(["-v", "error", "-select_streams", "s", "-show_entries", "packet=stream_index,pts_time,duration_time,size", "-of", "json"])
            .arg(path)
            .output()
            .context("running ffprobe")?;
        let json: Value = serde_json::from_slice(&answer.stdout).context("reading ffprobe’s packets")?;
        let num = |v: &Value| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64());
        let mut packets: Vec<(i64, f64, f64, f64)> = json["packets"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| Some((p["stream_index"].as_i64()?, num(&p["pts_time"])?, num(&p["duration_time"]).unwrap_or(0.0), num(&p["size"]).unwrap_or(0.0))))
                    .collect()
            })
            .unwrap_or_default();
        packets.sort_by(|a, b| a.1.total_cmp(&b.1));
        for track in out.iter_mut().filter(|t| picture_codec(&t.codec)) {
            let mut open: Option<f64> = None;
            for &(_, pts, duration, size) in packets.iter().filter(|p| p.0 == track.index) {
                if let Some(start) = open.take() {
                    track.events.push((start, pts.min(start + 15.0)));
                }
                // A clearing packet is a few dozen bytes; a picture, thousands.
                if size >= 100.0 {
                    if duration >= 0.05 {
                        track.events.push((pts, pts + duration.min(15.0)));
                    } else {
                        open = Some(pts);
                    }
                }
            }
        }
    }
    for track in out.iter_mut().filter(|t| !picture_codec(&t.codec)) {
        let format = if matches!(track.codec.as_str(), "ass" | "ssa") { "ass" } else { "srt" };
        let Ok(text) = read_subtitle_track(ffmpeg, path, track.index, format) else { continue };
        let Ok(lines) = crate::subs::parse(&format!("track.{format}"), text.as_bytes()) else { continue };
        track.events = lines.iter().filter(|l| !l.sign).map(|l| (l.start, l.end)).collect();
    }
    for track in &mut out {
        track.events.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    Ok(out)
}

/// The track whose timing follows the dialogue best: not one only for signs,
/// songs or forced lines, and of those the one with the most subtitles.
pub fn dialogue_track(timings: &[TrackTiming]) -> Option<&TrackTiming> {
    let partial = |t: &TrackTiming| {
        let title = t.title.to_lowercase();
        ["sign", "song", "forced", "karaoke", "lyric"].iter().any(|w| title.contains(w)) && !title.contains("full") && !title.contains("dialog")
    };
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
        assert_eq!(plan(&p, Path::new("title_t00.mkv")), Plan::Video);
        assert_eq!(ending_at(&p), Some(1300.0));
        let v = p.video.as_ref().unwrap();
        let filter = video_filter(v, "h264_amf");
        assert!(filter.starts_with("fieldmatch"), "{filter}");
        assert!(filter.contains("setsar=1") && filter.ends_with("format=nv12"), "{filter}");
        let args = convert_args(&p, Plan::Video, "h264_amf", Path::new("in.mkv"), Path::new("out.mp4"));
        let joined = args.join(" ");
        assert!(joined.contains("-map 0:a:1") && joined.contains("-c:a aac") && joined.contains("h264_amf"), "{joined}");
    }

    #[test]
    fn plans_other_files() {
        let mut j = dvd();
        j["streams"][0]["codec_name"] = json!("h264");
        j["streams"][0]["profile"] = json!("High");
        j["streams"][2]["codec_name"] = json!("aac");
        let p = parse_probe(&j);
        assert_eq!(plan(&p, Path::new("a.mkv")), Plan::Remux);
        j["streams"][2]["codec_name"] = json!("flac");
        assert_eq!(plan(&parse_probe(&j), Path::new("a.mkv")), Plan::Audio);
        j["streams"][0]["codec_name"] = json!("hevc");
        j["streams"][0]["pix_fmt"] = json!("yuv420p10le");
        assert_eq!(plan(&parse_probe(&j), Path::new("a.mkv")), Plan::Video);
        // An MP4 with the Japanese track first plays as it is.
        let mp4 = json!({"format": {"format_name": "mov,mp4,m4a,3gp,3g2,mj2", "duration": "1400"},
          "streams": [{"index": 0, "codec_type": "video", "codec_name": "h264", "pix_fmt": "yuv420p", "profile": "High"},
                      {"index": 1, "codec_type": "audio", "codec_name": "aac", "tags": {"language": "jpn"}}]});
        assert_eq!(plan(&parse_probe(&mp4), Path::new("a.mp4")), Plan::Direct);
    }
}
