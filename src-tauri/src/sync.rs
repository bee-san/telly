//! Timing a subtitle file to your rip, the way alass does it: find where people
//! speak in the episode's audio (WebRTC's voice detector, 10 ms at a time,
//! through a voice band-pass), then shift the lines to fit, with a speed change
//! for PAL discs (25 fps) against subtitles timed to 23.976, and different
//! shifts for scenes cut differently from the broadcast.
//!
//! Measured with Fairy Tail's Blu-rays (examples/sync_check.rs): files thrown
//! 3.2 s late, sped up for PAL, or cut, come back with every line within 0.1 s.
//!
//! The score says how far above chance the lines sit on the speech: their
//! overlap with it against the same lines moved 20 seconds to 5 minutes away,
//! in spreads (a z-score; see `standing`). A file timed to the episode stands
//! 5 to 6 spreads up; another episode's file 2 or so. Cuts (scenes cut
//! differently) are fixed only when they clearly beat a single shift; when
//! Telly isn't sure, it keeps the single shift and the score says to look.

use crate::subs::Line;
use alass_core::{TimePoint, TimeSpan};
use serde::Serialize;

/// The voice detector's step: 10 ms at 8 kHz.
pub const RATE: u32 = 8000;
const FRAME: usize = 80;
/// Alignment works in these units (ms) to keep it quick.
const UNIT_MS: f64 = 10.0;
/// The most cuts a sync may find before Telly stops believing them.
const MAX_CUTS: usize = 4;
/// The voice detector hears music as speech; a voice band-pass and its
/// strictest setting keep it to people talking (measured on Fairy Tail's
/// Blu-rays: every line back within 0.1 s, PAL speed-ups and cuts included).
pub const VOICE_FILTER: &str = "highpass=f=200,lowpass=f=3400";

/// Spans of speech, in seconds.
pub fn voice_spans(samples: &[i16]) -> Vec<(f64, f64)> {
    voice_spans_with(samples, 3)
}

/// Spans of speech with the detector at `aggressiveness` 0 (hears most) to 3 (hears least).
pub fn voice_spans_with(samples: &[i16], aggressiveness: u8) -> Vec<(f64, f64)> {
    let mode = match aggressiveness {
        0 => webrtc_vad::VadMode::Quality,
        1 => webrtc_vad::VadMode::LowBitrate,
        2 => webrtc_vad::VadMode::Aggressive,
        _ => webrtc_vad::VadMode::VeryAggressive,
    };
    let mut vad = webrtc_vad::Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate8kHz, mode);
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

