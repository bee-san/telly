//! Telly on your phone: the PC lends the phone the episodes you pick, and the
//! phone gives back how far you got.
//!
//! On the PC, Settings → Your phone turns on a small server on the home
//! network (port 8775), which a phone pairs with once by a six-figure code. It
//! lists your shows and their ready episodes, makes each one you pick into an
//! MP4 the phone plays (H.264, its Japanese audio only), and hands it over by
//! byte ranges, so a copy that stops carries on where it stopped. It sends the
//! subtitles of every episode (Previously on tells the story from them), your
//! dictionaries and the settings the phone needs, and takes back where you
//! got to, what you watched and what Claude said.
//!
//! On the phone, Get from PC finds the PC (or is told its address), and the
//! shows you get something of go in the phone's own library under the PC's
//! ids. Watched copies stay until room is wanted for new ones.

use crate::backend::Backend;
use crate::db::{Episode, Show};
use crate::{jobs, media, own_hachidori};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Where the PC listens, for the phone's calls and for its "where are you?".
pub const PORT: u16 = 8775;
/// Setting (PC): whether the PC is lending episodes to a phone.
const SHARE: &str = "phone.share";
/// Setting (PC): the code a phone pairs with.
const CODE: &str = "phone.code";
/// Setting (PC): the phones paired, [{token, name, at}].
const PHONES: &str = "phone.phones";
/// Setting (phone): the PC it's paired with, {address, token, name}.
const PC: &str = "phone.pc";
/// Setting (phone): when progress was last given to the PC (unix ms).
const SENT: &str = "phone.sentAt";
/// Setting (phone): each episode's subtitles as the phone has them, by the PC's stamp.
const LINES: &str = "phone.lines";
/// Setting (phone): each dictionary the phone has, by its revision on the PC.
const DICTIONARIES: &str = "phone.dictionaries";
/// The PC's settings the phone goes by: Explain and Previously on, and cards.
const SHARED_SETTINGS: [&str; 11] = [
    "openrouter.key", "openrouter.model", "claude.language", "anki.noteType", "anki.deck", "clip.padding", "subs.size",
    "anki.field.sentence", "anki.field.picture", "anki.field.audio", "anki.field.info",
];
/// What the phone says to find a PC, and the start of the PC's answer.
const HELLO: &[u8] = b"telly-phone?";
/// Room kept free on the phone beyond a copy's size.
const SPARE: u64 = 300 * 1024 * 1024;

/// What each side keeps while Telly is open.
#[derive(Default)]
pub struct Phone {
    /// PC: the copies for the phone being made, and how each is going.
    making: Mutex<HashMap<i64, Making>>,
    waiting: Mutex<VecDeque<(i64, Plays)>>,
    wake: Condvar,
    sharing: AtomicBool,
    /// Phone: the episodes being got from the PC, and how each is going.
    getting: Mutex<Vec<Getting>>,
    /// Phone: what its WebView said it plays, for the PC to go by.
    plays: Mutex<Plays>,
    get_wake: Condvar,
}

#[derive(Clone, Default)]
struct Making {
    state: &'static str,
    progress: f64,
    error: String,
    /// The file the phone copies: the episode's own (as it is) or one made for the phone.
    file: Option<PathBuf>,
    /// How it was made for the phone (`PhonePlan::name`).
    plan: &'static str,
}

/// What a phone's WebView plays: kinds of video ("h264", "hevc-10") and audio codecs ("aac"), as ffprobe names them.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Plays {
    pub video: Vec<String>,
    pub audio: Vec<String>,
}

impl Default for Plays {
    /// What any phone plays, for one that didn't say.
    fn default() -> Self {
        Plays { video: vec!["h264".into()], audio: ["aac", "mp3", "opus", "flac"].map(String::from).to_vec() }
    }
}

/// How an episode gets to the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhonePlan {
    /// The phone plays the file the PC plays, and it has only the Japanese audio: it's sent as it is.
    AsIs,
    /// The phone plays it, but the file has other audio tracks (a dub) or isn't an MP4: copied into an MP4 with the Japanese audio alone, in seconds.
    /// An MKV plays on Android, but opening one part-way through stalls on its first frame when the phone's hardware decodes it (H.264, HEVC).
    Remux,
    /// The phone plays the picture but not the audio (AC-3, DTS): the picture copied, the Japanese audio converted to AAC.
    Audio,
    /// The phone doesn't play the picture: converted to H.264 on the PC's encoder.
    Video,
}

impl PhonePlan {
    fn name(self) -> &'static str {
        match self {
            PhonePlan::AsIs => "as it is",
            PhonePlan::Remux => "remux",
            PhonePlan::Audio => "audio",
            PhonePlan::Video => "video",
        }
    }
}

/// How the episode in `probe` (its Japanese track `audio`) gets to a phone that plays `plays`.
fn phone_plan(probe: &media::Probe, path: &Path, audio: usize, plays: &Plays) -> PhonePlan {
    let picture = probe.video.as_ref().is_some_and(|v| plays.video.contains(&media::video_kind(v)));
    if !picture {
        return PhonePlan::Video;
    }
    let sound = probe.audio.get(audio).is_none_or(|a| plays.audio.contains(&a.codec));
    if !sound {
        return PhonePlan::Audio;
    }
    let extension = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if probe.audio.len() <= 1 && matches!(extension.as_str(), "mp4" | "m4v") {
        PhonePlan::AsIs
    } else {
        PhonePlan::Remux
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Getting {
    id: i64,
    show: i64,
    state: &'static str,
    progress: f64,
    size: u64,
    error: String,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A token or code nobody can guess from outside: mixed from the clock, the
/// process and the hasher's own random keys.
fn random() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.write_u32(std::process::id());
    h.finish()
}

fn computer_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "Your PC".into())
}

// ---- the PC ---------------------------------------------------------------

/// Where the PC keeps the copies it makes for the phone.
fn phone_dir(backend: &Backend) -> PathBuf {
    backend.media_dir().join("phone")
}

/// The PC's copy of an episode made for the phone.
fn phone_copy(backend: &Backend, id: i64) -> PathBuf {
    phone_dir(backend).join(format!("{id}.mp4"))
}

/// The file the phone copies for an episode: as made or picked for it, else a copy left from before.
fn phone_file(backend: &Backend, id: i64) -> Option<PathBuf> {
    let made = lock(&backend.phone.making).get(&id).cloned().unwrap_or_default();
    if made.state == "ready" {
        return made.file;
    }
    let left = phone_copy(backend, id);
    (made.state.is_empty() && left.is_file()).then_some(left)
}

