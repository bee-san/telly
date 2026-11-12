// Telly's Hachidori engine host.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Telly keeps this page open in a hidden window at
// chrome-extension://<id>/telly/host.html?link=1 (scripts/prepare-hachidori.mjs
// copies it into Telly's copy of Hachidori).
//
// 1. The engine. WebView2 has no chrome.offscreen, so Hachidori's service worker
//    cannot open offscreen.html, its dictionary engine, by itself. It expects
//    the host to keep that page open instead (background-core.js ensureOffscreen),
//    so this page holds it in a frame, as Hachidori's Firefox background page
//    does. The service worker trusts the frame because a message's sender.url
//    is the frame's own URL.
// 2. The link. With ?link=1, Hachidori here is linked to the Hachidori in your
//    browser (Settings → Sharing), through Hachidori Relay in Anki (or Telly's own relay), so the
//    player uses the dictionaries, settings and Anki setup you already have.
//    If the relay is not running yet, it tries again every poll. A link you
//    set up yourself to another address in Hachidori's settings is left alone.
//    ?link=0 unlinks it, once, when the page opens.
// 3. Character names. Telly makes a dictionary of the casts of the shows you're
//    watching (src-tauri/src/names) and serves it at /names on its media
//    protocol. Each poll this page asks whether there's a copy Hachidori hasn't
//    had, and sends it through Hachidori's upload requests, as SubMiner sends
//    its own: a linked Hachidori passes it to the Hachidori in your browser,
//    an unlinked one imports it itself. It replaces the last copy by title.
// 4. Unlinked, it's your Hachidori in Telly, ready to mine
//    (src-tauri/src/own_hachidori.rs): it installs every dictionary in your
//    dictionaries folder, one at a time; it sets its Anki template to the
//    note type and deck of the show being watched, with Hachidori's own Kiku
//    mapping; pronunciation defaults to Yomitan Audio Fast, with the Local
//    Audio Server Anki add-on as a fallback; and it answers premining's
//    questions, which Hachidori Relay would otherwise ask a sharing
//    Hachidori: its lookups itself (termEntries), the rest with Hachidori's
//    own API code (api-host.js).
//
// Telly watches this page's title for its state:
// "telly-hachidori:" + JSON {state, detail, host}, where state is one of
// starting, linked, connecting, standalone, link-failed or error.

import { extensionApi as chrome } from "../browser-api.js";
import { chromeOffscreenSupported, ensureChromeOffscreen } from "../chrome-offscreen.js";
import { uploadDictionary } from "../linked-import.js";
import "../reader-options.js";
import { createApiHost } from "../api-host.js";
import { ankiSetupTemplates } from "../anki-setup.js";
import { createLocalAudioSource } from "../local-audio-source.js";

const SHARING = "hachidori-sharing";
const PREFIX = "telly-hachidori:";
const ENGINE = "offscreen.html";
const POLL_MS = 15_000;
// Premining's lookups in flight at once.
const LOOKUPS_AT_ONCE = 16;
// The Anki setup follows the show being watched, so it's checked more often.
const ANKI_POLL_MS = 2_000;
// Hachidori's storage keys and message targets (background-core.js, offscreen.js).
const OPTIONS_KEY = "options";
const DICTIONARY_STATE_KEY = "dictionaryState";
const ENGINE_TARGET = "hoshidicts-offscreen";
const RENDER_TARGET = "hachidori-anki-render";
const WORKER_TARGET = "hoshidicts-worker";
const IMPORT_TARGET = "hachidori-linked-import";
// Telly's own files: the media protocol, as WebView2 names it.
const TELLY = "http://media.localhost";

const wantLink = new URLSearchParams(location.search).get("link") !== "0";

let lastReport = "";
function report(state, detail = "", host = "") {
  const title = PREFIX + JSON.stringify({ state, detail, host });
  if (title === lastReport) return;
  lastReport = title;
  document.title = title;
}

function errorText(error) {
  return String(error?.message ?? error ?? "unknown error");
}

