//! Whether a newer Telly is out. Telly doesn't update itself: once a day it
//! asks GitHub for the latest release, and when that's newer than this one
//! the title bar and Settings say so, with its page to download it from.
//!
//!   GET https://api.github.com/repos/bee-san/telly/releases/latest
//!
//! Nothing about you or your library is sent: the request is the same from
//! every Telly but for its version, in the User-Agent GitHub asks for.
//! Settings → Updates turns the asking off (updates.check).

use crate::backend::Backend;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::Value;
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
        asset["browser_download_url"].as_str().filter(|url| named && ours(url)).map(str::to_string)
    });
    Some(Newer { version, page, installer })
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
                { "name": "Telly_0.10.3_x64-setup.exe", "browser_download_url": format!("https://github.com/bee-san/telly/releases/download/{tag}/Telly_0.10.3_x64-setup.exe") },
            ],
        })
    }

    #[test]
    fn a_later_release_is_newer_and_the_same_or_an_earlier_one_isn_t() {
        let found = newer("0.10.2", &release("v0.10.3")).unwrap();
        assert_eq!(found.version, "0.10.3");
        assert_eq!(found.page, "https://github.com/bee-san/telly/releases/tag/v0.10.3");
        assert_eq!(found.installer.as_deref(), Some("https://github.com/bee-san/telly/releases/download/v0.10.3/Telly_0.10.3_x64-setup.exe"));
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
}
