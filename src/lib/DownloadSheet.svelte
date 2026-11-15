<script lang="ts">
  import { onMount } from "svelte";
  import { call, errorText, type Anime, type DownloadPlan, type Release } from "./api";
  import { plural } from "./format";
  import Icon from "./Icon.svelte";

  /** `onstarted` is told the folder the release is downloading into. */
  let { release, onclose, onstarted }: { release: Release; onclose: () => void; onstarted: (folder: string) => void } = $props();

  type Phase = "finding" | "match" | "search" | "starting";
  let phase = $state<Phase>("finding");
  /** Where the chosen show's download goes. */
  let plan = $state.raw<DownloadPlan | null>(null);
  /** The anime the release's name found, for the search to open on. */
  let matches = $state.raw<Anime[]>([]);
  /** The show the release is, or none to download it without one. */
  let chosen = $state<Anime | null>(null);
  let error = $state("");
  let query = $state("");
  let found = $state<Anime[]>([]);
  let searching = $state(false);
  /** The name last searched for, once AniList has answered. */
  let searched = $state("");

  /** Asks where the release goes: as the show its name finds, or as `anime`, in its usual library or `library`. */
  async function place(anime?: Anime, library?: number) {
    if (library === undefined) phase = "finding";
    error = "";
    try {
      const answer = await call<DownloadPlan>("placeDownload", { title: release.title, torrent: release.torrent, anilistId: anime?.id ?? null, library: library ?? null });
      if (!anime) matches = answer.matches;
      plan = answer;
      chosen = answer.anime;
      if (answer.error) error = answer.error;
    } catch (e) {
      error = errorText(e);
    }
    phase = "match";
  }

  onMount(() => {
    place();
    const key = (e: KeyboardEvent) => e.key === "Escape" && phase !== "starting" && onclose();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  });

  async function search() {
    if (!query.trim()) return;
    searching = true;
    try {
      const name = query.trim();
      found = await call<Anime[]>("searchAnilist", { query: name, count: 0 });
      searched = name;
    } catch (e) {
      error = errorText(e);
    } finally {
      searching = false;
    }
  }

  function openSearch() {
    query ||= chosen ? chosen.english || chosen.romaji : "";
    phase = "search";
  }

  function pick(a: Anime | null) {
    if (a) place(a);
    else {
      chosen = null;
      plan = null;
      phase = "match";
    }
  }

  async function start() {
    phase = "starting";
    error = "";
    try {
      const { folder } = await call<{ folder: string }>("download", { magnet: release.magnet, torrent: release.torrent, title: release.title, show: null, anilistId: chosen?.id ?? null, library: chosen ? (plan?.to.library ?? null) : null });
      onstarted(folder);
    } catch (e) {
      error = errorText(e);
      phase = "match";
    }
  }

  const FORMATS: Record<string, string> = { TV: "TV", TV_SHORT: "TV short", MOVIE: "Movie", SPECIAL: "Special", OVA: "OVA", ONA: "ONA", MUSIC: "Music" };

  function meta(a: Anime): string {
    const count = a.format !== "MOVIE" && a.episodes ? plural(a.episodes, "episode") : "";
    return [FORMATS[a.format] ?? a.format.replace("_", " "), a.year ? String(a.year) : "", count].filter(Boolean).join(" · ");
  }

  /** What happens to it once it's down. */
  const after = $derived.by(() => {
    const to = chosen ? plan?.to : null;
    if (!to) return "It goes in the downloads folder, and the Library offers it to add once it’s down.";
    if (to.show !== null) return "More of a show in your library. Each episode joins it as it finishes.";
    if (!to.joins) return "This release holds several seasons or movies. The Library offers them to add once it’s down.";
    return "Added to the library when its first file finishes.";
  });
  /** Whether there's no library for a new show to go in. */
  const noLibrary = $derived(!!chosen && !!plan && plan.to.show === null && !plan.libraries.length);
</script>

