// Telly on a phone (src-tauri/src/phone.rs): the shows it got from your PC,
// and getting more.
import type { Show } from "$lib/api";

/** Whether this Telly is the one on your phone. */
export const onPhone = typeof navigator !== "undefined" && /Android/i.test(navigator.userAgent);

export interface Getting {
  id: number;
  show: number;
  state: "waiting" | "making" | "copying" | "failed";
  progress: number;
  size: number;
  error: string;
}

export interface PhoneState {
  pc: { address: string; name: string } | null;
  /** Room left on the phone, in bytes. */
  free: number;
  getting: Getting[];
  /** When progress last went to the PC (unix ms; 0 never). */
  sentAt: number;
}

export interface PhoneEpisode {
  id: number;
  number: number;
  duration: number;
  position: number;
  watched: boolean;
  onPhone: boolean;
  getting: { state: Getting["state"]; progress: number; error: string } | null;
}

export interface PhoneShow {
  show: Show;
  episodes: PhoneEpisode[];
  /** The first episode not watched yet. */
  next: number | null;
}

export interface PhoneLibrary {
  shows: PhoneShow[];
  state: PhoneState;
}

/** An episode in the PC's library, as Get from PC lists it. */
export interface PcEpisode {
  episode: { id: number; number: number; duration: number; watchedAt: number | null; position: number };
  ready: boolean;
  /** Its rip's size: about what the phone's copy will be, or more. */
  rip: number;
  phone: { state: "none" | "waiting" | "working" | "ready" | "failed"; progress: number; error: string; size: number };
  onPhone: "ready" | "getting" | "no";
}

export interface PcLibrary {
  name: string;
  shows: { show: Show; episodes: PcEpisode[] }[];
  free: number;
}

/** The episodes the phone gets without being asked: of each show you're part-way through on the PC,
 *  the last watched first, those of its next `n` not watched that the phone hasn't got and isn't getting. */
export function nextToGet(lib: PcLibrary, n = 3): number[] {
  return lib.shows
    .filter((s) => s.episodes.some((e) => e.episode.watchedAt || e.episode.position > 30))
    .sort((a, b) => (b.show.lastWatchedAt ?? 0) - (a.show.lastWatchedAt ?? 0))
    .flatMap((s) => s.episodes.filter((e) => !e.episode.watchedAt).slice(0, n))
    .filter((e) => e.onPhone === "no")
    .map((e) => e.episode.id);
}

/** What getting an episode is doing, in a few words. */
export function gettingText(g: { state: Getting["state"]; progress: number; error: string }): string {
  const pct = `${Math.round(g.progress * 100)}%`;
  switch (g.state) {
    case "waiting":
      return "Waiting";
    case "making":
      return `The PC is making its copy · ${pct}`;
    case "copying":
      return `Copying · ${pct}`;
    case "failed":
      return g.error || "Couldn’t be got";
  }
}

/** Video kinds as the PC names them (phone.rs video_kind), each with a codec string to ask the WebView about. */
const VIDEO: Record<string, string> = {
  h264: "avc1.640028",
  "h264-10": "avc1.6e0028",
  hevc: "hvc1.1.6.L120.90",
  "hevc-10": "hvc1.2.4.L120.90",
  av1: "av01.0.08M.08",
  "av1-10": "av01.0.08M.10",
  vp9: "vp09.00.40.08",
  "vp9-10": "vp09.02.40.10",
};
/** Audio codecs as ffprobe names them, each with the type to ask about. */
const AUDIO: Record<string, string> = {
  aac: 'audio/mp4; codecs="mp4a.40.2"',
  mp3: "audio/mpeg",
  opus: 'audio/mp4; codecs="opus"',
  flac: 'audio/mp4; codecs="flac"',
  vorbis: 'audio/webm; codecs="vorbis"',
  ac3: 'audio/mp4; codecs="ac-3"',
  eac3: 'audio/mp4; codecs="ec-3"',
  dts: 'audio/mp4; codecs="dtsc"',
  truehd: 'audio/mp4; codecs="mlpa"',
};

/** What this phone's WebView plays, for the PC to send episodes as they are when it can.
 *  A codec counts when either way of asking says yes: Media Capabilities says no to
 *  H.264 10-bit, which plays, and canPlayType says "maybe" to it. */
export async function phonePlays(): Promise<{ video: string[]; audio: string[] }> {
  const probe = document.createElement("video");
  const capable = async (config: MediaDecodingConfiguration) => {
    try {
      return (await navigator.mediaCapabilities.decodingInfo(config)).supported;
    } catch {
      return false;
    }
  };
  const video: string[] = [];
  for (const [kind, codec] of Object.entries(VIDEO)) {
    const type = `video/mp4; codecs="${codec}"`;
    const yes = probe.canPlayType(type) !== "" || (await capable({ type: "file", video: { contentType: type, width: 1920, height: 1080, bitrate: 8e6, framerate: 24 } }));
    if (yes) video.push(kind);
  }
  const audio: string[] = [];
  for (const [codec, type] of Object.entries(AUDIO)) {
    const yes = probe.canPlayType(type) !== "" || (await capable({ type: "file", audio: { contentType: type, channels: "2", bitrate: 192000, samplerate: 48000 } }));
    if (yes) audio.push(codec);
  }
  return { video, audio };
}
