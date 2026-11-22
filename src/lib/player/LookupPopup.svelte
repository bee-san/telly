<script lang="ts">
  // What a tapped word means, on the phone (lookup.svelte.ts): each word the
  // tap starts, the longest first, in LFG's dictionary popup
  // (lib/dictionary-popup), whose + adds a card. A dictionary that couldn't be
  // installed is said under them.
  import { onMount } from "svelte";
  import { createDictionaryPopup, type DictionaryPopup } from "$lib/dictionary-popup/core";
  import { dictionaryStyles, progress, type Found } from "$lib/lookup.svelte";

  let { found, error, onadd }: { found: Found[]; error: string; onadd: (f: Found) => void } = $props();

  /** One entry per word and reading: a word the deinflector found twice is shown once. */
  const entries = $derived(found.filter((f, i) => found.findIndex((g) => g.term.expression === f.term.expression && g.term.reading === f.term.reading) === i));

  let host = $state<HTMLDivElement | null>(null);
  let popup: DictionaryPopup | null = null;
  /** The height the entries need: the popup is as tall as them, up to its limit. */
  let height = $state(0);

  function show() {
    popup?.render(entries, { styles: dictionaryStyles.list }).catch(() => {});
  }

  onMount(() => {
    if (!host) return;
    const colour = (name: string) => getComputedStyle(host!).getPropertyValue(name).trim();
    popup = createDictionaryPopup(host, {
      // Served from Telly's own pages: its Content Security Policy runs no inline script.
      src: "/dictionary-popup/index.html",
      title: "What it means",
      theme: "dark",
      lang: "ja",
      fontSize: 15,
      css: ".entries { padding: 8px 12px 8px 10px; }",
      colors: { bg: colour("--vid-panel"), fg: colour("--vid-ink"), secondary: colour("--vid-ink-2"), border: colour("--vid-edge"), hoverBg: colour("--vid-hover"), accent: colour("--mark"), onAccent: colour("--mark-ink") },
      onAddCard: (result) => {
        const f = entries[(result as unknown as { index: number }).index];
        if (f) onadd(f);
      },
      onHeight: (px) => (height = px),
    });
    show();
    return () => popup?.destroy();
  });

  $effect(show);
</script>

<div class="popup" role="dialog" aria-label="What it means">
  {#if error}
    <p class="note">{error}</p>
  {:else if !entries.length}
    <p class="note">
      {#if progress.installing}Your dictionaries are being installed ({progress.installing}).{:else if !progress.count}No dictionaries yet: Sync on the Library gets yours from the PC.{:else}No word starts here.{/if}
    </p>
  {/if}
  <div class="entries" bind:this={host} style:height="{height}px" hidden={!!error || !entries.length}></div>
  {#if progress.failed && !error}<p class="note">{progress.failed}</p>{/if}
</div>

<style>
  .popup { position: absolute; left: 50%; top: 8px; transform: translateX(-50%); width: min(560px, calc(100% - 24px)); max-height: 56%; display: flex; flex-direction: column; overflow: hidden; background: var(--vid-panel); color: var(--vid-ink); border: 1px solid var(--vid-edge); border-radius: 12px; z-index: 30; box-shadow: 0 18px 40px -16px rgba(0, 0, 0, .8); }
  /* As tall as its entries, and no taller than the popup may be: past that the entries scroll inside. */
  .entries { flex: 0 1 auto; min-height: 0; }
  .entries[hidden] { display: none; }
  .note { flex: none; color: var(--vid-ink-2); font-size: 14px; margin: 10px 14px; }
</style>
