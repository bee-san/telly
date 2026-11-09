//! Noticing the cards you mine while the player is open, so Telly can add the
//! line's moving picture and audio to them (see cards.rs). Two ways in:
//!
//! - Anki Beacon, an Anki add-on (github.com/bpwhelan/AnkiBeacon), posts each
//!   note Anki adds to the addresses in its config. Telly listens for it at
//!   `address()`, on this computer only. While Beacon's heartbeat keeps
//!   coming, that's all it takes.
//! - Otherwise, while the player is open, Telly asks
//!   AnkiConnect for today's notes every POLL_EVERY and looks for new ones.
//!
//! Either way only notes made in the last couple of minutes count, a few at a
//! time: notes that arrive by sync or import were made earlier, or come in bulk.

use crate::anki;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Beside Hachidori Relay's 8771 and Gromit Reader's 8772.
pub const PORT: u16 = 8773;
/// The path GameSentenceMiner uses for Beacon, so its instructions fit.
pub const PATH: &str = "/anki/events";
const POLL_EVERY: Duration = Duration::from_millis(2000);
/// How often the listener checks whether it should be running.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// After the port was taken, how long before trying it again.
const RETRY_LISTEN: Duration = Duration::from_secs(10);
/// A note made longer ago than this wasn't just mined.
const RECENT_MS: i64 = 120_000;
/// More new notes than this at once is an import or a sync, not mining.
const MAX_AT_ONCE: usize = 5;
/// The largest event accepted (Beacon's "note" payload mode includes the fields).
const MAX_BODY: u64 = 1 << 20;
/// Beacon's heartbeat counts as current for this long at least, or three of its intervals.
const HEARTBEAT_GRACE: Duration = Duration::from_secs(30);

