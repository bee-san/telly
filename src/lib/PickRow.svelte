<script lang="ts">
  import { onMount } from "svelte";
  import Cover from "./Cover.svelte";
  import Icon from "./Icon.svelte";
  import type { Pick } from "./api";

  /**
   * A row of picks that scrolls sideways, as the Library's Currently watching does.
   * `ranked` numbers them, for a top ten; `meta` and `line` are the two lines under a
   * pick's title; `onall`, when there is more than the row holds, shows the rest.
   */
  let { title, note = "", picks, ranked = false, meta, line, onpick, onall }: {
    title: string;
    note?: string;
    picks: Pick[];
    ranked?: boolean;
    meta: (p: Pick) => string;
    line: (p: Pick) => string;
    onpick: (p: Pick, card: HTMLElement) => void;
    onall?: () => void;
  } = $props();

  let row = $state<HTMLElement | null>(null);
  let edges = $state({ start: true, end: true });

  /** Whether the row has more to its left and to its right. */
  function measure() {
    if (row) edges = { start: row.scrollLeft <= 1, end: row.scrollLeft + row.clientWidth >= row.scrollWidth - 1 };
  }

  function slide(by: number) {
    row?.scrollBy({ left: by * row.clientWidth * 0.8, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
  }

  $effect(() => {
    void picks.length;
    void row;
    measure();
  });

  onMount(() => {
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  });
</script>

<section class="prow" aria-label={title}>
  <div class="head">
    <h3>{#if onall}<button type="button" class="all" onclick={onall} title="Show all of {title}">{title}</button>{:else}{title}{/if}</h3>
    {#if note}<span>{note}</span>{/if}
    <span class="grow"></span>
    {#if onall}<button type="button" class="link-btn" onclick={onall}>Show all</button>{/if}
    {#if !(edges.start && edges.end)}
      <button type="button" class="pager" aria-label="Show previous" title="Show previous" disabled={edges.start} onclick={() => slide(-1)}><Icon name="back" /></button>
      <button type="button" class="pager next" aria-label="Show next" title="Show next" disabled={edges.end} onclick={() => slide(1)}><Icon name="back" /></button>
    {/if}
  </div>
  <div class="cards" class:ranked bind:this={row} onscroll={measure}>
    {#each picks as p, i (p.id)}
      {@const under = line(p)}
      <button type="button" class="pick" class:ranked onclick={(e) => onpick(p, e.currentTarget)} aria-label="{ranked ? `${i + 1}. ` : ""}{p.title}. {meta(p)}. {under}">
        {#if ranked}<span class="rank" aria-hidden="true">{i + 1}</span>{/if}
        <span class="cv-box" aria-hidden="true"><Cover cover={p.cover} color={p.color} native={p.native} title={p.title} /></span>
        <span class="p-title" title={p.title}><span>{p.title}</span></span>
        <span class="p-meta">{meta(p)}</span>
        <span class="p-why" class:have={p.show !== null}>{under}</span>
      </button>
    {/each}
  </div>
</section>

<style>
  .prow { min-width: 0; margin-bottom: 30px; }
  .head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; min-height: 36px; margin: 0 0 14px; }
  .head h3 { font: 600 17px/1.2 var(--font-ui); }
  .head h3 .all { all: unset; cursor: pointer; }
  .head h3 .all:hover { text-decoration: underline; text-underline-offset: 3px; }
  .head h3 .all:focus-visible { outline: 2px solid var(--ink); outline-offset: 2px; border-radius: 2px; }
  .head > span { font-size: 13px; color: var(--ink-3); }
  .pager { display: grid; place-items: center; width: 32px; height: 32px; padding: 0; border: 1px solid var(--rule-2); border-radius: 6px; background: transparent; color: var(--ink); cursor: pointer; }
  .pager:hover:not(:disabled) { background: var(--sunk); }
  .pager:disabled { opacity: .35; cursor: default; }
  .pager :global(svg) { width: 16px; height: 16px; }
  .pager.next :global(svg) { transform: scaleX(-1); }
  /* One row that scrolls sideways; the little padding keeps a card's focus ring and its lift whole. */
  .cards { display: grid; grid-auto-flow: column; grid-auto-columns: 168px; gap: 20px; margin: -6px -6px 0; padding: 6px; overflow-x: auto; scroll-snap-type: x proximity; scroll-padding-inline: 6px; scrollbar-width: none; }
  .pick { all: unset; position: relative; box-sizing: border-box; display: grid; gap: 3px; align-content: start; min-width: 0; cursor: pointer; scroll-snap-align: start; }
  .cards.ranked { grid-auto-columns: 222px; }
  .pick.ranked { padding-left: 54px; }
  .cv-box { position: relative; z-index: 1; display: block; }
  .rank { position: absolute; left: -6px; top: 236px; transform: translateY(-100%); font: 800 150px/.8 var(--font-display); font-variant-numeric: tabular-nums; letter-spacing: -.06em; color: var(--bg); -webkit-text-stroke: 2px var(--ink-3); pointer-events: none; }
  .pick :global(.cv) { transition: transform .15s; }
  .pick:hover :global(.cv) { transform: translateY(-2px); }
  .pick:focus-visible { outline: 2px solid var(--ink); outline-offset: 4px; border-radius: 4px; }
  .p-title { display: flex; align-items: baseline; gap: 8px; min-width: 0; margin-top: 9px; font: 600 13.5px/1.3 var(--font-ui); }
  .p-title span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .p-meta { font: 500 12.5px/1.3 var(--font-ui); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .p-why { min-height: 16px; font-size: 12px; line-height: 1.35; color: var(--ink-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .p-why.have { color: var(--ok); }
</style>
