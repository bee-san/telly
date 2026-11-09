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
  episodes: number;
  watched: number;
  mediaReady: number;
  subsReady: number;
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
  showId: number | null;
  title: string;
  done: number;
  total: number;
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
  endingAt: number | null;
  position: number;
  watchedAt: number | null;
  lastPlayedAt: number | null;
  subsState: "pending" | "working" | "synced" | "check" | "none" | "nokey" | "failed" | "manual";
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
}

export interface ShowView {
  card: ShowCard;
  episodes: EpisodeRow[];
  activity: Activity;
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
  lines: Line[];
  prev: EpisodeLite | null;
  next: EpisodeLite | null;
  count: number;
  tried: Tried[];
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
  logPath: string;
  spaceUsed: number;
  fields: { sentence: string; picture: string; audio: string; info: string };
}

export type Settings = Record<string, unknown>;

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
export async function pickFolder(): Promise<string | null> {
  if (!inTauri) return window.prompt("Folder to add (full path)");
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: true, multiple: false, title: "Add media" });
  return typeof picked === "string" ? picked : null;
}

export async function pickFile(title: string, extensions: string[], name: string): Promise<string | null> {
  if (!inTauri) return window.prompt(title + " (full path)");
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: false, multiple: false, title, filters: [{ name, extensions }] });
  return typeof picked === "string" ? picked : null;
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