async function sharing(type, extra = {}) {
  const reply = await chrome.runtime.sendMessage({ target: SHARING, type, ...extra });
  if (!reply?.ok) throw new Error(reply?.error ? String(reply.error) : `Hachidori did not answer ${type}`);
  return reply;
}

/** Starts the engine: the extension's own offscreen document if it can make one, else a frame here. */
async function startEngine() {
  if (chromeOffscreenSupported()) {
    await ensureChromeOffscreen(ENGINE);
    return;
  }
  const frame = document.createElement("iframe");
  frame.src = chrome.runtime.getURL(ENGINE);
  frame.title = "Hachidori engine";
  const loaded = new Promise((resolve, reject) => {
    frame.addEventListener("load", resolve, { once: true });
    frame.addEventListener("error", () => reject(new Error("the engine page did not load")), { once: true });
  });
  document.body.append(frame);
  await loaded;
}

function describe(client) {
  if (!client?.linked) return ["standalone", "", ""];
  const host = client.host?.name ?? "";
  if (client.connected) return ["linked", client.display ?? client.address ?? "", host];
  return ["connecting", client.error ? String(client.error) : "", host];
}

async function status() {
  const { sharing: state } = await sharing("hd_sharing_status");
  return state?.client ?? null;
}

async function reconcileLink() {
  const client = await status();
  if (wantLink && !client?.linked) {
    // An empty address is this computer, on the relay port Hachidori uses.
    await sharing("hd_sharing_client_link", { address: "" });
  } else if (!wantLink && client?.linked) {
    await sharing("hd_sharing_client_unlink");
  }
}

// The last link attempt's error. While it is set, every poll tries again, so
// opening Anki (and with it Hachidori Relay) later is enough to link.
let linkError = "";

async function step(first) {
  if (first || linkError) {
    try {
      await reconcileLink();
      linkError = "";
    } catch (error) {
      linkError = errorText(error);
    }
  }
  try {
    const client = await status();
    if (linkError && !client?.linked) report("link-failed", linkError);
    else report(...describe(client));
  } catch (error) {
    report("error", errorText(error));
  }
}

/** Waits for this Hachidori's engine to finish loading (SubMiner's waitForEngineReady). */
async function engineReady() {
  const deadline = Date.now() + 120_000;
  for (;;) {
    const reply = await chrome.runtime.sendMessage({ target: "hoshidicts-offscreen", type: "hd_status", requestId: crypto.randomUUID() }).catch(() => null);
    if (reply?.ready === true && reply.loading !== true) return;
    if (Date.now() >= deadline) throw new Error("Hachidori's dictionaries are still loading");
    await new Promise(resolve => setTimeout(resolve, 500));
  }
}

// One upload at a time: the character dictionary or one of yours.
let uploading = false;

const importSend = (type, fields) => chrome.runtime.sendMessage({ target: IMPORT_TARGET, type, requestId: crypto.randomUUID(), ...fields });

/** Sends Telly's character dictionary to Hachidori when there's a copy it hasn't had. */
async function syncNames() {
  if (uploading) return;
  let state;
  try {
    state = await (await fetch(`${TELLY}/names/state`)).json();
  } catch {
    return;
  }
  if (!state?.due) return;
  const client = await status().catch(() => null);
  // Linked, but the browser isn't there yet: next poll.
  if (client?.linked && !client.connected) return;
  uploading = true;
  const tell = (fields) => fetch(`${TELLY}/names/imported?${new URLSearchParams({ rev: state.revision, ...fields })}`).catch(() => {});
  try {
    if (!client?.linked) await engineReady();
    const archive = await fetch(`${TELLY}${state.url}`);
    if (!archive.ok) throw new Error("Telly's character dictionary wasn't there");
    const reply = await uploadDictionary({
      blob: await archive.blob(),
      fileName: "telly-characters.zip",
      replace: true,
      send: importSend,
    });
    if (reply?.report?.success !== true) throw new Error(reply?.report?.error || reply?.error || "Hachidori didn't take the dictionary");
    await tell({ ok: "1", to: client?.linked ? (client.host?.name || "your browser") : "telly" });
  } catch (error) {
    await tell({ ok: "0", error: errorText(error) });
  } finally {
    uploading = false;
  }
}

