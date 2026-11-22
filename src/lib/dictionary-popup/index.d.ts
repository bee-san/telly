/** One glossary of a word: which dictionary, and what it says (Yomitan's structured content as JSON, or a JSON array of strings). */
export interface Glossary {
  dictionary?: string;
  /** The same as `dictionary`, as LFG's JNI spells it. */
  dictName?: string;
  glossary: string;
  definitionTags?: string;
  termTags?: string;
}

export interface FrequencyGroup {
  dictionary?: string;
  dictName?: string;
  /** Under either name. */
  frequencies?: { value: number; displayValue?: string }[];
  values?: { value: number; display?: string }[];
}

export interface PitchGroup {
  dictionary?: string;
  dictName?: string;
  /** Each pitch as Hoshidicts gives it, under either name, or the positions alone. */
  pitches?: { position: number; pattern?: string }[];
  values?: { position: number; pattern?: string }[];
  pitchPositions?: number[];
}

/** One lookup result, as Hoshidicts gives it. */
export interface LookupResult {
  /** The text it was found as. */
  matched: string;
  deinflected?: string;
  /** The inflections undone to get from `matched` to the word, under either name. */
  trace?: { name: string; description?: string }[];
  process?: { name: string; description?: string }[];
  term: {
    expression: string;
    reading?: string;
    /** The word's parts of speech, space-separated ("v1 vt"): a verb's or adjective's accent is 起伏. */
    rules?: string;
    glossaries: Glossary[];
    frequencies?: FrequencyGroup[];
    pitches?: PitchGroup[];
  };
}

/** A result as LFG's entry: what `toEntry` makes and the HTML functions take. */
export interface Entry {
  matched: string;
  deinflected: string;
  trace: { name: string; description: string }[];
  expression: string;
  reading: string;
  rules: string;
  glossaries: { dictionary: string; glossary: string; definitionTags: string; termTags: string }[];
  frequencies: { dictionary: string; values: { value: number; display: string }[] }[];
  pitches: { dictionary: string; values: { position: number; pattern?: string }[] }[];
}

export type Theme = "dark" | "light";
export type Scheme = "pink" | "mono" | "amber" | "teal" | "purple";
export type CardState = "add" | "adding" | "in-anki" | "waiting";
export type PitchKind = "heiban" | "atamadaka" | "nakadaka" | "odaka" | "kifuku";

export interface Colors {
  bg: string;
  surface: string;
  border: string;
  borderStrong: string;
  text: string;
  dim: string;
  accent: string;
  soft: string;
  ok: string;
  okSoft: string;
  chip: string;
  freqName: string;
  freqNameText: string;
}

export interface Look {
  theme?: Theme;
  scheme?: Scheme;
  colors?: Partial<Colors>;
  /** In px (default 16). */
  fontSize?: number;
  /** Extra CSS for the popup. */
  css?: string;
}

export interface PopupOptions<R extends LookupResult = LookupResult> extends Look {
  /** The card button of a word not added yet. Without it there's no card button. */
  onAddCard?: (result: R, index: number) => void;
  /** The card button of a word that's in Anki. */
  onOpenCard?: (result: R, index: number) => void;
  /** The audio button. Without it there's no audio button. */
  onPlayAudio?: (result: R, index: number) => void;
  /** Whether a word has audio to play (default: every word). */
  hasAudio?: (result: R) => boolean;
  /** The note button. Without it there's no note button. */
  onNote?: (result: R, index: number) => void;
  /** A URL for a dictionary's picture. Without it pictures are left out. */
  mediaUrl?: (dictionary: string, path: string) => string;
}

export interface DictionaryStyle {
  dictionary?: string;
  dictName?: string;
  styles?: string;
  css?: string;
}

export interface RenderOptions {
  /** Each dictionary's own CSS, limited to its entries. */
  styles?: DictionaryStyle[];
  /** Each result's card state; "add" where it's left out. */
  states?: (CardState | undefined)[];
  /** Expressions already in Anki, as a shorter way of saying "in-anki". */
  added?: Iterable<string>;
}

export interface DictionaryPopup<R extends LookupResult = LookupResult> {
  /** The element the popup is in. It scrolls when its entries are taller than it. */
  element: HTMLDivElement;
  render(results: R[], options?: RenderOptions): void;
  setCardState(index: number, state: CardState): void;
  setAdded(expressions: Iterable<string>): void;
  setCurrent(index: number, options?: { scroll?: boolean }): void;
  setTheme(look: Look): void;
  clear(): void;
  destroy(): void;
}

export function createDictionaryPopup<R extends LookupResult = LookupResult>(container: HTMLElement, options?: PopupOptions<R>): DictionaryPopup<R>;

export interface Accent {
  position: number;
  kind: PitchKind;
  label: string;
  morae: string[];
  high: boolean[];
}

export interface BodyOptions {
  index?: number | null;
  audio?: ((entry: Entry) => boolean) | null;
  notes?: boolean;
  card?: boolean;
  mediaUrl?: ((dictionary: string, path: string) => string) | null;
}

export const CARD_STATES: Record<CardState, string>;
export const KINDS: Record<PitchKind, string>;
export const PALETTES: Record<Theme, Record<Scheme, Colors>>;
export function toEntry(result: LookupResult): Entry;
export function body(entry: Entry, options?: BodyOptions): string;
export function listBody(entries: Entry[], options?: Omit<BodyOptions, "index">): string;
export function glossary(json: string, dictionary: string, mediaUrl?: ((dictionary: string, path: string) => string) | null): string;
export function furigana(expression: string, reading: string): string;
export function graph(accent: Accent): string;
export function path(entry: Entry): string;
export function scope(css: string, dictionary: string): string;
export function escape(text: string): string;
export function accents(entry: Entry): Accent[];
export function morae(reading: string): string[];
export function frequencyRank(entry: Entry): number | null;
export function shortName(dictionary: string): string;
export function paletteOf(look?: Look): Colors & { dark: boolean };
export function colours(palette: Colors & { dark: boolean }): string;
export function hostCss(): string;
