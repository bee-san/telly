<script lang="ts">
  // The search in the title bar. As you type, a list under it has the shows in
  // your library it names, the lines that say it and the anime on AniList you
  // could download; Enter opens the page of every result (routes/search).
  import { afterNavigate, goto } from "$app/navigation";
  import { page } from "$app/state";
  import { callKept, downloadsOn, errorText, kept, mediaUrl, type FoundLine, type Library, type LineSearch, type Pick, type Settings, type ShowCard } from "./api";
  import Cover from "./Cover.svelte";
  import Icon from "./Icon.svelte";
  import { clock, plural, statusOf } from "./format";
  import { downloadLink, findLines, findOnAniList, findShows, groupLines, pickMeta } from "./search";
  import { app } from "./state.svelte";

  let { dark = false }: { dark?: boolean } = $props();

  let input = $state<HTMLInputElement>();
  let wrap = $state<HTMLElement>();
  let focused = $state(false);
  /** A word is half-typed in Japanese: it's looked for once it's chosen. */
  let composing = $state(false);
  /** The option chosen with the arrow keys; -1 when none is. */
  let active = $state(-1);

  const onPage = $derived(page.url.pathname === "/search");
  const q = $derived(app.search.trim());
  const open = $derived(focused && !onPage && q !== "");

  // What's found, and for which words.
  let shows = $state.raw<{ for: string; list: ShowCard[] } | null>(null);
  let lines = $state.raw<{ for: string; found: LineSearch; all: ShowCard[] } | null>(null);
  let anilist = $state.raw<{ for: string; picks: Pick[]; error: string } | null>(null);
  /** Your settings, for whether downloading is on. */
  let settings = $state.raw<Settings | null>(kept<Settings>("settings"));

  $effect(() => {
    if (open) callKept<Settings>("settings").then((s) => (settings = s)).catch(() => {});
  });
  /** Whether an anime found on AniList can be downloaded from here: it's out, and downloading is on (a search site is set). */
  const gettable = (p: Pick) => !p.unreleased && downloadsOn(settings);

  $effect(() => {
    const text = q;
    if (!open || composing) return;
    active = -1;
    // Shows and lines are in Telly; AniList is asked once typing pauses.
    const near = setTimeout(async () => {
      const lib = kept<Library>("library") ?? (await callKept<Library>("library").catch(() => null));
      const all = lib?.shows ?? [];
      const found = findLines(text);
      const named = await findShows(all, text);
      if (q !== text) return;
      shows = { for: text, list: named };
      // The lines can take a while: words typed since have their own.
      const said = await found;
      if (q !== text) return;
      lines = { for: text, found: said, all };
    }, 150);
    const far = setTimeout(async () => {
      try {
        const picks = await findOnAniList(text);
        if (q === text) anilist = { for: text, picks, error: "" };
      } catch (e) {
        if (q === text) anilist = { for: text, picks: [], error: errorText(e) };
      }
    }, 400);
    return () => {
      clearTimeout(near);
      clearTimeout(far);
    };
  });

  const named = $derived(shows?.for === q ? shows.list : null);
  const groups = $derived(lines?.for === q ? groupLines(lines.found, lines.all) : null);
  const picks = $derived(anilist?.for === q ? anilist : null);

  /** The best line of each of the first three shows with one. */
  const someLines = $derived((groups ?? []).slice(0, 3).map((g) => ({ line: g.lines[0], show: g.show })));
  const lineCount = $derived((groups ?? []).reduce((n, g) => n + g.total, 0));

  type Option = { id: string; go: () => void };
  const options = $derived.by(() => {
    const list: Option[] = [];
    for (const s of (named ?? []).slice(0, 4)) list.push({ id: `s${s.id}`, go: () => goto(`/show?id=${s.id}`) });
    for (const { line } of someLines) list.push({ id: `l${line.episodeId}-${line.line}`, go: () => openLine(line) });
    for (const p of (picks?.picks ?? []).slice(0, 3)) if (gettable(p)) list.push({ id: `p${p.id}`, go: () => goto(downloadLink(p)) });
    list.push({ id: "all", go: showAll });
    return list;
  });
  /** Where each group's options start, for Tab. */
  const starts = $derived(["s", "l", "p", "all"].map((k) => options.findIndex((o) => o.id.startsWith(k))).filter((i) => i >= 0));
  const indexOf = (id: string) => options.findIndex((o) => o.id === id);

  function showAll() {
    goto(`/search?q=${encodeURIComponent(q)}`);
    input?.blur();
  }

  function openLine(l: FoundLine) {
    goto(`/search?q=${encodeURIComponent(q)}&line=${l.episodeId}-${l.line}`);
    input?.blur();
  }

  function onKey(e: KeyboardEvent) {
    if (e.isComposing) return;
    if (e.key === "Escape") {
      if (app.search && !onPage) app.search = "";
      else input?.blur();
    } else if (e.key === "Enter") {
      if (onPage) return;
      (options[active] ?? { go: showAll }).go();
    } else if (open && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      const n = options.length;
      active = e.key === "ArrowDown" ? (active + 1) % n : active <= 0 ? n - 1 : active - 1;
    } else if (open && e.key === "Tab" && !e.shiftKey) {
      // To the next kind of result, while there's one.
      const next = starts.find((i) => i > active);
      if (next === undefined) return;
      active = next;
    } else return;
    e.preventDefault();
    // The player's own keys wait while the search has them.
    e.stopPropagation();
  }

  // Ctrl+K goes to the search from anywhere.
  $effect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.ctrlKey && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k") {
        e.preventDefault();
        input?.focus();
        input?.select();
      }
    };
    const outside = (e: PointerEvent) => {
      if (wrap && !wrap.contains(e.target as Node)) focused = false;
    };
    window.addEventListener("keydown", key);
    window.addEventListener("pointerdown", outside);
    return () => {
      window.removeEventListener("keydown", key);
      window.removeEventListener("pointerdown", outside);
    };
  });

  // Leaving for another page empties the search; the page of every result keeps it.
  afterNavigate(({ to }) => {
    focused = false;
    if (to?.url.pathname !== "/search") app.search = "";
  });

  $effect(() => {
    if (active >= 0) document.getElementById(`sb-${options[active]?.id}`)?.scrollIntoView({ block: "nearest" });
  });
