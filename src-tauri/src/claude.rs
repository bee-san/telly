//! Claude, for "Explain this line" and "Previously on": the Claude Code on
//! this PC, run as `claude -p` with no tools, so it uses your own Claude Code
//! sign-in and only answers. Gromit Reader's claude.rs, reading Claude Code's
//! stream-json the way Tidepool's Learn does, so an answer shows as it's written.

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(240);

/// One line: arguments with a line break can't be passed to a .cmd (an npm install's claude.cmd).
pub const SYSTEM: &str = "You help someone learn Japanese by watching anime with Japanese subtitles. \
Answer only from the subtitles you are given. Never reveal or guess anything that happens after them, even if you know the show. \
Write plain text, with no headings, tables or Markdown.";

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub available: bool,
    pub version: String,
    pub detail: String,
}

/// The claude executable: TELLY_CLAUDE (a stand-in for tests, which may be a
/// .mjs script run with node), then PATH, then where Claude Code's installers put it.
pub fn find() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("TELLY_CLAUDE") {
        return Some(PathBuf::from(path));
    }
    let names: &[&str] = if cfg!(windows) { &["claude.exe", "claude.cmd", "claude.bat"] } else { &["claude"] };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        dirs.push(PathBuf::from(&home).join(".local").join("bin"));
        dirs.push(PathBuf::from(&home).join(".claude").join("local"));
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
    }
    dirs.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

/// The command for `exe`. Rust runs a .cmd through cmd.exe itself, quoting
/// its arguments safely (Gromit Reader's note); a .mjs stand-in runs with node.
fn command(exe: &Path) -> Command {
    let script = exe.extension().is_some_and(|ext| ext == "mjs" || ext == "js");
    let mut cmd = if script {
        let mut node = crate::util::command("node");
        node.arg(exe);
        node
    } else {
        crate::util::command(exe)
    };
    // Away from any project, so no project's CLAUDE.md or settings come along.
    cmd.current_dir(std::env::temp_dir());
    cmd
}

fn kill(pid: u32) {
    let mut cmd = if cfg!(windows) {
        let mut c = crate::util::command("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]);
        c
    } else {
        let mut c = Command::new("kill");
        c.arg(pid.to_string());
        c
    };
    let _ = cmd.output();
}

/// Whether Claude Code is here, and its version. Asked at most once a minute.
pub fn status() -> Status {
    static LAST: OnceLock<Mutex<Option<(Instant, Status)>>> = OnceLock::new();
    let last = LAST.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = last.lock() {
        if let Some((at, status)) = guard.as_ref() {
            if at.elapsed() < Duration::from_secs(60) {
                return status.clone();
            }
        }
    }
    let status = check();
    if let Ok(mut guard) = last.lock() {
        *guard = Some((Instant::now(), status.clone()));
    }
    status
}

fn check() -> Status {
    let Some(exe) = find() else {
        return Status {
            available: false,
            version: String::new(),
            detail: "Claude Code isn’t installed on this computer (no claude command was found).".into(),
        };
    };
    let mut cmd = command(&exe);
    cmd.arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let Ok(child) = cmd.spawn() else {
        return Status { available: false, version: String::new(), detail: format!("Couldn’t start {}", exe.display()) };
    };
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(out)) if out.status.success() => Status {
            available: true,
            version: String::from_utf8_lossy(&out.stdout).trim().to_string(),
            detail: exe.display().to_string(),
        },
        Ok(Ok(out)) => Status { available: false, version: String::new(), detail: String::from_utf8_lossy(&out.stderr).trim().to_string() },
        Ok(Err(e)) => Status { available: false, version: String::new(), detail: e.to_string() },
        Err(_) => {
            kill(pid);
            Status { available: false, version: String::new(), detail: "Claude Code didn’t answer.".into() }
        }
    }
}

/// One question for Claude.
pub struct Ask<'a> {
    pub prompt: &'a str,
    /// A model name for --model (opus, sonnet, haiku…), or Claude Code's own default.
    pub model: Option<&'a str>,
    /// --effort: "low" answers in about a third of the time, for retelling and explaining.
    pub effort: Option<&'a str>,
}

/// Asks Claude, calling `on_text` with each piece of the answer as it's
/// written, and returns the whole answer. `stop` ends it early.
pub fn ask(question: &Ask, mut on_text: impl FnMut(&str), stop: &Arc<AtomicBool>) -> Result<String> {
    let exe = find().ok_or_else(|| anyhow!("Claude Code isn’t installed on this computer (no claude command was found)."))?;
    match run(&exe, question, true, &mut on_text, stop) {
        // An older Claude Code without --safe-mode or --effort: ask again with Gromit Reader's flags.
        Err(e) if format!("{e:#}").to_lowercase().contains("unknown option") => run(&exe, question, false, &mut on_text, stop),
        answer => answer,
    }
}

