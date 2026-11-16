<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import AddSheet from "$lib/AddSheet.svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import LibraryFilter from "$lib/LibraryFilter.svelte";
  import { call, callKept, errorText, kept, pickFolder, setWindowTitle, type Library, type Settings, type ShowCard } from "$lib/api";
  import { addedOf, applied, knownOf, libraryChoices, listed as listedBy, listingOf, NO_FILTERS, statusOf, type Status } from "$lib/listing";
  import { animeOf, animeStatus, lastWatched, partsText, watching as isWatching, type AnimeCard } from "$lib/series";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  let lib = $state.raw<Library | null>(kept<Library>("library"));
  let error = $state("");
  let adding = $state<string | null>(null);
  let menu = $state<{ anime: AnimeCard; x: number; y: number } | null>(null);

  async function load() {
    try {
      lib = await callKept<Library>("library");
      error = "";
    } catch (e) {
      error = errorText(e);
    }
  }

  onMount(() => {
    app.dark = false;
    setWindowTitle("Telly");
    load();
    // /?add=<folder> opens Add media on that folder (handy for testing in a browser).
    const folder = page.url.searchParams.get("add");
    if (folder) adding = folder;
    let timer: ReturnType<typeof setTimeout>;
    // Stopped when the page is left, even with an answer on its way.
    let stopped = false;
    const loop = async () => {
      await load();
      if (stopped) return;
      const busy = lib?.shows.some((s) => s.status === "getting" || s.subsReady < s.episodes) ?? false;
      timer = setTimeout(loop, busy ? 2000 : 8000);
    };
    timer = setTimeout(loop, 2000);
    const close = () => (menu = null);
    window.addEventListener("click", close);
    window.addEventListener("resize", measure);
    callKept<Settings>("settings").then((s) => (settings = s)).catch(() => {});
    // Each show's share of known words is brought up to date with Anki as it is now.
    call("wordShares").catch(() => {});
    return () => {
      stopped = true;
      clearTimeout(timer);
      window.removeEventListener("click", close);
      window.removeEventListener("resize", measure);
    };
  });

  // An anime is listed once, with its seasons and movies inside (series.ts).
  const shows = $derived(animeOf(lib?.shows ?? []));
  /** The anime being watched among `list`, the last one watched first. */
  const beingWatched = (list: AnimeCard[]) => list.filter(isWatching).sort((a, b) => lastWatched(b) - lastWatched(a));
  const watching = $derived(beingWatched(shows));

  // Which of the Library's shows are listed, and in what order: what its Filter chooses (listing.ts). Kept in Settings.
  let settings = $state<Settings>(kept<Settings>("settings") ?? {});
  const libraries = $derived(lib?.libraries ?? []);
  /** The libraries the Filter can leave out. */
  const choices = $derived(libraryChoices(libraries, shows));
  const listing = $derived(listingOf(settings, choices));
  const filtered = $derived(applied(listing) > 0);

  function choose(key: string, value: unknown) {
    settings = { ...settings, [key]: value };
    call("setSetting", { key, value }).catch(() => {});
  }

  /** Everything the Filter left out is listed again; its order stays. */
  function clear() {
    for (const [key, value] of Object.entries(NO_FILTERS)) choose(key, value);
  }

  /** Every anime the Filter leaves in, those in Currently watching too. */
  const listed = $derived(listedBy(shows, listing, libraries));
  /** How many anime are at each status, for the Filter. */
  const statuses = $derived.by(() => {
    const count: Record<Status, number> = { new: 0, watching: 0, finished: 0 };
    for (const a of shows) count[statusOf(a)]++;
    return count;
  });

  /** How long an anime stays in Recently added after its newest part was added. */
  const RECENT_MS = 30 * 24 * 3600 * 1000;
  /** How many anime Recently added holds at most. */
  const RECENT_COUNT = 20;
  /** The anime added in the last 30 days, the newest first. */
  const recent = $derived(
    shows
      .filter((a) => addedOf(a) > Date.now() - RECENT_MS)
      .sort((a, b) => addedOf(b) - addedOf(a))
      .slice(0, RECENT_COUNT),
  );

  /** Where an anime's name leads: its one show's page, or its own page of seasons and movies. */
  const pageOf = (a: AnimeCard) => (a.parts.length === 1 ? `/show?id=${a.parts[0].id}` : `/anime?id=${a.id}`);

  // The shows being watched, and those added lately, each sit in one row that scrolls sideways.
  let row = $state<HTMLElement | null>(null);
  let edges = $state({ start: true, end: true });
  let recentRow = $state<HTMLElement | null>(null);
  let recentEdges = $state({ start: true, end: true });

  /** Whether each row has more to its left and to its right. */
  function measure() {
    const of = (r: HTMLElement) => ({ start: r.scrollLeft <= 1, end: r.scrollLeft + r.clientWidth >= r.scrollWidth - 1 });
    if (row) edges = of(row);
    if (recentRow) recentEdges = of(recentRow);
  }

  function slide(r: HTMLElement | null, by: number) {
    r?.scrollBy({ left: by * r.clientWidth, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
  }

  $effect(() => {
    void watching.length;
    void recent.length;
    void row;
    void recentRow;
    measure();
  });

  async function add() {
    const folder = await pickFolder();
    if (folder) adding = folder;
  }

  /** The pointer is on an anime: the page of the show that's next is fetched now, to be there on the click. */
  function ahead(a: AnimeCard) {
    callKept("show", { id: a.current.id }).catch(() => {});
  }

  function open(a: AnimeCard) {
    const next = a.current.next;
    if (next && next.ready) goto(`/watch?ep=${next.episodeId}`);
    else goto(pageOf(a));
  }

  function context(e: MouseEvent, a: AnimeCard) {
    e.preventDefault();
    e.stopPropagation();
    menu = { anime: a, x: e.clientX, y: e.clientY };
  }

  async function remove(s: ShowCard) {
    menu = null;
    if (!confirm(`Remove ${s.title} from Telly? The video files stay where they are. Telly’s copies and subtitles are deleted.`)) return;
    try {
      await call("removeShow", { id: s.id });
    } catch (e) {
      toast({ key: "remove", state: "failed", title: `Couldn’t remove ${s.title}`, text: errorText(e) });
    }
    load();
  }

  const count = (n: number) => `${n} ${n === 1 ? "show" : "shows"}`;
</script>

<div class="lib scroll">
  <div class="toolbar">
    <span class="grow"></span>
    <a class="btn quiet" href="/recommended">Recommended</a>
    <a class="btn quiet" href="/stats">Stats</a>
    <a class="btn quiet" href="/settings"><Icon name="gear" /><span>Settings</span></a>
    <button type="button" class="btn" onclick={add}><Icon name="plus" /><span>Add media…</span></button>
  </div>

  {#if lib && (!lib.setup.ffmpeg || !lib.setup.jimaku)}
    <p class="setup">
      {#if !lib.setup.ffmpeg}Telly needs ffmpeg to play and convert rips.{/if}
      {#if !lib.setup.jimaku}Add a Jimaku API key so Telly can find subtitles.{/if}
      <a href="/settings" class="link-btn">Open Settings</a>
    </p>
  {/if}
  {#if error}<p class="err">{error}</p>{/if}

  {#if lib && lib.shows.length === 0}
    <div class="first">
      <h2>No shows yet</h2>
      <p>Press <b>Add media…</b> and pick a folder of rips. Telly works out the show, finds Japanese subtitles, syncs them, and gets each episode ready to play. The files stay where they are.</p>
    </div>
  {:else if lib}
    <div class="shelf-head">
      <h2>Currently watching</h2><span>{count(watching.length)}</span>
      {#if !(edges.start && edges.end)}
        <span class="grow"></span>
        <button type="button" class="pager" aria-label="Show previous" title="Show previous" disabled={edges.start} onclick={() => slide(row, -1)}><Icon name="back" /></button>
        <button type="button" class="pager next" aria-label="Show next" title="Show next" disabled={edges.end} onclick={() => slide(row, 1)}><Icon name="back" /></button>
      {/if}
    </div>
    {#if watching.length}
      <div class="watching" bind:this={row} onscroll={measure}>
        {#each watching as a (a.id)}
          {@const st = animeStatus(a)}
          {@const s = a.parts[0]}
          {@const n = a.current.next}
          {@const part = n?.resume && n.duration > 0 ? Math.min(1, n.position / n.duration) : 0}
          <div class="wcard card" role="presentation" onpointerenter={() => ahead(a)} oncontextmenu={(e) => context(e, a)}>
            <button type="button" class="hit" onclick={() => open(a)} aria-label="{a.title}. {st.text}. {st.sub}" title={a.title}></button>
            <span class="wcv">
              <Cover cover={s.cover} color={s.coverColor} native={a.native} title={a.title} caption={s.coverCaption} />
              <span class="wplay"><Icon name="play" /></span>
            </span>
            <span class="w-body">
              <a class="w-jp title" lang={a.native ? "ja" : undefined} href={pageOf(a)} title={a.native || a.title}>{a.native || a.title}</a>
              <span class="w-en">{a.native ? a.title : ""}</span>
              <span class="w-next">{st.text}</span>
              <span class="w-sub">{st.sub}</span>
              <span class="bar"><i style:width="{Math.round(100 * part)}%"></i></span>
            </span>
          </div>
        {/each}
      </div>
    {:else}
      <p class="empty">Nothing in progress. Pick something from the library.</p>
    {/if}

    {#if recent.length}
      <div class="shelf-head">
        <h2>Recently added</h2><span>In the last 30 days</span>
        {#if !(recentEdges.start && recentEdges.end)}
          <span class="grow"></span>
          <button type="button" class="pager" aria-label="Show previous" title="Show previous" disabled={recentEdges.start} onclick={() => slide(recentRow, -1)}><Icon name="back" /></button>
          <button type="button" class="pager next" aria-label="Show next" title="Show next" disabled={recentEdges.end} onclick={() => slide(recentRow, 1)}><Icon name="back" /></button>
        {/if}
      </div>
      <div class="recent" bind:this={recentRow} onscroll={measure}>
        {#each recent as a (a.id)}{@render libraryCard(a)}{/each}
      </div>
    {/if}

    <div class="shelf-head">
      <h2>Library</h2><span>{filtered ? `${listed.length} of ${count(shows.length)}` : count(shows.length)}</span>
      <span class="grow"></span>
      <LibraryFilter {listing} libraries={choices} {statuses} shown={listed.length} total={shows.length} onchoose={choose} onclear={clear} />
    </div>
    {#if listed.length}
      <div class="libgrid">
        {#each listed as a (a.id)}{@render libraryCard(a)}{/each}
      </div>
    {:else}
      <p class="empty">Nothing in your library matches these filters. <button type="button" class="link-btn" onclick={clear}>Clear filters</button></p>
    {/if}
  {/if}
</div>

{#snippet libraryCard(a: AnimeCard)}
  {@const st = animeStatus(a)}
  {@const s = a.parts[0]}
  {@const known = knownOf(a)}
  <div class="lcard card" role="presentation" onpointerenter={() => ahead(a)} oncontextmenu={(e) => context(e, a)}>
    <button type="button" class="hit" onclick={() => open(a)} aria-label="{a.title}. {st.text}. {st.sub}"></button>
    <Cover cover={s.cover} color={s.coverColor} native={a.native} title={a.title} caption={s.coverCaption} />
    <a class="l-title title" href={pageOf(a)} title={a.title}>{a.title}</a>
    <span class="l-status" class:ok={st.ok}>{st.text}</span>
    <span class="l-sub" title={st.sub}>{st.sub}</span>
    {#if a.parts.length > 1 && !st.ok}<span class="l-sub">{partsText(a.parts)}</span>{/if}
    {#if known !== null}<span class="l-known">{Math.round(known)}% known</span>{/if}
    {#if a.current.status === "getting" && a.current.next}
      <span class="bar"><i style:width="{Math.round(a.current.next.mediaProgress * 100)}%"></i></span>
    {/if}
  </div>
{/snippet}

{#if menu}
  {@const one = menu.anime.parts.length === 1}
  {@const now = menu.anime.current}
  <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px" role="menu">
    <button type="button" role="menuitem" onclick={() => menu && goto(pageOf(menu.anime))}>{one ? "Show episodes" : "Show seasons and movies"}</button>
    {#if now.next?.ready}
      <button type="button" role="menuitem" onclick={() => now.next && goto(`/watch?ep=${now.next.episodeId}`)}>{now.movie ? `Play ${now.title}` : `Play Episode ${now.next.number}`}</button>
    {/if}
    {#if one}
      <button type="button" role="menuitem" class="danger" onclick={() => menu && remove(menu.anime.parts[0])}>Remove from Telly…</button>
    {/if}
  </div>
{/if}

{#if adding}
  <AddSheet folder={adding} onclose={() => (adding = null)} onadded={() => { adding = null; load(); }} />
{/if}

<style>
  .lib { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 40px; }
  .toolbar { display: flex; align-items: center; gap: 12px; margin-bottom: 24px; }
  .toolbar a.btn { text-decoration: none; }
  .setup { display: flex; flex-wrap: wrap; gap: 6px 10px; align-items: baseline; padding: 10px 14px; margin: -8px 0 22px; border: 1px solid var(--rule-2); border-radius: 8px; background: var(--surface); font-size: 13.5px; color: var(--ink-2); }
  .first { max-width: 60ch; padding: 60px 0; display: grid; gap: 10px; }
  .first h2 { font: 600 22px/1.25 var(--font-display); }
  .first p { color: var(--ink-2); font-size: 15px; }
  .shelf-head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; min-height: 36px; margin: 0 0 14px; }
  .shelf-head h2 { font: 600 17px/1.2 var(--font-ui); }
  .shelf-head > span { font-size: 13px; color: var(--ink-3); }
  .pager { display: grid; place-items: center; width: 32px; height: 32px; padding: 0; border: 1px solid var(--rule-2); border-radius: 6px; background: transparent; color: var(--ink); cursor: pointer; }
  .pager:hover:not(:disabled) { background: var(--sunk); }
  .pager:disabled { opacity: .35; cursor: default; }
  .pager :global(svg) { width: 16px; height: 16px; }
  .pager.next :global(svg) { transform: scaleX(-1); }
  /* One row, three cards across, that scrolls sideways; the little padding keeps a card's focus ring whole. */
  .watching { display: grid; grid-auto-flow: column; grid-auto-columns: minmax(330px, calc((100% - 32px) / 3)); gap: 16px; margin: -3px -3px 27px; padding: 3px; overflow-x: auto; scroll-snap-type: x proximity; scroll-padding-inline: 3px; scrollbar-width: none; }
  .watching > :global(*) { scroll-snap-align: start; }
  .card { position: relative; }
  .hit { all: unset; position: absolute; inset: 0; z-index: 1; cursor: pointer; border-radius: inherit; }
  .title { position: relative; z-index: 2; justify-self: start; max-width: 100%; color: inherit; text-decoration: none; }
  .title:hover { text-decoration: underline; }
  .title:focus-visible { outline: 2px solid var(--ink); outline-offset: 2px; border-radius: 2px; }
  .wcard { box-sizing: border-box; display: grid; grid-template-columns: 92px minmax(0, 1fr); gap: 16px; align-items: center; padding: 12px 16px 12px 12px; border-radius: 10px; background: var(--surface); border: 1px solid var(--rule); cursor: pointer; transition: border-color .15s, box-shadow .15s; }
  .wcard:hover { border-color: var(--rule-2); box-shadow: var(--shadow); }
  .wcard:has(.hit:focus-visible) { outline: 2px solid var(--ink); outline-offset: 2px; }
  .wcv { position: relative; display: block; }
  .wplay { position: absolute; left: 50%; top: 50%; width: 36px; height: 36px; margin: -18px 0 0 -18px; border-radius: 50%; display: grid; place-items: center; background: rgba(0, 0, 0, .62); color: #fff; opacity: 0; transition: opacity .15s; }
  .wplay :global(svg) { width: 16px; height: 16px; margin-left: 2px; }
  .wcard:hover .wplay, .wcard:has(.hit:focus-visible) .wplay { opacity: 1; }
  .w-body { display: grid; gap: 3px; min-width: 0; }
  .w-jp { font: 800 16px/1.3 var(--font-jp-title); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .w-en { font-size: 12.5px; color: var(--ink-3); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .w-next { font: 600 15.5px/1.3 var(--font-ui); margin-top: 8px; }
  .w-sub { font-size: 12.5px; color: var(--ink-2); }
  .w-body .bar { margin-top: 8px; }
  /* One row of the Library's cards that scrolls sideways; the padding keeps a card's focus ring whole. */
  .recent { display: grid; grid-auto-flow: column; grid-auto-columns: 150px; gap: 22px 20px; margin: -6px -6px 28px; padding: 6px; overflow-x: auto; scroll-snap-type: x proximity; scroll-padding-inline: 6px; scrollbar-width: none; }
  .recent > :global(*) { scroll-snap-align: start; }
  .libgrid { display: grid; grid-template-columns: repeat(auto-fill, minmax(132px, 1fr)); gap: 22px 20px; }
  .lcard { box-sizing: border-box; display: grid; gap: 3px; align-content: start; cursor: pointer; min-width: 0; }
  .lcard :global(.cv) { transition: transform .15s; }
  .lcard:hover :global(.cv) { transform: translateY(-2px); }
  .lcard:has(.hit:focus-visible) { outline: 2px solid var(--ink); outline-offset: 4px; border-radius: 4px; }
  .l-title { font: 600 13.5px/1.3 var(--font-ui); margin-top: 9px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .l-status { font: 500 12.5px/1.3 var(--font-ui); }
  .l-status.ok { color: var(--ok); }
  .l-sub { font-size: 12px; color: var(--ink-3); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .l-known { font-size: 12px; color: var(--ink-2); }
  .lcard .bar { margin-top: 5px; }
  .empty { font-size: 14px; color: var(--ink-2); margin-bottom: 34px; }
  .menu { position: fixed; z-index: 80; min-width: 190px; display: grid; padding: 5px; background: var(--surface); border: 1px solid var(--rule-2); border-radius: 8px; box-shadow: var(--shadow-lg); }
  .menu button { all: unset; padding: 8px 10px; border-radius: 5px; font-size: 13.5px; cursor: pointer; }
  .menu button:hover, .menu button:focus-visible { background: var(--sunk); }
  .menu .danger { color: var(--bad); }
</style>
