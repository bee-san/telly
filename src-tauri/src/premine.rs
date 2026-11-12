//! Premining: before you watch a show, the fewest words that take you to the
//! share of its words you choose to know, made into cards in one go, each with
//! the line it first comes up in. Episodes without subtitles are left out.
//!
//! - Words: each synced line is split by Lindera (IPADIC, built into Telly).
//!   Particles, auxiliaries, pronouns, この and その, numbers, symbols, proper
//!   nouns and the cast's names are left out. Where nouns run together, the
//!   longest stretch Hachidori knows is one word (魔導士 in 魔導士たち, rather
//!   than 魔 and 導).
//! - Only what's said counts. Closed captions also describe sounds （爆発音）,
//!   name who's speaking （ナツ） and carry song lyrics (♪): words there are
//!   left out, so a card's sentence and audio are someone speaking.
//! - Each word's headword, reading and frequency come from Hachidori, so
//!   they're what a lookup would show: Telly's own (own_hachidori.rs), or,
//!   when Telly is linked to the Hachidori in your browser, that one through
//!   the Yomitan API Hachidori Relay serves on port 19633.
//! - Anki says what you know. A note whose word field holds the headword, or
//!   its reading, with a card you've studied (young or mature), is a word you
//!   know. A word whose only cards are new is mined again.
//! - Cards are made in your Hachidori card format (its first Template), in a
//!   Premined subdeck of the deck from Settings (例文マイニング::Premined), with
//!   the line the word came from. Telly's
//!   card finisher then adds the moving picture, the audio and MiscInfo, and
//!   tags them as it does the cards you mine while watching.

use crate::backend::Backend;
use crate::db::{Episode, Show};
use crate::names::kana::{contains_japanese, contains_kanji, kata_to_hira};
use crate::subs::Line;
use crate::{anki, cards, jobs, known, names, own_hachidori};
use anyhow::{anyhow, Result};
use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Hachidori Relay's Yomitan API.
const API: &str = "http://127.0.0.1:19633";

/// Where to ask Hachidori: its relay, or TELLY_YOMITAN_API (a stand-in for tests).
fn api_address() -> String {
    std::env::var("TELLY_YOMITAN_API").unwrap_or_else(|_| API.to_string())
}
/// Words asked of the Hachidori in your browser at a time: each answer carries
/// every dictionary's entries, and the relay waits 30 seconds at most for one.
const LOOKUPS_PER_REQUEST: usize = 40;
/// Words asked of Telly's own Hachidori at a time. Its answers carry only the
/// best match of each (host.js termEntries), and its page has several lookups
/// going at once, so a bigger batch keeps it busy.
const OWN_LOOKUPS_PER_REQUEST: usize = 400;
/// The subdeck of your card format's deck that premined cards go in.
const SUBDECK: &str = "Premined";
const NO_RELAY: &str = "Premining needs Anki open, with Hachidori Relay. Open Anki and try again.";
const NO_OWN: &str = "Telly’s Hachidori isn’t running yet. Wait a moment, or use Restart Hachidori in Settings.";
const INSTALLING: &str = "Telly’s Hachidori is still installing your dictionaries. Words are worked out once it has them all.";
const NO_SHARING: &str = "Premining needs the Hachidori in your browser. Turn on sharing in it (Settings → Sharing) and try again.";

/// What Hachidori shows for a word: its headword, its reading, and its rank in
/// your first frequency dictionary that has it (lower is more common).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Term {
    pub headword: String,
    pub reading: String,
    pub rank: Option<i64>,
}

/// A word to premine, with the first line it comes up in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Word {
    pub headword: String,
    pub reading: String,
    pub rank: Option<i64>,
    /// Times it comes up in the show.
    pub count: usize,
    pub episode_id: i64,
    pub number: i64,
    pub start: f64,
    pub end: f64,
    /// The line around the word, as it's written there.
    pub before: String,
    pub word: String,
    pub after: String,
}

/// The words to premine in a show.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct List {
    /// Episodes that have no subtitles yet, so aren't counted.
    pub without_subs: Vec<i64>,
    /// Words you don't know yet, most used first.
    pub words: Vec<Word>,
    /// Uses of words in the show, and how many of them are words you know.
    pub uses: usize,
    pub known_uses: usize,
    /// Where cards go.
    pub deck: String,
}

/// What premining is doing, for the page to show.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub show: i64,
    pub working: bool,
    /// "Reading Episode 4", "Making cards · 3 of 40".
    pub step: String,
    pub progress: f64,
    pub list: Option<List>,
    pub error: String,
    /// After making cards: how many went into Anki, and why any didn't.
    pub added: usize,
    pub skipped: Vec<String>,
}

/// How much of an episode's words you know: the uses of words in it, and how
/// many of them are of words on cards you've studied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Share {
    pub uses: usize,
    pub known: usize,
}

/// Working out each episode's share of known words, for a show's page and the
/// Library. The shares themselves are kept in the database.
#[derive(Default)]
struct Shares {
    /// Why they couldn't be brought up to date ("" when they were).
    error: String,
    /// What's asked for next (a show's page asks for its show first; the
    /// Library for none in particular), and whether a worker is going.
    wanted: Option<Option<i64>>,
    working: bool,
}

#[derive(Default)]
pub struct Premining {
    status: Mutex<Status>,
    /// What the list in `status` was made from (list_inputs): it's made again
    /// when they change.
    list_from: Mutex<String>,
    /// The words premined while Telly is open. Their cards are new, so Anki
    /// doesn't count them yet, but they aren't listed again.
    made: Mutex<HashSet<(String, String)>>,
    shares: Mutex<Shares>,
    /// Hachidori's answers, by the text asked, for as long as Telly is open.
    /// Each episode's words are also saved, when its subtitles sync.
    terms: Mutex<HashMap<String, Option<Term>>>,
}

/// Your card format: the first Template in Hachidori's Settings → Anki.
#[derive(Clone, Debug, PartialEq)]
struct CardFormat {
    deck: String,
    model: String,
    /// Each field and its template, with Hachidori's {markers}.
    fields: Vec<(String, String)>,
}

impl CardFormat {
    /// The field the word goes in: the one whose template is {expression}.
    fn word_field(&self) -> String {
        let has = |t: &str, m: &str| t.to_lowercase().contains(m);
        self.fields
            .iter()
            .find(|(_, t)| has(t, "{expression}"))
            .or_else(|| self.fields.first())
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "Expression".into())
    }

    /// Every {marker} its templates use, lower case.
    fn markers(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (_, template) in &self.fields {
            for marker in markers_in(template) {
                if !out.contains(&marker) {
                    out.push(marker);
                }
            }
        }
        out
    }
}

// ---- Words, by Lindera ----

/// One token of a line.
#[derive(Clone, Debug, PartialEq)]
struct Token {
    surface: String,
    /// Byte range in the text.
    from: usize,
    to: usize,
    /// IPADIC's part of speech and its first three subdivisions.
    pos: Vec<String>,
    /// Its dictionary form, or the surface when IPADIC has none.
    base: String,
    /// Its reading in hiragana, or nothing when IPADIC has none.
    reading: String,
}

fn segmenter() -> Result<&'static Segmenter> {
    static SEGMENTER: OnceLock<Segmenter> = OnceLock::new();
    if let Some(s) = SEGMENTER.get() {
        return Ok(s);
    }
    let dictionary = load_dictionary("embedded://ipadic").map_err(|e| anyhow!("Telly’s Japanese dictionary didn’t load: {e}"))?;
    Ok(SEGMENTER.get_or_init(|| Segmenter::new(Mode::Normal, dictionary, None)))
}

