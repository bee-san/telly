// What works and what doesn't: the checks the first-time setup makes and
// Settings' Overview lists, so both find a missing key or a broken Anki the
// same way. Each is worked out from what the backend says and,
// in the app, from Hachidori's own status.
import { call, errorText, inTauri, type AnkiNames, type ClaudeStatus, type HachidoriInfo, type OwnHachidori, type Settings, type Status } from "./api";

/** What Settings' Overview and the setup call each check, and the page of Settings that sets it right. */
export const IN_SETTINGS: Record<string, { name: string; page: string }> = {
  ffmpeg: { name: "ffmpeg", page: "playing" },
  clip: { name: "Pictures and audio for cards", page: "playing" },
  jimaku: { name: "Subtitles", page: "keys" },
  anki: { name: "Anki", page: "cards" },
  fields: { name: "Note type", page: "cards" },
  deck: { name: "Deck", page: "cards" },
  beacon: { name: "Anki Beacon", page: "cards" },
  hachidori: { name: "Hachidori", page: "lookups" },
  dictionaries: { name: "Dictionaries", page: "lookups" },
  claude: { name: "Explain and Previously on", page: "explain" },
  anilist: { name: "AniList", page: "keys" },
};

/** It works (ok), works less well than it could (warn), doesn't work (bad), is being found out or starting (wait), or is optional and not set up (off). */
export type CheckState = "ok" | "warn" | "bad" | "wait" | "off";

export interface Check {
  id: string;
  state: CheckState;
  text: string;
}

/** Anki's decks and note types, and the fields of the note type for cards (none when Anki has no such note type). */
export interface AnkiFacts extends AnkiNames {
  fields: string[];
}

/** What the checks are worked out from; null for what isn't known yet. */
export interface Facts {
  settings: Settings;
  status: Status | null;
  /** Whether Jimaku takes the key: "" when it does, else why not. */
  jimaku: string | null;
  anki: AnkiFacts | null;
  own: OwnHachidori | null;
  hd: HachidoriInfo | null;
  claude: ClaudeStatus | null;
}

/** The note type cards are made as, and the deck they go in, as Settings → Cards and Anki has them. */
export const noteType = (s: Settings) => String(s["anki.noteType"] ?? "").trim() || "kiku_typed";
export const deck = (s: Settings) => String(s["anki.deck"] ?? "").trim() || "例文マイニング";

/** Whether Jimaku takes the key in Settings: "" when it does, else why not; null when there's no key to ask about. */
export async function askJimaku(settings: Settings): Promise<string | null> {
  if (!String(settings["jimaku.key"] ?? "").trim()) return null;
  return call("checkJimaku").then(() => "", errorText);
}

/** What Anki has, for the note type in Settings; null while Anki can't be asked. */
export async function askAnki(settings: Settings): Promise<AnkiFacts | null> {
  try {
    const names = await call<AnkiNames>("ankiNames");
    const model = noteType(settings);
    const fields = names.noteTypes.includes(model) ? await call<string[]>("ankiFields", { model }) : [];
    return { ...names, fields };
  } catch {
    return null;
  }
}

/** "a, b and c", or "a, b or c". */
function list(items: string[], last: "and" | "or"): string {
  return items.length < 2 ? items.join("") : `${items.slice(0, -1).join(", ")} ${last} ${items[items.length - 1]}`;
}

/** Whether Telly's Hachidori is linked to the one in the browser, so the dictionaries are that one's. */
export const linked = (hd: HachidoriInfo | null) => hd?.status?.state === "linked" || hd?.status?.state === "connecting";

/** What Telly's Hachidori is doing. */
export function hachidoriCheck(hd: HachidoriInfo | null): Check {
  const check = (state: CheckState, text: string): Check => ({ id: "hachidori", state, text });
  if (!inTauri) return check("off", "Hachidori runs in the app only.");
  if (!hd) return check("wait", "Starting…");
  if (!hd.prepared) return check("bad", "Not set up. Run npm run prepare:hachidori, then restart Telly.");
  const s = hd.status;
  if (!s) return check("wait", "Starting…");
  switch (s.state) {
    case "linked":
      return check("ok", `Linked to the browser’s Hachidori${s.host ? ` (${s.host})` : ""}: its dictionaries, settings, and Anki setup.`);
    case "connecting":
      return check("wait", `Connecting to the browser’s Hachidori…${s.detail ? ` ${s.detail}` : ""}`);
    case "standalone":
      return check("ok", "Running on its own, with the dictionaries and Anki setup below.");
    case "link-failed":
      return check("bad", `Couldn’t link: ${s.detail}. Open Anki with Hachidori Relay, or leave Telly’s relay on, then open Hachidori in the browser.`);
    case "error":
      return check("bad", s.detail);
    default:
      return check("wait", "Starting…");
  }
}

