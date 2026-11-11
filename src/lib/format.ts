import type { ShowCard } from "./api";

/** "12:31" or "1:02:03". */
export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds || 0));
  const h = Math.floor(s / 3600);
  const m = Math.floor(s / 60) % 60;
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** "24 min", "1 h 32 min". */
export function minutes(seconds: number): string {
  const m = Math.round((seconds || 0) / 60);
  if (m < 60) return `${m} min`;
  return `${Math.floor(m / 60)} h ${m % 60} min`;
}

export function bytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${Math.round(n / 1e6)} MB`;
  if (n >= 1e3) return `${Math.round(n / 1e3)} KB`;
  return `${n} B`;
}

export function plural(n: number, one: string, many = one + "s"): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** The two lines under a show’s title: what’s next, and a detail. */
export function statusOf(s: ShowCard): { text: string; sub: string; ok?: boolean } {
  const n = s.next;
  if (s.status === "finished") return { text: "Finished", sub: `${plural(s.episodes, "episode")} · ${plural(s.cards, "card")}`, ok: true };
  if (!n) return { text: "No episodes", sub: "" };
  if (n.resume) {
    const left = Math.max(1, Math.round((n.duration - n.position) / 60));
    return { text: `Resume Episode ${n.number}`, sub: n.ready ? `${plural(left, "minute")} left` : gettingSub(s) };
  }
  const text = `Next: Episode ${n.number}`;
  if (!n.ready) return { text: s.watched > 0 || s.status === "getting" ? text : `Episode ${n.number}`, sub: gettingSub(s) };
  if (s.watched > 0) return { text, sub: `${s.watched} of ${plural(s.episodes, "episode")} watched` };
  return { text, sub: plural(s.episodes, "episode") };
}

function gettingSub(s: ShowCard): string {
  const n = s.next;
  if (!n) return "";
  if (n.mediaState === "working") return `Getting Episode ${n.number} ready · ${Math.round(n.mediaProgress * 100)}%`;
  if (n.mediaState === "failed") return `Episode ${n.number} couldn’t be converted`;
  return `Getting Episode ${n.number} ready`;
}

/** `file` is who made machine-made subtitles; `maker` is who would make them now. */
export function subsLabel(state: string, score: number | null, file: string, maker: string): { text: string; tone: "ok" | "warn" | "wait" | "bad" | "" } {
  switch (state) {
    case "synced":
      return score === null ? { text: `Made by ${file}`, tone: "ok" } : { text: `Synced · ${Math.round(score * 100)}%`, tone: "ok" };
    case "check":
      return { text: `Timing unsure · ${Math.round((score ?? 0) * 100)}%`, tone: "warn" };
    case "working":
      return { text: "Syncing…", tone: "wait" };
    case "pending":
      return { text: "Waiting", tone: "wait" };
    case "none":
      return { text: "None found", tone: "bad" };
    case "nokey":
      return { text: "Needs a Jimaku key", tone: "bad" };
    case "whisper":
      return { text: `Waiting for ${maker}`, tone: "wait" };
    case "failed":
      return { text: "Failed", tone: "bad" };
    default:
      return { text: state, tone: "" };
  }
}

export function mediaLabel(state: string, plan: string, progress: number): { text: string; tone: "ok" | "warn" | "wait" | "bad" | "" } {
  switch (state) {
    case "ready":
      return { text: plan === "direct" ? "Plays as it is" : plan === "video" ? "Converted" : "Copied", tone: "ok" };
    case "working":
      return { text: `${plan === "video" ? "Converting" : "Copying"} · ${Math.round(progress * 100)}%`, tone: "wait" };
    case "failed":
      return { text: "Failed", tone: "bad" };
    default:
      return { text: "Waiting", tone: "wait" };
  }
}

/** What a subtitle state means, for its tooltip. */
export function subsHelp(state: string, detail: string, file: string, maker: string): string {
  const what = [file, detail].filter(Boolean).join(" · ");
  switch (state) {
    case "synced":
      return `Synced: the lines fit this video. ${what}`;
    case "check":
      return `Telly found subtitles but isn’t sure they’re in time with this video. Watch a little, and use Sync ▾ in the player if they’re off. ${what}`;
    case "none":
      return `${detail || "No subtitles found for this episode."} Make subtitles with ${maker} can make some.`;
    default:
      return what;
  }
}

/** A readable ink for text on a cover colour. */
export function inkFor(hex: string): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex || "");
  if (!m) return "#f2f2f0";
  const n = parseInt(m[1], 16);
  const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255];
  return 0.299 * r + 0.587 * g + 0.114 * b > 150 ? "#151516" : "#f6f6f4";
}
