// dictionary-popup: LFG's dictionary popup (the Yomitan-style entries with
// furigana headwords, tags, frequencies, pitch accent and an add-to-Anki
// button) as something any web page can show.
//
// The popup itself is the page in ../popup (base.css, renderer.js,
// kanji-renderer.js), kept as LFG has it. This wraps it in an iframe, so its
// styles and your page's never meet, and gives it a small API.
//
// This is the whole of it but the inline copy of the page: import
// "dictionary-popup/core" and pass `src` (or `html`) to leave that copy out of
// your bundle; "dictionary-popup" itself fills `html` in for you.
// SPDX-License-Identifier: GPL-3.0-or-later

/** Colours for the two themes; any of them can be overridden with `colors`. */
const THEMES = {
  light: {
    bg: "#ffffff",
    fg: "#1f1f1f",
    secondary: "#757575",
    accent: "#2ecc71",
    onAccent: "#ffffff",
    border: "rgba(0, 0, 0, 0.08)",
    hoverBg: "rgba(0, 0, 0, 0.04)",
    tabBg: "#f3f3f3",
  },
  dark: {
    bg: "#1c1b1f",
    fg: "#e6e1e5",
    secondary: "#a9a4ad",
    accent: "#2ecc71",
    onAccent: "#0b2815",
    border: "rgba(255, 255, 255, 0.10)",
    hoverBg: "rgba(255, 255, 255, 0.07)",
    tabBg: "#252429",
  },
};

const COLOR_VARIABLES = {
  bg: "--bg",
  fg: "--fg",
  secondary: "--secondary",
  accent: "--accent",
  onAccent: "--on-accent",
  border: "--border",
  hoverBg: "--hover-bg",
  tabBg: "--tab-bg",
};

function tags(value) {
  return String(value ?? "")
    .split(/\s+/)
    .filter(Boolean);
}

function frequencyText(items) {
  return items
    .map((item) => item.displayValue || (item.value > 0 ? String(item.value) : ""))
    .filter(Boolean)
    .join(", ");
}

/** The harmonic mean of each dictionary's first rank, as LFG shows it. */
function harmonicRank(frequencies) {
  const seen = new Set();
  const values = [];
  for (const group of frequencies) {
    if (seen.has(group.dictName)) continue;
    seen.add(group.dictName);
    const first = group.frequencies.find((item) => item.value > 0);
    if (first) values.push(first.value);
  }
  if (!values.length) return undefined;
  return Math.round(values.length / values.reduce((sum, value) => sum + 1 / value, 0));
}

/**
 * One lookup result in the shape the renderer reads. It takes what Hoshidicts
 * gives (through its C interface, its wasm build or LFG's JNI) with either
 * spelling of a field: `dictName` or `dictionary`, `process` or `trace`,
 * `pitchPositions` or `pitches: [{position, nasal, devoice}]`.
 */
export function normalizeResult(result, index = 0) {
  if (result.kanji) return { ...result, index };
  const term = result.term ?? {};
  const glossaries = (term.glossaries ?? []).map((g) => ({
    dictName: g.dictName ?? g.dictionary ?? "",
    glossary: g.glossary ?? "",
    definitionTags: g.definitionTags ?? "",
    definitionTagList: g.definitionTagList ?? tags(g.definitionTags),
    termTags: g.termTags ?? "",
  }));
  const frequencies = (term.frequencies ?? []).map((group) => {
    const items = (group.frequencies ?? []).map((item) => ({ value: Number(item.value) || 0, displayValue: item.displayValue ?? "" }));
    return { dictName: group.dictName ?? group.dictionary ?? "", displayValueText: group.displayValueText ?? frequencyText(items), frequencies: items };
  });
  const pitches = (term.pitches ?? []).map((group) => {
    const marks = Array.isArray(group.pitches) ? group.pitches : [];
    const unique = (list) => [...new Set(list)];
    return {
      dictName: group.dictName ?? group.dictionary ?? "",
      pitchPositions: group.pitchPositions ?? unique(marks.map((p) => p.position)),
      nasalPositions: group.nasalPositions ?? unique(marks.flatMap((p) => p.nasal ?? [])),
      devoicePositions: group.devoicePositions ?? unique(marks.flatMap((p) => p.devoice ?? [])),
    };
  });
  const termTags = term.termTags ?? [...new Set(glossaries.flatMap((g) => tags(g.termTags)))].join(" ");
  return {
    index,
    matched: result.matched ?? term.expression ?? "",
    deinflected: result.deinflected ?? result.matched ?? "",
    process: (result.process ?? result.trace ?? []).map((step) => ({ name: step.name ?? "", description: step.description ?? "" })),
    term: {
      expression: term.expression ?? result.matched ?? "",
      reading: term.reading ?? "",
      rules: term.rules ?? "",
      ruleTags: term.ruleTags ?? [...new Set(tags(term.rules))],
      termTags,
      glossaries,
      frequencies,
      frequencyHarmonicRank: term.frequencyHarmonicRank ?? harmonicRank(frequencies),
      frequencyAverageRank: term.frequencyAverageRank,
      pitches,
    },
  };
}

