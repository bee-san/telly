/** The sizes the subtitles can be, in pixels, smallest first. */
export const SUB_SIZES = [20, 24, 28, 32, 36, 40, 44, 50, 56, 64];

/** How far above their usual place the subtitles can sit, in percent of the picture's height, lowest first. */
export const SUB_LIFTS = [0, 5, 10, 15, 20, 30, 40, 50, 60, 70];

/** The size or place one step along from `now` in `steps` (up when `by` is 1, down when -1), or `now` at the end. */
export function step(steps: number[], now: number, by: 1 | -1): number {
  const at = steps.findIndex((n) => n >= now);
  const i = at < 0 ? steps.length : at;
  // A value between two steps goes to the next one either way.
  const from = by > 0 ? (steps[i] === now ? i : i - 1) : i;
  return steps[Math.min(steps.length - 1, Math.max(0, from + by))];
}

/** What a place says, in Settings. */
export const liftText = (n: number) => (n === 0 ? "Where they usually are" : `${n}% of the picture higher`);