fn tokens(text: &str) -> Result<Vec<Token>> {
    let mut out = Vec::new();
    for mut t in segmenter()?.segment(Cow::Borrowed(text)).map_err(|e| anyhow!("Telly couldn’t read “{text}”: {e}"))? {
        let details: Vec<String> = t.details().into_iter().map(str::to_string).collect();
        let surface = t.surface.to_string();
        let field = |i: usize| details.get(i).filter(|d| d.as_str() != "*").cloned();
        out.push(Token {
            base: field(6).unwrap_or_else(|| surface.clone()),
            reading: field(7).map(|r| kata_to_hira(&r)).unwrap_or_default(),
            pos: details.iter().take(4).cloned().collect(),
            from: t.byte_start,
            to: t.byte_end,
            surface,
        });
    }
    Ok(out)
}

/// Whether a token is a word to learn: not a particle, an auxiliary, a
/// pronoun, この or その, a symbol, a number, a name, a suffix, an
/// interjection, or a noun or verb that's only there for the grammar (わけ,
/// こと, ている's いる).
fn is_word(token: &Token) -> bool {
    let at = |i: usize| token.pos.get(i).map(String::as_str).unwrap_or("");
    let kept = match at(0) {
        "名詞" => !matches!(at(1), "数" | "非自立" | "接尾" | "固有名詞" | "代名詞" | "特殊"),
        "動詞" | "形容詞" => !matches!(at(1), "非自立" | "接尾"),
        "副詞" | "接続詞" => true,
        _ => false,
    };
    // One kana alone is almost always a word cut short.
    kept && contains_japanese(&token.base) && (contains_kanji(&token.base) || token.base.chars().count() > 1)
}

/// Whether a token carries on the verb or adjective before it: an auxiliary
/// (た, ない, たい), a suffix (られる, させる), or て, で and ば.
fn is_ending(token: &Token) -> bool {
    let at = |i: usize| token.pos.get(i).map(String::as_str).unwrap_or("");
    match at(0) {
        "助動詞" => true,
        "動詞" => at(1) == "接尾",
        "助詞" => at(1) == "接続助詞" && matches!(token.surface.as_str(), "て" | "で" | "ば"),
        _ => false,
    }
}

/// Stretches each word's byte range in `text` over the endings that inflect
/// it, so the whole form is one stretch: 食べ in 食べられなくて, 食べれ in
/// 食べれば. A word that isn't a verb or an adjective stays as it is.
pub(crate) fn with_endings(text: &str, spans: &mut [(usize, usize)]) -> Result<()> {
    let tokens = tokens(text)?;
    for span in spans {
        let Some(last) = tokens.iter().position(|t| t.to == span.1) else { continue };
        if !matches!(tokens[last].pos.first().map(String::as_str), Some("動詞" | "形容詞")) {
            continue;
        }
        for t in &tokens[last + 1..] {
            if t.from != span.1 || !is_ending(t) {
                break;
            }
            span.1 = t.to;
        }
    }
    Ok(())
}

/// The dictionary form of `text` when it's one word in some form: 食べる for
/// 食べた and 食べられる.
pub(crate) fn dictionary_form(text: &str) -> Option<String> {
    let tokens = tokens(text).ok()?;
    let (first, rest) = tokens.split_first()?;
    (is_word(first) && rest.iter().all(is_ending)).then(|| first.base.clone())
}

/// A word in a line: one token, or nouns that run together, which may hold a
/// longer word than any of them.
#[derive(Clone, Debug, PartialEq)]
enum Candidate {
    One { base: String, from: usize, to: usize },
    /// Each token's byte range, and its dictionary form when it's a word alone.
    Run(Vec<(usize, usize, Option<String>)>),
}

impl Candidate {
    /// The texts to ask Hachidori about: the word, or every stretch of the run.
    fn texts(&self, line: &str) -> Vec<String> {
        match self {
            Candidate::One { base, .. } => vec![base.clone()],
            Candidate::Run(tokens) => {
                let mut out: Vec<String> = tokens.iter().filter_map(|t| t.2.clone()).collect();
                for a in 0..tokens.len() {
                    for b in a + 1..tokens.len() {
                        out.push(line[tokens[a].0..tokens[b].1].to_string());
                    }
                }
                out
            }
        }
    }

    /// Its words, as (text asked, byte range): along a run, the longest stretch
    /// `known` has as a word, else the token alone.
    fn words(&self, line: &str, known: impl Fn(&str) -> bool) -> Vec<(String, usize, usize)> {
        match self {
            Candidate::One { base, from, to } => vec![(base.clone(), *from, *to)],
            Candidate::Run(tokens) => {
                let mut out = Vec::new();
                let mut k = 0;
                while k < tokens.len() {
                    let longest = (k + 1..tokens.len()).rev().find(|&m| known(&line[tokens[k].0..tokens[m].1]));
                    if let Some(m) = longest {
                        out.push((line[tokens[k].0..tokens[m].1].to_string(), tokens[k].0, tokens[m].1));
                        k = m + 1;
                    } else {
                        if let Some(base) = &tokens[k].2 {
                            out.push((base.clone(), tokens[k].0, tokens[k].1));
                        }
                        k += 1;
                    }
                }
                out
            }
        }
    }
}

/// The words in a line, in order.
fn candidates(text: &str) -> Result<Vec<Candidate>> {
    let tokens = tokens(text)?;
    let joins = |t: &Token| t.pos.first().is_some_and(|p| p == "名詞") && t.pos.get(1).is_none_or(|p| p != "数");
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        // Nouns (and a prefix before them) that touch: 魔+導+士+たち, 大+魔闘+演舞.
        let starts_run = joins(&tokens[i]) || tokens[i].pos.first().is_some_and(|p| p == "接頭詞");
        let mut j = i;
        if starts_run {
            while j + 1 < tokens.len() && j + 1 - i < 4 && joins(&tokens[j + 1]) && tokens[j + 1].from == tokens[j].to {
                j += 1;
            }
        }
        if j > i {
            out.push(Candidate::Run(tokens[i..=j].iter().map(|t| (t.from, t.to, is_word(t).then(|| t.base.clone()))).collect()));
        } else if is_word(&tokens[i]) {
            out.push(Candidate::One { base: tokens[i].base.clone(), from: tokens[i].from, to: tokens[i].to });
        }
        i = j + 1;
    }
    Ok(out)
}

// ---- Hachidori ----

/// Asks Hachidori a Yomitan API request (`path` as Hachidori Relay serves it):
/// Telly's own, unless Telly is linked to the one in your browser, or a
/// stand-in is set for testing.
fn api(backend: &Backend, path: &str, body: Value) -> Result<Value> {
    if asks_own(backend) {
        if !backend.own_hachidori.answering() {
            return Err(anyhow!(NO_OWN));
        }
        // Words saved now would lack the dictionaries still to come, and stay that way.
        if own_hachidori::installing(backend) {
            return Err(anyhow!(INSTALLING));
        }
        return own_api(backend, path, body);
    }
    relay_api(path, body)
}

/// Whether premining asks Telly's own Hachidori, rather than the relay.
fn asks_own(backend: &Backend) -> bool {
    std::env::var_os("TELLY_YOMITAN_API").is_none() && !backend.db.setting_bool("hachidori.link", false)
}

