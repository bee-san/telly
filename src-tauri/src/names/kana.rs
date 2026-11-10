// Copied from bee-san/Japanese_Character_Name_Dictionary (characterdictionary.tokyo),
// yomitan-dict-builder/src/kana.rs at 2e3949d. MIT licence, Copyright (c) 2026 Autumn (Bee).
// Telly's changes are marked "Telly:".

//! Low-level kana conversion utilities.
//!
//! Provides romaji→hiragana, katakana↔hiragana conversion, and kanji detection.
//! These are pure text transforms with no name-level semantics.

/// Check if text contains kanji characters.
/// Covers CJK Unified Ideographs, Extensions A–H, and Compatibility Ideographs.
pub fn contains_kanji(text: &str) -> bool {
    text.chars().any(is_kanji)
}

/// Returns true if the character is a CJK ideograph (kanji).
fn is_kanji(c: char) -> bool {
    let code = c as u32;
    // CJK Unified Ideographs
    (0x4E00..=0x9FFF).contains(&code)
    // Extension A
    || (0x3400..=0x4DBF).contains(&code)
    // Extension B
    || (0x20000..=0x2A6DF).contains(&code)
    // Extension C
    || (0x2A700..=0x2B73F).contains(&code)
    // Extension D
    || (0x2B740..=0x2B81F).contains(&code)
    // Extension E
    || (0x2B820..=0x2CEAF).contains(&code)
    // Extension F
    || (0x2CEB0..=0x2EBEF).contains(&code)
    // Extension G
    || (0x30000..=0x3134F).contains(&code)
    // Extension H
    || (0x31350..=0x323AF).contains(&code)
    // CJK Compatibility Ideographs
    || (0xF900..=0xFAFF).contains(&code)
    // CJK Compatibility Ideographs Supplement
    || (0x2F800..=0x2FA1F).contains(&code)
    // Ideographic Iteration Mark (々, U+3005).
    // Not in any CJK Ideographs block but behaves like kanji in names:
    // it repeats the preceding character (e.g. 寧々 = 寧寧 = "nene").
    // Classifying it as kanji prevents the split heuristic from treating
    // it as a kana boundary and stops kata_to_hira from passing it through
    // unchanged into readings.
    || code == 0x3005
}

/// Returns true if the character is hiragana (U+3041–U+3096).
fn is_hiragana(c: char) -> bool {
    (0x3041..=0x3096).contains(&(c as u32))
}

/// Returns true if the character is katakana (U+30A1–U+30F6, plus ー U+30FC).
fn is_katakana(c: char) -> bool {
    let code = c as u32;
    (0x30A1..=0x30F6).contains(&code) || code == 0x30FC
}

/// Check if text contains any Japanese characters (kanji, hiragana, or katakana).
pub fn contains_japanese(text: &str) -> bool {
    text.chars()
        .any(|c| is_kanji(c) || is_hiragana(c) || is_katakana(c))
}

