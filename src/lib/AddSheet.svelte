<script lang="ts">
  import { onMount } from "svelte";
  import { call, errorText, type Anime, type ScanResult, type ScannedFile } from "./api";
  import { clock, plural } from "./format";
  import Icon from "./Icon.svelte";

  /** `scanned`: the folder's scan, when it was started before the sheet opened (the setup starts it while you go on). */
  let { folder, scanned, onclose, onadded }: { folder: string; scanned?: Promise<ScanResult>; onclose: () => void; onadded: (id: number) => void } = $props();

  type Phase = "scanning" | "groups" | "match" | "search" | "adding";
  let phase = $state<Phase>("scanning");
  let result = $state<ScanResult | null>(null);
  let error = $state("");
  let chosen = $state<Anime | null>(null);
  let files = $state<(ScannedFile & { include: boolean })[]>([]);
  let byOrder = $state(false);
  let startAt = $state(1);
  let showAll = $state(false);
  let query = $state("");
  let found = $state<Anime[]>([]);
  let searching = $state(false);
  /** The name last searched for, once AniList has answered. */
  let searched = $state("");
  let dragging = $state<number | null>(null);

  async function scan(path: string, started?: Promise<ScanResult>) {
    phase = "scanning";
    error = "";
    try {
      const r = await (started ?? call<ScanResult>("scan", { folder: path }));
      result = r;
      files = r.scan.files.map((f) => ({ ...f, include: f.number !== null }));
      byOrder = r.scan.numbering === "order";
      query = r.scan.guess;
      chosen = r.matches[0] ?? null;
      if (r.error) error = r.error;
      phase = r.scan.groups.length >= 2 ? "groups" : "match";
    } catch (e) {
      error = errorText(e);
      phase = "match";
    }
  }

  onMount(() => {
    scan(folder, scanned);
    const key = (e: KeyboardEvent) => e.key === "Escape" && onclose();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  });

  const included = $derived(files.filter((f) => f.include));
  /** The episode each included file becomes. */
  function numberOf(f: ScannedFile & { include: boolean }): number | null {
    if (!f.include) return null;
    if (!byOrder && f.parsed !== null) return f.parsed;
    return startAt + included.indexOf(f);
  }
  const numbers = $derived(files.map(numberOf));
  const clash = $derived.by(() => {
    const seen = new Set<number>();
    for (const n of numbers) {
      if (n === null) continue;
      if (seen.has(n)) return true;
      seen.add(n);
    }
    return false;
  });
  const visible = $derived(showAll ? files : files.slice(0, 12));
  const searchList = $derived(searched ? found : (result?.matches ?? []));

  function drop(target: number) {
    if (dragging === null || dragging === target) return;
    const list = [...files];
    const [moved] = list.splice(dragging, 1);
    list.splice(target, 0, moved);
    files = list;
    byOrder = true;
    dragging = null;
  }

  async function search() {
    if (!query.trim()) return;
    searching = true;
    try {
      const name = query.trim();
      found = await call<Anime[]>("searchAnilist", { query: name, count: included.length });
      searched = name;
    } catch (e) {
      error = errorText(e);
    } finally {
      searching = false;
    }
  }

  async function add() {
    if (!result || included.length === 0 || clash) return;
    phase = "adding";
    error = "";
    try {
      const episodes = files
        .map((f, i) => ({ number: numbers[i], path: f.path, size: f.size, duration: f.duration }))
        .filter((e) => e.number !== null);
      const { id } = await call<{ id: number }>("addShow", {
        folder: result.scan.folder,
        title: chosen ? "" : result.scan.guess,
        anilistId: chosen?.id ?? null,
        episodes,
      });
      onadded(id);
    } catch (e) {
      error = errorText(e);
      phase = "match";
    }
  }

  function meta(a: Anime): string {
    const parts = [a.format.replace("_", " "), a.year ? String(a.year) : "", a.episodes ? `${a.episodes} episodes` : ""];
    return parts.filter(Boolean).join(" · ");
  }
</script>

