// Copied from bee-san/Japanese_Character_Name_Dictionary (characterdictionary.tokyo),
// yomitan-dict-builder/src/dict_builder.rs at 2e3949d. MIT licence, Copyright (c) 2026 Autumn (Bee).
// Telly's changes are marked "Telly:".

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Write};

use serde_json::json;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use super::content_builder::{ContentBuilder, DictSettings};
use super::image_handler::ImageHandler;
use super::kana;
use super::models::*;
use super::name_parser::{self, HONORIFIC_SUFFIXES};

/// Parse AniList alternative name format "Romanized (Japanese)".
///
/// AniList alternative names often use the format "Romaji (日本語)" where
/// the romanized reading is outside the parentheses and the Japanese form
/// is inside. This function extracts both parts.
///
/// Returns `(term, optional_reading)`:
/// - For "Aoki Umi (碧井海)": returns ("碧井海", Some("あおきうみ"))
/// - For "碧井海": returns ("碧井海", None) — standalone Japanese
/// - For "Ruri's Mother": returns ("Ruri's Mother", None) — no parenthesized part
fn parse_alt_name_format(alias: &str) -> (String, Option<String>) {
    // Look for pattern: "text (text)" where the parenthesized part contains Japanese
    if let Some(open) = alias.rfind('(') {
        if let Some(close) = alias[open..].find(')') {
            let inside = alias[open + 1..open + close].trim();
            let outside = alias[..open].trim();

            if kana::contains_japanese(inside) && !outside.is_empty() {
                // The Japanese part is the term, the romanized part provides the reading
                let reading = kana::alphabet_to_kana(outside).replace(' ', "");
                return (inside.to_string(), Some(reading));
            }
        }
    }

    // No "X (Y)" format or not Japanese inside — return as-is
    (alias.to_string(), None)
}

/// Maximum number of entries to put into one term bank file.
/// Yomitan recommends keeping each term bank small to reduce import issues.
/// JMdict and Jitendex both use 2k entries as their limit.
const TERM_BANK_LIMIT: usize = 2_000;

fn get_score(role: &str) -> i32 {
    match role {
        "main" => 100,
        "primary" => 75,
        "side" => 50,
        "appears" => 25,
        _ => 0,
    }
}

/// Role importance ordering for sorting appearances.
fn role_importance(role: &str) -> u8 {
    match role {
        "main" => 0,
        "primary" => 1,
        "side" => 2,
        "appears" => 3,
        _ => 4,
    }
}

/// A character merged across multiple media appearances.
///
/// When the same character (by source+ID within a single source, or by
/// normalized name across sources) appears in multiple media, their data
/// is merged into one of these. Single-value fields use first-non-None
/// semantics; appearances are accumulated.
struct MergedCharacter {
    /// The base Character data (merged using first-non-None for Option fields).
    character: Character,
    /// All (role, media_title) pairs — one per media appearance.
    appearances: Vec<(String, String)>,
    /// Highest role score across all appearances.
    best_score: i32,
    /// Highest role string (for definition tags).
    best_role: String,
}

/// Compact data needed to lazily generate honorific entries during ZIP export.
/// Instead of eagerly expanding ~257 honorific suffixes x N base names into full
/// serde_json::Value entries (which can consume hundreds of MB for large dictionaries),
/// we store just the source data and generate entries on-the-fly during export.
struct HonorificSource {
    /// (base_name, base_reading) pairs to combine with each honorific suffix
    base_names_with_readings: Vec<(String, String)>,
    /// The shared structured content card for this character (cloned once per honorific)
    structured_content: serde_json::Value,
    tag_role: String,
    score: i32,
    /// Terms already added as base entries — skip these during honorific expansion
    added_terms: HashSet<String>,
    /// Telly: the suffixes this character gets (see names::honorifics_for).
    suffixes: Vec<(&'static str, &'static str, &'static str)>,
}

pub struct DictBuilder {
    pub entries: Vec<serde_json::Value>,
    /// Deferred honorific data — expanded lazily during export_bytes()
    honorific_sources: Vec<HonorificSource>,
    images: Vec<(String, Vec<u8>)>, // (filename, bytes) for ZIP img/ folder
    added_images: HashSet<String>,  // track image filenames to avoid duplicate ZIP entries

