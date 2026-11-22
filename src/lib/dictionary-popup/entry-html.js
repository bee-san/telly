// @ts-nocheck: plain JavaScript; src/index.d.ts says what it exports.
// A dictionary entry as HTML, the way Yomitan lays structured content out, so
// each dictionary's own stylesheet applies: every element carries the class
// `gloss-sc-<tag>` and its `data` as `data-sc-*` attributes, and each
// dictionary's block sits in an element with `data-dictionary`, which its
// rules are scoped to. A port of LFG's EntryHtml.kt.
//
// Dictionary content never becomes script: `glossary` keeps only TAGS, and of
// their attributes only data, style (STYLES), lang, title and a table's spans.
// SPDX-License-Identifier: GPL-3.0-or-later
import { accents, shortName } from "./pronunciation.js";

const TAGS = new Set(["br", "ruby", "rt", "rp", "table", "thead", "tbody", "tfoot", "tr", "td", "th", "span", "div", "ol", "ul", "li", "details", "summary", "a", "img"]);

// Yomitan's own list: what a dictionary may set in a style object.
const STYLES = new Set([
  "fontStyle", "fontWeight", "fontSize", "color", "background", "backgroundColor", "textDecorationLine",
  "textDecorationStyle", "textDecorationColor", "borderColor", "borderStyle", "borderRadius", "borderWidth",
  "clipPath", "verticalAlign", "textAlign", "textEmphasis", "textShadow", "margin", "marginTop", "marginLeft",
  "marginRight", "marginBottom", "padding", "paddingTop", "paddingLeft", "paddingRight", "paddingBottom",
  "wordBreak", "whiteSpace", "cursor", "listStyleType",
]);

const UNITLESS = new Set(["fontWeight"]);

const PENCIL = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 20h4L19 9l-4-4L4 16v4z"/><path d="M13.5 6.5l4 4"/></svg>';

const SPEAKER = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6h4l5 4V5L8 9H4zm12.5 3a4.5 4.5 0 0 0-2.5-4v8a4.5 4.5 0 0 0 2.5-4zM14 3.2v2.1a7 7 0 0 1 0 13.4v2.1a9 9 0 0 0 0-17.6z"/></svg>';

// Stroke icons for the card button's four states; the styles show the one for its `data-state`.
const CARD_ICONS =
  '<svg class="lfg-i-add" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14M5 12h14"/></svg>' +
  '<svg class="lfg-i-adding" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 4a8 8 0 1 0 8 8"/></svg>' +
  '<svg class="lfg-i-in-anki" viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>' +
  '<svg class="lfg-i-waiting" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8"/><path d="M12 8v4l3 2"/></svg>';

/** What each state of the card button is called. */
export const CARD_STATES = { add: "Add card", adding: "Adding…", "in-anki": "Open in Anki", waiting: "Waiting for AnkiDroid" };

export function escape(text) {
  return String(text).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
}

/** A primitive of a dictionary's JSON as text, as LFG reads it; "" for anything else. */
function text(object, key) {
  const value = object?.[key];
  return typeof value === "string" || typeof value === "number" || typeof value === "boolean" ? String(value) : "";
}

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const isPrimitive = (value) => typeof value === "string" || typeof value === "number" || typeof value === "boolean";

function kebab(name) {
  return name.replace(/\p{Lu}/gu, (c) => "-" + c.toLowerCase());
}

/**
 * One lookup result as LFG's entry: the word's fields flat, each frequency's
 * and pitch's values under `values`. It takes what Hoshidicts gives (through
 * its C interface, its wasm build or LFG's JNI), under either name of a field.
 */
export function toEntry(result) {
  const term = result.term ?? result;
  const name = (of) => of.dictionary ?? of.dictName ?? of.dict_name ?? "";
  return {
    matched: result.matched ?? "",
    deinflected: result.deinflected ?? "",
    trace: (result.trace ?? result.process ?? []).map((step) => ({ name: step.name ?? "", description: step.description ?? "" })),
    expression: term.expression ?? result.matched ?? "",
    reading: term.reading ?? "",
    rules: term.rules ?? "",
    glossaries: (term.glossaries ?? []).map((g) => ({ dictionary: name(g), glossary: g.glossary ?? "", definitionTags: g.definitionTags ?? "", termTags: g.termTags ?? "" })),
    frequencies: (term.frequencies ?? []).map((f) => ({
      dictionary: name(f),
      values: (f.values ?? f.frequencies ?? []).map((v) => ({ value: Number(v.value) || 0, display: v.display ?? v.displayValue ?? "" })),
    })),
    pitches: (term.pitches ?? []).map((p) => ({
      dictionary: name(p),
      values: p.values ?? p.pitches ?? (p.pitchPositions ?? []).map((position) => ({ position })),
    })),
  };
}

