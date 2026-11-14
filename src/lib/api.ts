// Talking to Telly's backend (src-tauri/src/backend.rs). In the app, through
// Tauri; in an ordinary browser (development), through the dev server
// (npm run devserver -- <data dir>), which runs the same backend.
import { invoke } from "@tauri-apps/api/core";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const DEV_SERVER = "http://127.0.0.1:1430";

export async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (inTauri) return invoke<T>("call", { cmd, args });
  const response = await fetch(DEV_SERVER + "/call", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ cmd, args }),
  });
  const answer = await response.json();
  if ("error" in answer) throw new Error(answer.error);
  return answer.ok as T;
}

const answers = new Map<string, { text: string; answer: unknown; tag?: string }>();

/** `call`, keeping the answer for `kept`. The same answer as last time is the
 *  same object, so a page that asks again and again has nothing to draw again. */
export async function callKept<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const key = cmd + JSON.stringify(args);
  const last = answers.get(key);
  // A tagged answer (the Library, a show's page) isn't sent again when it hasn't changed.
  const answer = await call<T & { unchanged?: boolean; tag?: string }>(cmd, last?.tag ? { ...args, tag: last.tag } : args);
  if (answer?.unchanged && last) return last.answer as T;
  const text = JSON.stringify(answer);
  if (last?.text === text) return last.answer as T;
  answers.set(key, { text, answer, tag: answer?.tag });
  return answer;
}

/** The last answer `callKept` got to this: a page shows it at once when it opens, and asks again. */
export function kept<T>(cmd: string, args: Record<string, unknown> = {}): T | null {
  return (answers.get(cmd + JSON.stringify(args))?.answer as T | undefined) ?? null;
}

/** A URL for something Telly serves: "/ep/12", "/cover/3". */
export function mediaUrl(path: string): string {
  if (!path) return "";
  if (!inTauri) return DEV_SERVER + "/media" + path;
  const windows = navigator.userAgent.includes("Windows");
  return (windows ? "http://media.localhost" : "media://localhost") + path;
}

export function errorText(e: unknown): string {
  if (e instanceof Error) return e.message;
  return String(e);
}

export interface Show {
  id: number;
  folder: string;
  title: string;
  titleNative: string;
  titleEnglish: string;
  titleRomaji: string;
  anilistId: number | null;
  anilistEpisodes: number | null;
  format: string;
  year: number | null;
  coverUrl: string;
  coverColor: string;
  jimakuId: number | null;
  addedAt: number;
  lastWatchedAt: number | null;
}

export interface NextUp {
  episodeId: number;
  number: number;
  resume: boolean;
  position: number;
  duration: number;
  ready: boolean;
  mediaState: string;
  mediaProgress: number;
}

export interface ShowCard extends Show {
  cover: string;
  /** The cover is a frame from the video: the title is drawn over it. */
  coverCaption: boolean;
  /** A film, listed as one rather than as an episode. */
  movie: boolean;
  /** The uses of words in its scored episodes, and how many are of words you know (0 and 0 when none is scored). */
  wordsUses: number;
  wordsKnown: number;
  episodes: number;
  watched: number;
  mediaReady: number;
  subsReady: number;
  /** Episodes with subtitles (synced, or needing a look). */
  subsFound: number;
  status: "new" | "watching" | "finished" | "getting";
  next: NextUp | null;
  cards: number;
}

export interface Task {
  showId: number;
  episodeId: number;
  number: number;
  title: string;
  label: string;
  progress: number;
}

export interface Activity {
  tasks: Task[];
  /** How many shows are being got ready at once. */
  shows: number;
  showId: number | null;
  title: string;
  done: number;
  total: number;
}

/** An episode waiting for work (backend.rs `Queued`). */
export interface Queued {
  showId: number;
  episodeId: number;
  title: string;
  number: number;
  /** How many episodes its show has: a film's one goes by the film's title. */
  episodes: number;
  /** What's done to it next, where that's worth saying: "To convert". */
  step: string;
}

