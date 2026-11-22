// @ts-nocheck: plain JavaScript; src/index.d.ts says what it exports.
// dictionary-popup: LFG's dictionary popup for any web page. A looked-up word
// as LFG (bee-san/LearnFromGames) shows it: the headword with its reading over
// it, its pitch drawn beside it, round buttons to hear it, note it and add its
// card, how common it is, the inflections undone to find it, then each
// dictionary's definitions in that dictionary's own styles.
//
// It draws in a shadow root, so its styles and your page's never meet, and it
// runs no script of its own in the page: nothing a Content Security Policy
// minds but the inline styles dictionaries use.
// SPDX-License-Identifier: GPL-3.0-or-later
import { BASE_CSS } from "./base-css.js";
import { CARD_STATES, listBody, scope, toEntry } from "./entry-html.js";
import { colours, paletteOf } from "./palette.js";

export { CARD_STATES, body, escape, furigana, glossary, graph, listBody, path, scope, toEntry } from "./entry-html.js";
export { PALETTES, colours, paletteOf } from "./palette.js";
export { KINDS, accents, frequencyRank, morae, shortName } from "./pronunciation.js";

/**
 * LFG's page styles, for a shadow root: what it sets on `html` goes on the
 * host, and what it sets on `body` on the list inside.
 */
