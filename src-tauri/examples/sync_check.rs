//! Checks the subtitle sync on a real episode: takes a subtitle file timed to
//! the episode (an English track from the rip will do, even just its timing),
//! throws the timing off (a shift, a speed change, a cut), syncs it back with
//! Telly's sync, and says how far each line ends up from where it was. Also
//! syncs another episode's subtitles, to see what a wrong file scores.
//!
//!   cargo run --example sync_check -- <video> <its subtitles> [another episode's subtitles] [--all]
//!
//! --all tries every voice detector setting, with and without a voice band-pass.

use std::path::Path;
use telly_lib::{media, subs, sync, tools};

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    if v.is_empty() { 0.0 } else { v[v.len() / 2] }
}

fn main() {
    let all = std::env::args().any(|a| a == "--all");
    let args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--all").collect();
    let video = Path::new(&args[0]);
    let found = tools::find(Path::new("."), Path::new("."), "", "");
    let (ffmpeg, ffprobe) = (found.ffmpeg.expect("ffmpeg"), found.ffprobe.expect("ffprobe"));
    let probe = media::probe(&ffprobe, video).expect("probe");
    let jp = media::japanese_audio(&probe);
    let original = subs::parse(&args[1], &std::fs::read(&args[1]).expect("subtitles")).expect("parse");
    let dialogue: Vec<subs::Line> = original.into_iter().filter(|l| !l.sign).collect();
    let other = args.get(1 + 1).map(|o| subs::parse(o, &std::fs::read(o).expect("other subtitles")).expect("parse"));

    let filters: Vec<Option<&str>> = if all { vec![None, Some("highpass=f=200,lowpass=f=3400")] } else { vec![Some("highpass=f=200,lowpass=f=3400")] };
    let levels: Vec<u8> = if all { vec![0, 1, 2, 3] } else { vec![2, 3] };
    for filter in filters {
        let samples = media::read_audio_filtered(&ffmpeg, video, jp, sync::RATE, None, None, filter).expect("audio");
        let length = samples.len() as f64 / sync::RATE as f64;
        for &level in &levels {
            let speech = sync::voice_spans_with(&samples, level);
            let share = speech.iter().map(|(s, e)| e - s).sum::<f64>() / length;
            println!("\n== track {jp}, filter {}, detector {level}: {} speech spans, {:.0}% speech, {} lines", filter.unwrap_or("none"), speech.len(), share * 100.0, dialogue.len());
            let truth: Vec<(f64, f64)> = dialogue.iter().map(|l| (l.start, l.end)).collect();
            let z = sync::standing(&truth, &speech, length);
            println!("  the file's own timing: z {:.1} alone {:.1} score {:.2}", z.above, z.alone, sync::score_of(z));
            let cases: [(&str, f64, f64, Option<(f64, f64)>); 4] = [
                ("as timed", 0.0, 1.0, None),
                ("3.2 s late", 3.2, 1.0, None),
                ("PAL speed-up, 1.5 s early", -1.5, 25.0 / 23.976, None),
                ("2 s late, 4 s cut at 10:00", 2.0, 1.0, Some((600.0, 4.0))),
            ];
            for (name, offset, ratio, cut) in cases {
                let broken: Vec<subs::Line> = dialogue
                    .iter()
                    .map(|l| {
                        let mut b = l.clone();
                        let extra = match cut {
                            Some((at, by)) if l.start >= at => by,
                            _ => 0.0,
                        };
                        b.start = l.start * ratio + offset + extra;
                        b.end = l.end * ratio + offset + extra;
                        b
                    })
                    .collect();
                let t = std::time::Instant::now();
                let synced = sync::sync(&broken, &speech, length);
                let errors: Vec<f64> = synced.lines.iter().zip(&dialogue).map(|(s, o)| (s.start - o.start).abs()).collect();
                let within = errors.iter().filter(|e| **e <= 0.3).count() as f64 / errors.len().max(1) as f64;
                let spans: Vec<(f64, f64)> = synced.lines.iter().filter(|l| !l.sign).map(|l| (l.start, l.end)).collect();
                let z = sync::standing(&spans, &speech, length);
                print!("  z {:>5.1} alone {:>4.1} ", z.above, z.alone);
                println!(
                    "  {name:<28} score {:.2}  offset {:+.2} s  ratio {:.4}  shifts {:>2}  median error {:.2} s  within 0.3 s {:>3.0}%  ({:.1} s)",
                    synced.score,
                    synced.offset_ms as f64 / 1000.0,
                    synced.ratio,
                    synced.shifts,
                    median(errors),
                    within * 100.0,
                    t.elapsed().as_secs_f64()
                );
            }
            if let Some(lines) = &other {
                let synced = sync::sync(lines, &speech, length);
                let spans: Vec<(f64, f64)> = synced.lines.iter().filter(|l| !l.sign).map(|l| (l.start, l.end)).collect();
                let z = sync::standing(&spans, &speech, length);
                print!("  z {:>5.1} alone {:>4.1} ", z.above, z.alone);
                println!("  {:<28} score {:.2}  offset {:+.2} s  shifts {}", "another episode's file", synced.score, synced.offset_ms as f64 / 1000.0, synced.shifts);
            }
        }
    }
}