    // --- Two-pass merge data structures ---
    /// Map from (source, character_id) to index in merged_characters.
    /// Used for within-source dedup (same character ID from same API).
    seen_by_id: HashMap<(String, String), usize>,
    /// Map from normalized name_original (whitespace stripped) to index in merged_characters.
    /// Used for cross-source dedup (same character across VNDB + AniList).
    seen_by_name: HashMap<String, usize>,
    /// Collected characters awaiting finalization into term entries.
    merged_characters: Vec<MergedCharacter>,
    /// Whether finalize() has been called (entries generated from merged_characters).
    finalized: bool,
    /// Counts of skipped characters with no Japanese name, grouped by media title.
    skipped_no_japanese_by_title: HashMap<String, usize>,

    settings: DictSettings,
    revision: String,
    download_url: Option<String>,
    game_title: String,
}

impl DictBuilder {
    pub fn new(settings: DictSettings, download_url: Option<String>, game_title: String) -> Self {
        // Unix timestamp as revision string (triggers Yomitan update detection)
        let revision: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            entries: Vec::new(),
            honorific_sources: Vec::new(),
            images: Vec::new(),
            added_images: HashSet::new(),
            seen_by_id: HashMap::new(),
            seen_by_name: HashMap::new(),
            merged_characters: Vec::new(),
            finalized: false,
            skipped_no_japanese_by_title: HashMap::new(),
            settings,
            revision: format!("{:012}", revision),
            download_url,
            game_title,
        }
    }

    pub fn skipped_no_japanese_count(&self) -> usize {
        self.skipped_no_japanese_by_title.values().sum()
    }

    pub fn skipped_no_japanese_summary(&self) -> Vec<(String, usize)> {
        let mut summary: Vec<(String, usize)> = self
            .skipped_no_japanese_by_title
            .iter()
            .map(|(title, count)| (title.clone(), *count))
            .collect();
        summary.sort_by(|a, b| a.0.cmp(&b.0));
        summary
    }


    /// Collect a character for later entry generation. Characters with the same
    /// (source, id) are merged (same character across media within one API).
    /// Characters with the same normalized name_original across different sources
    /// (VNDB + AniList) are also merged.
    ///
    /// Different characters that happen to share a name but have different IDs
    /// will produce separate dictionary entries (Yomitan stacks multiple definitions).
    pub fn add_character(&mut self, char: &Character, game_title: &str) {
        let name_original = &char.name_original;
        if name_original.is_empty() {
            *self
                .skipped_no_japanese_by_title
                .entry(game_title.to_string())
                .or_insert(0) += 1;
            return;
        }

        let normalized_name: String = name_original
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();

        let new_score = get_score(&char.role);
        let appearance = (char.role.clone(), game_title.to_string());

        // Check within-source dedup first: same (source, id)
        let id_key = (char.source.clone(), char.id.clone());
        if let Some(&idx) = self.seen_by_id.get(&id_key) {
            // Same character from same source — merge appearance
            let merged = &mut self.merged_characters[idx];
            merged.appearances.push(appearance);
            if new_score > merged.best_score {
                merged.best_score = new_score;
                merged.best_role = char.role.clone();
            }
            // First-non-None gap-fill for Option/Vec fields
            Self::merge_character_fields(&mut merged.character, char);
            return;
        }

        // Check cross-source dedup: same normalized name_original
        if let Some(&idx) = self.seen_by_name.get(&normalized_name) {
            // Same name from different source — merge appearance
            let merged = &mut self.merged_characters[idx];
            merged.appearances.push(appearance);
            if new_score > merged.best_score {
                merged.best_score = new_score;
                merged.best_role = char.role.clone();
            }
            Self::merge_character_fields(&mut merged.character, char);
            // Also register this (source, id) so future occurrences from the
            // same source hit the faster id-based lookup
            if !char.source.is_empty() && !char.id.is_empty() {
                self.seen_by_id.insert(id_key, idx);
            }
            return;
        }

        // New character — create a MergedCharacter
        let idx = self.merged_characters.len();
        self.merged_characters.push(MergedCharacter {
            character: char.clone(),
            appearances: vec![appearance],
            best_score: new_score,
            best_role: char.role.clone(),
        });

        // Register in both lookup maps
        if !char.source.is_empty() && !char.id.is_empty() {
            self.seen_by_id.insert(id_key, idx);
        }
        self.seen_by_name.insert(normalized_name, idx);
    }