/// Convert katakana to hiragana.
/// Katakana range: U+30A1 (ァ) to U+30F6 (ヶ). Subtract 0x60 to get hiragana equivalent.
pub fn kata_to_hira(text: &str) -> String {
    text.chars()
        .map(|c| {
            let code = c as u32;
            if (0x30A1..=0x30F6).contains(&code) {
                char::from_u32(code - 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// Convert hiragana to katakana.
/// Hiragana range: U+3041 (ぁ) to U+3096 (ゖ). Add 0x60 to get katakana equivalent.
pub fn hira_to_kata(text: &str) -> String {
    text.chars()
        .map(|c| {
            let code = c as u32;
            if (0x3041..=0x3096).contains(&code) {
                char::from_u32(code + 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// Returns true if the character is a syllable boundary marker in romanized Japanese.
///
/// In romanized names, apostrophes and hyphens serve as disambiguation markers:
/// - Apostrophe: "Shin'ichi" means しんいち (ん+い), not しにち (に)
/// - Hyphen: occasionally used similarly in some romanization systems
///
/// These characters force the preceding 'n' to be treated as ん (syllabic n)
/// rather than the start of a な-row syllable.
fn is_syllable_boundary(c: char) -> bool {
    matches!(c, '\'' | '\u{2019}' | '\u{2018}' | '-' | '.')
}

/// Convert romanized text to hiragana.
/// Handles double consonants (っ), special 'n' rules, multi-char sequences,
/// and syllable boundary markers (apostrophes, hyphens).
///
/// Syllable boundary markers like apostrophes force the preceding 'n' to become ん.
/// For example: "Shin'ichi" → し+ん+い+ち = しんいち (not しにち).
/// Other non-alphabetic characters (digits, misc punctuation) are silently dropped.
pub fn alphabet_to_kana(input: &str) -> String {
    let text = input.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // Handle syllable boundary markers: force pending 'n' to ん
        // The 'n' would already have been emitted or not by this point,
        // but the boundary marker tells us to treat the NEXT segment as
        // a fresh syllable start. We just skip the marker itself.
        if is_syllable_boundary(c) {
            // If the previous character was 'n' that got consumed as part of
            // a な-row check, we need to handle that. But actually, the way
            // the algorithm works, we need to check: if the char before the
            // boundary is 'n' and it hasn't been consumed yet...
            //
            // Simpler approach: when we see a boundary marker after 'n',
            // the 'n' rule (step 4) would have already NOT matched because
            // the next char is the boundary marker (not a vowel/y), so 'n'
            // would have been emitted as ん. The boundary marker just needs
            // to be skipped.
            i += 1;
            continue;
        }

        // Skip non-ASCII-alphabetic, non-space characters (digits, misc punctuation)
        if !c.is_ascii_alphabetic() && c != ' ' {
            i += 1;
            continue;
        }

        // Preserve spaces (used for name part splitting upstream)
        if c == ' ' {
            result.push(' ');
            i += 1;
            continue;
        }

        // 1. Double consonant check: if chars[i] == chars[i+1] and both are consonants → っ
        if i + 1 < chars.len() && chars[i] == chars[i + 1] && is_consonant(chars[i]) {
            result.push('っ');
            i += 1;
            continue;
        }

        // 2. Try 3-character sequence (skip non-alpha chars when building the window)
        if i + 3 <= chars.len() {
            let three: String = chars[i..i + 3].iter().collect();
            if let Some(kana) = lookup_romaji(&three) {
                result.push_str(kana);
                i += 3;
                continue;
            }
        }

        // 3. Try 2-character sequence
        if i + 2 <= chars.len() {
            let two: String = chars[i..i + 2].iter().collect();
            if let Some(kana) = lookup_romaji(&two) {
                result.push_str(kana);
                i += 2;
                continue;
            }
        }

        // 4. Special 'n' handling: ん when NOT followed by a vowel or 'y'
        //    A syllable boundary marker after 'n' means the next char is NOT
        //    a vowel (it's the marker), so 'n' correctly becomes ん.
        if chars[i] == 'n' {
            let next = next_alpha_char(&chars, i + 1);
            if next.is_none() || !is_vowel_or_y(next.unwrap()) {
                result.push('ん');
                i += 1;
                continue;
            }
        }

        // 5. Try 1-character sequence (vowels)
        let one = chars[i].to_string();
        if let Some(kana) = lookup_romaji(&one) {
            result.push_str(kana);
        } else {
            // Unknown alphabetic character — pass through unchanged
            result.push(chars[i]);
        }
        i += 1;
    }

    result
}

/// Look ahead past syllable boundary markers to find the next alphabetic character.
fn next_alpha_char(chars: &[char], start: usize) -> Option<char> {
    chars.get(start).copied()
}

fn is_consonant(c: char) -> bool {
    matches!(
        c,
        'b' | 'c'
            | 'd'
            | 'f'
            | 'g'
            | 'h'
            | 'j'
            | 'k'
            | 'l'
            | 'm'
            | 'n'
            | 'p'
            | 'q'
            | 'r'
            | 's'
            | 't'
            | 'v'
            | 'w'
            | 'x'
            | 'y'
            | 'z'
    )
}

fn is_vowel_or_y(c: char) -> bool {
    matches!(c, 'a' | 'i' | 'u' | 'e' | 'o' | 'y')
}

fn lookup_romaji(key: &str) -> Option<&'static str> {
    match key {
        // === 3-character sequences ===
        // Hepburn standard
        "sha" => Some("しゃ"),
        "shi" => Some("し"),
        "shu" => Some("しゅ"),
        "sho" => Some("しょ"),
        "she" => Some("しぇ"),
        "chi" => Some("ち"),
        "tsu" => Some("つ"),
        "cha" => Some("ちゃ"),
        "chu" => Some("ちゅ"),
        "cho" => Some("ちょ"),
        "che" => Some("ちぇ"),
        "nya" => Some("にゃ"),
        "nyu" => Some("にゅ"),
        "nyo" => Some("にょ"),
        "hya" => Some("ひゃ"),
        "hyu" => Some("ひゅ"),
        "hyo" => Some("ひょ"),
        "mya" => Some("みゃ"),
        "myu" => Some("みゅ"),
        "myo" => Some("みょ"),
        "rya" => Some("りゃ"),
        "ryu" => Some("りゅ"),
        "ryo" => Some("りょ"),
        "gya" => Some("ぎゃ"),
        "gyu" => Some("ぎゅ"),
        "gyo" => Some("ぎょ"),
        "bya" => Some("びゃ"),
        "byu" => Some("びゅ"),
        "byo" => Some("びょ"),
        "pya" => Some("ぴゃ"),
        "pyu" => Some("ぴゅ"),
        "pyo" => Some("ぴょ"),
        "kya" => Some("きゃ"),
        "kyu" => Some("きゅ"),
        "kyo" => Some("きょ"),
        "jya" => Some("じゃ"),
        "jyu" => Some("じゅ"),
        "jyo" => Some("じょ"),
        // Nihon-shiki / Kunrei-shiki variants
        "tya" => Some("ちゃ"),
        "tyu" => Some("ちゅ"),
        "tyo" => Some("ちょ"),
        "sya" => Some("しゃ"),
        "syu" => Some("しゅ"),
        "syo" => Some("しょ"),
        "zya" => Some("じゃ"),
        "zyu" => Some("じゅ"),
        "zyo" => Some("じょ"),
        "dya" => Some("ぢゃ"),
        "dyu" => Some("ぢゅ"),
        "dyo" => Some("ぢょ"),
        // Foreign-sound kana
        "tsa" => Some("つぁ"),
        "tsi" => Some("つぃ"),
        "tse" => Some("つぇ"),
        "tso" => Some("つぉ"),

        // === 2-character sequences ===
        "ka" => Some("か"),
        "ki" => Some("き"),
        "ku" => Some("く"),
        "ke" => Some("け"),
        "ko" => Some("こ"),
        "sa" => Some("さ"),
        "si" => Some("し"),
        "su" => Some("す"),
        "se" => Some("せ"),
        "so" => Some("そ"),
        "ta" => Some("た"),
        "ti" => Some("ち"),
        "tu" => Some("つ"),
        "te" => Some("て"),
        "to" => Some("と"),
        "na" => Some("な"),
        "ni" => Some("に"),
        "nu" => Some("ぬ"),
        "ne" => Some("ね"),
        "no" => Some("の"),
        "ha" => Some("は"),
        "hi" => Some("ひ"),
        "hu" => Some("ふ"),
        "fu" => Some("ふ"),
        "he" => Some("へ"),
        "ho" => Some("ほ"),
        "fa" => Some("ふぁ"),
        "fi" => Some("ふぃ"),
        "fe" => Some("ふぇ"),
        "fo" => Some("ふぉ"),
        "ji" => Some("じ"),
        "je" => Some("じぇ"),
        "la" => Some("ら"),
        "li" => Some("り"),
        "lu" => Some("る"),
        "le" => Some("れ"),
        "lo" => Some("ろ"),
        "ma" => Some("ま"),
        "mi" => Some("み"),
        "mu" => Some("む"),
        "me" => Some("め"),
        "mo" => Some("も"),
        "ra" => Some("ら"),
        "ri" => Some("り"),
        "ru" => Some("る"),
        "re" => Some("れ"),
        "ro" => Some("ろ"),
        "ya" => Some("や"),
        "yu" => Some("ゆ"),
        "yo" => Some("よ"),
        "wa" => Some("わ"),
        "wi" => Some("ゐ"),
        "we" => Some("ゑ"),
        "wo" => Some("を"),
        "ga" => Some("が"),
        "gi" => Some("ぎ"),
        "gu" => Some("ぐ"),
        "ge" => Some("げ"),
        "go" => Some("ご"),
        "za" => Some("ざ"),
        "zi" => Some("じ"),
        "zu" => Some("ず"),
        "ze" => Some("ぜ"),
        "zo" => Some("ぞ"),
        "da" => Some("だ"),
        "di" => Some("ぢ"),
        "du" => Some("づ"),
        "de" => Some("で"),
        "do" => Some("ど"),
        "ba" => Some("ば"),
        "bi" => Some("び"),
        "bu" => Some("ぶ"),
        "be" => Some("べ"),
        "bo" => Some("ぼ"),
        "pa" => Some("ぱ"),
        "pi" => Some("ぴ"),
        "pu" => Some("ぷ"),
        "pe" => Some("ぺ"),
        "po" => Some("ぽ"),
        "ja" => Some("じゃ"),
        "ju" => Some("じゅ"),
        "jo" => Some("じょ"),

        // === 1-character sequences (vowels only; 'n' handled separately) ===
        "a" => Some("あ"),
        "i" => Some("い"),
        "u" => Some("う"),
        "e" => Some("え"),
        "o" => Some("お"),

        _ => None,
    }
}

