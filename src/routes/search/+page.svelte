<script lang="ts">
  // Every result of the title bar's search: the shows in your library it
  // names, the lines that say it, and the anime on AniList you could download.
  import { goto, replaceState } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import Scene from "$lib/Scene.svelte";
  import { callKept, errorText, kept, mediaUrl, setWindowTitle, type FoundLine, type Library, type LineSearch, type Pick, type Settings, type ShowCard } from "$lib/api";
  import { clock, plural, statusOf } from "$lib/format";
  import { downloadLink, findLines, findOnAniList, findShows, groupLines, pickMeta } from "$lib/search";
  import { app } from "$lib/state.svelte";

  type Kind = "all" | "shows" | "lines" | "anilist";
  let kind = $state<Kind>("all");
  let lib = $state.raw<Library | null>(kept<Library>("library"));
  let settings = $state<Settings>(kept<Settings>("settings") ?? {});

  // What's found, and for which words.
  let shows = $state.raw<{ for: string; list: ShowCard[] } | null>(null);
  let lines = $state.raw<{ for: string; found: LineSearch } | null>(null);
  let anilist = $state.raw<{ for: string; picks: Pick[]; error: string } | null>(null);

  /** The shows whose every line is listed. */
  let opened = $state<Record<number, boolean>>({});
  /** The scene playing: the lines as they were listed when it opened, and which of them it's of. */
  let scene = $state<{ lines: FoundLine[]; at: number } | null>(null);
  /** The line the drop-down under the search opened, as "episode-line", played once it's found. */
  let asked = page.url.searchParams.get("line");

  const q = $derived(app.search.trim());

  onMount(() => {
    app.dark = false;
    app.search = page.url.searchParams.get("q") ?? "";
    callKept<Library>("library").then((l) => (lib = l)).catch(() => {});
    callKept<Settings>("settings").then((s) => (settings = s)).catch(() => {});
  });

  $effect(() => {
    const text = q;
    // Asked again once the library is in, on a first visit with nothing kept.
    void lib;
    setWindowTitle(text ? `${text} — Telly` : "Search — Telly");
    if (!text) return;
    const timer = setTimeout(async () => {
      if (page.url.searchParams.get("q") !== text) replaceState(`/search?q=${encodeURIComponent(text)}`, {});
      const all = lib?.shows ?? [];
      const found = findLines(text);
      findOnAniList(text)
        .then((picks) => q === text && (anilist = { for: text, picks, error: "" }))
        .catch((e) => q === text && (anilist = { for: text, picks: [], error: errorText(e) }));
      const named = await findShows(all, text);
      if (q !== text) return;
      shows = { for: text, list: named };
      const answer = await found;
      if (q !== text) return;
      lines = { for: text, found: answer };
      opened = {};
      scene = null;
    }, 150);
    return () => clearTimeout(timer);
  });

  const named = $derived(shows?.for === q ? shows.list : null);
  const groups = $derived(lines?.for === q ? groupLines(lines.found, lib?.shows ?? []) : null);
  const picks = $derived(anilist?.for === q ? anilist : null);
  const allLines = $derived((groups ?? []).flatMap((g) => g.lines));
  const lineCount = $derived((groups ?? []).reduce((n, g) => n + g.total, 0));
  /** A show with many lines lists a few until it's opened. */
  const listedOf = (g: { show: ShowCard; lines: FoundLine[] }) => (g.lines.length > 8 && !opened[g.show.id] ? g.lines.slice(0, 6) : g.lines);
  const nothing = $derived(!!named && !!groups && !!picks && !named.length && !groups.length && !picks.picks.length);

  // The line the drop-down opened plays as soon as it's listed.
  $effect(() => {
    if (!asked || !groups) return;
    const at = allLines.findIndex((l) => `${l.episodeId}-${l.line}` === asked);
    asked = null;
    if (at >= 0) scene = { lines: allLines, at };
  });

  const show = (k: Kind) => kind === "all" || kind === k;
</script>

