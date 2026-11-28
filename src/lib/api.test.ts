import { describe, expect, it } from "vitest";
import { errorText } from "./api";

describe("errorText", () => {
  it("says what went wrong, however it came", () => {
    expect(errorText(new Error("Telly can’t reach the PC"))).toBe("Telly can’t reach the PC");
    expect(errorText("That episode isn’t in Telly any more")).toBe("That episode isn’t in Telly any more");
    // An Android command's rejection: { message, code }, which once showed as [object Object].
    expect(errorText({ message: "AnkiDroid has no note type called kiku_typed.", code: null })).toBe("AnkiDroid has no note type called kiku_typed.");
    expect(errorText({ code: "NOT_FOUND" })).toBe('{"code":"NOT_FOUND"}');
  });
});
