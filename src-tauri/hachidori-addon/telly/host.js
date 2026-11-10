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
//
// Telly watches this page's title for its state:
// "telly-hachidori:" + JSON {state, detail, host}, where state is one of
// starting, linked, connecting, standalone, link-failed or error.

import { extensionApi as chrome } from "../browser-api.js";
import { chromeOffscreenSupported, ensureChromeOffscreen } from "../chrome-offscreen.js";
import { uploadDictionary } from "../linked-import.js";

const SHARING = "hachidori-sharing";
const PREFIX = "telly-hachidori:";
const ENGINE = "offscreen.html";
const POLL_MS = 15_000;
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

let addingNames = false;

/** Sends Telly's character dictionary to Hachidori when there's a copy it hasn't had. */
async function syncNames() {
  if (addingNames) return;
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
  addingNames = true;
  const tell = (fields) => fetch(`${TELLY}/names/imported?${new URLSearchParams({ rev: state.revision, ...fields })}`).catch(() => {});
  try {
    if (!client?.linked) await engineReady();
    const archive = await fetch(`${TELLY}${state.url}`);
    if (!archive.ok) throw new Error("Telly's character dictionary wasn't there");
    const reply = await uploadDictionary({
      blob: await archive.blob(),
      fileName: "telly-characters.zip",
      replace: true,
      send: (type, fields) => chrome.runtime.sendMessage({ target: "hachidori-linked-import", type, requestId: crypto.randomUUID(), ...fields }),
    });
    if (reply?.report?.success !== true) throw new Error(reply?.report?.error || reply?.error || "Hachidori didn't take the dictionary");
    await tell({ ok: "1", to: client?.linked ? (client.host?.name || "your browser") : "telly" });
  } catch (error) {
    await tell({ ok: "0", error: errorText(error) });
  } finally {
    addingNames = false;
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
  void syncNames();
  setInterval(() => {
    void step(false);
    void syncNames();
  }, POLL_MS);
}

main();
