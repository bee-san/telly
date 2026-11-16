import { describe, expect, it } from "vitest";
import { arrange, filters, toggle } from "./index";

describe("a chain of filters", () => {
  it("keeps the filters there are, each once, in the order picked", () => {
    expect(arrange(["denoise", "none", "denoise", "a filter long gone", 7, "restore"])).toEqual(["denoise", "restore"]);
  });

  it("has at most one television, and it comes last", () => {
    const tvs = filters.map((f) => f.id).filter((id) => id.startsWith("crt-"));
    expect(tvs.length).toBeGreaterThan(1);
    expect(arrange([tvs[0], "denoise", tvs[1], "restore"])).toEqual(["denoise", "restore", tvs[1]]);
  });

  it("takes a filter out when its tile is clicked again, and None clears it", () => {
    expect(toggle(["denoise", "restore"], "denoise")).toEqual(["restore"]);
    expect(toggle(["denoise"], "restore")).toEqual(["denoise", "restore"]);
    expect(toggle(["denoise", "restore"], "none")).toEqual([]);
  });

  it("puts a new filter before the television", () => {
    const tv = filters.map((f) => f.id).find((id) => id.startsWith("crt-"))!;
    expect(toggle(["denoise", tv], "restore")).toEqual(["denoise", "restore", tv]);
  });
});