    /// Merge single-value fields from `other` into `base` using first-non-None semantics.
    /// Only fills in gaps — never overwrites an existing Some value.
    /// For Vec fields: uses first-non-empty.
    /// For aliases: takes the union (deduplicated).
    fn merge_character_fields(base: &mut Character, other: &Character) {
        // Option fields: fill gaps
        macro_rules! fill_option {
            ($field:ident) => {
                if base.$field.is_none() {
                    base.$field = other.$field.clone();
                }
            };
        }
        fill_option!(sex);
        fill_option!(age);
        fill_option!(height);
        fill_option!(weight);
        fill_option!(blood_type);
        fill_option!(birthday);
        fill_option!(description);
        fill_option!(image_url);
        fill_option!(image_bytes);
        fill_option!(image_ext);
        fill_option!(image_width);
        fill_option!(image_height);
        fill_option!(seiyuu);
        fill_option!(seiyuu_image_url);
        fill_option!(seiyuu_image_bytes);
        fill_option!(seiyuu_image_ext);
        fill_option!(seiyuu_image_width);
        fill_option!(seiyuu_image_height);

        // Name hints: fill gaps
        if base.first_name_hint.is_none() {
            base.first_name_hint = other.first_name_hint.clone();
        }
        if base.last_name_hint.is_none() {
            base.last_name_hint = other.last_name_hint.clone();
        }

        // Romanized name: fill if empty
        if base.name.is_empty() && !other.name.is_empty() {
            base.name = other.name.clone();
        }

        // Vec trait fields: first-non-empty
        macro_rules! fill_vec {
            ($field:ident) => {
                if base.$field.is_empty() && !other.$field.is_empty() {
                    base.$field = other.$field.clone();
                }
            };
        }
        fill_vec!(personality);
        fill_vec!(roles);
        fill_vec!(engages_in);
        fill_vec!(subject_of);

        // Aliases: union (deduplicated)
        if !other.aliases.is_empty() {
            let mut existing: HashSet<String> = base.aliases.iter().cloned().collect();
            for alias in &other.aliases {
                if !alias.is_empty() && existing.insert(alias.clone()) {
                    base.aliases.push(alias.clone());
                }
            }
        }

        // Spoiler aliases: union (deduplicated)
        if !other.spoiler_aliases.is_empty() {
            let mut existing: HashSet<String> = base.spoiler_aliases.iter().cloned().collect();
            for alias in &other.spoiler_aliases {
                if !alias.is_empty() && existing.insert(alias.clone()) {
                    base.spoiler_aliases.push(alias.clone());
                }
            }
        }
    }

    /// Generate all term entries from merged characters.
    /// Called automatically by export_bytes() if not yet finalized.
    fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        // Take ownership of merged_characters to iterate without borrow conflict
        let merged_chars: Vec<MergedCharacter> = std::mem::take(&mut self.merged_characters);

