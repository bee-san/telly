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
//! 5 to 6 spreads up; another episode's file 2 or so. Where the timing has
//! lines most of the time, in long stretches of talk, that overlap says
//! little, and the lines' starts say more: a file whose lines start where the
//! timing's do far more often than chance, in every quarter of it, fits too
//! (see `STARTS_Z`). Cuts (scenes cut differently) are fixed only when they
//! clearly beat a single shift; when Telly isn't sure, it keeps the single
//! shift and the score says to look.
//!
//! A file timed to a TV broadcast is a minute or more out wherever the ads
//! went, so it doesn't fit with one shift at all. Such a file can be cut into
//! pieces, judged by where its lines start instead (see `sync_in_pieces`).

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

/// FireRedVAD's rate: 16 kHz.
pub const FIRERED_RATE: u32 = 16_000;

/// Spans of speech, in seconds, from FireRedVAD (FireRedTeam's detector, a
/// small DFSMN network): on FLEURS-VAD it finds 96% of speech where WebRTC's
/// finds 36%, with as few false alarms. `samples` are 16 kHz mono.
pub fn firered_spans(samples: &[i16]) -> anyhow::Result<Vec<(f64, f64)>> {
    let fail = |e: firered_vad::Error| anyhow::anyhow!("FireRedVAD: {e}");
    let mut vad = firered_vad::Vad::bundled().map_err(fail)?;
    let mut spans = Vec::new();
    let mut buffer: Vec<f32> = Vec::with_capacity(16_000);
    for chunk in samples.chunks(16_000) {
        buffer.clear();
        buffer.extend(chunk.iter().map(|&s| s as f32 / 32768.0));
        let mut next = vad.push_samples(&buffer).map_err(fail)?;
        while let Some(segment) = next {
            spans.push((segment.start().as_secs_f64(), segment.end().as_secs_f64()));
            next = vad.push_samples(&[]).map_err(fail)?;
        }
    }
    let mut next = vad.finish().map_err(fail)?;
    while let Some(segment) = next {
        spans.push((segment.start().as_secs_f64(), segment.end().as_secs_f64()));
        next = vad.push_samples(&[]).map_err(fail)?;
    }
    Ok(spans)
}

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

