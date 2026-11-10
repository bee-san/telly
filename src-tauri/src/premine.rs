//! Premining: before you watch, the words in the next episodes that you don't
//! know yet, most useful first, made into cards in one go.
//!
//! - Words: each synced line is split by Lindera (IPADIC, built into Telly).
//!   Particles, auxiliaries, pronouns, この and その, numbers, symbols, proper
//!   nouns and the cast's names are left out. Where nouns run together, the
//!   longest stretch Hachidori knows is one word (魔導士 in 魔導士たち, rather
//!   than 魔 and 導).
//! - Each word's headword, reading and frequency come from Hachidori, so
//!   they're what a lookup would show: Telly's own (own_hachidori.rs), or,
//!   when Telly is linked to the Hachidori in your browser, that one through
//!   the Yomitan API Hachidori Relay serves on port 19633.
//! - Anki says what you have. A note whose word field holds the headword, or
//!   its reading, is a card you have, so premining leaves the word out. A card
//!   21 days or more apart is mature: its word is one you know.
//! - Cards are made in your Hachidori card format (its first Template), in a
//!   Premined subdeck of the show's deck (Type::Fairy Tail::Premined), with
//!   the line the word came from. Telly's
//!   card finisher then adds the moving picture, the audio and MiscInfo, and
//!   tags them as it does the cards you mine while watching.

use crate::backend::Backend;
use crate::db::{Episode, Show};
use crate::names::kana::{contains_japanese, contains_kanji, kata_to_hira};
use crate::subs::Line;
use crate::{anki, cards, jobs, names, own_hachidori};
use anyhow::{anyhow, Result};
use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Hachidori Relay's Yomitan API.
const API: &str = "http://127.0.0.1:19633";

/// Where to ask Hachidori: its relay, or TELLY_YOMITAN_API (a stand-in for tests).
fn api_address() -> String {
    std::env::var("TELLY_YOMITAN_API").unwrap_or_else(|_| API.to_string())
}
/// A card this many days apart or more is mature: its word counts as known.
const MATURE_DAYS: i64 = 21;
/// Words asked of Hachidori at a time: each answer carries every dictionary's
/// entries, and the relay waits 30 seconds at most for one.
const LOOKUPS_PER_REQUEST: usize = 40;
/// Words checked in Anki at a time.
const CHECKS_PER_REQUEST: usize = 100;
/// The subdeck of your card format's deck that premined cards go in.
const SUBDECK: &str = "Premined";
const NO_RELAY: &str = "Premining needs Anki open, with Hachidori Relay. Open Anki and try again.";
const NO_OWN: &str = "Telly’s Hachidori isn’t running yet. Wait a moment, or use Restart Hachidori in Settings.";
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
    /// Times it comes up in these episodes.
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

/// The words to premine in some episodes.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct List {
    pub numbers: Vec<i64>,
    /// Episodes in the range that have no subtitles yet.
    pub without_subs: Vec<i64>,
    /// Words you have no card for, most useful first.
    pub words: Vec<Word>,
    /// The share of the words used in these episodes that you know (0 to 1).
    pub known: f64,
    /// Words with cards that aren't mature yet.
    pub learning: usize,
    /// Where cards go.
    pub deck: String,
}

/// What premining is doing, for the page to show.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub show: i64,
    pub from: i64,
    pub count: i64,
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

#[derive(Default)]
pub struct Premining {
    status: Mutex<Status>,
    /// Hachidori's answers, by the text asked, for as long as Telly is open.
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
    if std::env::var_os("TELLY_YOMITAN_API").is_none() && !backend.db.setting_bool("hachidori.link", false) {
        if !backend.own_hachidori.answering() {
            return Err(anyhow!(NO_OWN));
        }
        return own_api(backend, path, body);
    }
    relay_api(path, body)
}