<div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && phase !== "adding" && onclose()}>
  <div class="sheet" role="dialog" aria-modal="true" aria-label="Add media">
    {#if phase === "scanning"}
      <div class="busy"><span class="spin"></span><span>Reading <span class="mono">{folder}</span>…</span></div>
    {:else if phase === "groups" && result}
      <div class="head-text">
        <span class="kicker">Add media</span>
        <h3>This folder holds more than one season</h3>
        <p>Each season is its own show on AniList, so add them one at a time. Which one first?</p>
      </div>
      <div class="groups">
        {#each result.scan.groups as g (g.folder)}
          <button type="button" class="group" onclick={() => scan(g.folder)}>
            <Icon name="folder" /><span>{g.name}</span><small>{g.videos} videos</small>
          </button>
        {/each}
      </div>
      <div class="sheet-foot">
        <button type="button" class="btn quiet" onclick={() => (phase = "match")}>Add whole folder as one show</button>
        <button type="button" class="btn quiet" onclick={onclose}>Cancel</button>
      </div>
    {:else if phase === "search"}
      <div class="head-text">
        <span class="kicker">Which show is it?</span>
        <form class="searchrow" onsubmit={(e) => { e.preventDefault(); search(); }}>
          <input class="input grow" bind:value={query} placeholder="Search AniList" aria-label="Search AniList" />
          <button type="submit" class="btn" disabled={searching}>{searching ? "Searching…" : "Search"}</button>
        </form>
      </div>
      <div class="results scroll">
        {#each searchList as a (a.id)}
          <button type="button" class="result" class:on={chosen?.id === a.id} onclick={() => { chosen = a; phase = "match"; }}>
            {#if a.cover}<img src={a.cover} alt="" />{:else}<span class="noimg"></span>{/if}
            <span class="r-body">
              <span class="r-jp" lang="ja">{a.native || a.romaji}</span>
              <span class="r-en">{a.english || a.romaji}</span>
              <span class="r-meta">{meta(a)}</span>
            </span>
          </button>
        {:else}
          <p class="sheet-note">{searched ? `Nothing found for “${searched}”. Try another name.` : "Nothing found yet. Try another name."}</p>
        {/each}
      </div>
      <div class="sheet-foot">
        <button type="button" class="btn quiet" onclick={() => { chosen = null; phase = "match"; }}>Add without AniList</button>
        <button type="button" class="btn quiet" onclick={() => (phase = "match")}>Back</button>
      </div>
    {:else if result}
      <div class="sheet-head">
        {#if chosen?.cover}<img class="poster" src={chosen.cover} alt="" />{:else}<span class="poster none"></span>{/if}
        <div class="head-text">
          <span class="kicker">{chosen ? "Is this the show?" : "Not matched on AniList"}</span>
          {#if chosen}
            <h3 lang="ja">{chosen.native || chosen.romaji}</h3>
            <p>{chosen.english || chosen.romaji} · {meta(chosen)}</p>
          {:else}
            <h3>{result.scan.guess || "Untitled"}</h3>
            <p>Telly can still play it, but Jimaku finds subtitles by AniList’s entry.</p>
          {/if}
          <p class="folder mono">{result.scan.folder}</p>
        </div>
        <button type="button" class="link-btn" onclick={() => { phase = "search"; if (!searched) search(); }}>{chosen ? "Choose another show" : "Find it on AniList"}</button>
      </div>

      <div class="map-top">
        <b>{plural(included.length, "episode")}</b>
        <span>{byOrder ? "In folder order. Drag rows to reorder them." : "Numbered from the file names."}</span>
        {#if byOrder}
          <label class="start">Starts at episode <input class="input" type="number" min="1" bind:value={startAt} /></label>
        {:else}
          <button type="button" class="link-btn" onclick={() => (byOrder = true)}>Number in folder order</button>
        {/if}
      </div>
      <div class="map" role="list">
        <div class="map-row head"><span></span><span>File</span><span>Length</span><span>Becomes</span></div>
        {#each visible as f, i (f.path)}
          <div
            class="map-row"
            role="listitem"
            draggable={f.include}
            class:dragging={dragging === i}
            ondragstart={() => (dragging = i)}
            ondragover={(e) => e.preventDefault()}
            ondrop={() => drop(i)}
            ondragend={() => (dragging = null)}
          >
            <span class="grip" aria-hidden="true">{#if f.include}<Icon name="grip" />{/if}</span>
            <span class="f" title={f.path}>{f.rel}</span>
            <span class="len">{f.duration > 0 ? clock(f.duration) : "?"}</span>
            <button type="button" class="to" class:out={!f.include} title={f.include ? "Leave it out" : f.reason || "Add it back"} onclick={() => (f.include = !f.include)}>
              {#if f.include}Episode {numbers[files.indexOf(f)]}{:else}Left out{#if f.reason} · {f.reason}{/if}{/if}
            </button>
          </div>
        {/each}
        {#if files.length > 12 && !showAll}
          <button type="button" class="map-more" onclick={() => (showAll = true)}>Show All {files.length} Files</button>
        {/if}
      </div>
      {#if !result.ffprobe}<p class="err">Telly can’t find ffprobe, so it couldn’t measure the videos. Choose ffmpeg in Settings first.</p>{/if}
      {#if clash}<p class="err">Two files would be the same episode. Leave one out, or number them in order.</p>{/if}
      {#if error}<p class="err">{error}</p>{/if}
      <p class="sheet-note">Telly never moves or renames the files. Its copies, subtitles, and covers go in its own folder.</p>
      <div class="sheet-foot">
        <button type="button" class="btn quiet" onclick={onclose}>Cancel</button>
        <button type="button" class="btn" onclick={add} disabled={included.length === 0 || clash}><Icon name="plus" /><span>Add to library</span></button>
      </div>
    {:else if phase === "adding"}
      <div class="busy"><span class="spin"></span><span>Adding…</span></div>
    {:else}
      <p class="err">{error || "Telly couldn’t read that folder."}</p>
      <div class="sheet-foot"><button type="button" class="btn quiet" onclick={onclose}>Close</button></div>
    {/if}
  </div>
</div>

<style>
  .busy { display: flex; align-items: center; gap: 12px; padding: 20px 4px; color: var(--ink-2); }
  .kicker { font-size: 12.5px; color: var(--ink-3); }
  .head-text { display: grid; gap: 4px; min-width: 0; }
  .head-text h3 { font: 800 25px/1.2 var(--font-jp-title); margin: 2px 0; }
  .head-text p { font-size: 13.5px; color: var(--ink-2); }
  .folder { font-size: 12px; color: var(--ink-3) !important; overflow-wrap: anywhere; }
  .sheet-head { display: grid; grid-template-columns: 86px minmax(0, 1fr) auto; gap: 18px; align-items: center; }
  .poster { width: 86px; aspect-ratio: 135 / 190; object-fit: cover; border-radius: 3px; box-shadow: 0 12px 22px -14px rgba(0, 0, 0, .6); background: var(--sunk); }
  .poster.none { display: block; }
  .map-top { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; font-size: 13px; color: var(--ink-2); }
  .map-top b { color: var(--ink); font-weight: 600; }
  .start { display: inline-flex; align-items: center; gap: 8px; margin-left: auto; }
  .start .input { width: 72px; height: 30px; }
  .map { border-top: 1px solid var(--rule); }
  .map-row { display: grid; grid-template-columns: 18px minmax(0, 1fr) 64px 200px; gap: 10px; padding: 7px 4px; border-bottom: 1px solid var(--rule); font-size: 13px; align-items: center; }
  .map-row.head { font: 600 12px/1 var(--font-ui); color: var(--ink-3); padding: 9px 4px; }
  .map-row.dragging { opacity: .4; }
  .map-row[draggable="true"] { cursor: grab; }
  .grip { color: var(--ink-3); display: grid; }
  .grip :global(svg) { width: 14px; height: 14px; }
  .f { font-family: var(--font-mono); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; direction: rtl; text-align: left; }
  .len { font-variant-numeric: tabular-nums; color: var(--ink-2); }
  .to { all: unset; cursor: pointer; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; border-radius: 4px; padding: 2px 4px; margin: -2px -4px; }
  .to:hover { background: var(--sunk); }
  .to.out { color: var(--ink-3); font-weight: 500; }
  .map-more { all: unset; cursor: pointer; display: block; padding: 9px 4px; border-bottom: 1px solid var(--rule); color: var(--ink-2); font-size: 13px; }
  .map-more:hover { color: var(--ink); }
  .groups { display: grid; gap: 6px; }
  .group { all: unset; display: grid; grid-template-columns: 20px minmax(0, 1fr) auto; gap: 10px; align-items: center; padding: 10px 12px; border: 1px solid var(--rule-2); border-radius: 6px; cursor: pointer; }
  .group:hover { background: var(--sunk); }
  .group :global(svg) { width: 18px; height: 18px; color: var(--ink-3); }
  .group small { color: var(--ink-3); }
  .searchrow { display: flex; gap: 8px; margin-top: 6px; }
  .results { display: grid; gap: 4px; max-height: 420px; overflow: auto; }
  .result { all: unset; display: grid; grid-template-columns: 46px minmax(0, 1fr); gap: 12px; align-items: center; padding: 6px; border-radius: 6px; cursor: pointer; }
  .result:hover, .result.on { background: var(--sunk); }
  .result img, .result .noimg { width: 46px; aspect-ratio: 135 / 190; object-fit: cover; border-radius: 2px; background: var(--sunk); display: block; }
  .r-body { display: grid; gap: 2px; min-width: 0; }
  .r-jp { font: 800 15px/1.25 var(--font-jp-title); }
  .r-en { font-size: 13px; }
  .r-meta { font-size: 12px; color: var(--ink-3); }
</style>
