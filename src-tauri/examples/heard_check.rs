//! Checks timing a subtitle file by its words (heard.rs) on a real episode,
//! read-only: whisper.cpp on this computer listens to the stretches, the file
//! (in sync with the rip already) is thrown out of time by known amounts, and
//! each is found again.
//!
//!   cargo run --example heard_check -- <rip> <subtitle file> <whisper-cli> <model> <scratch folder>

use std::path::Path;
use std::process::Command;
use telly_lib::{heard, media, subs, tools};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (rip, file, whisper, model, scratch) = (Path::new(&args[1]), Path::new(&args[2]), Path::new(&args[3]), Path::new(&args[4]), Path::new(&args[5]));
    let found = tools::find(Path::new("."), Path::new("."), "", "");
    let (ffmpeg, ffprobe) = (found.ffmpeg.expect("ffmpeg"), found.ffprobe.expect("ffprobe"));
    let probe = media::probe(&ffprobe, rip)?;
    let track = media::japanese_audio(&probe);
    std::fs::create_dir_all(scratch)?;
    let mut listened = Vec::new();
    for (i, (start, length)) in heard::stretches(probe.duration).into_iter().enumerate() {
        let wav = scratch.join(format!("stretch{i}.wav"));
        let out = scratch.join(format!("stretch{i}"));
        let status = Command::new(&ffmpeg)
            .args(["-nostdin", "-hide_banner", "-v", "error", "-y", "-ss", &format!("{start:.3}"), "-i"])
            .arg(rip)
            .args(["-t", &format!("{length:.3}"), "-map", &format!("0:a:{track}"), "-vn", "-sn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
            .arg(&wav)
            .status()?;
        anyhow::ensure!(status.success(), "ffmpeg couldn't read the audio");
        let status = Command::new(whisper)
            .arg("-m")
            .arg(model)
            .args(["-l", "ja", "-mc", "0", "-ml", "60", "-osrt", "-np", "-of"])
            .arg(&out)
            .arg("-f")
            .arg(&wav)
            .current_dir(whisper.parent().unwrap())
            .status()?;
        anyhow::ensure!(status.success(), "whisper.cpp stopped");
        let mut lines = subs::parse("whisper.srt", &std::fs::read(out.with_extension("srt"))?)?;
        for line in &mut lines {
            line.start += start;
            line.end += start;
        }
        println!("stretch {i} from {start:.0} s: {} lines heard", lines.len());
        listened.push(lines);
    }
    let name = file.file_name().unwrap().to_string_lossy();
    let lines = subs::parse(&name, &std::fs::read(file)?)?;
    for off in [0.0, 7.3, -42.0, 1.1] {
        let mut moved = lines.clone();
        for line in &mut moved {
            line.start -= off;
            line.end -= off;
        }
        match heard::place(&moved, &listened) {
            Some(p) => println!("out by {off:+.2} s: found {:+.2} s (wrong by {:+.2} s), {} of {} heard lines agree", p.shift, p.shift - off, p.agreeing, p.heard),
            None => println!("out by {off:+.2} s: not placed"),
        }
    }
    Ok(())
}
