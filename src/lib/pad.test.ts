import { describe, expect, it } from "vitest";
import { firstBox, nextBox, wayOf, type Box } from "./pad";

const box = (left: number, top: number, width: number, height: number): Box => ({ left, top, right: left + width, bottom: top + height });

// A header button on the right, then two rows of a list, each a wide row with Play and Remove at its end.
const header = box(280, 10, 100, 40);
const row1 = box(10, 100, 200, 60);
const play1 = box(220, 110, 70, 40);
const remove1 = box(300, 110, 80, 40);
const row2 = box(10, 170, 200, 60);
const play2 = box(220, 180, 70, 40);
const remove2 = box(300, 180, 80, 40);
const all = [header, row1, play1, remove1, row2, play2, remove2];

describe("nextBox", () => {
  it("goes to the nearest one that way", () => {
    expect(all[nextBox(row1, all, "down")]).toBe(row2);
    expect(all[nextBox(row2, all, "up")]).toBe(row1);
    expect(all[nextBox(row1, all, "right")]).toBe(play1);
    expect(all[nextBox(play1, all, "right")]).toBe(remove1);
    expect(all[nextBox(remove1, all, "left")]).toBe(play1);
  });

  it("keeps to the column it's in before one off to the side", () => {
    expect(all[nextBox(remove1, all, "down")]).toBe(remove2);
    expect(all[nextBox(play2, all, "up")]).toBe(play1);
    expect(all[nextBox(header, all, "down")]).toBe(remove1);
  });

  it("goes off to the side when nothing is in line", () => {
    expect(all[nextBox(row1, all, "up")]).toBe(header);
  });

  it("stays when nothing is that way", () => {
    expect(nextBox(header, all, "up")).toBe(-1);
    expect(nextBox(remove2, all, "right")).toBe(-1);
    expect(nextBox(row2, all, "down")).toBe(-1);
    expect(nextBox(row1, [row1], "down")).toBe(-1);
  });

  it("reaches what's scrolled out of sight", () => {
    const below = box(10, 2000, 200, 60);
    expect(nextBox(row2, [...all, below], "down")).toBe(all.length);
  });
});

describe("firstBox", () => {
  it("starts at the top left of what's in sight", () => {
    expect(all[firstBox(all)]).toBe(header);
    expect(firstBox([box(10, -300, 100, 40), box(10, 20, 100, 40)])).toBe(1);
    expect(firstBox([])).toBe(-1);
  });
});

describe("wayOf", () => {
  it("takes the D-pad and the left stick as ways", () => {
    expect(wayOf("left")).toBe("left");
    expect(wayOf("stick-down")).toBe("down");
    expect(wayOf("a")).toBeNull();
  });
});
