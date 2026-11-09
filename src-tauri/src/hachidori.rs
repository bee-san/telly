//! Hachidori inside Telly (the same way Gromit Reader embeds it).
//!
//! `npm run prepare:hachidori` downloads Hachidori's Chrome release into
//! src-tauri/extensions/hachidori, gives it a fixed key (so its ID, dictionaries
//! and settings survive updates), turns on its overlay mode (its contract for
//! apps that embed it) and copies in src-tauri/hachidori-addon/telly/, our
//! engine host page. WebView2 loads every folder in extensions/ as an unpacked
//! extension, and Hachidori's content script then puts the real Hachidori
//! pop-up over the player.
//!
//! WebView2 has no chrome.offscreen, so Hachidori cannot start its dictionary
//! engine by itself. A hidden window keeps telly/host.html open on the
//! extension's origin; it runs the engine in a frame and links this Hachidori
//! to the one in the user's browser, through Hachidori Relay in Anki. The page
//! reports how that is going in its title, which `HostStatus::from_title` reads.
//!
//! Only WebView2 can load Chrome extensions, so on other systems Telly runs
//! without Hachidori.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub id: String,
    pub version: String,
}

pub struct Hachidori {
    pub extensions_dir: Option<PathBuf>,
    pub prepared: Option<Prepared>,
}

/// What telly/host.html last reported.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    /// starting, linked, connecting, standalone, link-failed or error.
    pub state: String,
    /// The linked address, or what went wrong.
    #[serde(default)]
    pub detail: String,
    /// The linked browser's name, when Hachidori knows it.
    #[serde(default)]
    pub host: String,
}

const TITLE_PREFIX: &str = "telly-hachidori:";

impl HostStatus {
    /// Reads the host page's title. Other titles (such as the engine page's
    /// own while it loads) are not status reports.
    pub fn from_title(title: &str) -> Option<Self> {
        serde_json::from_str(title.strip_prefix(TITLE_PREFIX)?).ok()
    }
}

/// Finds the prepared extension: next to the installed app, or in the source
/// tree during development.
pub fn locate(app: &AppHandle) -> Hachidori {
    let none = Hachidori {
        extensions_dir: None,
        prepared: None,
    };
    if !cfg!(windows) {
        return none;
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(resources) = app.path().resource_dir() {
        roots.push(resources);
    }
    if cfg!(debug_assertions) {
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    }
    for root in roots {
        let extensions = root.join("extensions");
        if extensions.join("hachidori").join("manifest.json").is_file() {
            let prepared = std::fs::read_to_string(root.join("hachidori.json"))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok());
            return Hachidori {
                extensions_dir: Some(extensions),
                prepared,
            };
        }
    }
    none
}

pub fn page_url(prepared: &Prepared, page: &str) -> String {
    format!("chrome-extension://{}/{}", prepared.id, page)
}

#[cfg(test)]
mod tests {
    use super::HostStatus;

    #[test]
    fn reads_host_titles() {
        let status = HostStatus::from_title(
            r#"telly-hachidori:{"state":"link-failed","detail":"No shared Hachidori answered.","host":""}"#,
        )
        .expect("a status title");
        assert_eq!(status.state, "link-failed");
        assert_eq!(status.detail, "No shared Hachidori answered.");
        assert_eq!(
            HostStatus::from_title(r#"telly-hachidori:{"state":"starting"}"#).map(|s| s.state),
            Some("starting".into())
        );
        assert!(HostStatus::from_title("hoshidicts engine").is_none());
    }
}
