/** A subtitle line as HTML: its furigana as ruby, its breaks as <br>, and the characters' names in it marked. */
import type { CastName, Line } from "./api";

/** The names to look for, by their first character, longest first. */
export type Names = Map<string, { chars: string[]; name: CastName; kana: boolean }[]>;

function esc(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c] as string);
}

/** Katakana and its long-vowel mark: a katakana name only counts when it isn’t part of a longer katakana word. */
const isKatakana = (c: string | undefined) => !!c && /[ァ-ヺー]/.test(c);

/** A cast's names, set out for finding them in lines. A name of one character is too easily another word. */
export function namesIndex(cast: CastName[]): Names {
  const map: Names = new Map();
  for (const n of cast) {
    const chars = [...n.term];
    if (chars.length < 2) continue;
    const list = map.get(chars[0]) ?? [];
    list.push({ chars, name: n, kana: chars.every(isKatakana) });
    map.set(chars[0], list);
  }
  for (const list of map.values()) list.sort((a, b) => b.chars.length - a.chars.length);
  return map;
}

/** Where characters' names are in a line: start → (end, who). A name never splits a furigana base. */
function nameMarks(chars: string[], ruby: [number, number, string][], names: Names): Map<number, { end: number; name: CastName }> {
  const marks = new Map<number, { end: number; name: CastName }>();
  if (!names.size) return marks;
  const crossesRuby = (a: number, b: number) => ruby.some(([at, len]) => (a > at && a < at + len) || (b > at && b < at + len));
  for (let i = 0; i < chars.length; i++) {
    const found = (names.get(chars[i]) ?? []).find(
      (n) =>
        n.chars.every((c, k) => chars[i + k] === c) &&
        !crossesRuby(i, i + n.chars.length) &&
        !(n.kana && (isKatakana(chars[i - 1]) || isKatakana(chars[i + n.chars.length]))),
    );
    if (!found) continue;
    marks.set(i, { end: i + found.chars.length, name: found.name });
    i += found.chars.length - 1;
  }
  return marks;
}

/** A line as HTML, with every character of its text and furigana escaped: it's set with {@html}. */
export function lineHtml(l: Line, names: Names): string {
  const chars = [...l.text];
  const ruby = new Map<number, [number, string]>((l.ruby ?? []).map(([at, len, r]) => [at, [len, r]]));
  const marks = nameMarks(chars, l.ruby ?? [], names);
  let out = "";
  let closeAt = -1;
  for (let i = 0; i < chars.length; ) {
    const m = marks.get(i);
    if (m) {
      out += `<span class="nm" title="${esc(m.name.name)}">`;
      closeAt = m.end;
    }
    const r = ruby.get(i);
    if (r && r[0] > 0) {
      out += `<ruby>${esc(chars.slice(i, i + r[0]).join(""))}<rt>${esc(r[1])}</rt></ruby>`;
      i += r[0];
    } else {
      out += chars[i] === "\n" ? "<br>" : esc(chars[i]);
      i++;
    }
    if (closeAt >= 0 && i >= closeAt) {
      out += "</span>";
      closeAt = -1;
    }
  }
  if (closeAt >= 0) out += "</span>";
  return out;
}
