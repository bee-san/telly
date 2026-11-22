// Telly on a second screen, like the AYN Thor's bottom one: Android's side
// (SecondScreen.kt) shows Telly's page /second there while Telly is in front.
// That page can't ask the backend anything itself, so the pages on the main
// screen say what it shows, and are told what was touched on it.
import { invoke } from "@tauri-apps/api/core";
import type { DictionaryStyle, Found } from "./lookup.svelte";

/** Whether the device's other screen is showing Telly. */
export const second = $state({ on: false });

/** A line as the second screen lists it: when it's said, who says it, and its text as HTML. */
export interface SecondLine {
  start: number;
  speaker: string;
  html: string;
}

/** What the second screen shows, each part said by the page that knows it. */
export interface SecondShows {
  /** What's playing, or null outside the player. */
  playing: { title: string; episode: string } | null;
  /** The lines of what's playing. */
  lines: SecondLine[];
  /** The line playing, or the last one said, and whether the video is paused. */
  at: { line: number; paused: boolean };
  /** The word looked up: which line it's in, where, how long it is, and what it means. */
  look: { line: number; offset: number; length: number; found: Found[]; error: string } | null;
  /** Your dictionaries' own styles. */
  styles: DictionaryStyle[];
  /** What to carry on watching, outside the player. */
  watching: { episode: number; title: string; label: string; cover: string }[];
}

/** What the second screen asks of its popup, and is answered, without it being kept. */
export interface SecondTold {
  /** A controller's buttons for the popup, which is on the second screen. */
  popup: { step?: number; scroll?: number; add?: boolean };
  /** What a word tapped in a definition means. */
  answer: { id: number; found: Found[]; styles: DictionaryStyle[] };
}

/** What was touched on the second screen. */
export type Touched =
  | { type: "seek"; line: number }
  | { type: "look"; line: number; offset: number }
  | { type: "lookup"; id: number; text: string }
  | { type: "add"; word: Found }
  | { type: "close" }
  | { type: "press"; button: "play" | "previous" | "next" | "replay" }
  | { type: "watch"; episode: number };

/** What the second screen was last given, by what it's about. */
const showing = new Map<string, string>();

/** Shows `value` on the second screen as its `about`, until it's given another. Nothing is sent when there's no second screen. */
export function showSecond<K extends keyof SecondShows>(about: K, value: SecondShows[K]) {
  if (!second.on) return;
  const json = JSON.stringify(value);
  // Said once: a page that asks its backend again every few seconds mostly has nothing new.
  if (showing.get(about) === json) return;
  showing.set(about, json);
  invoke("plugin:telly|second", { about, json, keep: true }).catch(() => {});
}

/** Tells the second screen something once. */
export function tellSecond<K extends keyof SecondTold>(about: K, value: SecondTold[K]) {
  if (second.on) invoke("plugin:telly|second", { about, json: JSON.stringify(value), keep: false }).catch(() => {});
}

const listeners = new Set<(touched: Touched) => void>();

/** Hears what's touched on the second screen, until the function returned is called. */
export function onSecond(listener: (touched: Touched) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Starts hearing from the second screen: whether it's there, and what's touched on it. */
export function startSecond(): () => void {
  const heard = (e: Event) => {
    const said = (e as CustomEvent<Touched | { type: "shown" | "hidden" }>).detail;
    if (said.type === "shown" || said.type === "hidden") {
      second.on = said.type === "shown";
      // Android's side keeps what it was given only while the second screen is up.
      if (!second.on) showing.clear();
    }
    else for (const listener of [...listeners]) listener(said as Touched);
  };
  window.addEventListener("telly-second", heard);
  invoke<{ on: boolean }>("plugin:telly|secondState")
    .then((state) => (second.on = state.on))
    .catch(() => {});
  return () => window.removeEventListener("telly-second", heard);
}
