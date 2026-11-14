<script lang="ts">
  import { onMount } from "svelte";
  import { call, errorText, type Anime, type ScanPart, type ScanResult, type ScannedFile } from "./api";
  import { clock, plural } from "./format";
  import Icon from "./Icon.svelte";

  /** `scanned`: the folder's scan, when it was started before the sheet opened (the setup starts it while you go on). */
  let { folder, scanned, onclose, onadded }: { folder: string; scanned?: Promise<ScanResult>; onclose: () => void; onadded: (id: number) => void } = $props();

  type File = ScannedFile & { include: boolean };
  /** One part of the folder (a season's folder, a movie, an OVA) as the sheet has it: what's chosen for it. */
  interface PartState {
    part: ScanPart;
    include: boolean;
    anime: Anime | null;
    files: File[];
    byOrder: boolean;
    startAt: number;
  }

  type Phase = "scanning" | "parts" | "match" | "search" | "adding";
  let phase = $state<Phase>("scanning");
  let result = $state<ScanResult | null>(null);
  let error = $state("");
  /** The folder's parts, when it holds more than one show: each is added as a show of its own. */
  let parts = $state<PartState[]>([]);
  /** The part whose show and files are open, or null when the folder is open as one show. */
  let editing = $state<number | null>(null);
  /** What a search chooses: the show the open files are, or the anime the whole folder is. */
  let choosing = $state<"show" | "anime">("show");
  // The show and files that are open: the folder's, or those of the part being changed.
  let chosen = $state<Anime | null>(null);
  let files = $state<File[]>([]);
  let byOrder = $state(false);
  let startAt = $state(1);
  let showAll = $state(false);
  let query = $state("");
  let found = $state<Anime[]>([]);
  let searching = $state(false);
  /** The name last searched for, once AniList has answered. */
  let searched = $state("");
  let dragging = $state<number | null>(null);

  /** Takes a scan's answer: the folder as one show, and its parts when it has several. */
  function take(r: ScanResult) {
    result = r;
    parts = r.parts.map((part) => ({
      part,
      include: true,
      anime: part.anime,
      files: part.files.map((f) => ({ ...f, include: f.number !== null })),
      byOrder: part.numbering === "order",
      startAt: 1,
    }));
    openFolder(r);
    if (r.error) error = r.error;
    phase = r.parts.length > 1 ? "parts" : "match";
  }

  /** Opens the whole folder as one show. */
  function openFolder(r: ScanResult) {
    editing = null;
    files = r.scan.files.map((f) => ({ ...f, include: f.number !== null }));
    byOrder = r.scan.numbering === "order";
    startAt = 1;
    query = r.scan.guess;
    chosen = r.matches[0] ?? null;
    showAll = false;
    found = [];
    searched = "";
  }

  async function scan(path: string, started?: Promise<ScanResult>, anilistId?: number) {
    phase = "scanning";
    error = "";
    try {
      take(await (started ?? call<ScanResult>("scan", { folder: path, anilistId })));
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

  /** The episode each file becomes (null for one left out): its name's number, or its place in the order. */
  function numbersFor(list: File[], order: boolean, start: number): (number | null)[] {
    let at = 0;
    return list.map((f) => {
      if (!f.include) return null;
      const place = at++;
      return !order && f.parsed !== null ? f.parsed : start + place;
    });
  }

  /** Whether two files would be the same episode. */
  const clashIn = (numbers: (number | null)[]) => {
    const given = numbers.filter((n) => n !== null);
    return new Set(given).size !== given.length;
  };

  const included = $derived(files.filter((f) => f.include));
  const numbers = $derived(numbersFor(files, byOrder, startAt));
  const clash = $derived(clashIn(numbers));
  const visible = $derived(showAll ? files : files.slice(0, 12));
  /** Before a search, the anime's own seasons, movies and OVAs come first: the show wanted is usually one of them. */
  const searchList = $derived.by(() => {
    if (searched) return found;
    const matches = result?.matches ?? [];
    if (choosing === "anime") return matches;
    const entries = result?.entries ?? [];
    return [...entries, ...matches.filter((m) => !entries.some((e) => e.id === m.id))];
  });

  // ---- The folder's parts ----

  const partNumbers = (p: PartState) => numbersFor(p.files, p.byOrder, p.startAt);
  const episodesIn = (p: PartState) => p.files.filter((f) => f.include).length;
  const adding = $derived(parts.filter((p) => p.include));
  /** Whether every part to add has episodes, each a different one. */
  const partsReady = $derived(adding.length > 0 && adding.every((p) => episodesIn(p) > 0 && !clashIn(partNumbers(p))));
  /** How many of the folder's videos aren't being added: extras, previews, and parts unticked. */
  const leftOut = $derived((result?.scan.files.length ?? 0) - adding.reduce((n, p) => n + episodesIn(p), 0));

  /** Opens a part, to change its show or its files. */
  function openPart(i: number) {
    const p = parts[i];
    editing = i;
    chosen = p.anime;
    files = p.files.map((f) => ({ ...f }));
    byOrder = p.byOrder;
    startAt = p.startAt;
    showAll = false;
    found = [];
    searched = "";
    query = p.anime ? p.anime.english || p.anime.romaji : (result?.anime?.romaji ?? result?.scan.guess ?? "");
    phase = "match";
  }

  /** Keeps what was changed in the open part, and goes back to the list. */
  function closePart() {
    if (editing !== null) parts[editing] = { ...parts[editing], anime: chosen, files, byOrder, startAt };
    editing = null;
    phase = "parts";
  }

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
      found = await call<Anime[]>("searchAnilist", { query: name, count: choosing === "anime" ? 0 : included.length });
      searched = name;
    } catch (e) {
      error = errorText(e);
    } finally {
      searching = false;
    }
  }

  /** Opens the search, for the open files' show or for the anime the folder is. */
  function choose(what: "show" | "anime") {
    choosing = what;
    if (what === "anime") {
      found = [];
      searched = "";
      query = result?.scan.guess ?? "";
    }
    phase = "search";
    // A part's list opens on the anime's own entries; the folder's, on what its name finds.
    if (!searched && (what === "anime" || !result?.entries.length)) search();
  }

  function pick(a: Anime | null) {
    if (choosing === "anime") {
      // The folder's parts are matched again, to this anime's seasons and movies.
      if (a && result) scan(result.scan.folder, undefined, a.id);
      return;
    }
    chosen = a;
    phase = "match";
  }

  const episodesOf = (list: File[], given: (number | null)[]) =>
    list.map((f, i) => ({ number: given[i], path: f.path, size: f.size, duration: f.duration })).filter((e) => e.number !== null);

  async function add() {
    if (!result || included.length === 0 || clash) return;
    phase = "adding";
    error = "";
    try {
      const { id } = await call<{ id: number }>("addShow", {
        folder: result.scan.folder,
        title: chosen ? "" : result.scan.guess,
        anilistId: chosen?.id ?? null,
        episodes: episodesOf(files, numbers),
      });
      onadded(id);
    } catch (e) {
      error = errorText(e);
      phase = "match";
    }
  }

  async function addParts() {
    if (!result || !partsReady) return;
    phase = "adding";
    error = "";
    try {
      const { ids } = await call<{ ids: number[] }>("addShows", {
        shows: adding.map((p) => ({
          folder: p.part.folder,
          title: p.anime ? "" : p.part.title,
          anilistId: p.anime?.id ?? null,
          shared: p.part.shared,
          episodes: episodesOf(p.files, partNumbers(p)),
        })),
        // A part AniList doesn't know is listed with the rest of the anime.
        anime: result.anime?.id ?? null,
      });
      onadded(ids[0]);
    } catch (e) {
      error = errorText(e);
      phase = "parts";
    }
  }

  const FORMATS: Record<string, string> = { TV: "TV", TV_SHORT: "TV short", MOVIE: "Movie", SPECIAL: "Special", OVA: "OVA", ONA: "ONA", MUSIC: "Music" };

  function meta(a: Anime): string {
    const count = a.format !== "MOVIE" && a.episodes ? plural(a.episodes, "episode") : "";
    return [FORMATS[a.format] ?? a.format.replace("_", " "), a.year ? String(a.year) : "", count].filter(Boolean).join(" · ");
  }