<div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && phase !== "starting" && onclose()}>
  <div class="sheet" role="dialog" aria-modal="true" aria-label="Download">
    {#if phase === "finding"}
      <div class="busy"><span class="spin"></span><span>Finding <span class="mono">{release.title}</span> on AniList…</span></div>
    {:else if phase === "starting"}
      <div class="busy"><span class="spin"></span><span>Starting…</span></div>
    {:else if phase === "search"}
      <div class="head-text">
        <span class="kicker">Which show is it?</span>
        <form class="searchrow" onsubmit={(e) => { e.preventDefault(); search(); }}>
          <input class="input grow" bind:value={query} placeholder="Search AniList" aria-label="Search AniList" />
          <button type="submit" class="btn" disabled={searching}>{searching ? "Searching…" : "Search"}</button>
        </form>
      </div>
      <div class="results scroll">
        {#each searched ? found : matches as a (a.id)}
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
      {#if error}<p class="err">{error}</p>{/if}
      <div class="sheet-foot">
        <button type="button" class="btn quiet" onclick={() => pick(null)}>Download without AniList</button>
        <button type="button" class="btn quiet" onclick={() => (phase = "match")}>Back</button>
      </div>
    {:else}
      <div class="sheet-head">
        {#if chosen?.cover}<img class="poster" src={chosen.cover} alt="" />{:else}<span class="poster none"></span>{/if}
        <div class="head-text">
          <span class="kicker">{chosen ? "Is this the show?" : "Not matched on AniList"}</span>
          {#if chosen}
            <h3 lang="ja">{chosen.native || chosen.romaji}</h3>
            <p>{chosen.english || chosen.romaji} · {meta(chosen)}</p>
          {:else}
            <h3>{release.title}</h3>
          {/if}
          <p class="folder mono">{chosen ? release.title : release.size}</p>
        </div>
        <button type="button" class="link-btn" onclick={openSearch}>{chosen ? "Choose another show" : "Find it on AniList"}</button>
      </div>
      {#if chosen && plan}
        <div class="where">
          {#if plan.to.show === null && plan.libraries.length}
            <label class="row">
              <span>Library</span>
              <select class="input" value={plan.to.library ?? 0} onchange={(e) => place(chosen ?? undefined, Number(e.currentTarget.value))}>
                {#each plan.libraries as l, i (i)}<option value={i}>{l.name}</option>{/each}
              </select>
            </label>
          {/if}
          <p class="row"><span>Goes in</span> <span class="mono">{plan.to.folder}</span></p>
        </div>
      {/if}
      <p class="sheet-note">
        {after}
        {#if noLibrary}<a href="/settings#libraries" class="link-btn">Add a library</a> to have it sorted into your library.{/if}
      </p>
      {#if error}<p class="err">{error}</p>{/if}
      <div class="sheet-foot">
        <button type="button" class="btn quiet" onclick={onclose}>Cancel</button>
        <button type="button" class="btn" onclick={start}><Icon name="download" /><span>Download</span></button>
      </div>
    {/if}
  </div>
</div>

<style>
  .busy { display: flex; align-items: center; gap: 12px; padding: 20px 4px; color: var(--ink-2); overflow-wrap: anywhere; }
  .kicker { font-size: 12.5px; color: var(--ink-3); }
  .head-text { display: grid; gap: 4px; min-width: 0; }
  .head-text h3 { font: 800 25px/1.2 var(--font-jp-title); margin: 2px 0; overflow-wrap: anywhere; }
  .head-text p { font-size: 13.5px; color: var(--ink-2); }
  .folder { font-size: 12px; color: var(--ink-3) !important; overflow-wrap: anywhere; }
  .sheet-head { display: grid; grid-template-columns: 86px minmax(0, 1fr) auto; gap: 18px; align-items: center; }
  .poster { width: 86px; aspect-ratio: 135 / 190; object-fit: cover; border-radius: 3px; box-shadow: 0 12px 22px -14px rgba(0, 0, 0, .6); background: var(--sunk); }
  .poster.none { display: block; }
  .where { display: grid; gap: 12px; padding: 12px 0; border-block: 1px solid var(--rule); font-size: 13px; color: var(--ink-2); }
  .where .row { display: grid; gap: 4px; }
  .where select { max-width: 320px; }
  .where .mono { font-size: 12.5px; color: var(--ink); overflow-wrap: anywhere; }
  .searchrow { display: flex; gap: 8px; margin-top: 6px; }
  .results { display: grid; gap: 4px; max-height: 420px; overflow: auto; }
  .result { all: unset; display: grid; grid-template-columns: 46px minmax(0, 1fr); gap: 12px; align-items: center; padding: 6px; border-radius: 6px; cursor: pointer; }
  .result:hover, .result.on { background: var(--sunk); }
  .result:focus-visible { outline: 2px solid var(--ink); }
  .result img, .result .noimg { width: 46px; aspect-ratio: 135 / 190; object-fit: cover; border-radius: 2px; background: var(--sunk); display: block; }
  .r-body { display: grid; gap: 2px; min-width: 0; }
  .r-jp { font: 800 15px/1.25 var(--font-jp-title); }
  .r-en { font-size: 13px; }
  .r-meta { font-size: 12px; color: var(--ink-3); }
</style>
