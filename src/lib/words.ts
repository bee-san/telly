// The words of a subtitle line, for a controller's cursor to step across:
// found by asking your dictionaries how long the word starting at each place is.

/** A word of a line: where it starts and how long it is, in the line's text. */
export interface Word {
  at: number;
  length: number;
}

/** Spaces, line breaks and punctuation: they belong to no word. */
const BETWEEN = /[\s\p{P}\p{S}]/u;

/** Whether the character at `at` can be looked up: it's there, and it isn't a space or punctuation. */
export function inWord(text: string, at: number): boolean {
  return at >= 0 && at < text.length && !BETWEEN.test(String.fromCodePoint(text.codePointAt(at) ?? 32));
}

/**
 * The words of `text`, in order. `longest` says how long the word that the
 * rest of the line starts with is (0 when no word starts there: that
 * character is a word by itself).
 */
export async function wordsOf(text: string, longest: (rest: string) => Promise<number>): Promise<Word[]> {
  const words: Word[] = [];
  for (let at = 0; at < text.length; ) {
    const size = String.fromCodePoint(text.codePointAt(at) ?? 32).length;
    if (!inWord(text, at)) {
      at += size;
      continue;
    }
    const length = Math.min(Math.max(await longest(text.slice(at)), size), text.length - at);
    words.push({ at, length });
    at += length;
  }
  return words;
}

/** The word `by` words on from the one `at` is in (or the nearest to it), staying within the line. */
export function stepWord(words: Word[], at: number, by: number): Word | null {
  if (!words.length) return null;
  let here = words.findIndex((w) => at < w.at + w.length);
  // Past the last word (on the 。 after it), a step back goes to the last word.
  if (here < 0) here = words.length;
  // Between two words, a step back goes to the one before, a step on to the one after.
  else if (at < words[here].at && by > 0) here -= 1;
  return words[Math.min(words.length - 1, Math.max(0, here + by))];
}

/** The place `by` characters on from `at` that can be looked up, or `at` when there's none that way. */
export function stepCharacter(text: string, at: number, by: number): number {
  for (let next = at + by; next >= 0 && next < text.length; next += by) {
    // The second half of a character that takes two is no place to start.
    if (next > 0 && text.charCodeAt(next) >= 0xdc00 && text.charCodeAt(next) <= 0xdfff) continue;
    if (inWord(text, next)) return next;
  }
  return at;
}
