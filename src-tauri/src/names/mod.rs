//! Character names. Telly makes a Yomitan dictionary of the casts of the shows
//! you're watching, from AniList, the way characterdictionary.tokyo makes them
//! (its builder is copied into this folder), and adds it to Hachidori the way
//! SubMiner adds its own: hovering a name in the subtitles shows who it is,
//! with their portrait, and how they're being addressed (さん, 様, 先輩…).
//!
//! The dictionary goes in through Hachidori's upload requests, from the hidden
//! engine page (hachidori-addon/telly/host.js), which asks `/names/state` for
//! a new copy every poll. A linked Hachidori passes it on to the Hachidori in
//! your browser, so the names work there too; an unlinked one keeps it.

// characterdictionary.tokyo's builder, as it is apart from the changes marked
// "Telly:"; the parts Telly doesn't use stay.
#[allow(dead_code)]
mod content_builder;
#[allow(dead_code)]
mod dict_builder;
#[allow(dead_code)]
mod image_handler;
#[allow(dead_code)]
pub mod kana;
#[allow(dead_code)]
pub mod models;
#[allow(dead_code)]
pub mod name_parser;

use crate::backend::Backend;
use crate::jobs::Task;
use crate::util::now_ms;
use anyhow::{anyhow, Result};
use content_builder::DictSettings;
use dict_builder::DictBuilder;
use image_handler::ImageHandler;
use models::Character;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// The dictionary's title in Hachidori. Each new copy replaces the last.
pub const TITLE: &str = "Telly characters";
/// How many shows' casts the dictionary holds: the ones you watched last.
const MAX_SHOWS: usize = 6;
/// A cast is fetched again after this long (AniList adds characters as a show airs).
const REFRESH_MS: i64 = 30 * 24 * 3600 * 1000;
/// After a failed import, how long the engine page waits before trying that copy again.
const RETRY_MS: i64 = 5 * 60 * 1000;
/// The character dictionary, in the names folder.
pub const ZIP: &str = "telly-characters.zip";

/// Supporting characters get the honorifics anime use most (their notes come
/// from the builder's own list); main characters get every one of them.
const COMMON_HONORIFICS: &[&str] = &[
    "さん", "様", "さま", "氏", "殿", "どの", "君", "くん", "ちゃん", "たん", "ちん", "っち", "坊", "坊ちゃん", "嬢", "嬢ちゃん",
    "お嬢", "姫", "姫様", "王子", "王子様", "先生", "先輩", "後輩", "博士", "師匠", "社長", "会長", "部長", "隊長", "団長", "船長",
    "大将", "兄", "兄さん", "お兄さん", "お兄ちゃん", "兄ちゃん", "兄貴", "姉", "姉さん", "お姉さん", "お姉ちゃん", "姉ちゃん", "姉貴",
    "殿下", "陛下", "閣下", "神様", "長老", "パイセン", "っす",
];

/// The honorifics a character's names get, by their role in the dictionary
/// (AniList's main characters are "primary", supporting "side", background "appears").
pub fn honorifics_for(role: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    match role {
        "main" | "primary" => name_parser::HONORIFIC_SUFFIXES.to_vec(),
        "side" => name_parser::HONORIFIC_SUFFIXES.iter().filter(|(term, _, _)| COMMON_HONORIFICS.contains(term)).copied().collect(),
        _ => Vec::new(),
    }
}

/// Whether `term` could be a name or part of one: not one that starts with
/// ー or a small kana, which only ever end up there from a bad split.
pub fn plausible_name(term: &str) -> bool {
    let mut chars = term.trim().chars();
    match chars.next() {
        None => false,
        Some(first) => !"ーぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ".contains(first),
    }
}

const NAME_DOTS: &[char] = &['・', '･', '·', '•', '＝', '='];

/// The parts of a name written with ・ (ナツ・ドラグニル: ナツ and ドラグニル),
/// leaving out one-letter parts (the D in モンキー・D・ルフィ).
pub fn dotted_parts(name: &str) -> Vec<String> {
    if !name.contains(NAME_DOTS) || name.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    name.split(NAME_DOTS).map(str::trim).filter(|p| p.chars().count() >= 2).map(str::to_string).collect()
}

fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace() && !matches!(c, '・' | '･' | '·' | '=' | '＝')).collect()
}

