<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import AddSheet from "$lib/AddSheet.svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import Scene from "$lib/Scene.svelte";
  import { call, callKept, errorText, kept, mediaUrl, pickFolder, setWindowTitle, type FoundLine, type LineSearch, type Library, type Settings, type ShowCard } from "$lib/api";
  import { clock, plural, statusOf } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

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

  let titleMatches = $state.raw<{ q: string; ids: Set<number> } | null>(null);

  function matches(s: ShowCard): boolean {
    const t = q.trim().toLowerCase();
    if (!t) return true;
    return [s.title, s.titleNative, s.titleEnglish, s.titleRomaji].some((title) => title.toLowerCase().includes(t))
      || (titleMatches?.q === q.trim() && titleMatches.ids.has(s.id));
  }


  const shows = $derived((lib?.shows ?? []).filter((s) => matches(s)));
  const inProgress = (s: ShowCard) => s.status === "watching" || (s.status === "getting" && (s.watched > 0 || !!s.next?.resume));
  /** The shows being watched among `list`, the last one watched first. */
  const beingWatched = (list: ShowCard[]) => list.filter(inProgress).sort((a, b) => (b.lastWatchedAt ?? 0) - (a.lastWatchedAt ?? 0));
  const watching = $derived(beingWatched(shows));
  const rest = $derived(shows.filter((s) => !watching.includes(s)));

  // Which of the Library's shows are listed, and in what order. Kept in Settings.
  type Kind = "all" | "series" | "movies";
  type Sort = "added" | "name" | "released" | "known";
  let settings = $state<Settings>(kept<Settings>("settings") ?? {});
  const kind = $derived<Kind>(["series", "movies"].includes(settings["library.show"] as string) ? (settings["library.show"] as Kind) : "all");
  const sort = $derived<Sort>(["name", "released", "known"].includes(settings["library.sort"] as string) ? (settings["library.sort"] as Sort) : "added");

  function choose(key: string, value: string | boolean) {
    settings = { ...settings, [key]: value };
    call("setSetting", { key, value }).catch(() => {});
  }

  /** The share of a show's words you know, in per cent; null when none of its episodes is scored yet. */
  const knownOf = (s: ShowCard) => (s.wordsUses ? (100 * s.wordsKnown) / s.wordsUses : null);

  /** `list` (one made for this) in the order Sort chose. */
  function sorted(list: ShowCard[]): ShowCard[] {
    const byName = (a: ShowCard, b: ShowCard) => a.title.localeCompare(b.title);
    if (sort === "name") return list.sort(byName);
    // Newest first; a show with no year goes last.
    if (sort === "released") return list.sort((a, b) => (b.year ?? 0) - (a.year ?? 0) || byName(a, b));
    // Most words known first; a show with no score yet goes last.
    if (sort === "known") return list.sort((a, b) => (knownOf(b) ?? -1) - (knownOf(a) ?? -1) || byName(a, b));
    return list;
  }

  const listed = $derived(sorted(rest.filter((s) => kind === "all" || (kind === "movies") === s.movie)));

  // ---- Lines the search finds in the subtitles (search.rs) ----

  const searching = $derived(q.trim() !== "");
  let found = $state.raw<FoundLine[]>([]);
  /** How many lines each show has, by its ID: `found` has only the best of them (search.rs). */
  let totals = $state.raw<Record<number, number>>({});
  /** The search `found` is the answer to. */
  let foundFor = $state("");
  /** A word is half-typed in Japanese: it's looked for once it's chosen. */
  let composing = $state(false);
  /** The shows whose every line is listed. */
  let opened = $state<Record<number, boolean>>({});
  /** The scene playing: the lines as they were listed when it opened, and which of them it's of. */
  let scene = $state<{ lines: FoundLine[]; at: number } | null>(null);

  $effect(() => {
    const text = q.trim();
    if (composing) return;
    if (!text) {
      found = [];
      totals = {};
      foundFor = "";
      titleMatches = null;
      return;
    }
    const timer = setTimeout(async () => {
      const shows = call<number[]>("searchShows", { q: text }).catch(() => []);
      const subtitles = call<LineSearch>("searchLines", { q: text }).catch((): LineSearch => ({ lines: [], shows: {} }));
      const ids = await shows;
      if (q.trim() !== text) return;
      titleMatches = { q: text, ids: new Set(ids) };
      const answer = await subtitles;
      if (q.trim() !== text) return;
      found = answer.lines;
      totals = answer.shows;
      foundFor = text;
      opened = {};
      scene = null;
    }, 150);
    return () => clearTimeout(timer);
  });

  /** A line you can read sooner is lower: one that's said, then the fewest other words you don't know
   *  (none is i+1), then those whose episode's words aren't saved yet. */
  const reach = (l: FoundLine) => (l.said ? 0 : 2_000_000) + (l.unknown ?? 1_000_000);

  /** The lines found, show by show. A show with a line you can read comes first, and so do such
   *  lines in it; among equals, shows are in the page's order and lines as they come in the show. */
  const groups = $derived.by(() => {
    const all = lib?.shows ?? [];
    const now = beingWatched(all);
    const of = new Map<number, FoundLine[]>();
    for (const l of found) {
      const list = of.get(l.showId);
      if (list) list.push(l);
      else of.set(l.showId, [l]);
    }
    return [...now, ...sorted(all.filter((s) => !now.includes(s)))]
      .filter((show) => of.has(show.id))
      .map((show) => {
        const lines = of.get(show.id)!.sort((a, b) => reach(a) - reach(b) || a.number - b.number || a.start - b.start);
        return { show, lines, total: Math.max(totals[show.id] ?? 0, lines.length) };
      })
      .sort((a, b) => reach(a.lines[0]) - reach(b.lines[0]));
  });
  const lines = $derived(groups.flatMap((g) => g.lines));
  const linesFound = $derived(groups.reduce((n, g) => n + g.total, 0));
  /** A show with many lines lists a few until it's opened. */
  const listedOf = (g: { show: ShowCard; lines: FoundLine[] }) => (g.lines.length > 8 && !opened[g.show.id] ? g.lines.slice(0, 6) : g.lines);

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
    try {
      await call("removeShow", { id: s.id });
    } catch (e) {
      toast({ key: "remove", state: "failed", title: `Couldn’t remove ${s.title}`, text: errorText(e) });
    }
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
      <input type="search" bind:value={q} placeholder="Search" aria-label="Search shows and lines" oncompositionstart={() => (composing = true)} oncompositionend={() => (composing = false)} />
    </label>
    <span class="grow"></span>
    <a class="btn quiet" href="/recommended">Recommended</a>
    <a class="btn quiet" href="/stats">Stats</a>
    <a class="btn quiet" href="/processing" title="What’s downloading, and what Telly is getting ready">Processing</a>
    <a class="btn quiet" href="/downloads" title="Find a show on nyaa.si, and Telly downloads it"><Icon name="download" /><span>Download…</span></a>
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
  {#each lib?.downloaded ?? [] as d (d.folder)}
    <p class="setup">
      <span>Downloaded: <b>{d.name}</b>{d.downloading ? " (the rest is still downloading)" : ""}</span>
      <button type="button" class="link-btn" onclick={() => (adding = d.folder)}>Add to library…</button>
    </p>
  {/each}
  {#if error}<p class="err">{error}</p>{/if}

  {#if lib && lib.shows.length === 0}
    <div class="first">
      <h2>No shows yet</h2>
      <p>Press <b>Add media…</b> and pick a folder of rips. Telly works out the show, finds Japanese subtitles, syncs them, and gets each episode ready to play. The files stay where they are.</p>
    </div>
  {:else if lib}
    <!-- A search lists only the shelves it finds something for. -->
    {#if !searching || watching.length}
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
            <button type="button" class="hit" onclick={() => open(s)} aria-label="{s.title}. {st.text}. {st.sub}" title={s.title}></button>
            <span class="wcv">
              <Cover cover={s.cover} color={s.coverColor} native={s.titleNative} title={s.title} caption={s.coverCaption} />
              <span class="wplay"><Icon name="play" /></span>
            </span>
            <span class="w-body">
              <a class="w-jp title" lang={s.titleNative ? "ja" : undefined} href="/show?id={s.id}" title={s.titleNative || s.title}>{s.titleNative || s.title}</a>
              <span class="w-en">{s.titleNative ? s.title : ""}</span>
              <span class="w-next">{st.text}</span>
              <span class="w-sub">{st.sub}</span>
              <span class="bar"><i style:width="{Math.round((100 * s.watched) / Math.max(1, s.episodes))}%"></i></span>
            </span>
          </div>
        {/each}
      </div>
    {:else}
      <p class="empty">Nothing in progress. Pick something from the library.</p>
    {/if}
    {/if}

    {#if !searching || rest.length}
    <div class="shelf-head">
      <h2>Library</h2><span>{count(listed.length, kind)}</span>
      <span class="grow"></span>
      <div class="seg" role="group" aria-label="Show">
        {#each [["all", "All"], ["series", "Series"], ["movies", "Movies"]] as [value, label] (value)}
          <button type="button" aria-pressed={kind === value} onclick={() => choose("library.show", value)}>{label}</button>
        {/each}
      </div>
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
            <a class="l-title title" href="/show?id={s.id}" title={s.title}>{s.title}</a>
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

    {#if groups.length}
      <div class="shelf-head">
        <h2>Lines</h2><span>{plural(linesFound, "line")} in {count(groups.length)}</span>
      </div>
      {#each groups as g (g.show.id)}
        <section class="lgroup">
          <div class="lg-head">
            <span class="lg-cv"><Cover cover={g.show.cover} color={g.show.coverColor} native={g.show.titleNative} title={g.show.title} /></span>
            <a class="lg-title" lang={g.show.titleNative ? "ja" : undefined} href="/show?id={g.show.id}" title={g.show.titleNative || g.show.title}>{g.show.titleNative || g.show.title}</a>
            <span class="lg-sub" title={g.show.titleNative ? g.show.title : undefined}>{g.show.titleNative ? `${g.show.title} · ` : ""}{plural(g.total, "line")}</span>
          </div>
          <div class="lrows">
            {#each listedOf(g) as l (`${l.episodeId} ${l.line}`)}
              <button type="button" class="lrow" onclick={() => (scene = { lines, at: lines.indexOf(l) })} aria-label="Play scene: {l.before}{l.word}{l.after} {g.show.title}{g.show.movie ? "" : `, Episode ${l.number}`}, {clock(l.start)}">
                <span class="lthumb">
                  <img src={mediaUrl(`/picture/${l.episodeId}/${l.picture}`)} alt="" loading="lazy" decoding="async" onerror={(e) => ((e.currentTarget as HTMLImageElement).hidden = true)} />
                  <span class="lplay"><Icon name="play" /></span>
                  <span class="ltime">{clock(l.start)}</span>
                </span>
                <span class="lbody">
                  <span class="lq" lang="ja">{l.before}<mark>{l.word}</mark>{l.after}</span>
                  {#if !g.show.movie}<span class="lmeta">Episode {l.number}</span>{/if}
                </span>
              </button>
            {/each}
          </div>
          {#if listedOf(g).length < g.lines.length}
            <button type="button" class="link-btn more" onclick={() => (opened = { ...opened, [g.show.id]: true })}>Show {g.lines.length - listedOf(g).length} more</button>
          {:else if g.total > g.lines.length}
            <p class="more lnote">These are the {g.lines.length} lines you can read soonest, of {g.total}.</p>
          {/if}
        </section>
      {/each}
    {:else if searching && !composing && foundFor === q.trim() && !watching.length && !rest.length}
      <p class="empty">No shows or lines match.</p>
    {/if}
  {/if}
</div>

{#if scene}
  <Scene lines={scene.lines} bind:at={scene.at} showOf={(l) => lib?.shows.find((s) => s.id === l.showId)} {settings} onclose={() => (scene = null)} />
{/if}

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
  .lgroup { margin-bottom: 22px; }
  .lg-head { display: flex; align-items: center; gap: 10px; margin: 0 0 6px; padding-left: 8px; min-width: 0; }
  .lg-cv { width: 22px; flex: none; }
  .lg-title { font: 800 15px/1.3 var(--font-jp-title); color: inherit; text-decoration: none; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .lg-title:hover { text-decoration: underline; }
  .lg-sub { font-size: 12.5px; color: var(--ink-3); white-space: nowrap; }
  .lrows { display: grid; grid-template-columns: repeat(auto-fill, minmax(380px, 1fr)); gap: 2px 16px; }
  .lrow { all: unset; box-sizing: border-box; display: grid; grid-template-columns: 112px minmax(0, 1fr); gap: 14px; align-items: center; min-width: 0; padding: 8px; border-radius: 8px; cursor: pointer; }
  .lrow:hover { background: var(--sunk); }
  .lrow:focus-visible { outline: 2px solid var(--ink); outline-offset: -2px; }
  .lthumb { position: relative; display: block; aspect-ratio: 16 / 9; border-radius: 4px; overflow: hidden; background: var(--sunk); }
  .lrow:hover .lthumb { background: var(--rule-2); }
  .lthumb img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; display: block; }
  .lplay { position: absolute; left: 50%; top: 50%; width: 30px; height: 30px; margin: -15px 0 0 -15px; border-radius: 50%; display: grid; place-items: center; background: rgba(0, 0, 0, .62); color: #fff; opacity: 0; transition: opacity .15s; }
  .lplay :global(svg) { width: 14px; height: 14px; margin-left: 2px; }
  .lrow:hover .lplay, .lrow:focus-visible .lplay { opacity: 1; }
  .ltime { position: absolute; right: 4px; bottom: 4px; padding: 2px 4px; border-radius: 3px; background: rgba(0, 0, 0, .66); color: #f2f2f0; font: 500 11px/1 var(--font-mono); font-variant-numeric: tabular-nums; }
  .lbody { display: grid; gap: 3px; min-width: 0; }
  .lq { font: 500 15.5px/1.45 var(--font-jp); display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .lq mark { font-weight: 700; color: inherit; background: linear-gradient(transparent 58%, var(--mark-soft) 58%); }
  .lmeta { font-size: 12.5px; color: var(--ink-3); }
  .more { margin: 6px 0 0 8px; }
  .lnote { font-size: 12.5px; color: var(--ink-3); }
  .menu { position: fixed; z-index: 80; min-width: 190px; display: grid; padding: 5px; background: var(--surface); border: 1px solid var(--rule-2); border-radius: 8px; box-shadow: var(--shadow-lg); }
  .menu button { all: unset; padding: 8px 10px; border-radius: 5px; font-size: 13.5px; cursor: pointer; }
  .menu button:hover, .menu button:focus-visible { background: var(--sunk); }
  .menu .danger { color: var(--bad); }
</style>
