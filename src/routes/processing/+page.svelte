<script lang="ts">
  import { goto } from "$app/navigation";
  import { onMount } from "svelte";
  import AddSheet from "$lib/AddSheet.svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, callKept, errorText, kept, setWindowTitle, type DownloadsView, type Queue, type Queued, type Torrent } from "$lib/api";
  import { bytes } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  /** How many finished downloads are listed. */
  const FINISHED_SHOWN = 8;

  let downloads = $state.raw<DownloadsView | null>(kept<DownloadsView>("downloads"));
  let queue = $state.raw<Queue | null>(null);
  /** A folder of downloads being added to the library. */
  let adding = $state<string | null>(null);

  const downloading = $derived(downloads?.torrents.filter((t) => t.state !== "done") ?? []);
  const finished = $derived(downloads?.torrents.filter((t) => t.state === "done").slice(0, FINISHED_SHOWN) ?? []);

  onMount(() => {
    app.dark = false;
    setWindowTitle("Processing — Telly");
    let stopped = false;
    let downloadsTimer: ReturnType<typeof setTimeout>;
    // What Telly is doing changes by the second; downloads are asked for less often once they're done.
    const work = setInterval(() => call<Queue>("queue").then((q) => (queue = q)).catch(() => {}), 1500);
    call<Queue>("queue").then((q) => (queue = q)).catch(() => {});
    const loop = async () => {
      downloads = await callKept<DownloadsView>("downloads").catch(() => downloads);
      if (stopped) return;
      downloadsTimer = setTimeout(loop, downloading.length ? 2500 : 8000);
    };
    loop();
    return () => {
      stopped = true;
      clearInterval(work);
      clearTimeout(downloadsTimer);
    };
  });

  /** "2 hours left", "14 minutes left". */
  function left(seconds: number): string {
    if (seconds < 90) return "a minute left";
    if (seconds < 90 * 60) return `${Math.round(seconds / 60)} minutes left`;
    return `${Math.round(seconds / 3600)} hours left`;
  }

  /** How a download is going, in a few words. */
  function progressText(t: Torrent): string {
    if (t.state === "done") return t.show ? "In the library" : "Not in the library yet";
    if (t.state === "finding") return "Asking the people sharing it for its list of files";
    if (t.state === "error") return t.error ? `Stopped: ${t.error}` : "Stopped by a problem";
    const pct = `${Math.floor(t.progress * 100)}% of ${bytes(t.size)}`;
    const files = t.files > 1 ? ` · ${t.filesDone} of ${t.files} files done` : "";
    switch (t.state) {
      case "checking":
        return `Checking what’s downloaded${files}`;
      case "paused":
        return `${pct} · Paused${files}`;
      case "waiting":
        return `${pct} · Waiting for someone sharing it${files}`;
      default:
        return [pct, t.speed > 0 ? `${bytes(t.speed)}/s` : "", t.eta !== null ? left(t.eta) : ""].filter(Boolean).join(" · ") + files;
    }
  }

  const troubled = (t: Torrent) => t.state === "waiting" || t.state === "error";

  /** Pauses a download or carries on with it, or takes it off the list; Processing shows the answer. */
  async function act(t: Torrent, what: "pause" | "resume" | "remove") {
    if (t.id === null) return;
    if (what === "remove" && !confirm(`Take ${t.name} off the list? ${t.state === "done" ? "Telly stops sharing it." : "Telly stops downloading it."} What it downloaded stays where it is.`)) return;
    try {
      downloads = what === "remove" ? await call<DownloadsView>("removeDownload", { id: t.id }) : await call<DownloadsView>("pauseDownload", { id: t.id, paused: what === "pause" });
    } catch (e) {
      toast({ key: "download", state: "failed", title: "That didn’t work", text: errorText(e) });
    }
  }

  /** "Fairy Tail · Episode 12"; a film by its title. */
  const named = (q: Queued) => (q.episodes === 1 ? q.title : `${q.title} · Episode ${q.number}`);
</script>