/// Settings → Your phone: whether it's on, the code to pair with, and the
/// addresses the phone can reach this PC at.
pub fn sharing_state(backend: &Backend) -> Value {
    let on = backend.db.setting_bool(SHARE, false);
    let phones = backend.db.setting(PHONES).ok().flatten().unwrap_or(json!([]));
    let names: Vec<Value> = phones.as_array().into_iter().flatten().map(|p| json!({ "name": p["name"], "at": p["at"] })).collect();
    json!({
        "on": on,
        "listening": backend.phone.sharing.load(Ordering::SeqCst),
        "code": if on { backend.db.setting_str(CODE) } else { String::new() },
        "addresses": if on { addresses() } else { Vec::new() },
        "port": PORT,
        "phones": names,
    })
}

/// This PC's addresses on the home network.
fn addresses() -> Vec<String> {
    #[cfg(not(target_os = "android"))]
    {
        use network_interface::{NetworkInterface, NetworkInterfaceConfig};
        let mut out = Vec::new();
        for interface in NetworkInterface::show().unwrap_or_default() {
            // Virtual networks (Docker's, WSL's and Hyper-V's, a VPN's) aren't where a phone is.
            let name = interface.name.to_lowercase();
            let virtual_name = ["docker", "vethernet", "wsl", "hyper-v", "vmware", "virtualbox", "tailscale", "zerotier"].iter().any(|v| name.contains(v))
                || ["br-", "veth", "virbr", "tun", "tap", "wg", "zt", "utun"].iter().any(|v| name.starts_with(v));
            if virtual_name {
                continue;
            }
            for addr in interface.addr {
                if let std::net::IpAddr::V4(ip) = addr.ip() {
                    if ip.is_private() {
                        out.push(format!("{ip}:{PORT}"));
                    }
                }
            }
        }
        out
    }
    #[cfg(target_os = "android")]
    Vec::new()
}

/// Turns lending to a phone on or off. Turning it on makes a new code.
pub fn set_sharing(backend: &Backend, on: bool) -> Result<Value> {
    backend.db.set_setting(SHARE, &json!(on))?;
    if on {
        backend.db.set_setting(CODE, &json!(format!("{:06}", random() % 1_000_000)))?;
    }
    Ok(sharing_state(backend))
}

/// Forgets every phone paired: they pair again with the code.
pub fn forget_phones(backend: &Backend) -> Result<Value> {
    backend.db.set_setting(PHONES, &json!([]))?;
    Ok(sharing_state(backend))
}

/// Keeps the PC's server running while lending is on, and the worker that
/// makes the phone's copies.
pub fn start_sharing(backend: Arc<Backend>) {
    let for_copies = backend.clone();
    std::thread::Builder::new().name("telly-phone-copies".into()).spawn(move || copy_worker(&for_copies)).ok();
    std::thread::Builder::new()
        .name("telly-phone".into())
        .spawn(move || loop {
            if backend.db.setting_bool(SHARE, false) {
                if let Err(e) = serve(&backend) {
                    eprintln!("phone: the server stopped: {e:#}");
                }
                backend.phone.sharing.store(false, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_secs(3));
        })
        .ok();
}

/// Answers the phone until lending is turned off.
fn serve(backend: &Arc<Backend>) -> Result<()> {
    let server = tiny_http::Server::http(("0.0.0.0", PORT)).map_err(|e| anyhow!("listening on port {PORT}: {e}"))?;
    let udp = UdpSocket::bind(("0.0.0.0", PORT)).context("listening for phones")?;
    udp.set_read_timeout(Some(Duration::from_millis(500)))?;
    backend.phone.sharing.store(true, Ordering::SeqCst);
    let stop = Arc::new(AtomicBool::new(false));
    let (for_udp, stop_udp) = (backend.clone(), stop.clone());
    let finder = std::thread::spawn(move || {
        let mut buffer = [0u8; 64];
        while !stop_udp.load(Ordering::SeqCst) {
            if let Ok((n, from)) = udp.recv_from(&mut buffer) {
                if &buffer[..n] == HELLO {
                    let answer = json!({ "telly": for_udp.db.setting_bool(SHARE, false), "name": computer_name(), "port": PORT });
                    let _ = udp.send_to(answer.to_string().as_bytes(), from);
                }
            }
        }
    });
    while backend.db.setting_bool(SHARE, false) {
        if let Ok(Some(request)) = server.recv_timeout(Duration::from_secs(1)) {
            let backend = backend.clone();
            std::thread::spawn(move || answer(&backend, request));
        }
    }
    stop.store(true, Ordering::SeqCst);
    let _ = finder.join();
    Ok(())
}

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("a valid header")
}

fn json_response(status: u16, value: &Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_data(value.to_string().into_bytes())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
}

fn header_value(headers: &[tiny_http::Header], name: &str) -> Option<String> {
    headers.iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string())
}