/// Total overlap between `lines` and `speech`, as a share of the lines' time.
pub fn coverage(lines: &[(f64, f64)], speech: &[(f64, f64)]) -> f64 {
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

/// Seconds of speech under `lines` (sorted by start) moved by `shift`.
fn covered_at(lines: &[(f64, f64)], speech: &[(f64, f64)], shift: f64) -> f64 {
    let mut covered = 0.0;
    let mut j = 0;
    for &(s0, e0) in lines {
        let (s, e) = (s0 + shift, e0 + shift);
        while j < speech.len() && speech[j].1 <= s {
            j += 1;
        }
        let mut k = j;
        while k < speech.len() && speech[k].0 < e {
            covered += (e.min(speech[k].1) - s.max(speech[k].0)).max(0.0);
            k += 1;
        }
    }
    covered
}

/// The single shift (within 5 minutes) that puts the most of `lines` (sorted)
/// on speech: every quarter second, then every hundredth around the best.
fn best_shift(lines: &[(f64, f64)], speech: &[(f64, f64)]) -> f64 {
    let mut best = (0.0, -1.0);
    for k in -1200..=1200 {
        let shift = k as f64 * 0.25;
        let c = covered_at(lines, speech, shift);
        if c > best.1 {
            best = (shift, c);
        }
    }
    let coarse = best.0;
    for k in -30..=30 {
        let shift = coarse + k as f64 * 0.01;
        let c = covered_at(lines, speech, shift);
        if c > best.1 {
            best = (shift, c);
        }
    }
    best.0
}

/// Extra shifts (one per line of `lines`, sorted and already placed by the
/// single best shift) for scenes cut differently from the broadcast. The lines
/// go in blocks of 16; each block may move up to 12 seconds, in tenths, and the
/// moves are smoothed (Viterbi) so the shift only changes where the new one
/// covers at least `JUMP` seconds more speech. Where it changes, the cut is put
/// at the line that fits both sides best.
fn piecewise(lines: &[(f64, f64)], speech: &[(f64, f64)]) -> Vec<f64> {
    const BLOCK: usize = 16;
    const REACH: i32 = 120;
    const JUMP: f64 = 8.0;
    let blocks: Vec<&[(f64, f64)]> = lines.chunks(BLOCK).collect();
    if blocks.len() < 2 {
        return vec![0.0; lines.len()];
    }
    let states = (2 * REACH + 1) as usize;
    let shift_of = |s: usize| (s as i32 - REACH) as f64 * 0.1;
    let gains: Vec<Vec<f64>> = blocks.iter().map(|b| (0..states).map(|s| covered_at(b, speech, shift_of(s))).collect()).collect();
    // Starting anywhere but the single shift costs a jump too.
    let mut best: Vec<f64> = (0..states).map(|s| gains[0][s] - if s == REACH as usize { 0.0 } else { JUMP }).collect();
    let mut back: Vec<Vec<usize>> = Vec::with_capacity(blocks.len());
    for gain in gains.iter().skip(1) {
        let (top_state, top) = best.iter().enumerate().fold((0, f64::MIN), |acc, (s, &v)| if v > acc.1 { (s, v) } else { acc });
        let mut next = vec![0.0; states];
        let mut from = vec![0; states];
        for s in 0..states {
            let (prev, value) = if best[s] >= top - JUMP { (s, best[s]) } else { (top_state, top - JUMP) };
            next[s] = value + gain[s];
            from[s] = prev;
        }
        back.push(from);
        best = next;
    }
    let mut state = best.iter().enumerate().fold((0, f64::MIN), |acc, (s, &v)| if v > acc.1 { (s, v) } else { acc }).0;
    let mut path = vec![state; blocks.len()];
    for b in (1..blocks.len()).rev() {
        state = back[b - 1][state];
        path[b - 1] = state;
    }
    let mut extra: Vec<f64> = Vec::with_capacity(lines.len());
    for (b, block) in blocks.iter().enumerate() {
        extra.extend(std::iter::repeat_n(shift_of(path[b]), block.len()));
    }
    // Put each cut at the best line within the two blocks around it.
    for b in 1..blocks.len() {
        let (p, q) = (shift_of(path[b - 1]), shift_of(path[b]));
        if (p - q).abs() < 0.05 {
            continue;
        }
        let lo = (b - 1) * BLOCK;
        let hi = ((b + 1) * BLOCK).min(lines.len());
        let mut best_cut = (b * BLOCK, f64::MIN);
        for cut in lo..=hi {
            let fit: f64 = (lo..hi).map(|i| covered_at(&lines[i..=i], speech, if i < cut { p } else { q })).sum();
            if fit > best_cut.1 {
                best_cut = (cut, fit);
            }
        }
        for (i, e) in extra.iter_mut().enumerate().take(hi).skip(lo) {
            *e = if i < best_cut.0 { p } else { q };
        }
    }
    extra
}

/// Coverage of `lines` (sorted, `total` long) moved by `shift`, wrapping round
/// an episode `length` long, so every moved line still lands somewhere.
fn wrapped_coverage(lines: &[(f64, f64)], total: f64, speech: &[(f64, f64)], shift: f64, length: f64) -> f64 {
    let covered = if shift >= 0.0 {
        let cut = lines.partition_point(|l| l.0 + shift < length);
        covered_at(&lines[..cut], speech, shift) + covered_at(&lines[cut..], speech, shift - length)
    } else {
        let cut = lines.partition_point(|l| l.0 + shift < 0.0);
        covered_at(&lines[..cut], speech, shift + length) + covered_at(&lines[cut..], speech, shift)
    };
    covered / total
}

/// How the lines' fit compares with the same lines moved 20 seconds to 5
/// minutes either way (round the episode), in spreads of those moved fits'
/// coverage:
///
/// - `above`: how far above the moved fits' average. A file timed to the
///   episode stands 7 to 10 up on real soundtracks; another episode's 2 or 3.
/// - `alone`: how far above the best moved fit. A file that belongs makes one
///   peak; one that doesn't is only the highest of many bumps, so it stands
///   barely above the next (this catches what `above` can miss on very regular
///   dialogue).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Standing {
    pub above: f64,
    pub alone: f64,
}