/// Whether Telly's own Hachidori can be asked: its page is there and has every dictionary.
fn own_ready(backend: &Backend) -> bool {
    backend.own_hachidori.answering() && !own_hachidori::installing(backend)
}

/// The request as Hachidori's own API code takes it (its hd_api_* messages),
/// answered the way Hachidori Relay answers it.
fn own_api(backend: &Backend, path: &str, body: Value) -> Result<Value> {
    let ask = |request: Value| backend.own_hachidori.ask(request, Duration::from_secs(120));
    match path {
        // Only the best match of each, and only what term_of reads of it (host.js termEntries).
        "/termEntries" => Ok(ask(json!({ "type": "telly_term_entries", "terms": body["term"] }))?["results"].take()),
        "/ankiCardFormats" => Ok(ask(json!({ "type": "hd_api_anki_card_formats" }))?["cardFormats"].take()),
        "/ankiFields" => ask(json!({
            "type": "hd_api_anki_fields",
            "text": body["text"],
            "entryType": body["type"],
            "markers": body["markers"],
            "maxEntries": body["maxEntries"],
            "includeMedia": body["includeMedia"],
        })),
        _ => Err(anyhow!("Telly’s Hachidori doesn’t answer {path}")),
    }
}

fn relay_api(path: &str, body: Value) -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(90))).http_status_as_error(false).build().into();
    let mut response = agent.post(format!("{}{path}", api_address())).send_json(body).map_err(|_| anyhow!(NO_RELAY))?;
    let value: Value = response
        .body_mut()
        .with_config()
        .limit(512 * 1024 * 1024)
        .read_json()
        .map_err(|e| anyhow!("Hachidori’s answer couldn’t be read ({e})"))?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(anyhow!(if error.contains("No sharing Hachidori") { NO_SHARING.to_string() } else { format!("Hachidori: {error}") }));
    }
    Ok(value)
}

/// What Hachidori shows for `text`, when its best match is all of `text`.
fn term_of(text: &str, result: &Value) -> Option<Term> {
    let matched = result.get("originalTextLength")?.as_u64()? as usize;
    if matched != text.encode_utf16().count() {
        return None;
    }
    let entry = result.get("dictionaryEntries")?.as_array()?.first()?;
    let head = entry.get("headwords")?.as_array()?.first()?;
    let headword = head.get("term")?.as_str()?.to_string();
    let reading = head.get("reading").and_then(Value::as_str).unwrap_or("").to_string();
    let mut ranks: Vec<(i64, i64)> = entry
        .get("frequencies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| Some((f.get("dictionaryIndex").and_then(Value::as_i64).unwrap_or(0), rank_of(f)?)))
        .filter(|(_, v)| *v > 0)
        .collect();
    ranks.sort();
    Some(Term { headword, reading, rank: ranks.first().map(|(_, v)| *v) })
}

/// A frequency as a rank (lower is more common). A dictionary that counts
/// occurrences (higher is more common) has no rank, unless it shows one: Bee's
/// Frequency Dictionary calls itself occurrence-based but shows 本 as "#459".
fn rank_of(frequency: &Value) -> Option<i64> {
    if frequency.get("frequencyMode").and_then(Value::as_str) != Some("occurrence-based") {
        return frequency.get("frequency").and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|x| x.round() as i64)));
    }
    let shown = frequency.get("displayValue").and_then(Value::as_str)?.trim().strip_prefix('#')?;
    let digits: String = shown.chars().take_while(|c| c.is_ascii_digit() || *c == ',').filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Looks up the texts Telly hasn't asked Hachidori about yet, and says how many those were.
fn look_up(backend: &Backend, texts: &[String], step: &dyn Fn(f64)) -> Result<usize> {
    let cache = &backend.premining.terms;
    let new: Vec<String> = {
        let known = cache.lock().map_err(|_| anyhow!("premining stopped"))?;
        let mut seen = HashSet::new();
        texts.iter().filter(|t| !known.contains_key(*t) && seen.insert((*t).clone())).cloned().collect()
    };
    let per_request = if asks_own(backend) { OWN_LOOKUPS_PER_REQUEST } else { LOOKUPS_PER_REQUEST };
    for (k, chunk) in new.chunks(per_request).enumerate() {
        step(k as f64 * per_request as f64 / new.len().max(1) as f64);
        let answer = api(backend, "/termEntries", json!({ "term": chunk }))?;
        let results = answer.as_array().cloned().unwrap_or_default();
        let mut known = cache.lock().map_err(|_| anyhow!("premining stopped"))?;
        for (i, text) in chunk.iter().enumerate() {
            known.insert(text.clone(), results.get(i).and_then(|r| term_of(text, r)));
        }
    }
    Ok(new.len())
}

fn card_format(backend: &Backend) -> Result<CardFormat> {
    let formats = api(backend, "/ankiCardFormats", json!({}))?;
    let first = formats
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!("Hachidori has no card format yet. Set one up in its Settings → Anki."))?;
    let text = |k: &str| first.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let fields = first
        .get("fields")
        .and_then(Value::as_object)
        .map(|f| f.iter().map(|(name, v)| (name.clone(), v.get("value").and_then(Value::as_str).unwrap_or("").to_string())).collect())
        .unwrap_or_default();
    let format = CardFormat { deck: text("deck"), model: text("model"), fields };
    if format.deck.is_empty() || format.model.is_empty() {
        return Err(anyhow!("Hachidori’s card format has no deck or note type. Choose them in its Settings → Anki."));
    }
    Ok(format)
}

// ---- Anki ----

/// The field your card format puts the word in.
pub fn word_field(backend: &Backend) -> Result<String> {
    Ok(card_format(backend)?.word_field())
}

/// Whether a word is among `studied`: a note there holds its headword, or its reading
/// where the note has the word in kana (する for 為る, こと for 事).
fn known(studied: &HashSet<String>, term: &Term) -> bool {
    studied.contains(&term.headword) || (!term.reading.is_empty() && studied.contains(&term.reading))
}

// ---- The list ----

/// A use of a word: which episode and line, and where in the line.
struct Use {
    episode: usize,
    line: usize,
    from: usize,
    to: usize,
}

/// One use of a word in an episode, as Hachidori sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedUse {
    /// Its line, counting only spoken lines (not signs), and its byte range there.
    pub(crate) line: usize,
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) headword: String,
    pub(crate) reading: String,
    rank: Option<i64>,
}

/// An episode's words, worked out when its subtitles sync, for the lines they
/// were found in.
#[derive(Serialize, Deserialize)]
struct Saved {
    lines: u64,
    uses: Vec<SavedUse>,
}

pub fn words_path(backend: &Backend, episode_id: i64) -> std::path::PathBuf {
    backend.dirs.subs().join(format!("{episode_id}.words.json"))
}

/// Stands for the lines' text: when it changes, the saved words are redone.
fn fingerprint(lines: &[Line]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in lines {
        line.text.hash(&mut hasher);
    }
    hasher.finish()
}

pub(crate) fn spoken_lines(backend: &Backend, episode_id: i64) -> Vec<Line> {
    jobs::read_lines(backend, episode_id).into_iter().filter(|l| !l.sign).collect()
}

/// Whether a use of a word counts: it's said, and isn't one of the cast's names.
fn counts(text: &str, u: &SavedUse, cast: &HashSet<String>) -> bool {
    said(text, u.from) && !cast.contains(&u.headword) && !cast.contains(&text[u.from..u.to])
}