fn answer(backend: &Backend, mut request: tiny_http::Request) {
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let path = percent_encoding::percent_decode_str(path).decode_utf8_lossy().into_owned();
    let token = header_value(request.headers(), "Authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_string));
    let range = header_value(request.headers(), "Range");
    let mut body = Vec::new();
    let _ = request.as_reader().take(64 * 1024 * 1024).read_to_end(&mut body);
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if parts == ["pair"] {
        let reply = pair(backend, &body);
        let _ = request.respond(match reply {
            Ok(v) => json_response(200, &v),
            Err(e) => json_response(403, &json!({ "error": format!("{e:#}") })),
        });
        return;
    }
    if !paired(backend, token.as_deref()) {
        let _ = request.respond(json_response(401, &json!({ "error": "This phone isn’t paired with Telly on the PC. Pair it again with the code in Settings → Your phone." })));
        return;
    }
    let reply = match parts.as_slice() {
        ["hello"] => Ok(json!({ "name": computer_name() })),
        ["library"] => library(backend),
        ["make"] => make(backend, &body),
        ["got", id] => id.parse().map_err(|_| anyhow!("bad episode")).map(|id| {
            // Only a copy made for the phone goes: an episode sent as it is is your rip (or Telly's copy of it).
            if let Some(file) = phone_file(backend, id).filter(|f| f.starts_with(phone_dir(backend))) {
                let _ = std::fs::remove_file(file);
            }
            lock(&backend.phone.making).remove(&id);
            json!({})
        }),
        ["progress"] => backend.db.take_progress(&body).map(|()| json!({})),
        ["lines", id] => id.parse().map_err(|_| anyhow!("bad episode")).map(|id| json!(jobs::read_lines(backend, id))),
        ["episode", id] => id.parse().map_err(|_| anyhow!("bad episode")).and_then(|id| episode_extra(backend, id)),
        ["video", id] => {
            let file = id.parse().map_err(|_| anyhow!("bad episode")).and_then(|id| phone_file(backend, id).ok_or_else(|| anyhow!("That episode isn’t ready for the phone")));
            let _ = match file.and_then(|f| {
                let mime = if f.extension().is_some_and(|e| e.eq_ignore_ascii_case("mkv")) { "video/x-matroska" } else { "video/mp4" };
                send_file(&f, range.as_deref(), mime)
            }) {
                Ok(response) => request.respond(response),
                Err(e) => request.respond(json_response(404, &json!({ "error": format!("{e:#}") })).boxed()),
            };
            return;
        }
        ["cover", id] => {
            let small = backend.dirs.covers().join(format!("{id}.small.jpg"));
            let file = if small.is_file() { small } else { backend.dirs.covers().join(format!("{id}.jpg")) };
            let _ = match send_file(&file, None, "image/jpeg") {
                Ok(response) => request.respond(response),
                Err(e) => request.respond(json_response(404, &json!({ "error": format!("{e:#}") })).boxed()),
            };
            return;
        }
        ["dictionary", name] => {
            let _ = match own_hachidori::dictionary_file(backend, name).and_then(|f| send_file(&f, range.as_deref(), "application/zip")) {
                Ok(response) => request.respond(response),
                Err(e) => request.respond(json_response(404, &json!({ "error": format!("{e:#}") })).boxed()),
            };
            return;
        }
        _ => Err(anyhow!("nothing at {path}")),
    };
    let _ = query;
    let _ = request.respond(match reply {
        Ok(v) => json_response(200, &v),
        Err(e) => json_response(400, &json!({ "error": format!("{e:#}") })),
    });
}

/// A file, or the part of it `range` asks for.
fn send_file(path: &Path, range: Option<&str>, mime: &str) -> Result<tiny_http::ResponseBox> {
    let mut file = std::fs::File::open(path).with_context(|| format!("{} isn’t there", path.display()))?;
    let total = file.metadata()?.len();
    let from = range.and_then(|r| r.trim().strip_prefix("bytes=")).and_then(|r| r.split('-').next()?.trim().parse::<u64>().ok());
    let mut headers = vec![header("Content-Type", mime), header("Accept-Ranges", "bytes")];
    match from {
        Some(start) if start < total => {
            file.seek(SeekFrom::Start(start))?;
            headers.push(header("Content-Range", &format!("bytes {start}-{}/{total}", total - 1)));
            Ok(tiny_http::Response::new(206.into(), headers, file, Some((total - start) as usize), None).boxed())
        }
        _ => Ok(tiny_http::Response::new(200.into(), headers, file, Some(total as usize), None).boxed()),
    }
}

fn paired(backend: &Backend, token: Option<&str>) -> bool {
    let Some(token) = token.filter(|t| !t.is_empty()) else { return false };
    backend.db.setting(PHONES).ok().flatten().and_then(|p| p.as_array().cloned()).unwrap_or_default().iter().any(|p| p["token"] == token)
}

fn pair(backend: &Backend, body: &Value) -> Result<Value> {
    let code = backend.db.setting_str(CODE);
    if code.is_empty() || body["code"].as_str().map(str::trim) != Some(code.as_str()) {
        return Err(anyhow!("That isn’t the code Telly shows on the PC (Settings → Your phone)."));
    }
    let token = format!("{:016x}{:016x}", random(), random());
    let mut phones = backend.db.setting(PHONES).ok().flatten().and_then(|p| p.as_array().cloned()).unwrap_or_default();
    let name = body["name"].as_str().unwrap_or("Your phone").to_string();
    phones.retain(|p| p["name"] != name.as_str());
    phones.push(json!({ "token": token, "name": name, "at": crate::util::now_ms() }));
    backend.db.set_setting(PHONES, &json!(phones))?;
    Ok(json!({ "token": token, "name": computer_name() }))
}

/// The stamp of an episode's subtitles: when they were last written.
fn lines_stamp(backend: &Backend, id: i64) -> String {
    std::fs::metadata(jobs::lines_path(backend, id))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis().to_string())
        .unwrap_or_default()
}

/// Every show, each episode with whether it's ready and how its copy for the
/// phone is going; your dictionaries; and the settings the phone goes by.
fn library(backend: &Backend) -> Result<Value> {
    let reached = backend.db.all_reached()?;
    let making = lock(&backend.phone.making).clone();
    let mut shows = Vec::new();
    for show in backend.db.shows()? {
        let episodes: Vec<Value> = backend
            .db
            .episodes(show.id)?
            .into_iter()
            .map(|e| {
                let made = making.get(&e.id).cloned().unwrap_or_default();
                let file = phone_file(backend, e.id);
                let (state, size) = match &file {
                    Some(f) => ("ready", std::fs::metadata(f).map(|m| m.len()).unwrap_or(0)),
                    None if made.state.is_empty() || made.state == "ready" => ("none", 0),
                    None => (made.state, 0),
                };
                let ext = file.as_ref().and_then(|f| f.extension()).map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                json!({
                    "episode": e,
                    "reached": reached.get(&e.id).copied().unwrap_or(0.0),
                    "ready": e.media_state == "ready",
                    "lines": lines_stamp(backend, e.id),
                    "phone": { "state": state, "progress": made.progress, "error": made.error, "size": size, "ext": ext, "plan": made.plan },
                    "rip": e.size,
                })
            })
            .collect();
        shows.push(json!({ "show": show, "episodes": episodes }));
    }
    let dictionaries: Vec<Value> = own_hachidori::folder(backend)
        .map(|dir| own_hachidori::archives(&dir))
        .unwrap_or_default()
        .into_iter()
        .map(|(name, size, revision)| json!({ "name": name, "size": size, "revision": revision }))
        .collect();
    let settings: serde_json::Map<String, Value> =
        SHARED_SETTINGS.iter().filter_map(|key| Some((key.to_string(), backend.db.setting(key).ok().flatten()?))).collect();
    Ok(json!({ "name": computer_name(), "shows": shows, "dictionaries": dictionaries, "settings": settings }))
}

/// What the player needs of an episode beside its video: what ffprobe found
/// (its chapters) and where its opening is.
fn episode_extra(backend: &Backend, id: i64) -> Result<Value> {
    let probe = backend.db.probe(id)?;
    Ok(json!({ "probe": probe, "opening": backend.db.opening(id)?.map(|(s, e)| [s, e]) }))
}

