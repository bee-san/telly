//! Telly's Tauri app: the window, the `media://` protocol the player and the
//! library load from, and Hachidori. Everything else the UI asks for goes
//! through `call`, into backend.rs, which the development server shares.

pub mod anilist;
pub mod anki;
pub mod ask;
pub mod backend;
pub mod cards;
pub mod claude;
pub mod clip;
pub mod db;
mod hachidori;
pub mod jimaku;
pub mod jobs;
pub mod log;
pub mod matching;
pub mod media;
pub mod mining;
pub mod names;
pub mod premine;
pub mod relay;
pub mod scan;
pub mod subs;
pub mod sync;
pub mod tools;
pub mod util;

use backend::{Backend, Dirs};
use hachidori::HostStatus;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{http, AppHandle, Emitter, Manager, State, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent};

struct HachidoriState {
    hachidori: hachidori::Hachidori,
    /// What the engine host page last reported.
    status: Mutex<Option<HostStatus>>,
    /// Counts hachidori_start calls, so a start's timeout can tell whether it is still the latest.
    starts: AtomicU64,
}

/// How long the host page has to report before Settings says something went wrong.
const HOST_TIMEOUT: Duration = Duration::from_secs(20);
/// Setting: link Telly's Hachidori to the one in your browser.
const LINK_SETTING: &str = "hachidori.link";
/// Setting: run the built-in relay (when Anki isn't running).
const RELAY_SETTING: &str = "hachidori.relay";
const HOST_WINDOW: &str = "hachidori-host";
const SETTINGS_WINDOW: &str = "hachidori-settings";

struct RelayStatus(Arc<Mutex<relay::RelayState>>);

#[tauri::command]
fn relay_status(status: State<'_, RelayStatus>) -> relay::RelayState {
    status.0.lock().map(|s| s.clone()).unwrap_or(relay::RelayState::Off)
}

/// Answers the UI's requests (see backend.rs), off the main thread, since some
/// (scanning a folder, AniList) take a while.
#[tauri::command]
async fn call(backend: State<'_, Arc<Backend>>, cmd: String, args: Value) -> Result<Value, String> {
    let backend = backend.inner().clone();
    tauri::async_runtime::spawn_blocking(move || backend.call(&cmd, args).map_err(|e| format!("{e:#}")))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HachidoriInfo {
    /// False where Chrome extensions cannot load (anywhere but WebView2).
    supported: bool,
    prepared: bool,
    id: Option<String>,
    version: Option<String>,
    link: bool,
    status: Option<HostStatus>,
}

fn link_wanted(backend: &Backend) -> bool {
    backend.db.setting_bool(LINK_SETTING, true)
}

#[tauri::command]
fn hachidori_info(backend: State<'_, Arc<Backend>>, state: State<'_, HachidoriState>) -> HachidoriInfo {
    let p = state.hachidori.prepared.as_ref();
    HachidoriInfo {
        supported: cfg!(windows),
        prepared: p.is_some(),
        id: p.map(|p| p.id.clone()),
        version: p.map(|p| p.version.clone()),
        link: link_wanted(&backend),
        status: state.status.lock().ok().and_then(|s| s.clone()),
    }
}

fn set_host_status(app: &AppHandle, status: HostStatus) {
    let state = app.state::<HachidoriState>();
    if let Ok(mut current) = state.status.lock() {
        if current.as_ref() == Some(&status) {
            return;
        }
        *current = Some(status.clone());
    }
    let _ = app.emit_to("main", "hachidori-status", status);
}

/// WebView2's browser arguments, for every window (WebView2 refuses windows that
/// share a data folder but not their arguments). Tauri passes its own, which
/// makes WebView2 ignore WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, so when that's
/// set it's added to Tauri's here (`--remote-debugging-port=9222` lets DevTools drive the app).
fn browser_args() -> Option<String> {
    let extra = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").ok().filter(|a| !a.trim().is_empty())?;
    Some(format!("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required {extra}"))
}

/// Opens (or reloads) the hidden window that keeps Hachidori's engine running
/// and linked. The UI calls this once the main page has loaded, and again when
/// the link setting changes.
///
/// Async, like every command that builds a window: a synchronous command runs
/// on the main thread, where building a WebView2 window deadlocks (wry#583).
#[tauri::command]
async fn hachidori_start(app: AppHandle, backend: State<'_, Arc<Backend>>, state: State<'_, HachidoriState>) -> Result<bool, String> {
    let Some(prepared) = &state.hachidori.prepared else {
        return Ok(false);
    };
    let page = format!("telly/host.html?link={}", if link_wanted(&backend) { 1 } else { 0 });
    let url = hachidori::page_url(prepared, &page).parse::<Url>().map_err(|e| e.to_string())?;
    set_host_status(&app, HostStatus { state: "starting".into(), detail: String::new(), host: String::new() });
    let start = state.starts.fetch_add(1, Ordering::SeqCst) + 1;
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(HOST_TIMEOUT);
        let state = handle.state::<HachidoriState>();
        let still_starting = state.starts.load(Ordering::SeqCst) == start
            && state.status.lock().is_ok_and(|s| s.as_ref().is_some_and(|s| s.state == "starting"));
        if still_starting {
            set_host_status(
                &handle,
                HostStatus {
                    state: "error".into(),
                    detail: "Hachidori’s engine page never opened. Try Restart Hachidori, or restart Telly.".into(),
                    host: String::new(),
                },
            );
        }
    });
    if let Some(window) = app.get_webview_window(HOST_WINDOW) {
        window.navigate(url).map_err(|e| e.to_string())?;
        return Ok(true);
    }
    let mut host = WebviewWindowBuilder::new(&app, HOST_WINDOW, WebviewUrl::External(url))
        .title("Hachidori engine")
        .visible(false)
        .skip_taskbar(true)
        .browser_extensions_enabled(true)
        .on_document_title_changed(|window, title| {
            if let Some(status) = HostStatus::from_title(&title) {
                set_host_status(window.app_handle(), status);
            }
        });
    if let Some(args) = browser_args() {
        host = host.additional_browser_args(&args);
    }
    host.build().map_err(|e| e.to_string())?;
    Ok(true)
}

