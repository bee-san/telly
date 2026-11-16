// Which of the Library's anime are listed, and in what order: what its Filter chooses, kept in Settings.
import type { Settings, ShowCard } from "./api";
import { inProgress, lastWatched, wordsOf, type AnimeCard } from "./series";

export type Kind = "all" | "series" | "movies";
/** How far you are with an anime: nothing of it watched, some of it, or all of it. */
export type Status = "new" | "watching" | "finished";

/** The share of an anime's words you know, in per cent; null when none of its episodes is scored yet. */
export const knownOf = (a: AnimeCard) => {
  const words = wordsOf(a);
  return words.uses ? (100 * words.known) / words.uses : null;
};
/** When an anime's newest part was added. */
export const addedOf = (a: AnimeCard) => Math.max(...a.parts.map((p) => p.addedAt));

const sum = (a: AnimeCard, of: (p: ShowCard) => number) => a.parts.reduce((n, p) => n + of(p), 0);
const episodesOf = (a: AnimeCard) => sum(a, (p) => p.episodes);
/** The share of an anime's episodes you've watched; null when it has none. */
const watchedOf = (a: AnimeCard) => (episodesOf(a) ? sum(a, (p) => p.watched) / episodesOf(a) : null);

/** An order the Library can be listed in. */
export interface Sort {
  id: string;
  label: string;
  /** What its two ways round are called, the usual one first. */
  orders: [string, string];
  /** What it goes by, the smallest first the usual way round; an anime with none (null) goes last either way round. Without it, by name alone. */
  by?: (a: AnimeCard) => number | null;
}

/** `of`, for an order that has the largest first. */
const most = (of: (a: AnimeCard) => number | null) => (a: AnimeCard) => {
  const n = of(a);
  return n === null ? null : -n;
};

/** The orders, as the Filter lists them; the first is the usual one. */
export const SORTS: Sort[] = [
  // The anime whose newest part was added last comes first.
  { id: "added", label: "Recently added", orders: ["Newest", "Oldest"], by: most(addedOf) },
  { id: "watched", label: "Last watched", orders: ["Latest", "Longest ago"], by: most((a) => lastWatched(a) || null) },
  { id: "name", label: "Name", orders: ["A to Z", "Z to A"] },
  // By the year its first part came out.
  { id: "released", label: "Released", orders: ["Newest", "Oldest"], by: most((a) => a.parts[0].year || null) },
  { id: "known", label: "Words known", orders: ["Most", "Least"], by: most(knownOf) },
  { id: "episodes", label: "Episodes", orders: ["Fewest", "Most"], by: episodesOf },
  { id: "progress", label: "Progress", orders: ["Most watched", "Least watched"], by: most(watchedOf) },
  { id: "cards", label: "Cards made", orders: ["Most", "Fewest"], by: most((a) => sum(a, (p) => p.cards)) },
];

/** The statuses, as the Filter lists them. */
export const STATUSES: [Status, string][] = [
  ["new", "Not started"],
  ["watching", "Watching"],
  ["finished", "Finished"],
];

/** The least shares of known words the Filter offers, in per cent; 0 is any. */
export const KNOWN = [0, 60, 70, 80];

/** A library the Filter can leave out: its name ("" for the shows in no library) and how many anime have a part in it. */
export interface LibraryChoice {
  name: string;
  count: number;
}

/** What the Library's Filter is set to. */
export interface Listing {
  sort: Sort;
  /** The other way round from the sort's usual one. */
  reversed: boolean;
  kind: Kind;
  /** The libraries left out, by name ("" for the shows in no library). */
  without: string[];
  /** The statuses left out. */
  withoutStatus: Status[];
  /** The least share of its words you know, in per cent; 0 for any. */
  known: number;
  /** Only anime whose next episode is ready to play. */
  ready: boolean;
}