/**
 * Puts a dictionary popup in `container` and returns its handle.
 *
 * Options:
 *   src         URL of the packaged popup page (popup/index.html, served
 *               from your own origin). Leave it out and the page is written
 *               inline, which needs no files but does need a Content Security
 *               Policy that lets inline scripts run.
 *   html        the popup's page as text, written inline when there's no `src`
 *               ("dictionary-popup" passes its own copy).
 *   theme       "dark" or "light" (default "light").
 *   colors      {bg, fg, secondary, accent, onAccent, border, hoverBg, tabBg} over the theme's.
 *   fontSize    in px (default 14).
 *   lang        the language of the entries, e.g. "ja".
 *   css         extra CSS for the popup page.
 *   onAddCard(result, {dictionary, selection})   the + button; without it there's no button.
 *   onOpenCard(result, {dictionary, selection})  the book button of a word already added.
 *   onHeight(px)  the height the entries need, whenever it changes.
 *   bridges     objects to put on the popup's window as they are (LFG's
 *               WordAudioBridge, say), for hosts that speak the renderer's own bridges.
 */
export function createDictionaryPopup(container, options = {}) {
  if (!options.src && !options.html) throw new Error("dictionary-popup/core needs `src` (where popup/index.html is served) or `html`");
  const iframe = document.createElement("iframe");
  iframe.className = "dictionary-popup";
  iframe.title = options.title ?? "Dictionary";
  iframe.style.cssText = "display:block;width:100%;height:100%;border:0;background:transparent;color-scheme:normal";

  let win = null;
  let shown = [];
  let observer = null;
  let destroyed = false;

  const ready = new Promise((resolve, reject) => {
    iframe.addEventListener(
      "load",
      () => {
        try {
          win = iframe.contentWindow;
          if (!win || !win.DictionaryRenderer) throw new Error("the dictionary popup's page didn't load its renderer");
          setUp();
          resolve();
        } catch (e) {
          reject(e);
        }
      },
      { once: true },
    );
  });
  // A page that never renders shouldn't leave an unhandled rejection behind; render() still reports it.
  ready.catch(() => {});

  if (options.src) iframe.src = options.src;
  else iframe.srcdoc = options.html;
  container.appendChild(iframe);

  function setUp() {
    const doc = win.document;
    applyTheme(options);
    if (!options.onOpenCard) {
      // The book button opens a card that's already added: without a way to, it isn't shown.
      const style = doc.createElement("style");
      style.textContent = ".anki-open-btn { display: none !important; }";
      doc.head.appendChild(style);
    }
    if (options.css) {
      const style = doc.createElement("style");
      style.id = "dictionary-popup-custom-css";
      style.textContent = options.css;
      doc.head.appendChild(style);
    }
    const pick = (handler) => (index, _glossary, dictionary, selection) => {
      const result = shown[Number(index)];
      if (result && handler) handler(result, { dictionary: dictionary || "", selection: selection || "" });
    };
    win.AnkiBridge = { addToAnki: pick(options.onAddCard), openInAnki: pick(options.onOpenCard) };
    for (const [name, bridge] of Object.entries(options.bridges ?? {})) win[name] = bridge;
    if (options.onHeight && win.ResizeObserver) {
      // The body's own height: the page's scroll height never falls below the iframe's.
      observer = new win.ResizeObserver(() => options.onHeight(Math.ceil(doc.body.getBoundingClientRect().height)));
      observer.observe(doc.body);
    }
  }

  function applyTheme({ theme = "light", colors = {}, fontSize, lang }) {
    const root = win.document.documentElement;
    const dark = theme === "dark";
    root.dataset.theme = dark ? "dark" : "light";
    root.style.colorScheme = dark ? "dark" : "light";
    const all = { ...THEMES[dark ? "dark" : "light"], ...colors };
    for (const [name, variable] of Object.entries(COLOR_VARIABLES)) root.style.setProperty(variable, all[name]);
    root.style.setProperty("--pronunciation-annotation-color", all.fg);
    if (fontSize) root.style.setProperty("--font-size-no-units", String(fontSize));
    if (lang) root.lang = lang;
  }

  return {
    /** The iframe the popup is in. */
    iframe,
    /** Resolves once the popup's page is up. */
    ready,

    /**
     * Shows `results` (see normalizeResult for their shape).
     *   styles       [{dictName, styles}], each dictionary's own CSS, scoped to its entries.
     *   added        expressions already in Anki: their + is a green tick.
     *   placeholder  what to say when there are no results.
     *   payload      anything else the renderer's payload takes (showPitchDiagram,
     *                dictionaryCollapseMode, mediaDataUris, …).
     */
    async render(results, { styles = [], added = [], placeholder = "", payload = {} } = {}) {
      await ready;
      if (destroyed) return;
      shown = results.map(normalizeResult);
      win.DictionaryRenderer.render({
        results: shown,
        styles: styles.map((s) => ({ dictName: s.dictName ?? s.dictionary ?? "", styles: s.styles ?? "" })),
        existingExpressions: [...added],
        ankiEnabled: !!options.onAddCard,
        ankiDupAction: "allow",
        isDark: options.theme === "dark",
        placeholder,
        groupTerms: true,
        wordAudioEnabled: !!options.bridges?.WordAudioBridge,
        showNavigationButtons: false,
        showFrequencyHarmonic: false,
        ...payload,
      });
    },

    /** Marks these expressions as in Anki (a green tick, and the book button if onOpenCard is given). */
    async setAdded(expressions) {
      await ready;
      if (!destroyed) win.DictionaryRenderer.updateAnkiStatus([...expressions]);
    },

    /** Changes the theme, colours, font size or language. */
    async setTheme(theme) {
      await ready;
      if (destroyed) return;
      Object.assign(options, theme);
      applyTheme(options);
    },

    /** Empties the popup. */
    async clear() {
      await ready;
      if (destroyed) return;
      shown = [];
      win.DictionaryRenderer.clear();
    },

    /** Takes the popup out of the page. */
    destroy() {
      destroyed = true;
      observer?.disconnect();
      iframe.remove();
    },
  };
}
