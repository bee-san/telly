<script lang="ts">
  /** Stats' row of big figures: what each is, the figure as numbers and units, and a line under it. */
  let { tiles }: { tiles: { label: string; parts: { n: string; unit: string }[]; sub: string }[] } = $props();
</script>

<dl class="tiles" style:--n={tiles.length}>
  {#each tiles as t (t.label)}
    <div class="tile">
      <dt>{t.label}</dt>
      <dd class="t-n">{#each t.parts as p, i (i)}<span>{p.n}</span>{#if p.unit}<span class="t-unit">{p.unit}</span>{/if}{/each}</dd>
      {#if t.sub}<dd class="t-sub">{t.sub}</dd>{/if}
    </div>
  {/each}
</dl>

<style>
  /* One row of them, or two columns in a narrow window. */
  .tiles { display: grid; grid-template-columns: repeat(var(--n), minmax(0, 1fr)); margin: 0; border-top: 1px solid var(--rule); border-bottom: 1px solid var(--rule); }
  @media (max-width: 960px) { .tiles { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  .tile { min-width: 0; display: flex; flex-direction: column; gap: 6px; padding: 18px 20px 20px 0; }
  dt { font: 600 13px/1.3 var(--font-ui); color: var(--ink-2); }
  dd { margin: 0; }
  .t-n { font: 600 40px/1.05 var(--font-display); letter-spacing: -.02em; white-space: nowrap; }
  .t-unit { font-size: 18px; letter-spacing: 0; color: var(--ink-2); margin: 0 10px 0 4px; }
  .t-unit:last-child { margin-right: 0; }
  .t-sub { font-size: 12.5px; color: var(--ink-2); }
</style>