/** What the title bar's list shows: what each worker is doing, and the work
 *  waiting of each kind, the next to be taken first. */
export interface Queue {
  now: Task[];
  waiting: { kind: string; what: string; count: number; first: Queued[] }[];
}

export interface Setup {
  ffmpeg: boolean;
  jimaku: boolean;
  canClip: boolean;
}

export interface Library {
  shows: ShowCard[];
  activity: Activity;
  setup: Setup;
}

export interface Episode {
  id: number;
  showId: number;
  number: number;
  path: string;
  size: number;
  duration: number;
  mediaState: "pending" | "working" | "ready" | "failed";
  mediaPlan: string;
  mediaProgress: number;
  mediaError: string;
  playPath: string;
  audioIndex: number;
  /** The rip's audio track that plays (null: the Japanese one). */
  audioTrack: number | null;
  endingAt: number | null;
  position: number;
  watchedAt: number | null;
  lastPlayedAt: number | null;
  subsState: "pending" | "working" | "synced" | "check" | "none" | "nokey" | "failed" | "manual" | "whisper";
  subsDetail: string;
  subsFile: string;
  subsScore: number | null;
  subsOffsetMs: number | null;
  subsNudgeMs: number;
}

export interface EpisodeRow extends Episode {
  file: string;
  watched: boolean;
  isNext: boolean;
  /** Uses of words in the episode, and how many are of words on cards studied in Anki. */
  words: { uses: number; known: number } | null;
}

/** What was watched and mined on a day (YYYY-MM-DD, local time). */
export interface StatsDay {
  day: string;
  seconds: number;
  /** Episodes played to their end that day, the first time or again. */
  episodes: number;
  lines: number;
  chars: number;
  cards: number;
  /** The shows played. */
  shows: number[];
  /** Uses of words in the episodes played, and how many are of known words (their shares as they are now). */
  uses: number;
  known: number;
}

export interface Stats {
  days: StatsDay[];
  /** Known words, by the day their first card was made. */
  words: { day: string; n: number }[];
  /** Uses of words in the whole library, and how many are of known words. */
  library: { uses: number; known: number };
  /** The finished shows' cards, by show ID. */
  shows: Record<string, ShowCard>;
  /** Each finish and full rewatch, oldest first, in Unix milliseconds. */
  finished: { show: number; at: number; rewatch: boolean }[];
}

/** A show's own page in Stats. */
export interface ShowStats {
  card: ShowCard;
  completion: { finishedAt: number | null; rewatches: number[] };
  episodes: { number: number; words: { uses: number; known: number } | null }[];
  /** What played of each episode each day. */
  played: { day: string; showId: number; episode: number; seconds: number; lines: number; chars: number }[];
  /** Each card mined from it: its note, episode and sentence, and its word once it's been studied. */
  cards: { note: number; episode: number; sentence: string; word: string | null }[];
}

export interface ShowView {
  card: ShowCard;
  episodes: EpisodeRow[];
  activity: Activity;
  /** First completion and full rewatches, newest first, in Unix milliseconds. */
  completion: { finishedAt: number | null; rewatches: number[] };
  /**
   * Whether subtitles can be made (by a model on OpenRouter, or whisper.cpp
   * here with a model), whether whisper.cpp only lacks its model, and who
   * makes them: "MAI-Transcribe", "ElevenLabs" or "Whisper".
   */
  whisper: { ready: boolean; needsModel: boolean; maker: string };
  /** Why episodes have no share of known words ("" when they do, or it's being worked out). */
  wordsError: string;
  /** Its rips that aren't where Telly has them, for Find its folder… (null when they all are). */
  ripsMissing: MissingRips | null;
}

/** A show whose rips aren't all where Telly has them: its folder isn't there (its drive unplugged, or the folder moved), or some of its episodes' files aren't. */
export interface MissingRips {
  showId: number;
  title: string;
  folder: string;
  folderMissing: boolean;
  episodes: number;
  /** How many of its episodes' rips aren't there: all of them when its folder isn't. */
  missing: number;
}

