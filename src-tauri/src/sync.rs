//! Timing a subtitle file to your rip, the way alass does it: find where people
//! speak in the episode's audio (WebRTC's voice detector, 10 ms at a time),
//! then shift the lines to fit, allowing a few different shifts for scenes cut
//! differently from the broadcast, and a speed change for PAL discs (25 fps)
//! against subtitles timed to 23.976.
//!
//! The score is how much of the subtitles' time lands on speech after a single
//! best shift, scaled so that chance (lines dropped anywhere) is 0 and every
//! line on speech is 1. A file for another episode can be bent to fit with
//! enough shifts, but not with one, so this catches it.

use crate::subs::Line;
use alass_core::{TimeDelta, TimePoint, TimeSpan};
use serde::Serialize;

/// The voice detector's step: 10 ms at 8 kHz.
pub const RATE: u32 = 8000;
const FRAME: usize = 80;
/// Alignment works in these units (ms) to keep it quick.
const UNIT_MS: f64 = 10.0;
const SPLIT_PENALTY: f64 = 7.0;

/// Spans of speech, in seconds.
pub fn voice_spans(samples: &[i16]) -> Vec<(f64, f64)> {
    let mut vad = webrtc_vad::Vad::new_with_rate(webrtc_vad::SampleRate::Rate8kHz);
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    let frames = samples.chunks_exact(FRAME);
    let count = frames.len();
    for (i, frame) in frames.enumerate() {
        let voice = vad.is_voice_segment(frame).unwrap_or(false);
        match (voice, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                spans.push((s as f64 * 0.01, i as f64 * 0.01));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s as f64 * 0.01, count as f64 * 0.01));
    }
    spans
}

fn to_unit(seconds: f64) -> i64 {
    (seconds * 1000.0 / UNIT_MS).round() as i64
}

fn spans_of(list: &[(f64, f64)]) -> Vec<TimeSpan> {
    list.iter()
        .filter(|(s, e)| e > s)
        .map(|&(s, e)| {
            let (a, b) = (to_unit(s), to_unit(e).max(to_unit(s) + 1));
            TimeSpan::new(TimePoint::from(a), TimePoint::from(b))
        })
        .collect()
}

/// Total overlap between `lines` (each shifted by its delta) and `speech`, as a share of the lines' time.
fn coverage(lines: &[(f64, f64)], speech: &[(f64, f64)]) -> f64 {
    let total: f64 = lines.iter().map(|(s, e)| (e - s).max(0.0)).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let mut sorted: Vec<(f64, f64)> = lines.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut covered = 0.0;
    let mut j = 0;
    for &(s, e) in &sorted {
        while j < speech.len() && speech[j].1 <= s {
            j += 1;
        }
        let mut k = j;
        while k < speech.len() && speech[k].0 < e {
            covered += (e.min(speech[k].1) - s.max(speech[k].0)).max(0.0);
            k += 1;
        }
    }
    covered / total
}

/// The extra shift (within half a second) that puts `lines` in the middle of
/// the shifts that cover the speech best.
fn centre(lines: &[(f64, f64)], speech: &[(f64, f64)]) -> f64 {
    let steps: Vec<f64> = (-50..=50).map(|i| i as f64 * 0.01).collect();
    let scores: Vec<f64> = steps
        .iter()
        .map(|&c| coverage(&lines.iter().map(|&(s, e)| (s + c, e + c)).collect::<Vec<_>>(), speech))
        .collect();
    let best = scores.iter().cloned().fold(0.0, f64::max);
    if best <= 0.0 {
        return 0.0;
    }
    // The unbroken run of near-best shifts around the best one.
    let peak = scores.iter().position(|s| *s >= best).unwrap_or(50);
    let (mut lo, mut hi) = (peak, peak);
    while lo > 0 && scores[lo - 1] >= best - 0.002 {
        lo -= 1;
    }
    while hi + 1 < scores.len() && scores[hi + 1] >= best - 0.002 {
        hi += 1;
    }
    (steps[lo] + steps[hi]) / 2.0
}

