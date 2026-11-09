//! Small helpers shared by the rest of the backend.

use std::cmp::Ordering;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// A command for a helper program (ffmpeg, ffprobe, whisper) that never opens
/// a console window of its own.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Compares names the way Explorer sorts them: "Disc 2" before "Disc 10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let order = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// Characters as Tidepool and GameSentenceMiner count them: letters and
/// numbers only, so no punctuation, symbols or spaces.
pub fn count_chars(text: &str) -> i64 {
    text.chars().filter(|c| c.is_alphanumeric()).count() as i64
}

/// "12:31" or "1:02:03".
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).floor() as i64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// A file name made safe for Windows from any text.
pub fn safe_file_name(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').to_string();
    if trimmed.is_empty() { "file".into() } else { trimmed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_numbers_naturally() {
        let mut v = vec!["Disc 10/t00", "Disc 2/t01", "Disc 2/t00", "Disc 1/t03"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["Disc 1/t03", "Disc 2/t00", "Disc 2/t01", "Disc 10/t00"]);
        assert_eq!(natural_cmp("title_t09", "title_t10"), Ordering::Less);
    }

    #[test]
    fn counts_like_gsm() {
        assert_eq!(count_chars("明日の試験、絶対に合格してみせる。"), 15);
        assert_eq!(count_chars("「えっ？」 …"), 2);
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(751.4), "12:31");
        assert_eq!(clock(3723.0), "1:02:03");
    }
}