/// Where Anki Beacon should post its events.
pub fn address() -> String {
    format!("http://127.0.0.1:{PORT}{PATH}")
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MiningStatus {
    /// Where Anki Beacon should post.
    pub address: String,
    /// Telly is listening there.
    pub listening: bool,
    /// Why it couldn't (the port is taken, say).
    pub listen_error: Option<String>,
    /// Beacon's heartbeat is current, so new notes arrive without asking Anki.
    pub beacon: bool,
}

#[derive(Default)]
struct State {
    /// The newest note seen since watching began; None until the first look.
    newest: Option<i64>,
    /// Notes already handed on (Beacon and polling can both report one).
    seen: HashSet<i64>,
    /// When Beacon's last heartbeat came, and how often it says they come.
    heartbeat: Option<(Instant, Duration)>,
    listening: bool,
    listen_error: Option<String>,
}

impl State {
    fn beacon(&self) -> bool {
        self.heartbeat.is_some_and(|(at, every)| at.elapsed() < (every * 3).max(HEARTBEAT_GRACE))
    }
}

type Enabled = Arc<dyn Fn() -> bool + Send + Sync>;
type Found = Arc<dyn Fn(Vec<i64>) + Send + Sync>;

pub struct Mining {
    /// The player is open and wants new notes.
    watching: AtomicBool,
    state: Mutex<State>,
}

impl Mining {
    /// Listens for Beacon and polls AnkiConnect (at `url()`) while `enabled()`;
    /// `found` gets the notes just mined while the player is watching.
    pub fn start(
        enabled: impl Fn() -> bool + Send + Sync + 'static,
        url: impl Fn() -> String + Send + 'static,
        found: impl Fn(Vec<i64>) + Send + Sync + 'static,
    ) -> Arc<Mining> {
        let mining = Arc::new(Mining {
            watching: AtomicBool::new(false),
            state: Mutex::new(State::default()),
        });
        let enabled: Enabled = Arc::new(enabled);
        let found: Found = Arc::new(found);
        {
            let (mining, enabled, found) = (mining.clone(), enabled.clone(), found.clone());
            std::thread::spawn(move || mining.listen(&enabled, &found));
        }
        {
            let mining = mining.clone();
            std::thread::spawn(move || mining.poll(&enabled, &url, &found));
        }
        mining
    }

    /// The player starts (or stops) wanting new notes. Each start begins afresh,
    /// so notes from before it aren't news.
    pub fn watch(&self, on: bool) {
        let was = self.watching.swap(on, Ordering::SeqCst);
        if on && !was {
            if let Ok(mut s) = self.state.lock() {
                s.newest = None;
            }
        }
    }

    pub fn status(&self) -> MiningStatus {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        MiningStatus {
            address: address(),
            listening: s.listening,
            listen_error: s.listen_error.clone(),
            beacon: s.beacon(),
        }
    }

    /// Hands on the notes among `ids` that were just mined and aren't news already.
    fn deliver(&self, ids: impl IntoIterator<Item = i64>, found: &Found) {
        let now = now_ms();
        let fresh = {
            let Ok(mut s) = self.state.lock() else { return };
            if s.seen.len() > 2000 {
                // `newest` still keeps polling from reporting these again.
                s.seen.clear();
            }
            let mut fresh: Vec<i64> = ids.into_iter().filter(|id| !s.seen.contains(id)).collect();
            fresh.sort_unstable();
            fresh.dedup();
            for &id in &fresh {
                s.seen.insert(id);
                s.newest = Some(s.newest.map_or(id, |n| n.max(id)));
            }
            fresh.retain(|&id| now - id < RECENT_MS);
            fresh
        };
        if !fresh.is_empty() && fresh.len() <= MAX_AT_ONCE && self.watching.load(Ordering::SeqCst) {
            found(fresh);
        }
    }

    /// While the player is watching and Beacon is quiet, asks Anki for today's notes.
    fn poll(&self, enabled: &Enabled, url: &dyn Fn() -> String, found: &Found) {
        loop {
            std::thread::sleep(POLL_EVERY);
            if !self.watching.load(Ordering::SeqCst) || self.state.lock().is_ok_and(|s| s.beacon()) || !enabled() {
                continue;
            }
            // Anki closed: try again next time.
            let Ok(ids) = anki::notes_added_today(&url()) else { continue };
            let newest = self.state.lock().ok().and_then(|s| s.newest);
            match newest {
                Some(newest) => self.deliver(ids.into_iter().filter(|&id| id > newest), found),
                // The first look since watching began: what's there now isn't news.
                None => {
                    if let Ok(mut s) = self.state.lock() {
                        s.newest = Some(ids.into_iter().max().unwrap_or(0));
                    }
                }
            }
        }
    }

    fn set_listening(&self, listening: bool, error: Option<String>) {
        if let Ok(mut s) = self.state.lock() {
            s.listening = listening;
            s.listen_error = error;
            if !listening {
                s.heartbeat = None;
            }
        }
    }

    /// Listens for Beacon at `address()` while `enabled()`.
    fn listen(&self, enabled: &Enabled, found: &Found) {
        let mut server: Option<tiny_http::Server> = None;
        let mut checked: Option<Instant> = None;
        let mut failed: Option<Instant> = None;
        loop {
            if checked.is_none_or(|c| c.elapsed() >= CHECK_EVERY) {
                checked = Some(Instant::now());
                let want = enabled();
                if want && server.is_none() && failed.is_none_or(|f| f.elapsed() >= RETRY_LISTEN) {
                    match tiny_http::Server::http(("127.0.0.1", PORT)) {
                        Ok(s) => {
                            server = Some(s);
                            failed = None;
                            self.set_listening(true, None);
                        }
                        Err(e) => {
                            failed = Some(Instant::now());
                            self.set_listening(false, Some(format!("Port {PORT} is taken by another program ({e})")));
                        }
                    }
                } else if !want && (server.is_some() || failed.is_some()) {
                    server = None;
                    failed = None;
                    self.set_listening(false, None);
                }
            }
            match &server {
                Some(s) => match s.recv_timeout(Duration::from_millis(500)) {
                    Ok(Some(request)) => self.answer(request, found),
                    Ok(None) => {}
                    Err(e) => {
                        server = None;
                        failed = Some(Instant::now());
                        self.set_listening(false, Some(format!("Stopped listening ({e})")));
                    }
                },
                None => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    }

    /// Answers one of Beacon's posts: a heartbeat or a new note.
    fn answer(&self, mut request: tiny_http::Request, found: &Found) {
        let header = |name: &'static str| {
            request
                .headers()
                .iter()
                .find(|h| h.field.equiv(name))
                .map(|h| h.value.as_str().to_ascii_lowercase())
        };
        // A website open in a browser can post here too, but browsers name the
        // page in Origin, and can't send JSON at all without asking first
        // (which this never allows). Beacon does neither.
        let from_browser = header("Origin").is_some();
        let json = header("Content-Type").is_some_and(|t| t.starts_with("application/json"));
        let ours = request.method() == &tiny_http::Method::Post && request.url().split('?').next() == Some(PATH);
        let code = if from_browser {
            403
        } else if !ours {
            404
        } else if !json {
            415
        } else {
            let mut body = Vec::new();
            let read = request.as_reader().take(MAX_BODY).read_to_end(&mut body);
            match read.ok().and_then(|_| serde_json::from_slice::<Value>(&body).ok()) {
                Some(event) => match event.get("event").and_then(Value::as_str) {
                    Some("heartbeat") => {
                        let every = event
                            .get("heartbeat_interval_seconds")
                            .and_then(Value::as_f64)
                            .filter(|s| s.is_finite() && *s > 0.0)
                            .unwrap_or(10.0)
                            .min(3600.0);
                        if let Ok(mut s) = self.state.lock() {
                            s.heartbeat = Some((Instant::now(), Duration::from_secs_f64(every)));
                        }
                        204
                    }
                    Some("note_added") => match event.get("note_id").and_then(Value::as_i64) {
                        Some(id) => {
                            // Answer first, so Anki isn't kept waiting on the picture.
                            let _ = request.respond(tiny_http::Response::empty(202));
                            self.deliver([id], found);
                            return;
                        }
                        None => 400,
                    },
                    _ => 400,
                },
                None => 400,
            }
        };
        let _ = request.respond(tiny_http::Response::empty(code));
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn collector() -> (Found, Arc<StdMutex<Vec<Vec<i64>>>>) {
        let got = Arc::new(StdMutex::new(Vec::new()));
        let sink = got.clone();
        (Arc::new(move |ids| sink.lock().unwrap().push(ids)), got)
    }

    fn mining() -> Mining {
        Mining {
            watching: AtomicBool::new(true),
            state: Mutex::new(State::default()),
        }
    }

    #[test]
    fn hands_on_a_new_note_once() {
        let (found, got) = collector();
        let m = mining();
        let id = now_ms();
        m.deliver([id], &found);
        m.deliver([id], &found);
        assert_eq!(*got.lock().unwrap(), vec![vec![id]]);
    }

    #[test]
    fn skips_old_notes_and_bulk() {
        let (found, got) = collector();
        let m = mining();
        m.deliver([now_ms() - RECENT_MS - 1000], &found);
        let now = now_ms();
        m.deliver((0..10).map(|i| now - i), &found);
        assert!(got.lock().unwrap().is_empty());
    }

    #[test]
    fn says_nothing_while_no_page_watches() {
        let (found, got) = collector();
        let m = mining();
        m.watch(false);
        m.deliver([now_ms()], &found);
        assert!(got.lock().unwrap().is_empty());
    }

    #[test]
    fn heartbeat_lasts_three_intervals_or_thirty_seconds() {
        let mut s = State::default();
        assert!(!s.beacon());
        s.heartbeat = Some((Instant::now(), Duration::from_secs(10)));
        assert!(s.beacon());
    }
}