/// Queues the phone's copies of the episodes asked for.
fn make(backend: &Backend, body: &Value) -> Result<Value> {
    let ids: Vec<i64> = body["ids"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
    let plays: Plays = serde_json::from_value(body["plays"].clone()).unwrap_or_default();
    let mut making = lock(&backend.phone.making);
    let mut waiting = lock(&backend.phone.waiting);
    for id in ids {
        let episode = backend.db.episode(id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly on the PC any more"))?;
        if matches!(making.get(&id).map(|m| m.state), Some("waiting" | "working" | "ready")) || (!making.contains_key(&id) && phone_copy(backend, id).is_file()) {
            continue;
        }
        if episode.media_state != "ready" {
            // Getting it ready comes first: it's asked for, and the phone asks again.
            backend.jobs.ask_for(id);
            backend.jobs.poke();
        }
        making.insert(id, Making { state: "waiting", ..Default::default() });
        waiting.push_back((id, plays.clone()));
    }
    backend.phone.wake.notify_all();
    Ok(json!({}))
}

fn copy_worker(backend: &Backend) {
    loop {
        let (id, plays) = {
            let mut waiting = lock(&backend.phone.waiting);
            loop {
                // An episode not ready yet waits its turn behind those that are.
                let ready = waiting.iter().position(|(id, _)| backend.db.episode(*id).ok().flatten().is_some_and(|e| e.media_state == "ready"));
                if let Some(i) = ready {
                    break waiting.remove(i).expect("there");
                }
                waiting = backend.phone.wake.wait_timeout(waiting, Duration::from_secs(5)).unwrap_or_else(|e| e.into_inner()).0;
            }
        };
        let set = |state: &'static str, progress: f64, error: String, file: Option<PathBuf>, plan: &'static str| {
            lock(&backend.phone.making).insert(id, Making { state, progress, error, file, plan });
        };
        set("working", 0.0, String::new(), None, "");
        match make_copy(backend, id, &plays, &|p| set("working", p, String::new(), None, "")) {
            Ok((file, plan)) => set("ready", 1.0, String::new(), Some(file), plan.name()),
            Err(e) => set("failed", 0.0, format!("{e:#}"), None, ""),
        }
    }
}

/// Gets an episode ready for a phone that plays `plays`, and says which
/// file it copies and how it was got ready (`phone_plan`): the file the PC
/// plays itself when the phone plays it too and it's an MP4, else an MP4
/// with the Japanese audio alone, made in seconds, its audio converted to AAC when the phone
/// doesn't play that, and its picture converted only when it can't play that.
fn make_copy(backend: &Backend, id: i64, plays: &Plays, progress: &dyn Fn(f64)) -> Result<(PathBuf, PhonePlan)> {
    let episode = backend.db.episode(id)?.ok_or_else(|| anyhow!("That episode isn’t in Telly any more"))?;
    let tools = backend.tools();
    let ffprobe = tools.ffprobe.clone().ok_or_else(|| anyhow!("Telly on the PC needs ffprobe for this"))?;
    let input = PathBuf::from(if episode.play_path.is_empty() { &episode.path } else { &episode.play_path });
    let probe = media::probe(&ffprobe, &input)?;
    let audio = (episode.audio_index.max(0) as usize).min(probe.audio.len().saturating_sub(1));
    let plan = phone_plan(&probe, &input, audio, plays);
    if plan == PhonePlan::AsIs {
        return Ok((input, plan));
    }
    let ffmpeg = tools.ffmpeg.clone().ok_or_else(|| anyhow!("Telly on the PC needs ffmpeg for this"))?;
    let out = phone_copy(backend, id);
    std::fs::create_dir_all(phone_dir(backend))?;
    let part = out.with_extension("part.mp4");
    let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-y", "-v", "error", "-progress", "pipe:1", "-nostats", "-i"].iter().map(|a| a.to_string()).collect();
    args.push(input.to_string_lossy().into_owned());
    args.extend(["-map".into(), "0:v:0".into()]);
    if !probe.audio.is_empty() {
        args.extend(["-map".into(), format!("0:a:{audio}")]);
    }
    args.extend(["-map_metadata".into(), "-1".into(), "-sn".into(), "-dn".into()]);
    if plan == PhonePlan::Video {
        let video = probe.video.clone().unwrap_or_default();
        args.extend(["-vf".into(), media::video_filter(&video, &tools.encoder)]);
        args.extend(media::encoder_args(&tools.encoder));
    } else {
        args.extend(["-c:v".into(), "copy".into()]);
    }
    let sound = probe.audio.get(audio).is_none_or(|a| plays.audio.contains(&a.codec));
    args.extend(if sound { ["-c:a", "copy"].map(String::from).to_vec() } else { ["-c:a", "aac", "-b:a", "192k", "-ac", "2"].map(String::from).to_vec() });
    // hvc1 is how an MP4 names HEVC for every player; the index goes first, so the phone can start anywhere at once.
    if probe.video.as_ref().is_some_and(|v| v.codec == "hevc") && plan != PhonePlan::Video {
        args.extend(["-tag:v", "hvc1"].map(String::from));
    }
    args.extend(["-movflags", "+faststart", "-f", "mp4"].map(String::from));
    args.push(part.to_string_lossy().into_owned());
    let duration = if plan == PhonePlan::Video { episode.duration } else { 0.0 };
    media::run_ffmpeg(&ffmpeg, &args, duration, &AtomicBool::new(false), progress)?;
    std::fs::rename(&part, &out)?;
    Ok((out, plan))
}

// ---- the phone ------------------------------------------------------------

fn agent(timeout: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(4)))
        .timeout_global(Some(Duration::from_secs(timeout)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// The PC the phone is paired with: its address and token.
fn pc(backend: &Backend) -> Result<(String, String)> {
    let pc = backend.db.setting(PC)?.ok_or_else(|| anyhow!("This phone isn’t paired with a PC yet"))?;
    Ok((pc["address"].as_str().unwrap_or_default().to_string(), pc["token"].as_str().unwrap_or_default().to_string()))
}

fn unreachable(address: &str, e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("The PC isn’t reachable at {address} ({e}). Is Telly open on it, with Settings → Your phone on, and is the phone on the same Wi-Fi?")
}

/// Asks the PC: GET, or POST when there's a body.
fn ask(backend: &Backend, path: &str, body: Option<&Value>) -> Result<Value> {
    let (address, token) = pc(backend)?;
    let url = format!("http://{address}/{path}");
    let agent = agent(60);
    let bearer = format!("Bearer {token}");
    let mut response = match body {
        Some(body) => agent.post(&url).header("Authorization", &bearer).send_json(body),
        None => agent.get(&url).header("Authorization", &bearer).call(),
    }
    .map_err(|e| unreachable(&address, e))?;
    let status = response.status().as_u16();
    let value: Value = response.body_mut().with_config().limit(256 * 1024 * 1024).read_json().unwrap_or(Value::Null);
    if status != 200 {
        return Err(anyhow!("{}", value["error"].as_str().unwrap_or("The PC said no")));
    }
    Ok(value)
}

/// The PCs on the Wi-Fi with Telly lending episodes.
pub fn find() -> Result<Value> {
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(300)))?;
    let _ = socket.send_to(HELLO, ("255.255.255.255", PORT));
    let mut found: Vec<Value> = Vec::new();
    let mut buffer = [0u8; 512];
    let until = std::time::Instant::now() + Duration::from_millis(1500);
    while std::time::Instant::now() < until {
        if let Ok((n, from)) = socket.recv_from(&mut buffer) {
            let Ok(answer) = serde_json::from_slice::<Value>(&buffer[..n]) else { continue };
            let address = format!("{}:{}", from.ip(), answer["port"].as_u64().unwrap_or(PORT as u64));
            if !found.iter().any(|f| f["address"] == address.as_str()) {
                found.push(json!({ "address": address, "name": answer["name"] }));
            }
        }
    }
    Ok(json!(found))
}

/// Pairs the phone with the PC at `address` (a port after it or not) by the code it shows.
pub fn pair_with(backend: &Backend, address: &str, code: &str) -> Result<Value> {
    let address = address.trim().trim_start_matches("http://").trim_end_matches('/').to_string();
    let address = if address.contains(':') { address } else { format!("{address}:{PORT}") };
    let mut response = agent(15)
        .post(&format!("http://{address}/pair"))
        .send_json(json!({ "code": code.trim(), "name": "Telly on your phone" }))
        .map_err(|e| unreachable(&address, e))?;
    let status = response.status().as_u16();
    let value: Value = response.body_mut().read_json().unwrap_or(Value::Null);
    if status != 200 {
        return Err(anyhow!("{}", value["error"].as_str().unwrap_or("The PC said no")));
    }
    backend.db.set_setting(PC, &json!({ "address": address, "token": value["token"], "name": value["name"] }))?;
    Ok(state(backend))
}

pub fn unpair(backend: &Backend) -> Result<Value> {
    backend.db.set_setting(PC, &Value::Null)?;
    Ok(state(backend))
}

/// Where the phone keeps its copies of episodes.
fn copies(backend: &Backend) -> PathBuf {
    backend.dirs.local.join("episodes")
}

/// Where the phone keeps a dictionary's file from the PC until it's installed (lookups.rs).
pub fn dictionaries_dir(backend: &Backend) -> PathBuf {
    backend.dirs.local.join("dictionaries")
}

/// The room left where the phone keeps its copies, in bytes.
fn free_space(dir: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let _ = std::fs::create_dir_all(dir);
        let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return 0 };
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path and a statvfs to fill, both alive for the call.
        if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
            return 0;
        }
        stat.f_bavail as u64 * stat.f_frsize as u64
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        u64::MAX
    }
}

