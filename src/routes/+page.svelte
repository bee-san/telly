<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import AddSheet from "$lib/AddSheet.svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, callKept, errorText, kept, pickFolder, setWindowTitle, type Library, type Settings, type ShowCard } from "$lib/api";
  import { statusOf } from "$lib/format";
  import { app } from "$lib/state.svelte";

  let lib = $state.raw<Library | null>(kept<Library>("library"));
  let error = $state("");
  let q = $state("");
  let adding = $state<string | null>(null);
  let menu = $state<{ show: ShowCard; x: number; y: number } | null>(null);

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
    const loop = async () => {
      await load();
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
      clearTimeout(timer);
      window.removeEventListener("click", close);
      window.removeEventListener("resize", measure);
    };
  });

  function matches(s: ShowCard): boolean {
    const t = q.trim().toLowerCase();
    if (!t) return true;
    return [s.title, s.titleNative, s.titleEnglish, s.titleRomaji].join(" ").toLowerCase().includes(t);
  }

  /** NSFW shows are the ones kept in a folder named JAV. */
  const nsfw = (s: ShowCard) => s.folder.split(/[\/]/).some((part) => part.toLowerCase() === "jav");

  const shows = $derived((lib?.shows ?? []).filter((s) => matches(s) && (showNsfw || !nsfw(s))));
  const watching = $derived(
    shows.filter((s) => s.status === "watching" || (s.status === "getting" && (s.watched > 0 || !!s.next?.resume))).sort((a, b) => (b.lastWatchedAt ?? 0) - (a.lastWatchedAt ?? 0)),
  );
  const rest = $derived(shows.filter((s) => !watching.includes(s)));

  // Which of the Library's shows are listed, and in what order. Kept in Settings.
  type Kind = "all" | "series" | "movies";
  type Sort = "added" | "name" | "released" | "known";
  let settings = $state<Settings>(kept<Settings>("settings") ?? {});
  const kind = $derived<Kind>(["series", "movies"].includes(settings["library.show"] as string) ? (settings["library.show"] as Kind) : "all");
  const showNsfw = $derived(settings["library.nsfw"] === true);
  const sort = $derived<Sort>(["name", "released", "known"].includes(settings["library.sort"] as string) ? (settings["library.sort"] as Sort) : "added");

  function choose(key: string, value: string | boolean) {
    settings = { ...settings, [key]: value };
    call("setSetting", { key, value }).catch(() => {});
  }

  /** The share of a show's words you know, in per cent; null when none of its episodes is scored yet. */
  const knownOf = (s: ShowCard) => (s.wordsUses ? (100 * s.wordsKnown) / s.wordsUses : null);

  const listed = $derived.by(() => {
    const list = rest.filter((s) => kind === "all" || (kind === "movies") === s.movie);
    const byName = (a: ShowCard, b: ShowCard) => a.title.localeCompare(b.title);
    if (sort === "name") return list.sort(byName);
    // Newest first; a show with no year goes last.
    if (sort === "released") return list.sort((a, b) => (b.year ?? 0) - (a.year ?? 0) || byName(a, b));
    // Most words known first; a show with no score yet goes last.
    if (sort === "known") return list.sort((a, b) => (knownOf(b) ?? -1) - (knownOf(a) ?? -1) || byName(a, b));
    return list;
  });

  // The shows being watched sit in one row that scrolls sideways.
  let row = $state<HTMLElement | null>(null);
  let edges = $state({ start: true, end: true });

  /** Whether the row has more to its left and to its right. */
  function measure() {
    if (row) edges = { start: row.scrollLeft <= 1, end: row.scrollLeft + row.clientWidth >= row.scrollWidth - 1 };
  }

  function slide(by: number) {
    row?.scrollBy({ left: by * row.clientWidth, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
  }

  $effect(() => {
    void watching.length;
    void row;
    measure();
  });

  async function add() {
    const folder = await pickFolder();
    if (folder) adding = folder;
  }

  /** The pointer is on a show: its page is fetched now, to be there on the click. */
  function ahead(s: ShowCard) {
    callKept("show", { id: s.id }).catch(() => {});
  }

  function open(s: ShowCard) {
    if (s.next && s.next.ready) goto(`/watch?ep=${s.next.episodeId}`);
    else goto(`/show?id=${s.id}`);
  }

  function context(e: MouseEvent, s: ShowCard) {
    e.preventDefault();
    e.stopPropagation();
    menu = { show: s, x: e.clientX, y: e.clientY };
  }

  async function remove(s: ShowCard) {
    menu = null;
    if (!confirm(`Remove ${s.title} from Telly? The video files stay where they are. Telly’s copies and subtitles are deleted.`)) return;
    await call("removeShow", { id: s.id });
    load();
  }

  function count(n: number, what: Kind = "all") {
    if (what === "movies") return `${n} ${n === 1 ? "movie" : "movies"}`;
    return what === "series" ? `${n} series` : `${n} ${n === 1 ? "show" : "shows"}`;
  }
</script>

<div class="lib scroll">
  <div class="toolbar">
    <label class="search">
      <Icon name="search" />
      <input type="search" bind:value={q} placeholder="Search" aria-label="Search shows" />
    </label>
    <span class="grow"></span>
    <a class="btn quiet" href="/recommended">Recommended</a>
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
        <button type="button" class="pager" aria-label="Show previous" title="Show previous" disabled={edges.start} onclick={() => slide(-1)}><Icon name="back" /></button>
        <button type="button" class="pager next" aria-label="Show next" title="Show next" disabled={edges.end} onclick={() => slide(1)}><Icon name="back" /></button>
      {/if}
    </div>
    {#if watching.length}
      <div class="watching" bind:this={row} onscroll={measure}>
        {#each watching as s (s.id)}
          {@const st = statusOf(s)}
          <div class="wcard card" role="presentation" onpointerenter={() => ahead(s)} oncontextmenu={(e) => context(e, s)}>
            <button type="button" class="hit" onclick={() => open(s)} aria-label="{s.title}. {st.text}. {st.sub}"></button>
            <span class="wcv">
              <Cover cover={s.cover} color={s.coverColor} native={s.titleNative} title={s.title} caption={s.coverCaption} />
              <span class="wplay"><Icon name="play" /></span>
            </span>
            <span class="w-body">
              <a class="w-jp title" lang="ja" href="/show?id={s.id}">{s.titleNative || s.title}</a>
              <span class="w-en">{s.titleNative ? s.title : ""}</span>
              <span class="w-next">{st.text}</span>
              <span class="w-sub">{st.sub}</span>
              <span class="bar"><i style:width="{Math.round((100 * s.watched) / Math.max(1, s.episodes))}%"></i></span>
            </span>
          </div>
        {/each}
      </div>
    {:else}
      <p class="empty">{q ? "No show in progress matches." : "Nothing in progress. Pick something from the library."}</p>
    {/if}

    <div class="shelf-head">
      <h2>Library</h2><span>{count(listed.length, kind)}</span>
      <span class="grow"></span>
      <div class="seg" role="group" aria-label="Show">
        {#each [["all", "All"], ["series", "Series"], ["movies", "Movies"]] as [value, label] (value)}
          <button type="button" aria-pressed={kind === value} onclick={() => choose("library.show", value)}>{label}</button>
        {/each}
      </div>
      <label class="check nsfw"><input type="checkbox" checked={showNsfw} onchange={(e) => choose("library.nsfw", e.currentTarget.checked)} /><span>Show NSFW</span></label>
      <label class="sort">
        <span>Sort</span>
        <select class="input" value={sort} onchange={(e) => choose("library.sort", e.currentTarget.value)}>
          <option value="added">Recently added</option>
          <option value="name">Name</option>
          <option value="released">Released</option>
          <option value="known">Words known</option>
        </select>
      </label>
    </div>
    {#if listed.length}
      <div class="libgrid">
        {#each listed as s (s.id)}
          {@const st = statusOf(s)}
          <div class="lcard card" role="presentation" onpointerenter={() => ahead(s)} oncontextmenu={(e) => context(e, s)}>
            <button type="button" class="hit" onclick={() => open(s)} aria-label="{s.title}. {st.text}. {st.sub}"></button>
            <Cover cover={s.cover} color={s.coverColor} native={s.titleNative} title={s.title} caption={s.coverCaption} />
            <a class="l-title title" href="/show?id={s.id}">{s.title}</a>
            <span class="l-status" class:ok={st.ok}>{st.text}</span>
            <span class="l-sub">{st.sub}</span>
            {#if s.wordsUses}<span class="l-known">{Math.round((100 * s.wordsKnown) / s.wordsUses)}% known</span>{/if}
            {#if s.status === "getting" && s.next}
              <span class="bar"><i style:width="{Math.round(s.next.mediaProgress * 100)}%"></i></span>
            {/if}
          </div>
        {/each}
      </div>
    {:else}
      <p class="empty">{q ? "No shows match." : kind === "movies" ? "No movies here." : kind === "series" ? "No series here." : "Every show in the library is in progress."}</p>
    {/if}
  {/if}
</div>

{#if menu}
  <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px" role="menu">
    <button type="button" role="menuitem" onclick={() => menu && goto(`/show?id=${menu.show.id}`)}>Show episodes</button>
    {#if menu.show.next?.ready}
      <button type="button" role="menuitem" onclick={() => menu?.show.next && goto(`/watch?ep=${menu.show.next.episodeId}`)}>Play Episode {menu.show.next.number}</button>
    {/if}
    <button type="button" role="menuitem" class="danger" onclick={() => menu && remove(menu.show)}>Remove from Telly…</button>
  </div>
{/if}

{#if adding}
  <AddSheet folder={adding} onclose={() => (adding = null)} onadded={() => { adding = null; load(); }} />
{/if}

<style>
  .lib { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 40px; }
  .toolbar { display: flex; align-items: center; gap: 12px; margin-bottom: 24px; }
  .search { display: flex; align-items: center; gap: 8px; width: 320px; height: 38px; padding: 0 12px; border: 1px solid var(--rule-2); border-radius: 6px; background: var(--surface); color: var(--ink-3); }
  .search:focus-within { border-color: var(--ink); }
  .search :global(svg) { width: 16px; height: 16px; flex: none; }
  .search input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; font: 14px/1 var(--font-ui); color: var(--ink); }
  .toolbar a.btn { text-decoration: none; }
  .setup { display: flex; flex-wrap: wrap; gap: 6px 10px; align-items: baseline; padding: 10px 14px; margin: -8px 0 22px; border: 1px solid var(--rule-2); border-radius: 8px; background: var(--surface); font-size: 13.5px; color: var(--ink-2); }
  .first { max-width: 60ch; padding: 60px 0; display: grid; gap: 10px; }
  .first h2 { font: 600 22px/1.25 var(--font-display); }
  .first p { color: var(--ink-2); font-size: 15px; }
  .shelf-head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; min-height: 36px; margin: 0 0 14px; }
  .shelf-head h2 { font: 600 17px/1.2 var(--font-ui); }
  .shelf-head > span { font-size: 13px; color: var(--ink-3); }
  .nsfw { align-items: center; font: 600 12.5px/1 var(--font-ui); color: var(--ink-2); }
  .nsfw input { margin: 0; }
  .sort { display: inline-flex; align-items: center; gap: 8px; }
  .sort span { font: 600 12.5px/1 var(--font-ui); color: var(--ink-2); }
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
  .libgrid { display: grid; grid-template-columns: repeat(auto-fill, minmax(132px, 1fr)); gap: 22px 20px; }
  .lcard { box-sizing: border-box; display: grid; gap: 3px; align-content: start; cursor: pointer; min-width: 0; }
  .lcard :global(.cv) { transition: transform .15s; }
  .lcard:hover :global(.cv) { transform: translateY(-2px); }
  .lcard:has(.hit:focus-visible) { outline: 2px solid var(--ink); outline-offset: 4px; border-radius: 4px; }
  .l-title { font: 600 13.5px/1.3 var(--font-ui); margin-top: 9px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .l-status { font: 500 12.5px/1.3 var(--font-ui); }
  .l-status.ok { color: var(--ok); }
  .l-sub { font-size: 12px; color: var(--ink-3); }
  .l-known { font-size: 12px; color: var(--ink-2); }
  .lcard .bar { margin-top: 5px; }
  .empty { font-size: 14px; color: var(--ink-2); margin-bottom: 34px; }
  .menu { position: fixed; z-index: 80; min-width: 190px; display: grid; padding: 5px; background: var(--surface); border: 1px solid var(--rule-2); border-radius: 8px; box-shadow: var(--shadow-lg); }
  .menu button { all: unset; padding: 8px 10px; border-radius: 5px; font-size: 13.5px; cursor: pointer; }
  .menu button:hover, .menu button:focus-visible { background: var(--sunk); }
  .menu .danger { color: var(--bad); }
</style>
