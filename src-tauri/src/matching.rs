//! Which subtitle files could be an episode's, and in what order to try them.
//!
//! A show added as one folder may hold several seasons, each in a folder of
//! its own, numbered from 1 in its file names. Subtitle sites number them
//! either way: Jimaku's Netflix pack for Fairy Tail runs 1 to 328 across every
//! season, so the 2014 series' first episode is 176 there; a season's own
//! entry calls it 1. So an episode's files are looked for under Telly's
//! number, the number in its file name, and that number after the season
//! folders before it, and the sync's score picks the one that fits.

use std::collections::BTreeMap;
use std::path::Path;

/// The season folder an episode is in, relative to the show's folder ("" when
/// it's straight in the show's folder).
pub fn group_of(show_folder: &str, episode_path: &str) -> String {
    let parent = Path::new(episode_path).parent().map(Path::to_path_buf).unwrap_or_default();
    let base = Path::new(show_folder);
    let relative = parent.strip_prefix(base).map(Path::to_path_buf).unwrap_or(parent);
    relative.to_string_lossy().replace('\\', "/").trim_matches('/').to_string()
}

/// A show's season folders in the order they come (by their first episode),
/// with how many episodes each holds.
pub fn groups(show_folder: &str, episodes: &[(i64, String)]) -> Vec<(String, usize)> {
    let mut first: BTreeMap<String, (i64, usize)> = BTreeMap::new();
    for (number, path) in episodes {
        let entry = first.entry(group_of(show_folder, path)).or_insert((*number, 0));
        entry.0 = entry.0.min(*number);
        entry.1 += 1;
    }
    let mut out: Vec<(String, (i64, usize))> = first.into_iter().collect();
    out.sort_by_key(|(_, (n, _))| *n);
    out.into_iter().map(|(name, (_, count))| (name, count)).collect()
}

/// The numbers an episode's subtitles could be filed under, likeliest first:
/// the offset that worked for its season before (`learned`), Telly's number,
/// the file's own number, then the file's number after each combination of
/// the season folders before it (the run's numbering), longest folders first,
/// since short ones are usually OVAs that the run's numbering leaves out.
pub fn numbers_for(number: i64, local: Option<i64>, earlier: &[usize], learned: Option<i64>) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    let mut add = |n: i64| {
        if n >= 1 && !out.contains(&n) {
            out.push(n);
        }
    };
    if let (Some(l), Some(offset)) = (local, learned) {
        add(l + offset);
    }
    add(number);
    if let Some(l) = local {
        add(l);
        // Every combination of the earlier folders (there are only a few), by
        // how much of the run they make up, longest first.
        let k = earlier.len().min(8);
        let mut sums: Vec<(usize, usize)> = (1u32..(1 << k))
            .map(|mask| {
                let sum: usize = (0..k).filter(|i| mask & (1 << i) != 0).map(|i| earlier[i]).sum();
                let long: usize = (0..k).filter(|i| mask & (1 << i) != 0 && earlier[*i] >= 13).map(|i| earlier[i]).sum();
                (long, sum)
            })
            .collect();
        sums.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        for (_, sum) in sums {
            add(l + sum as i64);
        }
    }
    out
}

/// How one subtitle file fitted an episode, to compare it with the others.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// The sync's score, 0 to 1.
    pub score: f64,
    /// `subs::quality`: the share of lines that read as Japanese.
    pub quality: f64,
    pub offset_ms: i64,
    /// How far above chance it stands (`sync::Standing::above`).
    pub z: f64,
    /// The file's name carries the rip's release tag ("[Judas]"): made for it.
    pub made_for: bool,
    /// It fits only cut into pieces (`sync::Synced::cut`).
    pub cut: bool,
}

/// The release tag a file name starts with: "[Judas]" in "[Judas] Fairy Tail - 001.mkv".
pub fn release_tag(name: &str) -> Option<String> {
    let name = name.trim();
    let rest = name.strip_prefix('[')?;
    let end = rest.find(']')?;
    let tag = rest[..end].trim();
    (!tag.is_empty()).then(|| tag.to_lowercase())
}

/// Whether a subtitle file's name says it was made for the rip `video` (the same release tag).
pub fn made_for(subtitle: &str, video: &str) -> bool {
    let subtitle = subtitle.rsplit('›').next().unwrap_or(subtitle);
    matches!((release_tag(subtitle), release_tag(video)), (Some(a), Some(b)) if a == b)
}