/**
 * What Telly found of a show's rips in the folder it's in now (`to`), which
 * it has as `from`: each episode found, where it was and where it is now, and
 * those not found, which are left as they are.
 */
export interface ShowMove {
  showId: number;
  title: string;
  from: string;
  to: string;
  found: { id: number; number: number; from: string; to: string }[];
  missing: { id: number; number: number; path: string }[];
}

/** What a backup says about itself (backup.rs): the Telly that made it, when, and how many shows and episodes it has. */
export interface Backup {
  telly: string;
  madeAt: number;
  shows: number;
  episodes: number;
}

/** A backup chosen to restore: what's in it, where it is, and how many shows the library here has. */
export interface BackupFile extends Backup {
  path: string;
  showsHere: number;
}

export interface Line {
  start: number;
  end: number;
  text: string;
  ruby?: [number, number, string][];
  speaker?: string;
  sign?: boolean;
}

export interface Tried {
  name: string;
  path: string;
  score: number;
  offsetMs: number;
  error: string;
  /** A file of yours beside the rip: used as it is, never synced, so it has no score. */
  beside?: boolean;
}

export interface EpisodeLite {
  id: number;
  number: number;
  ready: boolean;
  mediaState: string;
  mediaProgress: number;
}

export interface PlayerView {
  show: Show;
  episode: Episode;
  src: string;
  /** The last card event when the player opened: it shows only those after. */
  lastEvent?: number;
  lines: Line[];
  prev: EpisodeLite | null;
  next: EpisodeLite | null;
  count: number;
  tried: Tried[];
  /** The rip's chapters. */
  chapters: { start: number; end: number; title: string }[];
  opening: { start: number; end: number } | null;
  /** The ending, from the rip's chapters or AniSkip. */
  ending: { start: number; end: number } | null;
  /** The rip's audio tracks, by name, and which one plays. */
  audio: string[];
  audioTrack: number;
  /** Which of the audio tracks the player lists is that one (it lists those of the file it loads that it can play). */
  listedAudio: number;
  /** openPlayer only: how long since the show was last open, in ms (null the first time). */
  awayMs?: number | null;
  /** openPlayer only: whether there's an episode before this one to recap, and whether its recap is written. */
  recap?: { possible: boolean; ready?: boolean };
  /** openPlayer only, picking up part-way through: the same for a recap that runs up to where the episode was left. */
  resume?: { possible: boolean; ready?: boolean };
}

/** The seek bar's pictures of an episode: one every `every` seconds, tiled in one image. */
export interface SeekSheet {
  src: string;
  every: number;
  columns: number;
  rows: number;
}

/** An answer from Claude being written (ask), or written (text, done). */
export interface AskProgress {
  ask?: number;
  text?: string;
  done?: boolean;
  error?: string;
  seconds?: number;
  line?: string;
}

/** A stretch of an episode, played in "Previously on", and what's happening in it. */
export interface Moment {
  start: number;
  end: number;
  lines: Line[];
  caption: string;
  episodeId: number;
  number: number;
  src: string;
  /** As PlayerView's. */
  listedAudio: number;
  playable: boolean;
  nudgeMs: number;
  /** Whether it's from the episode you left: the moments that play when you're back within a few days. */
  latest: boolean;
}

/** A line a search found (search.rs). */
export interface FoundLine {
  episodeId: number;
  showId: number;
  number: number;
  /** Which of the episode's spoken lines it is. */
  line: number;
  start: number;
  end: number;
  /** The line around what was found. */
  before: string;
  word: string;
  after: string;
  /** How many of its other words you don't know; null when its episode's words aren't saved yet. */
  unknown: number | null;
  /** Whether what was found is said: not sung, and not a caption's note. */
  said: boolean;
  /** Which of the seek bar's pictures shows it. */
  picture: number;
}

