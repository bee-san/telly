// Looking words up on the phone, where Hachidori can't run (Android's WebView
// loads no extensions): the phone's backend has Hoshidicts, the engine inside
// Hachidori, and your dictionaries as the PC has them (lookups.rs). This asks
// it what a line's words mean, and lays out what it says.
import { call } from "$lib/api";

/** One glossary of a word: which dictionary, and what it says (Yomitan's structured content, as JSON). */
export interface Glossary {
  dictionary: string;
  glossary: string;
  definitionTags: string;
  termTags: string;
}

export interface Found {
  /** The text it was found as, from the start of what was looked up. */
  matched: string;
  deinflected: string;
  /** The inflections undone to get from `matched` to the word. */
  process: { name: string; description: string }[];
  term: {
    expression: string;
    reading: string;
    /** The word's parts of speech ("v1 vt"). */
    rules: string;
    glossaries: Glossary[];
    /** How common it is, by each frequency dictionary. */
    frequencies: { dictionary: string; frequencies: { value: number; displayValue: string }[] }[];
    /** Its pitch accent, by each pitch dictionary. */
    pitches: { dictionary: string; pitches: { position: number; pattern: string; nasal: number[]; devoice: number[] }[] }[];
  };
}

/** One dictionary's own CSS for its entries. */
export interface DictionaryStyle {
  dictionary: string;
  styles: string;
}

/** How many dictionaries the phone has open, the one being installed, and what couldn't be installed. */
export const progress = $state({ installing: "", failed: "", count: 0 });
/** Your dictionaries' own styles, as the last lookup had them. */
export const dictionaryStyles = $state<{ list: DictionaryStyle[] }>({ list: [] });

/** The words `text` starts with, the longest first. */
export async function lookUp(text: string, max = 6): Promise<Found[]> {
  const said = await call<{ found: Found[]; styles: DictionaryStyle[]; count: number; installing: string; failed: string }>("phoneLookUp", { text, max });
  if (said.styles.length) dictionaryStyles.list = said.styles;
  progress.installing = said.installing;
  progress.failed = said.failed;
  progress.count = said.count;
  return said.found;
}

function esc(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c] as string);
}

/** The tags structured content may use, as themselves; anything else is a span. */
const TAGS = new Set(["ul", "ol", "li", "div", "span", "ruby", "rt", "rp", "table", "thead", "tbody", "tr", "td", "th", "br", "b", "i", "em", "strong", "sub", "sup", "details", "summary"]);

/** Yomitan's structured content as HTML (images left out: the phone has no dictionary media yet). */
function structured(node: any): string {
  if (node === null || node === undefined) return "";
  if (typeof node === "string") return esc(node);
  if (Array.isArray(node)) return node.map(structured).join("");
  if (typeof node !== "object") return "";
  if (node.type === "structured-content") return structured(node.content);
  if (node.type === "text") return esc(node.text ?? "");
  if (node.tag === "img" || node.type === "image") return "";
  const tag = TAGS.has(node.tag) ? node.tag : "span";
  const data = node.data?.content ? ` data-sc="${esc(String(node.data.content))}"` : "";
  if (tag === "br") return "<br>";
  if (node.tag === "a") return `<span class="sc-link">${structured(node.content)}</span>`;
  return `<${tag}${data}>${structured(node.content)}</${tag}>`;
}

/** A glossary as HTML: plain definitions as a list, structured content as it's laid out. */
export function glossaryHtml(g: Glossary): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(g.glossary);
  } catch {
    return esc(g.glossary);
  }
  const items = Array.isArray(parsed) ? parsed : [parsed];
  if (items.every((i) => typeof i === "string")) return `<ul>${items.map((i) => `<li>${esc(i as string)}</li>`).join("")}</ul>`;
  return items.map(structured).join("");
}

/** A glossary as plain text, for a card's definition. */
export function glossaryText(g: Glossary): string {
  const div = document.createElement("div");
  div.innerHTML = glossaryHtml(g);
  div.querySelectorAll("[data-sc=examples], [data-sc=example-sentence]").forEach((n) => n.remove());
  return (div.innerText || div.textContent || "").replace(/\n{2,}/g, "\n").trim();
}