/// Whether `name` is written as in the character's own name (ナツ in
/// ナツ・ドラグニル), rather than a kana spelling made from its reading.
pub fn written_form(name: &str, original: &str) -> bool {
    let name = squash(name);
    !name.is_empty() && squash(original).contains(&name)
}

/// A character's card without its description and details: the honorific
/// entries carry this, and the full card stays on the name itself.
pub fn short_card(card: &Value) -> Value {
    const KEEP: &[&str] = &["name", "name-romaji", "portrait-container", "source", "role-container"];
    let Some(items) = card.get("content").and_then(Value::as_array) else { return card.clone() };
    let kept: Vec<Value> = items
        .iter()
        .filter(|item| item.pointer("/data/id").and_then(Value::as_str).is_some_and(|id| KEEP.contains(&id)))
        .cloned()
        .collect();
    if kept.is_empty() {
        return card.clone();
    }
    json!({ "type": "structured-content", "content": kept })
}

/// Today as YYYY-MM-DD (UTC), for the dictionary's "built" tag.
pub fn today() -> String {
    // Howard Hinnant's civil_from_days.
    let z = now_ms().div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// A show's cast as Telly keeps it: names/<AniList id>.json, with the
/// portraits in names/img (never in the JSON).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Cast {
    pub anilist_id: i64,
    /// The show's title on the cards ("From: …"): AniList's native title.
    pub title: String,
    pub fetched_at: i64,
    pub characters: Vec<Character>,
}

// AniList's characters, as characterdictionary.tokyo asks for them (anilist_client.rs).
const CHARACTERS_QUERY: &str = r#"
query ($id: Int!, $page: Int, $perPage: Int) {
  Media(id: $id, type: ANIME) {
    id
    title { romaji english native }
    characters(page: $page, perPage: $perPage, sort: [ROLE, RELEVANCE, ID]) {
      pageInfo { hasNextPage currentPage }
      edges {
        role
        voiceActors(language: JAPANESE) { name { full native } image { large } }
        node {
          id
          name { full native alternative alternativeSpoiler first last }
          image { large }
          description
          gender
          age
          dateOfBirth { month day }
          bloodType
        }
      }
    }
  }
}"#;

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// One character from AniList (characterdictionary.tokyo's process_character).
fn character(edge: &Value) -> Option<Character> {
    let node = edge.get("node")?;
    let role = match edge["role"].as_str().unwrap_or("BACKGROUND") {
        "MAIN" => "primary",
        "SUPPORTING" => "side",
        _ => "appears",
    };
    let name = node.get("name")?;
    let text = |v: &Value| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let voice = edge["voiceActors"].as_array().and_then(|a| a.first());
    Some(Character {
        id: node.get("id")?.as_u64()?.to_string(),
        name: name["full"].as_str().unwrap_or("").to_string(),
        name_original: name["native"].as_str().unwrap_or("").trim().to_string(),
        role: role.to_string(),
        source: "anilist".into(),
        sex: node["gender"].as_str().and_then(|g| match g.to_lowercase().chars().next() {
            Some('m') => Some("m".to_string()),
            Some('f') => Some("f".to_string()),
            _ => None,
        }),
        age: node["age"].as_str().map(str::to_string).or_else(|| node["age"].as_u64().map(|n| n.to_string())),
        blood_type: text(&node["bloodType"]),
        birthday: (|| Some(vec![node["dateOfBirth"]["month"].as_u64()? as u32, node["dateOfBirth"]["day"].as_u64()? as u32]))(),
        description: text(&node["description"]),
        aliases: strings(&name["alternative"]),
        spoiler_aliases: strings(&name["alternativeSpoiler"]),
        image_url: text(&node["image"]["large"]),
        first_name_hint: text(&name["first"]),
        last_name_hint: text(&name["last"]),
        seiyuu: voice.and_then(|va| text(&va["name"]["native"]).or_else(|| text(&va["name"]["full"]))),
        seiyuu_image_url: voice.and_then(|va| text(&va["image"]["large"])),
        ..Default::default()
    })
}

