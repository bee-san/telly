<script lang="ts">
  import { goto } from "$app/navigation";
  import type { Snapshot } from "./$types";
  import { onMount, tick } from "svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import PickRow from "$lib/PickRow.svelte";
  import { call, callKept, errorText, inTauri, kept, setWindowTitle, type Pick, type PickRowView, type Recommended, type Settings, type Verdict, downloadsOn } from "$lib/api";
  import { plural } from "$lib/format";
  import { downloadLink, pickMeta } from "$lib/search";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  /** How many picks Show all lists at first, and how many Show more adds. */
  const STEP = 24;

  let rec = $state.raw<Recommended | null>(kept<Recommended>("recommendations"));
  let shown = $state(STEP);
  let error = $state("");
  let settings = $state.raw<Settings>(kept<Settings>("settings") ?? {});
  let asking = $state(false);
  /** The pick whose sheet is open, and the card that opened it. */
  let open = $state.raw<Pick | null>(null);
  let opener: HTMLElement | null = null;
  let sheet = $state<HTMLElement | null>(null);
  /** The open pick's sheet is asking whether you liked it, having seen it elsewhere. */
  let seenIt = $state(false);
  let grid = $state<HTMLElement | null>(null);
  /** Show all is on: every pick is listed, where the rows were. */
  let all = $state(false);
  /** One of AniList's rows listed in full, where the rows were, a page at a time. */
  let full = $state.raw<{ key: string; title: string; note: string; picks: Pick[]; page: number; more: boolean } | null>(null);
  /** The row whose next page AniList is being asked for: another row opened meanwhile asks for its own. */
  let askingFor = $state<string | null>(null);
  const fullAsking = $derived(!!full && askingFor === full.key);
  /** The page's scrolling box. */
  let box = $state<HTMLElement | null>(null);
  /** How far down the rows were when a list was opened in full, to go back to. */
  let rowsAt = 0;
  // Searching AniList, for an anime that's neither a pick nor trending.
  let q = $state("");
  let searching = $state(false);
  /** What AniList found, and for which words; null when nothing has been searched for. */
  let results = $state.raw<{ for: string; picks: Pick[] } | null>(null);

  /** Which search is the latest: an older one's answer, coming back after it, is let go. */
  let searchRun = 0;

  async function search() {
    const words = q.trim();
    const run = ++searchRun;
    if (!words) return void (results = null);
    searching = true;
    try {
      const picks = await call<Pick[]>("searchPicks", { q: words });
      if (run !== searchRun) return;
      results = { for: words, picks };
      error = "";
    } catch (e) {
      if (run === searchRun) error = errorText(e);
    } finally {
      if (run === searchRun) searching = false;
    }
  }

  function clearSearch() {
    q = "";
    results = null;
    searchRun++;
    searching = false;
  }

  async function load() {
    asking = true;
    try {
      rec = await callKept<Recommended>("recommendations");
      error = "";
      // Coming back to the page, the rows it scrolls to may only be here now.
      await tick();
      scrollBack();
      returnTo = null;
    } catch (e) {
      error = errorText(e);
    } finally {
      asking = false;
    }
  }

  onMount(() => {
    app.dark = false;
    setWindowTitle("Recommended — Telly");
    callKept<Settings>("settings").then((s) => (settings = s)).catch(() => {});
    load();
    const key = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  });

  async function show(p: Pick, card: HTMLElement) {
    open = p;
    seenIt = false;
    opener = card;
    await tick();
    sheet?.focus();
  }

  /** Closes the sheet; focus goes back to the card that opened it. */
  async function close() {
    if (!open) return;
    open = null;
    await tick();
    opener?.focus();
  }

  /** Leaves a pick out, saying why (it counts for or against what's like it); focus goes to the one that takes its place. */
  async function hide(p: Pick, verdict: Verdict) {
    const at = rec?.picks.findIndex((pick) => pick.id === p.id) ?? 0;
    try {
      await call("hideRecommendation", { id: p.id, title: p.title, verdict });
      toast({ key: "left-out", state: "info", title: `Left out ${p.title}`, text: `${suggests[verdict]}.`, action: { label: "Undo", run: () => bringBack(p.id) } });
      open = null;
      await load();
      await tick();
      (grid ?? document).querySelectorAll<HTMLElement>(".pick")[at]?.focus();
    } catch (e) {
      error = errorText(e);
    }
  }

  /** What leaving a pick out does to what's suggested, by what you said of it. */
  const suggests: Record<Verdict, string> = { liked: "More like it will be suggested", disliked: "Less like it will be suggested", notForMe: "A little less like it will be suggested" };
  /** What you said of a pick you left out, as the list of them says it. */
  const saidOf = { liked: "Seen it, liked it", disliked: "Seen it, didn’t like it", notForMe: "Not for me", hidden: "Hidden" };

  /** Brings a pick you left out back, forgetting what you said of it. */
  async function bringBack(id: number) {
    try {
      await call("unhideRecommendation", { id });
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function openOnAniList(p: Pick) {
    const link = `https://anilist.co/anime/${p.id}`;
    try {
      if (inTauri) {
        const { openUrl } = await import("@tauri-apps/plugin-opener");
        await openUrl(link);
      } else window.open(link);
    } catch (e) {
      toast({ key: "link", state: "failed", title: "Couldn’t open the browser", text: errorText(e) });
    }
  }

  const meta = pickMeta;

  /** "Like Your Name. and 10 more"; nothing for one that's trending or was searched for. */
  function why(p: Pick): string {
    if (p.moreOf) return `More of ${p.moreOf}`;
    const [first, second] = p.from;
    if (!first) return "";
    if (!second) return `Like ${first.title}`;
    if (p.from.length === 2) return `Like ${first.title} and ${second.title}`;
    return `Like ${first.title} and ${p.from.length - 1} more`;
  }

  /** What a pick that's listed (trending, or searched for) says under its details: whether you have it, or it's still to come. */
  const listedAs = (p: Pick) => (p.show !== null ? "In your library" : p.unreleased ? "Not out yet" : "");

  /** Whether a pick is one of the suggestions, which can be hidden. */
  const suggested = (p: Pick) => rec?.picks.some((pick) => pick.id === p.id) ?? false;

  /** The line under a pick in a row: why it's suggested, or (in AniList's charts) whether you have it. */
  const lineFor = (key: string) => (key === "suggested" || key === "more" ? why : listedAs);

  /** The pick at the top of the page: the first with a wide picture, suggestions first. */
  const hero = $derived.by(() => {
    for (const row of rec?.rows ?? []) {
      const p = row.picks.find((pick) => pick.banner && pick.show === null && !pick.unreleased);
      if (p) return p;
    }
    return null;
  });

  /** The rows whose list AniList has more of: its charts and genres. */
  const charted = (key: string) => ["series", "movies", "season", "finished", "short", "gems", "top"].includes(key) || key.startsWith("genre:");

  /** Lists a row in full: every pick for Suggested for you, else AniList's whole list, its first page. */
  function showAll(row: PickRowView) {
    rowsAt = box?.scrollTop ?? 0;
    if (row.key === "suggested") all = true;
    else {
      full = { key: row.key, title: row.title, note: row.note, picks: row.picks, page: 0, more: true };
      moreOfFull();
    }
    if (box) box.scrollTop = 0;
  }

  /** Back to the rows, where they were. */
  async function showRows() {
    all = false;
    full = null;
    await tick();
    if (box) box.scrollTop = rowsAt;
  }

  /** Asks AniList for the next page of the list shown in full. The first replaces the row's picks. */
  async function moreOfFull() {
    const asked = full;
    if (!asked || askingFor === asked.key) return;
    askingFor = asked.key;
    try {
      const got = await call<{ picks: Pick[]; more: boolean }>("chartPage", { key: asked.key, page: asked.page + 1 });
      if (full?.key !== asked.key) return;
      const before = asked.page === 0 ? [] : full.picks;
      const seen = new Set(before.map((p) => p.id));
      full = { ...full, picks: [...before, ...got.picks.filter((p) => !seen.has(p.id))], page: asked.page + 1, more: got.more };
      error = "";
    } catch (e) {
      error = errorText(e);
    } finally {
      if (askingFor === asked.key) askingFor = null;
    }
  }

  /** Where the page was, kept when it's left (for Download…) and put back when you come back to it. */
  interface Place {
    q: string;
    results: { for: string; picks: Pick[] } | null;
    all: boolean;
    shown: number;
    full: typeof full;
    rowsAt: number;
    top: number;
    /** How far each row (by its name) was scrolled sideways. */
    rows: [string, number][];
  }
  /** Where to scroll back to once the rows are there. */
  let returnTo: { top: number; rows: [string, number][] } | null = null;

  /** Scrolls the page and its rows back to where they were. */
  function scrollBack() {
    if (!returnTo || !box) return;
    box.scrollTop = returnTo.top;
    for (const [title, left] of returnTo.rows) {
      const cards = [...box.querySelectorAll<HTMLElement>(".prow")].find((r) => r.getAttribute("aria-label") === title)?.querySelector<HTMLElement>(".cards");
      if (cards) cards.scrollLeft = left;
    }
  }

  export const snapshot: Snapshot<Place> = {
    capture: () => ({
      q,
      results,
      all,
      shown,
      full,
      rowsAt,
      top: box?.scrollTop ?? 0,
      rows: [...(box?.querySelectorAll<HTMLElement>(".prow") ?? [])].map((r): [string, number] => [r.getAttribute("aria-label") ?? "", r.querySelector<HTMLElement>(".cards")?.scrollLeft ?? 0]),
    }),
    restore: async (place) => {
      ({ q, results, all, shown, full, rowsAt } = place);
      returnTo = { top: place.top, rows: place.rows };
      await tick();
      scrollBack();
    },
  };

  /** Opens the search site's search for a pick, for Telly to download it. */
  function download(p: Pick) {
    goto(`${downloadLink(p)}&from=recommended`);
  }

  /** AniList's synopsis as plain text: its line breaks kept, the rest of its markup dropped. */
  function plain(html: string): string {
    const doc = new DOMParser().parseFromString(html.replace(/<br\s*\/?>/gi, "\n"), "text/html");
    return (doc.body.textContent ?? "").replace(/\n{3,}/g, "\n\n").trim();
  }
</script>

<div class="rec scroll" inert={!!open} bind:this={box}>
  <div class="rec-top">
    <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Library</span></button>
  </div>
  <div class="rec-head">
    <h2>Recommended</h2>
    {#if rec && rec.shows}<span>From {plural(rec.shows, "show")}</span>{/if}
    <span class="grow"></span>
    <form class="search" role="search" onsubmit={(e) => { e.preventDefault(); search(); }}>
      <Icon name="search" />
      <input type="search" bind:value={q} placeholder="Search AniList" aria-label="Search AniList for an anime to download" oninput={() => { if (!q.trim()) clearSearch(); }} />
    </form>
  </div>

  {#if error}
    <p class="err">{error}</p>
    <button type="button" class="btn quiet retry" onclick={load} disabled={asking}>Try again</button>
  {/if}
  {#if results || searching}
    <div class="shelf">
      <h3>{results ? `Found for “${results.for}”` : "Searching AniList…"}</h3>
      {#if results}<span>{plural(results.picks.length, "anime", "anime")}</span>{/if}
      <span class="grow"></span>
      <button type="button" class="link-btn" onclick={clearSearch}>Clear search</button>
    </div>
    {#if results && results.picks.length}
      <div class="picks">
        {#each results.picks as p (p.id)}
          <button type="button" class="pick" onclick={(e) => show(p, e.currentTarget)}>
            <span aria-hidden="true"><Cover cover={p.cover} color={p.color} native={p.native} title={p.title} /></span>
            <span class="p-title" title={p.title}>{p.title}</span>
            <span class="p-meta">{meta(p)}</span>
            <span class="p-why" class:have={p.show !== null}>{listedAs(p)}</span>
          </button>
        {/each}
      </div>
    {:else if results}
      <p class="note">Nothing found. Try another name.</p>
    {/if}
  {:else if !rec}
    {#if asking}<p class="note"><span class="spin"></span>Asking AniList…</p>{/if}
  {:else if all}
    <div class="shelf">
      <h3>Suggested for you</h3>
      <span>{plural(rec.picks.length, "pick")}</span>
      <span class="grow"></span>
      <button type="button" class="link-btn" onclick={showRows}>Show fewer</button>
    </div>
    <div class="picks" bind:this={grid}>
      {#each rec.picks.slice(0, shown) as p (p.id)}
        <button type="button" class="pick" onclick={(e) => show(p, e.currentTarget)}>
          <span aria-hidden="true"><Cover cover={p.cover} color={p.color} native={p.native} title={p.title} /></span>
          <span class="p-title" title={p.title}>{p.title}</span>
          <span class="p-meta">{meta(p)}</span>
          <span class="p-why">{why(p)}</span>
        </button>
      {/each}
    </div>
    {#if shown < rec.picks.length}
      <button type="button" class="btn quiet more" onclick={() => (shown += STEP)}>Show more</button>
    {/if}
  {:else if full}
    {@const f = full}
    <div class="shelf">
      <h3>{f.title}</h3>
      {#if f.note}<span>{f.note}</span>{/if}
      <span class="grow"></span>
      <button type="button" class="link-btn" onclick={showRows}>Show fewer</button>
    </div>
    <div class="picks" bind:this={grid}>
      {#each f.picks as p (p.id)}
        <button type="button" class="pick" onclick={(e) => show(p, e.currentTarget)}>
          <span aria-hidden="true"><Cover cover={p.cover} color={p.color} native={p.native} title={p.title} /></span>
          <span class="p-title" title={p.title}>{p.title}</span>
          <span class="p-meta">{meta(p)}</span>
          <span class="p-why" class:have={p.show !== null}>{listedAs(p)}</span>
        </button>
      {/each}
    </div>
    {#if f.more}
      <button type="button" class="btn quiet more" disabled={fullAsking} onclick={moreOfFull}>{fullAsking ? "Asking AniList…" : "Show more"}</button>
    {/if}
  {:else}
    {#if hero}
      {@const h = hero}
      <section class="hero" aria-label="Featured: {h.title}">
        <img class="hero-img" src={h.banner} alt="" />
        <div class="hero-text">
          <span class="hero-why">{why(h) || h.standing || "Featured"}</span>
          <h3 lang={h.native ? "ja" : undefined}>{h.native || h.title}</h3>
          <p class="hero-en">{[h.native ? h.title : "", meta(h)].filter(Boolean).join(" · ")}</p>
          <p class="hero-about">{plain(h.description)}</p>
          <div class="hero-actions">
            {#if downloadsOn(settings)}<button type="button" class="btn big" onclick={() => download(h)} title="Find it on your search site, most shared first, and pick a release for Telly to download"><Icon name="download" /><span>Download…</span></button>{/if}
            <button type="button" class="btn big quiet" onclick={(e) => show(h, e.currentTarget)}>More about it</button>
          </div>
        </div>
      </section>
    {/if}
    {#if !rec.picks.length}
      <p class="note none">{rec.shows === 0 ? "No show in the library is matched on AniList yet, so nothing is suggested for you." : "AniList has no recommendations for these shows."}</p>
    {/if}
    {#each rec.rows as row (row.title)}
      <PickRow
        title={row.title}
        note={row.key === "suggested" ? plural(rec.picks.length, "pick") : row.note}
        picks={row.picks}
        ranked={row.ranked}
        {meta}
        line={lineFor(row.key)}
        onpick={show}
        onall={(row.key === "suggested" && rec.picks.length > row.picks.length) || charted(row.key) ? () => showAll(row) : undefined}
      />
    {/each}
    {#if rec.leftOut?.length}
      <details class="left-out">
        <summary>Left out of your suggestions <span>{rec.leftOut.length}</span></summary>
        <ul>
          {#each rec.leftOut as l (l.id)}
            <li>
              <span class="lo-title" title={l.title}>{l.title || `AniList entry ${l.id}`}</span>
              <span class="lo-said">{saidOf[l.verdict]}</span>
              <button type="button" class="btn small quiet" onclick={() => bringBack(l.id)} title="Suggest it again, and forget what you said of it">Bring back</button>
            </li>
          {/each}
        </ul>
      </details>
    {/if}
  {/if}
</div>

{#if open}
  {@const p = open}
  <div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && close()}>
    <div class="sheet" role="dialog" aria-modal="true" aria-label={p.title} tabindex="-1" bind:this={sheet}>
      <div class="pk-head">
        <span aria-hidden="true"><Cover cover={p.cover} color={p.color} native={p.native} title={p.title} /></span>
        <div class="pk-text">
          <h3 lang="ja">{p.native || p.romaji}</h3>
          <p>{p.english || p.romaji}</p>
          <p class="pk-meta">{[meta(p), p.genres.join(", ")].filter(Boolean).join(" · ")}</p>
          <p class="pk-about">{plain(p.description)}</p>
        </div>
      </div>
      <div class="pk-from">
        {#if p.moreOf}
          <h4>More of {p.moreOf}</h4>
          <p class="pk-more">A {p.format === "MOVIE" ? "movie" : "season"} of it you don’t have, placed by how much you liked {p.from[0]?.title ?? p.moreOf}.</p>
        {:else if !p.from.length}
          {#if p.standing}<h4>{p.standing}</h4>{/if}
          {#if listedAs(p)}<p class="pk-more">{p.unreleased ? "Not out yet, so there’s nothing of it to download." : "In your library."}</p>{/if}
        {:else}
          <h4>Recommended from</h4>
          <ul>
            {#each p.from as f, i (i)}
              <li><span title={f.title}>{f.title}</span><span>{f.votes.toLocaleString("en-GB")} {f.votes === 1 ? "vote" : "votes"}</span></li>
            {/each}
          </ul>
        {/if}
      </div>
      <div class="sheet-foot">
        {#if seenIt}
          <span class="ask">Did you like it?</span>
          <button type="button" class="btn quiet" onclick={() => hide(p, "liked")} title="Leave it out, and suggest more like it">Liked it</button>
          <button type="button" class="btn quiet" onclick={() => hide(p, "disliked")} title="Leave it out, and suggest less like it">Didn’t like it</button>
          <span class="grow"></span>
          <button type="button" class="btn quiet" onclick={() => (seenIt = false)}>Back</button>
        {:else}
          {#if suggested(p)}
            <button type="button" class="btn quiet" onclick={() => (seenIt = true)} title="You’ve seen it elsewhere: say whether you liked it, and it’s left out">Seen it…</button>
            <button type="button" class="btn quiet" onclick={() => hide(p, "notForMe")} title="Leave it out, and suggest a little less like it">Not for me</button>
          {/if}
          <span class="grow"></span>
          <button type="button" class="btn quiet" onclick={close}>Close</button>
          <button type="button" class="btn quiet" onclick={() => openOnAniList(p)}>Open on AniList</button>
          {#if p.show !== null}
            <button type="button" class="btn" onclick={() => goto(`/show?id=${p.show}`)}>Open in library</button>
          {:else if !p.unreleased && downloadsOn(settings)}
            <button type="button" class="btn" onclick={() => download(p)} title="Find it on your search site, most shared first, and pick a release for Telly to download"><Icon name="download" /><span>Download…</span></button>
          {/if}
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .rec { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 40px; }
  .rec-top { display: flex; align-items: center; height: 38px; margin-bottom: 24px; }
  .rec-head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin: 0 0 26px; }
  .rec-head h2 { font: 600 22px/1.2 var(--font-ui); }
  .rec-head > span { font-size: 13px; color: var(--ink-2); }
  .search { display: flex; align-items: center; gap: 8px; width: 320px; max-width: 100%; height: 38px; box-sizing: border-box; padding: 0 12px; border: 1px solid var(--rule-2); border-radius: 6px; background: var(--surface); color: var(--ink-3); }
  .search:focus-within { border-color: var(--ink); }
  .search :global(svg) { width: 16px; height: 16px; flex: none; }
  .search input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; font: 14px/1 var(--font-ui); color: var(--ink); }
  .shelf { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; min-height: 36px; margin: 0 0 14px; }
  .shelf h3 { font: 600 17px/1.2 var(--font-ui); }
  .shelf > span { font-size: 13px; color: var(--ink-3); }
  .note.none { margin-bottom: 30px; }
  /* The pick at the top, over its wide picture, fading into the page as a streaming service's front page does. */
  .hero { position: relative; display: flex; align-items: flex-end; height: clamp(330px, 40vw, 470px); margin: -8px -32px 34px; overflow: hidden; background: var(--sunk); }
  .hero-img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; object-position: center 30%; }
  .hero::after { content: ""; position: absolute; inset: 0; background: linear-gradient(90deg, var(--bg) 6%, color-mix(in srgb, var(--bg) 72%, transparent) 36%, transparent 68%), linear-gradient(0deg, var(--bg) 0%, transparent 40%); }
  .hero-text { position: relative; z-index: 1; display: grid; gap: 6px; width: min(560px, 100%); box-sizing: border-box; padding: 0 32px 30px; }
  .hero-why { font: 600 13px/1.3 var(--font-ui); color: var(--ink-2); }
  .hero-text h3 { font: 800 40px/1.15 var(--font-jp-title); letter-spacing: .01em; text-wrap: balance; }
  .hero-en { font-size: 14px; color: var(--ink-2); }
  .hero-about { margin-top: 6px; font-size: 14px; line-height: 1.5; color: var(--ink-2); display: -webkit-box; -webkit-line-clamp: 3; line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }
  .hero-actions { display: flex; flex-wrap: wrap; gap: 10px; margin-top: 14px; }
  .note { display: flex; align-items: center; gap: 10px; font-size: 14px; color: var(--ink-2); }
  .retry { margin: 12px 0 22px; }
  .picks { display: grid; grid-template-columns: repeat(auto-fill, minmax(168px, 1fr)); gap: 26px 20px; }
  .pick { all: unset; box-sizing: border-box; display: grid; gap: 3px; align-content: start; min-width: 0; cursor: pointer; }
  .pick :global(.cv) { transition: transform .15s; }
  .pick:hover :global(.cv) { transform: translateY(-2px); }
  .pick:focus-visible { outline: 2px solid var(--ink); outline-offset: 4px; border-radius: 4px; }
  .p-title { font: 600 13.5px/1.3 var(--font-ui); margin-top: 9px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .p-meta { font: 500 12.5px/1.3 var(--font-ui); }
  .p-why { font-size: 12px; line-height: 1.35; color: var(--ink-2); display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .p-why.have { color: var(--ok); }
  .more { margin-top: 28px; }
  .sheet:focus { outline: none; }
  .ask { font-size: 13.5px; color: var(--ink-2); }
  .left-out { margin-top: 34px; max-width: 640px; }
  .left-out summary { cursor: pointer; font: 600 14px/1.3 var(--font-ui); color: var(--ink-2); }
  .left-out summary span { margin-left: 4px; font-weight: 500; color: var(--ink-3); }
  .left-out ul { list-style: none; padding: 0; margin-top: 10px; font-size: 13.5px; }
  .left-out li { display: flex; align-items: center; gap: 14px; padding: 6px 0; border-top: 1px solid var(--rule); }
  .lo-title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .lo-said { flex: none; color: var(--ink-2); }
  .pk-head { display: grid; grid-template-columns: 132px minmax(0, 1fr); gap: 20px; align-items: start; }
  .pk-text { display: grid; gap: 4px; min-width: 0; }
  .pk-text h3 { font: 800 24px/1.25 var(--font-jp-title); }
  .pk-text p { font-size: 14px; color: var(--ink-2); }
  .pk-text .pk-meta { font-size: 13px; }
  .pk-text .pk-about { margin-top: 10px; line-height: 1.5; max-width: 62ch; white-space: pre-line; }
  .pk-from { display: grid; gap: 6px; }
  .pk-from h4 { font: 600 12.5px/1.2 var(--font-ui); color: var(--ink-2); }
  .pk-from ul { list-style: none; padding: 0; font-size: 13.5px; line-height: 1.4; }
  .pk-more { font-size: 13.5px; color: var(--ink-2); }
  .pk-from li { display: flex; justify-content: space-between; align-items: baseline; gap: 16px; padding: 7px 0; border-top: 1px solid var(--rule); }
  .pk-from li span:first-child { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pk-from li span:last-child { flex: none; color: var(--ink-2); font-variant-numeric: tabular-nums; }
</style>