/** Your dictionaries going into Telly's Hachidori. */
export function dictionariesCheck(own: OwnHachidori, hd: HachidoriInfo | null): Check {
  const check = (state: CheckState, text: string): Check => ({ id: "dictionaries", state, text });
  if (linked(hd)) return check("ok", "Linked, so the dictionaries are the browser Hachidori’s.");
  if (!own.folder) return check("bad", "Choose the folder your Yomitan dictionaries (.zip files) are in.");
  if (!own.total) return check("bad", "There are no dictionaries (.zip files) in this folder.");
  const failed = own.failed.length ? ` Couldn’t install: ${own.failed.join(", ")}.` : "";
  if (own.installing) return check("wait", `Installing ${own.installing} · ${own.installed} of ${own.total} installed.${failed}`);
  if (own.installed === own.total) return check(failed ? "warn" : "ok", `All ${own.total} are installed.${failed}`);
  return check(failed ? "warn" : "wait", `${own.installed} of ${own.total} installed; the rest go in while Telly is open.${failed}`);
}

/** Every check. */
export function checks(f: Facts): Check[] {
  const out: Check[] = [];
  const add = (id: string, state: CheckState, text: string) => out.push({ id, state, text });
  const { settings, status } = f;

  // Your rips
  if (status) {
    const tools = status.tools;
    if (!tools.ffmpeg) add("ffmpeg", "bad", "Telly can’t find ffmpeg, which it needs to play and convert rips. It looks on PATH and where GameSentenceMiner keeps it. Choose ffmpeg.exe (its folder needs ffprobe.exe too).");
    else {
      add("ffmpeg", "ok", `Using ${tools.ffmpeg}`);
      add("clip", tools.canClip ? "ok" : "warn", tools.canClip ? "ffmpeg can make animated AVIF and Opus." : "This ffmpeg can’t make AVIF and Opus for cards (it needs libsvtav1 and libopus).");
    }
  }

  // Subtitles
  if (!String(settings["jimaku.key"] ?? "").trim()) add("jimaku", "bad", "Add a Jimaku API key so Telly can find subtitles.");
  else if (f.jimaku === null) add("jimaku", "wait", "Checking the Jimaku API key…");
  else if (f.jimaku === "") add("jimaku", "ok", "Jimaku takes the API key.");
  else add("jimaku", "bad", f.jimaku);

  // Anki
  if (status) {
    const open = status.anki;
    add("anki", open ? "ok" : "warn", open ? `Anki is open (AnkiConnect at ${status.ankiUrl}).` : "Anki isn’t open, or AnkiConnect isn’t installed.");
    const model = noteType(settings);
    if (!open) add("fields", "off", "Open Anki to check the note type and its fields.");
    else if (!f.anki) add("fields", "wait", "Asking Anki about the note type…");
    else if (!f.anki.noteTypes.includes(model)) add("fields", "bad", `Anki has no note type called ${model}.`);
    else {
      const wanted = [status.fields.sentence, status.fields.picture, status.fields.audio, status.fields.info];
      const missing = wanted.filter((name) => !f.anki?.fields.includes(name));
      if (missing.length) add("fields", "bad", `${model} has no ${list(missing, "or")} field.`);
      else add("fields", "ok", `${model} has the fields cards use: ${list(wanted, "and")}.`);
    }
    const into = deck(settings);
    if (open && f.anki && !into.includes("{show}")) {
      if (f.anki.decks.includes(into)) add("deck", "ok", `Cards go in ${into}.`);
      else add("deck", "warn", `Anki has no deck called ${into}. Make it in Anki, or choose another.`);
    }
    const m = status.mining;
    if (m?.beacon) add("beacon", "ok", "Anki Beacon is telling Telly about new cards.");
    else if (m?.listening)
      add("beacon", open ? "warn" : "off", `Listening for Anki Beacon at ${status.beaconAddress}. Add that address to Beacon’s config in Anki (Tools → Add-ons → Anki Beacon → Config). Until then, Telly asks AnkiConnect every 2 seconds during playback.`);
    else add("beacon", m?.listenError ? "bad" : "off", m?.listenError ?? "Not listening for Anki Beacon.");
  }

  // Lookups
  out.push(hachidoriCheck(f.hd));
  if (f.own) out.push(dictionariesCheck(f.own, f.hd));

  // Explain and Previously on
  if (!f.claude) add("claude", "wait", "Looking for Claude Code…");
  else if (f.claude.openrouter) add("claude", "ok", f.claude.detail);
  else if (f.claude.available) add("claude", "ok", `Claude Code ${f.claude.version.replace(/\s*\(Claude Code\)$/, "")} is installed.`);
  else add("claude", "bad", `Telly can’t find Claude Code: ${f.claude.detail} Install Claude Code, sign in, and check again, or add an OpenRouter API key and gpt-oss on OpenRouter answers instead.`);

  // AniList
  const user = String(settings["anilist.user"] ?? "");
  const anilistError = String(settings["anilist.error"] ?? "");
  if (!user) add("anilist", "off", "Not signed in, so what you watch doesn’t go on an AniList list.");
  else if (anilistError) add("anilist", "warn", `Your AniList list wasn’t updated: ${anilistError}. Telly tries again every 10 minutes.`);
  else add("anilist", "ok", `Signed in as ${user}. Episodes you watch go on your AniList list.`);

  return out;
}