/// A show's whole cast, page by page. `progress` hears how many pages are in.
pub fn fetch_cast(anilist_id: i64, mut progress: impl FnMut(usize)) -> Result<Cast> {
    let mut cast = Cast { anilist_id, fetched_at: now_ms(), ..Default::default() };
    for page in 1..=200 {
        let answer = crate::anilist::query_patiently(CHARACTERS_QUERY, json!({ "id": anilist_id, "page": page, "perPage": 25 }))?;
        let media = answer.pointer("/data/Media").ok_or_else(|| anyhow!("AniList has no show {anilist_id}"))?;
        if page == 1 {
            let title = |key: &str| media["title"][key].as_str().unwrap_or("").to_string();
            cast.title = [title("native"), title("romaji"), title("english")].into_iter().find(|t| !t.is_empty()).unwrap_or_default();
        }
        let edges = media.pointer("/characters/edges").and_then(Value::as_array).ok_or_else(|| anyhow!("AniList sent no characters"))?;
        cast.characters.extend(edges.iter().filter_map(character));
        progress(page);
        if !media.pointer("/characters/pageInfo/hasNextPage").and_then(Value::as_bool).unwrap_or(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(700));
    }
    Ok(cast)
}

fn image_file(dir: &Path, c: &Character) -> Option<PathBuf> {
    let ext = c.image_ext.as_deref()?;
    Some(dir.join(ImageHandler::make_filename(&c.id, ext)))
}

/// Downloads the portraits the cast doesn't have yet, four at a time, as
/// 160×200 JPEGs in `dir` (the builder's own resize). Records each one's
/// size on the character. `progress` hears (done, wanted).
pub fn portraits(cast: &mut Cast, dir: &Path, progress: impl Fn(usize, usize) + Sync) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    // Portraits already here, from an earlier fetch of this cast, are kept.
    for c in cast.characters.iter_mut().filter(|c| c.image_ext.is_none()) {
        for ext in ["jpg", "png", "webp", "gif"] {
            let path = dir.join(ImageHandler::make_filename(&c.id, ext));
            if path.is_file() {
                c.image_ext = Some(ext.to_string());
                if let Ok((w, h)) = image::image_dimensions(&path) {
                    (c.image_width, c.image_height) = (Some(w), Some(h));
                }
                break;
            }
        }
    }
    let wanted: Vec<usize> = (0..cast.characters.len())
        .filter(|&i| {
            let c = &cast.characters[i];
            c.image_url.is_some() && !c.name_original.is_empty() && image_file(dir, c).is_none_or(|p| !p.is_file())
        })
        .collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, &'static str, u32, u32)>> = Mutex::new(Vec::new());
    let characters = &cast.characters;
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| loop {
                let n = next.fetch_add(1, Ordering::SeqCst);
                let Some(&i) = wanted.get(n) else { break };
                let c = &characters[i];
                if let Some(url) = &c.image_url {
                    if let Ok(bytes) = crate::anilist::image(url) {
                        let (resized, ext, w, h) = ImageHandler::resize_image(&bytes);
                        if std::fs::write(dir.join(ImageHandler::make_filename(&c.id, ext)), &resized).is_ok() {
                            if let Ok(mut r) = results.lock() {
                                r.push((i, ext, w, h));
                            }
                        }
                    }
                }
                progress(done.fetch_add(1, Ordering::SeqCst) + 1, wanted.len());
            });
        }
    });
    for (i, ext, w, h) in results.into_inner().unwrap_or_default() {
        let c = &mut cast.characters[i];
        c.image_ext = Some(ext.to_string());
        (c.image_width, c.image_height) = if w > 0 && h > 0 { (Some(w), Some(h)) } else { (None, None) };
    }
    Ok(())
}

/// What a built dictionary holds, for Settings.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub shows: Vec<String>,
    pub characters: usize,
    pub bytes: usize,
}

/// The dictionary for these casts: a Yomitan ZIP.
pub fn build(casts: &[Cast], img_dir: &Path, spoilers: bool) -> Result<(Vec<u8>, Summary)> {
    let settings = DictSettings { show_spoilers: spoilers, ..Default::default() };
    let titles: Vec<String> = casts.iter().map(|c| c.title.clone()).collect();
    let mut builder = DictBuilder::new(settings, None, titles.join(", "));
    let mut characters = 0;
    for cast in casts {
        for c in &cast.characters {
            if c.name_original.is_empty() {
                continue;
            }
            let mut c = c.clone();
            c.image_bytes = image_file(img_dir, &c).and_then(|p| std::fs::read(p).ok());
            if c.image_bytes.is_none() {
                c.image_ext = None;
            }
            builder.add_character(&c, &cast.title);
            characters += 1;
        }
    }
    let bytes = builder.export_bytes().map_err(|e| anyhow!(e))?;
    let summary = Summary { shows: titles, characters, bytes: bytes.len() };
    Ok((bytes, summary))
}

