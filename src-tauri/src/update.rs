//! A newer Telly: whether one is out, and installing it. Once a day Telly
//! asks GitHub for the latest release, and when that's newer than this one
//! the title bar and Settings say so. Install, in Settings → Updates,
//! downloads the release's installer and runs it: it puts the newer Telly in
//! this one's place and opens it. Nothing is installed until you press it.
//!
//!   GET https://api.github.com/repos/bee-san/telly/releases/latest
//!
//! The installer isn't signed, so what says it's Telly's is where it comes
//! from: only a file on this project's own releases page, fetched over
//! HTTPS, of the size GitHub lists for it, is run. Nothing about you or your
//! library is sent: the request is the same from every Telly but for its
//! version, in the User-Agent GitHub asks for. Settings → Updates turns the
//! asking off (updates.check).

use crate::backend::Backend;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/bee-san/telly/releases/latest";
const USER_AGENT: &str = concat!("Telly/", env!("CARGO_PKG_VERSION"));
const FIRST_LOOK: Duration = Duration::from_secs(60);
const LOOK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// A release of Telly newer than this one.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Newer {
    /// "0.10.3"
    pub version: String,
    /// Its page on GitHub, with what changed.
    pub page: String,
    /// Its installer, when the release has one yet (it's built after the release is published).
    pub installer: Option<String>,
    /// The installer's size in bytes, as GitHub lists it.
    pub size: Option<u64>,
}

/// A version's numbers ("v0.10.2" is [0, 10, 2]). None for anything else (a pre-release's "0.11.0-beta").
fn numbers(version: &str) -> Option<Vec<u64>> {
    version.trim().trim_start_matches('v').split('.').map(|n| n.parse().ok()).collect()
}

/// The release GitHub answered with, if it's a later version than `current`.
/// Only addresses on github.com are passed on: the page is opened in your browser.
fn newer(current: &str, latest: &Value) -> Option<Newer> {
    if latest["draft"] == true || latest["prerelease"] == true {
        return None;
    }
    let version = latest["tag_name"].as_str()?.trim().trim_start_matches('v').to_string();
    if numbers(&version)? <= numbers(current)? {
        return None;
    }
    let ours = |url: &&str| url.starts_with("https://github.com/bee-san/telly/");
    let page = latest["html_url"].as_str().filter(ours)?.to_string();
    let installer = latest["assets"].as_array().into_iter().flatten().find_map(|asset| {
        let named = asset["name"].as_str().is_some_and(|name| name.ends_with("-setup.exe"));
        let url = asset["browser_download_url"].as_str().filter(|url| named && ours(url))?;
        Some((url.to_string(), asset["size"].as_u64()))
    });
    let (installer, size) = installer.map_or((None, None), |(url, size)| (Some(url), size));
    Some(Newer { version, page, installer, size })
}

/// Asks GitHub for the latest release.
fn latest() -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).http_status_as_error(false).build().into();
    let mut response = agent
        .get(LATEST)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| anyhow!("GitHub isn’t reachable ({e})"))?;
    if response.status().as_u16() != 200 {
        return Err(anyhow!("GitHub answered {}", response.status().as_u16()));
    }
    response.body_mut().with_config().limit(4 * 1024 * 1024).read_json().context("GitHub sent something unexpected")
}

/// Installers bigger than this aren't Telly's.
const MAX_INSTALLER: u64 = 1024 * 1024 * 1024;

