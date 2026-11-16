/** What an episode has after its ending, from where the ending stops (the rip's chapters, or AniSkip) and the episode's length. */

export interface AfterCredits {
  /** Where it starts, in seconds: where the ending stops. */
  at: number;
  /** A preview of the next episode, not a scene of this one. */
  preview: boolean;
}

/** Less than this after the ending is a logo or a card, not something to watch. */
const LEAST_S = 8;
/** A preview of the next episode runs about half a minute; a scene after the credits, longer. */
const PREVIEW_S = 40;

/**
 * What comes after the ending, if anything does. A chapter there named as a preview (予告, Next
 * Episode) is one; with no name to go by, so is anything as short as a preview.
 */
export function afterCredits(
  ending: { start: number; end: number } | null | undefined,
  duration: number,
  chapters: { start: number; end: number; title: string }[],
): AfterCredits | null {
  if (!ending || !(duration > 0) || duration - ending.end < LEAST_S) return null;
  const chapter = chapters.find((c) => c.start >= ending.end - 1 && c.start < duration);
  const title = chapter?.title.trim() ?? "";
  const named = /preview|next|予告|次回/i.test(title);
  // A chapter named something else (Part C, Epilogue) is a scene, however short.
  const scene = !!title && !named && !/^chapter\s*\d+$/i.test(title);
  return { at: ending.end, preview: named || (!scene && duration - ending.end <= PREVIEW_S) };
}
