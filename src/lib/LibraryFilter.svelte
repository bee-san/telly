<script lang="ts">
  import Icon from "./Icon.svelte";
  import { applied, KNOWN, SORTS, STATUSES, type Kind, type LibraryChoice, type Listing, type Sort, type Status } from "./listing";

  /** `libraries` are those that can be left out, `statuses` how many anime are at each, and `shown` of `total` anime are listed. */
  let {
    listing,
    libraries,
    statuses,
    shown,
    total,
    onchoose,
    onclear,
  }: {
    listing: Listing;
    libraries: LibraryChoice[];
    statuses: Record<Status, number>;
    shown: number;
    total: number;
    onchoose: (key: string, value: unknown) => void;
    onclear: () => void;
  } = $props();

  const count = $derived(applied(listing));

  let open = $state(false);
  let wrap = $state<HTMLElement>();
  let button = $state<HTMLButtonElement>();
  let panel = $state<HTMLElement>();

  // Open, a click outside it or Esc closes it, and it's scrolled to when it reaches below the window.
  // A click, not the press before it: closing can move the page, and what was pressed has to get its click first.
  $effect(() => {
    if (!open) return;
    panel?.scrollIntoView({ block: "nearest", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    const outside = (e: MouseEvent) => {
      if (wrap && !wrap.contains(e.target as Node)) open = false;
    };
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      open = false;
      button?.focus();
    };
    window.addEventListener("click", outside);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("click", outside);
      window.removeEventListener("keydown", key);
    };
  });

  /** A new order starts its usual way round. */
  function sortBy(sort: Sort) {
    onchoose("library.sort", sort.id);
    onchoose("library.reversed", false);
  }

  /** `list` with `item` left out when it should be, and not when it shouldn't. */
  const without = <T,>(list: T[], item: T, out: boolean) => (out ? [...list, item] : list.filter((i) => i !== item));

  const kinds: [Kind, string][] = [["all", "All"], ["series", "Series"], ["movies", "Movies"]];
</script>

<div class="filter" bind:this={wrap}>
  <button type="button" class="btn quiet" bind:this={button} aria-haspopup="dialog" aria-expanded={open} title="Filter and sort the Library" onclick={() => (open = !open)}>
    <Icon name="filter" />
    <span>Filter</span>
    {#if count}<span class="n">{count}<span class="sr"> applied</span></span>{/if}
    <span class="down"><Icon name="down" /></span>
  </button>
  {#if open}
    <div class="panel" role="dialog" aria-label="Filter and sort the Library" bind:this={panel}>
      <div class="cols">
        <div class="group sort" role="radiogroup" aria-label="Sort by">
          <span class="label">Sort by</span>
          {#each SORTS as sort (sort.id)}
            <label class="opt"><input type="radio" name="library-sort" checked={listing.sort.id === sort.id} onchange={() => sortBy(sort)} /><span>{sort.label}</span></label>
          {/each}
          <div class="seg order" role="group" aria-label="Order">
            {#each listing.sort.orders as order, i (order)}
              <button type="button" aria-pressed={listing.reversed === (i === 1)} onclick={() => onchoose("library.reversed", i === 1)}>{order}</button>
            {/each}
          </div>
        </div>
        <div class="filters">
          <div class="group">
            <span class="label">Show</span>
            <div class="seg" role="group" aria-label="Show">
              {#each kinds as [value, label] (value)}
                <button type="button" aria-pressed={listing.kind === value} onclick={() => onchoose("library.show", value)}>{label}</button>
              {/each}
            </div>
          </div>
          {#if libraries.length > 1}
            <div class="group" role="group" aria-label="Libraries">
              <span class="label">Libraries</span>
              {#each libraries as library (library.name)}
                <label class="opt">
                  <input type="checkbox" checked={!listing.without.includes(library.name)} onchange={(e) => onchoose("library.without", without(listing.without, library.name, !e.currentTarget.checked))} />
                  <span>{library.name || "Not in a library"}</span>
                  <small>{library.count}</small>
                </label>
              {/each}
            </div>
          {/if}
          <div class="group" role="group" aria-label="Status">
            <span class="label">Status</span>
            {#each STATUSES as [status, label] (status)}
              <label class="opt">
                <input type="checkbox" checked={!listing.withoutStatus.includes(status)} onchange={(e) => onchoose("library.withoutStatus", without(listing.withoutStatus, status, !e.currentTarget.checked))} />
                <span>{label}</span>
                <small>{statuses[status]}</small>
              </label>
            {/each}
          </div>
          <div class="group">
            <span class="label">Words known</span>
            <div class="seg" role="group" aria-label="Words known">
              {#each KNOWN as least (least)}
                <button type="button" aria-pressed={listing.known === least} onclick={() => onchoose("library.known", least)}>{least ? `${least}%+` : "Any"}</button>
              {/each}
            </div>
          </div>
          <label class="opt"><input type="checkbox" checked={listing.ready} onchange={(e) => onchoose("library.ready", e.currentTarget.checked)} /><span>Only what’s ready to play</span></label>
        </div>
      </div>
      <div class="foot">
        <span>Showing {shown} of {total} {total === 1 ? "show" : "shows"}</span>
        <span class="grow"></span>
        {#if count}<button type="button" class="link-btn" onclick={onclear}>Clear filters</button>{/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .filter { position: relative; }
  .filter > .btn { padding: 0 11px 0 13px; }
  .filter > .btn[aria-expanded="true"] { background: var(--sunk); }
  .n { display: inline-grid; place-items: center; min-width: 19px; height: 19px; padding: 0 5px; border-radius: 10px; background: var(--btn); color: var(--btn-ink); font: 700 11.5px/1 var(--font-ui); }
  .down { display: grid; color: var(--ink-2); }
  .down :global(svg) { width: 14px; height: 14px; }
  .panel { position: absolute; z-index: 20; top: calc(100% + 6px); right: 0; width: 572px; max-width: calc(100vw - 64px); scroll-margin: 16px; background: var(--surface); border: 1px solid var(--rule-2); border-radius: 8px; box-shadow: var(--shadow-lg); font: 400 13.5px/1.3 var(--font-ui); color: var(--ink); }
  .cols { display: grid; grid-template-columns: 240px minmax(0, 1fr); }
  .sort { padding: 16px; border-right: 1px solid var(--rule); }
  .filters { display: grid; gap: 16px; align-content: start; padding: 16px; }
  .group { display: grid; justify-items: start; align-content: start; }
  .label { margin-bottom: 6px; font: 600 12.5px/1.2 var(--font-ui); color: var(--ink-2); }
  .opt { justify-self: stretch; display: flex; align-items: center; gap: 10px; min-height: 30px; margin: 0 -8px; padding: 0 8px; border-radius: 5px; cursor: pointer; }
  .opt:hover { background: var(--sunk); }
  .opt input { width: 16px; height: 16px; margin: 0; flex: none; accent-color: var(--ink); }
  .opt small { margin-left: auto; font-size: 12px; color: var(--ink-3); }
  .order { justify-self: stretch; display: flex; margin-top: 12px; }
  .order button { flex: 1; white-space: nowrap; }
  .foot { display: flex; align-items: center; gap: 12px; min-height: 44px; padding: 0 16px; border-top: 1px solid var(--rule); font-size: 13px; color: var(--ink-2); }
  .foot .link-btn { color: var(--ink); }
  /* In a narrow window the order goes above the filters. */
  @media (max-width: 660px) {
    .cols { grid-template-columns: minmax(0, 1fr); }
    .sort { border-right: 0; border-bottom: 1px solid var(--rule); }
  }
</style>