/// One name the subtitles may use, for Claude's prompts and the player.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Name {
    /// As the show writes it: ナツ, ドラグニル, ナツ・ドラグニル.
    pub term: String,
    /// AniList's romanised full name: Natsu Dragneel.
    pub name: String,
    pub role: String,
}

/// The ways each character's name is written: their full name and its
/// family and given parts, as AniList's native name has them.
pub fn names(cast: &Cast) -> Vec<Name> {
    let mut out: Vec<Name> = Vec::new();
    for c in &cast.characters {
        if c.name_original.is_empty() {
            continue;
        }
        let parts = name_parser::split_japanese_name_with_hints(&c.name_original, c.first_name_hint.as_deref(), c.last_name_hint.as_deref());
        let mut terms = vec![c.name_original.clone(), parts.combined.clone()];
        terms.extend(parts.family.clone());
        terms.extend(parts.given.clone());
        terms.extend(dotted_parts(&c.name_original));
        for term in terms {
            let term = term.trim().to_string();
            // One-character names (宮, 上) are too often something else.
            if term.chars().count() < 2 || !plausible_name(&term) || out.iter().any(|n| n.term == term) {
                continue;
            }
            out.push(Name { term, name: c.name.clone(), role: c.role.clone() });
        }
    }
    out
}