export function hostCss() {
  return (
    BASE_CSS.replace(/^html\{/m, ":host{display:block;overflow-y:auto;overscroll-behavior:contain;").replace(/^body\{/m, ".lfg-list{") +
    // What LFG's WebView does by itself: a button looks pressable, and its focus shows.
    ".lfg-button{cursor:pointer;text-decoration:none}.lfg-button:focus-visible{outline:2px solid var(--accent);outline-offset:2px}" +
    ".lfg-card[data-state=adding] .lfg-i-adding{animation:lfg-turn 1s linear infinite}@keyframes lfg-turn{to{transform:rotate(360deg)}}"
  );
}

/**
 * Puts a dictionary popup in `container` and returns its handle.
 *
 * Options:
 *   theme       "dark" (default) or "light".
 *   scheme      LFG's colour scheme: "pink" (default), "mono", "amber", "teal" or "purple".
 *   colors      any of {bg, surface, border, borderStrong, text, dim, accent, soft, ok, okSoft, chip, freqName, freqNameText} over the scheme's.
 *   fontSize    in px (default 16).
 *   css         extra CSS for the popup.
 *   onAddCard(result, index)     the card button of a word not added yet. Without it there's no card button.
 *   onOpenCard(result, index)    the card button of a word that's in Anki (its state "in-anki").
 *   onPlayAudio(result, index)   the audio button. Without it there's no audio button.
 *   hasAudio(result)             whether a word has audio to play (default: every word).
 *   onNote(result, index)        the note button. Without it there's no note button.
 *   mediaUrl(dictionary, path)   a URL for a dictionary's picture. Without it pictures are left out.
 */
export function createDictionaryPopup(container, options = {}) {
  const host = document.createElement("div");
  host.className = "dictionary-popup";
  const root = host.attachShadow({ mode: "open" });
  const theme = document.createElement("style");
  const base = document.createElement("style");
  const dictionaries = document.createElement("style");
  const custom = document.createElement("style");
  const list = document.createElement("div");
  list.className = "lfg-list";
  root.append(theme, base, dictionaries, custom, list);
  base.textContent = hostCss();

  let shown = [];
  let states = [];
  let styled = "";
  // Scoping a stylesheet is most of the time a lookup takes to show (a grammar dictionary's is 48 KB), and stylesheets rarely change.
  const scoped = new Map();

  function applyTheme() {
    theme.textContent = colours(paletteOf(options)) + (options.fontSize ? `:host{font-size:${Number(options.fontSize) || 16}px}` : "");
    custom.textContent = options.css ?? "";
  }
  applyTheme();

  function paintStates() {
    states.forEach((state, i) => {
      const button = root.getElementById(`c${i}`);
      if (!button) return;
      button.dataset.state = state;
      button.setAttribute("aria-label", CARD_STATES[state]);
      button.title = CARD_STATES[state];
    });
  }

  function act(target) {
    const button = target.closest?.("[data-action]");
    if (!button) return false;
    const index = Number(button.dataset.index);
    const result = shown[index];
    if (!result) return false;
    if (button.dataset.action === "audio") options.onPlayAudio?.(result, index);
    else if (button.dataset.action === "note") options.onNote?.(result, index);
    else if (states[index] === "add") options.onAddCard?.(result, index);
    else if (states[index] === "in-anki") options.onOpenCard?.(result, index);
    return true;
  }
  root.addEventListener("click", (e) => {
    if (act(e.target)) e.preventDefault();
  });
  root.addEventListener("keydown", (e) => {
    if ((e.key === "Enter" || e.key === " ") && act(e.target)) e.preventDefault();
  });

  container.appendChild(host);

  return {
    /** The element the popup is in: size it, or its container, as you like. It scrolls when its entries are taller than it. */
    element: host,

    /**
     * Shows `results` (see toEntry for their shape), one under the other.
     *   styles   [{dictionary, styles}], each dictionary's own CSS, limited to its entries.
     *   states   each result's card state ("add", "adding", "in-anki", "waiting"); "add" where it's left out.
     *   added    expressions already in Anki, as a shorter way of saying "in-anki".
     */
    render(results, { styles = [], states: given = [], added = [] } = {}) {
      const key = styles.map((s) => `${s.dictionary ?? s.dictName}\u0000${s.styles ?? s.css ?? ""}`).join("\u0001");
      if (key !== styled) {
        styled = key;
        dictionaries.textContent = styles
          .map((s) => {
            const [name, css] = [s.dictionary ?? s.dictName ?? "", s.styles ?? s.css ?? ""];
            const id = `${name}\u0000${css}`;
            if (!scoped.has(id)) scoped.set(id, scope(css, name));
            return scoped.get(id);
          })
          .join("");
      }
      shown = [...results];
      const entries = shown.map(toEntry);
      const inAnki = new Set(added);
      states = entries.map((entry, i) => given[i] ?? (inAnki.has(entry.expression) ? "in-anki" : "add"));
      list.innerHTML = listBody(entries, {
        audio: options.onPlayAudio ? (entry) => (options.hasAudio ? !!options.hasAudio(shown[entries.indexOf(entry)]) : true) : null,
        notes: !!options.onNote,
        card: !!options.onAddCard,
        mediaUrl: options.mediaUrl ?? null,
      });
      paintStates();
      host.scrollTop = 0;
    },

    /** Sets one result's card state: "add", "adding", "in-anki" or "waiting". */
    setCardState(index, state) {
      if (index < 0 || index >= states.length || !(state in CARD_STATES)) return;
      states[index] = state;
      paintStates();
    },

    /** Marks these expressions as in Anki: their card button is a green tick. */
    setAdded(expressions) {
      const inAnki = new Set(expressions);
      shown.forEach((result, i) => {
        if (inAnki.has(toEntry(result).expression)) states[i] = "in-anki";
      });
      paintStates();
    },

    /** Marks the entry a controller or keyboard is on (its headword takes the accent colour), and scrolls to it if asked. */
    setCurrent(index, { scroll = false } = {}) {
      root.querySelectorAll(".lfg-entry").forEach((entry, i) => entry.toggleAttribute("data-current", i === index));
      if (scroll) root.getElementById(`e${index}`)?.scrollIntoView({ block: "nearest" });
    },

    /** Changes the theme, scheme, colours, font size or extra CSS. */
    setTheme(change) {
      Object.assign(options, change);
      applyTheme();
    },

    /** Empties the popup. */
    clear() {
      shown = [];
      states = [];
      list.innerHTML = "";
    },

    /** Takes the popup out of the page. */
    destroy() {
      host.remove();
    },
  };
}