</script>

<div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && phase !== "adding" && onclose()}>
  <div class="sheet" role="dialog" aria-modal="true" aria-label="Add media">
    {#if phase === "scanning"}
      <div class="busy"><span class="spin"></span><span>Reading <span class="mono">{folder}</span>…</span></div>
    {:else if phase === "adding"}
      <div class="busy"><span class="spin"></span><span>Adding…</span></div>
    {:else if phase === "parts" && result}
      {@const anime = result.anime}
      <div class="sheet-head">
        {#if anime?.cover}<img class="poster" src={anime.cover} alt="" />{:else}<span class="poster none"></span>{/if}
        <div class="head-text">
          <span class="kicker">{anime ? "Is this the anime?" : "Not matched on AniList"}</span>
          <h3 lang={anime ? "ja" : undefined}>{anime ? anime.native || anime.romaji : result.scan.guess || "Untitled"}</h3>
          <p>{anime ? `${anime.english || anime.romaji} · ` : ""}{plural(parts.length, "part")} in this folder, each added as a show of its own</p>
          <p class="folder mono">{result.scan.folder}</p>
        </div>
        <button type="button" class="link-btn" onclick={() => choose("anime")}>{anime ? "Choose another anime" : "Find it on AniList"}</button>
      </div>

      <div class="parts scroll" role="list">
        {#each parts as p, i (p.part.name)}
          {@const count = episodesIn(p)}
          <div class="part" class:off={!p.include} role="listitem">
            <input type="checkbox" bind:checked={p.include} aria-label="Add {p.part.name}" />
            <span class="p-from">
              <span class="mono" title={p.part.folder}>{p.part.name}</span>
              <small>{plural(count, "episode")}{p.files.length > count ? ` · ${p.files.length - count} left out` : ""}</small>
            </span>
            <span class="p-to">
              {#if p.anime}
                <span class="p-jp" lang="ja">{p.anime.native || p.anime.romaji}</span>
                <small>{p.anime.english || p.anime.romaji} · {meta(p.anime)}</small>
              {:else}
                <span>Not matched on AniList</span>
                <small>Added as “{p.part.title}”</small>
              {/if}
              {#if clashIn(partNumbers(p))}
                <small class="warn">Two files would be the same episode.</small>
              {:else if p.part.note && p.anime?.id === p.part.anime?.id}
                <small class="warn">{p.part.note}</small>
              {/if}
            </span>
            <button type="button" class="link-btn" onclick={() => openPart(i)} title="Choose another show for it, or change its files">Change…</button>
          </div>
        {/each}
      </div>
      {#if leftOut > 0}<p class="sheet-note">{plural(leftOut, "video")} in the folder {leftOut === 1 ? "isn’t" : "aren’t"} being added: extras, previews and whatever is unticked. Change… lists a part’s files.</p>{/if}
      {#if !result.ffprobe}<p class="err">Telly can’t find ffprobe, so it couldn’t measure the videos. Choose ffmpeg in Settings first.</p>{/if}
      {#if error}<p class="err">{error}</p>{/if}
      <p class="sheet-note">Telly never moves or renames the files. Its copies, subtitles, and covers go in its own folder.</p>
      <div class="sheet-foot">
        <button type="button" class="btn quiet" onclick={() => { if (result) openFolder(result); phase = "match"; }}>Add whole folder as one show</button>
        <button type="button" class="btn quiet" onclick={onclose}>Cancel</button>
        <button type="button" class="btn" onclick={addParts} disabled={!partsReady}><Icon name="plus" /><span>Add {adding.length} to library</span></button>
      </div>
    {:else if phase === "search"}
      <div class="head-text">
        <span class="kicker">{choosing === "anime" ? "Which anime is it?" : "Which show is it?"}</span>
        <form class="searchrow" onsubmit={(e) => { e.preventDefault(); search(); }}>
          <input class="input grow" bind:value={query} placeholder="Search AniList" aria-label="Search AniList" />
          <button type="submit" class="btn" disabled={searching}>{searching ? "Searching…" : "Search"}</button>
        </form>
      </div>
      <div class="results scroll">
        {#each searchList as a (a.id)}
          <button type="button" class="result" class:on={chosen?.id === a.id} onclick={() => pick(a)}>
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
        {#if choosing === "show"}
          <button type="button" class="btn quiet" onclick={() => pick(null)}>Add without AniList</button>
        {/if}
        <button type="button" class="btn quiet" onclick={() => (phase = choosing === "anime" ? "parts" : "match")}>Back</button>
      </div>
    {:else if result}
      {@const part = editing !== null ? parts[editing].part : null}
      <div class="sheet-head">
        {#if chosen?.cover}<img class="poster" src={chosen.cover} alt="" />{:else}<span class="poster none"></span>{/if}
        <div class="head-text">
          <span class="kicker">{chosen ? "Is this the show?" : "Not matched on AniList"}</span>
          {#if chosen}
            <h3 lang="ja">{chosen.native || chosen.romaji}</h3>
            <p>{chosen.english || chosen.romaji} · {meta(chosen)}</p>
          {:else}
            <h3>{part ? part.title : result.scan.guess || "Untitled"}</h3>
            <p>Telly can still play it, but Jimaku finds subtitles by AniList’s entry.</p>
          {/if}
          <p class="folder mono">{part ? part.name : result.scan.folder}</p>
        </div>
        <button type="button" class="link-btn" onclick={() => choose("show")}>{chosen ? "Choose another show" : "Find it on AniList"}</button>
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
      {#if part}
        <div class="sheet-foot">
          <button type="button" class="btn" onclick={closePart}>Done</button>
        </div>
      {:else}
        <p class="sheet-note">Telly never moves or renames the files. Its copies, subtitles, and covers go in its own folder.</p>
        <div class="sheet-foot">
          {#if parts.length > 1}
            <button type="button" class="btn quiet" onclick={() => (phase = "parts")}>Show its {parts.length} parts</button>
          {/if}
          <button type="button" class="btn quiet" onclick={onclose}>Cancel</button>
          <button type="button" class="btn" onclick={add} disabled={included.length === 0 || clash}><Icon name="plus" /><span>Add to library</span></button>
        </div>
      {/if}
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
  .parts { border-top: 1px solid var(--rule); max-height: 420px; overflow: auto; }
  .part { display: grid; grid-template-columns: 18px minmax(0, 5fr) minmax(0, 6fr) auto; gap: 12px; align-items: center; padding: 9px 4px; border-bottom: 1px solid var(--rule); font-size: 13px; }
  .part.off .p-from, .part.off .p-to { opacity: .45; }
  .part input { margin: 0; }
  .p-from, .p-to { display: grid; gap: 2px; min-width: 0; }
  .p-from .mono { font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .part small { font-size: 12px; color: var(--ink-3); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .part small.warn { color: var(--warn); white-space: normal; }
  .p-jp { font: 800 14.5px/1.3 var(--font-jp-title); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
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
