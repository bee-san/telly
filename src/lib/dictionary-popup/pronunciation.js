// @ts-nocheck: plain JavaScript; src/index.d.ts says what it exports.
// Pitch accent and frequency, worked out the way Yomitan does (japanese.js:
// getPitchCategory, getDownstepPositions). A port of LFG's Pronunciation.kt.
// SPDX-License-Identifier: GPL-3.0-or-later

/** The kinds of accent, by the name the styles know them by. */
export const KINDS = { heiban: "平板", atamadaka: "頭高", nakadaka: "中高", odaka: "尾高", kifuku: "起伏" };

const SMALL = "ゃゅょぁぃぅぇぉゎゕゖャュョァィゥェォヮヵヶ";
const VERB_OR_ADJECTIVE = new Set(["v1", "v5", "vk", "vs", "vz", "adj-i"]);

/** The reading cut into morae: a small kana belongs to the one before it. */
export function morae(reading) {
  const out = [];
  for (const c of reading) {
    if (SMALL.includes(c) && out.length) out[out.length - 1] += c;
    else out.push(c);
  }
  return out;
}

/** Where the pitch drops: the mora after which it falls, or 0 for none. */
export function position(value) {
  const pattern = (value.pattern ?? "").toUpperCase();
  if (!pattern) return value.position ?? 0;
  for (let i = 1; i < pattern.length; i++) if (pattern[i - 1] === "H" && pattern[i] === "L") return i;
  return 0;
}

export function kind(position, moraCount, rules) {
  if (position === 0) return "heiban";
  if ((rules ?? "").split(" ").some((rule) => VERB_OR_ADJECTIVE.has(rule))) return "kifuku";
  if (position === 1) return "atamadaka";
  if (position >= moraCount) return "odaka";
  return "nakadaka";
}

/** Each different accent the pitch dictionaries give, in their order: {position, kind, label, morae, high}. */
export function accents(entry) {
  const cut = morae(entry.reading || entry.expression);
  if (!cut.length) return [];
  const positions = entry.pitches.flatMap((p) => p.values).map(position).filter((p) => p >= 0 && p <= cut.length);
  return [...new Set(positions)].map((p) => {
    // Heiban rises after the first mora and stays up; otherwise the first mora is low unless the drop comes after it.
    const high = Array.from({ length: cut.length + 1 }, (_, i) => (p === 0 ? i > 0 : p === 1 ? i === 0 : i >= 1 && i < p));
    const k = kind(p, cut.length, entry.rules);
    return { position: p, kind: k, label: KINDS[k], morae: cut, high };
  });
}

/** Yomitan's average: the floored harmonic mean of each dictionary's first value. */
export function frequencyRank(entry) {
  const firsts = entry.frequencies.map((f) => f.values[0]?.value).filter((v) => v > 0);
  if (!firsts.length) return null;
  return Math.floor(firsts.length / firsts.reduce((sum, v) => sum + 1 / v, 0));
}

/** A frequency dictionary's name without its version and date: `JPDB_v2.2_Frequency_Kana` reads `JPDB`. */
export function shortName(dictionary) {
  const words = dictionary.split(/[ _]/).filter(Boolean);
  const kept = [];
  for (const w of words) {
    if (/^[0-9([]/.test(w) || /^[vV]\d/.test(w)) break;
    kept.push(w);
  }
  return kept.join(" ") || dictionary;
}