/// A file synced in one piece beats one that fits only cut into pieces.
/// Otherwise a file that fits beats one that doesn't; of files that fit, clean
/// text beats furigana or English set as lines, then one made for this very
/// release (its timing is its own), then the smaller shift, then the tighter fit.
pub fn better(a: &Fit, b: &Fit) -> bool {
    const FITS: f64 = 0.75;
    const CLEAN: f64 = 0.85;
    if a.cut != b.cut {
        let whole = if a.cut { b } else { a };
        if whole.score >= crate::jobs::GOOD_SYNC {
            return !a.cut;
        }
    }
    let (fa, fb) = (a.score >= FITS, b.score >= FITS);
    if fa != fb {
        return fa;
    }
    if !fa {
        return a.score > b.score || (a.score == b.score && a.z > b.z);
    }
    let (ca, cb) = (a.quality >= CLEAN, b.quality >= CLEAN);
    if ca != cb {
        return ca;
    }
    if a.made_for != b.made_for {
        return a.made_for;
    }
    let (oa, ob) = (a.offset_ms.abs(), b.offset_ms.abs());
    if oa + 500 < ob {
        return true;
    }
    if ob + 500 < oa {
        return false;
    }
    a.z > b.z
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOW: &str = r"C:\Videos\Fairy Tail";

    fn library() -> Vec<(i64, String)> {
        let mut eps = Vec::new();
        for i in 1..=175 {
            eps.push((i, format!(r"{SHOW}\[Judas] Fairy Tail (2009)\[Judas] Fairy Tail (2009) - {i:03}.mkv")));
        }
        for i in 1..=9 {
            eps.push((175 + i, format!(r"{SHOW}\[Judas] Fairy Tail (2011) - OVAs\[Judas] Fairy Tail (2011) - OVA {i:02}.mkv")));
        }
        for i in 1..=102 {
            eps.push((184 + i, format!(r"{SHOW}\[Judas] Fairy Tail (2014)\[Judas] Fairy Tail (2014) - {i:03}.mkv")));
        }
        eps
    }

    #[test]
    fn picks_the_clean_file_made_for_the_release() {
        let fit = |score: f64, quality: f64, offset_ms: i64, z: f64| Fit { score, quality, offset_ms, z, made_for: false, cut: false };
        let judas = Fit { made_for: true, ..fit(1.0, 0.99, -40, 10.3) };
        let netflix = fit(1.0, 0.98, 1670, 9.7);
        let furigana_lines = fit(1.0, 0.6, 40, 12.9);
        let other_episode = fit(0.27, 0.99, -7740, 3.1);
        assert!(better(&judas, &netflix) && !better(&netflix, &judas));
        assert!(better(&netflix, &furigana_lines));
        assert!(better(&netflix, &other_episode));
        // Close shifts: the tighter fit.
        assert!(better(&fit(1.0, 0.99, 1670, 12.0), &fit(1.0, 0.99, 1740, 10.0)));
        // Made for the release beats a tighter fit at the same shift.
        assert!(better(&judas, &fit(1.0, 0.94, 40, 12.9)));
        assert!(made_for("[Judas] Fairy Tail (2009) - 017 [BD 1080p].srt", "[Judas] Fairy Tail (2009) - 017.mkv"));
        assert!(made_for("Pack.zip › [judas] Show - 01.srt", "[Judas] Show - 01.mkv"));
        assert!(!made_for("[Erai-raws] Fairy Tail - 017.srt", "[Judas] Fairy Tail (2009) - 017.mkv"));
    }

    #[test]
    fn a_file_that_fits_in_one_piece_beats_one_cut_into_pieces() {
        let fit = |cut: bool, made_for: bool, z: f64| Fit { score: 1.0, quality: 0.99, offset_ms: 0, z, made_for, cut };
        // A TV broadcast's file, cut where its ad breaks were, loses to one
        // that fits with one shift, even one made for another release, and
        // even one only just synced (Fairy Tail 2014's 45th: 0.52).
        assert!(better(&fit(false, false, 7.6), &fit(true, true, 30.0)));
        assert!(!better(&fit(true, true, 30.0), &fit(false, false, 7.6)));
        assert!(better(&Fit { score: 0.52, ..fit(false, false, 6.3) }, &fit(true, false, 30.0)));
        // It still beats one that isn't sure to fit.
        assert!(better(&fit(true, false, 9.0), &Fit { score: 0.3, ..fit(false, false, 5.75) }));
    }

    #[test]
    fn season_folders_in_order() {
        let g = groups(SHOW, &library());
        assert_eq!(g, vec![("[Judas] Fairy Tail (2009)".to_string(), 175), ("[Judas] Fairy Tail (2011) - OVAs".to_string(), 9), ("[Judas] Fairy Tail (2014)".to_string(), 102)]);
        assert_eq!(group_of(SHOW, &format!(r"{SHOW}\Show - 01.mkv")), "");
    }

    #[test]
    fn the_2014_series_is_looked_for_after_the_2009_run() {
        // Telly's 185 is the 2014 series' first episode: Netflix's 176.
        let n = numbers_for(185, Some(1), &[175, 9], None);
        assert_eq!(&n[..3], &[185, 1, 176]);
        assert!(n.contains(&185) && n.contains(&10));
        // Once 176 has fitted, its season's offset comes first.
        assert_eq!(numbers_for(186, Some(2), &[175, 9], Some(175))[0], 177);
        // A one-folder show: just its number.
        assert_eq!(numbers_for(12, Some(12), &[], None), vec![12]);
    }
}
