<script lang="ts">
  // What a tapped word means, on the phone (lookup.svelte.ts): each word the
  // tap starts, the longest first, with what your dictionaries say, and Add card.
  // A dictionary that couldn't be installed is said under them.
  import { glossaryHtml, progress, type Found } from "$lib/lookup.svelte";

  let {
    found,
    error,
    adding,
    onadd,
  }: { found: Found[]; error: string; adding: string; onadd: (f: Found) => void } = $props();

  /** One entry per word and reading: a word the deinflector found twice is shown once. */
  const entries = $derived(found.filter((f, i) => found.findIndex((g) => g.term.expression === f.term.expression && g.term.reading === f.term.reading) === i));
</script>

<div class="popup" role="dialog" aria-label="What it means" lang="ja">
  {#if error}
    <p class="note">{error}</p>
  {:else if !entries.length}
    <p class="note" lang="en">
      {#if progress.installing}Your dictionaries are being installed ({progress.installing}).{:else if !progress.count}No dictionaries yet: Sync on the Library gets yours from the PC.{:else}No word starts here.{/if}
    </p>
  {/if}
  {#each entries as f, i (f.term.expression + f.term.reading + i)}
    <article>
      <header>
        <span class="word">{f.term.expression}</span>
        {#if f.term.reading && f.term.reading !== f.term.expression}<span class="reading">{f.term.reading}</span>{/if}
        {#if f.deinflected !== f.matched}<span class="from" lang="en">from {f.matched}</span>{/if}
        <button type="button" class="add" disabled={!!adding} onclick={() => onadd(f)} title="Add a card for this word" lang="en">{adding === f.term.expression ? "Adding…" : "Add card"}</button>
      </header>
      {#each f.term.glossaries as g, j (g.dictionary + j)}
        <section>
          <div class="dict" lang="en">{g.dictionary}{#if g.definitionTags} · {g.definitionTags}{/if}</div>
          <div class="gloss" lang="en">{@html glossaryHtml(g)}</div>
        </section>
      {/each}
    </article>
  {/each}
  {#if progress.failed && !error}<p class="note" lang="en">{progress.failed}</p>{/if}
</div>

<style>
  .popup { position: absolute; left: 50%; top: 8px; transform: translateX(-50%); width: min(560px, calc(100% - 24px)); max-height: 56%; overflow-y: auto; background: var(--vid-panel); color: var(--vid-ink); border: 1px solid var(--vid-edge); border-radius: 12px; padding: 4px 14px 10px; z-index: 30; box-shadow: 0 18px 40px -16px rgba(0, 0, 0, .8); }
  article { padding: 10px 0; border-bottom: 1px solid var(--vid-edge); }
  article:last-child { border-bottom: 0; }
  header { display: flex; align-items: baseline; flex-wrap: wrap; gap: 4px 10px; }
  .word { font: 700 24px/1.2 var(--font-jp); }
  .reading { font: 500 16px var(--font-jp); color: var(--vid-ink-2); }
  .from { font-size: 12px; color: var(--vid-ink-2); }
  .add { margin-left: auto; border: 0; border-radius: 8px; background: var(--mark); color: var(--mark-ink); font: 600 14px var(--font-ui); padding: 7px 12px; }
  .add:disabled { opacity: .6; }
  .dict { font-size: 11px; color: var(--vid-ink-2); margin-top: 6px; text-transform: none; }
  .gloss { font: 400 15px/1.45 var(--font-ui); }
  .gloss :global(ul), .gloss :global(ol) { margin: 2px 0; padding-left: 18px; }
  .gloss :global([data-sc="examples"]), .gloss :global([data-sc="example-sentence"]) { color: var(--vid-ink-2); font-size: 13px; font-family: var(--font-jp); }
  .note { color: var(--vid-ink-2); font-size: 14px; margin: 10px 0; }
</style>