/// The share of the episode that's speech: where lines dropped at random would land.
fn speech_share(speech: &[(f64, f64)], length: f64) -> f64 {
    if length <= 0.0 {
        return 0.5;
    }
    (speech.iter().map(|(s, e)| e - s).sum::<f64>() / length).clamp(0.05, 0.95)
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Synced {
    pub lines: Vec<Line>,
    /// 0 to 1 (see the module notes).
    pub score: f64,
    /// The single best shift, in ms (what the player shows as the offset).
    pub offset_ms: i64,
    /// The speed change applied, such as 25/23.976, or 1.
    pub ratio: f64,
    /// How many different shifts the lines got.
    pub shifts: usize,
}

/// Times `lines` to the speech in an episode `length` seconds long.
pub fn sync(lines: &[Line], speech: &[(f64, f64)], length: f64) -> Synced {
    let dialogue: Vec<usize> = (0..lines.len()).filter(|&i| !lines[i].sign).collect();
    let spans: Vec<(f64, f64)> = dialogue.iter().map(|&i| (lines[i].start, lines[i].end)).collect();
    if spans.is_empty() || speech.is_empty() {
        return Synced { lines: lines.to_vec(), score: 0.0, offset_ms: 0, ratio: 1.0, shifts: 0 };
    }
    let reference = spans_of(speech);
    let incoming = spans_of(&spans);

    // A speed change first (alass's framerate guess): keep it only if it fits clearly better.
    let (base_delta, base_score) = alass_core::align_nosplit(&reference, &incoming, alass_core::overlap_scoring, alass_core::NoProgressHandler);
    let (a, b, c) = (25.0, 24.0, 23.976);
    let ratios = [1.0, a / b, a / c, b / a, b / c, c / a, c / b];
    let mut best = (1.0, base_delta, base_score);
    for &ratio in &ratios[1..] {
        let scaled: Vec<TimeSpan> = incoming.iter().map(|t| t.scaled(ratio)).collect();
        let (delta, score) = alass_core::align_nosplit(&reference, &scaled, alass_core::overlap_scoring, alass_core::NoProgressHandler);
        if score > best.2 * 1.05 {
            best = (ratio, delta, score);
        }
    }
    let ratio = best.0;
    let scaled: Vec<TimeSpan> = incoming.iter().map(|t| t.scaled(ratio)).collect();

    // The single best shift, for the score and the offset shown. Lines run
    // longer than the speech in them, so a range of shifts covers it equally;
    // take the middle of that range rather than its earliest end.
    let (one, _) = alass_core::align_nosplit(&reference, &scaled, alass_core::standard_scoring, alass_core::NoProgressHandler);
    let mut one_s = one.as_i64() as f64 * UNIT_MS / 1000.0;
    let shifted = |shift: f64| -> Vec<(f64, f64)> { spans.iter().map(|&(s, e)| (s * ratio + shift, e * ratio + shift)).collect() };
    one_s += centre(&shifted(one_s), speech);
    let chance = speech_share(speech, length);
    let raw = coverage(&shifted(one_s), speech);
    let score = ((raw - chance) / (1.0 - chance)).clamp(0.0, 1.0);

    // Then the full alignment, with splits, centred the same way.
    let (deltas, _) = alass_core::align(&reference, &scaled, SPLIT_PENALTY, Some(1.0), alass_core::standard_scoring, alass_core::NoProgressHandler);
    let mut shifts: Vec<i64> = deltas.iter().map(TimeDelta::as_i64).collect();
    shifts.sort_unstable();
    shifts.dedup();
    let split_spans: Vec<(f64, f64)> = spans
        .iter()
        .zip(&deltas)
        .map(|(&(s, e), d)| {
            let d = d.as_i64() as f64 * UNIT_MS / 1000.0;
            (s * ratio + d, e * ratio + d)
        })
        .collect();
    let nudge = centre(&split_spans, speech);

    // Signs and songs follow the dialogue line nearest them.
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let k = match dialogue.binary_search(&i) {
            Ok(k) => k,
            Err(k) => k.min(dialogue.len() - 1),
        };
        let d = deltas.get(k).map(|d| d.as_i64() as f64 * UNIT_MS / 1000.0 + nudge).unwrap_or(one_s);
        let mut l = line.clone();
        l.start = (line.start * ratio + d).max(0.0);
        l.end = (line.end * ratio + d).max(l.start + 0.05);
        out.push(l);
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    Synced { lines: out, score, offset_ms: (one_s * 1000.0).round() as i64, ratio, shifts: shifts.len() }
}

/// Shifts every line from `from` seconds on by `ms` ("Late from here").
pub fn shift_from(lines: &mut [Line], from: f64, ms: i64) {
    let d = ms as f64 / 1000.0;
    for l in lines.iter_mut().filter(|l| l.start >= from - 0.01) {
        l.start = (l.start + d).max(0.0);
        l.end = (l.end + d).max(l.start + 0.05);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: f64, end: f64) -> Line {
        Line { start, end, text: "せりふ".into(), ruby: vec![], speaker: String::new(), sign: false }
    }

    /// Speech in irregular bursts, like dialogue.
    fn speech() -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut t = 5.0;
        let mut seed: u64 = 7;
        while t < 1300.0 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let len = 0.8 + (seed >> 33) as f64 / (1u64 << 31) as f64 * 3.0;
            let gap = 0.4 + ((seed >> 13) & 0xffff) as f64 / 65536.0 * 4.0;
            out.push((t, t + len));
            t += len + gap;
        }
        out
    }

    #[test]
    fn finds_a_fixed_offset() {
        let sp = speech();
        let lines: Vec<Line> = sp.iter().map(|&(s, e)| line(s - 0.1 + 2.5, e + 0.2 + 2.5)).collect();
        let synced = sync(&lines, &sp, 1320.0);
        assert!((synced.offset_ms + 2500).abs() <= 60, "offset {}", synced.offset_ms);
        assert!(synced.score > 0.6, "score {}", synced.score);
        assert!((synced.lines[10].start - (sp[10].0 - 0.1)).abs() < 0.08);
    }

    #[test]
    fn undoes_a_pal_speed_up() {
        let sp = speech();
        // Subtitles timed to 23.976 fps against a 25 fps PAL disc are too slow by 25/23.976.
        let r = 23.976 / 25.0;
        let lines: Vec<Line> = sp.iter().map(|&(s, e)| line(s / r + 1.0, e / r + 1.0)).collect();
        let synced = sync(&lines, &sp, 1320.0);
        assert!((synced.ratio - r).abs() < 1e-6, "ratio {}", synced.ratio);
        assert!(synced.score > 0.6, "score {}", synced.score);
    }

    #[test]
    fn scores_another_episode_low() {
        let sp = speech();
        let other: Vec<Line> = speech().iter().map(|&(s, e)| line(((s * 7.3) % 1290.0) + 3.0, ((s * 7.3) % 1290.0) + 3.0 + (e - s))).collect();
        let synced = sync(&other, &sp, 1320.0);
        assert!(synced.score < 0.4, "score {}", synced.score);
    }
}
