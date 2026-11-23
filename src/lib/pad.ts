// A controller on Telly for Android: which buttons there are, and where the
// D-pad moves to among what's on the page. Listening for them is pad.svelte.ts.

/** A controller's buttons, as Android's side names them (Pad.kt). The left stick pushed a way is a button too. */
export type PadButton =
  | "up" | "down" | "left" | "right"
  | "stick-up" | "stick-down" | "stick-left" | "stick-right"
  | "a" | "b" | "x" | "y" | "l1" | "r1" | "l2" | "r2" | "l3" | "r3" | "start" | "select";

export type Way = "up" | "down" | "left" | "right";

/** A button going down, or still held (`repeat`). */
export interface Press {
  button: PadButton;
  repeat: boolean;
}

/** The way a press of the D-pad or the left stick points, if it's one of them. */
export function wayOf(button: PadButton): Way | null {
  const way = button.replace("stick-", "");
  return way === "up" || way === "down" || way === "left" || way === "right" ? way : null;
}

export interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/**
 * Which of `boxes` a press of `way` moves to from `from`: the nearest that way,
 * one in line with `from` before one off to the side. -1 when nothing is that way.
 */
export function nextBox(from: Box, boxes: Box[], way: Way): number {
  const across = way === "left" || way === "right";
  // Along the way pressed: [near edge, far edge]; and across it.
  const along = (b: Box): [number, number] => (way === "right" ? [b.left, b.right] : way === "left" ? [-b.right, -b.left] : way === "down" ? [b.top, b.bottom] : [-b.bottom, -b.top]);
  const side = (b: Box): [number, number] => (across ? [b.top, b.bottom] : [b.left, b.right]);
  const [fromNear, fromFar] = along(from);
  const [fromA, fromB] = side(from);
  let best = -1;
  let bestScore = Infinity;
  boxes.forEach((b, i) => {
    const [near, far] = along(b);
    // That way means its middle is past this one's, and it ends past this one's end.
    if ((near + far) / 2 <= (fromNear + fromFar) / 2 + 0.5 || far <= fromFar + 0.5) return;
    const [a, z] = side(b);
    const gap = Math.max(0, near - fromFar);
    const off = Math.max(0, a - fromB, fromA - z);
    const middles = Math.abs((a + z) / 2 - (fromA + fromB) / 2);
    const score = gap + off * 4 + middles / 1000;
    if (score < bestScore) {
      best = i;
      bestScore = score;
    }
  });
  return best;
}

/** Which of `boxes` is nearest the top left of the page: where the D-pad starts. */
export function firstBox(boxes: Box[]): number {
  let best = -1;
  let bestScore = Infinity;
  boxes.forEach((b, i) => {
    const score = Math.max(0, b.top) * 4 + Math.max(0, b.left) + (b.bottom <= 0 || b.right <= 0 ? 1e6 : 0);
    if (score < bestScore) {
      best = i;
      bestScore = score;
    }
  });
  return best;
}