/// An episode's saved words, when they were worked out for these very lines.
pub(crate) fn saved_uses(backend: &Backend, episode_id: i64, lines: &[Line]) -> Option<Vec<SavedUse>> {
    let saved = std::fs::read(words_path(backend, episode_id)).ok().and_then(|b| serde_json::from_slice::<Saved>(&b).ok())?;
    (saved.lines == fingerprint(lines)).then_some(saved.uses)
}

/// An episode's words: saved ones, or worked out (by Lindera and Hachidori) and saved.
fn episode_uses(backend: &Backend, episode_id: i64, lines: &[Line]) -> Result<Vec<SavedUse>> {
    if let Some(uses) = saved_uses(backend, episode_id, lines) {
        return Ok(uses);
    }
    let print = fingerprint(lines);
    let path = words_path(backend, episode_id);
    let started = Instant::now();
    let mut found: Vec<(usize, Candidate)> = Vec::new();
    for (l, line) in lines.iter().enumerate() {
        for candidate in candidates(&line.text)? {
            found.push((l, candidate));
        }
    }
    let texts: Vec<String> = found.iter().flat_map(|(l, c)| c.texts(&lines[*l].text)).collect();
    let read = started.elapsed();
    let asked = look_up(backend, &texts, &|_| {})?;
    eprintln!(
        "words: episode {episode_id}: {} lines, {} lookups, {asked} asked of Hachidori: {:.2} s ({:.0} ms reading the lines)",
        lines.len(),
        texts.len(),
        started.elapsed().as_secs_f64(),
        read.as_secs_f64() * 1000.0
    );
    let terms = backend.premining.terms.lock().map_err(|_| anyhow!("premining stopped"))?;
    let mut uses = Vec::new();
    for (l, candidate) in found {
        for (asked, from, to) in candidate.words(&lines[l].text, |t| matches!(terms.get(t), Some(Some(_)))) {
            if let Some(Some(term)) = terms.get(&asked) {
                uses.push(SavedUse { line: l, from, to, headword: term.headword.clone(), reading: term.reading.clone(), rank: term.rank });
            }
        }
    }
    drop(terms);
    std::fs::create_dir_all(backend.dirs.subs())?;
    std::fs::write(&path, serde_json::to_vec(&Saved { lines: print, uses: uses.clone() })?)?;
    crate::search::index(backend, episode_id);
    Ok(uses)
}

/// Works out and saves an episode's words once its subtitles have synced, so
/// premining doesn't have to.
pub fn prepare(backend: &Backend, episode_id: i64) {
    let lines = spoken_lines(backend, episode_id);
    // Line search has the lines as they are now, whether or not their words can be worked out.
    crate::search::index(backend, episode_id);
    if lines.is_empty() {
        return;
    }
    match episode_uses(backend, episode_id, &lines) {
        Ok(uses) => {
            let show = backend.db.episode(episode_id).ok().flatten().and_then(|e| backend.db.show(e.show_id).ok().flatten());
            let cast: HashSet<String> = show.map(|s| names::show_names(backend, &s).into_iter().map(|n| n.term).collect()).unwrap_or_default();
            let share = share_of(&lines, uses, &cast, &known::words(backend).known);
            let _ = backend.db.set_word_score(episode_id, share.uses as i64, share.known as i64, &known::stamp(backend));
        }
        Err(e) => eprintln!("premining words for episode {episode_id} not saved: {e}"),
    }
}

/// Whether the word at byte `at` of a caption is said: not in brackets, where
/// captions describe a sound （爆発音） or name the speaker, and not in a
/// line of a song (♪).
pub(crate) fn said(text: &str, at: usize) -> bool {
    !bracketed(text, at) && !text.contains('♪')
}

/// Whether byte `at` of a caption is inside brackets.
pub(crate) fn bracketed(text: &str, at: usize) -> bool {
    let mut depth = 0;
    for c in text[..at].chars() {
        match c {
            '（' | '(' => depth += 1,
            '）' | ')' if depth > 0 => depth -= 1,
            _ => {}
        }
    }
    depth > 0
}

/// The words to premine in `show`.
fn make_list(backend: &Backend, show: &Show, step: &dyn Fn(&str, f64)) -> Result<List> {
    let mut episodes: Vec<Episode> = backend.db.episodes(show.id)?;
    episodes.sort_by_key(|e| e.number);
    let deck = premined_deck(backend, show);

    step("Reading your card format", 0.0);
    card_format(backend)?;
    let cast: HashSet<String> = names::show_names(backend, show).into_iter().map(|n| n.term).collect();

    // Each use of each word, episode by episode; an episode without subtitles adds none.
    let mut lines: Vec<Vec<Line>> = Vec::new();
    let mut without_subs = Vec::new();
    let mut uses: Vec<(Term, Use)> = Vec::new();
    for (episode, e) in episodes.iter().enumerate() {
        step(&format!("Finding the words in Episode {}", e.number), 0.8 * episode as f64 / episodes.len() as f64);
        let episode_lines = spoken_lines(backend, e.id);
        if episode_lines.is_empty() {
            without_subs.push(e.number);
            lines.push(episode_lines);
            continue;
        }
        for u in episode_uses(backend, e.id, &episode_lines)? {
            let text = &episode_lines[u.line].text;
            if counts(text, &u, &cast) {
                let term = Term { headword: u.headword, reading: u.reading, rank: u.rank };
                uses.push((term, Use { episode, line: u.line, from: u.from, to: u.to }));
            }
        }
        lines.push(episode_lines);
    }

    // One row per word, with how often it comes up and its first line.
    let mut order: Vec<Term> = Vec::new();
    let mut seen: HashMap<(String, String), (usize, usize)> = HashMap::new();
    for (i, (term, _)) in uses.iter().enumerate() {
        let key = (term.headword.clone(), term.reading.clone());
        match seen.get_mut(&key) {
            Some((_, count)) => *count += 1,
            None => {
                seen.insert(key, (i, 1));
                order.push(term.clone());
            }
        }
    }
    step("Checking your cards in Anki", 0.8);
    known::refresh(backend)?;
    let have = known::words(backend);

    let mut known_uses = 0;
    let mut words: Vec<(usize, Word)> = Vec::new();
    let made = backend.premining.made.lock().map(|m| m.clone()).unwrap_or_default();
    for term in &order {
        // A word just premined isn't listed again; its card counts as known, as it did in the list it was made from.
        if made.contains(&(term.headword.clone(), term.reading.clone())) {
            known_uses += seen[&(term.headword.clone(), term.reading.clone())].1;
            continue;
        }
        // A word you have a card for isn't listed; it counts as known when Settings says its card does.
        if known(&have.studied, term) {
            if known(&have.known, term) {
                known_uses += seen[&(term.headword.clone(), term.reading.clone())].1;
            }
            continue;
        }
        let (first, count) = seen[&(term.headword.clone(), term.reading.clone())];
        let at = &uses[first].1;
        let line = &lines[at.episode][at.line];
        let episode = &episodes[at.episode];
        words.push((
            first,
            Word {
                headword: term.headword.clone(),
                reading: term.reading.clone(),
                rank: term.rank,
                count,
                episode_id: episode.id,
                number: episode.number,
                start: line.start,
                end: line.end,
                before: line.text[..at.from].replace('\n', ""),
                word: line.text[at.from..at.to].to_string(),
                after: line.text[at.to..].replace('\n', ""),
            },
        ));
    }
    sort_words(&mut words);
    Ok(List { without_subs, words: words.into_iter().map(|(_, w)| w).collect(), uses: uses.len(), known_uses, deck })
}