/// The characters named in `text`, as "ナツ (Natsu Dragneel)", main characters
/// first, for a prompt: Claude then writes their names as AniList does.
pub fn named_in(cast: &Cast, text: &str, limit: usize) -> Vec<String> {
    let mut found: Vec<(u8, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for n in names(cast) {
        if text.contains(&n.term) && seen.insert(n.name.clone()) {
            let rank = match n.role.as_str() {
                "primary" => 0,
                "side" => 1,
                _ => 2,
            };
            found.push((rank, format!("{} ({})", n.term, n.name)));
        }
    }
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().take(limit).map(|(_, s)| s).collect()
}

/// The background worker's wake-up, and what it's doing for the title bar.
#[derive(Default)]
pub struct Names {
    wake: Mutex<u64>,
    signal: Condvar,
    pub task: Mutex<Option<Task>>,
}

impl Names {
    /// Asks the worker to look again (a show started, a setting changed).
    pub fn poke(&self) {
        if let Ok(mut n) = self.wake.lock() {
            *n += 1;
        }
        self.signal.notify_all();
    }

    fn wait(&self, seen: &mut u64, timeout: Duration) {
        let Ok(guard) = self.wake.lock() else { return };
        if *guard != *seen {
            *seen = *guard;
            return;
        }
        if let Ok((guard, _)) = self.signal.wait_timeout(guard, timeout) {
            *seen = *guard;
        }
    }

    fn set_task(&self, task: Option<Task>) {
        if let Ok(mut t) = self.task.lock() {
            *t = task;
        }
    }
}

pub fn dir(backend: &Backend) -> PathBuf {
    backend.dirs.local.join("names")
}

fn cast_path(backend: &Backend, anilist_id: i64) -> PathBuf {
    dir(backend).join(format!("{anilist_id}.json"))
}

/// The cast Telly has for a show, if it has fetched it.
pub fn cached_cast(backend: &Backend, anilist_id: i64) -> Option<Cast> {
    serde_json::from_slice(&std::fs::read(cast_path(backend, anilist_id)).ok()?).ok()
}

fn save_cast(backend: &Backend, cast: &Cast) -> Result<()> {
    std::fs::create_dir_all(dir(backend))?;
    let path = cast_path(backend, cast.anilist_id);
    let partial = path.with_extension("json.part");
    std::fs::write(&partial, serde_json::to_vec(cast)?)?;
    std::fs::rename(partial, path)?;
    Ok(())
}

pub fn enabled(backend: &Backend) -> bool {
    backend.db.setting_bool("names.enabled", true)
}

/// The shows whose casts go in: the ones opened in the player most recently.
fn wanted(backend: &Backend) -> Result<Vec<crate::db::Show>> {
    let mut shows: Vec<crate::db::Show> =
        backend.db.shows()?.into_iter().filter(|s| s.anilist_id.is_some() && s.last_watched_at.is_some()).collect();
    shows.sort_by_key(|s| std::cmp::Reverse(s.last_watched_at));
    shows.truncate(MAX_SHOWS);
    Ok(shows)
}

/// Fetches what's missing and builds a new copy when the shows changed.
fn refresh(backend: &Backend) -> Result<()> {
    let shows = wanted(backend)?;
    if shows.is_empty() {
        return Ok(());
    }
    let img = dir(backend).join("img");
    let mut casts = Vec::new();
    // A show whose cast can't be had (AniList has no such entry any more) is
    // left out, and the dictionary is made of the others'.
    let mut missing = None;
    for show in &shows {
        let Some(anilist_id) = show.anilist_id else { continue };
        let task = |label: String, progress: f64| {
            backend.names.set_task(Some(Task {
                show_id: show.id,
                episode_id: 0,
                number: 0,
                title: show.title.clone(),
                label,
                progress,
            }))
        };
        let mut cast = match cached_cast(backend, anilist_id) {
            Some(c) if now_ms() - c.fetched_at < REFRESH_MS => c,
            _ => {
                task(format!("Getting {}'s characters", show.title), 0.0);
                let fetched = fetch_cast(anilist_id, |page| task(format!("Getting {}'s characters · page {page}", show.title), 0.0));
                // An old copy beats none while AniList is away.
                match fetched.map_err(|e| e.context(format!("getting {}'s characters", show.title))).or_else(|e| cached_cast(backend, anilist_id).ok_or(e)) {
                    Ok(c) => c,
                    Err(e) => {
                        missing.get_or_insert(e);
                        continue;
                    }
                }
            }
        };
        let before = cast.characters.iter().filter(|c| c.image_ext.is_some()).count();
        portraits(&mut cast, &img, |done, of| task(format!("Getting {}'s portraits · {done} of {of}", show.title), done as f64 / of.max(1) as f64))?;
        if cast.characters.iter().filter(|c| c.image_ext.is_some()).count() != before || cached_cast(backend, anilist_id).is_none_or(|c| c.fetched_at != cast.fetched_at) {
            save_cast(backend, &cast)?;
        }
        casts.push(cast);
    }
    if let (true, Some(e)) = (casts.is_empty(), missing.take()) {
        return Err(e);
    }
    let spoilers = backend.db.setting_bool("names.spoilers", false);
    // A portrait got since the last copy (one AniList's image server didn't send then) makes a new one.
    let key: Vec<String> = casts
        .iter()
        .map(|cast| format!("{}:{}:{}", cast.anilist_id, cast.fetched_at, cast.characters.iter().filter(|c| c.image_ext.is_some()).count()))
        .collect();
    let key = format!("{}|spoilers={spoilers}|v1", key.join(","));
    let zip = dir(backend).join(ZIP);
    if backend.db.setting_str("names.built") == key && zip.is_file() {
        return missing.map_or(Ok(()), Err);
    }
    backend.names.set_task(Some(Task {
        show_id: shows[0].id,
        episode_id: 0,
        number: 0,
        title: shows[0].title.clone(),
        label: "Making the character dictionary".into(),
        progress: 0.0,
    }));
    let (bytes, summary) = build(&casts, &img, spoilers)?;
    let partial = zip.with_extension("zip.part");
    std::fs::write(&partial, &bytes)?;
    std::fs::rename(&partial, &zip)?;
    backend.db.set_setting("names.built", &json!(key))?;
    backend.db.set_setting("names.revision", &json!(now_ms().to_string()))?;
    backend.db.set_setting("names.summary", &serde_json::to_value(&summary)?)?;
    // Made, and what was left out is said.
    missing.map_or(Ok(()), Err)
}

/// Keeps the dictionary up to date in the background: when a show starts,
/// and every half hour.
pub fn start(backend: Arc<Backend>) {
    std::thread::Builder::new()
        .name("telly-names".into())
        .spawn(move || {
            let mut seen = 0;
            loop {
                if enabled(&backend) {
                    let result = refresh(&backend);
                    backend.names.set_task(None);
                    let error = result.err().map(|e| format!("{e:#}")).unwrap_or_default();
                    let _ = backend.db.set_setting("names.buildError", &json!(error));
                }
                backend.names.wait(&mut seen, Duration::from_secs(1800));
            }
        })
        .ok();
}

/// What the engine page asks every poll: is there a copy it hasn't added yet?
pub fn state(backend: &Backend) -> Value {
    let revision = backend.db.setting_str("names.revision");
    let imported = backend.db.setting_str("names.imported");
    let failed = backend.db.setting_str("names.failedRevision");
    let failed_at = backend.db.setting_i64("names.failedAt", 0);
    let zip = dir(backend).join(ZIP).is_file();
    let due = enabled(backend) && zip && !revision.is_empty() && revision != imported && (failed != revision || now_ms() - failed_at > RETRY_MS);
    json!({ "due": due, "revision": revision, "url": format!("/names/{ZIP}?rev={revision}"), "title": TITLE })
}

/// The engine page's report on an import: `rev`, `ok`, and `error` or `to`
/// (the browser it went to, or "telly").
pub fn imported(backend: &Backend, query: &str) -> Result<()> {
    let get = |key: &str| {
        query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == key).then(|| percent_encoding::percent_decode_str(&v.replace('+', " ")).decode_utf8_lossy().into_owned())
        })
    };
    let revision = get("rev").ok_or_else(|| anyhow!("no revision"))?;
    if get("ok").as_deref() == Some("1") {
        backend.db.set_setting("names.imported", &json!(revision))?;
        backend.db.set_setting("names.importedAt", &json!(now_ms()))?;
        backend.db.set_setting("names.importedTo", &json!(get("to").unwrap_or_default()))?;
        backend.db.set_setting("names.error", &json!(""))?;
    } else {
        backend.db.set_setting("names.failedRevision", &json!(revision))?;
        backend.db.set_setting("names.failedAt", &json!(now_ms()))?;
        backend.db.set_setting("names.error", &json!(get("error").unwrap_or_else(|| "Hachidori didn’t take it".into())))?;
    }
    Ok(())
}