/** The settings of a Filter that leaves nothing out; its order is left as it is. */
export const NO_FILTERS: Record<string, unknown> = {
  "library.show": "all",
  "library.without": [],
  "library.withoutStatus": [],
  "library.known": 0,
  "library.ready": false,
};

/** How far you are with an anime: started once a part of it is finished or under way, finished once they all are. */
export function statusOf(a: AnimeCard): Status {
  if (a.parts.every((p) => p.status === "finished")) return "finished";
  return a.parts.some((p) => p.status === "finished" || inProgress(p)) ? "watching" : "new";
}

/** Whether an anime can be played now: its next episode is ready, or you've finished it. */
function ready(a: AnimeCard): boolean {
  const next = a.current.next;
  return next ? next.ready : a.current.status === "finished";
}

/** The libraries an anime's parts are in, by name ("" for a part in none); `names` are your libraries'. */
const librariesOf = (a: AnimeCard, names: string[]) => a.parts.map((p) => (p.library === null ? "" : (names[p.library] ?? "")));

/** The libraries with something in them, in Settings' order, then the shows in none: what the Filter offers to leave out. */
export function libraryChoices(names: string[], shows: AnimeCard[]): LibraryChoice[] {
  const count = (name: string) => shows.filter((a) => librariesOf(a, names).includes(name)).length;
  return [...new Set([...names, ""])].map((name) => ({ name, count: count(name) })).filter((c) => c.count > 0);
}

/** What the Filter is set to, from Settings. A library that's no longer among `choices` isn't left out, and none is when there's only one. */
export function listingOf(settings: Settings, choices: LibraryChoice[]): Listing {
  const list = (key: string) => (Array.isArray(settings[key]) ? (settings[key] as unknown[]) : []);
  const offered = choices.length > 1 ? choices.map((c) => c.name) : [];
  return {
    sort: SORTS.find((s) => s.id === settings["library.sort"]) ?? SORTS[0],
    reversed: settings["library.reversed"] === true,
    kind: (["series", "movies"] as unknown[]).includes(settings["library.show"]) ? (settings["library.show"] as Kind) : "all",
    without: offered.filter((name) => list("library.without").includes(name)),
    withoutStatus: STATUSES.map(([status]) => status).filter((status) => list("library.withoutStatus").includes(status)),
    known: KNOWN.find((n) => n === settings["library.known"]) ?? 0,
    ready: settings["library.ready"] === true,
  };
}

/** How many of the Filter's groups leave something out: the number on its button. The order isn't one of them. */
export function applied(l: Listing): number {
  return [l.kind !== "all", l.without.length > 0, l.withoutStatus.length > 0, l.known > 0, l.ready].filter(Boolean).length;
}

/** The anime among `shows` the Filter leaves in, in its order; `names` are your libraries'. */
export function listed(shows: AnimeCard[], l: Listing, names: string[]): AnimeCard[] {
  const known = (a: AnimeCard) => knownOf(a) ?? -1;
  const kept = shows.filter(
    (a) =>
      // One that's only movies (ten Pokémon films) is listed under Movies; one with a season under Series.
      (l.kind === "all" || (l.kind === "movies") === a.parts.every((p) => p.movie)) &&
      // An anime stays while a part of it is in a library that's left in.
      (l.without.length === 0 || librariesOf(a, names).some((name) => !l.without.includes(name))) &&
      !l.withoutStatus.includes(statusOf(a)) &&
      (l.known === 0 || known(a) >= l.known) &&
      (!l.ready || ready(a)),
  );
  const byName = (a: AnimeCard, b: AnimeCard) => a.title.localeCompare(b.title);
  const way = l.reversed ? -1 : 1;
  const by = l.sort.by;
  if (!by) return kept.sort((a, b) => way * byName(a, b));
  return kept.sort((a, b) => {
    const [x, y] = [by(a), by(b)];
    if (x === null || y === null) return (x === null ? 1 : 0) - (y === null ? 1 : 0) || byName(a, b);
    return way * (x - y) || byName(a, b);
  });
}