<div class="results scroll">
  <div class="results-top">
    <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Library</span></button>
    {#if q}
      <div class="seg" role="group" aria-label="Show">
        <button type="button" aria-pressed={kind === "all"} onclick={() => (kind = "all")}>All</button>
        <button type="button" aria-pressed={kind === "shows"} onclick={() => (kind = "shows")}>Library{#if named}<small>{named.length}</small>{/if}</button>
        <button type="button" aria-pressed={kind === "lines"} onclick={() => (kind = "lines")}>Lines{#if groups}<small>{lineCount.toLocaleString("en-GB")}</small>{/if}</button>
        <button type="button" aria-pressed={kind === "anilist"} onclick={() => (kind = "anilist")}>AniList{#if picks && !picks.error}<small>{picks.picks.length}</small>{/if}</button>
      </div>
    {/if}
  </div>

  {#if !q}
    <p class="note">Nothing searched for.</p>
  {:else if nothing}
    <p class="note">Nothing found for “{q}”.</p>
  {:else}
    {#if show("shows") && named && (named.length || kind === "shows")}
      <section>
        <div class="sh"><h2>Library</h2><span>{plural(named.length, "show")}</span></div>
        {#if named.length}
          <div class="covers">
            {#each named as s (s.id)}
              {@const st = statusOf(s)}
              <a class="card" href="/show?id={s.id}">
                <Cover cover={s.cover} color={s.coverColor} native={s.titleNative} title={s.title} caption={s.coverCaption} />
                <span class="c-title" title={s.title}>{s.title}</span>
                <span class="c-status" class:ok={st.ok}>{st.text}</span>
                <span class="c-sub" title={st.sub}>{st.sub}</span>
              </a>
            {/each}
          </div>
        {:else}
          <p class="note">No shows match.</p>
        {/if}
      </section>
    {/if}

    {#if show("lines") && groups && (groups.length || kind === "lines")}
      <section>
        <div class="sh"><h2>Lines</h2><span>{plural(lineCount, "line")} in {plural(groups.length, "show")}</span></div>
        {#if !groups.length}<p class="note">No lines say it.</p>{/if}
        {#each groups as g (g.show.id)}
          <div class="lgroup">
            <div class="lg-head">
              <span class="lg-cv"><Cover cover={g.show.cover} color={g.show.coverColor} native={g.show.titleNative} title={g.show.title} /></span>
              <a class="lg-title" lang={g.show.titleNative ? "ja" : undefined} href="/show?id={g.show.id}" title={g.show.titleNative || g.show.title}>{g.show.titleNative || g.show.title}</a>
              <span class="lg-sub" title={g.show.titleNative ? g.show.title : undefined}>{g.show.titleNative ? `${g.show.title} · ` : ""}{plural(g.total, "line")}</span>
            </div>
            <div class="lrows">
              {#each listedOf(g) as l (`${l.episodeId} ${l.line}`)}
                <button type="button" class="lrow" onclick={() => (scene = { lines: allLines, at: allLines.indexOf(l) })} aria-label="Play scene: {l.before}{l.word}{l.after} {g.show.title}{g.show.movie ? "" : `, Episode ${l.number}`}, {clock(l.start)}">
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
          </div>
        {/each}
      </section>
    {/if}

    {#if show("anilist")}
      <section>
        <div class="sh"><h2>Download</h2>{#if picks && !picks.error}<span>{plural(picks.picks.length, "anime", "anime")} on AniList</span>{/if}</div>
        {#if !picks}
          <p class="note"><span class="spin"></span>Searching AniList…</p>
        {:else if picks.error}
          <p class="err">Couldn’t search AniList: {picks.error}</p>
        {:else if !picks.picks.length}
          <p class="note">Nothing else on AniList.</p>
        {:else}
          <div class="covers">
            {#each picks.picks as p (p.id)}
              <div class="card">
                <Cover cover={p.cover} color={p.color} native={p.native} title={p.title} />
                <span class="c-title" title={p.title}>{p.title}</span>
                <span class="c-sub">{pickMeta(p)}</span>
                {#if p.unreleased}
                  <span class="c-sub">Not out yet</span>
                {:else}
                  <a class="btn quiet small dl" href={downloadLink(p)} title="Find it on nyaa.si, most shared first, and pick a release for Telly to download">Download…</a>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </section>
    {/if}
  {/if}
</div>

{#if scene}
  <Scene lines={scene.lines} bind:at={scene.at} showOf={(l) => lib?.shows.find((s) => s.id === l.showId)} {settings} onclose={() => (scene = null)} />
{/if}

<style>
  .results { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 40px; }
  .results-top { display: flex; flex-wrap: wrap; align-items: center; gap: 14px; margin-bottom: 26px; }
  .seg small { font-weight: 500; font-size: 12px; margin-left: 6px; opacity: .75; }
  section { margin-bottom: 34px; }
  .sh { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; min-height: 36px; margin: 0 0 14px; }
  .sh h2 { font: 600 17px/1.2 var(--font-ui); }
  .sh > span { font-size: 13px; color: var(--ink-3); }
  .note { display: flex; align-items: center; gap: 10px; font-size: 14px; color: var(--ink-2); }
  .covers { display: grid; grid-template-columns: repeat(auto-fill, minmax(132px, 1fr)); gap: 22px 20px; }
  .card { display: grid; gap: 3px; align-content: start; min-width: 0; color: inherit; text-decoration: none; }
  a.card :global(.cv) { transition: transform .15s; }
  a.card:hover :global(.cv) { transform: translateY(-2px); }
  a.card:focus-visible { outline: 2px solid var(--ink); outline-offset: 4px; border-radius: 4px; }
  .c-title { font: 600 13.5px/1.3 var(--font-ui); margin-top: 9px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .c-status { font: 500 12.5px/1.3 var(--font-ui); }
  .c-status.ok { color: var(--ok); }
  .c-sub { font-size: 12px; color: var(--ink-3); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .dl { justify-self: start; margin-top: 6px; text-decoration: none; }
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
</style>