/// `spans` in order, with those that overlap joined: when anyone speaks.
/// A rip's own subtitles can show several lines at once (a song's karaoke,
/// eight deep in Bocchi the Rock!'s English track), and counted line by line
/// such a stretch would outweigh the dialogue, so a file fits best piled onto
/// the song. Joined, each second counts once.
fn joined(spans: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut sorted: Vec<(f64, f64)> = spans.iter().copied().filter(|(s, e)| e > s).collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(sorted.len());
    for (s, e) in sorted {
        match out.last_mut() {
            Some(last) if s < last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
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

/// A quick look at how well `lines` could fit `reference` with one shift: the
/// best of every second's shift within five minutes, in spreads above the
/// rest. Under a millisecond a file, so hundreds can be screened for the few
/// worth a full sync (a season whose numbering jumps).
pub fn quick_fit(lines: &[(f64, f64)], reference: &[(f64, f64)]) -> f64 {
    let mut sorted: Vec<(f64, f64)> = lines.iter().copied().filter(|(s, e)| e > s).collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = sorted.iter().map(|(s, e)| e - s).sum();
    let reference = &joined(reference);
    if total <= 0.0 || reference.is_empty() {
        return 0.0;
    }
    let fits: Vec<f64> = (-300..=300).map(|k| covered_at(&sorted, reference, k as f64) / total).collect();
    let n = fits.len() as f64;
    let mean = fits.iter().sum::<f64>() / n;
    let spread = (fits.iter().map(|f| (f - mean).powi(2)).sum::<f64>() / n).sqrt();
    let best = fits.iter().cloned().fold(0.0, f64::max);
    if spread <= 1e-9 { 0.0 } else { (best - mean) / spread }
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

/// A line agrees with the timing when it starts this close to one of its starts.
const AGREE: f64 = 0.3;
/// A broadcast differs from the disc only where it stops for ads or an
/// opening, so its lines pause at least this long (seconds) where it's cut.
const PAUSE: f64 = 20.0;
/// A piece keeps a shift of its own only with at least this many lines
/// standing this far above chance; and all the pieces together must stand
/// `PIECES_Z` above it.
const PIECE_LINES: usize = 10;
const PIECE_Z: f64 = 2.0;
const PIECES_Z: f64 = 6.5;
/// The most pieces a file may be cut into.
const MAX_PIECES: usize = 6;
/// A file synced with one shift fits, whatever its coverage, when its line
/// starts stand this far above chance (and each quarter of them `PIECE_Z`):
/// the bar the pieces of a file must clear together. Coverage says little
/// where the timing has lines most of the time in long stretches of talk,
/// since the lines moved a minute cover nearly as much; line starts still say
/// where they belong. The same goes for a speed change (PAL).
///
/// Measured on Cyberpunk: Edgerunners season 2, against each of the rip's 10
/// subtitle tracks: files that belong stood 29 to 43 spreads above chance by
/// their line starts (23 to 36 against the speech, heard by FireRedVAD); the
/// other 9 episodes' files at most 4.7 (3.3), at the shift their coverage
/// chose, and in none did every quarter stand even 1 spread above chance
/// (0.5). By coverage, files that belong stood only 4.7 to 5.4 in episodes 4
/// and 7, whose subtitles show 57 and 59% of the time.
const STARTS_Z: f64 = PIECES_Z;
/// Into how many parts a file's lines go, each to stand above chance on its own.
const PARTS: usize = 4;

/// How far `starts` (as placed) stand above chance by where they start
/// (`agreement_z`), when each quarter of them does on its own (`PIECE_Z`, as
/// a piece must); else the weakest quarter's. The lines of a TV broadcast's
/// file between two of its ad breaks stand far above chance at their shift,
/// but that one shift doesn't fit the file throughout.
fn starts_standing(mut starts: Vec<f64>, refs: &[f64], length: f64) -> f64 {
    starts.sort_by(|a, b| a.total_cmp(b));
    let whole = agreement_z(&starts, refs, 0.0, length);
    let weakest = starts.chunks(starts.len().div_ceil(PARTS).max(1)).map(|part| agreement_z(part, refs, 0.0, length)).fold(f64::INFINITY, f64::min);
    if weakest >= PIECE_Z { whole } else { weakest.min(whole) }
}

/// How many of `starts`, moved by `shift`, agree with `refs` (sorted).
fn agreeing(starts: &[f64], refs: &[f64], shift: f64) -> usize {
    starts
        .iter()
        .filter(|&&s| {
            let i = refs.partition_point(|&r| r < s + shift - AGREE);
            i < refs.len() && refs[i] <= s + shift + AGREE
        })
        .count()
}

/// The shift within half a second of `shift` at which most of `starts` agree
/// (the middle of the run of such shifts).
fn centre_on_starts(starts: &[f64], refs: &[f64], shift: f64) -> f64 {
    let counts: Vec<usize> = (-50..=50).map(|i| agreeing(starts, refs, shift + i as f64 * 0.01)).collect();
    let best = counts.iter().copied().max().unwrap_or(0);
    let peak = counts.iter().position(|&c| c == best).unwrap_or(50);
    let end = peak + counts[peak..].iter().take_while(|&&c| c == best).count() - 1;
    shift + (peak + end) as f64 * 0.005 - 0.5
}

/// How far the agreement of `starts` at `shift` stands above chance: the
/// most that agree within half a second of it, against the most within half
/// a second of each shift 23 seconds to 3¼ minutes away (round the episode,
/// `length` long), in spreads of those.
fn agreement_z(starts: &[f64], refs: &[f64], shift: f64, length: f64) -> f64 {
    // Shifts in steps of 0.05 s (20 to a second), from 197.5 s before `shift`
    // (step 0) to 197.5 s after (step 2 × MID); `shift` is step MID.
    const STEP: f64 = 0.05;
    const MID: usize = 3950;
    let here = (-10..=10).map(|i| agreeing(starts, refs, shift + i as f64 * STEP)).max().unwrap_or(0) as f64;
    // How many agree at every step, counted in one go: each line agrees over
    // a stretch of steps around each of the timing's starts (and those an
    // episode away, for lines moved round it).
    let from = shift - MID as f64 * STEP;
    let last = 2 * MID;
    let mut round: Vec<f64> = (-2..=2).flat_map(|j| refs.iter().map(move |r| r + j as f64 * length)).collect();
    round.sort_by(|a, b| a.total_cmp(b));
    let mut change = vec![0i32; last + 2];
    for &s in starts {
        let mut run: Option<(usize, usize)> = None;
        let first = round.partition_point(|&r| r < s + from - AGREE);
        for &r in round[first..].iter().take_while(|&&r| r <= s + from + last as f64 * STEP + AGREE) {
            let (a, b) = (((r - s - AGREE - from) / STEP).ceil().max(0.0), ((r - s + AGREE - from) / STEP).floor().min(last as f64));
            if a > b {
                continue;
            }
            let (a, b) = (a as usize, b as usize);
            run = match run {
                Some((ra, rb)) if a <= rb + 1 => Some((ra, rb.max(b))),
                Some((ra, rb)) => {
                    change[ra] += 1;
                    change[rb + 1] -= 1;
                    Some((a, b))
                }
                None => Some((a, b)),
            };
        }
        if let Some((ra, rb)) = run {
            change[ra] += 1;
            change[rb + 1] -= 1;
        }
    }
    let mut count = 0;
    let counts: Vec<i32> = change[..=last]
        .iter()
        .map(|c| {
            count += c;
            count
        })
        .collect();
    let others: Vec<f64> = (23..=197).flat_map(|k| [MID + 20 * k, MID - 20 * k]).map(|m| counts[m - 10..=m + 10].iter().copied().max().unwrap_or(0) as f64).collect();
    let n = others.len() as f64;
    let mean = others.iter().sum::<f64>() / n;
    let spread = (others.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / n).sqrt();
    if spread <= 1e-9 { 0.0 } else { (here - mean) / spread }
}

/// Lines `first..end` of a file, moved by `shift`.
#[derive(Clone, Copy)]
struct Piece {
    first: usize,
    end: usize,
    shift: f64,
    /// How far its line starts stand above chance, once judged.
    z: Option<f64>,
}

/// A file cut into pieces, each with its own shift.
struct Pieces {
    /// The shift of each line of `lines` (in order).
    shifts: Vec<f64>,
    /// How many pieces.
    count: usize,
    /// The shift of the piece with the most lines.
    main: f64,
    /// How far the pieces' line starts stand above chance together
    /// (`above`), and the weakest piece's (`alone`).
    standing: Standing,
}

/// `lines` (sorted by start) cut into pieces where a TV broadcast differs
/// from the disc (ad breaks, a recap, the opening moved), each moved to where
/// its line starts agree best with `reference` (`length` long).
///
/// alass finds the cuts. A cut is kept only where the broadcast's lines
/// pause: elsewhere alass has moved a scene onto a song whose sung lines,
/// thick and fast, agree with anything (Fairy Tail's last scenes went onto
/// the closing song). A piece of fewer than `PIECE_LINES` lines, or one whose
/// line starts don't stand `PIECE_Z` spreads above chance, follows the piece
/// before it (or after it).
///
/// Coverage can't judge pieces: cut up, another episode's file covers the
/// timing as well as one that belongs. Line starts can: a piece that belongs
/// starts its lines where the timing does far more often than the same piece
/// moved 23 seconds to 3¼ minutes away. The pieces are kept only if there are
/// 2 to `MAX_PIECES` of them and together they stand `PIECES_Z` above chance.
///
/// Measured on Fairy Tail 1 to 40: the 23 TV-timed files that don't fit with
/// one shift came apart into 2 or 3 pieces standing 8.1 to 20 above chance
/// together against the rip's English subtitles (5.5 to 12 against its
/// speech, heard by FireRedVAD), their lines where the Judas files have them;
/// 508 other episodes' files (TV and Netflix-timed, 1, 2, 5 and 11 episodes
/// away) stood at most 4.4.
fn in_pieces(lines: &[(f64, f64)], reference: &[(f64, f64)], length: f64) -> Option<Pieces> {
    let mut refs: Vec<f64> = reference.iter().map(|r| r.0).collect();
    refs.sort_by(|a, b| a.total_cmp(b));
    let starts: Vec<f64> = lines.iter().map(|l| l.0).collect();
    let length = length.max(lines.last().map_or(0.0, |l| l.1)).max(1.0);
    let incoming: Vec<TimeSpan> = lines
        .iter()
        .map(|&(s, e)| TimeSpan::new(TimePoint::from(to_unit(s)), TimePoint::from(to_unit(e).max(to_unit(s) + 1))))
        .collect();
    let (deltas, _) = alass_core::align(&spans_of(reference), &incoming, 7.0, Some(3.0), alass_core::standard_scoring, alass_core::NoProgressHandler);
    let mut pieces: Vec<Piece> = Vec::new();
    for (i, d) in deltas.iter().enumerate() {
        match pieces.last_mut() {
            Some(p) if (to_unit(p.shift) - d.as_i64()).abs() <= 1 => p.end = i + 1,
            _ => pieces.push(Piece { first: i, end: i + 1, shift: d.as_i64() as f64 * UNIT_MS / 1000.0, z: None }),
        }
    }
    // Where the broadcast runs straight on, the two pieces are one, at the
    // shift of the longer.
    let mut ended = f64::MIN;
    let pause: Vec<f64> = lines
        .iter()
        .map(|&(s, e)| {
            let gap = s - ended;
            ended = ended.max(e);
            gap
        })
        .collect();
    let mut i = 1;
    while i < pieces.len() {
        let (a, b) = (pieces[i - 1], pieces[i]);
        if pause[b.first] < PAUSE {
            let shift = if b.end - b.first > a.end - a.first { b.shift } else { a.shift };
            pieces[i - 1] = Piece { first: a.first, end: b.end, shift, z: None };
            pieces.remove(i);
        } else {
            i += 1;
        }
    }
    for p in &mut pieces {
        p.shift = centre_on_starts(&starts[p.first..p.end], &refs, p.shift);
    }
    // Pieces too small to judge, or no better than chance, follow a neighbour.
    while pieces.len() > 1 {
        let weak = (0..pieces.len()).find(|&i| {
            let p = &mut pieces[i];
            p.end - p.first < PIECE_LINES || *p.z.get_or_insert_with(|| agreement_z(&starts[p.first..p.end], &refs, p.shift, length)) < PIECE_Z
        });
        let Some(i) = weak else { break };
        let j = if i == 0 { 1 } else { i - 1 };
        let (a, b) = (pieces[i.min(j)], pieces[i.max(j)]);
        let shift = centre_on_starts(&starts[a.first..b.end], &refs, pieces[j].shift);
        pieces[i.min(j)] = Piece { first: a.first, end: b.end, shift, z: None };
        pieces.remove(i.max(j));
    }
    // Each piece's z weighted by the square root of its lines: one z over all of them.
    let judged: Vec<(f64, f64)> = pieces.iter().map(|p| ((p.end - p.first) as f64, p.z.unwrap_or(0.0))).collect();
    let together = judged.iter().map(|&(n, z)| n.sqrt() * z).sum::<f64>() / judged.iter().map(|j| j.0).sum::<f64>().sqrt();
    let weakest = judged.iter().map(|j| j.1).fold(f64::INFINITY, f64::min);
    let kept = (2..=MAX_PIECES).contains(&pieces.len()) && together >= PIECES_Z;
    if std::env::var_os("TELLY_SYNC_DEBUG").is_some() {
        let list: Vec<String> = pieces.iter().map(|p| format!("[{} {:+.2} z {:.1}]", p.end - p.first, p.shift, p.z.unwrap_or(0.0))).collect();
        eprintln!("pieces: {} together {together:.2} weakest {weakest:.2} → {} | {}", pieces.len(), if kept { "kept" } else { "not kept" }, list.join(" "));
    }
    if !kept {
        return None;
    }
    let mut shifts = Vec::with_capacity(lines.len());
    for p in &pieces {
        shifts.extend(std::iter::repeat_n(p.shift, p.end - p.first));
    }
    let main = pieces.iter().max_by_key(|p| p.end - p.first).map_or(0.0, |p| p.shift);
    Some(Pieces { shifts, count: pieces.len(), main, standing: Standing { above: together, alone: weakest, starts: 0.0 } })
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
/// - `starts`: for a file synced with one shift, how far above chance its
///   lines start where the timing's do (see `agreement_z`; 0 when not judged).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Standing {
    pub above: f64,
    pub alone: f64,
    pub starts: f64,
}

pub fn standing(lines: &[(f64, f64)], speech: &[(f64, f64)], length: f64) -> Standing {
    let speech = &joined(speech);
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
    Standing { above: (here - mean) / spread, alone: (here - next_best) / spread, starts: 0.0 }
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
    /// The file fits only cut into pieces (see `sync_in_pieces`), not with one shift.
    #[serde(skip)]
    pub cut: bool,
    /// How far the fit stands above chance (what `score` comes from): by its
    /// coverage, or for a file cut into pieces, by their line starts.
    #[serde(skip)]
    pub standing: Standing,
}

/// How sure a fit is, for the timing it's matched to: how many spreads above
/// chance scores 0 and 1, and how high a file must stand with one shift before
/// Telly looks for cuts within it (cuts let a file that doesn't belong fit in
/// pieces; `sync_in_pieces` judges a file cut into pieces another way). All
/// by coverage: line starts are judged the same whatever the timing (`STARTS_Z`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Calibration {
    pub zero: f64,
    pub full: f64,
    pub cuts_from: f64,
}

impl Calibration {
    /// WebRTC's view of the speech, which hears music too: files that belong
    /// stand 4 to 10 up, another episode's 2 or 3.
    pub const NOISY: Calibration = Calibration { zero: 2.0, full: 6.0, cuts_from: 2.5 };
    /// The rip's own subtitles, or FireRedVAD's view of the speech. Over all
    /// 286 episodes of Fairy Tail, files that belong stood 6.5 to 17 up with
    /// one shift; files that don't, at most 5.4, and only by fitting in pieces.
    pub const CLEAN: Calibration = Calibration { zero: 5.0, full: 7.5, cuts_from: 7.0 };
}

/// Times `lines` to the speech in an episode `length` seconds long (WebRTC's).
pub fn sync(lines: &[Line], speech: &[(f64, f64)], length: f64) -> Synced {
    sync_with(lines, speech, length, &Calibration::NOISY)
}

/// Times `lines` to `speech` (speech, or another subtitle track's timing), scored by `cal`.
pub fn sync_with(lines: &[Line], speech: &[(f64, f64)], length: f64, cal: &Calibration) -> Synced {
    let dialogue: Vec<usize> = (0..lines.len()).filter(|&i| !lines[i].sign).collect();
    let spans: Vec<(f64, f64)> = dialogue.iter().map(|&i| (lines[i].start, lines[i].end)).collect();
    // Where each of the timing's lines (or bursts of speech) starts, before
    // those that overlap are joined: a line starting while another shows
    // starts where the file's may.
    let mut refs: Vec<f64> = speech.iter().filter(|(s, e)| e > s).map(|s| s.0).collect();
    refs.sort_by(|a, b| a.total_cmp(b));
    let speech = &joined(speech);
    if spans.is_empty() || speech.is_empty() {
        return Synced { lines: lines.to_vec(), score: 0.0, offset_ms: 0, ratio: 1.0, shifts: 0, cut: false, standing: Standing::default() };
    }
    let reference = spans_of(speech);
    let incoming = spans_of(&spans);
    let round = length.max(spans.iter().map(|l| l.1).fold(0.0, f64::max)).max(1.0);
    // How far the lines' starts, sped up by `ratio` and moved by `shifts`
    // (one per line), stand above chance throughout.
    let starts_z = |ratio: f64, shifts: &[f64]| -> f64 { starts_standing(spans.iter().zip(shifts).map(|(&(s, _), d)| s * ratio + d).collect(), &refs, round) };

    // A speed change first (alass's framerate guess): each ratio's best single
    // shift, judged by how far it stands above chance. A ratio wins only if it
    // fits clearly better than none.
    let (a, b, c) = (25.0, 24.0, 23.976);
    let ratios = [1.0, a / b, a / c, b / a, b / c, c / a, c / b];
    // Two opinions on the single best shift, alass's and the most speech
    // covered; whichever stands higher wins (each misses now and then when
    // the file has cuts).
    let fit = |ratio: f64| -> (f64, Standing) {
        let scaled: Vec<TimeSpan> = incoming.iter().map(|t| t.scaled(ratio)).collect();
        let (delta, _) = alass_core::align_nosplit(&reference, &scaled, alass_core::standard_scoring, alass_core::NoProgressHandler);
        let mut stretched: Vec<(f64, f64)> = spans.iter().map(|&(s, e)| (s * ratio, e * ratio)).collect();
        stretched.sort_by(|a, b| a.0.total_cmp(&b.0));
        let opinions = [delta.as_i64() as f64 * UNIT_MS / 1000.0, best_shift(&stretched, speech)];
        let (shift, z) = opinions
            .into_iter()
            .map(|shift| {
                let moved: Vec<(f64, f64)> = stretched.iter().map(|&(s, e)| (s + shift, e + shift)).collect();
                (shift, standing(&moved, speech, length))
            })
            .max_by(|a, b| a.1.above.total_cmp(&b.1.above))
            .unwrap_or_default();
        (shift, Standing { starts: starts_z(ratio, &vec![shift; spans.len()]), ..z })
    };
    let fits: Vec<(f64, f64, Standing)> = ratios.iter().map(|&r| {
        let (shift, z) = fit(r);
        (r, shift, z)
    }).collect();
    let (mut ratio, mut one_s, plain) = fits[0];
    // A speed change must fit convincingly on its own, not just beat a poor
    // fit: by the speech it covers, or else by where its lines start.
    let most = |key: fn(&Standing) -> f64| fits[1..].iter().max_by(|a, b| key(&a.2).total_cmp(&key(&b.2))).copied();
    let sped = most(|z| z.above)
        .filter(|(_, _, z)| z.above > plain.above + 2.0 && z.above >= 4.0_f64.max(cal.cuts_from))
        .or_else(|| most(|z| z.starts).filter(|(_, _, z)| z.starts >= STARTS_Z && z.starts > plain.starts + 2.0));
    if let Some((r, shift, _)) = sped {
        (ratio, one_s) = (r, shift);
    }
    // The single best shift, for the score and the offset shown. Lines run
    // longer than the speech in them, so a range of shifts covers it equally;
    // take the middle of that range rather than its earliest end.
    let shifted = |shift: f64| -> Vec<(f64, f64)> { spans.iter().map(|&(s, e)| (s * ratio + shift, e * ratio + shift)).collect() };
    one_s += centre(&shifted(one_s), speech);
    let z_one = Standing { starts: starts_z(ratio, &vec![one_s; spans.len()]), ..standing(&shifted(one_s), speech, length) };

    // Then scenes cut differently: the lines in blocks, each block's own best
    // shift near the single one, changing only where that covers clearly more
    // speech (see `piecewise`). Cuts are kept only for a file that already
    // fits with one shift (by its coverage or its line starts), and only if
    // they fit clearly better: they refine a file that belongs, and must
    // never make one that doesn't look as if it did.
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&a, &b| spans[a].0.total_cmp(&spans[b].0));
    let placed: Vec<(f64, f64)> = order.iter().map(|&i| (spans[i].0 * ratio + one_s, spans[i].1 * ratio + one_s)).collect();
    let extra = piecewise(&placed, speech);
    let mut split = vec![one_s; spans.len()];
    for (rank, &i) in order.iter().enumerate() {
        split[i] = one_s + extra[rank];
    }
    let split_spans = |split: &[f64]| -> Vec<(f64, f64)> { spans.iter().zip(split).map(|(&(s, e), d)| (s * ratio + d, e * ratio + d)).collect() };
    let z_split = Standing { starts: starts_z(ratio, &split), ..standing(&split_spans(&split), speech, length) };
    let jumps = extra.windows(2).filter(|w| (w[1] - w[0]).abs() > 0.05).count();
    let fits = z_one.above >= cal.cuts_from || z_one.starts >= STARTS_Z;
    let use_split = (1..=MAX_CUTS).contains(&jumps) && fits && z_split.above > z_one.above + 0.3;
    if std::env::var_os("TELLY_SYNC_DEBUG").is_some() {
        eprintln!("sync: ratio {ratio:.4} shift {one_s:+.2} one {z_one:?} split {z_split:?} jumps {jumps} → {}", if use_split { "split" } else { "one" });
    }
    let (z, shifts) = if use_split { (z_split, jumps + 1) } else { (z_one, 1) };
    let moves = if use_split { split } else { vec![one_s; spans.len()] };
    Synced { lines: place(lines, &dialogue, ratio, &moves), score: score_with(z, cal), offset_ms: (one_s * 1000.0).round() as i64, ratio, shifts, cut: false, standing: z }
}

/// `lines` sped up by `ratio` and each dialogue line (`dialogue` holds their
/// indices) moved by its own of `moves`. Signs and songs follow the dialogue
/// line nearest them.
fn place(lines: &[Line], dialogue: &[usize], ratio: f64, moves: &[f64]) -> Vec<Line> {
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let k = match dialogue.binary_search(&i) {
            Ok(k) => k,
            Err(k) => k.min(dialogue.len() - 1),
        };
        let mut l = line.clone();
        l.start = (line.start * ratio + moves[k]).max(0.0);
        l.end = (line.end * ratio + moves[k]).max(l.start + 0.05);
        out.push(l);
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// `lines` cut into pieces to fit `speech` (see `in_pieces`), at the speed
/// change (`ratio`) their sync with one shift found: for a file that doesn't
/// fit with one shift, such as one timed to a TV broadcast. None if the pieces
/// don't fit either.
pub fn sync_in_pieces(lines: &[Line], ratio: f64, speech: &[(f64, f64)], length: f64, cal: &Calibration) -> Option<Synced> {
    let dialogue: Vec<usize> = (0..lines.len()).filter(|&i| !lines[i].sign).collect();
    if dialogue.is_empty() || speech.is_empty() {
        return None;
    }
    let mut order: Vec<usize> = (0..dialogue.len()).collect();
    order.sort_by(|&a, &b| lines[dialogue[a]].start.total_cmp(&lines[dialogue[b]].start));
    let scaled: Vec<(f64, f64)> = order.iter().map(|&k| (lines[dialogue[k]].start * ratio, lines[dialogue[k]].end * ratio)).collect();
    let pieces = in_pieces(&scaled, speech, length)?;
    let mut moves = vec![0.0; dialogue.len()];
    for (rank, &k) in order.iter().enumerate() {
        moves[k] = pieces.shifts[rank];
    }
    Some(Synced {
        lines: place(lines, &dialogue, ratio, &moves),
        score: score_with(pieces.standing, cal),
        offset_ms: (pieces.main * 1000.0).round() as i64,
        ratio,
        shifts: pieces.count,
        cut: true,
        standing: pieces.standing,
    })
}

/// The score shown (0 to 1): 2 spreads above the moved fits or less is chance,
/// 6 or more is as good as a file fits. On Fairy Tail's Blu-rays, files timed
/// to the episode scored 0.85 to 1, another episode's 0 to 0.15. A fit that's
/// no peak of its own (`alone` under 0.3) can't count as synced.
pub fn score_of(z: Standing) -> f64 {
    score_with(z, &Calibration::NOISY)
}

pub fn score_with(z: Standing, cal: &Calibration) -> f64 {
    let score = ((z.above - cal.zero) / (cal.full - cal.zero)).clamp(0.0, 1.0);
    let covered = if z.alone < 0.3 { score.min(0.45) } else { score };
    // Line starts far above chance say a file belongs, scored as the pieces
    // of a file are (by `Calibration::CLEAN`), whatever the timing.
    let clean = Calibration::CLEAN;
    let started = if z.starts >= STARTS_Z { ((z.starts - clean.zero) / (clean.full - clean.zero)).clamp(0.0, 1.0) } else { 0.0 };
    covered.max(started)
}

/// The first line at or after `at` seconds: where "Earlier/Later from here" starts.
pub fn first_from(lines: &[Line], at: f64) -> usize {
    lines.iter().position(|l| l.start >= at - 0.01).unwrap_or(lines.len())
}

/// Moves line `first` and every line after it by `ms` ("Earlier/Later from
/// here"). Lines moved earlier stop at the start of the line before them, so
/// the lines stay in order.
pub fn shift_from(lines: &mut [Line], first: usize, ms: i64) {
    let mut d = ms as f64 / 1000.0;
    if let (Some(before), Some(moved)) = (first.checked_sub(1).and_then(|i| lines.get(i)), lines.get(first)) {
        d = d.max(before.start - moved.start);
    }
    for l in lines.iter_mut().skip(first) {
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

    #[test]
    fn lines_moved_from_here_again_and_again_stay_in_order() {
        let mut lines: Vec<Line> = [9.0, 10.4, 10.7, 11.0, 12.0, 14.0].iter().map(|&s| line(s, s + 0.2)).collect();
        let first = first_from(&lines, 10.4);
        assert_eq!(first, 1);
        // Earlier four times at the same spot: the same lines move, until they reach the line before.
        for _ in 0..4 {
            shift_from(&mut lines, first, -500);
        }
        let starts: Vec<f64> = lines.iter().map(|l| (l.start * 100.0).round() / 100.0).collect();
        assert_eq!(starts, vec![9.0, 9.0, 9.3, 9.6, 10.6, 12.6]);
        assert!(lines.windows(2).all(|w| w[0].start <= w[1].start));
        // Later moves them back.
        shift_from(&mut lines, first, 1000);
        assert_eq!((lines[1].start * 100.0).round() / 100.0, 10.0);
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
    fn a_song_s_stacked_lines_count_once() {
        // The rip's own subtitles as the timing: its dialogue, then an ending
        // whose karaoke shows eight lines at once (Bocchi the Rock!'s English track).
        let dialogue = speech();
        let mut timing = dialogue.clone();
        timing.extend(std::iter::repeat_n((1310.0, 1400.0), 8));
        // A file timed to the episode stays where it is, not piled onto the song.
        let lines: Vec<Line> = dialogue.iter().map(|&(s, e)| line(s - 0.1, e + 0.2)).collect();
        let synced = sync_with(&lines, &timing, 1420.0, &Calibration::CLEAN);
        assert!(synced.offset_ms.abs() <= 300, "offset {}", synced.offset_ms);
        assert!(synced.score >= 0.75, "score {}", synced.score);
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

    /// Dialogue in long stretches of talk (150 to 400 s of lines one after
    /// another) between long quiet ones (60 to 180 s), as in an episode
    /// with long fights.
    fn scenes(mut seed: u64, step: u64) -> Vec<(f64, f64)> {
        let mut unit = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(step);
            (seed >> 33) as f64 / (1u64 << 31) as f64
        };
        let mut out = Vec::new();
        let mut t = 5.0;
        while t < 1320.0 {
            let end = (t + 150.0 + unit() * 250.0).min(1320.0);
            while t < end {
                let len = 0.8 + unit() * 2.7;
                out.push((t, t + len));
                t += len + 0.05 + unit() * 0.45;
            }
            t += 60.0 + unit() * 120.0;
        }
        out
    }

    #[test]
    fn a_file_fits_by_its_line_starts_where_the_timing_has_lines_most_of_the_time() {
        // The rip's own subtitles come in long stretches of talk (as in
        // Cyberpunk: Edgerunners' 4th and 7th episodes, on 57 to 59% of the
        // time). Moved a minute or two, a file still covers nearly as much
        // of them as where it belongs, so by coverage it stands too little
        // above chance to count. Its lines still start where the timing's do.
        let timing = scenes(7, 1442695040888963407);
        let lines: Vec<Line> = timing.iter().map(|&(s, e)| line(s - 0.1 + 2.5, e + 0.2 + 2.5)).collect();
        let synced = sync_with(&lines, &timing, 1320.0, &Calibration::CLEAN);
        assert!(synced.standing.above < 5.0, "coverage alone would do: {:?}", synced.standing);
        // Within the 0.3 s the lines run longer than the timing's.
        assert!((synced.offset_ms + 2550).abs() <= 200, "offset {}", synced.offset_ms);
        assert!(synced.score >= 0.75, "score {} {:?}", synced.score, synced.standing);
        // Sped up for PAL, it's slowed down again: the right speed is told by its line starts too.
        let r = 23.976 / 25.0;
        let pal: Vec<Line> = timing.iter().map(|&(s, e)| line((s - 0.1) / r + 1.0, (e + 0.2) / r + 1.0)).collect();
        let synced = sync_with(&pal, &timing, 1320.0, &Calibration::CLEAN);
        assert!((synced.ratio - r).abs() < 1e-6 && synced.score >= 0.75, "ratio {} score {}", synced.ratio, synced.score);
        // Another episode's lines, in stretches of their own, don't fit.
        let other: Vec<Line> = scenes(12345, 0x9E37_79B9_7F4A_7C15).iter().map(|&(s, e)| line(s - 0.1, e + 0.2)).collect();
        let synced = sync_with(&other, &timing, 1320.0, &Calibration::CLEAN);
        assert!(synced.score < 0.15, "score {} {:?}", synced.score, synced.standing);
    }

    #[test]
    fn scores_another_episode_low() {
        let sp = speech();
        // Another episode: dialogue with its own rhythm.
        let other: Vec<Line> = speech_from(12345, 0x9E37_79B9_7F4A_7C15).iter().map(|&(s, e)| line(s - 0.1, e + 0.2)).collect();
        let synced = sync(&other, &sp, 1320.0);
        assert!(synced.score < 0.4, "score {}", synced.score);
    }

    /// Lines timed like a TV broadcast's captions, each running on to the
    /// next (3 seconds at most), `late(t)` seconds late at time `t`.
    fn broadcast(sp: &[(f64, f64)], late: impl Fn(f64) -> f64) -> Vec<Line> {
        sp.iter()
            .enumerate()
            .map(|(i, &(s, e))| {
                let end = sp.get(i + 1).map_or(e, |next| next.0).min(e + 3.0);
                line(s - 0.1 + late(s), end - 0.1 + late(s))
            })
            .collect()
    }

    #[test]
    fn a_broadcast_with_ad_breaks_fits_in_pieces() {
        let sp = speech();
        // 30 seconds before the episode, then ad breaks of 90 and 75 seconds.
        let ads = |t: f64| if t < 430.0 { 30.0 } else if t < 870.0 { 120.0 } else { 195.0 };
        let lines = broadcast(&sp, ads);
        let one = sync_with(&lines, &sp, 1320.0, &Calibration::CLEAN);
        assert!(one.score < 0.5, "fits with one shift: {}", one.score);
        let synced = sync_in_pieces(&lines, one.ratio, &sp, 1320.0, &Calibration::CLEAN).expect("fits in pieces");
        assert!(synced.cut && synced.shifts == 3, "cut {} shifts {}", synced.cut, synced.shifts);
        assert!(synced.score >= 0.75, "score {}", synced.score);
        for (l, &(s, _)) in synced.lines.iter().zip(&sp) {
            assert!((l.start - (s - 0.1)).abs() < 0.3, "{:.2} should be at {:.2}", l.start, s - 0.1);
        }
        // Another episode's lines, cut the same way, don't fit.
        let other = broadcast(&speech_from(12345, 0x9E37_79B9_7F4A_7C15), ads);
        assert!(sync_in_pieces(&other, 1.0, &sp, 1320.0, &Calibration::CLEAN).is_none());
    }
}