/// Most useful first: the words that come up most in the show, then the
/// most common anywhere (by your frequency dictionary), then the first seen.
fn sort_words(words: &mut [(usize, Word)]) {
    words.sort_by(|(fa, a), (fb, b)| {
        b.count.cmp(&a.count).then(a.rank.unwrap_or(i64::MAX).cmp(&b.rank.unwrap_or(i64::MAX))).then(fa.cmp(fb))
    });
}

// ---- Each episode's share of known words ----

/// Each scored episode's share, and why the shares couldn't be brought up to
/// date ("" when they were).
pub fn shares(backend: &Backend) -> (HashMap<i64, Share>, String) {
    let scores = backend.db.word_scores().unwrap_or_default();
    let shares = scores.into_iter().map(|(id, (uses, known, _))| (id, Share { uses: uses as usize, known: known as usize })).collect();
    (shares, backend.premining.shares.lock().map(|s| s.error.clone()).unwrap_or_default())
}

/// Brings every episode's share of known words up to date, in the background:
/// Telly's copy of Anki's words first, then the shares of `first`'s episodes (a
/// show's page asks when it opens), then the rest of the library's, working
/// out the words of any episode that has none saved. Asking again while it's
/// going starts it over with the new `first`. When Telly's own Hachidori
/// couldn't be asked (it was starting, or installing dictionaries), it goes
/// again once it can be.
pub fn start_shares(backend: &Arc<Backend>, first: Option<i64>) {
    {
        let Ok(mut shares) = backend.premining.shares.lock() else { return };
        shares.wanted = Some(first);
        if shares.working {
            return;
        }
        shares.working = true;
    }
    let worker = backend.clone();
    std::thread::spawn(move || loop {
        let next = worker.premining.shares.lock().ok().and_then(|mut s| {
            let next = s.wanted.take();
            s.working = next.is_some();
            next
        });
        let Some(first) = next else { break };
        let error = work_out_shares(&worker, first).err().map(|e| format!("{e:#}")).unwrap_or_default();
        let wait = !error.is_empty() && asks_own(&worker) && !own_ready(&worker);
        if let Ok(mut shares) = worker.premining.shares.lock() {
            shares.error = error;
        }
        if wait {
            let asked = |w: &Backend| w.premining.shares.lock().is_ok_and(|s| s.wanted.is_some());
            while !own_ready(&worker) && !asked(&worker) {
                std::thread::sleep(Duration::from_secs(2));
            }
            if let Ok(mut shares) = worker.premining.shares.lock() {
                shares.wanted.get_or_insert(first);
            }
        }
    });
}

/// How much of an episode's words (`uses`, found in `lines`) are among `known_words`.
fn share_of(lines: &[Line], uses: Vec<SavedUse>, cast: &HashSet<String>, known_words: &HashSet<String>) -> Share {
    let mut share = Share::default();
    for u in uses {
        if counts(&lines[u.line].text, &u, cast) {
            share.uses += 1;
            share.known += known(known_words, &Term { headword: u.headword, reading: u.reading, rank: u.rank }) as usize;
        }
    }
    share
}

fn work_out_shares(backend: &Backend, first: Option<i64>) -> Result<()> {
    // With Anki closed the copy Telly has stands, and the page says why it may be old.
    let asked = known::refresh(backend);
    let known_words = known::words(backend).known;
    // A share worked out from another state of Anki's words, or another choice in Settings, is worked out again.
    let stamp = known::stamp(backend);
    let scores = backend.db.word_scores()?;
    let mut shows = backend.db.shows()?;
    shows.sort_by_key(|s| Some(s.id) != first);
    // Why Hachidori couldn't work out an episode's words. The rest of this
    // pass then uses saved words only; the next (the Library and a show's
    // page each ask for one as they open) tries again.
    let mut unasked: Option<anyhow::Error> = None;
    for show in shows {
        let cast: HashSet<String> = names::show_names(backend, &show).into_iter().map(|n| n.term).collect();
        for e in backend.db.episodes(show.id)? {
            // Asked again (another show's page opened): start over, that show first.
            if backend.premining.shares.lock().is_ok_and(|s| s.wanted.is_some()) {
                return Ok(());
            }
            if scores.get(&e.id).is_some_and(|(_, _, of)| *of == stamp) {
                continue;
            }
            let lines = spoken_lines(backend, e.id);
            if lines.is_empty() {
                continue;
            }
            // Words not saved yet (subtitles that synced while Hachidori wasn't
            // there to ask) are worked out here: the open show's first, then the
            // rest of the library's, so every show has its share.
            let uses = match saved_uses(backend, e.id, &lines) {
                Some(uses) => uses,
                None if unasked.is_some() => continue,
                None => match episode_uses(backend, e.id, &lines) {
                    Ok(uses) => uses,
                    Err(error) => {
                        unasked = Some(error);
                        continue;
                    }
                },
            };
            let share = share_of(&lines, uses, &cast, &known_words);
            backend.db.set_word_score(e.id, share.uses as i64, share.known as i64, &stamp)?;
        }
    }
    asked?;
    unasked.map_or(Ok(()), Err)
}

// ---- Cards ----

/// Where `show`'s premined cards go: a Premined subdeck of the show's deck.
fn premined_deck(backend: &Backend, show: &Show) -> String {
    format!("{}::{SUBDECK}", own_hachidori::deck_for(backend, &show.title))
}

/// The {markers} in a template, lower case, in order.
fn markers_in(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if close > 0 && after[..close].chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => {
                out.push(after[..close].to_ascii_lowercase());
                rest = &after[close + 1..];
            }
            _ => rest = after,
        }
    }
    out
}