        for mut merged in merged_chars {
            // Sort appearances by role importance (main first)
            merged
                .appearances
                .sort_by_key(|(role, _)| role_importance(role));

            self.generate_entries_for_merged(&merged);
        }
    }

    /// Generate all term entries (base + kana + alias + honorific) for one merged character.
    fn generate_entries_for_merged(&mut self, merged: &MergedCharacter) {
        let char = &merged.character;
        let name_original = &char.name_original;

        let score = merged.best_score;
        let tag_role: &str = if self.settings.show_tag {
            &merged.best_role
        } else {
            ""
        };

        let content_builder = ContentBuilder::new(self.settings.clone());

        // Generate hiragana readings using unified name handling (supports hints)
        let readings = name_parser::generate_name_readings(
            name_original,
            &char.name,
            char.first_name_hint.as_deref(),
            char.last_name_hint.as_deref(),
        );

        // Handle image: use raw bytes from download + resize (gated by show_image)
        let image_path = if self.settings.show_image {
            if let Some(ref img_bytes) = char.image_bytes {
                let ext = char.image_ext.as_deref().unwrap_or("jpg");
                let filename = ImageHandler::make_filename(&char.id, ext);
                let path = format!("img/{}", filename);
                // Only add image bytes once per filename to avoid duplicate ZIP entries
                if self.added_images.insert(filename.clone()) {
                    self.images.push((filename, img_bytes.clone()));
                }
                Some(path)
            } else {
                None
            }
        } else {
            None
        };

        // Handle seiyuu image (gated by show_seiyuu)
        let seiyuu_image_path = if self.settings.show_seiyuu {
            if let Some(ref img_bytes) = char.seiyuu_image_bytes {
                let ext = char.seiyuu_image_ext.as_deref().unwrap_or("jpg");
                let filename = ImageHandler::make_filename(&format!("{}_va", char.id), ext);
                let path = format!("img/{}", filename);
                if self.added_images.insert(filename.clone()) {
                    self.images.push((filename, img_bytes.clone()));
                }
                Some(path)
            } else {
                None
            }
        } else {
            None
        };

        // Build the structured content card
        let image_dims = match (char.image_width, char.image_height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        };
        let seiyuu_image_dims = match (char.seiyuu_image_width, char.seiyuu_image_height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        };

        // Use merged content builder when there are multiple appearances,
        // single-appearance builder otherwise (keeps output identical for
        // the common single-media case)
        let structured_content = if merged.appearances.len() > 1 {
            content_builder.build_merged_content(
                char,
                image_path.as_deref(),
                image_dims,
                seiyuu_image_path.as_deref(),
                seiyuu_image_dims,
                &merged.appearances,
            )
        } else {
            // Single appearance — use original build_content for backward compatibility
            let game_title = merged
                .appearances
                .first()
                .map(|(_, t)| t.as_str())
                .unwrap_or("");
            content_builder.build_content(
                char,
                image_path.as_deref(),
                image_dims,
                seiyuu_image_path.as_deref(),
                seiyuu_image_dims,
                game_title,
            )
        };

        // Track terms to avoid duplicates within this character
        let mut added_terms: HashSet<String> = HashSet::new();

        // Split the Japanese name (with hints for AniList characters)
        let name_parts = name_parser::split_japanese_name_with_hints(
            name_original,
            char.first_name_hint.as_deref(),
            char.last_name_hint.as_deref(),
        );

        // Get ALL plausible splits (for ambiguous all-kanji names with symmetric
        // kana lengths, multiple candidates may exist — we generate entries for
        // each so lookups work regardless of which split is correct).
        let all_candidates = name_parser::split_japanese_name_all_candidates(
            name_original,
            char.first_name_hint.as_deref(),
            char.last_name_hint.as_deref(),
        );

        // --- Base name entries ---
        // Generate split entries when we have family/given parts (either from space or hints)
        let has_split = name_parts.has_space || name_parts.family.is_some();

        if has_split {
            // 1. Original with space: "須々木 心一"
            if !name_parts.original.is_empty() && added_terms.insert(name_parts.original.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &name_parts.original,
                    &readings.full,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }

            // 2. Combined without space: "須々木心一"
            if !name_parts.combined.is_empty() && added_terms.insert(name_parts.combined.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &name_parts.combined,
                    &readings.full,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }

            // 3. Family name only and 4. Given name only — for ALL split candidates.
            for candidate in &all_candidates {
                if let Some(ref family) = candidate.family {
                    if !family.is_empty() && added_terms.insert(family.clone()) {
                        self.entries.push(ContentBuilder::create_term_entry(
                            family,
                            &readings.family,
                            tag_role,
                            score,
                            &structured_content,
                        ));
                    }
                }
                if let Some(ref given) = candidate.given {
                    if !given.is_empty() && added_terms.insert(given.clone()) {
                        self.entries.push(ContentBuilder::create_term_entry(
                            given,
                            &readings.given,
                            tag_role,
                            score,
                            &structured_content,
                        ));
                    }
                }
            }
        } else {
            // Single-word name
            if !name_original.is_empty() && added_terms.insert(name_original.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    name_original,
                    &readings.full,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
        }

        // --- Hiragana / Katakana term entries ---
        if has_split {
            // Hiragana combined (no space): "すずきしんいち"
            let hira_combined = format!("{}{}", readings.family, readings.given);
            if !hira_combined.is_empty() && added_terms.insert(hira_combined.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &hira_combined,
                    &readings.full,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
            // Hiragana with space: "すずき しんいち"
            let hira_spaced = format!("{} {}", readings.family, readings.given);
            if added_terms.insert(hira_spaced.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &hira_spaced,
                    &readings.full,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
            // Hiragana family only
            if !readings.family.is_empty() && added_terms.insert(readings.family.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &readings.family,
                    &readings.family,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
            // Hiragana given only
            if !readings.given.is_empty() && added_terms.insert(readings.given.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &readings.given,
                    &readings.given,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }

            // Katakana variants
            let kata_family = kana::hira_to_kata(&readings.family);
            let kata_given = kana::hira_to_kata(&readings.given);
            let kata_combined = format!("{}{}", kata_family, kata_given);
            if !kata_combined.is_empty() && added_terms.insert(kata_combined.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &kata_combined,
                    &readings.full,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
            let kata_spaced = format!("{} {}", kata_family, kata_given);
            if added_terms.insert(kata_spaced.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &kata_spaced,
                    &readings.full,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
            if !kata_family.is_empty() && added_terms.insert(kata_family.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &kata_family,
                    &readings.family,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
            if !kata_given.is_empty() && added_terms.insert(kata_given.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &kata_given,
                    &readings.given,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
        } else {
            // Single-word name: add hiragana and katakana forms
            if !readings.full.is_empty() && added_terms.insert(readings.full.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &readings.full,
                    &readings.full,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
            let kata_full = kana::hira_to_kata(&readings.full);
            if !kata_full.is_empty() && added_terms.insert(kata_full.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(
                    &kata_full,
                    &readings.full,
                    tag_role,
                    score,
                    &structured_content,
                ));
            }
        }

        // --- Honorific suffix variants for all base names ---
        let mut base_names_with_readings: Vec<(String, String)> = Vec::new();
        if has_split {
            // Original kanji forms — all split candidates' family/given pairs
            for candidate in &all_candidates {
                if let Some(ref family) = candidate.family {
                    if !family.is_empty() {
                        base_names_with_readings.push((family.clone(), readings.family.clone()));
                    }
                }
                if let Some(ref given) = candidate.given {
                    if !given.is_empty() {
                        base_names_with_readings.push((given.clone(), readings.given.clone()));
                    }
                }
            }
            if !name_parts.combined.is_empty() {
                base_names_with_readings.push((name_parts.combined.clone(), readings.full.clone()));
            }
            if !name_parts.original.is_empty() {
                base_names_with_readings.push((name_parts.original.clone(), readings.full.clone()));
            }
            // Hiragana forms (family, given, combined)
            if !readings.family.is_empty() {
                base_names_with_readings.push((readings.family.clone(), readings.family.clone()));
            }
            if !readings.given.is_empty() {
                base_names_with_readings.push((readings.given.clone(), readings.given.clone()));
            }
            let hira_combined = format!("{}{}", readings.family, readings.given);
            if !hira_combined.is_empty() {
                base_names_with_readings.push((hira_combined, readings.full.clone()));
            }
            // Katakana forms (family, given, combined)
            let kata_family = kana::hira_to_kata(&readings.family);
            let kata_given = kana::hira_to_kata(&readings.given);
            if !kata_family.is_empty() {
                base_names_with_readings.push((kata_family.clone(), readings.family.clone()));
            }
            if !kata_given.is_empty() {
                base_names_with_readings.push((kata_given.clone(), readings.given.clone()));
            }
            let kata_combined = format!("{}{}", kata_family, kata_given);
            if !kata_combined.is_empty() {
                base_names_with_readings.push((kata_combined, readings.full.clone()));
            }
        } else if !name_original.is_empty() {
            base_names_with_readings.push((name_original.clone(), readings.full.clone()));
            // Hiragana form
            if !readings.full.is_empty() {
                base_names_with_readings.push((readings.full.clone(), readings.full.clone()));
            }
            // Katakana form
            let kata_full = kana::hira_to_kata(&readings.full);
            if !kata_full.is_empty() {
                base_names_with_readings.push((kata_full, readings.full.clone()));
            }
        }

        // Telly: a name written with ・ (ナツ・ドラグニル, given name first) gets
        // each part too, as the subtitles mostly say just one of them, with
        // its reading when the part is kana.
        for part in super::dotted_parts(name_original) {
            if kana::contains_kanji(&part) {
                continue;
            }
            let reading = kana::kata_to_hira(&part);
            if added_terms.insert(part.clone()) {
                self.entries.push(ContentBuilder::create_term_entry(&part, &reading, tag_role, score, &structured_content));
            }
            base_names_with_readings.push((part, reading));
        }

        // --- Alias entries ---
        // Include spoiler aliases when the user has enabled spoilers
        let spoiler_aliases: &[String] = if self.settings.show_spoilers {
            &char.spoiler_aliases
        } else {
            &[]
        };

        for alias in char.aliases.iter().chain(spoiler_aliases.iter()) {
            if alias.is_empty() {
                continue;
            }

            // Parse "Romanized (Japanese)" format used by AniList alternative names.
            // E.g. "Aoki Umi (碧井海)" → term="碧井海", reading from "Aoki Umi"
            let (alias_term, alias_reading) = parse_alt_name_format(alias);

            // Skip aliases that don't contain any Japanese characters
            if !kana::contains_japanese(&alias_term) {
                continue;
            }

            if added_terms.insert(alias_term.clone()) {
                let reading = alias_reading.unwrap_or_else(|| readings.full.clone());
                self.entries.push(ContentBuilder::create_term_entry(
                    &alias_term,
                    &reading,
                    tag_role,
                    score,
                    &structured_content,
                ));

                // Also collect alias name+reading for deferred honorific generation
                if self.settings.honorifics {
                    base_names_with_readings.push((alias_term, reading));
                }
            }
        }

        // --- Deferred honorific generation ---
        // Telly: main and supporting characters only, their names as the show
        // writes them (not every kana spelling), and a short card, so a cast of
        // hundreds stays a dictionary of thousands of entries, not millions.
        let suffixes = super::honorifics_for(&merged.best_role);
        let base_names_with_readings: Vec<(String, String)> = base_names_with_readings
            .into_iter()
            .filter(|(name, _)| super::written_form(name, name_original))
            .collect();
        if self.settings.honorifics && !base_names_with_readings.is_empty() && !suffixes.is_empty() {
            self.honorific_sources.push(HonorificSource {
                base_names_with_readings,
                structured_content: super::short_card(&structured_content),
                tag_role: tag_role.to_string(),
                score,
                added_terms,
                suffixes,
            });
        }
    }

    /// Returns true if the builder has any entries (base, deferred honorifics, or pending merge).
    pub fn has_entries(&self) -> bool {
        !self.entries.is_empty()
            || !self.honorific_sources.is_empty()
            || !self.merged_characters.is_empty()
    }

    /// Collect all entries including lazily-generated honorifics.
    /// This materializes the full entry set — use only for testing or inspection,
    /// not for ZIP export (which streams in chunks to avoid memory spikes).
    #[cfg(test)]
    fn collect_all_entries(&mut self) -> Vec<serde_json::Value> {
        self.finalize();
        let mut all = self.entries.clone();
        let mut honorific_dedup: HashSet<String> = HashSet::new();
        for source in &self.honorific_sources {
            for (base_name, base_reading) in &source.base_names_with_readings {
                if !super::plausible_name(base_name) {
                    continue; // Telly: as above
                }
                for (suffix, suffix_reading, description) in &source.suffixes {
                    let term_with_suffix = format!("{}{}", base_name, suffix);
                    // Skip if this term was already added as a base entry for this character
                    if source.added_terms.contains(&term_with_suffix) {
                        continue;
                    }
                    // Skip if already emitted as an honorific (cross-character or
                    // duplicate base name forms within the same character)
                    if !honorific_dedup.insert(term_with_suffix.clone()) {
                        continue;
                    }
                    let reading_with_suffix = format!("{}{}", base_reading, suffix_reading);
                    let honorific_content = ContentBuilder::build_honorific_content(
                        &source.structured_content,
                        suffix,
                        description,
                    );
                    all.push(ContentBuilder::create_term_entry(
                        &term_with_suffix,
                        &reading_with_suffix,
                        &source.tag_role,
                        source.score,
                        &honorific_content,
                    ));
                }
            }
        }
        all
    }

    /// Finalize and return a reference to just the base (non-honorific) entries.
    /// Convenience helper for tests that don't need honorific entries.
    #[cfg(test)]
    fn base_entries(&mut self) -> &[serde_json::Value] {
        self.finalize();
        &self.entries
    }

    /// Create index.json metadata.
    fn create_index(&self) -> serde_json::Value {
        let description = if self.game_title.is_empty() {
            "Character names dictionary".to_string()
        } else {
            format!("Character names from {}", self.game_title)
        };

        // Telly: its own title, so it replaces only Telly's dictionary.
        let mut index = json!({
            "title": super::TITLE,
            "revision": &self.revision,
            "format": 3,
            "author": "Telly, with Bee's character dictionary builder (https://characterdictionary.tokyo)",
            "description": description
        });

        // Add auto-update URLs if download_url is set
        if let Some(ref url) = self.download_url {
            index["downloadUrl"] = json!(url);
            // indexUrl is the same URL but with /api/yomitan-index instead of /api/yomitan-dict
            index["indexUrl"] = json!(url.replace("/api/yomitan-dict", "/api/yomitan-index"));
            index["isUpdatable"] = json!(true);
        }

        index
    }

    /// Public accessor for index generation (used by the index endpoint).
    pub fn create_index_public(&self) -> serde_json::Value {
        self.create_index()
    }

    #[allow(dead_code)]
    pub fn add_prebuilt_entries_with_optional_honorifics(
        &mut self,
        term_readings: &[(String, String)],
        tag_role: &str,
        score: i32,
        structured_content: &serde_json::Value,
    ) -> usize {
        self.finalize();
        let mut added_terms = HashSet::new();
        let mut added_count = 0;
        let mut base_names_with_readings = Vec::new();

        for (term, reading) in term_readings {
            let term = term.trim();
            if term.is_empty() || !added_terms.insert(term.to_string()) {
                continue;
            }
            self.entries.push(ContentBuilder::create_term_entry(
                term,
                reading.trim(),
                tag_role,
                score,
                structured_content,
            ));
            base_names_with_readings.push((term.to_string(), reading.trim().to_string()));
            added_count += 1;
        }

        if self.settings.honorifics && !base_names_with_readings.is_empty() {
            self.honorific_sources.push(HonorificSource {
                base_names_with_readings,
                structured_content: structured_content.clone(),
                tag_role: tag_role.to_string(),
                score,
                added_terms,
                suffixes: HONORIFIC_SUFFIXES.to_vec(),
            });
        }

        added_count
    }

    /// Create tag_bank_1.json — fixed tag definitions for character roles.
    fn create_tags(&self) -> serde_json::Value {
        // Telly: no Docker build stamp, and the date without chrono.
        let dict_built = super::today();

        let mut tags = vec![json!(["name", "partOfSpeech", 0, "Character name", 0])];

        // Only include role tag definitions when show_tag is enabled;
        // when disabled, no term entry references these tags so they'd be unused.
        if self.settings.show_tag {
            tags.push(json!(["main", "name", 0, "Protagonist", 0]));
            tags.push(json!(["primary", "name", 0, "Main character", 0]));
            tags.push(json!(["side", "name", 0, "Side character", 0]));
            tags.push(json!(["appears", "name", 0, "Minor appearance", 0]));
        }

        tags.push(json!(["dict-built", "meta", 0, dict_built, 0]));

        serde_json::Value::Array(tags)
    }

    /// Export the dictionary as in-memory ZIP bytes.
    /// Automatically finalizes (generates entries from merged characters) if needed.
    pub fn export_bytes(&mut self) -> Result<Vec<u8>, String> {
        self.finalize();
        // Telly: no entry for a part no name could be (ート, from splitting
        // アチェート by its romaji), which would match inside other words.
        self.entries.retain(|e| e[0].as_str().is_some_and(super::plausible_name));
        let buffer = Vec::new();
        let cursor = Cursor::new(buffer);
        let mut zip = ZipWriter::new(cursor);
        let json_options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        // Images are already compressed (JPEG/PNG) — storing them
        // uncompressed avoids wasting CPU for near-zero size reduction.
        let image_options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // 1. index.json
        zip.start_file("index.json", json_options)
            .map_err(|e| format!("Failed to create index.json in ZIP: {}", e))?;
        let index_json = serde_json::to_string_pretty(&self.create_index())
            .map_err(|e| format!("Failed to serialize index.json: {}", e))?;
        zip.write_all(index_json.as_bytes())
            .map_err(|e| format!("Failed to write index.json: {}", e))?;

        // 2. tag_bank_1.json
        zip.start_file("tag_bank_1.json", json_options)
            .map_err(|e| format!("Failed to create tag_bank_1.json in ZIP: {}", e))?;
        let tags_json = serde_json::to_string(&self.create_tags())
            .map_err(|e| format!("Failed to serialize tag_bank: {}", e))?;
        zip.write_all(tags_json.as_bytes())
            .map_err(|e| format!("Failed to write tag_bank: {}", e))?;

        // 3. term_bank_N.json — stream base entries then lazily generate honorifics.
        // Instead of holding all honorific entries in memory (which can be hundreds of
        // MB for large dictionaries), we buffer up to a term bank amount at a time and
        // flush each chunk to the ZIP before moving on.
        let entries_per_bank = TERM_BANK_LIMIT;
        let mut bank_buffer: Vec<serde_json::Value> = Vec::with_capacity(entries_per_bank);
        let mut bank_number: usize = 1;

        // Helper closure: flush the buffer as a term_bank file
        let flush_bank = |buf: &mut Vec<serde_json::Value>,
                          num: &mut usize,
                          zip: &mut ZipWriter<Cursor<Vec<u8>>>,
                          opts: SimpleFileOptions|
         -> Result<(), String> {
            if buf.is_empty() {
                return Ok(());
            }
            let filename = format!("term_bank_{}.json", *num);
            zip.start_file(&filename, opts)
                .map_err(|e| format!("Failed to create {} in ZIP: {}", filename, e))?;
            let data = serde_json::to_string(&*buf)
                .map_err(|e| format!("Failed to serialize {}: {}", filename, e))?;
            zip.write_all(data.as_bytes())
                .map_err(|e| format!("Failed to write {}: {}", filename, e))?;
            buf.clear();
            *num += 1;
            Ok(())
        };

        // Phase 1: Drain base entries (non-honorific)
        for entry in &self.entries {
            bank_buffer.push(entry.clone());
            if bank_buffer.len() >= entries_per_bank {
                flush_bank(&mut bank_buffer, &mut bank_number, &mut zip, json_options)?;
            }
        }

        // Phase 2: Lazily generate and stream honorific entries
        let mut honorific_dedup: HashSet<String> = HashSet::new();
        for source in &self.honorific_sources {
            for (base_name, base_reading) in &source.base_names_with_readings {
                if !super::plausible_name(base_name) {
                    continue; // Telly: as above
                }
                for (suffix, suffix_reading, description) in &source.suffixes {
                    let term_with_suffix = format!("{}{}", base_name, suffix);

                    // Skip if this term was already added as a base entry
                    if source.added_terms.contains(&term_with_suffix) {
                        continue;
                    }

                    // Skip duplicates (e.g. same base name form appearing multiple
                    // times, or same honorific term across different characters)
                    if !honorific_dedup.insert(term_with_suffix.clone()) {
                        continue;
                    }

                    let reading_with_suffix = format!("{}{}", base_reading, suffix_reading);
                    let honorific_content = ContentBuilder::build_honorific_content(
                        &source.structured_content,
                        suffix,
                        description,
                    );
                    bank_buffer.push(ContentBuilder::create_term_entry(
                        &term_with_suffix,
                        &reading_with_suffix,
                        &source.tag_role,
                        source.score,
                        &honorific_content,
                    ));

                    if bank_buffer.len() >= entries_per_bank {
                        flush_bank(&mut bank_buffer, &mut bank_number, &mut zip, json_options)?;
                    }
                }
            }
        }

        // Flush any remaining entries
        flush_bank(&mut bank_buffer, &mut bank_number, &mut zip, json_options)?;

        // 4. Images in img/ folder (stored uncompressed)
        for (filename, bytes) in &self.images {
            zip.start_file(format!("img/{}", filename), image_options)
                .map_err(|e| format!("Failed to create img/{} in ZIP: {}", filename, e))?;
            zip.write_all(bytes)
                .map_err(|e| format!("Failed to write img/{}: {}", filename, e))?;
        }

        let cursor = zip
            .finish()
            .map_err(|e| format!("Failed to finalize ZIP: {}", e))?;
        Ok(cursor.into_inner())
    }

    #[allow(dead_code)]
    pub fn export_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let bytes = self.export_bytes()?;
        std::fs::write(path, bytes)
            .map_err(|e| format!("Failed to write ZIP to {}: {}", path.display(), e))
    }
}