/// Downloads `newer`'s installer into `folder`, saying how far it's got, and returns where it is.
/// It has to be the size GitHub lists for it, and a Windows program.
fn download(newer: &Newer, folder: &Path, progress: impl Fn(f64)) -> Result<PathBuf> {
    let url = newer.installer.as_deref().ok_or_else(|| anyhow!("Telly {}’s installer isn’t built yet. Try again in a few minutes.", newer.version))?;
    if !url.starts_with("https://github.com/bee-san/telly/releases/download/") {
        return Err(anyhow!("That installer isn’t on Telly’s releases page"));
    }
    // The last one's installer has done its work.
    let _ = std::fs::remove_dir_all(folder);
    std::fs::create_dir_all(folder)?;
    let installer = folder.join(format!("Telly_{}_x64-setup.exe", crate::util::safe_file_name(&newer.version)));
    let partial = installer.with_extension("part");
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3600))).http_status_as_error(false).build().into();
    let mut response = agent.get(url).header("User-Agent", USER_AGENT).call().map_err(|e| anyhow!("GitHub isn’t reachable ({e})"))?;
    if response.status().as_u16() != 200 {
        return Err(anyhow!("GitHub answered {} for the installer", response.status().as_u16()));
    }
    let listed = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    let total = newer.size.or(listed).unwrap_or(0);
    let mut reader = response.body_mut().with_config().limit(MAX_INSTALLER).reader();
    let mut file = std::fs::File::create(&partial)?;
    let mut buffer = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = std::io::Read::read(&mut reader, &mut buffer)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buffer[..n])?;
        done += n as u64;
        if total > 0 {
            progress((done as f64 / total as f64).min(1.0));
        }
    }
    file.sync_all()?;
    drop(file);
    check(&partial, newer.size)?;
    std::fs::rename(&partial, &installer)?;
    Ok(installer)
}

/// Whether what was downloaded is the installer: the size GitHub lists for it, and a Windows program.
fn check(file: &Path, size: Option<u64>) -> Result<()> {
    let got = std::fs::metadata(file)?.len();
    if size.is_some_and(|size| size != got) {
        return Err(anyhow!("The installer didn’t download whole ({got} bytes of {})", size.unwrap_or(0)));
    }
    let mut start = [0u8; 2];
    std::io::Read::read_exact(&mut std::fs::File::open(file)?, &mut start).map_err(|_| anyhow!("The installer that downloaded is empty"))?;
    if &start != b"MZ" {
        return Err(anyhow!("What downloaded isn’t an installer"));
    }
    Ok(())
}

/// Downloads the newer Telly's installer and runs it, in the background: the
/// title bar shows the download, then this Telly closes and the installer
/// puts the newer one in its place and opens it (/P: without questions, /R:
/// opening Telly after, /UPDATE: over the Telly that's here). What goes wrong
/// is said in a note, and this Telly stays open.
pub fn install(backend: Arc<Backend>) -> Result<()> {
    static INSTALLING: AtomicBool = AtomicBool::new(false);
    let newer = backend.newer().ok_or_else(|| anyhow!("There’s no newer Telly to install"))?;
    if !cfg!(windows) {
        return Err(anyhow!("Telly installs itself only on Windows. Its installer is on {}", newer.page));
    }
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err(anyhow!("Telly {} is being installed already", newer.version));
    }
    std::thread::spawn(move || {
        let label = format!("Downloading Telly {}", newer.version);
        let task = |progress: f64| {
            let task = crate::jobs::Task { show_id: 0, episode_id: 0, number: 0, title: "Telly".into(), label: label.clone(), progress };
            crate::jobs::set_task(&backend.jobs.download, Some(task));
        };
        task(0.0);
        // Not in Telly's own folder, which the installer writes to.
        let folder = std::env::temp_dir().join("Telly-update");
        let ran = download(&newer, &folder, task).and_then(|installer| {
            std::process::Command::new(&installer).args(["/P", "/R", "/UPDATE"]).spawn().with_context(|| format!("starting {}", installer.display()))
        });
        crate::jobs::set_task(&backend.jobs.download, None);
        match ran {
            // The installer takes it from here: it waits for this Telly to close.
            Ok(_) => {
                std::thread::sleep(Duration::from_millis(500));
                std::process::exit(0);
            }
            Err(e) => {
                INSTALLING.store(false, Ordering::SeqCst);
                backend.push_event("failed", &format!("Telly {} wasn’t installed", newer.version), &format!("{e:#}"), 0, 0);
            }
        }
    });
    Ok(())
}

