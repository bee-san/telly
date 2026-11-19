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