/** Installs your dictionaries, one at a time, while this Hachidori isn't linked. */
async function syncDictionaries() {
  if (uploading) return;
  const client = await status().catch(() => null);
  if (client?.linked) return;
  uploading = true;
  try {
    await engineReady();
    for (;;) {
      const next = await fetch(`${TELLY}/hd/dictionaries/next`).then((r) => r.json()).catch(() => null);
      if (!next?.file) return;
      const tell = (fields) => fetch(`${TELLY}/hd/dictionaries/done?${new URLSearchParams({ file: next.file, rev: next.revision, ...fields })}`).catch(() => {});
      try {
        const archive = await fetch(`${TELLY}${next.url}`);
        if (!archive.ok) throw new Error(`${next.file} wasn't there`);
        const reply = await uploadDictionary({ blob: await archive.blob(), fileName: next.file, replace: true, send: importSend });
        if (reply?.report?.success !== true) throw new Error(reply?.report?.error || reply?.error || "Hachidori didn't take it");
        await tell({ ok: "1" });
      } catch (error) {
        await tell({ ok: "0", error: errorText(error) });
      }
    }
  } catch (error) {
    report("error", `Couldn't install your dictionaries: ${errorText(error)}`);
  } finally {
    uploading = false;
  }
}

const readerOptions = () => globalThis.HDReaderOptions;

async function storedOptions() {
  return (await chrome.storage.local.get(OPTIONS_KEY))[OPTIONS_KEY];
}

/** This Hachidori's options, as its background reads them for Japanese. */
function japaneseOptions(stored) {
  const o = readerOptions();
  return o.hasProfiles(stored) ? o.projectOptions(stored, o.JAPANESE_PROFILE_ID) : o.normaliseOptions(stored);
}

// The setup last made, so it's made once per show.
let ankiMade = "";

// Set once, so later changes in Hachidori's Audio Settings stick.
const LOCAL_AUDIO_SET_KEY = "tellyLocalAudioSet";
const LOCAL_AUDIO_URLS = [
  "http://127.0.0.1:5052/?term={term}&reading={reading}", // Yomitan Audio Fast's standalone Rust server.
  "http://127.0.0.1:5050/?term={term}&reading={reading}", // Its drop-in Anki add-on (also the original server).
];

/** Defaults pronunciation to local recordings, even if Anki or the server starts later. */
async function enableLocalAudio() {
  try {
    if ((await chrome.storage.local.get(LOCAL_AUDIO_SET_KEY))[LOCAL_AUDIO_SET_KEY]) return;
    const client = await status();
    if (client?.linked) return;
    for (let attempt = 0; attempt < 3; attempt += 1) {
      const stored = await storedOptions();
      const sources = japaneseOptions(stored).audioSources;
      const local = LOCAL_AUDIO_URLS.map(url => sources.find(source => source.type === "custom-json" && source.url === url)
        ?? createLocalAudioSource(crypto.randomUUID(), url));
      const reply = await chrome.runtime.sendMessage({
        target: WORKER_TARGET, type: "hd_options_write", requestId: crypto.randomUUID(),
        profileId: readerOptions().JAPANESE_PROFILE_ID,
        baseRevision: Number.isInteger(stored?.revision) ? stored.revision : 0,
        options: { audioSources: [...local, ...sources.filter(source => !local.includes(source))] },
      });
      if (reply?.conflict) continue;
      if (!reply?.ok) throw new Error(reply?.error || "Hachidori didn't take the audio sources");
      await chrome.storage.local.set({ [LOCAL_AUDIO_SET_KEY]: true });
      return;
    }
    throw new Error("Hachidori's settings kept changing");
  } catch (error) {
    report("error", `Couldn't set up local pronunciation: ${errorText(error)}`);
  }
}

