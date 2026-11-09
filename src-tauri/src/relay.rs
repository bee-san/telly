//! A built-in Hachidori sharing relay, so linking to the Hachidori in your
//! browser doesn't need Anki open. It is a port of the relay that the
//! Hachidori Relay Anki add-on runs (server.py): the sharing Hachidori (the
//! host) connects to ws://127.0.0.1:8771/host, linked ones to /link, and text
//! frames are forwarded between them unread. It listens on this computer only.
//!
//! Anki's add-on uses the same port, so this relay steps aside for it: it
//! runs only while Anki isn't running, and closes (freeing the port, which
//! the add-on retries every 10 s) as soon as Anki starts. Both Hachidoris
//! reconnect to whichever relay is there.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket};

pub const PORT: u16 = 8771;
const PING_EVERY: Duration = Duration::from_secs(20);
/// Hachidori in Chrome and Edge, and in Firefox (as the add-on accepts from relay 0.0.6).
const EXTENSION_ORIGINS: [&str; 2] = ["chrome-extension://", "moz-extension://"];
const HOSHIDICTS_ORIGIN: &str = "hoshi://hoshidicts";

enum Out {
    Text(String),
    Close,
}

struct Peer {
    /// Which connection this is, so a closing host only clears itself.
    id: u64,
    tx: Sender<Out>,
}

#[derive(Default)]
struct State {
    host: Option<Peer>,
    clients: HashMap<String, Peer>,
    next: u64,
    connections: u64,
}

/// What the relay is doing, for Settings ("Off", "Running", "Anki", "PortBusy").
#[derive(Serialize, Clone, Debug, PartialEq)]
pub enum RelayState {
    /// Turned off in Settings.
    Off,
    /// Relaying on 127.0.0.1:8771.
    Running,
    /// Anki is running, so its Hachidori Relay add-on relays instead.
    Anki,
    /// Something else holds the port.
    PortBusy,
}

pub struct Relay {
    state: Mutex<State>,
    stop: AtomicBool,
    /// Where it listens, for the host's `listening` frame.
    port: u16,
}

fn send(peer: &Peer, frame: &Value) {
    let _ = peer.tx.send(Out::Text(frame.to_string()));
}

impl Relay {
    fn to_host(&self, state: &State, frame: Value) {
        if let Some(host) = &state.host {
            send(host, &frame);
        }
    }

    /// One frame from the host: text for one or every linked Hachidori, a close, or the network switch.
    fn host_frame(&self, text: &str) {
        let Ok(frame) = serde_json::from_str::<Value>(text) else { return };
        let kind = frame.get("kind").and_then(Value::as_str).unwrap_or("");
        let state = self.state.lock().expect("relay state");
        match kind {
            // Telly's relay stays on this computer; Anki's add-on can open it to the network.
            "network" => {
                let mut reply = json!({ "kind": "network", "enabled": false, "addresses": [] });
                if frame.get("enabled") == Some(&Value::Bool(true)) {
                    reply["error"] = json!("Telly's relay only listens on this computer. Open Anki with the Hachidori Relay add-on to share with your other computers.");
                }
                self.to_host(&state, reply)
            }
            "broadcast" => {
                let text = frame.get("text").map(|t| t.as_str().map(str::to_string).unwrap_or_else(|| t.to_string())).unwrap_or_default();
                for client in state.clients.values() {
                    let _ = client.tx.send(Out::Text(text.clone()));
                }
            }
            "send" | "close" => {
                let Some(client) = frame.get("clientId").and_then(Value::as_str).and_then(|id| state.clients.get(id)) else { return };
                if kind == "send" {
                    let text = frame.get("text").map(|t| t.as_str().map(str::to_string).unwrap_or_else(|| t.to_string())).unwrap_or_default();
                    let _ = client.tx.send(Out::Text(text));
                } else {
                    let _ = client.tx.send(Out::Close);
                }
            }
            _ => {}
        }
    }