/// Hachidori's own settings page, in a normal window.
#[tauri::command]
async fn open_hachidori_settings(app: AppHandle, state: State<'_, HachidoriState>) -> Result<(), String> {
    let Some(prepared) = &state.hachidori.prepared else {
        return Err(if cfg!(windows) {
            "Hachidori is not set up yet. Run `npm run prepare:hachidori`, then restart Telly.".into()
        } else {
            "Hachidori needs WebView2, so it only runs on Windows.".into()
        });
    };
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        window.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let url = hachidori::page_url(prepared, "settings.html").parse::<Url>().map_err(|e| e.to_string())?;
    let mut window = WebviewWindowBuilder::new(&app, SETTINGS_WINDOW, WebviewUrl::External(url))
        .title("Hachidori settings — Telly")
        .inner_size(1100.0, 820.0)
        .browser_extensions_enabled(true);
    if let Some(args) = browser_args() {
        window = window.additional_browser_args(&args);
    }
    window.build().map_err(|e| e.to_string())?;
    Ok(())
}

/// Answers a `media://` request: an episode, a cover or a clip (see Backend::serve).
fn serve_media(backend: &Backend, path: &str, range: Option<&str>) -> http::Response<Vec<u8>> {
    let response = http::Response::builder()
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Expose-Headers", "Content-Range, Accept-Ranges");
    let built = match backend.serve(path, range) {
        Ok(served) => {
            let response = response.header(http::header::CONTENT_TYPE, served.mime).header("Accept-Ranges", "bytes");
            match served.range {
                Some((start, end, total)) => response
                    .status(http::StatusCode::PARTIAL_CONTENT)
                    .header("Content-Range", format!("bytes {start}-{end}/{total}"))
                    .body(served.bytes),
                None => response.body(served.bytes),
            }
        }
        Err(e) => response
            .status(http::StatusCode::NOT_FOUND)
            .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(format!("{e:#}").into_bytes()),
    };
    built.expect("valid response")
}

/// Telly's data folders: %LOCALAPPDATA%\Telly and %APPDATA%\Telly, or
/// TELLY_DATA_DIR for both (development and tests).
fn data_dirs(app: &tauri::App) -> Result<Dirs, Box<dyn std::error::Error>> {
    if let Some(dir) = std::env::var_os("TELLY_DATA_DIR") {
        let dir = PathBuf::from(dir);
        return Ok(Dirs { local: dir.clone(), roaming: dir });
    }
    Ok(Dirs { local: app.path().local_data_dir()?.join("Telly"), roaming: app.path().data_dir()?.join("Telly") })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // Answered off the main thread: a video range is a disk read.
        .register_asynchronous_uri_scheme_protocol("media", |ctx, request, responder| {
            let backend = ctx.app_handle().state::<Arc<Backend>>().inner().clone();
            let path = request.uri().path().to_string();
            let query = request.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
            let range = request.headers().get(http::header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
            tauri::async_runtime::spawn_blocking(move || responder.respond(serve_media(&backend, &format!("{path}{query}"), range.as_deref())));
        })
        .setup(|app| {
            let backend = Arc::new(Backend::open(data_dirs(app)?)?);
            backend.start();
            backend.start_mining();
            let for_relay = backend.clone();
            app.manage(RelayStatus(relay::supervise(move || {
                for_relay.db.setting_bool(RELAY_SETTING, true) && for_relay.db.setting_bool(LINK_SETTING, true)
            })));
            app.manage(backend);

            let hachidori = hachidori::locate(app.handle());
            let mut main = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Telly")
                .inner_size(1280.0, 820.0)
                .min_inner_size(960.0, 600.0)
                .decorations(false)
                .browser_extensions_enabled(cfg!(windows));
            if let Some(dir) = &hachidori.extensions_dir {
                main = main.extensions_path(dir);
            }
            if let Some(args) = browser_args() {
                main = main.additional_browser_args(&args);
            }
            main.build()?;
            app.manage(HachidoriState { hachidori, status: Mutex::new(None), starts: AtomicU64::new(0) });
            Ok(())
        })
        // The hidden Hachidori windows would otherwise keep Telly running after its window closes.
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, WindowEvent::Destroyed) {
                window.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![call, hachidori_info, hachidori_start, open_hachidori_settings, relay_status])
        .run(tauri::generate_context!())
        .expect("error while running Telly");
}