/// The phone's side: the PC it's paired with, the room left, and what's being got.
pub fn state(backend: &Backend) -> Value {
    let pc = backend.db.setting(PC).ok().flatten().filter(|p| !p.is_null());
    json!({
        "pc": pc.map(|p| json!({ "address": p["address"], "name": p["name"] })),
        "free": free_space(&copies(backend)),
        "getting": *lock(&backend.phone.getting),
        "sentAt": backend.db.setting_i64(SENT, 0),
    })
}

/// The PC's library, as Get from PC shows it: each episode with whether the
/// phone has it or is getting it.
pub fn pc_library(backend: &Backend) -> Result<Value> {
    let mut library = ask(backend, "library", None)?;
    let getting = lock(&backend.phone.getting).clone();
    for show in library["shows"].as_array_mut().into_iter().flatten() {
        for e in show["episodes"].as_array_mut().into_iter().flatten() {
            let id = e["episode"]["id"].as_i64().unwrap_or(0);
            let on_phone = backend.db.episode(id).ok().flatten().is_some_and(|e| e.media_state == "ready");
            e["onPhone"] = json!(if on_phone { "ready" } else if getting.iter().any(|g| g.id == id && g.state != "failed") { "getting" } else { "no" });
        }
    }
    library["free"] = json!(free_space(&copies(backend)));
    Ok(library)
}

