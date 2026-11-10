<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, errorText, pickFile, setWindowTitle, type Anime, type EpisodeRow, type ShowView } from "$lib/api";
  import { bytes, clock, mediaLabel, plural, statusOf, subsLabel } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  const id = $derived(Number(page.url.searchParams.get("id")));
  let view = $state<ShowView | null>(null);
  let error = $state("");
  let menu = $state<{ ep: EpisodeRow; x: number; y: number } | null>(null);
  let matching = $state(false);
  let query = $state("");
  let found = $state<Anime[]>([]);
  let busy = $state(false);

  async function load() {
    try {
      view = await call<ShowView>("show", { id });
      error = "";
    } catch (e) {
      error = errorText(e);
    }
  }

  onMount(() => {
    app.dark = false;
    let timer: ReturnType<typeof setTimeout>;
    const loop = async () => {
      await load();
      if (view) setWindowTitle(`${view.card.title} — Telly`);
      const busy = view?.episodes.some((e) => ["pending", "working"].includes(e.subsState) || e.mediaState === "working") ?? false;
      timer = setTimeout(loop, busy ? 1500 : 6000);
    };
    loop();
    const close = () => (menu = null);
    window.addEventListener("click", close);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("click", close);
    };
  });

  const card = $derived(view?.card ?? null);
  const st = $derived(card ? statusOf(card) : null);
  const next = $derived(card?.next ?? null);

  function play(ep: { id: number; mediaState: string }) {
    if (ep.mediaState === "ready") goto(`/watch?ep=${ep.id}`);
    else toast({ key: "notready", state: "info", title: "Not ready yet", text: "Telly is still getting this episode ready to play." });
  }

  function rowMenu(e: MouseEvent, ep: EpisodeRow) {
    e.preventDefault();
    e.stopPropagation();
    menu = { ep, x: Math.min(e.clientX, window.innerWidth - 240), y: Math.min(e.clientY, window.innerHeight - 220) };
  }

  async function act(fn: () => Promise<unknown>) {
    menu = null;
    try {
      await fn();
    } catch (e) {
      toast({ key: "showerr", state: "failed", title: "That didn’t work", text: errorText(e) });
    }
    load();
  }

  async function chooseSubtitle(ep: EpisodeRow) {
    const path = await pickFile("Subtitles for episode " + ep.number, ["srt", "ass", "ssa", "vtt"], "Subtitles");
    if (!path) return;
    toast({ key: "usesub", state: "working", title: "Syncing the file", text: path.split(/[\\/]/).pop() ?? "" });
    await act(() => call("useSubtitle", { id: ep.id, path }));
    toast({ key: "usesub", state: "done", title: "Subtitles synced", text: "" });
  }

  async function search() {
    busy = true;
    try {
      found = await call<Anime[]>("searchAnilist", { query, count: card?.episodes ?? 0 });
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  async function rematch(a: Anime) {
    matching = false;
    await act(() => call("rematch", { id, anilistId: a.id }));
  }

  async function freeSpace() {
    const r = await call<{ episodes: number; bytes: number }>("freeSpace", { show: id });
    toast({ key: "free", state: "done", title: r.episodes ? `Freed ${bytes(r.bytes)}` : "Nothing to free", text: r.episodes ? `Removed Telly’s copies of ${plural(r.episodes, "watched episode")}. The rips are untouched.` : "Telly only removes its copies of watched episodes." });
    load();
  }

  const missing = $derived((view?.episodes ?? []).filter((e) => ["none", "nokey", "failed"].includes(e.subsState)).length);

  async function whisperAll() {
    const r = await call<{ queued: number }>("whisperSubs", { show: id });
    toast({ key: "whisper", state: "info", title: `Whisper will make subtitles for ${plural(r.queued, "episode")}`, text: "One at a time, a few minutes each. They’re marked as machine-made." });
    load();
  }

  async function getModel() {
    await call("getWhisperModel");
    toast({ key: "whispermodel", state: "working", title: "Getting Whisper’s Japanese model", text: "190 MB. The title bar shows how it’s going." });
  }

  async function lookAgain() {
    toast({ key: "rescan", state: "working", title: "Looking for new episodes", text: card?.folder ?? "" });
    try {
      const r = await call<{ added: number }>("rescan", { id });
      toast({ key: "rescan", state: "done", title: r.added ? `Added ${plural(r.added, "episode")}` : "No new episodes", text: r.added ? "They’ll be ready to play soon." : "Telly checks the folder every 10 minutes too." });
    } catch (e) {
      toast({ key: "rescan", state: "failed", title: "Couldn’t read the folder", text: errorText(e) });
    }
    load();
  }

  async function remove() {
    if (!card || !confirm(`Remove ${card.title} from Telly? The video files stay where they are. Telly’s copies and subtitles are deleted.`)) return;
    await call("removeShow", { id });
    goto("/");
  }

  function tone(t: string) {
    return t ? `tone-${t}` : "";
  }
</script>

<div class="showpg scroll">
  <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Back to Library</span></button>
  {#if error}<p class="err">{error}</p>{/if}
  {#if card && st}
    <div class="sp-head">
      <Cover cover={card.cover} color={card.coverColor} native={card.titleNative} title={card.title} />
      <div>
        <h2 lang="ja">{card.titleNative || card.title}</h2>
        <p class="sp-meta">
          {card.title}{card.format ? ` · ${card.format.replace("_", " ")}` : ""}{card.year ? ` · ${card.year}` : ""}
          · {card.episodes}{card.anilistEpisodes ? ` of ${card.anilistEpisodes}` : ""} episodes here
        </p>
        <p class="sp-folder mono">{card.folder}</p>
        <div class="sp-actions">
          {#if next}
            <button type="button" class="btn" disabled={!next.ready} onclick={() => next && play({ id: next.episodeId, mediaState: next.mediaState })}>
              <Icon name="play" /><span>{next.ready ? (next.resume ? `Resume Episode ${next.number}` : `Watch Episode ${next.number}`) : st.sub}</span>
            </button>
          {/if}
          <span class="sp-state">
            {#if card.subsReady < card.episodes}
              <span class="spin"></span>Finding subtitles · {card.subsReady} of {card.episodes} done
            {:else if card.subsFound === card.episodes}
              <Icon name="check" />Every episode has subtitles
            {:else}
              Subtitles for {card.subsFound} of {card.episodes} episodes
            {/if}
            <span class="bar"><i style:width="{Math.round((100 * (card.subsReady < card.episodes ? card.subsReady : card.subsFound)) / Math.max(1, card.episodes))}%"></i></span>
          </span>
          <span class="grow"></span>
          <button type="button" class="btn quiet" onclick={() => { matching = true; query = card?.titleRomaji || card?.title || ""; found = []; search(); }}>Change Match…</button>
          {#if missing > 0 && view?.whisper.cli}
            {#if view.whisper.model}
              <button type="button" class="btn quiet" onclick={whisperAll} title="For the {missing} episodes without subtitles; marked as machine-made">Make Subtitles With Whisper</button>
            {:else}
              <button type="button" class="btn quiet" onclick={getModel} title="So Whisper can make subtitles Jimaku doesn’t have">Get Whisper’s Model</button>
            {/if}
          {/if}
          <button type="button" class="btn quiet" onclick={lookAgain}>Look for New Episodes</button>
          <button type="button" class="btn quiet" onclick={() => act(() => call("retrySubs", { show: id }))}>Find Subtitles Again</button>
          <button type="button" class="btn quiet" onclick={freeSpace}>Free Up Space</button>
          <button type="button" class="btn danger" onclick={remove}>Remove…</button>
        </div>
      </div>
    </div>

    <div class="eps">
      <div class="ep-row head"><span>#</span><span>Video</span><span>Subtitles</span><span>Sync</span><span class="right">Watched</span></div>
      {#each view?.episodes ?? [] as ep (ep.id)}
        {@const m = mediaLabel(ep.mediaState, ep.mediaPlan, ep.mediaProgress)}
        {@const s = subsLabel(ep.subsState, ep.subsScore)}
        <button type="button" class="ep-row" class:next={ep.isNext} onclick={() => play(ep)} oncontextmenu={(e) => rowMenu(e, ep)} title={ep.path}>
          <span class="ep-n">{ep.number}</span>
          <span class="cell {tone(m.tone)}" title={ep.mediaError || m.text}>
            {#if m.tone === "ok"}<Icon name="check" />{:else if ep.mediaState === "working"}<span class="spin"></span>{/if}
            <span>{m.text}{ep.duration > 0 ? ` · ${clock(ep.duration)}` : ""}</span>
          </span>
          <span class="cell {tone(s.tone)}" title={ep.subsFile || ep.subsDetail}>
            {#if s.tone === "ok"}<Icon name="check" />{:else if ep.subsState === "working"}<span class="spin"></span>{/if}
            <span>{s.text}</span>
          </span>
          <span class="cell muted" title={ep.subsFile || ep.subsDetail}><span>{["synced", "check", "manual", "failed", "none"].includes(ep.subsState) ? ep.subsDetail || ep.subsFile : ""}</span></span>
          <span class="ep-state" class:ok={ep.watched}>
            {#if ep.watched}<i></i>Watched{:else if ep.position > 20}<i class="part"></i>{clock(ep.position)}{:else if ep.isNext}Next{/if}
            <span class="more" role="presentation" onclick={(e) => rowMenu(e, ep)}><Icon name="more" /></span>
          </span>
        </button>
      {/each}
    </div>
  {/if}
</div>

{#if menu}
  {@const ep = menu.ep}
  <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px" role="menu">
    {#if ep.mediaState === "ready"}<button type="button" role="menuitem" onclick={() => goto(`/watch?ep=${ep.id}`)}>Play</button>{/if}
    <button type="button" role="menuitem" onclick={() => act(() => call("markWatched", { id: ep.id, watched: !ep.watched }))}>{ep.watched ? "Mark as Unwatched" : "Mark as Watched"}</button>
    <button type="button" role="menuitem" onclick={() => chooseSubtitle(ep)}>Use a Subtitle File…</button>
    <button type="button" role="menuitem" onclick={() => act(() => call("retrySubs", { id: ep.id }))}>Find Subtitles Again</button>
    {#if view?.whisper.cli && view.whisper.model}
      <button type="button" role="menuitem" onclick={() => act(() => call("whisperSubs", { id: ep.id }))}>Make Subtitles With Whisper</button>
    {/if}
    {#if ep.mediaState === "failed" || ep.mediaState === "ready"}
      <button type="button" role="menuitem" onclick={() => act(() => call("retryMedia", { id: ep.id }))}>{ep.mediaState === "failed" ? "Try Converting Again" : "Convert Again"}</button>
    {/if}
  </div>
{/if}

{#if matching}
  <div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && (matching = false)}>
    <div class="sheet" role="dialog" aria-modal="true" aria-label="Change Match">
      <form class="searchrow" onsubmit={(e) => { e.preventDefault(); search(); }}>
        <input class="input grow" bind:value={query} aria-label="Search AniList" />
        <button type="submit" class="btn" disabled={busy}>{busy ? "Searching…" : "Search"}</button>
      </form>
      <div class="results scroll">
        {#each found as a (a.id)}
          <button type="button" class="result" onclick={() => rematch(a)}>
            {#if a.cover}<img src={a.cover} alt="" />{:else}<span class="noimg"></span>{/if}
            <span class="r-body">
              <span class="r-jp" lang="ja">{a.native || a.romaji}</span>
              <span class="r-en">{a.english || a.romaji}</span>
              <span class="r-meta">{[a.format, a.year, a.episodes ? `${a.episodes} episodes` : ""].filter(Boolean).join(" · ")}</span>
            </span>
          </button>
        {/each}
      </div>
      <div class="sheet-foot"><button type="button" class="btn quiet" onclick={() => (matching = false)}>Cancel</button></div>
    </div>
  </div>
{/if}

<style>
  .showpg { position: absolute; inset: 0; overflow: auto; padding: 18px 32px 40px; }
  .sp-head { display: grid; grid-template-columns: 112px minmax(0, 1fr); gap: 24px; align-items: center; margin: 14px 0 26px; }
  .sp-head h2 { font: 800 30px/1.2 var(--font-jp-title); letter-spacing: .02em; }
  .sp-meta { font-size: 13.5px; color: var(--ink-2); margin-top: 4px; }
  .sp-folder { font-size: 12px; color: var(--ink-3); margin-top: 2px; overflow-wrap: anywhere; }
  .sp-actions { display: flex; align-items: center; gap: 12px; margin-top: 16px; flex-wrap: wrap; }
  .sp-state { display: inline-flex; align-items: center; gap: 9px; font-size: 13px; color: var(--ink-2); }
  .sp-state .bar { width: 110px; }
  .sp-state :global(svg) { width: 14px; height: 14px; color: var(--ok); }
  .eps { border-top: 1px solid var(--rule); }
  .ep-row { display: grid; grid-template-columns: 44px minmax(0, 1.1fr) minmax(0, 1fr) minmax(0, 1.2fr) 120px; gap: 12px; align-items: center; padding: 9px 6px; border-bottom: 1px solid var(--rule); font-size: 13px; color: var(--ink-2); }
  .ep-row.head { font: 600 12px/1 var(--font-ui); color: var(--ink-3); padding: 10px 6px; }
  button.ep-row { all: unset; box-sizing: border-box; display: grid; grid-template-columns: 44px minmax(0, 1.1fr) minmax(0, 1fr) minmax(0, 1.2fr) 120px; gap: 12px; align-items: center; padding: 9px 6px; border-bottom: 1px solid var(--rule); font-size: 13px; color: var(--ink-2); width: 100%; cursor: pointer; }
  button.ep-row:hover { background: var(--surface); }
  button.ep-row.next { background: var(--surface); }
  button.ep-row:focus-visible { outline: 2px solid var(--ink); outline-offset: -2px; }
  .ep-n { font: 500 13px/1 var(--font-mono); color: var(--ink); font-variant-numeric: tabular-nums; }
  .cell { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .cell span:last-child { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .cell :global(svg) { width: 13px; height: 13px; flex: none; color: var(--ok); }
  .cell .spin { width: 9px; height: 9px; border-width: 1.5px; }
  .tone-ok { color: var(--ink); }
  .tone-warn span:last-child { color: var(--warn); }
  .tone-bad span:last-child { color: var(--bad); }
  .tone-wait { color: var(--ink-3); }
  .muted { color: var(--ink-3); font-size: 12.5px; }
  .ep-state { display: inline-flex; align-items: center; gap: 7px; justify-self: end; font-weight: 600; font-size: 12.5px; color: var(--ink-3); }
  .ep-state i { width: 7px; height: 7px; border-radius: 50%; background: var(--rule-2); }
  .ep-state i.part { background: var(--warn-dot); }
  .ep-state.ok { color: var(--ink); }
  .ep-state.ok i { background: var(--ok-dot); }
  .more { display: grid; place-items: center; width: 24px; height: 24px; border-radius: 4px; color: var(--ink-3); }
  .more:hover { background: var(--sunk); color: var(--ink); }
  .more :global(svg) { width: 15px; height: 15px; }
  .right { justify-self: end; }
  .menu { position: fixed; z-index: 80; min-width: 210px; display: grid; padding: 5px; background: var(--surface); border: 1px solid var(--rule-2); border-radius: 8px; box-shadow: var(--shadow-lg); }
  .menu button { all: unset; padding: 8px 10px; border-radius: 5px; font-size: 13.5px; cursor: pointer; }
  .menu button:hover { background: var(--sunk); }
  .searchrow { display: flex; gap: 8px; }
  .results { display: grid; gap: 4px; max-height: 460px; overflow: auto; }
  .result { all: unset; display: grid; grid-template-columns: 46px minmax(0, 1fr); gap: 12px; align-items: center; padding: 6px; border-radius: 6px; cursor: pointer; }
  .result:hover { background: var(--sunk); }
  .result img, .result .noimg { width: 46px; aspect-ratio: 135 / 190; object-fit: cover; border-radius: 2px; background: var(--sunk); display: block; }
  .r-body { display: grid; gap: 2px; min-width: 0; }
  .r-jp { font: 800 15px/1.25 var(--font-jp-title); }
  .r-en { font-size: 13px; }
  .r-meta { font-size: 12px; color: var(--ink-3); }
</style>
