<script lang="ts">
  // What a tapped word means, on the phone (lookup.svelte.ts): each word the
  // tap starts, the longest first, in Hachidori's own popup
  // (lib/dictionary-popup), as the PC shows a word. Its + adds a card, and a
  // word tapped in a definition is looked up in its place. A dictionary that
  // couldn't be installed is said under it.
  import { onMount, untrack } from "svelte";
  import type { DictionaryPopup } from "$lib/dictionary-popup";
  import { dictionaryStyles, lookUp, progress, type Found } from "$lib/lookup.svelte";

  let { found, error, onadd }: { found: Found[]; error: string; onadd: (f: Found) => void } = $props();

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
    if (said) popup?.notice(said);
    else popup?.render(entries, { styles: untrack(() => dictionaryStyles.list) });
  }

  onMount(() => {
    let gone = false;
    // Loaded when a word is first tapped: the popup is Hachidori's whole renderer, which the PC's player never needs.
    import("$lib/dictionary-popup").then(({ createDictionaryPopup }) => {
      if (gone || !host) return;
      // Hachidori's own colours, which are dark: the player is dark whatever Telly's theme is.
      popup = createDictionaryPopup<Found>(host, {
        theme: "default",
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
</script>

<div class="popup" role="dialog" aria-label="What it means">
  <div class="entries" bind:this={host}></div>
  {#if progress.failed && !error}<p class="note">{progress.failed}</p>{/if}
</div>

<style>
  .popup { position: absolute; left: 50%; top: 8px; transform: translateX(-50%); width: min(560px, calc(100% - 24px)); max-height: 56%; display: flex; flex-direction: column; gap: 6px; z-index: 30; }
  /* Hachidori's popup draws its own box. It's as tall as its entries, and no taller than this may be: past that the definitions scroll inside. */
  .entries { flex: 0 1 auto; min-height: 0; display: flex; flex-direction: column; }
  .note { flex: none; margin: 0; padding: 6px 12px; border-radius: 10px; background: rgba(39, 38, 48, .92); color: var(--vid-ink-2); font-size: 14px; }
</style>
