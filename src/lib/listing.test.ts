import { describe, expect, it } from "vitest";
import type { Settings, ShowCard } from "./api";
import { applied, listed, listingOf, statusOf } from "./listing";
import { animeOf } from "./series";

let next = 1;
/** A season or film in the library, `watched` of its `episodes` watched, under the anime `anime`. */
function part(title: string, anime: number, episodes: number, watched: number): ShowCard {
  const status = watched >= episodes ? "finished" : watched > 0 ? "watching" : "new";
  return {
    id: next++,
    title,
    titleNative: "",
    movie: false,
    episodes,
    watched,
    status,
    next: null,
    cards: 0,
    wordsUses: 0,
    wordsKnown: 0,
    addedAt: 0,
    lastWatchedAt: watched ? 1 : null,
    tmdbId: null,
    library: 0,
    series: { id: anime, title, native: "", order: next, parts: 1, next: null },
  } as unknown as ShowCard;
}

const library = () =>
  animeOf([
    part("Frieren", 1, 28, 28),
    part("Overlord", 2, 13, 13),
    part("Overlord II", 2, 13, 4),
    part("Bocchi the Rock!", 3, 12, 0),
    part("Cowboy Bebop", 4, 26, 26),
    part("Cowboy Bebop: The Movie", 4, 1, 1),
  ]);
const titles = (settings: Settings) => listed(library(), listingOf(settings, []), []).map((a) => a.title).sort();

describe("the Library's Filter and the anime you've finished", () => {
  it("counts an anime as finished once every part of it you have is", () => {
    const status = Object.fromEntries(library().map((a) => [a.title, statusOf(a)]));
    expect(status).toEqual({ Frieren: "finished", Overlord: "watching", "Bocchi the Rock!": "new", "Cowboy Bebop": "finished" });
  });

  it("lists every anime until something is left out", () => {
    expect(titles({})).toEqual(["Bocchi the Rock!", "Cowboy Bebop", "Frieren", "Overlord"]);
    expect(applied(listingOf({}, []))).toBe(0);
  });

  it("leaves out the finished ones when Finished is unticked, and nothing else", () => {
    const settings = { "library.withoutStatus": ["finished"] };
    // Overlord stays: its first season is finished, but its second isn't.
    expect(titles(settings)).toEqual(["Bocchi the Rock!", "Overlord"]);
    expect(applied(listingOf(settings, []))).toBe(1);
  });

  it("can list only the finished ones", () => {
    expect(titles({ "library.withoutStatus": ["new", "watching"] })).toEqual(["Cowboy Bebop", "Frieren"]);
  });
});
