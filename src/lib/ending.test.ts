import { describe, expect, it } from "vitest";
import { afterCredits } from "./ending";

const chapter = (start: number, end: number, title: string) => ({ start, end, title });

describe("what comes after an episode's ending", () => {
  it("is nothing when the credits run to the end, or the ending isn't known", () => {
    expect(afterCredits({ start: 1300, end: 1420 }, 1420, [])).toBeNull();
    expect(afterCredits({ start: 1300, end: 1415 }, 1420, [])).toBeNull();
    expect(afterCredits(null, 1420, [])).toBeNull();
    expect(afterCredits({ start: 1300, end: 1390 }, 0, [])).toBeNull();
  });

  it("is a preview when a chapter says so, or it's as short as one", () => {
    expect(afterCredits({ start: 1300, end: 1390 }, 1420, [])).toEqual({ at: 1390, preview: true });
    expect(afterCredits({ start: 1200, end: 1290 }, 1420, [chapter(1290, 1420, "Next Episode Preview")])).toEqual({ at: 1290, preview: true });
    expect(afterCredits({ start: 1200, end: 1290 }, 1420, [chapter(1290, 1420, "次回予告")])).toEqual({ at: 1290, preview: true });
  });

  it("is a scene when it's longer than a preview, or its chapter is named as one", () => {
    expect(afterCredits({ start: 1200, end: 1290 }, 1420, [])).toEqual({ at: 1290, preview: false });
    expect(afterCredits({ start: 1300, end: 1395 }, 1420, [chapter(1395, 1420, "Epilogue")])).toEqual({ at: 1395, preview: false });
    // A chapter with no name of its own says nothing either way.
    expect(afterCredits({ start: 1300, end: 1395 }, 1420, [chapter(1395, 1420, "Chapter 05")])).toEqual({ at: 1395, preview: true });
  });
});
