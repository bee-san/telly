import { describe, expect, it } from "vitest";
import { bytes, clock, plural } from "./format";

describe("how Telly writes times, sizes and counts", () => {
  it("writes a time as a clock", () => {
    expect(clock(0)).toBe("0:00");
    expect(clock(751)).toBe("12:31");
    expect(clock(3723)).toBe("1:02:03");
  });

  it("counts one thing and several", () => {
    expect(plural(1, "episode")).toBe("1 episode");
    expect(plural(3, "episode")).toBe("3 episodes");
    expect(plural(2, "library", "libraries")).toBe("2 libraries");
  });

  it("writes a size in the unit that suits it", () => {
    expect(bytes(0)).toMatch(/^0/);
    expect(bytes(1536 * 1024 * 1024)).toMatch(/GB$/);
    expect(bytes(350 * 1024 * 1024)).toMatch(/MB$/);
  });
});
