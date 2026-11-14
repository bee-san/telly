import type { ShowCard } from "./api";
import { plural, statusOf } from "./format";

/** An anime as the Library lists it: its seasons, movies and specials you have, in the order they came out. */
export interface AnimeCard {
  /** The anime's key (`ShowCard.series.id`). */
  id: number;
  title: string;
  native: string;
  parts: ShowCard[];
  /** The part to watch next: the one you're in, else the first you haven't finished. */
  current: ShowCard;
}

export const inProgress = (s: ShowCard) => s.status === "watching" || (s.status === "getting" && (s.watched > 0 || !!s.next?.resume));

/** The part of an anime to watch next, from its parts in order. */
function currentOf(parts: ShowCard[]): ShowCard {
  const now = parts.filter(inProgress).sort((a, b) => (b.lastWatchedAt ?? 0) - (a.lastWatchedAt ?? 0))[0];
  if (now) return now;
  const lastFinished = parts.findLastIndex((p) => p.status === "finished");
  return parts.slice(lastFinished + 1).find((p) => p.status !== "finished") ?? parts.find((p) => p.status !== "finished") ?? parts[parts.length - 1];
}

/** `shows` as the anime they belong to, in the order their first show comes in `shows`. */
export function animeOf(shows: ShowCard[]): AnimeCard[] {
  const by = new Map<number, ShowCard[]>();
  for (const s of shows) {
    const parts = by.get(s.series.id);
    if (parts) parts.push(s);
    else by.set(s.series.id, [s]);
  }
  return [...by.entries()].map(([id, list]) => {
    const parts = [...list].sort((a, b) => a.series.order - b.series.order || a.id - b.id);
    const one = parts.length === 1;
    return { id, title: one ? parts[0].title : parts[0].series.title, native: one ? parts[0].titleNative : parts[0].series.native, parts, current: currentOf(parts) };
  });
}

/** What kind of part a show is, as the anime's page names it. */
export function kindOf(s: ShowCard): string {
  if (s.movie) return "Movie";
  switch (s.format) {
    case "OVA":
      return "OVA";
    case "SPECIAL":
    case "":
      return "Special";
    case "TV_SHORT":
      return "TV short";
    default:
      return s.format.replace("_", " ");
  }
}

/** "3 seasons · 2 movies · 1 OVA". */
export function partsText(parts: ShowCard[]): string {
  const count = (kind: (s: ShowCard) => boolean) => parts.filter(kind).length;
  const movies = count((s) => s.movie);
  const ovas = count((s) => !s.movie && s.format === "OVA");
  const specials = count((s) => !s.movie && (s.format === "SPECIAL" || s.format === ""));
  const seasons = parts.length - movies - ovas - specials;
  return [seasons ? plural(seasons, "season") : "", movies ? plural(movies, "movie") : "", ovas ? plural(ovas, "OVA") : "", specials ? plural(specials, "special") : ""].filter(Boolean).join(" · ");
}

/** Whether an anime is being watched: a part of it is, or you've finished a season and its next one waits. */
export function watching(a: AnimeCard): boolean {
  if (a.parts.some(inProgress)) return true;
  const seasons = a.parts.filter((p) => !p.movie);
  const finished = seasons.findLastIndex((p) => p.status === "finished");
  return finished >= 0 && finished < seasons.length - 1;
}

export const lastWatched = (a: AnimeCard) => Math.max(0, ...a.parts.map((p) => p.lastWatchedAt ?? 0));

/** The two lines under an anime's name: what's next, and which part it's in. */
export function animeStatus(a: AnimeCard): { text: string; sub: string; ok?: boolean } {
  const st = statusOf(a.current);
  if (a.parts.length === 1) return st;
  if (a.parts.every((p) => p.status === "finished")) return { text: "Finished", sub: partsText(a.parts), ok: true };
  // "Movie" names the part's kind where a season has its next episode: the title says it better.
  const text = a.current.movie && !a.current.next?.resume ? "Next: Movie" : st.text;
  return { text, sub: [a.current.title, a.current.next?.resume || !a.current.next?.ready ? st.sub : ""].filter(Boolean).join(" · ") };
}

/** The uses of words across an anime's parts, and how many are of words you know. */
export function wordsOf(a: AnimeCard): { uses: number; known: number } {
  return a.parts.reduce((sum, p) => ({ uses: sum.uses + p.wordsUses, known: sum.known + p.wordsKnown }), { uses: 0, known: 0 });
}
