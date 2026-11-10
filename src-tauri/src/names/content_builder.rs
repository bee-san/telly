// Copied from bee-san/Japanese_Character_Name_Dictionary (characterdictionary.tokyo),
// yomitan-dict-builder/src/content_builder.rs at 2e3949d. MIT licence, Copyright (c) 2026 Autumn (Bee).
// Telly's changes are marked "Telly:".

use regex::Regex;
use serde_json::json;
use std::sync::LazyLock;

use super::models::{Character, CharacterTrait};

// === Cached regex patterns (compiled once, reused across all calls) ===

static RE_VNDB_SPOILER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)\[spoiler\].*?\[/spoiler\]").unwrap());
static RE_ANILIST_SPOILER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)~!.*?!~").unwrap());
static RE_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\[url=[^\]]+\]([^\[]*)\[/url\]").unwrap());
static RE_QUOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)\[quote\](.*?)\[/quote\]").unwrap());
static RE_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)\[code\](.*?)\[/code\]").unwrap());
static RE_RAW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)\[raw\](.*?)\[/raw\]").unwrap());
static RE_UNDERLINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)\[u\](.*?)\[/u\]").unwrap());
static RE_STRIKETHROUGH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)\[s\](.*?)\[/s\]").unwrap());
static RE_BBCODE_INNER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)\[(b|i)\]([^\[]*?)\[/(b|i)\]").unwrap());
static RE_PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x00NODE(\d+)\x00").unwrap());

/// Role badge colors
const ROLE_COLORS: &[(&str, &str)] = &[
    ("main", "#4CAF50"),    // green
    ("primary", "#2196F3"), // blue
    ("side", "#FF9800"),    // orange
    ("appears", "#9E9E9E"), // gray
];

/// Role display labels
const ROLE_LABELS: &[(&str, &str)] = &[
    ("main", "Protagonist"),
    ("primary", "Main Character"),
    ("side", "Side Character"),
    ("appears", "Minor Role"),
];

/// Month names for birthday formatting
const MONTH_NAMES: &[(u32, &str)] = &[
    (1, "January"),
    (2, "February"),
    (3, "March"),
    (4, "April"),
    (5, "May"),
    (6, "June"),
    (7, "July"),
    (8, "August"),
    (9, "September"),
    (10, "October"),
    (11, "November"),
    (12, "December"),
];

/// Sex display mapping — must handle both "m"/"f" and "male"/"female" inputs
const SEX_DISPLAY: &[(&str, &str)] = &[
    ("m", "♂ Male"),
    ("f", "♀ Female"),
    ("male", "♂ Male"),
    ("female", "♀ Female"),
];

const ID_NAME: &str = "name";
const ID_NAME_ROMAJI: &str = "name-romaji";
const ID_PORTRAIT_CONTAINER: &str = "portrait-container";
const ID_SOURCE: &str = "source";
const ID_ROLE_CONTAINER: &str = "role-container";
const ID_ROLE: &str = "role";

/// Per-dictionary display settings that control which sections appear in
/// the generated Yomitan structured-content cards.
#[derive(Clone, Debug)]
pub struct DictSettings {
    pub show_image: bool,
    pub show_tag: bool,
    pub show_description: bool,
    pub show_traits: bool,
    pub show_spoilers: bool,
    pub honorifics: bool,
    pub show_seiyuu: bool,
}

impl Default for DictSettings {
    fn default() -> Self {
        Self {
            show_image: true,
            show_tag: true,
            show_description: true,
            show_traits: true,
            show_spoilers: true,
            honorifics: true,
            show_seiyuu: true,
        }
    }
}

pub struct ContentBuilder {
    settings: DictSettings,
}

impl ContentBuilder {
    pub fn new(settings: DictSettings) -> Self {
        Self { settings }
    }

    /// Remove spoiler content from text. Both VNDB and AniList formats.
    pub fn strip_spoilers(text: &str) -> String {
        // VNDB: [spoiler]...[/spoiler]
        let text = RE_VNDB_SPOILER.replace_all(text, "");
        // AniList: ~!...!~
        RE_ANILIST_SPOILER.replace_all(&text, "").trim().to_string()
    }

    /// Parse VNDB markup: strip [url=...], [quote], [code], [raw] tags down to inner text.
    pub fn parse_vndb_markup(text: &str) -> String {
        // [url=https://...]text[/url] → text
        let text = RE_URL.replace_all(text, "$1");
        // [quote]...[/quote] → inner text
        let text = RE_QUOTE.replace_all(&text, "$1");
        // [code]...[/code] → inner text
        let text = RE_CODE.replace_all(&text, "$1");
        // [raw]...[/raw] → inner text (raw means "don't format", so we just unwrap)
        let text = RE_RAW.replace_all(&text, "$1");
        // [u]...[/u] → inner text (Yomitan doesn't support textDecoration style)
        let text = RE_UNDERLINE.replace_all(&text, "$1");
        // [s]...[/s] → inner text (Yomitan doesn't support textDecoration style)
        RE_STRIKETHROUGH.replace_all(&text, "$1").to_string()
    }

    /// Parse BBCode [b] and [i] tags into Yomitan structured content nodes.
    /// Returns a serde_json::Value that is either a plain string (no tags found)
    /// or an array of mixed strings and {"tag":"b"/"i","content":...} objects.
    pub fn parse_bbcode_to_structured(text: &str) -> serde_json::Value {
        // Process innermost BBCode tags first, then work outward.
        // Each pass finds tags whose content has no nested `[x]` tags (no `[` inside).
        let mut nodes: Vec<serde_json::Value> = Vec::new();
        let mut working = text.to_string();
        let placeholder_prefix = "\x00NODE";

        while let Some(cap) = RE_BBCODE_INNER.captures(&working) {
            let open_tag = cap[1].to_lowercase();
            let close_tag = cap[3].to_lowercase();

            // Mismatched tags — strip the tags, keep content
            if open_tag != close_tag {
                let full = cap.get(0).unwrap();
                working = format!(
                    "{}{}{}",
                    &working[..full.start()],
                    &cap[2],
                    &working[full.end()..]
                );
                continue;
            }

            let inner_text = &cap[2];
            // Build the inner content: resolve any placeholders in it
            let inner_content = Self::resolve_placeholders(inner_text, &nodes);

            let node = match open_tag.as_str() {
                "b" => json!({
                    "tag": "span",
                    "style": { "fontWeight": "bold" },
                    "content": inner_content
                }),
                "i" => json!({
                    "tag": "span",
                    "style": { "fontStyle": "italic" },
                    "content": inner_content
                }),
                _ => json!({ "tag": "span", "content": inner_content }),
            };

            let idx = nodes.len();
            nodes.push(node);
            let placeholder = format!("{}{}\x00", placeholder_prefix, idx);
            let full = cap.get(0).unwrap();
            working = format!(
                "{}{}{}",
                &working[..full.start()],
                placeholder,
                &working[full.end()..]
            );
        }

        // Now resolve the final working string with all placeholders
        Self::resolve_placeholders(&working, &nodes)
    }

    /// Resolve placeholder markers back into structured content nodes.
    /// Returns a single Value (string, object, or array of mixed).
    fn resolve_placeholders(text: &str, nodes: &[serde_json::Value]) -> serde_json::Value {
        if !RE_PLACEHOLDER.is_match(text) {
            return json!(text);
        }

        let mut result: Vec<serde_json::Value> = Vec::new();
        let mut last_end = 0;

        for cap in RE_PLACEHOLDER.captures_iter(text) {
            let full = cap.get(0).unwrap();
            if full.start() > last_end {
                let before = &text[last_end..full.start()];
                if !before.is_empty() {
                    result.push(json!(before));
                }
            }
            let idx: usize = cap[1].parse().unwrap();
            if idx < nodes.len() {
                result.push(nodes[idx].clone());
            }
            last_end = full.end();
        }

        if last_end < text.len() {
            let after = &text[last_end..];
            if !after.is_empty() {
                result.push(json!(after));
            }
        }

        if result.len() == 1 {
            result.into_iter().next().unwrap()
        } else {
            json!(result)
        }
    }

    /// Format birthday [month, day] → "September 1"
    pub fn format_birthday(birthday: &[u32]) -> String {
        if birthday.len() < 2 {
            return String::new();
        }
        let month = birthday[0];
        let day = birthday[1];
        let month_name = MONTH_NAMES
            .iter()
            .find(|(m, _)| *m == month)
            .map(|(_, name)| *name)
            .unwrap_or("Unknown");
        format!("{} {}", month_name, day)
    }

    /// Build physical stats line.
    pub fn format_stats(&self, char: &Character) -> String {
        let mut parts = Vec::new();

        if let Some(ref sex) = char.sex {
            let sex_lower = sex.to_lowercase();
            if let Some((_, display)) = SEX_DISPLAY.iter().find(|(k, _)| *k == sex_lower.as_str()) {
                parts.push(display.to_string());
            }
        }

        if let Some(ref age) = char.age {
            parts.push(format!("{} years", age));
        }

        if let Some(height) = char.height {
            parts.push(format!("{}cm", height));
        }

        if let Some(weight) = char.weight {
            parts.push(format!("{}kg", weight));
        }

        if let Some(ref blood_type) = char.blood_type {
            parts.push(format!("Blood Type {}", blood_type));
        }

        if let Some(ref birthday) = char.birthday {
            let formatted = Self::format_birthday(birthday);
            if !formatted.is_empty() {
                parts.push(format!("Birthday: {}", formatted));
            }
        }

        parts.join(" • ")
    }

    /// Build trait items grouped by category, filtered by spoiler_level.
    /// Returns a Vec of Yomitan `li` content items.
    pub fn build_traits_by_category(&self, char: &Character) -> Vec<serde_json::Value> {
        let mut items = Vec::new();

        let categories: &[(&[CharacterTrait], &str)] = &[
            (&char.personality, "Personality"),
            (&char.roles, "Role"),
            (&char.engages_in, "Activities"),
            (&char.subject_of, "Subject of"),
        ];

        for (traits, label) in categories {
            if traits.is_empty() {
                continue;
            }

            // Filter traits: when spoilers are hidden, only include non-spoiler traits (spoiler == 0)
            let filtered: Vec<&str> = traits
                .iter()
                .filter(|t| !t.name.is_empty() && (self.settings.show_spoilers || t.spoiler == 0))
                .map(|t| t.name.as_str())
                .collect();

            if !filtered.is_empty() {
                items.push(json!({
                    "tag": "li",
                    "content": format!("{}: {}", label, filtered.join(", "))
                }));
            }
        }

        items
    }

    /// Maximum display height for character portrait images (in CSS pixels).
    const MAX_DISPLAY_HEIGHT: u32 = 100;
    /// Fallback display width when actual dimensions are unknown.
    const FALLBACK_DISPLAY_WIDTH: u32 = 67;

    /// Build the complete Yomitan structured content for a character card.
    pub fn build_content(
        &self,
        char: &Character,
        image_path: Option<&str>,
        image_dims: Option<(u32, u32)>,
        seiyuu_image_path: Option<&str>,
        seiyuu_image_dims: Option<(u32, u32)>,
        game_title: &str,
    ) -> serde_json::Value {
        let mut content: Vec<serde_json::Value> = Vec::new();

        // ===== Always shown: Name block =====

        // Japanese name (large, bold)
        if !char.name_original.is_empty() {
            content.push(json!({
                "tag": "div",
                "data": { "id": ID_NAME },
                "style": { "fontWeight": "bold", "fontSize": "1.2em" },
                "content": &char.name_original
            }));
        }

        // Romanized name (italic, gray)
        if !char.name.is_empty() {
            content.push(json!({
                "tag": "div",
                "data": { "id": ID_NAME_ROMAJI },
                "style": { "fontStyle": "italic", "color": "#666", "marginBottom": "8px" },
                "content": &char.name
            }));
        }

        // ===== Character portrait image (gated by show_image) =====
        if self.settings.show_image {
            if let Some(path) = image_path {
                let (display_w, display_h) = match image_dims {
                    Some((w, h)) if w > 0 && h > 0 => {
                        let dh = Self::MAX_DISPLAY_HEIGHT;
                        let dw = (w * dh + h / 2) / h;
                        (dw, dh)
                    }
                    _ => (Self::FALLBACK_DISPLAY_WIDTH, Self::MAX_DISPLAY_HEIGHT),
                };
                content.push(json!({
                    "tag": "div",
                    "data": { "id": ID_PORTRAIT_CONTAINER },
                    "content": {
                        "tag": "img",
                        "path": path,
                        "width": display_w,
                        "height": display_h,
                        "sizeUnits": "px",
                        "collapsible": false,
                        "collapsed": false,
                        "background": false
                    }
                }));
            }
        }

        // Game/media title (always shown)
        if !game_title.is_empty() {
            content.push(json!({
                "tag": "div",
                "data": { "id": ID_SOURCE },
                "style": { "fontSize": "0.9em", "color": "#888", "marginTop": "4px" },
                "content": format!("From: {}", game_title)
            }));
        }

        // ===== Role badge (gated by show_tag) =====
        if self.settings.show_tag {
            let role = char.role.as_str();
            let role_color = ROLE_COLORS
                .iter()
                .find(|(r, _)| *r == role)
                .map(|(_, c)| *c)
                .unwrap_or("#9E9E9E");
            let role_label = ROLE_LABELS
                .iter()
                .find(|(r, _)| *r == role)
                .map(|(_, l)| *l)
                .unwrap_or("Unknown");

            // Wrap in a div for better compatibility with tools that strip HTML, like JL.
            // Yomitan's schema does not support `"display": "inline"` in style,
            // so we nest a div > span instead of using a single inline div.
            content.push(json!({
                "tag": "div",
                "data": { "id": ID_ROLE_CONTAINER },
                "content": {
                    "tag": "span",
                    "style": {
                        "background": role_color,
                        "color": "white",
                        "padding": "2px 6px",
                        "borderRadius": "3px",
                        "fontSize": "0.85em",
                        "marginTop": "4px"
                    },
                    "data": { "id": ID_ROLE },
                    "content": role_label
                }
            }));
        }

        // ===== Description section (gated by show_description) =====
        if self.settings.show_description {
            if let Some(ref desc) = char.description {
                if !desc.trim().is_empty() {
                    let display_desc = if !self.settings.show_spoilers {
                        Self::strip_spoilers(desc)
                    } else {
                        desc.clone()
                    };

                    if !display_desc.is_empty() {
                        let parsed = Self::parse_vndb_markup(&display_desc);
                        let structured = Self::parse_bbcode_to_structured(&parsed);
                        content.push(json!({
                            "tag": "details",
                            "content": [
                                { "tag": "summary", "content": "Description" },
                                {
                                    "tag": "div",
                                    "style": { "fontSize": "0.9em", "marginTop": "4px" },
                                    "content": structured
                                }
                            ]
                        }));
                    }
                }
            }
        }

        // ===== Character Information / Traits section (gated by show_traits) =====
        if self.settings.show_traits {
            let mut info_items: Vec<serde_json::Value> = Vec::new();

            // Physical stats as compact line
            let stats = self.format_stats(char);
            if !stats.is_empty() {
                info_items.push(json!({
                    "tag": "li",
                    "style": { "fontWeight": "bold" },
                    "content": stats
                }));
            }

            // Traits organized by category (filtered by spoiler setting)
            let trait_items = self.build_traits_by_category(char);
            info_items.extend(trait_items);

            if !info_items.is_empty() {
                content.push(json!({
                    "tag": "details",
                    "content": [
                        { "tag": "summary", "content": "Character Information" },
                        {
                            "tag": "ul",
                            "style": { "marginTop": "4px", "paddingLeft": "20px" },
                            "content": info_items
                        }
                    ]
                }));
            }
        }

        // ===== Seiyuu section (gated by show_seiyuu) =====
        if self.settings.show_seiyuu {
            if let Some(ref va) = char.seiyuu {
                if !va.is_empty() {
                    let mut seiyuu_inner: Vec<serde_json::Value> = Vec::new();

                    // Seiyuu portrait image (if available)
                    if let Some(va_path) = seiyuu_image_path {
                        let (display_w, display_h) = match seiyuu_image_dims {
                            Some((w, h)) if w > 0 && h > 0 => {
                                let dh = 60u32;
                                let dw = (w * dh + h / 2) / h;
                                (dw, dh)
                            }
                            _ => (40, 60),
                        };
                        seiyuu_inner.push(json!({
                            "tag": "img",
                            "path": va_path,
                            "width": display_w,
                            "height": display_h,
                            "sizeUnits": "px",
                            "collapsible": false,
                            "collapsed": false,
                            "background": false
                        }));
                    }

                    seiyuu_inner.push(json!({
                        "tag": "div",
                        "style": { "fontSize": "0.9em", "marginTop": "4px" },
                        "content": va.as_str()
                    }));

                    content.push(json!({
                        "tag": "details",
                        "content": [
                            { "tag": "summary", "content": "Voiced by" },
                            {
                                "tag": "div",
                                "style": { "marginTop": "4px" },
                                "content": seiyuu_inner
                            }
                        ]
                    }));
                }
            }
        }

        json!({
            "type": "structured-content",
            "content": content
        })
    }

    /// Build a structured content card for a character with multiple media appearances.
    ///
    /// Instead of showing a single "From: title" + role badge, this renders a list
    /// of (role badge, "From: title") rows — one per appearance — sorted by role
    /// importance (main > primary > side > appears).
    ///
    /// Single-value fields (image, description, traits, seiyuu) come from the
    /// `char` parameter (already merged by the caller using first-non-None logic).
    pub fn build_merged_content(
        &self,
        char: &Character,
        image_path: Option<&str>,
        image_dims: Option<(u32, u32)>,
        seiyuu_image_path: Option<&str>,
        seiyuu_image_dims: Option<(u32, u32)>,
        appearances: &[(String, String)], // (role, media_title), pre-sorted by importance
    ) -> serde_json::Value {
        let mut content: Vec<serde_json::Value> = Vec::new();

        // ===== Always shown: Name block =====

        // Japanese name (large, bold)
        if !char.name_original.is_empty() {
            content.push(json!({
                "tag": "div",
                "data": { "id": ID_NAME },
                "style": { "fontWeight": "bold", "fontSize": "1.2em" },
                "content": &char.name_original
            }));
        }

        // Romanized name (italic, gray)
        if !char.name.is_empty() {
            content.push(json!({
                "tag": "div",
                "data": { "id": ID_NAME_ROMAJI },
                "style": { "fontStyle": "italic", "color": "#666", "marginBottom": "8px" },
                "content": &char.name
            }));
        }

        // ===== Character portrait image (gated by show_image) =====
        if self.settings.show_image {
            if let Some(path) = image_path {
                let (display_w, display_h) = match image_dims {
                    Some((w, h)) if w > 0 && h > 0 => {
                        let dh = Self::MAX_DISPLAY_HEIGHT;
                        let dw = (w * dh + h / 2) / h;
                        (dw, dh)
                    }
                    _ => (Self::FALLBACK_DISPLAY_WIDTH, Self::MAX_DISPLAY_HEIGHT),
                };
                content.push(json!({
                    "tag": "div",
                    "data": { "id": ID_PORTRAIT_CONTAINER },
                    "content": {
                        "tag": "img",
                        "path": path,
                        "width": display_w,
                        "height": display_h,
                        "sizeUnits": "px",
                        "collapsible": false,
                        "collapsed": false,
                        "background": false
                    }
                }));
            }
        }

        // ===== Appearances: role badge + "From: title" for each media =====
        for (role, media_title) in appearances {
            if self.settings.show_tag {
                let role_color = ROLE_COLORS
                    .iter()
                    .find(|(r, _)| *r == role.as_str())
                    .map(|(_, c)| *c)
                    .unwrap_or("#9E9E9E");
                let role_label = ROLE_LABELS
                    .iter()
                    .find(|(r, _)| *r == role.as_str())
                    .map(|(_, l)| *l)
                    .unwrap_or("Unknown");

                let mut row_content: Vec<serde_json::Value> = vec![json!({
                    "tag": "span",
                    "style": {
                        "background": role_color,
                        "color": "white",
                        "padding": "2px 6px",
                        "borderRadius": "3px",
                        "fontSize": "0.85em"
                    },
                    "data": { "id": ID_ROLE },
                    "content": role_label
                })];

                if !media_title.is_empty() {
                    row_content.push(json!({
                        "tag": "span",
                        "data": { "id": ID_SOURCE },
                        "style": { "fontSize": "0.9em", "color": "#888" },
                        "content": format!(" From: {}", media_title)
                    }));
                }

                content.push(json!({
                    "tag": "div",
                    "data": { "id": ID_ROLE_CONTAINER },
                    "style": { "marginTop": "4px" },
                    "content": row_content
                }));
            } else if !media_title.is_empty() {
                // No role badge, just show the media title
                content.push(json!({
                    "tag": "div",
                    "data": { "id": ID_SOURCE },
                    "style": { "fontSize": "0.9em", "color": "#888", "marginTop": "4px" },
                    "content": format!("From: {}", media_title)
                }));
            }
        }

        // ===== Description section (gated by show_description) =====
        if self.settings.show_description {
            if let Some(ref desc) = char.description {
                if !desc.trim().is_empty() {
                    let display_desc = if !self.settings.show_spoilers {
                        Self::strip_spoilers(desc)
                    } else {
                        desc.clone()
                    };

                    if !display_desc.is_empty() {
                        let parsed = Self::parse_vndb_markup(&display_desc);
                        let structured = Self::parse_bbcode_to_structured(&parsed);
                        content.push(json!({
                            "tag": "details",
                            "content": [
                                { "tag": "summary", "content": "Description" },
                                {
                                    "tag": "div",
                                    "style": { "fontSize": "0.9em", "marginTop": "4px" },
                                    "content": structured
                                }
                            ]
                        }));
                    }
                }
            }
        }

        // ===== Character Information / Traits section (gated by show_traits) =====
        if self.settings.show_traits {
            let mut info_items: Vec<serde_json::Value> = Vec::new();

            // Physical stats as compact line
            let stats = self.format_stats(char);
            if !stats.is_empty() {
                info_items.push(json!({
                    "tag": "li",
                    "style": { "fontWeight": "bold" },
                    "content": stats
                }));
            }

            // Traits organized by category (filtered by spoiler setting)
            let trait_items = self.build_traits_by_category(char);
            info_items.extend(trait_items);

            if !info_items.is_empty() {
                content.push(json!({
                    "tag": "details",
                    "content": [
                        { "tag": "summary", "content": "Character Information" },
                        {
                            "tag": "ul",
                            "style": { "marginTop": "4px", "paddingLeft": "20px" },
                            "content": info_items
                        }
                    ]
                }));
            }
        }

        // ===== Seiyuu section (gated by show_seiyuu) =====
        if self.settings.show_seiyuu {
            if let Some(ref va) = char.seiyuu {
                if !va.is_empty() {
                    let mut seiyuu_inner: Vec<serde_json::Value> = Vec::new();

                    // Seiyuu portrait image (if available)
                    if let Some(va_path) = seiyuu_image_path {
                        let (display_w, display_h) = match seiyuu_image_dims {
                            Some((w, h)) if w > 0 && h > 0 => {
                                let dh = 60u32;
                                let dw = (w * dh + h / 2) / h;
                                (dw, dh)
                            }
                            _ => (40, 60),
                        };
                        seiyuu_inner.push(json!({
                            "tag": "img",
                            "path": va_path,
                            "width": display_w,
                            "height": display_h,
                            "sizeUnits": "px",
                            "collapsible": false,
                            "collapsed": false,
                            "background": false
                        }));
                    }

                    seiyuu_inner.push(json!({
                        "tag": "div",
                        "style": { "fontSize": "0.9em", "marginTop": "4px" },
                        "content": va.as_str()
                    }));

                    content.push(json!({
                        "tag": "details",
                        "content": [
                            { "tag": "summary", "content": "Voiced by" },
                            {
                                "tag": "div",
                                "style": { "marginTop": "4px" },
                                "content": seiyuu_inner
                            }
                        ]
                    }));
                }
            }
        }

        json!({
            "type": "structured-content",
            "content": content
        })
    }

    /// Build a structured content card with an honorific description banner.
    /// Clones the base content and prepends a styled honorific note.
    pub fn build_honorific_content(
        base_content: &serde_json::Value,
        suffix_display: &str,
        suffix_description: &str,
    ) -> serde_json::Value {
        let mut content_array = match base_content.get("content") {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => return base_content.clone(),
        };

        // Honorific banner: styled div at the top of the card
        // Note: Yomitan's structuredContentStyle only allows borderColor/borderStyle/borderWidth
        // (applied to all sides), not borderLeft. We use a thin border + background tint instead.
        let banner = json!({
            "tag": "div",
            "style": {
                "fontSize": "0.85em",
                "color": "#4A90D9",
                "borderColor": "#4A90D9",
                "borderStyle": "solid",
                "borderWidth": "0 0 0 3px",
                "paddingLeft": "6px",
                "marginBottom": "6px"
            },
            "content": [
                {
                    "tag": "span",
                    "style": { "fontWeight": "bold" },
                    "content": suffix_display
                },
                {
                    "tag": "span",
                    "content": format!(" — {}", suffix_description)
                }
            ]
        });

        content_array.insert(0, banner);

        json!({
            "type": "structured-content",
            "content": content_array
        })
    }

    /// Create a single Yomitan term entry.
    pub fn create_term_entry(
        term: &str,
        reading: &str,
        role: &str,
        score: i32,
        structured_content: &serde_json::Value,
    ) -> serde_json::Value {
        json!([
            term,
            reading,
            if role.is_empty() {
                "name".to_string()
            } else {
                format!("name {}", role)
            },
            "",
            score,
            [structured_content],
            0,
            ""
        ])
    }
}

