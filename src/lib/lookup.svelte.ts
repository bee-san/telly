// Looking words up on the phone, where Hachidori can't run (Android's WebView
// loads no extensions): Hoshidicts, the engine inside Hachidori, with your
// dictionaries as the PC has them (phone.rs gets them). Each dictionary is
// installed once and kept in the page's own storage (IndexedDB).
import { call, mediaUrl } from "$lib/api";

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
  term: { expression: string; reading: string; glossaries: Glossary[] };
}

interface Engine {
  FS: any;
  IDBFS: any;
  ccall: (name: string, returns: string, types: string[], args: unknown[]) => any;
}

const ROOT = "/dicts";
const INSTALLED = `${ROOT}/telly-installed.json`;
/** The kinds of what a dictionary holds, by hdw_add_dict's `kind`. */
const KINDS = ["term", "freq", "pitch", "kanji"];
const OPTIONS = JSON.stringify({ frequencyDictionary: "", frequencyOrder: "auto", primaryReading: "", metadataOnlyTerms: false });

let engine: Engine | null = null;
let starting: Promise<Engine> | null = null;
/** What's installed: each file's revision on the PC, and the folder the engine keeps it in. */
let installed: Record<string, { revision: string; path: string }> = {};
/** Dictionaries being installed, and the one that failed last. */
export const progress = $state({ installing: "", failed: "", count: 0 });

function lastError(e: Engine): string {
  return e.ccall("hdw_last_error", "string", [], []) || "the dictionary engine failed";
}

function sync(e: Engine, populate: boolean): Promise<void> {
  return new Promise((resolve, reject) => e.FS.syncfs(populate, (err: unknown) => (err ? reject(err) : resolve())));
}

function loadAll(e: Engine) {
  const paths = Object.values(installed).map((d) => d.path);
  for (const path of paths) {
    KINDS.forEach((_, kind) => e.ccall("hdw_add_dict", "number", ["string", "number", "number", "number"], [path, kind, 1, 0]));
  }
  e.ccall("hdw_set_dict_order", "number", ["string"], [JSON.stringify(paths)]);
  progress.count = paths.length;
}

/** The engine, started once: its dictionaries read back from the page's storage. */
export function start(): Promise<Engine> {
  starting ??= (async () => {
    const url = "/hoshi/hoshidicts.mjs";
    const { default: create } = await import(/* @vite-ignore */ url);
    const e: Engine = await create({ locateFile: (file: string) => `/hoshi/${file}` });
    e.FS.mkdir(ROOT);
    e.FS.mount(e.IDBFS, {}, ROOT);
    await sync(e, true);
    if (e.ccall("hdw_init_storage", "number", ["number"], [0]) !== 1) throw new Error(lastError(e));
    try {
      installed = JSON.parse(e.FS.readFile(INSTALLED, { encoding: "utf8" }));
    } catch {
      installed = {};
    }
    loadAll(e);
    engine = e;
    return e;
  })();
  return starting;
}

function removeTree(e: Engine, path: string) {
  if (!e.FS.analyzePath(path).exists) return;
  for (const name of e.FS.readdir(path)) {
    if (name === "." || name === "..") continue;
    const child = `${path}/${name}`;
    if (e.FS.isDir(e.FS.stat(child).mode)) removeTree(e, child);
    else e.FS.unlink(child);
  }
  e.FS.rmdir(path);
}

/** Copies a dictionary into the engine's /import.zip a piece at a time, as the backend gives it. */
async function fetchDictionary(e: Engine, url: string): Promise<void> {
  const file = e.FS.open("/import.zip", "w");
  try {
    for (let from = 0; ; ) {
      const answer = await fetch(`${mediaUrl(url)}?from=${from}`);
      if (!answer.ok) throw new Error(await answer.text());
      const piece = new Uint8Array(await answer.arrayBuffer());
      if (!piece.length) break;
      e.FS.write(file, piece, 0, piece.length);
      from += piece.length;
    }
  } finally {
    e.FS.close(file);
  }
}

let installs: Promise<void> = Promise.resolve();

/** Installs the dictionaries the phone has that the engine hasn't (or has an
 *  older one of), and lets go of those the phone no longer has. One at a time:
 *  asked again while it's installing, it looks again once that's done. */
export function installDictionaries(): Promise<void> {
  installs = installs.catch(() => {}).then(install);
  return installs;
}

async function install(): Promise<void> {
  const e = await start();
  const have = await call<{ name: string; revision: string; url: string }[]>("phoneDictionaries");
  let changed = false;
  for (const name of Object.keys(installed)) {
    if (have.some((d) => d.name === name)) continue;
    e.ccall("hdw_remove_dict", "number", ["string"], [installed[name].path]);
    removeTree(e, installed[name].path);
    delete installed[name];
    changed = true;
  }
  for (const d of have) {
    if (installed[d.name]?.revision === d.revision) continue;
    progress.installing = d.name;
    try {
      await fetchDictionary(e, d.url);
      const report = JSON.parse(e.ccall("hdw_import", "string", ["string", "string", "number"], ["/import.zip", ROOT, 1]));
      e.FS.unlink("/import.zip");
      if (!report.success) throw new Error(report.error || lastError(e));
      if (installed[d.name] && installed[d.name].path !== `${ROOT}/${report.title}`) removeTree(e, installed[d.name].path);
      installed[d.name] = { revision: d.revision, path: `${ROOT}/${report.title}` };
      changed = true;
    } catch (err) {
      progress.failed = `${d.name}: ${err instanceof Error ? err.message : String(err)}`;
    }
  }
  progress.installing = "";
  if (!changed) return;
  e.FS.writeFile(INSTALLED, JSON.stringify(installed));
  await sync(e, false);
  e.ccall("hdw_reset", "number", [], []);
  if (e.ccall("hdw_init_storage", "number", ["number"], [0]) !== 1) throw new Error(lastError(e));
  loadAll(e);
}

/** The words `text` starts with, the longest first. */
export async function lookUp(text: string, max = 6): Promise<Found[]> {
  const e = engine ?? (await start());
  if (!text.trim()) return [];
  const json = e.ccall("hdw_lookup", "string", ["string", "number", "number", "string"], [text, max, 16, OPTIONS]);
  if (!json) throw new Error(lastError(e));
  return JSON.parse(json).results ?? [];
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