pub fn standing(lines: &[(f64, f64)], speech: &[(f64, f64)], length: f64) -> Standing {
    let mut sorted: Vec<(f64, f64)> = lines.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = sorted.iter().map(|(s, e)| (e - s).max(0.0)).sum();
    let end = sorted.iter().map(|l| l.1).fold(0.0, f64::max);
    let length = length.max(end).max(speech.last().map(|s| s.1).unwrap_or(0.0));
    if total <= 0.0 || length <= 0.0 {
        return Standing::default();
    }
    let here = covered_at(&sorted, speech, 0.0) / total;
    let mut others = Vec::with_capacity(1200);
    for k in 40..=600 {
        let shift = k as f64 * 0.5;
        others.push(wrapped_coverage(&sorted, total, speech, shift, length));
        others.push(wrapped_coverage(&sorted, total, speech, -shift, length));
    }
    let n = others.len() as f64;
    let mean = others.iter().sum::<f64>() / n;
    let spread = (others.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / n).sqrt();
    let next_best = others.iter().cloned().fold(0.0, f64::max);
    if spread <= 1e-9 {
        return Standing::default();
    }
    Standing { above: (here - mean) / spread, alone: (here - next_best) / spread }
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

    // A speed change first (alass's framerate guess): each ratio's best single
    // shift, judged by how far it stands above chance. A ratio wins only if it
    // fits clearly better than none.
    let (a, b, c) = (25.0, 24.0, 23.976);
    let ratios = [1.0, a / b, a / c, b / a, b / c, c / a, c / b];
    // Two opinions on the single best shift, alass's and the most speech
    // covered; whichever stands higher wins (each misses now and then when
    // the file has cuts).
    let fit = |ratio: f64| -> (f64, f64) {
        let scaled: Vec<TimeSpan> = incoming.iter().map(|t| t.scaled(ratio)).collect();
        let (delta, _) = alass_core::align_nosplit(&reference, &scaled, alass_core::standard_scoring, alass_core::NoProgressHandler);
        let mut stretched: Vec<(f64, f64)> = spans.iter().map(|&(s, e)| (s * ratio, e * ratio)).collect();
        stretched.sort_by(|a, b| a.0.total_cmp(&b.0));
        let opinions = [delta.as_i64() as f64 * UNIT_MS / 1000.0, best_shift(&stretched, speech)];
        opinions
            .into_iter()
            .map(|shift| {
                let moved: Vec<(f64, f64)> = stretched.iter().map(|&(s, e)| (s + shift, e + shift)).collect();
                (shift, standing(&moved, speech, length).above)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap_or((0.0, 0.0))
    };
    let fits: Vec<(f64, f64, f64)> = ratios.iter().map(|&r| {
        let (shift, z) = fit(r);
        (r, shift, z)
    }).collect();
    let (mut ratio, mut one_s, plain) = fits[0];
    if let Some(&(r, shift, z)) = fits[1..].iter().max_by(|a, b| a.2.total_cmp(&b.2)) {
        // A speed change must fit convincingly on its own, not just beat a poor fit.
        if z > plain + 2.0 && z >= 4.0 {
            (ratio, one_s) = (r, shift);
        }
    }
    // The single best shift, for the score and the offset shown. Lines run
    // longer than the speech in them, so a range of shifts covers it equally;
    // take the middle of that range rather than its earliest end.
    let shifted = |shift: f64| -> Vec<(f64, f64)> { spans.iter().map(|&(s, e)| (s * ratio + shift, e * ratio + shift)).collect() };
    one_s += centre(&shifted(one_s), speech);
    let z_one = standing(&shifted(one_s), speech, length);

    // Then scenes cut differently: the lines in blocks, each block's own best
    // shift near the single one, changing only where that covers clearly more
    // speech (see `piecewise`). Cuts are kept only for a file that already
    // fits with one shift, and only if they fit clearly better: they refine a
    // file that belongs, and must never make one that doesn't look as if it did.
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&a, &b| spans[a].0.total_cmp(&spans[b].0));
    let placed: Vec<(f64, f64)> = order.iter().map(|&i| (spans[i].0 * ratio + one_s, spans[i].1 * ratio + one_s)).collect();
    let extra = piecewise(&placed, speech);
    let mut split = vec![one_s; spans.len()];
    for (rank, &i) in order.iter().enumerate() {
        split[i] = one_s + extra[rank];
    }
    let split_spans = |split: &[f64]| -> Vec<(f64, f64)> { spans.iter().zip(split).map(|(&(s, e), d)| (s * ratio + d, e * ratio + d)).collect() };
    let z_split = standing(&split_spans(&split), speech, length);
    let jumps = extra.windows(2).filter(|w| (w[1] - w[0]).abs() > 0.05).count();
    let use_split = (1..=MAX_CUTS).contains(&jumps) && z_one.above >= 2.5 && z_split.above > z_one.above + 0.3;
    if std::env::var_os("TELLY_SYNC_DEBUG").is_some() {
        eprintln!("sync: ratio {ratio:.4} shift {one_s:+.2} one {z_one:?} split {z_split:?} jumps {jumps} → {}", if use_split { "split" } else { "one" });
    }
    let (z, shifts) = if use_split { (z_split, jumps + 1) } else { (z_one, 1) };

    // Signs and songs follow the dialogue line nearest them.
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let k = match dialogue.binary_search(&i) {
            Ok(k) => k,
            Err(k) => k.min(dialogue.len() - 1),
        };
        let d = if use_split { split.get(k).copied().unwrap_or(one_s) } else { one_s };
        let mut l = line.clone();
        l.start = (line.start * ratio + d).max(0.0);
        l.end = (line.end * ratio + d).max(l.start + 0.05);
        out.push(l);
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    Synced { lines: out, score: score_of(z), offset_ms: (one_s * 1000.0).round() as i64, ratio, shifts }
}

/// The score shown (0 to 1): 2 spreads above the moved fits or less is chance,
/// 6 or more is as good as a file fits. On Fairy Tail's Blu-rays, files timed
/// to the episode scored 0.85 to 1, another episode's 0 to 0.15. A fit that's
/// no peak of its own (`alone` under 0.3) can't count as synced.
pub fn score_of(z: Standing) -> f64 {
    let score = ((z.above - 2.0) / 4.0).clamp(0.0, 1.0);
    if z.alone < 0.3 { score.min(0.45) } else { score }
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
        speech_from(7, 1442695040888963407)
    }

    /// Bursts from a random generator; another `step` gives an unrelated
    /// rhythm (another seed alone would give the same one, shifted).
    fn speech_from(mut seed: u64, step: u64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut t = 5.0;
        while t < 1300.0 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(step);
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
        // Another episode: dialogue with its own rhythm.
        let other: Vec<Line> = speech_from(12345, 0x9E37_79B9_7F4A_7C15).iter().map(|&(s, e)| line(s - 0.1, e + 0.2)).collect();
        let synced = sync(&other, &sp, 1320.0);
        assert!(synced.score < 0.4, "score {}", synced.score);
    }
}
