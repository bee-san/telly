import { describe, expect, it } from "vitest";
import { SUB_SIZES, step } from "./sub-look";

describe("step", () => {
  it("goes to the next size up or down", () => {
    expect(step(SUB_SIZES, 36, 1)).toBe(40);
    expect(step(SUB_SIZES, 36, -1)).toBe(32);
  });

  it("stops at either end", () => {
    expect(step(SUB_SIZES, 64, 1)).toBe(64);
    expect(step(SUB_SIZES, 20, -1)).toBe(20);
  });

  it("takes a size between two steps to the nearer one in that direction", () => {
    expect(step(SUB_SIZES, 38, 1)).toBe(40);
    expect(step(SUB_SIZES, 38, -1)).toBe(36);
  });
});