    fn ping(&self) {
        let ping = json!({ "kind": "ping" });
        let state = self.state.lock().expect("relay state");
        if let Some(host) = &state.host {
            send(host, &ping);
        }
        for client in state.clients.values() {
            send(client, &ping);
        }
    }

    /// Closes every connection.
    fn close_all(&self) {
        let mut state = self.state.lock().expect("relay state");
        if let Some(host) = state.host.take() {
            let _ = host.tx.send(Out::Close);
        }
        for (_, client) in state.clients.drain() {
            let _ = client.tx.send(Out::Close);
        }
    }
}

/// Why a handshake is turned away (as server.py's `refusal`), or None to accept it.
fn refusal(path: &str, origin: &str, has_host: bool) -> Option<u16> {
    if path != "/host" && path != "/link" {
        return Some(404);
    }
    let extension = EXTENSION_ORIGINS.iter().any(|o| origin.starts_with(o));
    if !extension && (path != "/link" || origin != HOSHIDICTS_ORIGIN) {
        return Some(403);
    }
    if path == "/link" && !has_host {
        return Some(503);
    }
    None
}

fn serve_connection(relay: Arc<Relay>, stream: TcpStream, peer: SocketAddr) {
    let _ = stream.set_nodelay(true);
    // A peer that connects and never finishes its handshake doesn't keep a thread forever.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut path = String::new();
    let mut origin = String::new();
    let has_host = relay.state.lock().map(|s| s.host.is_some()).unwrap_or(false);
    let handshake = tungstenite::accept_hdr(stream, |request: &Request, response: Response| {
        path = request.uri().path().to_string();
        origin = request.headers().get("origin").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        match refusal(&path, &origin, has_host) {
            None => Ok(response),
            Some(status) => {
                let mut refused = ErrorResponse::new(None);
                *refused.status_mut() = tungstenite::http::StatusCode::from_u16(status).expect("valid status");
                Err(refused)
            }
        }
    });
    let Ok(mut ws) = handshake else { return };
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(40)));
    let (tx, rx) = mpsc::channel();
    // Register as the host or as a linked client. Refusals are sent after the lock is let go.
    enum Joined {
        Host,
        Client(String),
        Refused(Option<&'static str>),
    }
    let connection: u64;
    let joined = {
        let mut state = relay.state.lock().expect("relay state");
        state.connections += 1;
        connection = state.connections;
        if path == "/host" {
            if state.host.is_some() {
                Joined::Refused(Some("Another browser on this computer is already sharing."))
            } else {
                state.host = Some(Peer { id: connection, tx: tx.clone() });
                let _ = tx.send(Out::Text(json!({ "kind": "listening", "port": relay.port }).to_string()));
                Joined::Host
            }
        } else if state.host.is_none() {
            Joined::Refused(None)
        } else {
            state.next += 1;
            let id = format!("client-{}", state.next);
            state.clients.insert(id.clone(), Peer { id: connection, tx: tx.clone() });
            relay.to_host(&state, json!({ "kind": "client-open", "clientId": id, "origin": origin, "address": peer.ip().to_string() }));
            Joined::Client(id)
        }
    };
    let client_id = match joined {
        Joined::Host => None,
        Joined::Client(id) => Some(id),
        Joined::Refused(why) => {
            if let Some(error) = why {
                let _ = ws.send(Message::text(json!({ "kind": "listen-failed", "error": error }).to_string()));
            }
            let _ = ws.close(None);
            return;
        }
    };
    pump(&relay, &mut ws, &rx, client_id.as_deref());
    // Gone: tell the host, or as the host, drop every linked client.
    let mut state = relay.state.lock().expect("relay state");
    match &client_id {
        Some(id) => {
            if state.clients.remove(id).is_some() {
                relay.to_host(&state, json!({ "kind": "client-close", "clientId": id }));
            }
        }
        None => {
            if state.host.as_ref().is_some_and(|h| h.id == connection) {
                state.host = None;
                for (_, client) in state.clients.drain() {
                    let _ = client.tx.send(Out::Close);
                }
            }
        }
    }
}