/// A template with each {marker} replaced by its value (nothing for one without).
fn render(template: &str, values: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if close > 0 && after[..close].chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => {
                out.push_str(values.get(&after[..close].to_ascii_lowercase()).map(String::as_str).unwrap_or(""));
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A token's text cut into kana and the kanji the reading belongs to:
/// 減っ (へっ) is 減 (へ) and っ.
fn ruby(surface: &str, reading: &str) -> Vec<(String, String)> {
    let s: Vec<char> = surface.chars().collect();
    let r: Vec<char> = reading.chars().collect();
    let same = |a: char, b: char| kata_to_hira(&a.to_string()) == b.to_string();
    let mut head = 0;
    while head < s.len() && head < r.len() && !contains_kanji(&s[head].to_string()) && same(s[head], r[head]) {
        head += 1;
    }
    let mut tail = 0;
    while tail < s.len() - head
        && tail < r.len() - head
        && !contains_kanji(&s[s.len() - 1 - tail].to_string())
        && same(s[s.len() - 1 - tail], r[r.len() - 1 - tail])
    {
        tail += 1;
    }
    let core: String = s[head..s.len() - tail].iter().collect();
    let core_reading: String = r[head..r.len() - tail].iter().collect();
    if core.is_empty() || core_reading.is_empty() {
        return vec![(surface.to_string(), String::new())];
    }
    let mut out = Vec::new();
    if head > 0 {
        out.push((s[..head].iter().collect(), String::new()));
    }
    out.push((core, core_reading));
    if tail > 0 {
        out.push((s[s.len() - tail..].iter().collect(), String::new()));
    }
    out
}

/// `text` with furigana over its kanji, as IPADIC reads it.
fn furigana(text: &str, plain: bool) -> Result<String> {
    let mut segments: Vec<(String, String)> = Vec::new();
    let mut at = 0;
    for token in tokens(text)? {
        if token.from > at {
            segments.push((text[at..token.from].to_string(), String::new()));
        }
        if contains_kanji(&token.surface) && !token.reading.is_empty() {
            segments.extend(ruby(&token.surface, &token.reading));
        } else {
            segments.push((token.surface.clone(), String::new()));
        }
        at = token.to;
    }
    if at < text.len() {
        segments.push((text[at..].to_string(), String::new()));
    }
    Ok(furigana_text(&segments, plain))
}

/// Text and readings as HTML ruby, or Anki's 漢字[かんじ] when `plain`, as
/// Hachidori's furiganaHtml writes them (a space before each reading but the
/// first, in the plain form).
fn furigana_text(segments: &[(String, String)], plain: bool) -> String {
    segments
        .iter()
        .enumerate()
        .map(|(i, (t, r))| match (r.is_empty(), plain) {
            (true, _) => escape(t),
            (false, true) => format!("{}{}[{}]", if i > 0 { " " } else { "" }, escape(t), escape(r)),
            (false, false) => format!("<ruby>{}<rt>{}</rt></ruby>", escape(t), escape(r)),
        })
        .collect()
}

/// The markers that come from the line, made by Telly the way Hachidori makes
/// them when you mine a word you hovered.
fn sentence_markers(word: &Word) -> Result<HashMap<String, String>> {
    let (before, body, after) = (escape(&word.before), escape(&word.word), escape(&word.after));
    let mut values = HashMap::new();
    values.insert("sentence".into(), format!("{before}<b>{body}</b>{after}"));
    values.insert("cloze-prefix".into(), before);
    values.insert("cloze-body".into(), body);
    values.insert("cloze-suffix".into(), after);
    // The word itself as Hachidori reads it (魔導士 is まどうし, where IPADIC
    // reads 魔, 導 and 士 apart), unless it's inflected here.
    let as_written = word.word == word.headword && !word.reading.is_empty() && contains_kanji(&word.word);
    for (marker, plain) in [("sentence-furigana", false), ("sentence-furigana-plain", true)] {
        let body = if as_written { furigana_text(&ruby(&word.word, &word.reading), plain) } else { furigana(&word.word, plain)? };
        let html = format!("{}<b>{}</b>{}", furigana(&word.before, plain)?, body, furigana(&word.after, plain)?);
        values.insert(marker.into(), html);
    }
    Ok(values)
}

/// Makes one card in Anki; returns its note.
fn make_card(backend: &Backend, url: &str, format: &CardFormat, deck: &str, word: &Word) -> Result<i64> {
    let answer = api(
        backend,
        "/ankiFields",
        json!({ "text": word.headword, "type": "term", "markers": format.markers(), "maxEntries": 1, "includeMedia": true }),
    )?;
    let first = answer
        .get("fields")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("Hachidori has no entry for it"))?;
    let mut values: HashMap<String, String> = first.iter().map(|(k, v)| (k.to_lowercase(), v.as_str().unwrap_or("").to_string())).collect();
    values.extend(sentence_markers(word)?);
    for kind in ["audioMedia", "dictionaryMedia"] {
        for media in answer.get(kind).and_then(Value::as_array).into_iter().flatten() {
            let (Some(name), Some(data)) = (media.get("ankiFilename").and_then(Value::as_str), media.get("content").and_then(Value::as_str)) else {
                continue;
            };
            anki::invoke(url, "storeMediaFile", json!({ "filename": name, "data": data }))?;
        }
    }
    let fields: Map<String, Value> = format.fields.iter().map(|(name, template)| (name.clone(), Value::String(render(template, &values)))).collect();
    let note = anki::invoke(
        url,
        "addNote",
        json!({ "note": {
            "deckName": deck,
            "modelName": format.model,
            "fields": fields,
            "tags": ["telly::premine"],
            // A word whose only cards are new gets another, with this show's line.
            "options": { "allowDuplicate": true },
        }}),
    )?;
    note.as_i64().ok_or_else(|| anyhow!("Anki didn’t make the card"))
}

fn make_cards(backend: &Backend, words: &[Word], step: &dyn Fn(&str, f64)) -> Result<(usize, Vec<String>)> {
    step("Reading your card format", 0.0);
    let format = card_format(backend)?;
    let episode = words.first().map(|w| w.episode_id).unwrap_or_default();
    let show = backend.db.episode(episode)?.and_then(|e| backend.db.show(e.show_id).ok().flatten());
    let show = show.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let deck = premined_deck(backend, &show);
    let url = cards::anki_url(backend);
    anki::invoke(&url, "createDeck", json!({ "deck": deck }))?;
    let padding = backend.db.setting_str("clip.padding");
    let mut lines: HashMap<i64, Vec<Line>> = HashMap::new();
    let (mut added, mut skipped) = (0, Vec::new());
    for (i, word) in words.iter().enumerate() {
        step(&format!("Making cards · {} of {}", i + 1, words.len()), i as f64 / words.len().max(1) as f64);
        match make_card(backend, &url, &format, &deck, word) {
            Ok(note) => {
                added += 1;
                // The moving picture and the audio, by the card finisher, from the line as it plays.
                let nudge = cards::nudge(backend, word.episode_id);
                let episode_lines = lines.entry(word.episode_id).or_insert_with(|| cards::played_lines(backend, word.episode_id));
                let line = word_line(episode_lines, word, nudge);
                let (start, end) = cards::clip_bounds(episode_lines, &line, &padding);
                backend.db.add_mine_job(note, word.episode_id, start, end, &line.text)?;
            }
            Err(e) => skipped.push(format!("{}: {e:#}", word.headword)),
        }
    }
    backend.cards.poke();
    Ok((added, skipped))
}

/// The line a word was found in, among an episode's lines as they play
/// (`nudge` after the file's times): the spoken one with its times, not a
/// sign that starts with it.
fn word_line(lines: &[Line], word: &Word, nudge: f64) -> Line {
    let (start, end) = (word.start + nudge, word.end + nudge);
    lines
        .iter()
        .find(|l| !l.sign && (l.start - start).abs() < 0.01 && (l.end - end).abs() < 0.01)
        .cloned()
        .unwrap_or(Line { start, end, text: format!("{}{}{}", word.before, word.word, word.after), ruby: vec![], speaker: String::new(), sign: false })
}

// ---- What the page calls ----

fn set(backend: &Backend, change: impl FnOnce(&mut Status)) {
    if let Ok(mut status) = backend.premining.status.lock() {
        change(&mut status);
    }
}

pub fn status(backend: &Backend) -> Status {
    backend.premining.status.lock().map(|s| s.clone()).unwrap_or_default()
}

/// Starts finding the words to premine in a show, unless that list is being
/// made or made already.
/// What a show's list is made from: each episode's saved words, and the
/// state of your known words.
fn list_inputs(backend: &Backend, show_id: i64) -> String {
    let mut inputs = known::stamp(backend);
    for e in backend.db.episodes(show_id).unwrap_or_default() {
        let saved = std::fs::metadata(words_path(backend, e.id)).and_then(|m| m.modified()).ok();
        inputs.push_str(&format!(" {}:{:?}", e.id, saved));
    }
    inputs
}

/// Makes the list of words to premine in a show, in the background. One made
/// already is kept while what it was made from stays the same. While another
/// show's list or cards are being made, that's what's said; the page asks again.
pub fn start_list(backend: &Arc<Backend>, show_id: i64) -> Result<Status> {
    let show = backend.db.show(show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    let inputs = list_inputs(backend, show_id);
    {
        let mut status = backend.premining.status.lock().map_err(|_| anyhow!("premining stopped"))?;
        let unchanged = backend.premining.list_from.lock().is_ok_and(|from| *from == inputs);
        if status.working || (status.show == show_id && status.list.is_some() && unchanged) {
            return Ok(status.clone());
        }
        *status = Status { show: show_id, working: true, step: "Starting".into(), ..Default::default() };
    }
    let worker = backend.clone();
    std::thread::spawn(move || {
        let step = |text: &str, progress: f64| set(&worker, |s| (s.step, s.progress) = (text.to_string(), progress));
        let result = make_list(&worker, &show, &step);
        // Working the words out may have saved some: the list is of what's saved now.
        if let Ok(mut from) = worker.premining.list_from.lock() {
            *from = list_inputs(&worker, show.id);
        }
        set(&worker, |s| {
            s.working = false;
            match result {
                Ok(list) => s.list = Some(list),
                Err(e) => s.error = format!("{e:#}"),
            }
        });
    });
    Ok(status(backend))
}

/// Starts making cards for `words`. When it's done, they drop out of the list
/// and count as known.
pub fn start_cards(backend: &Arc<Backend>, words: Vec<Word>) -> Result<Status> {
    {
        let mut status = backend.premining.status.lock().map_err(|_| anyhow!("premining stopped"))?;
        if status.working {
            return Ok(status.clone());
        }
        status.working = true;
        status.error.clear();
        (status.step, status.progress) = ("Making cards".into(), 0.0);
        (status.added, status.skipped) = (0, Vec::new());
    }
    let worker = backend.clone();
    std::thread::spawn(move || {
        let step = |text: &str, progress: f64| set(&worker, |s| (s.step, s.progress) = (text.to_string(), progress));
        let result = make_cards(&worker, &words, &step);
        let made: HashSet<(String, String)> = words.iter().map(|w| (w.headword.clone(), w.reading.clone())).collect();
        if let (Ok(mut all), Ok(_)) = (worker.premining.made.lock(), result.as_ref()) {
            all.extend(made.iter().cloned());
        }
        set(&worker, |s| {
            s.working = false;
            match result {
                Ok((added, skipped)) => {
                    (s.added, s.skipped) = (added, skipped);
                    if let Some(list) = s.list.as_mut() {
                        let mined = |w: &Word| made.contains(&(w.headword.clone(), w.reading.clone()));
                        list.known_uses += list.words.iter().filter(|w| mined(w)).map(|w| w.count).sum::<usize>();
                        list.words.retain(|w| !mined(w));
                    }
                }
                Err(e) => s.error = format!("{e:#}"),
            }
        });
    });
    Ok(status(backend))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_premined_card_is_cut_from_the_spoken_line_not_a_sign_starting_with_it() {
        let at = |start: f64, end: f64, text: &str, sign: bool| Line { start, end, text: text.into(), ruby: vec![], speaker: String::new(), sign };
        // A sign 10.0–11.0 sorts before the line said 10.0–13.5; the nudge moves both 0.5 s later.
        let lines = vec![at(10.5, 11.5, "看板", true), at(10.5, 14.0, "腹が減った", false)];
        let word = Word {
            headword: "減る".into(),
            reading: "へる".into(),
            rank: None,
            count: 1,
            episode_id: 1,
            number: 1,
            start: 10.0,
            end: 13.5,
            before: "腹が".into(),
            word: "減っ".into(),
            after: "た".into(),
        };
        let line = word_line(&lines, &word, 0.5);
        assert_eq!((line.start, line.end, line.text.as_str()), (10.5, 14.0, "腹が減った"));
        assert_eq!(cards::clip_bounds(&lines, &line, "250"), (10.25, 14.25));
    }

    #[test]
    fn words_that_waited_for_telly_s_hachidori_are_worked_out_once_it_answers() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = std::env::temp_dir().join(format!("telly-shares-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Arc::new(Backend::open(crate::backend::Dirs { local: dir.clone(), roaming: dir.clone() }).unwrap());
        // No dictionaries to install, and an Anki that isn't there.
        std::fs::create_dir_all(dir.join("dictionaries")).unwrap();
        backend.db.set_setting(own_hachidori::FOLDER, &json!(dir.join("dictionaries").to_string_lossy())).unwrap();
        backend.db.set_setting("anki.url", &json!("http://127.0.0.1:9")).unwrap();
        backend.db.set_setting("known.field", &json!("Word")).unwrap();
        let episodes = (1..=2).map(|number| crate::db::NewEpisode { number, path: format!("{number}.mkv"), size: 1, duration: 1400.0 }).collect();
        let show = backend.db.add_show(&crate::db::NewShow { folder: "Show".into(), title: "Show".into(), episodes, ..Default::default() }).unwrap();
        let ids: Vec<i64> = backend.db.episodes(show).unwrap().iter().map(|e| e.id).collect();
        for id in &ids {
            let line = Line { start: 0.0, end: 1.0, text: "腹が減った".into(), ruby: vec![], speaker: String::new(), sign: false };
            jobs::write_lines(&backend, *id, &[line]).unwrap();
        }
        let scored = || {
            let scores = backend.db.word_scores().unwrap();
            ids.iter().filter(|id| scores.contains_key(id)).count()
        };
        let waiting = |what: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while !what() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        // The Library opens before Telly's Hachidori answers: nothing to count
        // yet, and the pass says why (Anki first, which isn't there either).
        start_shares(&backend, None);
        waiting(&|| !shares(&backend).1.is_empty());
        assert!(shares(&backend).1.contains("Anki isn’t reachable"), "{}", shares(&backend).1);
        assert_eq!(scored(), 0);

        // Its page comes, and knows every word asked as itself.
        let stop = Arc::new(AtomicBool::new(false));
        let page = {
            let (backend, stop) = (backend.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let next = backend.own_hachidori.next_request(Duration::from_millis(100));
                    let Some(id) = next["id"].as_u64() else { continue };
                    assert_eq!(next["request"]["type"], "telly_term_entries");
                    let results: Vec<Value> = next["request"]["terms"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|t| {
                            let t = t.as_str().unwrap();
                            json!({ "originalTextLength": t.encode_utf16().count(), "dictionaryEntries": [{ "headwords": [{ "term": t, "reading": "" }], "frequencies": [] }] })
                        })
                        .collect();
                    let body = json!({ "ok": true, "result": { "results": results } }).to_string();
                    backend.own_hachidori.answer(&format!("id={id}"), body.as_bytes()).unwrap();
                }
            })
        };
        waiting(&|| scored() == ids.len());
        stop.store(true, Ordering::Relaxed);
        page.join().unwrap();
        assert_eq!(scored(), ids.len());
        // 腹 and 減る: が and た aren't words.
        assert!(shares(&backend).0.values().all(|s| s.uses == 2), "{:?}", shares(&backend).0);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The words in a line, with a dictionary that knows only the words alone.
    fn bases(text: &str) -> Vec<String> {
        candidates(text).unwrap().into_iter().flat_map(|c| c.words(text, |_| false)).map(|w| w.0).collect()
    }

    #[test]
    fn an_episode_s_share_is_of_the_words_said_that_are_known() {
        let line = |text: &str| Line { start: 0.0, end: 1.0, text: text.into(), ruby: vec![], speaker: String::new(), sign: false };
        let lines = vec![line("腹は減った"), line("（ナツ）お金もない")];
        let used = |line: usize, from: usize, to: usize, headword: &str| SavedUse { line, from, to, headword: headword.into(), reading: String::new(), rank: None };
        let uses = vec![used(0, 0, 3, "腹"), used(0, 6, 15, "減る"), used(1, 3, 9, "ナツ"), used(1, 12, 18, "お金"), used(1, 21, 27, "ない")];
        let known_words: HashSet<String> = ["腹", "お金", "ナツ"].iter().map(|w| w.to_string()).collect();
        // ナツ names who's speaking, in brackets: it isn't said, so it isn't counted either way.
        assert_eq!(share_of(&lines, uses, &HashSet::new(), &known_words), Share { uses: 4, known: 2 });
    }

    #[test]
    fn finds_the_words_to_learn_in_a_line() {
        // Particles, auxiliaries, わけ and ている's いる are left out; verbs come in their dictionary form.
        assert_eq!(bases("腹は減ったし、お金もないもんね"), ["腹", "減る", "お金", "ない"]);
        assert_eq!(bases("彼とて、悪意があったわけじゃないんだからね"), ["悪意", "ある"]);
        assert_eq!(bases("そして　その魔法を駆使して"), ["そして", "魔法", "駆使", "する"]);
        assert_eq!(bases("仲間を待っている"), ["仲間", "待つ"]);
    }

    #[test]
    fn nouns_that_touch_are_the_longest_word_hachidori_knows() {
        let line = "魔導士たちはギルドに";
        let run = candidates(line).unwrap().remove(0);
        assert!(run.texts(line).contains(&"魔導士".to_string()), "{run:?}");
        let words: Vec<String> = run.words(line, |t| t == "魔導士").into_iter().map(|w| w.0).collect();
        assert_eq!(words, ["魔導士"]);
    }

    #[test]
    fn furigana_sits_on_the_kanji() {
        assert_eq!(ruby("減っ", "へっ"), [("減".to_string(), "へ".to_string()), ("っ".to_string(), String::new())]);
        assert_eq!(ruby("お金", "おかね"), [("お".to_string(), String::new()), ("金".to_string(), "かね".to_string())]);
        assert_eq!(furigana("腹は減った", true).unwrap(), "腹[はら]は 減[へ]った");
        assert_eq!(furigana("悪意", false).unwrap(), "<ruby>悪意<rt>あくい</rt></ruby>");
    }

    #[test]
    fn the_sentence_reads_the_word_as_hachidori_does() {
        let word = Word {
            headword: "魔導士".into(),
            reading: "まどうし".into(),
            rank: None,
            count: 1,
            episode_id: 1,
            number: 1,
            start: 0.0,
            end: 1.0,
            before: "彼らを".into(),
            word: "魔導士".into(),
            after: "と呼んだ".into(),
        };
        let values = sentence_markers(&word).unwrap();
        assert_eq!(values["sentence"], "彼らを<b>魔導士</b>と呼んだ");
        assert_eq!(values["sentence-furigana-plain"], "彼[かれ]らを<b>魔導士[まどうし]</b>と 呼[よ]んだ");
        assert_eq!(values["cloze-body"], "魔導士");
    }

    #[test]
    fn templates_fill_their_markers() {
        let values: HashMap<String, String> = [("expression".to_string(), "悪意".to_string()), ("reading".to_string(), "あくい".to_string())].into();
        assert_eq!(render("{expression}【{Reading}】{missing} {not a marker}", &values), "悪意【あくい】 {not a marker}");
        assert_eq!(markers_in("{cloze-prefix}<b>{cloze-body}</b>{cloze-suffix}"), ["cloze-prefix", "cloze-body", "cloze-suffix"]);
    }

    #[test]
    fn a_studied_note_with_the_headword_or_reading_is_known() {
        let studied: HashSet<String> = ["する".to_string(), "悪意".to_string()].into();
        let term = |headword: &str, reading: &str| Term { headword: headword.into(), reading: reading.into(), rank: None };
        assert!(known(&studied, &term("為る", "する")));
        assert!(known(&studied, &term("悪意", "あくい")));
        assert!(!known(&studied, &term("魔導士", "まどうし")));
        assert!(!known(&studied, &term("悪意的", "")));
    }

    #[test]
    fn hachidori_s_answer_gives_the_headword_and_rank() {
        let result = json!({ "index": 0, "originalTextLength": 2, "dictionaryEntries": [{
            "headwords": [{ "term": "悪意", "reading": "あくい" }],
            "frequencies": [
                { "dictionaryIndex": 3, "frequency": 9000, "frequencyMode": "rank-based" },
                { "dictionaryIndex": 1, "frequency": 15000, "frequencyMode": null },
                { "dictionaryIndex": 0, "frequency": 42, "frequencyMode": "occurrence-based" }
            ]
        }]});
        assert_eq!(term_of("悪意", &result), Some(Term { headword: "悪意".into(), reading: "あくい".into(), rank: Some(15000) }));
        // A match of only part of the text is another word.
        assert_eq!(term_of("悪意者", &result), None);
        // An occurrence-based dictionary that shows a rank ranks by it.
        let shown = json!({ "dictionaryIndex": 0, "frequency": 481, "frequencyMode": "occurrence-based", "displayValue": "#459 (sum)" });
        assert_eq!(rank_of(&shown), Some(459));
        assert_eq!(rank_of(&json!({ "frequency": 42, "frequencyMode": "occurrence-based", "displayValue": "42" })), None);
    }

    #[test]
    fn only_words_that_are_said_count() {
        let at = |text: &str, word: &str| said(text, text.find(word).unwrap());
        assert!(at("腹は減ったし", "腹"));
        // A sound, described.
        assert!(!at("（爆発音）", "爆発"));
        assert!(!at("ンッ！
(蹴る音)", "蹴る"));
        // After the speaker's name, the line is said; a bracket left open by nothing isn't one.
        assert!(at("はい…
（シャルル）何なの？ こいつ", "何"));
        assert!(!at("はい…
（魔導士）覇気がねえよ", "魔導士"));
        assert!(at("たち）乾杯！", "乾杯"));
        // A song.
        assert!(!at("♪ 生き続ける言葉", "言葉"));
    }

    #[test]
    fn most_useful_first() {
        let word = |headword: &str, count: usize, rank: Option<i64>| Word {
            headword: headword.into(),
            reading: String::new(),
            rank,
            count,
            episode_id: 1,
            number: 1,
            start: 0.0,
            end: 1.0,
            before: String::new(),
            word: headword.into(),
            after: String::new(),
        };
        let mut words = vec![(0, word("稀", 1, Some(40000))), (1, word("依頼", 5, Some(3000))), (2, word("報酬", 5, Some(800))), (3, word("謎", 1, None))];
        sort_words(&mut words);
        let order: Vec<&str> = words.iter().map(|(_, w)| w.headword.as_str()).collect();
        assert_eq!(order, ["報酬", "依頼", "稀", "謎"]);
    }
}
