// A subtitle line as it's shown on a page: its text, where a touch falls in
// it, and marking the word that's looked up. The player's and the second
// screen's.

/** The text of a subtitle line as it's shown (its furigana and speaker left out), and where each piece of it starts. */
export function lineText(el: HTMLElement): { text: string; nodes: [Text, number][] } {
  const nodes: [Text, number][] = [];
  let text = "";
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (n.parentElement?.closest("rt, rp, .speaker") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  for (let n = walker.nextNode() as Text | null; n; n = walker.nextNode() as Text | null) {
    nodes.push([n, text.length]);
    text += n.data;
  }
  return { text, nodes };
}

/** A subtitle line's text, and where (x, y) falls in it. */
export function textAt(el: HTMLElement, x: number, y: number): { text: string; offset: number; nodes: [Text, number][] } | null {
  const { text, nodes } = lineText(el);
  const caret = document.caretRangeFromPoint?.(x, y);
  const hit = caret && nodes.find(([n]) => n === caret.startContainer);
  if (!hit || !caret) return null;
  // The caret falls between characters: a tap on a character's right half is still that character.
  let offset = hit[1] + caret.startOffset;
  if (caret.startOffset > 0) {
    const r = document.createRange();
    r.setStart(hit[0], caret.startOffset - 1);
    r.setEnd(hit[0], caret.startOffset);
    const box = r.getBoundingClientRect();
    if (x >= box.left && x <= box.right) offset -= 1;
  }
  return offset < text.length ? { text, offset, nodes } : null;
}

/** Marks `length` characters of the tapped line from `offset`, as the word looked up. A page styles the mark with `::highlight(telly-look)`. */
export function markWord(nodes: [Text, number][], offset: number, length: number) {
  const highlights = (globalThis.CSS as any)?.highlights;
  if (!highlights || !length) return;
  const place = (at: number): [Text, number] | null => {
    for (const [n, start] of nodes) if (at <= start + n.data.length) return [n, at - start];
    return null;
  };
  const a = place(offset);
  const b = place(offset + length);
  if (!a || !b) return;
  const range = document.createRange();
  range.setStart(a[0], a[1]);
  range.setEnd(b[0], b[1]);
  highlights.set("telly-look", new (globalThis as any).Highlight(range));
}

/** Takes the mark away. */
export function unmark() {
  (globalThis.CSS as any)?.highlights?.delete("telly-look");
}
