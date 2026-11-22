<script lang="ts">
  // What a tapped word means, on the phone (lookup.svelte.ts): each word the
  // tap starts, the longest first, in Hachidori's own popup
  // (lib/dictionary-popup), as the PC shows a word. Its + adds a card, and a
  // word tapped in a definition is looked up in its place. A dictionary that
  // couldn't be installed is said under it. It takes the room above the line
  // being read (`under`), which the player keeps small at the bottom meanwhile.
  import { onMount, untrack } from "svelte";
  import type { DictionaryPopup } from "$lib/dictionary-popup";
  import { dictionaryStyles, lookUp, progress, type Found } from "$lib/lookup.svelte";

  let { found, error, onadd, under = null }: { found: Found[]; error: string; onadd: (f: Found) => void; under?: HTMLElement | null } = $props();

  /** One entry per word and reading: a word the deinflector found twice is shown once. */
  function once(list: Found[]): Found[] {
    return list.filter((f, i) => list.findIndex((g) => g.term.expression === f.term.expression && g.term.reading === f.term.reading) === i);
  }
  const entries = $derived(once(found));
  /** What the popup says when there's no entry to show. */
  const said = $derived(
    error ||
      (entries.length
        ? ""
        : progress.installing
          ? `Your dictionaries are being installed (${progress.installing}).`
          : !progress.count
            ? "No dictionaries yet: Sync on the Library gets yours from the PC."
            : "No word starts here."),
  );

  let host = $state<HTMLDivElement | null>(null);
  let popup: DictionaryPopup<Found> | null = null;

  /** Shows the words tapped. Not again for a lookup inside the popup, which changes the styles kept but not the words. */
  function show() {
    // Read even while the popup is still loading, so the next words looked up are shown too.
    const words = entries;
    if (said) popup?.notice(said);
    else popup?.render(words, { styles: untrack(() => dictionaryStyles.list) });
  }

  onMount(() => {
    let gone = false;
    // Loaded when a word is first tapped: the popup is Hachidori's whole renderer, which the PC's player never needs.
    import("$lib/dictionary-popup").then(({ createDictionaryPopup }) => {
      if (gone || !host) return;
      // Hachidori's own colours, which are dark: the player is dark whatever Telly's theme is.
      popup = createDictionaryPopup<Found>(host, {
        theme: "default",
        // Nearly solid, where Hachidori's is 85%: over a page that's enough, but the player's own words showed through.
        opacity: 97,
        onAddCard: (f) => onadd(f),
        lookup: async (text) => ({ results: once(await lookUp(text)), styles: dictionaryStyles.list }),
      });
      show();
    });
    return () => {
      gone = true;
      popup?.destroy();
    };
  });

  $effect(show);

  let box = $state<HTMLDivElement | null>(null);
  /** How tall the popup may be, in px (0 until it's measured), and whether it goes over the line for want of room above it. */
  let room = $state(0);
  let over = $state(false);
  /** The least height an entry can be read in. */
  const LEAST = 190;

  // The room between the top of the player and the line, kept as either changes size.
  $effect(() => {
    const [popupBox, line] = [box, under];
    const player = popupBox?.offsetParent;
    if (!popupBox || !player) return;
    const measure = () => {
      const top = player.getBoundingClientRect().top + 8;
      const above = line && line.offsetHeight > 0 ? line.getBoundingClientRect().top - 8 - top : 0;
      over = above < LEAST;
      room = Math.round(over ? player.clientHeight - 16 : above);
    };
    const watch = new ResizeObserver(measure);
    watch.observe(player);
    if (line) watch.observe(line);
    measure();
    return () => watch.disconnect();
  });

  /** The entry a controller is on. */
  let current = 0;
  $effect(() => {
    void entries;
    current = 0;
  });

  /** A controller's L1 and R1: on to the entry before or after. */
  export function step(by: number) {
    current = Math.min(entries.length - 1, Math.max(0, current + by));
    popup?.setCurrent(current);
  }

  /** A controller's A: adds the card of the entry it's on. */
  export function add() {
    if (entries[current]) onadd(entries[current]);
  }

  /** A controller's right stick: scrolls the definitions. */
  export function scroll(by: number) {
    const inside = popup?.element.shadowRoot?.querySelectorAll<HTMLElement>("*") ?? [];
    [...inside].find((el) => el.scrollHeight > el.clientHeight + 1 && /auto|scroll/.test(getComputedStyle(el).overflowY))?.scrollBy(0, by);
  }

  /** A controller's B: back from a word looked up in a definition. False when there's nothing to go back to. */
  export function back(): boolean {
    return popup?.back() ?? false;
  }
</script>

<div class="popup" class:over role="dialog" aria-label="What it means" bind:this={box} style:max-height={room ? `${room}px` : null}>
  <div class="entries" bind:this={host}></div>
  {#if progress.failed && !error}<p class="note">{progress.failed}</p>{/if}
</div>

<style>
  /* Above the line being read (z-index 31), and as tall as the room there. Over it only when that's too little to read an entry in. */
  .popup { position: absolute; left: 50%; top: 8px; transform: translateX(-50%); width: min(560px, calc(100% - 24px)); max-height: calc(100% - 16px); display: flex; flex-direction: column; gap: 6px; z-index: 30; }
  .popup.over { z-index: 32; }
  /* Hachidori's popup draws its own box. It's as tall as its entries, and no taller than this may be: past that the definitions scroll inside. */
  .entries { flex: 0 1 auto; min-height: 0; display: flex; flex-direction: column; }
  .note { flex: none; margin: 0; padding: 6px 12px; border-radius: 10px; background: rgba(39, 38, 48, .92); color: var(--vid-ink-2); font-size: 14px; }
</style>