/// Looks for a newer Telly a minute after it opens and once a day after, unless Settings says not to.
pub fn start(backend: Arc<Backend>) {
    let looking = move || {
        std::thread::sleep(FIRST_LOOK);
        loop {
            if backend.db.setting_bool("updates.check", true) {
                match latest() {
                    Ok(release) => backend.set_newer(newer(env!("CARGO_PKG_VERSION"), &release)),
                    Err(e) => eprintln!("update: {e:#}; asking again tomorrow"),
                }
            } else {
                backend.set_newer(None);
            }
            std::thread::sleep(LOOK_EVERY);
        }
    };
    std::thread::Builder::new().name("telly-update".into()).spawn(looking).ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> Value {
        json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "html_url": format!("https://github.com/bee-san/telly/releases/tag/{tag}"),
            "assets": [
                { "name": "notes.txt", "browser_download_url": "https://github.com/bee-san/telly/releases/download/x/notes.txt" },
                { "name": "Telly_0.10.3_x64-setup.exe", "size": 48_000_000u64, "browser_download_url": format!("https://github.com/bee-san/telly/releases/download/{tag}/Telly_0.10.3_x64-setup.exe") },
            ],
        })
    }

    #[test]
    fn a_later_release_is_newer_and_the_same_or_an_earlier_one_isn_t() {
        let found = newer("0.10.2", &release("v0.10.3")).unwrap();
        assert_eq!(found.version, "0.10.3");
        assert_eq!(found.page, "https://github.com/bee-san/telly/releases/tag/v0.10.3");
        assert_eq!(found.installer.as_deref(), Some("https://github.com/bee-san/telly/releases/download/v0.10.3/Telly_0.10.3_x64-setup.exe"));
        assert_eq!(found.size, Some(48_000_000));
        assert_eq!(newer("0.10.3", &release("v0.10.3")), None);
        assert_eq!(newer("0.11.0", &release("v0.10.3")), None);
        // By its numbers, not its letters: 0.10 is after 0.9.
        assert!(newer("0.9.1", &release("v0.10.0")).is_some());
        assert_eq!(newer("0.10.0", &release("v0.9.1")), None);
    }

    #[test]
    fn a_draft_a_pre_release_or_a_page_elsewhere_isn_t_offered() {
        let mut draft = release("v0.11.0");
        draft["draft"] = json!(true);
        assert_eq!(newer("0.10.2", &draft), None);
        let mut early = release("v0.11.0");
        early["prerelease"] = json!(true);
        assert_eq!(newer("0.10.2", &early), None);
        assert_eq!(newer("0.10.2", &release("v0.11.0-beta")), None);
        let mut elsewhere = release("v0.11.0");
        elsewhere["html_url"] = json!("https://example.com/telly");
        assert_eq!(newer("0.10.2", &elsewhere), None);
        // A release whose installer isn't built yet still has its page.
        let mut unbuilt = release("v0.11.0");
        unbuilt["assets"] = json!([]);
        assert_eq!(newer("0.10.2", &unbuilt).map(|n| n.installer), Some(None));
    }

    #[test]
    fn only_a_whole_installer_from_telly_s_releases_is_run() {
        let dir = std::env::temp_dir().join(format!("telly-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("setup.part");
        // The size GitHub lists, and a Windows program.
        std::fs::write(&file, b"MZ\x90\x00 the rest").unwrap();
        assert!(check(&file, Some(13)).is_ok() && check(&file, None).is_ok());
        // Cut short, or a page saying it can't be found.
        assert!(check(&file, Some(48_000_000)).unwrap_err().to_string().contains("didn’t download whole"));
        std::fs::write(&file, b"<html>Not Found</html>").unwrap();
        assert!(check(&file, None).unwrap_err().to_string().contains("isn’t an installer"));
        // An installer anywhere but the project's releases is never fetched.
        let elsewhere = Newer { version: "9.9.9".into(), page: String::new(), installer: Some("https://example.com/Telly_9.9.9_x64-setup.exe".into()), size: None };
        assert!(download(&elsewhere, &dir.join("to"), |_| {}).unwrap_err().to_string().contains("isn’t on Telly’s releases page"));
        let unbuilt = Newer { installer: None, ..elsewhere };
        assert!(download(&unbuilt, &dir.join("to"), |_| {}).unwrap_err().to_string().contains("isn’t built yet"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
