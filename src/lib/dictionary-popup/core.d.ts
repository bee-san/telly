/** One glossary of a word: which dictionary, and what it says (Yomitan's structured content as JSON, or a JSON array of strings). */
export interface Glossary {
  dictName?: string;
  /** The same as dictName, as Hoshidicts' wasm build spells it. */
  dictionary?: string;
  glossary: string;
  definitionTags?: string;
  termTags?: string;
}

export interface FrequencyGroup {
  dictName?: string;
  dictionary?: string;
  frequencies: { value: number; displayValue?: string }[];
}

export interface PitchGroup {
  dictName?: string;
  dictionary?: string;
  /** Either the positions themselves… */
  pitchPositions?: number[];
  nasalPositions?: number[];
  devoicePositions?: number[];
  /** …or each pitch as Hoshidicts gives it. */
  pitches?: { position: number; nasal?: number[]; devoice?: number[] }[];
}

/** One lookup result, as Hoshidicts gives it. */
export interface LookupResult {
  /** The text it was found as. */
  matched: string;
  deinflected?: string;
  /** How `matched` became the word: each inflection undone. `trace` is the same under Hoshidicts' other name. */
  process?: { name: string; description?: string }[];
  trace?: { name: string; description?: string }[];
  term: {
    expression: string;
    reading?: string;
    /** The word's parts of speech, space-separated ("v1 vt"). */
    rules?: string;
    termTags?: string;
    glossaries: Glossary[];
    frequencies?: FrequencyGroup[];
    pitches?: PitchGroup[];
  };
}

export interface PopupColors {
  bg: string;
  fg: string;
  secondary: string;
  accent: string;
  onAccent: string;
  border: string;
  hoverBg: string;
  tabBg: string;
}

export interface PopupTheme {
  theme?: "light" | "dark";
  colors?: Partial<PopupColors>;
  /** In px (default 14). */
  fontSize?: number;
  /** The language of the entries, e.g. "ja". */
  lang?: string;
}

export interface CardContext {
  /** The dictionary picked in the entry, or "" for all of them. */
  dictionary: string;
  /** Text selected in the popup when the button was pressed. */
  selection: string;
}

export interface PopupOptions extends PopupTheme {
  /** URL of the packaged popup page (popup/index.html) on your own origin. Without it the page is written inline, which a strict Content Security Policy blocks. */
  src?: string;
  /** The popup's page as text, written inline when there's no `src`. "dictionary-popup" passes its own copy; "dictionary-popup/core" needs this or `src`. */
  html?: string;
  title?: string;
  /** Extra CSS for the popup page. */
  css?: string;
  /** The + button. Without it there's no button. */
  onAddCard?: (result: LookupResult, context: CardContext) => void;
  /** The book button of a word already added. */
  onOpenCard?: (result: LookupResult, context: CardContext) => void;
  /** The height the entries need, whenever it changes. */
  onHeight?: (px: number) => void;
  /** Objects put on the popup's window as they are, for hosts that speak the renderer's own bridges (WordAudioBridge, …). */
  bridges?: Record<string, unknown>;
}

export interface RenderOptions {
  /** Each dictionary's own CSS, scoped to its entries. */
  styles?: { dictName?: string; dictionary?: string; styles: string }[];
  /** Expressions already in Anki: their + is a green tick. */
  added?: Iterable<string>;
  /** What to say when there are no results. */
  placeholder?: string;
  /** Anything else the renderer's payload takes. */
  payload?: Record<string, unknown>;
}

export interface DictionaryPopup {
  iframe: HTMLIFrameElement;
  ready: Promise<void>;
  render(results: LookupResult[], options?: RenderOptions): Promise<void>;
  setAdded(expressions: Iterable<string>): Promise<void>;
  setTheme(theme: PopupTheme): Promise<void>;
  clear(): Promise<void>;
  destroy(): void;
}

export function createDictionaryPopup(container: HTMLElement, options?: PopupOptions): DictionaryPopup;
export function normalizeResult(result: LookupResult, index?: number): Record<string, unknown>;
