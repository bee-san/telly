import { describe, expect, it } from "vitest";
import type { CastName, Line } from "./api";
import { lineHtml, namesIndex } from "./subtitle-html";

const line = (text: string, ruby?: [number, number, string][]): Line => ({ start: 0, end: 1, text, ruby });
const cast = (...terms: string[]): CastName[] => terms.map((term) => ({ term, name: `${term} (a character)`, role: "MAIN" }));
const none = namesIndex([]);

describe("a subtitle line as HTML", () => {
  it("escapes everything a subtitle file could carry", () => {
    expect(lineHtml(line(`<img src=x onerror="alert(1)"> & more`), none)).toBe("&lt;img src=x onerror=&quot;alert(1)&quot;&gt; &amp; more");
    // Furigana and its base too.
    expect(lineHtml(line("<b>", [[0, 3, "<i>"]]), none)).toBe("<ruby>&lt;b&gt;<rt>&lt;i&gt;</rt></ruby>");
  });

  it("puts furigana over its base and breaks lines", () => {
    expect(lineHtml(line("学校へ\n行く", [[0, 2, "がっこう"], [4, 1, "い"]]), none)).toBe("<ruby>学校<rt>がっこう</rt></ruby>へ<br><ruby>行<rt>い</rt></ruby>く");
  });

  it("marks a character's name, the longest that fits", () => {
    const names = namesIndex(cast("ナツ", "ナツ・ドラグニル"));
    expect(lineHtml(line("ナツ・ドラグニルだ"), names)).toBe('<span class="nm" title="ナツ・ドラグニル (a character)">ナツ・ドラグニル</span>だ');
    expect(lineHtml(line("おいナツ！"), names)).toBe('おい<span class="nm" title="ナツ (a character)">ナツ</span>！');
  });

  it("escapes a name's title", () => {
    const names = namesIndex([{ term: "ハク", name: `Haku "the dragon" <3`, role: "MAIN" }]);
    expect(lineHtml(line("ハク"), names)).toBe('<span class="nm" title="Haku &quot;the dragon&quot; &lt;3">ハク</span>');
  });

  it("doesn't take part of a longer katakana word, or half a furigana base, for a name", () => {
    const names = namesIndex(cast("アイ", "千尋"));
    // アイス is ice cream, not アイ.
    expect(lineHtml(line("アイスが食べたい"), names)).toBe("アイスが食べたい");
    expect(lineHtml(line("アイが来た"), names)).toContain('<span class="nm"');
    // 千尋 with one reading over both characters is marked whole; a base that starts inside the name isn't split.
    expect(lineHtml(line("千尋だ", [[0, 2, "ちひろ"]]), names)).toBe('<span class="nm" title="千尋 (a character)"><ruby>千尋<rt>ちひろ</rt></ruby></span>だ');
    expect(lineHtml(line("千尋常", [[1, 2, "じんじょう"]]), names)).toBe("千<ruby>尋常<rt>じんじょう</rt></ruby>");
  });

  it("leaves names of one character alone", () => {
    expect(namesIndex(cast("L")).size).toBe(0);
  });
});
