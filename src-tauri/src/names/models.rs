// Copied from bee-san/Japanese_Character_Name_Dictionary (characterdictionary.tokyo),
// yomitan-dict-builder/src/models.rs at 2e3949d. MIT licence, Copyright (c) 2026 Autumn (Bee).
// Telly's changes are marked "Telly:".

use serde::{Deserialize, Serialize};

/// An entry from a user's media list (VNDB or AniList).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMediaEntry {
    pub id: String,           // "v17" for VNDB, "9253" for AniList
    pub title: String,        // Display title (prefer Japanese/native)
    pub title_romaji: String, // Romanized title
    pub source: String,       // "vndb" or "anilist"
    pub media_type: String,   // "vn", "anime", "manga"
    #[serde(default = "default_user_media_status")]
    pub status: String, // "playing", "finished", "current", "completed", etc.
}

fn default_user_media_status() -> String {
    "current".to_string()
}

/// A trait with spoiler metadata.
/// Represents entries like: {"name": "Kind", "spoiler": 0}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterTrait {
    pub name: String,
    pub spoiler: u8, // 0=none, 1=minor, 2=major
}

/// Normalized character data. Both VNDB and AniList clients produce this format.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Character {
    pub id: String,
    pub name: String,          // Romanized (Western order for VNDB: "Given Family")
    pub name_original: String, // Japanese (Japanese order: "Family Given")
    pub role: String,          // "main", "primary", "side", "appears"
    #[serde(default)]
    pub source: String, // "vndb" or "anilist" — identifies which API produced this character
    pub sex: Option<String>,   // "m" or "f"
    pub age: Option<String>,   // String because AniList may return "17-18"
    pub height: Option<u32>,   // cm (VNDB only; None for AniList)
    pub weight: Option<u32>,   // kg (VNDB only; None for AniList)
    pub blood_type: Option<String>,
    pub birthday: Option<Vec<u32>>, // [month, day]
    pub description: Option<String>,
    pub aliases: Vec<String>,
    #[serde(default)]
    pub spoiler_aliases: Vec<String>, // Alternative names marked as spoilers (AniList only)
    pub personality: Vec<CharacterTrait>,
    pub roles: Vec<CharacterTrait>,
    pub engages_in: Vec<CharacterTrait>,
    pub subject_of: Vec<CharacterTrait>,
    pub image_url: Option<String>, // Raw URL from API (used for downloading)
    pub image_bytes: Option<Vec<u8>>, // Raw image bytes (after download + resize)
    pub image_ext: Option<String>, // File extension: "jpg", "png", "webp", etc.
    pub image_width: Option<u32>,  // Actual pixel width after resize
    pub image_height: Option<u32>, // Actual pixel height after resize
    pub first_name_hint: Option<String>, // Given name romaji hint (AniList "first")
    pub last_name_hint: Option<String>, // Family name romaji hint (AniList "last")
    #[serde(default)]
    pub seiyuu: Option<String>, // Voice actor name (e.g. "花澤香菜")
    #[serde(default)]
    pub seiyuu_image_url: Option<String>, // VA portrait URL (AniList only)
    #[serde(default)]
    pub seiyuu_image_bytes: Option<Vec<u8>>,
    #[serde(default)]
    pub seiyuu_image_ext: Option<String>,
    #[serde(default)]
    pub seiyuu_image_width: Option<u32>,
    #[serde(default)]
    pub seiyuu_image_height: Option<u32>,
}

/// Categorized characters for a single game/media.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterData {
    pub main: Vec<Character>,
    pub primary: Vec<Character>,
    pub side: Vec<Character>,
    pub appears: Vec<Character>,
}

impl CharacterData {
    pub fn new() -> Self {
        Self {
            main: Vec::new(),
            primary: Vec::new(),
            side: Vec::new(),
            appears: Vec::new(),
        }
    }

    /// Iterate over all characters across all role categories.
    pub fn all_characters(&self) -> impl Iterator<Item = &Character> {
        self.main
            .iter()
            .chain(self.primary.iter())
            .chain(self.side.iter())
            .chain(self.appears.iter())
    }

    /// Mutable iterator (used for populating image_bytes after download).
    pub fn all_characters_mut(&mut self) -> impl Iterator<Item = &mut Character> {
        self.main
            .iter_mut()
            .chain(self.primary.iter_mut())
            .chain(self.side.iter_mut())
            .chain(self.appears.iter_mut())
    }
}