/// Takes in the PC's shows the phone has something of, with every episode
/// of each (their subtitles too, for Previously on), the PC's settings and
/// your dictionaries. `also` are shows to take in though the phone has
/// nothing of them yet (they're being got).
fn take_library(backend: &Backend, library: &Value, also: &[i64]) -> Result<()> {
    let have: Vec<i64> = backend.db.shows()?.iter().map(|s| s.id).collect();
    let mut stamps = backend.db.setting(LINES)?.and_then(|v| v.as_object().cloned()).unwrap_or_default();
    for entry in library["shows"].as_array().into_iter().flatten() {
        let show: Show = serde_json::from_value(entry["show"].clone())?;
        if !have.contains(&show.id) && !also.contains(&show.id) {
            continue;
        }
        backend.db.put_show(&show)?;
        let cover = backend.dirs.covers().join(format!("{}.jpg", show.id));
        if !cover.is_file() {
            let _ = get_file(backend, &format!("cover/{}", show.id), &cover, &|_| {});
        }
        for e in entry["episodes"].as_array().into_iter().flatten() {
            let episode: Episode = serde_json::from_value(e["episode"].clone())?;
            let id = episode.id;
            let fresh = backend.db.episode(id)?.is_none();
            backend.db.put_episode(&episode, e["reached"].as_f64().unwrap_or(0.0))?;
            if fresh {
                if let Ok(extra) = ask(backend, &format!("episode/{id}"), None) {
                    // One audio track: the Japanese one, the only one the phone's copy has.
                    let mut probe: Value = serde_json::from_str(extra["probe"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                    if let Some(audio) = probe.get_mut("audio").and_then(Value::as_array_mut) {
                        let japanese = audio.get(episode.audio_index.max(0) as usize).cloned();
                        *audio = japanese.into_iter().collect();
                    }
                    backend.db.set_probe(id, &probe.to_string(), episode.duration, episode.ending_at, "direct")?;
                    if let Some([start, end]) = extra["opening"].as_array().map(|o| [o[0].as_f64(), o[1].as_f64()]) {
                        if let (Some(start), Some(end)) = (start, end) {
                            backend.db.set_opening(id, Some((start, end)))?;
                        }
                    }
                }
            }
            let stamp = e["lines"].as_str().unwrap_or_default();
            if !stamp.is_empty() && stamps.get(&id.to_string()).and_then(Value::as_str) != Some(stamp) {
                if let Ok(lines) = ask(backend, &format!("lines/{id}"), None) {
                    let lines: Vec<crate::subs::Line> = serde_json::from_value(lines).unwrap_or_default();
                    jobs::write_lines(backend, id, &lines)?;
                    stamps.insert(id.to_string(), json!(stamp));
                }
            }
        }
    }
    backend.db.set_setting(LINES, &Value::Object(stamps))?;
    for (key, value) in library["settings"].as_object().into_iter().flatten() {
        if SHARED_SETTINGS.contains(&key.as_str()) {
            backend.db.set_setting(key, value)?;
        }
    }
    take_dictionaries(backend, library)?;
    Ok(())
}

/// Gets the dictionaries the PC has that the phone hasn't (or has an older
/// copy of), and lets go of those the PC no longer has.
fn take_dictionaries(backend: &Backend, library: &Value) -> Result<()> {
    // Not while one of these files is being installed.
    let _installing = crate::lookups::hold(backend);
    let dir = dictionaries_dir(backend);
    std::fs::create_dir_all(&dir)?;
    let mut have = backend.db.setting(DICTIONARIES)?.and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let listed: Vec<&Value> = library["dictionaries"].as_array().into_iter().flatten().collect();
    for d in &listed {
        let (Some(name), Some(revision)) = (d["name"].as_str(), d["revision"].as_str()) else { continue };
        if name.contains(['/', '\\']) || name.contains("..") || have.get(name).and_then(Value::as_str) == Some(revision) {
            continue;
        }
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        let encoded = percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC).to_string();
        get_file(backend, &format!("dictionary/{encoded}"), &path, &|_| {})?;
        have.insert(name.to_string(), json!(revision));
    }
    have.retain(|name, _| {
        let kept = listed.iter().any(|d| d["name"] == name.as_str());
        if !kept {
            let _ = std::fs::remove_file(dir.join(name));
        }
        kept
    });
    backend.db.set_setting(DICTIONARIES, &Value::Object(have))?;
    Ok(())
}

/// Your dictionaries on the phone, each file's name with its revision on the
/// PC, in the order lookups show them (lookups.rs installs them).
pub fn dictionaries(backend: &Backend) -> Vec<(String, String)> {
    let have = backend.db.setting(DICTIONARIES).ok().flatten().and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let mut list: Vec<(String, String)> = have.iter().map(|(name, revision)| (name.clone(), revision.as_str().unwrap_or_default().to_string())).collect();
    list.sort_by_key(|(name, _)| own_hachidori::install_order(name, 0));
    list
}

/// Forgets a dictionary the phone got, so the next sync gets it from the PC again.
pub fn forget_dictionary(backend: &Backend, name: &str) -> Result<()> {
    let mut have = backend.db.setting(DICTIONARIES)?.and_then(|v| v.as_object().cloned()).unwrap_or_default();
    have.remove(name);
    let _ = std::fs::remove_file(dictionaries_dir(backend).join(name));
    backend.db.set_setting(DICTIONARIES, &Value::Object(have))
}

/// Downloads `path` from the PC into `to`, carrying on from a `.part` left
/// by a copy that stopped.
fn get_file(backend: &Backend, path: &str, to: &Path, progress: &dyn Fn(u64)) -> Result<u64> {
    let (address, token) = pc(backend)?;
    let part = to.with_extension(format!("{}.part", to.extension().and_then(|e| e.to_str()).unwrap_or("")));
    std::fs::create_dir_all(to.parent().expect("a folder"))?;
    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut response = agent(6 * 3600)
        .get(&format!("http://{address}/{path}"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Range", &format!("bytes={have}-"))
        .call()
        .map_err(|e| unreachable(&address, e))?;
    let status = response.status().as_u16();
    if status != 200 && status != 206 {
        let value: Value = response.body_mut().read_json().unwrap_or(Value::Null);
        return Err(anyhow!("{}", value["error"].as_str().unwrap_or("The PC didn’t send it")));
    }
    let mut file = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&part)?;
    let mut done = if status == 206 { file.seek(SeekFrom::Start(have))? } else { file.set_len(0).map(|()| 0)? };
    let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buffer).map_err(|e| unreachable(&address, e))?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])?;
        done += n as u64;
        progress(done);
    }
    file.sync_all()?;
    drop(file);
    std::fs::rename(&part, to)?;
    Ok(done)
}

/// Starts getting episodes from the PC: the PC makes each one's copy, and the
/// phone copies it over, one at a time.
pub fn get(backend: &Arc<Backend>, ids: &[i64], plays: Option<Plays>) -> Result<Value> {
    if let Some(plays) = plays {
        *lock(&backend.phone.plays) = plays;
    }
    let plays = lock(&backend.phone.plays).clone();
    let library = ask(backend, "library", None)?;
    let mut shows = Vec::new();
    let mut sizes = HashMap::new();
    for entry in library["shows"].as_array().into_iter().flatten() {
        for e in entry["episodes"].as_array().into_iter().flatten() {
            let id = e["episode"]["id"].as_i64().unwrap_or(0);
            if ids.contains(&id) {
                shows.push(entry["show"]["id"].as_i64().unwrap_or(0));
                sizes.insert(id, (entry["show"]["id"].as_i64().unwrap_or(0), e["rip"].as_u64().unwrap_or(0)));
            }
        }
    }
    take_library(backend, &library, &shows)?;
    ask(backend, "make", Some(&json!({ "ids": ids, "plays": plays })))?;
    let mut getting = lock(&backend.phone.getting);
    for &id in ids {
        getting.retain(|g| g.id != id || g.state != "failed");
        if getting.iter().any(|g| g.id == id) {
            continue;
        }
        let (show, size) = sizes.get(&id).copied().unwrap_or((0, 0));
        getting.push(Getting { id, show, state: "waiting", progress: 0.0, size, error: String::new() });
    }
    drop(getting);
    backend.phone.get_wake.notify_all();
    Ok(state(backend))
}

/// Lets go of the phone's copies of these episodes (they stay on the PC).
pub fn remove(backend: &Backend, ids: &[i64]) -> Result<Value> {
    for &id in ids {
        if let Some(e) = backend.db.episode(id)? {
            if !e.play_path.is_empty() {
                let _ = std::fs::remove_file(&e.play_path);
            }
            backend.db.set_phone_copy(id, None)?;
        }
        lock(&backend.phone.getting).retain(|g| g.id != id);
    }
    Ok(state(backend))
}

/// Makes room for `size` more bytes by letting go of watched copies, the
/// longest-watched first. Errs when even that isn't enough.
fn make_room(backend: &Backend, size: u64) -> Result<()> {
    let dir = copies(backend);
    let mut watched: Vec<Episode> = backend.db.all_episodes()?.into_iter().filter(|e| e.media_state == "ready" && e.watched_at.is_some()).collect();
    watched.sort_by_key(|e| e.watched_at);
    let mut watched = watched.into_iter();
    while free_space(&dir) < size + SPARE {
        let Some(e) = watched.next() else {
            let short = size + SPARE - free_space(&dir);
            return Err(anyhow!("There isn’t room on the phone: it needs {} MB more", short / 1_000_000 + 1));
        };
        let _ = std::fs::remove_file(&e.play_path);
        backend.db.set_phone_copy(e.id, None)?;
    }
    Ok(())
}