/** `見た → 見る · past`, or empty when the word was not inflected. */
export function path(entry) {
  if (!entry.trace.length || entry.matched === entry.expression) return "";
  return `${entry.matched} → ${entry.expression} · ` + entry.trace.map((step) => step.name).join(" · ");
}

/**
 * The entries of a lookup, one under the other. Each carries its own card
 * button and, where `audio` and `notes` say, an audio and a note button; the
 * buttons carry `data-action` and `data-index`.
 */
export function listBody(entries, options = {}) {
  return entries.map((entry, i) => `<section class="lfg-entry" id="e${i}">${body(entry, { ...options, index: i })}</section>`).join("");
}

/**
 * The entry alone: headword with its reading as furigana, the pitch graph, the
 * buttons, frequencies and accents, the inflection path, then each
 * dictionary's glossaries. `index` is the entry's place in a list (without it
 * there's no card or note button); `audio(entry)` and `notes` say whether it
 * has an audio and a note button; `card: false` leaves the card button out;
 * `mediaUrl(dictionary, path)` gives a dictionary's picture a URL.
 */
export function body(entry, { index = null, audio = null, notes = false, card = true, mediaUrl = null } = {}) {
  const out = [];
  const pitch = accents(entry);
  out.push(`<div class="lfg-head"><span class="lfg-term" lang="ja">${furigana(entry.expression, entry.reading)}</span>`);
  if (pitch.length) out.push(graph(pitch[0]));
  out.push('<span class="lfg-actions">');
  const at = index === null ? "" : ` data-index="${index}"`;
  if (audio && audio(entry)) out.push(`<a class="lfg-button lfg-audio" role="button" tabindex="0" data-action="audio"${at} aria-label="Listen" title="Listen">${SPEAKER}</a>`);
  if (index !== null && notes) out.push(`<a class="lfg-button lfg-note" role="button" tabindex="0" data-action="note"${at} aria-label="Note" title="Note">${PENCIL}</a>`);
  if (index !== null && card) {
    out.push(`<a class="lfg-button lfg-card" role="button" tabindex="0" id="c${index}" data-action="card"${at} data-state="add" aria-label="Add card" title="Add card">${CARD_ICONS}</a>`);
  }
  out.push("</span></div>");
  if (entry.frequencies.length || pitch.length) {
    out.push('<div class="lfg-meta">');
    for (const a of pitch) out.push(`<span class="lfg-pitch-tag" data-pitch="${a.kind}">${a.label} <b>${a.position}</b></span>`);
    for (const f of entry.frequencies) {
      const values = [...new Set(f.values.map((v) => v.display || String(v.value)))];
      if (!values.length) continue;
      out.push(`<span class="lfg-freq"><span class="lfg-freq-name">${escape(shortName(f.dictionary))}</span><span class="lfg-freq-value">${escape(values.join(" · "))}</span></span>`);
    }
    out.push("</div>");
  }
  const inflected = path(entry);
  if (inflected) out.push(`<div class="lfg-path">${escape(inflected)}</div>`);
  const byDictionary = new Map();
  for (const g of entry.glossaries) byDictionary.set(g.dictionary, [...(byDictionary.get(g.dictionary) ?? []), g]);
  for (const [dictionary, glossaries] of byDictionary) {
    out.push(`<div class="lfg-dict" data-dictionary="${escape(dictionary)}"><div class="lfg-source">${escape(dictionary)}</div><ol class="lfg-senses">`);
    for (const g of glossaries) {
      out.push("<li>");
      const tags = g.definitionTags.split(" ").filter((tag) => tag.trim() && !/^[+-]?\d+$/.test(tag));
      if (tags.length) out.push(`<span class="lfg-tags">${tags.map((tag) => `<span class="lfg-tag">${escape(tag)}</span>`).join("")}</span>`);
      out.push(glossary(g.glossary, dictionary, mediaUrl), "</li>");
    }
    out.push("</ol></div>");
  }
  return out.join("");
}