/// The request as Hachidori's own API code takes it (its hd_api_* messages),
/// answered the way Hachidori Relay answers it.
fn own_api(backend: &Backend, path: &str, body: Value) -> Result<Value> {
    let ask = |request: Value| backend.own_hachidori.ask(request, Duration::from_secs(120));
    match path {
        "/termEntries" => Ok(ask(json!({ "type": "hd_api_term_entries", "terms": body["term"] }))?["results"].take()),
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

/// Looks up the texts Telly hasn't asked Hachidori about yet.
fn look_up(backend: &Backend, texts: &[String], step: &dyn Fn(f64)) -> Result<()> {
    let cache = &backend.premining.terms;
    let new: Vec<String> = {
        let known = cache.lock().map_err(|_| anyhow!("premining stopped"))?;
        let mut seen = HashSet::new();
        texts.iter().filter(|t| !known.contains_key(*t) && seen.insert((*t).clone())).cloned().collect()
    };
    for (k, chunk) in new.chunks(LOOKUPS_PER_REQUEST).enumerate() {
        step(k as f64 * LOOKUPS_PER_REQUEST as f64 / new.len().max(1) as f64);
        let answer = api(backend, "/termEntries", json!({ "term": chunk }))?;
        let results = answer.as_array().cloned().unwrap_or_default();
        let mut known = cache.lock().map_err(|_| anyhow!("premining stopped"))?;
        for (i, text) in chunk.iter().enumerate() {
            known.insert(text.clone(), results.get(i).and_then(|r| term_of(text, r)));
        }
    }
    Ok(())
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

/// A search for notes whose `field` is exactly `value`.
fn exact(field: &str, value: &str) -> String {
    let mut escaped = String::new();
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '*' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    format!("\"{field}:{escaped}\"")
}

/// The search for a word's notes: its headword, or its reading where the
/// note has the word in kana (する for 為る, こと for 事).
fn word_search(field: &str, term: &Term) -> String {
    let headword = exact(field, &term.headword);
    if term.reading.is_empty() || term.reading == term.headword {
        headword
    } else {
        format!("({headword} OR {})", exact(field, &term.reading))
    }
}

/// For each word: whether you have a card for it, and whether one is mature.
fn what_you_have(url: &str, field: &str, terms: &[Term], step: &dyn Fn(f64)) -> Result<Vec<(bool, bool)>> {
    let mut out = Vec::with_capacity(terms.len());
    for (k, chunk) in terms.chunks(CHECKS_PER_REQUEST).enumerate() {
        step(k as f64 * CHECKS_PER_REQUEST as f64 / terms.len().max(1) as f64);
        let mut actions = Vec::new();
        for term in chunk {
            let search = word_search(field, term);
            actions.push(json!({ "action": "findNotes", "params": { "query": search } }));
            actions.push(json!({ "action": "findCards", "params": { "query": format!("{search} prop:ivl>={MATURE_DAYS}") } }));
        }
        let results = anki::invoke(url, "multi", json!({ "actions": actions }))?;
        let list = results.as_array().cloned().unwrap_or_default();
        // AnkiConnect answers each action bare, or as {result, error} when it names a version.
        let found = |v: &Value| v.get("result").unwrap_or(v).as_array().is_some_and(|a| !a.is_empty());
        for i in 0..chunk.len() {
            out.push((list.get(2 * i).is_some_and(found), list.get(2 * i + 1).is_some_and(found)));
        }
    }
    Ok(out)
}

// ---- The list ----

/// A use of a word: which episode and line, and where in the line.
struct Use {
    episode: usize,
    line: usize,
    from: usize,
    to: usize,
}

/// The words to premine in `count` episodes of `show` from Episode `from`.
fn make_list(backend: &Backend, show: &Show, from: i64, count: i64, step: &dyn Fn(&str, f64)) -> Result<List> {
    step("Reading your card format", 0.0);
    let format = card_format(backend)?;
    let field = format.word_field();
    let mut episodes: Vec<Episode> = backend.db.episodes(show.id)?.into_iter().filter(|e| e.number >= from).collect();
    episodes.sort_by_key(|e| e.number);
    episodes.truncate(count.max(1) as usize);
    let cast: HashSet<String> = names::show_names(backend, show).into_iter().map(|n| n.term).collect();

    // The words in every line.
    let mut lines: Vec<Vec<Line>> = Vec::new();
    let mut found: Vec<(usize, usize, Candidate)> = Vec::new();
    let mut without_subs = Vec::new();
    for (e, episode) in episodes.iter().enumerate() {
        step(&format!("Reading Episode {}", episode.number), 0.05 + 0.15 * e as f64 / episodes.len() as f64);
        let episode_lines: Vec<Line> = jobs::read_lines(backend, episode.id).into_iter().filter(|l| !l.sign).collect();
        if episode_lines.is_empty() {
            without_subs.push(episode.number);
        }
        for (l, line) in episode_lines.iter().enumerate() {
            for candidate in candidates(&line.text)? {
                found.push((e, l, candidate));
            }
        }
        lines.push(episode_lines);
    }

    // What Hachidori makes of them.
    let texts: Vec<String> = found.iter().flat_map(|(e, l, c)| c.texts(&lines[*e][*l].text)).collect();
    look_up(backend, &texts, &|p| step("Looking up the words in Hachidori", 0.2 + 0.6 * p))?;
    let terms = backend.premining.terms.lock().map_err(|_| anyhow!("premining stopped"))?.clone();

    // Each use of each word.
    let mut uses: Vec<(Term, Use)> = Vec::new();
    for (episode, line, candidate) in found {
        let text = &lines[episode][line].text;
        for (asked, from, to) in candidate.words(text, |t| matches!(terms.get(t), Some(Some(_)))) {
            if let Some(Some(term)) = terms.get(&asked) {
                if !cast.contains(&term.headword) && !cast.contains(&text[from..to]) {
                    uses.push((term.clone(), Use { episode, line, from, to }));
                }
            }
        }
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
    let url = cards::anki_url(backend);
    let have = what_you_have(&url, &field, &order, &|p| step("Checking your cards in Anki", 0.8 + 0.2 * p))?;
    let mature: HashSet<(String, String)> = order
        .iter()
        .zip(&have)
        .filter(|(_, (_, mature))| *mature)
        .map(|(t, _)| (t.headword.clone(), t.reading.clone()))
        .collect();
    let known_uses = uses.iter().filter(|(t, _)| mature.contains(&(t.headword.clone(), t.reading.clone()))).count();

    let mut words: Vec<(usize, Word)> = Vec::new();
    for (term, (card, _)) in order.iter().zip(&have) {
        if *card {
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
    Ok(List {
        numbers: episodes.iter().map(|e| e.number).collect(),
        without_subs,
        known: if uses.is_empty() { 0.0 } else { known_uses as f64 / uses.len() as f64 },
        learning: have.iter().filter(|(card, mature)| *card && !*mature).count(),
        words: words.into_iter().map(|(_, w)| w).collect(),
        deck: premined_deck(backend, show),
    })
}

/// Most useful first: the words that come up most in these episodes, then the
/// most common anywhere (by your frequency dictionary), then the first seen.
fn sort_words(words: &mut [(usize, Word)]) {
    words.sort_by(|(fa, a), (fb, b)| {
        b.count.cmp(&a.count).then(a.rank.unwrap_or(i64::MAX).cmp(&b.rank.unwrap_or(i64::MAX))).then(fa.cmp(fb))
    });
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
            "options": { "allowDuplicate": false, "duplicateScope": "collection", "duplicateScopeOptions": { "checkAllModels": true } },
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
                // The moving picture and the audio, by the card finisher.
                let episode_lines = lines.entry(word.episode_id).or_insert_with(|| jobs::read_lines(backend, word.episode_id));
                let line = episode_lines
                    .iter()
                    .find(|l| (l.start - word.start).abs() < 0.01)
                    .cloned()
                    .unwrap_or(Line { start: word.start, end: word.end, text: format!("{}{}{}", word.before, word.word, word.after), ruby: vec![], speaker: String::new(), sign: false });
                let (start, end) = cards::clip_bounds(episode_lines, &line, &padding);
                backend.db.add_mine_job(note, word.episode_id, start, end, &line.text)?;
            }
            Err(e) => skipped.push(format!("{}: {e:#}", word.headword)),
        }
    }
    backend.cards.poke();
    Ok((added, skipped))
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

/// Starts finding the words to premine in `count` episodes from Episode
/// `from`, unless that list is being made or made already.
pub fn start_list(backend: &Arc<Backend>, show_id: i64, from: i64, count: i64) -> Result<Status> {
    let show = backend.db.show(show_id)?.ok_or_else(|| anyhow!("That show isn’t in Telly any more"))?;
    {
        let mut status = backend.premining.status.lock().map_err(|_| anyhow!("premining stopped"))?;
        let same = status.show == show_id && status.from == from && status.count == count;
        if status.working || (same && status.list.is_some()) {
            return Ok(status.clone());
        }
        *status = Status { show: show_id, from, count, working: true, step: "Starting".into(), ..Default::default() };
    }
    let worker = backend.clone();
    std::thread::spawn(move || {
        let step = |text: &str, progress: f64| set(&worker, |s| (s.step, s.progress) = (text.to_string(), progress));
        let result = make_list(&worker, &show, from, count, &step);
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

/// Starts making cards for `words`. When it's done, the list is made again so
/// they drop out of it.
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
        set(&worker, |s| {
            s.working = false;
            match result {
                Ok((added, skipped)) => {
                    (s.added, s.skipped) = (added, skipped);
                    if let Some(list) = s.list.as_mut() {
                        list.words.retain(|w| !made.contains(&(w.headword.clone(), w.reading.clone())));
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

    /// The words in a line, with a dictionary that knows only the words alone.
    fn bases(text: &str) -> Vec<String> {
        candidates(text).unwrap().into_iter().flat_map(|c| c.words(text, |_| false)).map(|w| w.0).collect()
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
    fn anki_searches_match_the_whole_field() {
        assert_eq!(exact("Expression", "悪意"), "\"Expression:悪意\"");
        assert_eq!(exact("Expression", "a_b*\"c"), "\"Expression:a\\_b\\*\\\"c\"");
        let term = Term { headword: "為る".into(), reading: "する".into(), rank: Some(10) };
        assert_eq!(word_search("Expression", &term), "(\"Expression:為る\" OR \"Expression:する\")");
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
