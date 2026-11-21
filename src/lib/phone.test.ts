import { describe, expect, it } from "vitest";
import { nextToGet, type PcEpisode, type PcLibrary } from "./phone";

const ep = (id: number, over: Partial<PcEpisode["episode"]> = {}, onPhone: PcEpisode["onPhone"] = "no"): PcEpisode => ({
  episode: { id, number: id % 100, duration: 1400, watchedAt: null, position: 0, ...over },
  ready: true,
  rip: 1,
  phone: { state: "none", progress: 0, error: "", size: 0 },
  onPhone,
});
const show = (id: number, lastWatchedAt: number | null, episodes: PcEpisode[]) => ({ show: { id, lastWatchedAt } as PcLibrary["shows"][number]["show"], episodes });

describe("nextToGet", () => {
  it("gets the next three episodes not watched of the shows you're part-way through, the last watched first", () => {
    const lib: PcLibrary = {
      name: "PC",
      free: 0,
      shows: [
        // Not started: left alone.
        show(1, null, [ep(101), ep(102)]),
        // Two watched, the next on the phone already, the one after being got.
        show(2, 50, [ep(201, { watchedAt: 1 }), ep(202, { watchedAt: 2 }), ep(203, {}, "ready"), ep(204, {}, "getting"), ep(205), ep(206)]),
        // Only started, and watched more lately.
        show(3, 90, [ep(301, { position: 400 }), ep(302), ep(303), ep(304)]),
        // Finished.
        show(4, 70, [ep(401, { watchedAt: 1 })]),
      ],
    };
    expect(nextToGet(lib)).toEqual([301, 302, 303, 205]);
  });
});
