<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import DownloadSheet from "$lib/DownloadSheet.svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, callKept, errorText, inTauri, kept, setWindowTitle, type DownloadPlan, type DownloadsView, type NyaaFound, type Release, type ShowView } from "$lib/api";
  import { plural } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  /** /downloads?show=<id> downloads into that show's folder; ?anilist=<id> downloads as
   *  that AniList entry, into its place in the library; ?q=<title> searches for it at
   *  once, and ?from=recommended goes back to Recommended. */
  const showId = Number(page.url.searchParams.get("show")) || null;
  const anilistId = Number(page.url.searchParams.get("anilist")) || null;
  const asked = page.url.searchParams.get("q") ?? "";
  const fromRecommended = page.url.searchParams.get("from") === "recommended";
  let show = $state<{ id: number; title: string; folder: string } | null>(null);
  /** Where the AniList entry's download goes. */
  let into = $state("");
  /** The release whose show is being asked. */
  let asking = $state.raw<Release | null>(null);

  let q = $state("");
  let category = $state("all");
  let trusted = $state(false);
  let sort = $state("seeders");
  let releases = $state.raw<Release[]>([]);
  let more = $state(false);
  let pageNo = $state(1);
  let searching = $state(false);
  /** The search the results are for. */
  let searched = $state("");
  /** The AniList entry the releases listed are: the one asked for, until something else is searched for. */
  const entry = $derived(anilistId && searched === asked.trim() ? anilistId : null);
  let searchError = $state("");
  /** Releases started downloading from this page. */
  let sent = $state<Record<number, "sending" | "sent">>({});
  /** How much is downloading, and where new shows go. */
  let view = $state.raw<DownloadsView | null>(kept<DownloadsView>("downloads"));
  const date = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });

  async function loadDownloads() {
    try {
      view = await callKept<DownloadsView>("downloads");
    } catch {
      // The last answer stays.
    }
  }

  onMount(() => {
    app.dark = false;
    setWindowTitle("Download — Telly");
    let timer: ReturnType<typeof setTimeout>;
    let stopped = false;
    const loop = async () => {
      await loadDownloads();
      if (stopped) return;
      const busy = view?.torrents.some((t) => t.progress < 1) ?? false;
      timer = setTimeout(loop, busy ? 2500 : 8000);
    };
    loop();
    if (showId) {
      callKept<ShowView>("show", { id: showId })
        .then((v) => {
          show = { id: v.card.id, title: v.card.title, folder: v.card.folder };
          q = v.card.titleRomaji || v.card.title;
          search();
        })
        .catch((e) => (searchError = errorText(e)));
    } else if (asked) {
      q = asked;
      search();
    }
    if (anilistId) {
      call<DownloadPlan>("placeDownload", { title: asked, anilistId })
        .then((plan) => (into = plan.to.folder))
        .catch(() => {});
    }
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  });

  async function search(next = false) {
    const query = q.trim();
    if (!query) return;
    searching = true;
    searchError = "";
    const at = next ? pageNo + 1 : 1;
    try {
      const found = await call<NyaaFound>("searchNyaa", { q: query, category, trusted, sort, page: at });
      releases = next ? [...releases, ...found.releases] : found.releases;
      more = found.more;
      pageNo = at;
      searched = query;
    } catch (e) {
      searchError = errorText(e);
    } finally {
      searching = false;
    }
  }

  /** A release is on its way into `folder`. */
  function started(r: Release, folder: string) {
    sent[r.id] = "sent";
    toast({ key: `download-${r.id}`, state: "done", title: "Downloading", text: `Into ${folder}. The title bar shows how it’s going.` });
    loadDownloads();
  }

  async function download(r: Release) {
    // A release for no show yet: its sheet asks which it is.
    if (!show && !entry) {
      asking = r;
      return;
    }
    sent[r.id] = "sending";
    try {
      const { folder } = await call<{ folder: string }>("download", { magnet: r.magnet, torrent: r.torrent, title: r.title, show: show?.id ?? null, anilistId: entry });
      started(r, folder);
    } catch (e) {
      delete sent[r.id];
      toast({ key: "download", state: "failed", title: "Couldn’t start the download", text: errorText(e) });
    }
  }

  async function openPage(r: Release) {
    try {
      if (inTauri) {
        const { openUrl } = await import("@tauri-apps/plugin-opener");
        await openUrl(r.page);
      } else window.open(r.page);
    } catch (e) {
      toast({ key: "link", state: "failed", title: "Couldn’t open the browser", text: errorText(e) });
    }
  }
</script>

