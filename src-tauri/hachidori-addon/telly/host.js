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
//
// Telly watches this page's title for its state:
// "telly-hachidori:" + JSON {state, detail, host}, where state is one of
// starting, linked, connecting, standalone, link-failed or error.

import { extensionApi as chrome } from "../browser-api.js";
import { chromeOffscreenSupported, ensureChromeOffscreen } from "../chrome-offscreen.js";

const SHARING = "hachidori-sharing";
const PREFIX = "telly-hachidori:";
const ENGINE = "offscreen.html";
const POLL_MS = 15_000;

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

async function main() {
  try {
    await startEngine();
  } catch (error) {
    report("error", `Hachidori's engine did not start: ${errorText(error)}`);
    return;
  }
  await step(true);
  setInterval(() => step(false), POLL_MS);
}

main();
