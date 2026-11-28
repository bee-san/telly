import { describe, expect, it } from "vitest";
import { furigana } from "./phone-cards";

describe("furigana", () => {
  it("sits over the kanji, as Hachidori's cards have it", () => {
    expect(furigana("学校", "がっこう")).toBe("学校[がっこう]");
    expect(furigana("食べる", "たべる")).toBe("食[た]べる");
    expect(furigana("お茶", "おちゃ")).toBe("お 茶[ちゃ]");
    expect(furigana("取り込む", "とりこむ")).toBe("取り込[とりこ]む");
    expect(furigana("見かける", "みかける")).toBe("見[み]かける");
  });

  it("leaves a word without kanji, or without a reading, as it is", () => {
    expect(furigana("ラーメン", "らーめん")).toBe("ラーメン");
    expect(furigana("食べる", "")).toBe("食べる");
    expect(furigana("ダメ", "だめ")).toBe("ダメ");
  });
});