/** What a line search answers: the lines each show can be read soonest in (up to 100 a show), and
 *  how many lines each show has, by its ID. */
export interface LineSearch {
  lines: FoundLine[];
  shows: Record<number, number>;
}

/** The scene around a line: the stretch of its episode to play, and what's said in it. */
export interface SceneView {
  episodeId: number;
  start: number;
  end: number;
  lines: { start: number; end: number; text: string }[];
  /** Which of `lines` was asked for. */
  line: number;
  src: string;
  /** As PlayerView's. */
  listedAudio: number;
  playable: boolean;
}

/** A part of the story in a recap, and the episodes it covers. */
export interface StoryArc {
  title: string;
  from: number;
  to: number;
  summary: string;
}

export interface Recap {
  ask?: number;
  done?: boolean;
  error?: string;
  /** The whole recap as plain text. */
  recap?: string;
  arcs?: StoryArc[];
  last?: string;
  lastNumber?: number;
  moments?: Moment[];
}

export interface ClaudeStatus {
  available: boolean;
  version: string;
  detail: string;
}

/** A character's name as the show writes it, from the names dictionary. */
export interface CastName {
  term: string;
  name: string;
  role: string;
}

export interface NamesStatus {
  enabled: boolean;
  spoilers: boolean;
  summary: { shows: string[]; characters: number; bytes: number } | null;
  built: boolean;
  imported: boolean;
  importedAt: number;
  importedTo: string;
  error: string;
  buildError: string;
  working: Task | null;
}

export interface CardEvent {
  id: number;
  state: "working" | "done" | "failed";
  title: string;
  text: string;
  noteId: number;
  jobId: number;
}

export interface ScannedFile {
  path: string;
  rel: string;
  size: number;
  duration: number;
  parsed: number | null;
  number: number | null;
  reason: string;
}

export interface Scan {
  folder: string;
  guess: string;
  files: ScannedFile[];
  numbering: "names" | "order";
  groups: { folder: string; name: string; videos: number }[];
}

export interface Anime {
  id: number;
  romaji: string;
  english: string;
  native: string;
  synonyms: string[];
  format: string;
  episodes: number | null;
  year: number | null;
  cover: string;
  color: string;
  score: number;
}

export interface ScanResult {
  scan: Scan;
  matches: Anime[];
  error: string | null;
  ffprobe: boolean;
}

/** An anime AniList's users recommend to people who liked shows in the library. */
export interface Pick extends Anime {
  /** Its short title, as the library would show it. */
  title: string;
  genres: string[];
  /** AniList's synopsis, with its markup. */
  description: string;
  /** The library's shows it's recommended from, the one that counts most first. */
  from: { title: string; votes: number }[];
}

export interface Recommended {
  /** Shows in the library that AniList knows. */
  shows: number;
  /** Every pick, best first. */
  picks: Pick[];
}

export interface Tools {
  ffmpeg: string | null;
  ffprobe: string | null;
  version: string;
  encoder: string;
  canClip: boolean;
  whisper: string | null;
  whisperModel: string | null;
}

export interface Status {
  tools: Tools;
  mining: { address: string; listening: boolean; listenError: string | null; beacon: boolean } | null;
  beaconAddress: string;
  anki: boolean;
  ankiUrl: string;
  dataDir: string;
  /** Where Telly's copies of episodes go. */
  mediaDir: string;
  logPath: string;
  spaceUsed: number;
  fields: { sentence: string; picture: string; audio: string; info: string };
}

export type Settings = Record<string, unknown>;

/** What Settings → Known words chooses from: Anki's decks, its note types with the word's field, and how many notes the choice finds. */
export interface KnownSources {
  field: string;
  decks: string[];
  noteTypes: string[];
  studied: number;
  /** The studied notes whose studied cards are all suspended, so their words aren't known. */
  suspended: number;
  mature: number;
}

/** Anki's decks and note types, by name. */
export interface AnkiNames {
  decks: string[];
  noteTypes: string[];
}