/** Points this Hachidori's Anki template at the note type, deck and tags Telly asks for. */
async function syncAnki() {
  const client = await status().catch(() => null);
  if (client?.linked) return;
  const want = await fetch(`${TELLY}/hd/anki`).then((r) => r.json()).catch(() => null);
  if (!want?.model || !want?.deck) return;
  const tags = Array.isArray(want.tags) ? want.tags : [];
  const key = `${want.model}\n${want.deck}\n${tags.join(" ")}`;
  if (key === ankiMade) return;
  const tell = (fields) => fetch(`${TELLY}/hd/anki/done?${new URLSearchParams(fields)}`).catch(() => {});
  try {
    for (let attempt = 0; attempt < 3; attempt += 1) {
      const stored = await storedOptions();
      const options = japaneseOptions(stored);
      const anki = options.anki;
      const tagged = Array.isArray(anki.tags) && anki.tags.join(" ") === tags.join(" ");
      if (anki.model === want.model && anki.deck === want.deck && tagged && anki.fieldTemplates) {
        ankiMade = key;
        await tell({ ok: "1" });
        return;
      }
      // A note type already set up keeps its fields; a new one gets the Kiku mapping.
      const fieldTemplates = anki.model === want.model && anki.fieldTemplates
        ? anki.fieldTemplates
        : ankiSetupTemplates("kiku", want.model, want.deck, want.fields, anki);
      if (!fieldTemplates) throw new Error(`${want.model} doesn't have Kiku's fields`);
      const reply = await chrome.runtime.sendMessage({
        target: WORKER_TARGET, type: "hd_options_write", requestId: crypto.randomUUID(),
        baseRevision: Number.isInteger(stored?.revision) ? stored.revision : 0,
        options: { anki: { ...anki, model: want.model, deck: want.deck, tags, fieldTemplates } },
      });
      if (reply?.conflict) continue;
      if (!reply?.ok) throw new Error(reply?.error || "Hachidori didn't take the Anki setup");
      ankiMade = key;
      await tell({ ok: "1" });
      return;
    }
    throw new Error("Hachidori's settings kept changing");
  } catch (error) {
    await tell({ ok: "0", error: errorText(error) });
  }
}

// Set once word highlighting has been turned on, so turning it off in
// Hachidori's Settings sticks.
const HIGHLIGHTS_SET_KEY = "tellyWordHighlightsSet";

/** Turns on Hachidori's word highlighting, once, so subtitles mark the words without a mature card. */
async function enableHighlights() {
  try {
    if ((await chrome.storage.local.get(HIGHLIGHTS_SET_KEY))[HIGHLIGHTS_SET_KEY]) return;
    const client = await status();
    if (client?.linked) return;
    for (let attempt = 0; attempt < 3; attempt += 1) {
      const stored = await storedOptions();
      const reply = await chrome.runtime.sendMessage({
        target: WORKER_TARGET, type: "hd_options_write", requestId: crypto.randomUUID(),
        baseRevision: Number.isInteger(stored?.revision) ? stored.revision : 0,
        options: { wordHighlightEnabled: true },
      });
      if (reply?.conflict) continue;
      if (!reply?.ok) throw new Error(reply?.error || "Hachidori didn't take the setting");
      await chrome.storage.local.set({ [HIGHLIGHTS_SET_KEY]: true });
      return;
    }
  } catch (error) {
    report("error", `Couldn't turn on word highlighting: ${errorText(error)}`);
  }
}

/** A request to this Hachidori's engine (relayed by its background) or its Anki renderer. */
async function engineRequest(target, fields, direct) {
  const reply = await chrome.runtime.sendMessage({ ...fields, target, requestId: `telly-${crypto.randomUUID()}`, ...(direct ? { relayed: true } : {}) });
  if (!reply?.ok) throw new Error(reply?.error || "Hachidori's engine didn't answer");
  return reply;
}

/** This Hachidori's dictionaries, in its order, as its Japanese profile has them. */
async function readDictionaries() {
  const state = (await chrome.storage.local.get(DICTIONARY_STATE_KEY))[DICTIONARY_STATE_KEY] ?? null;
  const o = readerOptions();
  if (!o.hasDictionaryProfiles(state)) return state?.dictionaries ?? [];
  return o.projectDictionaryState(state, o.JAPANESE_PROFILE_ID)?.dictionaries ?? [];
}