<div class="dls scroll">
  <div class="dl-top">
    <button type="button" class="back" onclick={() => goto(show ? `/show?id=${show.id}` : fromRecommended ? "/recommended" : "/")}><Icon name="back" /><span>{show ? show.title : fromRecommended ? "Recommended" : "Library"}</span></button>
  </div>
  <div class="dl-head">
    <h2>Download</h2>
    <span>{show ? `Into ${show.title}’s folder` : entry && into ? `Into ${into}` : "Telly asks which show a release is, and puts it in its place"}</span>
  </div>

  {#if view?.error}
    <p class="note">
      <span>{view.error}</span>
      <a href="/settings#downloads" class="link-btn">Settings → Downloads</a>
    </p>
  {/if}

  <section aria-label="Find on nyaa.si">
    <form class="find" onsubmit={(e) => { e.preventDefault(); search(); }}>
      <input class="input grow" bind:value={q} placeholder="Sousou no Frieren 1080p" aria-label="Search nyaa.si" />
      <button type="submit" class="btn" disabled={searching || !q.trim()}>{searching && pageNo === 1 ? "Searching…" : "Search"}</button>
    </form>
    <div class="filters">
      <select class="input" bind:value={category} onchange={() => search()} aria-label="Category">
        <option value="all">All anime</option>
        <option value="translated">English-translated</option>
        <option value="raw">Raw</option>
      </select>
      <select class="input" bind:value={sort} onchange={() => search()} aria-label="Order">
        <option value="seeders">Most shared first</option>
        <option value="newest">Newest first</option>
      </select>
      <label class="check"><input type="checkbox" bind:checked={trusted} onchange={() => search()} /><span>Trusted uploaders only</span></label>
    </div>
    <p class="hint">The more people share a release, the faster and steadier it downloads. Telly keeps going through dropouts, and picks up where it was when it opens again.</p>

    {#if searchError}<p class="err">{searchError}</p>{/if}
    {#if searched && !releases.length && !searching && !searchError}
      <p class="hint">nyaa has nothing for “{searched}”.</p>
    {/if}
    {#if releases.length}
      <ul class="list" role="list">
        {#each releases as r (r.id)}
          <li class="row">
            <div class="row-body">
              <button type="button" class="row-title link" onclick={() => openPage(r)} title="Open its page on nyaa.si">{r.title}</button>
              <span class="row-meta">
                {r.size} ·
                <span class:warn={r.seeders === 0}>{r.seeders === 0 ? "No one sharing it" : `${plural(r.seeders, "person", "people")} sharing it`}</span>
                · {date.format(r.date * 1000)}
                {#if r.trusted}· <b class="ok">Trusted</b>{/if}
                {#if r.remake}· Re-upload{/if}
              </span>
            </div>
            <button type="button" class="btn small" disabled={!!sent[r.id]} onclick={() => download(r)} title={show ? `Telly downloads it into ${show.folder}` : entry ? "Telly downloads it into its place in your library" : "Telly asks which show it is, then downloads it"}>
              {#if sent[r.id] === "sent"}<Icon name="check" /><span>Downloading</span>{:else if sent[r.id]}<span>Sending…</span>{:else}<Icon name="download" /><span>{show || entry ? "Download" : "Download…"}</span>{/if}
            </button>
          </li>
        {/each}
      </ul>
      {#if more}
        <button type="button" class="btn quiet more" disabled={searching} onclick={() => search(true)}>{searching ? "Searching…" : "Show more"}</button>
      {/if}
    {/if}
  </section>
</div>

{#if asking}
  {@const release = asking}
  <DownloadSheet {release} onclose={() => (asking = null)} onstarted={(folder) => { asking = null; started(release, folder); }} />
{/if}

<style>
  .dls { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 40px; }
  .dl-top { display: flex; align-items: center; gap: 12px; height: 38px; margin-bottom: 24px; }
  .dl-head { display: flex; align-items: baseline; gap: 10px; margin: 0 0 6px; }
  .dl-head h2 { font: 600 17px/1.2 var(--font-ui); }
  .dl-head span { font-size: 13px; color: var(--ink-2); }
  section { max-width: 900px; display: grid; gap: 12px; padding: 18px 0; border-bottom: 1px solid var(--rule); }
  section:last-child { border-bottom: 0; }
  .note { display: flex; flex-wrap: wrap; gap: 6px 12px; align-items: baseline; max-width: 900px; padding: 10px 14px; margin: 12px 0 0; border: 1px solid var(--rule-2); border-radius: 8px; background: var(--surface); font-size: 13.5px; color: var(--ink-2); }
  .find { display: flex; gap: 8px; }
  .filters { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 14px; }
  .filters .check { align-items: center; font-size: 13.5px; }
  .filters .check input { margin: 0; }
  .hint { font-size: 13px; color: var(--ink-3); max-width: 70ch; }
  .list { display: grid; margin: 0; padding: 0; border-top: 1px solid var(--rule); }
  .row { list-style: none; display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 16px; align-items: center; padding: 10px 6px; border-bottom: 1px solid var(--rule); }
  .row-body { display: grid; gap: 5px; min-width: 0; }
  .row-title { font: 600 13.5px/1.35 var(--font-ui); overflow-wrap: anywhere; }
  .row-title.link { all: unset; font: 600 13.5px/1.35 var(--font-ui); overflow-wrap: anywhere; cursor: pointer; }
  .row-title.link:hover { text-decoration: underline; text-underline-offset: 3px; }
  .row-title.link:focus-visible { outline: 2px solid var(--ink); outline-offset: 2px; }
  .row-meta { font-size: 12.5px; color: var(--ink-2); overflow-wrap: anywhere; }
  .warn { color: var(--warn); }
  .ok { color: var(--ok); font-weight: 600; }
  .more { justify-self: start; }
</style>