// ---- Hachidori (app only) -----------------------------------------------------

export interface HostStatus {
  state: "starting" | "linked" | "connecting" | "standalone" | "link-failed" | "error";
  detail: string;
  host: string;
}

export interface HachidoriInfo {
  supported: boolean;
  prepared: boolean;
  id: string | null;
  version: string | null;
  link: boolean;
  status: HostStatus | null;
}

export async function hachidoriInfo(): Promise<HachidoriInfo | null> {
  if (!inTauri) return null;
  return invoke<HachidoriInfo>("hachidori_info");
}

export async function hachidoriStart(): Promise<boolean> {
  if (!inTauri) return false;
  return invoke<boolean>("hachidori_start");
}

export async function openHachidoriSettings(): Promise<void> {
  if (!inTauri) return;
  await invoke("open_hachidori_settings");
}

export async function relayStatus(): Promise<string> {
  if (!inTauri) return "Off";
  return invoke<string>("relay_status");
}

/** Windows' folder picker in the app; a prompt in a browser. */
export async function pickFolder(title = "Add media"): Promise<string | null> {
  if (!inTauri) return window.prompt(`${title} (full path)`);
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: true, multiple: false, title });
  return typeof picked === "string" ? picked : null;
}

/** Windows' folder picker, for one folder or several; a prompt for one in a browser. */
export async function pickFolders(title: string): Promise<string[]> {
  if (!inTauri) {
    const one = window.prompt(`${title} (full path)`);
    return one ? [one] : [];
  }
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: true, multiple: true, title });
  return Array.isArray(picked) ? picked : typeof picked === "string" ? [picked] : [];
}

export async function pickFile(title: string, extensions: string[], name: string): Promise<string | null> {
  if (!inTauri) return window.prompt(title + " (full path)");
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: false, multiple: false, title, filters: [{ name, extensions }] });
  return typeof picked === "string" ? picked : null;
}

/** Windows' save dialog in the app; a prompt in a browser. */
export async function pickSaveFile(title: string, extensions: string[], name: string, suggested: string): Promise<string | null> {
  if (!inTauri) return window.prompt(title + " (full path)", suggested);
  const { save } = await import("@tauri-apps/plugin-dialog");
  return save({ title, defaultPath: suggested, filters: [{ name, extensions }] });
}

/** Starts Telly again, to finish restoring a backup (in the app only). */
export async function restartTelly(): Promise<void> {
  if (inTauri) await invoke("restart");
}

export async function setWindowTitle(title: string): Promise<void> {
  document.title = title;
  if (!inTauri) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().setTitle(title);
}

export async function setFullscreen(on: boolean): Promise<void> {
  if (!inTauri) {
    if (on) await document.documentElement.requestFullscreen?.();
    else if (document.fullscreenElement) await document.exitFullscreen();
    return;
  }
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().setFullscreen(on);
}

/** A word to premine, with the first line it comes up in. */
export interface PremineWord {
  headword: string;
  reading: string;
  /** Its rank in your frequency dictionary: lower is more common. */
  rank: number | null;
  /** Times it comes up in the show. */
  count: number;
  episodeId: number;
  number: number;
  start: number;
  end: number;
  before: string;
  word: string;
  after: string;
}

export interface PremineList {
  /** Episodes with no subtitles yet. Until there are none, there are no words. */
  withoutSubs: number[];
  /** Words you don't know yet, most used first. */
  words: PremineWord[];
  /** Uses of words in the show, and how many are words you know. */
  uses: number;
  knownUses: number;
  deck: string;
}

export interface PremineStatus {
  show: number;
  working: boolean;
  step: string;
  progress: number;
  list: PremineList | null;
  error: string;
  added: number;
  skipped: string[];
}

/** Telly's own Hachidori: your dictionaries going in, and its Anki setup. */
export interface OwnHachidori {
  folder: string;
  total: number;
  installed: number;
  installing: string;
  failed: string[];
  model: string;
  deck: string;
  ankiError: string;
}