<div class="proc scroll">
  <div class="pr-top">
    <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Library</span></button>
    <span class="grow"></span>
    <a class="btn quiet" href="/downloads"><Icon name="download" /><span>Download…</span></a>
  </div>
  <div class="pr-head">
    <h2>Processing</h2>
    <span>What’s downloading, and what Telly is getting ready</span>
  </div>

  <section aria-labelledby="pr-dl">
    <h3 id="pr-dl">Downloading</h3>
    {#if downloads?.error}
      <p class="note"><span>{downloads.error}</span></p>
    {/if}
    {#if downloads && downloads.offers.length}
      <ul class="list" role="list" aria-label="Ready to add">
        {#each downloads.offers as o (o.folder)}
          <li class="row">
            <div class="row-body">
              <span class="row-title">{o.name}</span>
              <span class="row-meta">{o.downloading ? "Some of it is downloaded. Add it now, and each episode joins as it finishes." : "Downloaded, and ready to add to the library."}</span>
              <span class="row-meta mono" title={o.folder}>{o.folder}</span>
            </div>
            <button type="button" class="btn small" onclick={() => (adding = o.folder)}>Add to library…</button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if downloading.length}
      <ul class="list" role="list">
        {#each downloading as t (t.hash)}
          <li class="row">
            <div class="row-body">
              <span class="row-title" title={t.name}>{t.name}</span>
              <span class="bar" role="progressbar" aria-label="Downloaded" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.floor(t.progress * 100)}><i style:width="{t.progress * 100}%"></i></span>
              <span class="row-meta" class:warn={troubled(t)}>{progressText(t)}</span>
            </div>
            <span class="row-acts">
              <!-- Stopped on an error (the disk full, say): Carry on starts it again from what it has. -->
              {#if t.id !== null && (t.state === "paused" || t.state === "error")}
                <button type="button" class="btn small" onclick={() => act(t, "resume")}>Carry on</button>
              {:else if t.id !== null}
                <button type="button" class="btn small quiet" onclick={() => act(t, "pause")}>Pause</button>
              {/if}
              {#if t.id !== null}<button type="button" class="btn small quiet" onclick={() => act(t, "remove")} title="Stops the download; what it got stays in its folder">Remove</button>{/if}
              {#if t.show}<button type="button" class="btn small quiet" onclick={() => goto(`/show?id=${t.show}`)}>Open show</button>{/if}
            </span>
          </li>
        {/each}
      </ul>
    {:else if downloads && !downloads.offers.length}
      <p class="hint">Nothing is downloading. <a href="/downloads" class="link-btn">Download from nyaa.si…</a></p>
    {/if}
    {#if finished.length}
      <h4>Finished</h4>
      <ul class="list" role="list" aria-label="Finished">
        {#each finished as t (t.hash)}
          <li class="row">
            <div class="row-body">
              <span class="row-title" title={t.name}>{t.name}</span>
              <span class="row-meta">{bytes(t.size)} · {progressText(t)} · Shared with others while Telly is open</span>
            </div>
            <span class="row-acts">
              <button type="button" class="btn small quiet" onclick={() => act(t, "remove")} title="Stops sharing it; the files stay where they are">Remove</button>
              {#if t.show}<button type="button" class="btn small quiet" onclick={() => goto(`/show?id=${t.show}`)}>Open show</button>{/if}
            </span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section aria-labelledby="pr-work">
    <h3 id="pr-work">Getting ready</h3>
    {#if queue}
      {#if queue.now.length}
        <ul class="list" role="list" aria-label="Working on">
          {#each queue.now as t, i (i)}
            <li class="row">
              <div class="row-body">
                <span class="row-title">{t.label}</span>
                {#if t.progress > 0}
                  <span class="bar" role="progressbar" aria-label={t.label} aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(t.progress * 100)}><i style:width="{t.progress * 100}%"></i></span>
                  <span class="row-meta">{Math.round(t.progress * 100)}%</span>
                {/if}
              </div>
              {#if t.showId}<button type="button" class="btn small quiet" onclick={() => goto(`/show?id=${t.showId}`)}>Open show</button>{/if}
            </li>
          {/each}
        </ul>
      {:else}
        <p class="hint">{queue.waiting.length ? "Nothing at the moment." : "Nothing at the moment: every episode is ready."}</p>
      {/if}
      {#each queue.waiting as w (w.kind)}
        <h4>{w.what} <span class="count">{w.count} waiting</span></h4>
        <ul class="list" role="list" aria-label="{w.what}: {w.count} waiting">
          {#each w.first as q (q.episodeId)}
            <li class="row">
              <div class="row-body">
                <span class="row-title">{named(q)}</span>
                {#if q.step}<span class="row-meta">{q.step}</span>{/if}
              </div>
              <button type="button" class="btn small quiet" onclick={() => goto(`/show?id=${q.showId}`)}>Open show</button>
            </li>
          {/each}
        </ul>
        {#if w.count > w.first.length}<p class="hint">and {w.count - w.first.length} more</p>{/if}
      {/each}
    {:else}
      <p class="hint"><span class="spin"></span> Asking…</p>
    {/if}
  </section>
</div>

{#if adding}
  <AddSheet folder={adding} onclose={() => (adding = null)} onadded={(id) => { adding = null; goto(`/show?id=${id}`); }} />
{/if}

<style>
  .proc { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 40px; }
  .pr-top { display: flex; align-items: center; gap: 12px; height: 38px; margin-bottom: 24px; }
  .pr-top a.btn { text-decoration: none; }
  .pr-head { display: flex; align-items: baseline; gap: 10px; margin: 0 0 6px; }
  .pr-head h2 { font: 600 17px/1.2 var(--font-ui); }
  .pr-head span { font-size: 13px; color: var(--ink-2); }
  section { max-width: 900px; display: grid; gap: 12px; padding: 18px 0; border-bottom: 1px solid var(--rule); }
  section:last-child { border-bottom: 0; }
  h3 { font: 600 14px/1.2 var(--font-ui); }
  h4 { font: 600 12.5px/1.2 var(--font-ui); color: var(--ink-2); margin-top: 6px; }
  h4 .count { font-weight: 400; color: var(--ink-3); margin-left: 6px; }
  .note { display: flex; flex-wrap: wrap; gap: 6px 12px; align-items: baseline; padding: 10px 14px; border: 1px solid var(--rule-2); border-radius: 8px; background: var(--surface); font-size: 13.5px; color: var(--ink-2); }
  .hint { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--ink-3); }
  .list { display: grid; margin: 0; padding: 0; border-top: 1px solid var(--rule); }
  .row { list-style: none; display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 16px; align-items: center; padding: 10px 6px; border-bottom: 1px solid var(--rule); }
  .row-body { display: grid; gap: 5px; min-width: 0; }
  .row-title { font: 600 13.5px/1.35 var(--font-ui); overflow-wrap: anywhere; }
  .row-meta { font-size: 12.5px; color: var(--ink-2); overflow-wrap: anywhere; }
  .row .bar { max-width: 420px; }
  .warn { color: var(--warn); }
  .row-acts { display: flex; gap: 8px; flex-wrap: wrap; justify-content: flex-end; }
</style>
