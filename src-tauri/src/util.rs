//! Small helpers shared by the rest of the backend.

use std::cmp::Ordering;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Unix time in milliseconds.
/// Writes a file whole or not at all: to a file beside it first, then put in
/// its place, so a crash mid-write (or a reader at the same moment) never
/// sees half of it. Half a JSON file reads back as nothing: no lines, no
/// known words.
pub fn write_whole(path: &std::path::Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let part = path.with_file_name(format!("{name}.part-{}-{n}", std::process::id()));
    std::fs::write(&part, bytes).and_then(|()| std::fs::rename(&part, path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
}

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

/// The longest a helper program gets for a quick question (ffprobe's of a rip, one frame, a short clip).
pub const QUICK: std::time::Duration = std::time::Duration::from_secs(3 * 60);
/// The longest it gets to read a rip from end to end: a film off a slow hard drive takes minutes.
pub const WHOLE_RIP: std::time::Duration = std::time::Duration::from_secs(90 * 60);
/// The longest whisper.cpp gets to hear an episode with its biggest model on a slow processor.
pub const LISTENING: std::time::Duration = std::time::Duration::from_secs(8 * 60 * 60);

/// Running a helper program to its end, for no longer than a limit. A read of
/// a rip on a share that stopped answering, or an ffmpeg that hangs on a
/// broken file, would otherwise hold its worker (and the drive's turn) until
/// Telly restarts. The program also goes when Telly does (`own`).
pub trait Helper {
    /// As `Command::output`, but the program is stopped after `limit`, which is then the error.
    fn output_within(&mut self, limit: std::time::Duration) -> std::io::Result<std::process::Output>;
    /// As `Command::status`, with nothing of what the program writes kept.
    fn status_within(&mut self, limit: std::time::Duration) -> std::io::Result<std::process::ExitStatus>;
}

impl Helper for Command {
    fn output_within(&mut self, limit: std::time::Duration) -> std::io::Result<std::process::Output> {
        use std::io::Read;
        use std::process::Stdio;
        let mut child = self.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        own(&child);
        // Read as it's written: a program whose pipe is full waits for a reader.
        let read = |mut pipe: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                bytes
            })
        };
        let stdout = read(Box::new(child.stdout.take().expect("piped stdout")));
        let stderr = read(Box::new(child.stderr.take().expect("piped stderr")));
        let status = wait_within(&mut child, limit);
        let (stdout, stderr) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
        Ok(std::process::Output { status: status?, stdout, stderr })
    }

    fn status_within(&mut self, limit: std::time::Duration) -> std::io::Result<std::process::ExitStatus> {
        use std::process::Stdio;
        let mut child = self.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        own(&child);
        wait_within(&mut child, limit)
    }
}

/// Waits for a program to end, stopping it once `limit` has passed.
fn wait_within(child: &mut std::process::Child, limit: std::time::Duration) -> std::io::Result<std::process::ExitStatus> {
    let started = std::time::Instant::now();
    let mut pause = std::time::Duration::from_millis(2);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            let minutes = limit.as_secs().div_ceil(60);
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("it was still running after {minutes} minutes, and Telly stopped it")));
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(std::time::Duration::from_millis(100));
    }
}

/// Has a helper program Telly started end when Telly does, however Telly
/// ends: it joins a Windows job that closes with Telly, and Windows ends
/// what's in it. A conversion left running after Telly closed would go on
/// for minutes with nobody to use its copy, beside the one Telly starts when
/// it next opens.
#[cfg(windows)]
pub fn own(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    // Kept as a number: a handle is a pointer, which a static can't hold.
    static JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    // SAFETY: the job is made once and never closed (it closes with Telly, which is the point);
    // `limits` is a zeroed struct of the size passed, and the child's handle is open while `child` is.
    unsafe {
        let job = *JOB.get_or_init(|| {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&raw const limits).cast(), size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
            job as usize
        });
        if job != 0 {
            AssignProcessToJobObject(job as _, child.as_raw_handle() as _);
        }
    }
}

/// Elsewhere than Windows (where Telly is only developed), helper programs are left to end by themselves.
#[cfg(not(windows))]
pub fn own(_child: &std::process::Child) {}

/// Full-width letters, digits and signs (Ｓ０１Ｅ０５, 第１０話, ＃０５) as
/// their ASCII selves, and the ideographic space as a space.
pub fn half_width(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '！'..='～' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            _ => c,
        })
        .collect()
}

/// A digit, full-width (第１０話) or not, as an ASCII digit.
fn digit(c: char) -> Option<char> {
    match c {
        '0'..='9' => Some(c),
        '０'..='９' => char::from_u32(c as u32 - 0xFEE0),
        _ => None,
    }
}

/// Compares names the way Explorer sorts them: "Disc 2" before "Disc 10",
/// 第２話 before 第１０話.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if digit(x).is_some() && digit(y).is_some() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().and_then(digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().and_then(digit) {
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
    fn full_width_numbers_sort_by_their_value() {
        let mut v = vec!["第１０話", "第２話", "第１１話", "第１話"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["第１話", "第２話", "第１０話", "第１１話"]);
        assert_eq!(half_width("ＳＰＹ×ＦＡＭＩＬＹ　＃０５"), "SPY×FAMILY #05");
    }

    #[test]
    fn counts_like_gsm() {
        assert_eq!(count_chars("明日の試験、絶対に合格してみせる。"), 15);
        assert_eq!(count_chars("「えっ？」 …"), 2);
    }

    #[test]
    fn a_helper_program_is_stopped_after_its_limit() {
        use std::time::{Duration, Instant};
        if cfg!(windows) {
            let said = command("cmd").args(["/C", "echo hello"]).output_within(Duration::from_secs(20)).unwrap();
            assert_eq!(String::from_utf8_lossy(&said.stdout).trim(), "hello");
            let started = Instant::now();
            let hung = command("ping").args(["-n", "30", "127.0.0.1"]).output_within(Duration::from_millis(300));
            assert_eq!(hung.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
            assert!(started.elapsed() < Duration::from_secs(10));
        } else {
            let said = command("sh").args(["-c", "echo hello; echo oops >&2"]).output_within(Duration::from_secs(20)).unwrap();
            assert_eq!((String::from_utf8_lossy(&said.stdout).trim(), String::from_utf8_lossy(&said.stderr).trim()), ("hello", "oops"));
            assert!(command("sh").args(["-c", "exit 3"]).status_within(Duration::from_secs(20)).is_ok_and(|s| s.code() == Some(3)));
            let started = Instant::now();
            let hung = command("sleep").arg("30").output_within(Duration::from_millis(300));
            assert_eq!(hung.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
            assert!(started.elapsed() < Duration::from_secs(10));
        }
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(751.4), "12:31");
        assert_eq!(clock(3723.0), "1:02:03");
    }
}
