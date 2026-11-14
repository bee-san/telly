// What the search in the title bar finds: shows in your library, lines in
// their subtitles, and anime on AniList to download. The drop-down under the
// bar and the page of every result (routes/search) both ask through here.
import { call, type FoundLine, type LineSearch, type Pick, type ShowCard } from "./api";

/** Whether a show's titles (AniList's, and the anime it's listed under) hold `q`. */
function titled(s: ShowCard, q: string): boolean {
  const t = q.toLowerCase();
  return [s.title, s.titleNative, s.titleEnglish, s.titleRomaji, s.series.title, s.series.native].some((title) => title.toLowerCase().includes(t));
}

/** The shows in `shows` that `q` names, by any title or the reading of its Japanese one, the one watched last first. */
export async function findShows(shows: ShowCard[], q: string): Promise<ShowCard[]> {
  const read = new Set(await call<number[]>("searchShows", { q }).catch(() => []));
  return shows.filter((s) => titled(s, q) || read.has(s.id)).sort((a, b) => (b.lastWatchedAt ?? 0) - (a.lastWatchedAt ?? 0));
}

/** The lines `q` finds, and how many each show has (search.rs). */
export const findLines = (q: string) => call<LineSearch>("searchLines", { q }).catch((): LineSearch => ({ lines: [], shows: {} }));

/** What AniList finds for `q` that isn't in your library. */
export async function findOnAniList(q: string): Promise<Pick[]> {
  return (await call<Pick[]>("searchPicks", { q })).filter((p) => p.show === null);
}

/** A line you can read sooner is lower: one that's said, then the fewest other words you don't know
 *  (none is i+1), then those whose episode's words aren't saved yet. */
export const reach = (l: FoundLine) => (l.said ? 0 : 2_000_000) + (l.unknown ?? 1_000_000);

export interface LineGroup {
  show: ShowCard;
  lines: FoundLine[];
  /** How many lines the show has: `lines` has only the best of them. */
  total: number;
}

/** The lines found, show by show. A show with a line you can read comes first, and so do such lines
 *  in it; among equals, the show watched last comes first and lines are in the show's order. */
export function groupLines(found: LineSearch, shows: ShowCard[]): LineGroup[] {
  const of = new Map<number, FoundLine[]>();
  for (const l of found.lines) {
    const list = of.get(l.showId);
    if (list) list.push(l);
    else of.set(l.showId, [l]);
  }
  return shows
    .filter((show) => of.has(show.id))
    .map((show) => {
      const lines = of.get(show.id)!.sort((a, b) => reach(a) - reach(b) || a.number - b.number || a.start - b.start);
      return { show, lines, total: Math.max(found.shows[show.id] ?? 0, lines.length) };
    })
    .sort((a, b) => reach(a.lines[0]) - reach(b.lines[0]) || (b.show.lastWatchedAt ?? 0) - (a.show.lastWatchedAt ?? 0));
}

/** Where Download… goes for an anime on AniList: nyaa.si's search for it. */
export const downloadLink = (p: Pick) => `/downloads?anilist=${p.id}&q=${encodeURIComponent(p.romaji || p.english || p.title)}`;

const FORMATS: Record<string, string> = { TV: "TV", TV_SHORT: "TV short", MOVIE: "Movie", SPECIAL: "Special", OVA: "OVA", ONA: "ONA", MUSIC: "Music" };

/** "Movie · 2016", "TV · 2011 · 148 episodes". */
export function pickMeta(p: Pick): string {
  const episodes = p.format !== "MOVIE" && p.episodes ? `${p.episodes} ${p.episodes === 1 ? "episode" : "episodes"}` : "";
  return [FORMATS[p.format] ?? p.format, p.year ? String(p.year) : "", episodes].filter(Boolean).join(" · ");
}