/** One glossary (a JSON array of definitions) as HTML. */
export function glossary(json, dictionary, mediaUrl = null) {
  let root;
  try {
    root = JSON.parse(json);
  } catch {
    return escape(json);
  }
  const items = Array.isArray(root) ? root : [root];
  const plain = [];
  const rich = [];
  for (const item of items) {
    if (isPrimitive(item)) plain.push(String(item));
    else if (isObject(item) && text(item, "type") === "text") plain.push(text(item, "text"));
    else if (isObject(item) && text(item, "type") === "image") image(item, dictionary, mediaUrl, rich);
    else if (isObject(item) && text(item, "type") === "structured-content") {
      if (item.content !== undefined) node(item.content, dictionary, mediaUrl, rich);
    } else node(item, dictionary, mediaUrl, rich);
  }
  let out = "";
  if (plain.length === 1) out += `<span class="gloss-text">${escape(plain[0])}</span>`;
  if (plain.length > 1) out += `<ul class="gloss-list">${plain.map((line) => `<li>${escape(line)}</li>`).join("")}</ul>`;
  return out + rich.join("");
}

function node(content, dictionary, mediaUrl, out) {
  if (content === null || content === undefined) return;
  if (isPrimitive(content)) {
    out.push(escape(content));
  } else if (Array.isArray(content)) {
    for (const child of content) node(child, dictionary, mediaUrl, out);
  } else {
    const tag = text(content, "tag");
    if (!TAGS.has(tag)) {
      // Unknown elements keep their text and lose their markup.
      node(content.content, dictionary, mediaUrl, out);
      return;
    }
    if (tag === "br") out.push("<br>");
    else if (tag === "img") image(content, dictionary, mediaUrl, out);
    else {
      out.push(`<${tag} class="gloss-sc-${tag}"`);
      attributes(content, out);
      // Links inside dictionaries point at other entries; they are not followed here.
      if (tag === "a") out.push(` data-href="${escape(text(content, "href"))}"`);
      out.push(">");
      node(content.content, dictionary, mediaUrl, out);
      out.push(`</${tag}>`);
    }
  }
}