/// Moves frames both ways until either side closes.
fn pump(relay: &Relay, ws: &mut WebSocket<TcpStream>, rx: &Receiver<Out>, client: Option<&str>) {
    loop {
        match ws.read() {
            Ok(Message::Text(text)) => match client {
                Some(id) => {
                    let state = relay.state.lock().expect("relay state");
                    relay.to_host(&state, json!({ "kind": "client-text", "clientId": id, "text": text.as_str() }));
                }
                None => relay.host_frame(text.as_str()),
            },
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        while let Ok(out) = rx.try_recv() {
            match out {
                Out::Text(text) => {
                    if ws.send(Message::text(text)).is_err() {
                        return;
                    }
                }
                Out::Close => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return;
                }
            }
        }
        if relay.stop.load(Ordering::SeqCst) {
            let _ = ws.close(None);
            return;
        }
    }
}

/// Relays on 127.0.0.1:port until the process ends, outside the app
/// (src/relayserver.rs, for Hachidori's own sharing tests). False when the
/// port is taken.
pub fn serve(port: u16) -> bool {
    let Some(_relay) = start(port) else { return false };
    loop {
        std::thread::park();
    }
}

/// Starts relaying on 127.0.0.1:port; None when the port is taken.
fn start(port: u16) -> Option<Arc<Relay>> {
    let listener = TcpListener::bind(("127.0.0.1", port)).ok()?;
    listener.set_nonblocking(true).ok()?;
    let relay = Arc::new(Relay { state: Mutex::new(State::default()), stop: AtomicBool::new(false), port });
    let accepting = relay.clone();
    std::thread::spawn(move || {
        let mut last_ping = Instant::now();
        while !accepting.stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, peer)) => {
                    let _ = stream.set_nonblocking(false);
                    let relay = accepting.clone();
                    std::thread::spawn(move || serve_connection(relay, stream, peer));
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
            if last_ping.elapsed() >= PING_EVERY {
                accepting.ping();
                last_ping = Instant::now();
            }
        }
        // Dropping the listener frees the port.
    });
    Some(relay)
}

/// Whether a program or command line is Anki: `anki.exe`, or Python running Anki
/// (`python /usr/bin/anki`, or `pythonw.exe -m aqt` from Anki's newer launcher).
fn is_anki(name: &str, exe: &str, cmd: &[String]) -> bool {
    let anki = |s: &str| {
        let file = s.rsplit(['/', '\\']).next().unwrap_or(s).to_ascii_lowercase();
        matches!(file.as_str(), "anki" | "anki.exe" | "anki-console.exe" | "aqt")
    };
    if anki(name) || anki(exe) {
        return true;
    }
    let python = name.to_ascii_lowercase().starts_with("python");
    python && cmd.iter().skip(1).take(3).any(|arg| anki(arg) || arg.contains("aqt.run") || arg.ends_with("/anki/__main__.py"))
}

fn anki_running(system: Option<&mut sysinfo::System>) -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
    let Some(system) = system else { return false };
    let kind = ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet).with_exe(UpdateKind::OnlyIfNotSet);
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    system.processes().values().any(|p| {
        let cmd: Vec<String> = p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let exe = p.exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        is_anki(&p.name().to_string_lossy(), &exe, &cmd)
    })
}

/// Anki's add-on folders: ANKI_BASE's, or the platform's Anki2 folder's.
fn anki_addon_folders() -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Some(base) = std::env::var_os("ANKI_BASE") {
        bases.push(PathBuf::from(base));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            bases.push(PathBuf::from(appdata).join("Anki2"));
        }
    } else if cfg!(target_os = "macos") {
        bases.extend(home.map(|h| h.join("Library/Application Support/Anki2")));
    } else {
        let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".local/share")));
        bases.extend(data.map(|d| d.join("Anki2")));
    }
    bases.into_iter().map(|b| b.join("addons21")).filter(|d| d.is_dir()).collect()
}

/// Whether Anki has the Hachidori Relay add-on, enabled and on the relay port, so it will
/// relay once Telly lets go. Without it, Telly keeps relaying while Anki runs.
fn anki_relays() -> bool {
    relay_addon_in(&anki_addon_folders())
}

