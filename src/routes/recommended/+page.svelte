<script lang="ts">
  import { goto } from "$app/navigation";
  import { onMount, tick } from "svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, callKept, errorText, inTauri, kept, setWindowTitle, type Pick, type Recommended } from "$lib/api";
  import { plural } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  /** How many picks show at first, and how many Show more adds. */
  const STEP = 12;
  const FORMATS: Record<string, string> = { TV: "TV", TV_SHORT: "TV short", MOVIE: "Film", SPECIAL: "Special", OVA: "OVA", ONA: "ONA", MUSIC: "Music" };

  let rec = $state.raw<Recommended | null>(kept<Recommended>("recommendations"));
  let shown = $state(STEP);
  let error = $state("");
  let asking = $state(false);
  /** The pick whose sheet is open, and the card that opened it. */
  let open = $state.raw<Pick | null>(null);
  let opener: HTMLElement | null = null;
  let sheet = $state<HTMLElement | null>(null);
  let grid = $state<HTMLElement | null>(null);

  async function load() {
    asking = true;
    try {
      rec = await callKept<Recommended>("recommendations");
      error = "";
    } catch (e) {
      error = errorText(e);
    } finally {
      asking = false;
    }
  }

  onMount(() => {
    app.dark = false;
    setWindowTitle("Recommended — Telly");
    load();
    const key = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  });

  async function show(p: Pick, card: HTMLElement) {
    open = p;
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

  /** Hides a pick; focus goes to the one that takes its place. */
  async function hide(p: Pick) {
    const at = rec?.picks.indexOf(p) ?? 0;
    try {
      await call("hideRecommendation", { id: p.id });
      open = null;
      await load();
      await tick();
      grid?.querySelectorAll<HTMLElement>(".pick")[at]?.focus();
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

  /** "Film · 2016", "TV · 2011 · 148 episodes". */
  function meta(p: Pick): string {
    const episodes = p.format !== "MOVIE" && p.episodes ? plural(p.episodes, "episode") : "";
    return [FORMATS[p.format] ?? p.format, p.year ? String(p.year) : "", episodes].filter(Boolean).join(" · ");
  }

  /** "Like Your Name. and 10 more". */
  function why(p: Pick): string {
    const [first, second] = p.from;
    if (!second) return `Like ${first.title}`;
    if (p.from.length === 2) return `Like ${first.title} and ${second.title}`;
    return `Like ${first.title} and ${p.from.length - 1} more`;
  }

  /** AniList's synopsis as plain text: its line breaks kept, the rest of its markup dropped. */
  function plain(html: string): string {
    const doc = new DOMParser().parseFromString(html.replace(/<br\s*\/?>/gi, "\n"), "text/html");
    return (doc.body.textContent ?? "").replace(/\n{3,}/g, "\n\n").trim();
  }
</script>

<div class="rec scroll" inert={!!open}>
  <div class="rec-top">
    <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Library</span></button>
  </div>
  <div class="rec-head">
    <h2>Recommended</h2>
    {#if rec && rec.shows}<span>From {plural(rec.shows, "show")}</span>{/if}
  </div>

  {#if error}
    <p class="err">{error}</p>
    <button type="button" class="btn quiet retry" onclick={load} disabled={asking}>Try again</button>
  {/if}
  {#if !rec}
    {#if asking}<p class="note"><span class="spin"></span>Asking AniList…</p>{/if}
  {:else if rec.shows === 0}
    <p class="note">No show in the library is matched on AniList yet.</p>
  {:else if rec.picks.length === 0}
    <p class="note">AniList has no recommendations for these shows.</p>
  {:else}
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
        <h4>Recommended from</h4>
        <ul>
          {#each p.from as f, i (i)}
            <li><span title={f.title}>{f.title}</span><span>{f.votes.toLocaleString("en-GB")} {f.votes === 1 ? "vote" : "votes"}</span></li>
          {/each}
        </ul>
      </div>
      <div class="sheet-foot">
        <button type="button" class="btn quiet" onclick={() => hide(p)}>Hide</button>
        <span class="grow"></span>
        <button type="button" class="btn quiet" onclick={close}>Close</button>
        <button type="button" class="btn quiet" onclick={() => openOnAniList(p)}>Open on AniList</button>
        <button type="button" class="btn" onclick={() => goto(`/downloads?from=recommended&q=${encodeURIComponent(p.romaji || p.english || p.title)}`)} title="Find it on nyaa.si, most shared first, and pick a release for Telly to download"><Icon name="download" /><span>Download…</span></button>
      </div>
    </div>
  </div>
{/if}

<style>
  .rec { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 40px; }
  .rec-top { display: flex; align-items: center; height: 38px; margin-bottom: 24px; }
  .rec-head { display: flex; align-items: baseline; gap: 10px; margin: 0 0 14px; }
  .rec-head h2 { font: 600 17px/1.2 var(--font-ui); }
  .rec-head span { font-size: 13px; color: var(--ink-2); }
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
  .more { margin-top: 28px; }
  .sheet:focus { outline: none; }
  .pk-head { display: grid; grid-template-columns: 132px minmax(0, 1fr); gap: 20px; align-items: start; }
  .pk-text { display: grid; gap: 4px; min-width: 0; }
  .pk-text h3 { font: 800 24px/1.25 var(--font-jp-title); }
  .pk-text p { font-size: 14px; color: var(--ink-2); }
  .pk-text .pk-meta { font-size: 13px; }
  .pk-text .pk-about { margin-top: 10px; line-height: 1.5; max-width: 62ch; white-space: pre-line; }
  .pk-from { display: grid; gap: 6px; }
  .pk-from h4 { font: 600 12.5px/1.2 var(--font-ui); color: var(--ink-2); }
  .pk-from ul { list-style: none; padding: 0; font-size: 13.5px; line-height: 1.4; }
  .pk-from li { display: flex; justify-content: space-between; align-items: baseline; gap: 16px; padding: 7px 0; border-top: 1px solid var(--rule); }
  .pk-from li span:first-child { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pk-from li span:last-child { flex: none; color: var(--ink-2); font-variant-numeric: tabular-nums; }
</style>