/// The ZIP the engine page downloads.
pub fn zip_path(backend: &Backend) -> PathBuf {
    dir(backend).join(ZIP)
}

/// For Settings.
pub fn status(backend: &Backend) -> Value {
    let summary: Option<Summary> = backend.db.setting("names.summary").ok().flatten().and_then(|v| serde_json::from_value(v).ok());
    let revision = backend.db.setting_str("names.revision");
    json!({
        "enabled": enabled(backend),
        "spoilers": backend.db.setting_bool("names.spoilers", false),
        "summary": summary,
        "built": !revision.is_empty(),
        "imported": !revision.is_empty() && backend.db.setting_str("names.imported") == revision,
        "importedAt": backend.db.setting_i64("names.importedAt", 0),
        "importedTo": backend.db.setting_str("names.importedTo"),
        "error": backend.db.setting_str("names.error"),
        "buildError": backend.db.setting_str("names.buildError"),
        "working": backend.names.task.lock().ok().and_then(|t| t.clone()),
    })
}

/// A show's names, if Telly has its cast.
pub fn show_names(backend: &Backend, show: &crate::db::Show) -> Vec<Name> {
    show.anilist_id.and_then(|id| cached_cast(backend, id)).map(|c| names(&c)).unwrap_or_default()
}

/// Reads a cast from disk, for prompts.
pub fn cast_for(backend: &Backend, show: &crate::db::Show) -> Option<Cast> {
    show.anilist_id.and_then(|id| cached_cast(backend, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chara(id: &str, name: &str, native: &str, role: &str, first: Option<&str>, last: Option<&str>) -> Character {
        Character {
            id: id.into(),
            name: name.into(),
            name_original: native.into(),
            role: role.into(),
            source: "anilist".into(),
            description: Some("A wizard of Fairy Tail. ~!Secretly a dragon.!~".into()),
            first_name_hint: first.map(Into::into),
            last_name_hint: last.map(Into::into),
            ..Default::default()
        }
    }

    fn cast() -> Cast {
        Cast {
            anilist_id: 6702,
            title: "FAIRY TAIL".into(),
            fetched_at: 1,
            characters: vec![
                chara("1", "Natsu Dragneel", "ナツ・ドラグニル", "primary", Some("Natsu"), Some("Dragneel")),
                chara("2", "Lucy Heartfilia", "ルーシィ・ハートフィリア", "primary", Some("Lucy"), Some("Heartfilia")),
                chara("3", "Makarov Dreyar", "マカロフ・ドレアー", "side", Some("Makarov"), Some("Dreyar")),
                chara("4", "Guild Member", "ギルドの男", "appears", None, None),
                chara("5", "No Japanese", "", "side", None, None),
            ],
        }
    }

    fn entries(zip: &[u8]) -> Vec<Value> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
        let mut all = Vec::new();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            if file.name().is_ok_and(|n| n.starts_with("term_bank_")) {
                let mut text = String::new();
                std::io::Read::read_to_string(&mut file, &mut text).unwrap();
                all.extend(serde_json::from_str::<Vec<Value>>(&text).unwrap());
            }
        }
        all
    }

    #[test]
    fn builds_a_dictionary_of_names_with_honorifics_for_the_cast_that_matters() {
        let dir = std::env::temp_dir().join(format!("telly-names-{}", std::process::id()));
        let (bytes, summary) = build(&[cast()], &dir, false).unwrap();
        assert_eq!(summary.characters, 4);
        let all = entries(&bytes);
        let terms: Vec<&str> = all.iter().map(|e| e[0].as_str().unwrap()).collect();
        for term in ["ナツ", "ドラグニル", "ルーシィ", "ナツさん", "ルーシィ様", "マカロフさん", "ギルドの男"] {
            assert!(terms.contains(&term), "{term} is missing");
        }
        // Background characters get no honorifics, and only main characters the rare ones.
        assert!(!terms.contains(&"ギルドの男さん"));
        assert!(terms.contains(&"ナツ伍長"));
        assert!(!terms.contains(&"マカロフ伍長"));
        // Honorifics only on names as the show writes them, not on hiragana spellings.
        assert!(!terms.contains(&"なつさん"));
        // The honorific entry has the short card; the name itself the description, spoilers out.
        let card = |term: &str| serde_json::to_string(&all.iter().find(|e| e[0] == term).unwrap()[5]).unwrap();
        assert!(card("ナツ").contains("A wizard of Fairy Tail") && !card("ナツ").contains("dragon"));
        assert!(!card("ナツさん").contains("A wizard of Fairy Tail") && card("ナツさん").contains("Natsu Dragneel"));
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let mut index = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("index.json").unwrap(), &mut index).unwrap();
        assert!(index.contains(TITLE) && index.contains("FAIRY TAIL"));
    }

    #[test]
    fn finds_the_characters_a_line_names() {
        let c = cast();
        let found = named_in(&c, "ナツ！ルーシィさんが待ってるよ", 10);
        assert_eq!(found, vec!["ナツ (Natsu Dragneel)".to_string(), "ルーシィ (Lucy Heartfilia)".to_string()]);
        assert!(names(&c).iter().any(|n| n.term == "ナツ・ドラグニル"));
        assert!(written_form("ナツドラグニル", "ナツ・ドラグニル") && !written_form("なつ", "ナツ・ドラグニル"));
        assert!(plausible_name("ナツ") && !plausible_name("ート") && !plausible_name("ッス") && !plausible_name(""));
    }

    #[test]
    fn a_portrait_got_after_the_dictionary_was_made_goes_into_a_new_copy() {
        let data = std::env::temp_dir().join(format!("telly-names-portraits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let backend = Backend::open(crate::backend::Dirs { local: data.clone(), roaming: data.clone() }).unwrap();
        let episodes = vec![crate::db::NewEpisode { number: 1, path: "/rips/Fairy Tail/01.mkv".into(), size: 1, duration: 1400.0 }];
        let show = crate::db::NewShow { folder: "/rips/Fairy Tail".into(), title: "Fairy Tail".into(), anilist_id: Some(6702), episodes, ..Default::default() };
        let id = backend.db.add_show(&show).unwrap();
        backend.db.touch_show(id).unwrap();
        save_cast(&backend, &Cast { fetched_at: now_ms(), ..cast() }).unwrap();
        refresh(&backend).unwrap();
        let first = backend.db.setting_str("names.revision");
        assert!(!first.is_empty());
        // Natsu's portrait comes in at the next look (AniList's image server was away the first time).
        std::fs::write(dir(&backend).join("img").join(ImageHandler::make_filename("1", "jpg")), b"portrait").unwrap();
        std::thread::sleep(Duration::from_millis(5));
        refresh(&backend).unwrap();
        assert_ne!(backend.db.setting_str("names.revision"), first);
        let zip = std::fs::read(zip_path(&backend)).unwrap();
        assert!(zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap().by_name("img/c1.jpg").is_ok());
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn dates_and_short_cards() {
        assert_eq!(today().len(), 10);
        let card = json!({ "type": "structured-content", "content": [
            { "tag": "div", "data": { "id": "name" }, "content": "ナツ" },
            { "tag": "details", "content": "long description" },
        ]});
        assert_eq!(short_card(&card)["content"].as_array().unwrap().len(), 1);
    }
}

