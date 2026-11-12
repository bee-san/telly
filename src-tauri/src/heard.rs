//! Times a subtitle file by its words, for when its timing alone doesn't say
//! where it belongs: a speech model listens to a few stretches of the episode,
//! and each line it hears that reads like one of the file's lines says how far
//! the file is from the episode. Lines that only look alike disagree with one
//! another; the right ones agree, from every stretch.
//!
//! The limits are those a campaign over this library settled on (a Kaguya set
//! timed by hand: no wrong shift passed them): at least 12 lines agreeing,
//! from at least 3 stretches.

use crate::subs::Line;

/// How many stretches of an episode are listened to, and how long each is, in seconds.
const STRETCHES: usize = 4;
const STRETCH: f64 = 90.0;
/// A heard line and a file's line are the same words when this much of their letter pairs match.
const SAME: f64 = 0.5;
/// Lines shorter than this many letters are said too often to place anything.
const LETTERS: usize = 4;
/// Shifts within this many seconds of one another agree.
const AGREE: f64 = 1.0;
/// The fewest lines that must agree, and the fewest stretches they come from.
const LINES: usize = 12;
const FROM: usize = 3;

/// Where to listen in an episode `length` seconds long: (start, length) of each stretch.
pub fn stretches(length: f64) -> Vec<(f64, f64)> {
    let each = STRETCH.min(length / STRETCHES as f64);
    (0..STRETCHES).map(|i| (((i as f64 + 0.5) * length / STRETCHES as f64 - each / 2.0).max(0.0), each)).collect()
}

/// A file placed by its words.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    /// Seconds to move every line by.
    pub shift: f64,
    /// How many heard lines agree on it, and how many were heard that could have.
    pub agreeing: usize,
    pub heard: usize,
}

/// A line's letters, in pairs, sorted: punctuation, spaces and case don't count.
fn pairs(text: &str) -> Vec<(char, char)> {
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    let mut pairs: Vec<(char, char)> = letters.windows(2).map(|w| (w[0], w[1])).collect();
    pairs.sort_unstable();
    pairs
}

/// How alike two lines are, 0 to 1: the share of their letter pairs they have in common.
fn alike(a: &[(char, char)], b: &[(char, char)]) -> f64 {
    let (mut i, mut j, mut both) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both += 1;
                i += 1;
                j += 1;
            }
        }
    }
    2.0 * both as f64 / (a.len() + b.len()) as f64
}

/// The most of `shifts` (sorted) that lie within `AGREE` of one another: where they start, and how many.
fn most_agreeing(shifts: &[f64]) -> (usize, usize) {
    let mut best = (0, 0);
    let mut from = 0;
    for to in 0..shifts.len() {
        while shifts[to] - shifts[from] > AGREE {
            from += 1;
        }
        if to + 1 - from > best.1 {
            best = (from, to + 1 - from);
        }
    }
    best
}

/// How far `lines` are from the episode, going by `heard`: the lines a speech
/// model heard in each stretch, timed to the episode. None unless enough of
/// them agree, from enough stretches, with no other shift half as likely.
pub fn place(lines: &[Line], heard: &[Vec<Line>]) -> Option<Placed> {
    let file: Vec<(f64, Vec<(char, char)>)> = lines.iter().filter(|l| !l.sign).map(|l| (l.start, pairs(&l.text))).filter(|(_, p)| p.len() + 1 >= LETTERS).collect();
    let mut said = 0;
    // (shift, stretch) for every heard line and file line that read alike.
    let mut votes: Vec<(f64, usize)> = Vec::new();
    for (stretch, heard) in heard.iter().enumerate() {
        for line in heard {
            let words = pairs(&line.text);
            if words.len() + 1 < LETTERS {
                continue;
            }
            said += 1;
            votes.extend(file.iter().filter(|(_, p)| alike(&words, p) >= SAME).map(|(start, _)| (line.start - start, stretch)));
        }
    }
    votes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let shifts: Vec<f64> = votes.iter().map(|v| v.0).collect();
    let (from, count) = most_agreeing(&shifts);
    let agreeing = &votes[from..from + count];
    let mut from_stretches: Vec<usize> = agreeing.iter().map(|v| v.1).collect();
    from_stretches.sort_unstable();
    from_stretches.dedup();
    if count < LINES || from_stretches.len() < FROM {
        return None;
    }
    let shift = agreeing[count / 2].0;
    // A file that repeats itself (a recap, a song) can agree at two shifts: then neither is safe.
    let elsewhere: Vec<f64> = shifts.iter().copied().filter(|s| (s - shift).abs() > 2.0 * AGREE).collect();
    if most_agreeing(&elsewhere).1 * 2 > count {
        return None;
    }
    Some(Placed { shift, agreeing: count, heard: said })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: f64, text: &str) -> Line {
        Line { start, end: start + 2.0, text: text.to_string(), ruby: vec![], speaker: String::new(), sign: false }
    }

    /// Forty different lines, four seconds apart from `start`.
    fn script(start: f64) -> Vec<Line> {
        (0..40u32).map(|i| line(start + i as f64 * 4.0, &(0..8).filter_map(|k| char::from_u32(0x4E00 + i * 8 + k)).collect::<String>())).collect()
    }

    /// What's heard of `script` in four stretches of five lines, `late` seconds after the file says.
    fn heard(script: &[Line], late: f64) -> Vec<Vec<Line>> {
        script.chunks(10).map(|c| c[..5].iter().map(|l| line(l.start + late, &format!("{}。", l.text))).collect()).collect()
    }

    #[test]
    fn a_file_is_placed_by_the_lines_heard() {
        let file = script(10.0);
        let placed = place(&file, &heard(&file, 7.3)).unwrap();
        assert!((placed.shift - 7.3).abs() < 1e-6, "{placed:?}");
        assert_eq!((placed.agreeing, placed.heard), (20, 20));
    }

    #[test]
    fn another_episode_s_file_is_not_placed() {
        let file = script(10.0);
        let other: Vec<Line> = (0..40).map(|i| line(10.0 + i as f64 * 4.0, &format!("ぜんぜん違う話をしている{i}"))).collect();
        assert_eq!(place(&other, &heard(&file, 0.0)), None);
    }

    #[test]
    fn lines_heard_in_two_stretches_are_too_few() {
        let file = script(10.0);
        let mut two = heard(&file, 3.0);
        two.truncate(2);
        assert_eq!(place(&file, &two), None);
    }

    #[test]
    fn a_file_cut_differently_is_not_placed() {
        // The second half is 30 s later in the episode than in the file: no one shift is right.
        let file = script(10.0);
        let mut cut = heard(&file, 0.0);
        for line in cut[2..].iter_mut().flatten() {
            line.start += 30.0;
        }
        assert_eq!(place(&file, &cut), None);
    }

    #[test]
    fn stretches_are_spread_over_the_episode() {
        assert_eq!(stretches(1440.0), [(135.0, 90.0), (495.0, 90.0), (855.0, 90.0), (1215.0, 90.0)]);
        assert_eq!(stretches(120.0), [(0.0, 30.0), (30.0, 30.0), (60.0, 30.0), (90.0, 30.0)]);
    }
}