fn relay_addon_in(folders: &[PathBuf]) -> bool {
    let read = |path: PathBuf| std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for entry in entries.flatten() {
            let dir = entry.path();
            let (manifest, meta) = (read(dir.join("manifest.json")), read(dir.join("meta.json")));
            let named = |v: &Option<Value>| {
                v.as_ref().is_some_and(|v| v["name"] == "Hachidori Relay" || v["package"] == "hachidori-relay")
            };
            if !(dir.file_name().is_some_and(|n| n == "hachidori-relay") || named(&manifest) || named(&meta)) {
                continue;
            }
            if meta.as_ref().is_some_and(|m| m["disabled"] == true) {
                continue;
            }
            // The user's config (in meta.json) over the add-on's default (config.json).
            let port = meta
                .as_ref()
                .and_then(|m| m["config"]["port"].as_u64())
                .or_else(|| read(dir.join("config.json")).and_then(|c| c["port"].as_u64()))
                .unwrap_or(PORT as u64);
            if port == PORT as u64 {
                return true;
            }
        }
    }
    false
}

/// Keeps the relay running while `enabled()` and Anki isn't, checking every second: Anki's add-on
/// binds the port a few seconds after Anki starts, and warns once if it finds the port taken.
pub fn supervise(enabled: impl Fn() -> bool + Send + 'static) -> Arc<Mutex<RelayState>> {
    let status = Arc::new(Mutex::new(RelayState::Off));
    let shared = status.clone();
    std::thread::spawn(move || {
        let mut system = sysinfo::System::new().ok();
        let mut running: Option<Arc<Relay>> = None;
        loop {
            let want = enabled();
            // Step aside only for an Anki that will relay: one with the add-on, enabled, on this port.
            let anki = want && anki_running(system.as_mut()) && anki_relays();
            let next = if !want {
                RelayState::Off
            } else if anki {
                RelayState::Anki
            } else if running.is_some() {
                RelayState::Running
            } else {
                match start(PORT) {
                    Some(relay) => {
                        running = Some(relay);
                        RelayState::Running
                    }
                    None => RelayState::PortBusy,
                }
            };
            if next != RelayState::Running {
                if let Some(relay) = running.take() {
                    relay.stop.store(true, Ordering::SeqCst);
                    relay.close_all();
                }
            }
            if let Ok(mut s) = shared.lock() {
                *s = next;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host and a linked client through a real relay, as the two Hachidoris use it.
    #[test]
    fn relays_between_host_and_client() {
        use tungstenite::client::IntoClientRequest;
        let port = {
            let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            probe.local_addr().unwrap().port()
        };
        let relay = start(port).expect("relay starts");
        let connect = |path: &str, origin: &str| {
            let mut request = format!("ws://127.0.0.1:{port}{path}").into_client_request().unwrap();
            request.headers_mut().insert("Origin", origin.parse().unwrap());
            let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            tungstenite::client(request, stream)
        };
        let text = |ws: &mut WebSocket<TcpStream>| -> Value {
            loop {
                match ws.read().expect("a frame") {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::String(t.as_str().to_string()));
                        if v.get("kind").and_then(Value::as_str) != Some("ping") {
                            return v;
                        }
                    }
                    _ => continue,
                }
            }
        };
        // No host yet: linking is refused, and web pages are refused outright.
        assert!(connect("/link", "chrome-extension://linked").is_err());
        assert!(connect("/host", "https://example.com").is_err());
        let (mut host, _) = connect("/host", "chrome-extension://host").expect("host connects");
        let listening = text(&mut host);
        assert_eq!((listening["kind"].as_str(), listening["port"].as_u64()), (Some("listening"), Some(port as u64)));
        let (mut client, _) = connect("/link", "chrome-extension://linked").expect("client connects");
        let open = text(&mut host);
        assert_eq!((open["kind"].as_str(), open["origin"].as_str()), (Some("client-open"), Some("chrome-extension://linked")));
        let id = open["clientId"].as_str().unwrap().to_string();
        client.send(Message::text(r#"{"hello":1}"#)).unwrap();
        let got = text(&mut host);
        assert_eq!((got["kind"].as_str(), got["text"].as_str()), (Some("client-text"), Some(r#"{"hello":1}"#)));
        host.send(Message::text(json!({ "kind": "send", "clientId": id, "text": "answer" }).to_string())).unwrap();
        assert_eq!(text(&mut client), Value::String("answer".into()));
        // A second host is turned away.
        let (mut second, _) = connect("/host", "chrome-extension://other").expect("handshake");
        assert_eq!(text(&mut second)["kind"], "listen-failed");
        // The host leaving closes its clients.
        host.close(None).unwrap();
        let _ = host.flush();
        let closed = loop {
            match client.read() {
                Ok(Message::Close(_)) | Err(_) => break true,
                Ok(_) => continue,
            }
        };
        assert!(closed);
        relay.stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn steps_aside_only_for_an_anki_that_relays() {
        let root = std::env::temp_dir().join(format!("telly-addons-{}", std::process::id()));
        let add = |name: &str, files: &[(&str, &str)]| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            for (file, text) in files {
                std::fs::write(dir.join(file), text).unwrap();
            }
        };
        let folders = [root.clone()];
        add("1045800357", &[("meta.json", r#"{"name":"Some Other Add-on"}"#)]);
        assert!(!relay_addon_in(&folders), "no relay add-on");
        add("hachidori-relay", &[("manifest.json", r#"{"package":"hachidori-relay","name":"Hachidori Relay"}"#), ("config.json", r#"{"port":8771}"#)]);
        assert!(relay_addon_in(&folders), "installed from a file");
        add("hachidori-relay", &[("meta.json", r#"{"disabled":true}"#)]);
        assert!(!relay_addon_in(&folders), "disabled");
        add("hachidori-relay", &[("meta.json", r#"{"config":{"port":9000}}"#)]);
        assert!(!relay_addon_in(&folders), "on another port");
        std::fs::remove_dir_all(root.join("hachidori-relay")).unwrap();
        add("2055492159", &[("meta.json", r#"{"name":"Hachidori Relay","disabled":false}"#)]);
        assert!(relay_addon_in(&folders), "from AnkiWeb");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn states_reach_settings_by_their_names() {
        // Settings (src/routes/settings) labels the relay by these exact names.
        for (state, name) in [(RelayState::Off, "Off"), (RelayState::Running, "Running"), (RelayState::Anki, "Anki"), (RelayState::PortBusy, "PortBusy")] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
    }

    #[test]
    fn recognises_anki() {
        let v = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert!(is_anki("anki.exe", r"C:\Users\me\AppData\Local\Programs\Anki\anki.exe", &[]));
        assert!(is_anki("python", "/usr/bin/python3.13", &v(&["/usr/bin/python", "/usr/bin/anki"])));
        assert!(is_anki("pythonw.exe", r"C:\x\pythonw.exe", &v(&["pythonw.exe", "-m", "aqt"])));
        assert!(!is_anki("python", "/usr/bin/python", &v(&["python", "/home/me/ankiweb-scraper.py"])));
        assert!(!is_anki("telly", "/usr/bin/telly", &v(&["telly", "anki.mkv"])));
    }

    #[test]
    fn refuses_like_the_anki_relay() {
        assert_eq!(refusal("/other", "chrome-extension://x", true), Some(404));
        assert_eq!(refusal("/host", "https://evil.example", false), Some(403));
        assert_eq!(refusal("/link", "hoshi://hoshidicts", true), None);
        assert_eq!(refusal("/host", "hoshi://hoshidicts", true), Some(403));
        assert_eq!(refusal("/link", "chrome-extension://abc", false), Some(503));
        assert_eq!(refusal("/host", "chrome-extension://abc", false), None);
        assert_eq!(refusal("/host", "moz-extension://4b1c-uuid", false), None);
    }
}
