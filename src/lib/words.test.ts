import { describe, expect, it } from "vitest";
import { inWord, stepCharacter, stepWord, wordsOf } from "./words";

/** A dictionary of these words: the longest one the text starts with. */
const dictionary = (...known: string[]) => async (rest: string) => Math.max(0, ...known.filter((w) => rest.startsWith(w)).map((w) => w.length));

describe("wordsOf", () => {
  it("takes the longest word at each place, and leaves punctuation out", async () => {
    const text = "「景色が、きれいだ」";
    const words = await wordsOf(text, dictionary("景色", "景", "が", "きれい", "きれいだ"));
    expect(words.map((w) => text.slice(w.at, w.at + w.length))).toEqual(["景色", "が", "きれいだ"]);
  });

  it("makes a character no dictionary knows a word by itself", async () => {
    const text = "猫𠮷x\n犬";
    const words = await wordsOf(text, dictionary("猫"));
    expect(words.map((w) => text.slice(w.at, w.at + w.length))).toEqual(["猫", "𠮷", "x", "犬"]);
  });

  it("never runs past the line", async () => {
    expect(await wordsOf("犬", async () => 9)).toEqual([{ at: 0, length: 1 }]);
  });
});

describe("stepWord", () => {
  const words = [{ at: 1, length: 2 }, { at: 3, length: 1 }, { at: 5, length: 4 }];

  it("steps by word and stops at the line's ends", () => {
    expect(stepWord(words, 1, 1)).toEqual(words[1]);
    expect(stepWord(words, 2, 1)).toEqual(words[1]);
    expect(stepWord(words, 3, -1)).toEqual(words[0]);
    expect(stepWord(words, 1, -1)).toEqual(words[0]);
    expect(stepWord(words, 6, 1)).toEqual(words[2]);
  });

  it("steps from between two words to the one on that side", () => {
    expect(stepWord(words, 4, 1)).toEqual(words[2]);
    expect(stepWord(words, 4, -1)).toEqual(words[1]);
    expect(stepWord(words, 0, 0)).toEqual(words[0]);
    expect(stepWord(words, 99, 0)).toEqual(words[2]);
    expect(stepWord([], 0, 1)).toBeNull();
  });
});

describe("stepCharacter", () => {
  it("skips punctuation, and stays when there's nothing that way", () => {
    const text = "猫、犬。";
    expect(stepCharacter(text, 0, 1)).toBe(2);
    expect(stepCharacter(text, 2, -1)).toBe(0);
    expect(stepCharacter(text, 2, 1)).toBe(2);
    expect(stepCharacter(text, 0, -1)).toBe(0);
  });

  it("steps over a character that takes two", () => {
    const text = "𠮷猫";
    expect(stepCharacter(text, 0, 1)).toBe(2);
    expect(stepCharacter(text, 2, -1)).toBe(0);
    expect(inWord(text, 0)).toBe(true);
  });
});
