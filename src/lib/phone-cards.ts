// Cards on the phone: a word you tapped, with its line, its picture and its
// audio, added to AnkiDroid with the note type, deck, tags and fields your
// PC's cards use (phone.rs card_plan, TellyPlugin.kt).
import { invoke } from "@tauri-apps/api/core";
import { call, type Line, type PlayerView } from "$lib/api";
import { glossaryHtml, type Found } from "$lib/lookup.svelte";

interface AnkiState {
  installed: boolean;
  allowed: boolean;
  /** The Anki app cards go to: AnkiDroid, or kuma3 (its fork) when only that is installed. */
  app: string;
  /** Whether this build can cut clips (FFmpegKit is in it). */
  clips: boolean;
}

interface CardPlan {
  picture: string;
  audio: string;
  pictureArgs: string[];
  audioArgs: string[];
  model: string;
  deck: string;
  tags: string[];
  fields: { sentence: string; picture: string; audio: string; info: string };
  info: string;
}

function esc(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c] as string);
}

/** The word with its reading as Anki's furigana writes it: 学校[がっこう]. */
function furigana(expression: string, reading: string): string {
  return reading && reading !== expression && /[㐀-鿿々]/.test(expression) ? `${expression}[${reading}]` : expression;
}

/** Whether AnkiDroid is there and Telly may add to it, asking you once if not. */
export async function ankiReady(): Promise<AnkiState> {
  let state = await invoke<AnkiState>("plugin:telly|ankiState");
  if (!state.installed) throw new Error("AnkiDroid isn’t installed. Install it, and sync it with AnkiWeb so it has your note type.");
  if (!state.allowed) state = await invoke<AnkiState>("plugin:telly|ankiAllow");
  if (!state.allowed) throw new Error(`Telly needs ${state.app}’s permission to add cards. Allow it, and add the card again.`);
  return state;
}

/** Adds a card for the word `f` from `line` of the episode playing. Its clip is cut first, when this build can. */
export async function addPhoneCard(view: PlayerView, line: Line, f: Found): Promise<{ withClip: boolean; app: string }> {
  const state = await ankiReady();
  const plan = await call<CardPlan>("phoneCardPlan", { episodeId: view.episode.id, start: line.start, end: line.end, text: line.text });
  const media: { field: string; path: string; kind: string }[] = [];
  if (state.clips) {
    await invoke("plugin:telly|clip", { runs: [plan.pictureArgs, plan.audioArgs] });
    media.push({ field: plan.fields.picture, path: plan.picture, kind: "image" }, { field: plan.fields.audio, path: plan.audio, kind: "audio" });
  }
  const text = line.text.replace(/\n/g, " ");
  const at = text.indexOf(f.matched);
  const sentence = at < 0 ? esc(text) : `${esc(text.slice(0, at))}<b>${esc(f.matched)}</b>${esc(text.slice(at + f.matched.length))}`;
  const glossaries = f.term.glossaries.map(glossaryHtml);
  const fields: Record<string, string> = {
    Expression: f.term.expression,
    ExpressionFurigana: furigana(f.term.expression, f.term.reading),
    ExpressionReading: f.term.reading,
    MainDefinition: glossaries[0] ?? "",
    Glossary: glossaries.join(""),
    [plan.fields.sentence]: sentence,
    [plan.fields.info]: plan.info,
  };
  await invoke("plugin:telly|addCard", { model: plan.model, deck: plan.deck, tags: plan.tags, fields, media });
  return { withClip: state.clips, app: state.app };
}