let apiHost;
function api() {
  apiHost ??= createApiHost({
    version: chrome.runtime.getManifest().version,
    engine: (fields) => engineRequest(ENGINE_TARGET, fields, false),
    render: (fields) => engineRequest(RENDER_TARGET, fields, true),
    readDictionaries,
    readAudioSources: async () => japaneseOptions(await storedOptions()).audioSources.filter((source) => source.enabled),
    readAnkiTemplates: async () => japaneseOptions(await storedOptions()).anki.templates,
    readSmallerAnkiCards: async () => japaneseOptions(await storedOptions()).smallerAnkiCards,
    readCompactGlossaries: async () => japaneseOptions(await storedOptions()).glossaryLayoutMode === "compact-popup-anki",
  });
  return apiHost;
}

/**
 * Premining's lookups (telly_term_entries): for each text, Hachidori's best
 * match, the first /termEntries gives (api-host.js termEntry), with only what
 * Telly reads of it: its length, headword, reading and frequencies. The rest
 * (every dictionary's definitions of up to 32 matches) never leaves the page.
 * The engine still finds its usual 32: asked for one, it can rank two that tie
 * the other way round from the popup. Several lookups go at once, so the
 * engine isn't left waiting on the messages between them.
 */
async function termEntries(terms) {
  const dictionaries = await readDictionaries();
  const titles = dictionaries.map((item) => item.title);
  const index = (title) => Math.max(0, titles.indexOf(title));
  const mode = (title) => dictionaries.find((item) => item.title === title)?.frequencyMode || null;
  const entry = async (text) => {
    const reply = text === "" ? null : await engineRequest(ENGINE_TARGET, { type: "hd_lookup", text }, false);
    const best = reply?.results?.[0];
    if (!best) return { originalTextLength: 0, dictionaryEntries: [] };
    const frequencies = best.term.frequencies.flatMap((group) => group.frequencies.map((value) => ({
      dictionaryIndex: index(group.dictionary),
      frequencyMode: mode(group.dictionary),
      frequency: value.value,
      displayValue: value.displayValue || null,
    })));
    return {
      originalTextLength: best.matched.length,
      dictionaryEntries: [{ headwords: [{ term: best.term.expression, reading: best.term.reading }], frequencies }],
    };
  };
  const results = new Array(terms.length);
  let next = 0;
  const worker = async () => {
    while (next < terms.length) {
      const i = next++;
      results[i] = await entry(terms[i]);
    }
  };
  await Promise.all(Array.from({ length: LOOKUPS_AT_ONCE }, worker));
  return { results };
}

/** Answers premining's questions for as long as the page is open (Telly holds each ask 20 s). */
async function serveApi() {
  for (;;) {
    const next = await fetch(`${TELLY}/hd/api/next`).then((r) => r.json()).catch(() => null);
    if (next === null) {
      await new Promise((resolve) => setTimeout(resolve, 2000));
      continue;
    }
    if (!next.id) continue;
    let answer;
    try {
      const client = await status().catch(() => null);
      if (client?.linked) throw new Error("Telly's Hachidori is linked to your browser's, so premining asks that one");
      await engineReady();
      const request = next.request;
      answer = { ok: true, result: request?.type === "telly_term_entries" ? await termEntries(request.terms) : await api()(request) };
    } catch (error) {
      answer = { ok: false, error: errorText(error) };
    }
    await fetch(`${TELLY}/hd/api/answer?id=${next.id}`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(answer),
    }).catch(() => {});
  }
}

async function main() {
  try {
    await startEngine();
  } catch (error) {
    report("error", `Hachidori's engine did not start: ${errorText(error)}`);
    return;
  }
  await step(true);
  await enableLocalAudio();
  void syncNames();
  void enableHighlights();
  void syncAnki();
  void syncDictionaries();
  void serveApi();
  setInterval(() => {
    void step(false);
    void enableLocalAudio();
    void syncNames();
    void syncDictionaries();
  }, POLL_MS);
  setInterval(() => void syncAnki(), ANKI_POLL_MS);
}

main();