fn run(exe: &Path, question: &Ask, modern: bool, on_text: &mut impl FnMut(&str), stop: &Arc<AtomicBool>) -> Result<String> {
    let mut cmd = command(exe);
    cmd.args(["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages"]);
    cmd.args(["--tools", "", "--no-session-persistence", "--system-prompt", SYSTEM]);
    if modern {
        // No CLAUDE.md, hooks, plugins or MCP servers: just the answer.
        cmd.args(["--safe-mode", "--strict-mcp-config"]);
        if let Some(effort) = question.effort {
            cmd.args(["--effort", effort]);
        }
    }
    // A model name is one word; anything else can't go to a .cmd.
    if let Some(model) = question.model.map(str::trim).filter(|m| !m.is_empty() && !m.contains(char::is_whitespace)) {
        cmd.args(["--model", model]);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| anyhow!("Couldn’t start Claude Code: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(question.prompt.as_bytes()).map_err(|e| anyhow!("Couldn’t talk to Claude Code: {e}"))?;
    }
    let stderr = child.stderr.take();
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    });
    // Ends Claude Code if it's stopped or takes too long; reading its output then ends too.
    let pid = child.id();
    let finished = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    {
        let (finished, timed_out, stop) = (finished.clone(), timed_out.clone(), stop.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            while !finished.load(Ordering::SeqCst) {
                if stop.load(Ordering::SeqCst) || started.elapsed() > TIMEOUT {
                    timed_out.store(!stop.load(Ordering::SeqCst), Ordering::SeqCst);
                    kill(pid);
                    break;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        });
    }
    let mut text = String::new();
    let mut outcome: Option<std::result::Result<String, String>> = None;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(result) = read_line(&line, &mut text, on_text) {
                outcome = Some(result);
            }
        }
    }
    let _ = child.wait();
    finished.store(true, Ordering::SeqCst);
    let stderr = errors.join().unwrap_or_default();
    if stop.load(Ordering::SeqCst) {
        return Err(anyhow!("Stopped."));
    }
    if timed_out.load(Ordering::SeqCst) {
        return Err(anyhow!("Claude didn’t answer within {} seconds.", TIMEOUT.as_secs()));
    }
    match outcome {
        Some(Ok(result)) if result.trim().is_empty() && !text.trim().is_empty() => Ok(text.trim().to_string()),
        Some(Ok(result)) if !result.trim().is_empty() => Ok(result.trim().to_string()),
        Some(Ok(_)) => Err(anyhow!("Claude gave no answer.")),
        Some(Err(message)) => Err(classify(&message)),
        None if stderr.trim().is_empty() => Err(anyhow!("Claude Code stopped without answering.")),
        None => Err(classify(&stderr)),
    }
}

/// Reads one line of stream-json (Tidepool's learn_cli.rs). Text goes to
/// `on_text` and `text`; the final `result` line ends the answer.
fn read_line(line: &str, text: &mut String, on_text: &mut impl FnMut(&str)) -> Option<std::result::Result<String, String>> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value["type"].as_str()? {
        "stream_event" => {
            let event = &value["event"];
            match event["type"].as_str()? {
                "content_block_start" if event["content_block"]["type"] == "text" && !text.is_empty() => {
                    text.push_str("\n\n");
                    on_text("\n\n");
                    None
                }
                "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                    let piece = event["delta"]["text"].as_str()?;
                    text.push_str(piece);
                    on_text(piece);
                    None
                }
                _ => None,
            }
        }
        "result" => {
            let result = value["result"].as_str().unwrap_or_default().to_string();
            if value["is_error"].as_bool() == Some(true) || value["subtype"] != "success" {
                Some(Err(if result.is_empty() { value["subtype"].as_str().unwrap_or("error").to_string() } else { result }))
            } else {
                Some(Ok(result))
            }
        }
        _ => None,
    }
}

fn classify(message: &str) -> anyhow::Error {
    let lower = message.to_lowercase();
    if lower.contains("unknown option") {
        anyhow!("unknown option: {}", message.trim())
    } else if ["authenticat", "oauth", "/login", "not logged in", "log in", "sign in"].iter().any(|w| lower.contains(w)) {
        anyhow!("Claude Code isn’t signed in. Run claude in a terminal and sign in, then try again.")
    } else {
        let first: String = message.trim().lines().next().unwrap_or("").trim().chars().take(300).collect();
        anyhow!(if first.is_empty() { "Claude Code couldn’t answer.".to_string() } else { format!("Claude Code: {first}") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str]) -> (String, Vec<String>, Option<std::result::Result<String, String>>) {
        let mut text = String::new();
        let mut pieces = Vec::new();
        let mut outcome = None;
        for line in lines {
            if let Some(r) = read_line(line, &mut text, &mut |p: &str| pieces.push(p.to_string())) {
                outcome = Some(r);
            }
        }
        (text, pieces, outcome)
    }

    #[test]
    fn streams_text_and_takes_the_final_result() {
        let (text, pieces, outcome) = feed(&[
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"I'm "}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hungry."}}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"I'm hungry."}"#,
        ]);
        assert_eq!(text, "I'm hungry.");
        assert_eq!(pieces, vec!["I'm ", "hungry."]);
        assert_eq!(outcome, Some(Ok("I'm hungry.".into())));
        let (_, _, failed) = feed(&[r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Not logged in · Please run /login"}"#]);
        assert!(format!("{}", classify(&failed.unwrap().unwrap_err())).contains("isn’t signed in"));
    }
}