function attributes(element, out) {
  if (isObject(element.data)) {
    for (const [key, value] of Object.entries(element.data)) {
      const name = [...key].filter((c) => /[\p{L}\p{N}_-]/u.test(c)).join("");
      if (name && isPrimitive(value)) out.push(` data-sc-${kebab(name)}="${escape(value)}"`);
    }
  }
  if (isObject(element.style)) {
    const css = [];
    for (const [key, value] of Object.entries(element.style)) {
      if (!STYLES.has(key)) continue;
      let of;
      if (typeof value === "string") of = value;
      else if (isPrimitive(value)) of = String(value) + (UNITLESS.has(key) ? "" : "em");
      else if (Array.isArray(value)) of = value.filter(isPrimitive).join(" ");
      else continue;
      // A value may not close the declaration or open something else.
      if (!/[;{}<>"]/.test(of)) css.push(`${kebab(key)}:${of}`);
    }
    if (css.length) out.push(` style="${css.join(";")}"`);
  }
  for (const name of ["lang", "title"]) {
    const value = text(element, name);
    if (value) out.push(` ${name}="${escape(value)}"`);
  }
  for (const [key, attribute] of [["colSpan", "colspan"], ["rowSpan", "rowspan"]]) {
    const value = text(element, key);
    if (/^[+-]?\d+$/.test(value)) out.push(` ${attribute}="${parseInt(value, 10)}"`);
  }
  if (text(element, "tag") === "details" && text(element, "open") === "true") out.push(" open");
}

function image(element, dictionary, mediaUrl, out) {
  const file = text(element, "path");
  const src = file && mediaUrl ? mediaUrl(dictionary, file) : "";
  if (!src) return;
  const units = text(element, "sizeUnits") === "em" ? "em" : "px";
  const number = (key) => (text(element, key).trim() !== "" && Number.isFinite(Number(text(element, key))) ? Number(text(element, key)) : null);
  const align = text(element, "verticalAlign");
  const size = [];
  if (number("width") !== null) size.push(`width:${number("width")}${units}`);
  if (number("height") !== null) size.push(`height:${number("height")}${units}`);
  if (align && /^[\p{L}-]+$/u.test(align)) size.push(`vertical-align:${align}`);
  out.push(`<img class="gloss-image" src="${escape(src)}"`);
  if (size.length) out.push(` style="${size.join(";")}"`);
  out.push(` alt="${escape(text(element, "alt") || text(element, "title"))}">`);
}

const isKana = (c) => (c >= "ぁ" && c <= "ゖ") || (c >= "ァ" && c <= "ヶ") || c === "ー";
const hiragana = (c) => (c >= "ァ" && c <= "ヶ" ? String.fromCharCode(c.charCodeAt(0) - 0x60) : c);
const same = (a, b) => a.length === b.length && [...Array(a.length).keys()].every((i) => hiragana(a[i]) === hiragana(b[i]));

/**
 * The headword with its reading as furigana. Kana the word and its reading
 * share at either end stay bare, so 新しい / あたらしい reads 新[あたら]しい; a
 * word whose kana do not line up carries the whole reading.
 */
export function furigana(expression, reading) {
  if (!reading || same(expression, reading)) return escape(expression);
  let head = 0;
  while (head < expression.length && head < reading.length && isKana(expression[head]) && hiragana(expression[head]) === hiragana(reading[head])) head++;
  let tail = 0;
  while (
    tail < expression.length - head &&
    tail < reading.length - head &&
    isKana(expression[expression.length - 1 - tail]) &&
    hiragana(expression[expression.length - 1 - tail]) === hiragana(reading[reading.length - 1 - tail])
  ) {
    tail++;
  }
  const word = expression.substring(head, expression.length - tail);
  const over = reading.substring(head, reading.length - tail);
  if (!word || !over) return `<ruby>${escape(expression)}<rt>${escape(reading)}</rt></ruby>`;
  return escape(expression.substring(0, head)) + `<ruby>${escape(word)}<rt>${escape(over)}</rt></ruby>` + escape(expression.substring(expression.length - tail));
}

/**
 * An accent as a graph, after Yomitan's pitch-accent graph: a dot for each
 * mora, high or low, joined by a line, and a hollow dot for the particle after
 * the word.
 */
export function graph(accent) {
  const step = 14;
  const points = accent.high.map((high, i) => [6 + step * i, high ? 5 : 19]);
  const width = 6 + step * (points.length - 1) + 6;
  return (
    `<svg class="lfg-graph" data-pitch="${accent.kind}" role="img" aria-label="${accent.label} ${accent.position}" width="${width}" height="24" viewBox="0 0 ${width} 24">` +
    `<polyline points="${points.map(([x, y]) => `${x},${y}`).join(" ")}"/>` +
    points.map(([x, y], i) => `<circle cx="${x}" cy="${y}" r="3.5"${i === points.length - 1 ? ' class="lfg-particle"' : ""}/>`).join("") +
    "</svg>"
  );
}

function matching(css, open) {
  let depth = 0;
  for (let i = open; i < css.length; i++) {
    if (css[i] === "{") depth++;
    else if (css[i] === "}" && --depth === 0) return i;
  }
  return -1;
}

/**
 * A dictionary's stylesheet, each rule limited to that dictionary's block.
 * Rules inside `@media` and the like are scoped too; other at-rules
 * (`@font-face`, `@import`) are dropped.
 */
export function scope(css, dictionary) {
  const prefix = `[data-dictionary="${dictionary.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"]`;
  const source = css.replace(/\/\*[\s\S]*?\*\//g, "").replaceAll("</", "<\\/");
  let out = "";
  let i = 0;
  while (i < source.length) {
    const open = source.indexOf("{", i);
    if (open < 0) break;
    const selector = source.substring(i, open).trim();
    const close = matching(source, open);
    if (close < 0) break;
    const block = source.substring(open + 1, close);
    if (selector.startsWith("@media") || selector.startsWith("@supports")) out += `${selector}{${scope(block, dictionary)}}`;
    else if (selector.startsWith("@")) {
      // Dropped.
    } else if (selector) out += selector.split(",").map((one) => `${prefix} ${one.trim()}`).join(",") + `{${block}}`;
    i = close + 1;
  }
  return out;
}