</script>

<div class="sb" class:dark bind:this={wrap}>
  <label class="sb-field">
    <Icon name="search" />
    <input
      bind:this={input}
      bind:value={app.search}
      type="search"
      placeholder="Search"
      title="Search shows, lines and AniList (Ctrl+K)"
      aria-label="Search shows, lines and AniList"
      role="combobox"
      aria-expanded={open}
      aria-controls="sb-list"
      aria-autocomplete="list"
      aria-activedescendant={open && active >= 0 ? `sb-${options[active]?.id}` : undefined}
      autocomplete="off"
      spellcheck="false"
      onfocus={() => (focused = true)}
      onkeydown={onKey}
      oncompositionstart={() => (composing = true)}
      oncompositionend={() => (composing = false)}
    />
    {#if !app.search}<kbd>Ctrl K</kbd>{/if}
  </label>

  {#if open}
    <div class="sb-list scroll" id="sb-list" role="listbox" aria-label="Results for {q}">
      {#if named && named.length}
        <div role="group" aria-labelledby="sb-h-shows">
          <div class="sb-h" id="sb-h-shows" role="presentation">In your library</div>
          {#each named.slice(0, 4) as s (s.id)}
            {@const st = statusOf(s)}
            {@const id = `s${s.id}`}
            <div class="sb-row show" id="sb-{id}" role="option" tabindex="-1" aria-selected={options[active]?.id === id} onpointerdown={(e) => e.preventDefault()} onclick={() => options[indexOf(id)].go()} onkeydown={() => {}}>
              <span class="sb-cv"><Cover cover={s.cover} color={s.coverColor} native={s.titleNative} title={s.title} /></span>
              <span class="sb-text">
                <span class="sb-jp" lang={s.titleNative ? "ja" : undefined}>{s.titleNative || s.title}</span>
                <span class="sb-meta">{s.titleNative ? `${s.title} · ` : ""}{st.sub}</span>
              </span>
              <span class="sb-tag" class:ok={st.ok}>{st.text}</span>
            </div>
          {/each}
          {#if named.length > 4}<div class="sb-note" role="presentation">and {named.length - 4} more</div>{/if}
        </div>
      {/if}

      {#if groups && groups.length}
        <div role="group" aria-labelledby="sb-h-lines">
          <div class="sb-h" id="sb-h-lines" role="presentation">Lines <span>{plural(lineCount, "line")} in {plural(groups.length, "show")}</span></div>
          {#each someLines as { line: l, show } (`${l.episodeId} ${l.line}`)}
            {@const id = `l${l.episodeId}-${l.line}`}
            <div class="sb-row line" id="sb-{id}" role="option" tabindex="-1" aria-selected={options[active]?.id === id} onpointerdown={(e) => e.preventDefault()} onclick={() => options[indexOf(id)].go()} onkeydown={() => {}}>
              <span class="sb-thumb">
                <img src={mediaUrl(`/picture/${l.episodeId}/${l.picture}`)} alt="" loading="lazy" decoding="async" onerror={(e) => ((e.currentTarget as HTMLImageElement).hidden = true)} />
                <span>{clock(l.start)}</span>
              </span>
              <span class="sb-text">
                <span class="sb-q" lang="ja">{l.before}<mark>{l.word}</mark>{l.after}</span>
                <span class="sb-meta">{show.title}{show.movie ? "" : ` · Episode ${l.number}`}</span>
              </span>
            </div>
          {/each}
        </div>
      {/if}

      <div role="group" aria-labelledby="sb-h-anilist">
        <div class="sb-h" id="sb-h-anilist" role="presentation">Download <span>from AniList</span></div>
        {#if !picks}
          <div class="sb-note" role="presentation"><span class="spin"></span>Searching AniList…</div>
        {:else if picks.error}
          <div class="sb-note" role="presentation">Couldn’t search AniList: {picks.error}</div>
        {:else if !picks.picks.length}
          <div class="sb-note" role="presentation">Nothing else on AniList</div>
        {:else}
          {#each picks.picks.slice(0, 3) as p (p.id)}
            {@const id = `p${p.id}`}
            <div class="sb-row show" id="sb-{id}" role="option" tabindex="-1" aria-selected={options[active]?.id === id} onpointerdown={(e) => e.preventDefault()} onclick={() => options[indexOf(id)]?.go()} onkeydown={() => {}}>
              <span class="sb-cv"><Cover cover={p.cover} color={p.color} native={p.native} title={p.title} /></span>
              <span class="sb-text">
                <span class="sb-jp" lang={p.native ? "ja" : undefined}>{p.native || p.title}</span>
                <span class="sb-meta">{p.native ? `${p.title} · ` : ""}{pickMeta(p)}</span>
              </span>
              {#if p.unreleased}<span class="sb-tag">Not out yet</span>{:else if gettable(p)}<span class="sb-tag">Download…</span>{/if}
            </div>
          {/each}
        {/if}
      </div>

      <div class="sb-row all" id="sb-all" role="option" tabindex="-1" aria-selected={options[active]?.id === "all"} onpointerdown={(e) => e.preventDefault()} onclick={showAll} onkeydown={() => {}}>
        <span>Show all results</span>
        <kbd>Enter</kbd>
      </div>
    </div>
  {/if}
</div>

<style>
  .sb { position: relative; width: 100%; }
  .sb-field { display: flex; align-items: center; gap: 8px; height: 28px; padding: 0 6px 0 10px; background: var(--surface); border: 1px solid var(--rule-2); border-radius: 6px; color: var(--ink-3); cursor: text; }
  .sb-field:focus-within { border-color: var(--ink); }
  .sb-field :global(svg) { width: 15px; height: 15px; flex: none; }
  .sb-field input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; font: 13.5px/1 var(--font-ui); color: var(--ink); }
  .sb-field input::-webkit-search-cancel-button { cursor: pointer; }
  kbd { flex: none; font: 500 11px/1 var(--font-ui); color: var(--ink-2); padding: 3px 5px; border: 1px solid var(--rule-2); border-radius: 4px; }
  .sb-list { position: absolute; top: calc(100% + 6px); left: 50%; transform: translateX(-50%); width: min(600px, calc(100vw - 24px)); max-height: calc(100vh - 60px); overflow-y: auto; display: grid; grid-template-columns: minmax(0, 1fr); padding: 5px; background: var(--surface); border: 1px solid var(--rule-2); border-radius: 10px; box-shadow: var(--shadow-lg); color: var(--ink); font: 13.5px/1.3 var(--font-ui); }
  .sb-h { display: flex; align-items: baseline; gap: 8px; padding: 10px 10px 4px; font: 600 11.5px/1.2 var(--font-ui); color: var(--ink-2); }
  .sb-h span { font-weight: 400; color: var(--ink-3); }
  .sb-row { display: grid; align-items: center; gap: 12px; padding: 6px 10px; border-radius: 6px; cursor: pointer; min-width: 0; }
  .sb-row:hover { background: var(--sunk); }
  .sb-row[aria-selected="true"] { background: var(--sunk); box-shadow: inset 2px 0 0 var(--ink); }
  .sb-row.show { grid-template-columns: 28px minmax(0, 1fr) auto; }
  .sb-row.line { grid-template-columns: 96px minmax(0, 1fr); }
  .sb-row.all { display: flex; justify-content: space-between; margin-top: 4px; border-top: 1px solid var(--rule); border-radius: 0 0 6px 6px; font-weight: 600; }
  .sb-cv { display: block; }
  .sb-cv :global(.cv-jp), .sb-cv :global(.cv-en) { display: none; }
  .sb-text { display: grid; gap: 1px; min-width: 0; }
  .sb-jp { font: 800 14px/1.3 var(--font-jp-title); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .sb-meta { font-size: 12px; color: var(--ink-3); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .sb-tag { font-size: 12px; color: var(--ink-2); white-space: nowrap; }
  .sb-tag.ok { color: var(--ok); }
  .sb-thumb { position: relative; display: block; aspect-ratio: 16 / 9; border-radius: 4px; overflow: hidden; background: var(--sunk); }
  .sb-thumb img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; }
  .sb-thumb span { position: absolute; right: 3px; bottom: 3px; padding: 2px 4px; border-radius: 3px; background: rgba(0, 0, 0, .66); color: #f2f2f0; font: 500 10.5px/1 var(--font-mono); font-variant-numeric: tabular-nums; }
  .sb-q { font: 500 14.5px/1.45 var(--font-jp); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .sb-q mark { font-weight: 700; color: inherit; background: linear-gradient(transparent 58%, var(--mark-soft) 58%); }
  .sb-note { display: flex; align-items: center; gap: 8px; padding: 4px 10px 8px; font-size: 12.5px; color: var(--ink-3); }
  .dark .sb-field { background: var(--vid-hover); border-color: var(--vid-edge); color: var(--vid-ink-2); }
  .dark .sb-field:focus-within { border-color: var(--vid-ink-2); }
  .dark .sb-field input { color: var(--vid-ink); }
  .dark kbd { color: var(--vid-ink-2); border-color: var(--vid-edge); }
</style>