/// Copies the episodes asked for over from the PC, one at a time.
pub fn start_getting(backend: Arc<Backend>) {
    std::thread::Builder::new()
        .name("telly-get".into())
        .spawn(move || loop {
            let next = {
                let mut getting = lock(&backend.phone.getting);
                loop {
                    if let Some(g) = getting.iter().find(|g| g.state == "waiting" || g.state == "copying").cloned() {
                        break g;
                    }
                    getting = backend.phone.get_wake.wait(getting).unwrap_or_else(|e| e.into_inner());
                }
            };
            let set = |state: &'static str, progress: f64, error: String| {
                if let Some(g) = lock(&backend.phone.getting).iter_mut().find(|g| g.id == next.id) {
                    (g.state, g.progress, g.error) = (state, progress, error);
                }
            };
            match get_one(&backend, &next, &|state, progress| set(state, progress, String::new())) {
                Ok(()) => lock(&backend.phone.getting).retain(|g| g.id != next.id),
                Err(e) => {
                    set("failed", 0.0, format!("{e:#}"));
                }
            }
        })
        .ok();
}

/// Waits for the PC to make the episode's copy, then copies it over.
fn get_one(backend: &Backend, g: &Getting, set: &dyn Fn(&'static str, f64)) -> Result<()> {
    let (size, ext) = loop {
        let library = ask(backend, "library", None)?;
        let e = library["shows"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|s| s["episodes"].as_array().into_iter().flatten())
            .find(|e| e["episode"]["id"] == g.id)
            .cloned()
            .ok_or_else(|| anyhow!("That episode isn’t in Telly on the PC any more"))?;
        match e["phone"]["state"].as_str() {
            Some("ready") => break (e["phone"]["size"].as_u64().unwrap_or(0), e["phone"]["ext"].as_str().filter(|x| !x.is_empty()).unwrap_or("mkv").to_string()),
            Some("failed") => return Err(anyhow!("The PC couldn’t make the phone’s copy: {}", e["phone"]["error"].as_str().unwrap_or(""))),
            Some("working") => set("making", e["phone"]["progress"].as_f64().unwrap_or(0.0)),
            Some("none") => {
                let plays = lock(&backend.phone.plays).clone();
                ask(backend, "make", Some(&json!({ "ids": [g.id], "plays": plays })))?;
                set("making", 0.0);
            }
            _ => set("making", 0.0),
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    let path = copies(backend).join(format!("{}.{ext}", g.id));
    let part = path.with_extension(format!("{ext}.part"));
    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    make_room(backend, size.saturating_sub(have))?;
    set("copying", have as f64 / size.max(1) as f64);
    get_file(backend, &format!("video/{}", g.id), &path, &|done| set("copying", done as f64 / size.max(1) as f64))?;
    backend.db.set_phone_copy(g.id, Some(&path.to_string_lossy()))?;
    let _ = ask(backend, &format!("got/{}", g.id), Some(&json!({})));
    Ok(())
}

/// The phone's Library: each show it has something of, the last watched
/// first, with every episode and whether the phone has it, is getting it,
/// or has only its subtitles.
pub fn phone_library(backend: &Backend) -> Result<Value> {
    let getting = lock(&backend.phone.getting).clone();
    let mut shows = backend.db.shows()?;
    shows.sort_by_key(|s| (std::cmp::Reverse(s.last_watched_at.unwrap_or(0)), s.title.to_lowercase()));
    let mut out = Vec::new();
    for show in shows {
        let episodes: Vec<Value> = backend
            .db
            .episodes(show.id)?
            .into_iter()
            .map(|e| {
                let g = getting.iter().find(|g| g.id == e.id);
                json!({
                    "id": e.id, "number": e.number, "duration": e.duration, "position": e.position,
                    "watched": e.watched_at.is_some(), "onPhone": e.media_state == "ready",
                    "getting": g.map(|g| json!({ "state": g.state, "progress": g.progress, "error": g.error })),
                })
            })
            .collect();
        let next = episodes
            .iter()
            .find(|e| e["watched"] == false)
            .map(|e| e["id"].clone());
        out.push(json!({ "show": show, "episodes": episodes, "next": next }));
    }
    Ok(json!({ "shows": out, "state": state(backend) }))
}

/// What a card for `line` of an episode on the phone needs: ffmpeg's
/// arguments for its picture and audio (run by FFmpegKit, in TellyPlugin.kt),
/// where they go, and the note type, deck, tags and fields the PC's cards use.
pub fn card_plan(backend: &Backend, episode_id: i64, start: f64, end: f64, text: &str) -> Result<Value> {
    let episode = backend.db.episode(episode_id)?.ok_or_else(|| anyhow!("That episode isn’t on the phone any more"))?;
    let show = backend.db.show(episode.show_id)?.ok_or_else(|| anyhow!("That show isn’t on the phone any more"))?;
    if episode.play_path.is_empty() {
        return Err(anyhow!("This episode isn’t on the phone"));
    }
    let lines = crate::cards::played_lines(backend, episode_id);
    let line = crate::subs::Line { start, end, text: text.to_string(), ruby: vec![], speaker: String::new(), sign: false };
    let (from, to) = crate::cards::clip_bounds(&lines, &line, &backend.db.setting_str("clip.padding"));
    let from = from.max(0.0);
    let length = (to - from).clamp(0.3, crate::clip::MAX_SECONDS);
    let dir = backend.dirs.clips();
    std::fs::create_dir_all(&dir)?;
    let base = format!("telly_{episode_id}_{}", (from * 1000.0) as i64);
    let (picture, audio) = (dir.join(format!("{base}.avif")), dir.join(format!("{base}.opus")));
    let video = Path::new(&episode.play_path);
    let field = |key: &str, default: &str| match backend.db.setting_str(key).trim() {
        "" => default.to_string(),
        name => name.to_string(),
    };
    Ok(json!({
        "picture": picture,
        "audio": audio,
        "pictureArgs": crate::clip::picture_args(video, from, length, &picture),
        "audioArgs": crate::clip::audio_args(video, 0, from, length, &audio),
        "model": own_hachidori::model(backend),
        "deck": own_hachidori::deck_for(backend, &show.title),
        "tags": own_hachidori::tags_for(&show.title),
        "fields": {
            "sentence": field("anki.field.sentence", "Sentence"),
            "picture": field("anki.field.picture", "Picture"),
            "audio": field("anki.field.audio", "SentenceAudio"),
            "info": field("anki.field.info", "MiscInfo"),
        },
        "info": format!("{} · E{:02} · {}", show.title, episode.number, crate::util::clock(from + 0.25)),
    }))
}

/// Gives the PC what changed on the phone since last time, and takes in
/// what changed on the PC for the shows the phone has.
pub fn sync(backend: &Backend) -> Result<Value> {
    let now = crate::util::now_ms();
    let since = backend.db.setting_i64(SENT, 0);
    let progress = backend.db.progress_since(since)?;
    ask(backend, "progress", Some(&progress))?;
    backend.db.set_setting(SENT, &json!(now))?;
    let library = ask(backend, "library", None)?;
    take_library(backend, &library, &[])?;
    Ok(state(backend))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Dirs;
    use crate::db::{NewEpisode, NewShow};

    fn library(name: &str) -> (Backend, PathBuf) {
        let dir = std::env::temp_dir().join(format!("telly-phone-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap(), dir)
    }

    fn probe(codec: &str, pix_fmt: &str, audio: &[&str]) -> media::Probe {
        media::Probe {
            video: Some(media::VideoInfo { codec: codec.into(), pix_fmt: pix_fmt.into(), ..Default::default() }),
            audio: audio.iter().map(|c| media::AudioInfo { codec: c.to_string(), ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn an_episode_goes_to_the_phone_as_it_is_when_the_phone_plays_it() {
        // What the Thor's WebView said it plays.
        let thor = Plays {
            video: ["h264", "h264-10", "hevc", "hevc-10", "av1", "av1-10", "vp9", "vp9-10"].map(String::from).to_vec(),
            audio: ["aac", "mp3", "opus", "flac", "vorbis"].map(String::from).to_vec(),
        };
        let mkv = Path::new("/rips/Show - 01.mkv");
        let mp4 = Path::new("/rips/Show - 01.mp4");
        assert_eq!(phone_plan(&probe("hevc", "yuv420p10le", &["aac"]), mp4, 0, &thor), PhonePlan::AsIs);
        assert_eq!(phone_plan(&probe("av1", "yuv420p10le", &["opus"]), mp4, 0, &thor), PhonePlan::AsIs);
        // An MKV is copied into an MP4: one opened part-way stalls on Android.
        assert_eq!(phone_plan(&probe("hevc", "yuv420p10le", &["flac"]), mkv, 0, &thor), PhonePlan::Remux);
        // A dub beside the Japanese: the Japanese alone, copied in seconds.
        assert_eq!(phone_plan(&probe("hevc", "yuv420p10le", &["aac", "aac"]), mkv, 1, &thor), PhonePlan::Remux);
        // An AVI the phone's WebView doesn't open.
        assert_eq!(phone_plan(&probe("h264", "yuv420p", &["aac"]), Path::new("/rips/old.avi"), 0, &thor), PhonePlan::Remux);
        // Dolby and DTS play silently on Android: only the audio is converted.
        assert_eq!(phone_plan(&probe("h264", "yuv420p", &["ac3"]), mkv, 0, &thor), PhonePlan::Audio);
        assert_eq!(phone_plan(&probe("hevc", "yuv420p10le", &["eac3"]), mkv, 0, &thor), PhonePlan::Audio);
        // A picture the phone can't play is converted, whatever its audio.
        assert_eq!(phone_plan(&probe("mpeg2video", "yuv420p", &["ac3"]), mkv, 0, &thor), PhonePlan::Video);
        // A phone that said nothing plays H.264 alone.
        assert_eq!(phone_plan(&probe("hevc", "yuv420p10le", &["aac"]), mkv, 0, &Plays::default()), PhonePlan::Video);
        assert_eq!(phone_plan(&probe("h264", "yuv420p", &["aac"]), mp4, 0, &Plays::default()), PhonePlan::AsIs);
    }

    #[test]
    fn what_the_phone_watched_goes_back_to_the_pc() {
        let (pc, pc_dir) = library("pc");
        let (phone, phone_dir) = library("phone");
        let episodes = (1..=3).map(|n| NewEpisode { number: n, path: format!("/rips/{n}.mkv"), size: 10, duration: 1400.0 }).collect();
        let show = pc.db.add_show(&NewShow { folder: "/rips".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let ids: Vec<i64> = pc.db.episodes(show).unwrap().iter().map(|e| e.id).collect();
        pc.db.save_position(ids[2], 100.0).unwrap();

        // The phone takes in the show under the PC's ids.
        phone.db.put_show(&pc.db.show(show).unwrap().unwrap()).unwrap();
        for e in pc.db.episodes(show).unwrap() {
            phone.db.put_episode(&e, 0.0).unwrap();
        }
        assert_eq!(phone.db.episodes(show).unwrap().iter().map(|e| e.id).collect::<Vec<_>>(), ids);
        assert_eq!(phone.db.episode(ids[2]).unwrap().unwrap().position, 100.0);
        assert_eq!(phone.db.episode(ids[0]).unwrap().unwrap().media_state, "away");
        phone.db.set_phone_copy(ids[0], Some("/phone/1.mp4")).unwrap();

        // On the phone: episode 1 played to its end, episode 2 part-way.
        std::thread::sleep(Duration::from_millis(5));
        phone.db.save_position(ids[0], 1390.0).unwrap();
        phone.db.finish_episode(ids[0], false).unwrap();
        phone.db.save_position(ids[1], 600.0).unwrap();
        phone.db.reach(ids[1], 650.0).unwrap();
        phone.db.save_answer(ids[1], "recap", -1, "about", "The story so far", "[]").unwrap();

        let progress = phone.db.progress_since(0).unwrap();
        pc.db.take_progress(&progress).unwrap();
        // Given twice (a sync that didn't hear back), it's still once.
        pc.db.take_progress(&progress).unwrap();

        let first = pc.db.episode(ids[0]).unwrap().unwrap();
        assert!(first.watched_at.is_some());
        assert_eq!(pc.db.watches_since(show, 0).unwrap().len(), 1);
        assert_eq!(pc.db.episode(ids[1]).unwrap().unwrap().position, 600.0);
        assert_eq!(pc.db.reached(ids[1]).unwrap(), 650.0);
        assert_eq!(pc.db.episode(ids[2]).unwrap().unwrap().position, 100.0, "untouched on the phone");
        assert_eq!(pc.db.answer(ids[1], "recap", -1).unwrap().unwrap().text, "The story so far");

        // Taking the PC's episode in again keeps the phone's copy and its later place.
        for e in pc.db.episodes(show).unwrap() {
            phone.db.put_episode(&e, 0.0).unwrap();
        }
        let again = phone.db.episode(ids[0]).unwrap().unwrap();
        assert_eq!((again.media_state.as_str(), again.play_path.as_str()), ("ready", "/phone/1.mp4"));
        drop((pc, phone));
        let _ = std::fs::remove_dir_all(pc_dir);
        let _ = std::fs::remove_dir_all(phone_dir);
    }
}
