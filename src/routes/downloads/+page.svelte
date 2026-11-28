<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount, untrack } from "svelte";
  import DownloadSheet from "$lib/DownloadSheet.svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, callKept, errorText, inTauri, kept, setWindowTitle, type DownloadPlan, type DownloadsView, type EpisodeSet, type NyaaFound, type Release, type Settings, type ShowView, downloadsOn } from "$lib/api";
  import { bytes, plural } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  /** /downloads?show=<id> downloads into that show's folder; ?anilist=<id> downloads as
   *  that AniList entry, into its place in the library; ?q=<title> searches for it at
   *  once, and ?from=recommended goes back to Recommended. */
  const showId = $derived(Number(page.url.searchParams.get("show")) || null);
  const anilistId = $derived(Number(page.url.searchParams.get("anilist")) || null);
  const asked = $derived(page.url.searchParams.get("q") ?? "");
  const fromRecommended = $derived(page.url.searchParams.get("from") === "recommended");
  let show = $state<{ id: number; title: string; folder: string } | null>(null);
  /** Where the AniList entry's download goes, and the libraries it could go in. */
  let plan = $state.raw<DownloadPlan | null>(null);
  /** A download pressed before Telly knows where the entry goes: started once it does. */
  let choosing = $state.raw<{ what: string; go: (library: number | null) => void } | null>(null);
  /** Telly couldn't work out where the AniList entry's download goes. */
  let placeFailed = $state(false);
  /** The release whose show is being asked. */
  let asking = $state.raw<Release | null>(null);

  /** Your settings, for whether downloading is on; null until Telly has said. */
  let settings = $state.raw<Settings | null>(kept<Settings>("settings"));
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
  /** Which search is the latest: an older one's answer, coming back after it, is let go. */
  let searchRun = 0;
  /** The AniList entry the releases listed are: the one asked for, until something else is searched for. */
  const entry = $derived(anilistId && searched === asked.trim() ? anilistId : null);
  let searchError = $state("");
  /** A release for each episode of the AniList entry, from one group, for when no one release has them all. */
  let set = $state.raw<EpisodeSet | null>(null);
  /** The search site's pages are being read for a release of each episode. */
  let lookingForSet = $state(false);
  /** Which look for a release of each episode is the latest: Show more, a search's next page, doesn't start one. */
  let setRun = 0;
  /** How many of the set's releases Download all has started, while it's starting them. */
  let sending = $state<{ done: number; of: number } | null>(null);
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
    callKept<Settings>("settings").then((s) => (settings = s)).catch(() => {});
    let timer: ReturnType<typeof setTimeout>;
    let stopped = false;
    const loop = async () => {
      await loadDownloads();
      if (stopped) return;
      const busy = view?.torrents.some((t) => t.progress < 1) ?? false;
      timer = setTimeout(loop, busy ? 2500 : 8000);
    };
    loop();
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  });

  // Another anime searched for from the title bar keeps this page: what was
  // found for the last one goes, and the new one is searched for.
  $effect.pre(() => {
    const [forShow, forEntry, words] = [showId, anilistId, asked];
    untrack(() => {
      show = null;
      plan = null;
      choosing = null;
      asking = null;
      releases = [];
      more = false;
      set = null;
      searched = "";
      searchError = "";
      if (forShow) {
        callKept<ShowView>("show", { id: forShow })
          .then((v) => {
            if (showId !== forShow) return;
            show = { id: v.card.id, title: v.card.title, folder: v.card.folder };
            q = v.card.titleRomaji || v.card.title;
            search();
          })
          .catch((e) => (searchError = errorText(e)));
      } else if (words) {
        q = words;
        search();
      }
      if (forEntry) placeEntry();
    });
  });

  /** Asks where the AniList entry's download goes: in its usual library, or `library`. */
  function placeEntry(library?: number) {
    const [title, forEntry] = [asked, anilistId];
    placeFailed = false;
    call<DownloadPlan>("placeDownload", { title, anilistId: forEntry, library: library ?? null })
      .then((p) => {
        if (anilistId === forEntry) plan = p;
      })
      .catch(() => {
        if (anilistId === forEntry) placeFailed = true;
      });
  }

  async function search(next = false) {
    const query = q.trim();
    if (!query) return;
    const run = ++searchRun;
    searching = true;
    searchError = "";
    const at = next ? pageNo + 1 : 1;
    if (!next) findSet(query);
    try {
      const found = await call<NyaaFound>("searchNyaa", { q: query, category, trusted, sort, page: at });
      if (run !== searchRun) return;
      releases = next ? [...releases, ...found.releases] : found.releases;
      more = found.more;
      pageNo = at;
      searched = query;
    } catch (e) {
      if (run === searchRun) searchError = errorText(e);
    } finally {
      if (run === searchRun) searching = false;
    }
  }

  /** Looks for a release of each episode of the AniList entry, for when the search site has no one release of them all. */
  async function findSet(query: string) {
    const run = ++setRun;
    set = null;
    const forEntry = anilistId && query === asked.trim() ? anilistId : null;
    lookingForSet = !!forEntry;
    if (!forEntry) return;
    try {
      const found = await call<EpisodeSet | null>("everyEpisode", { q: query, category, trusted, anilistId: forEntry });
      if (run === setRun) set = found;
    } catch {
      // The releases listed are still there to pick from.
    } finally {
      if (run === setRun) lookingForSet = false;
    }
  }

  /** Whether the AniList entry's downloads can go in a library you choose: it's a new show, and there's a choice. */
  const choosesLibrary = $derived(!show && !!entry && !!plan && plan.to.show === null && plan.libraries.length > 1);

  /** Starts `go` into the library chosen above the results. Pressed before Telly knows where the entry
   *  goes, it waits for that. */
  function intoLibrary(what: string, go: (library: number | null) => void) {
    if (entry && !show && !plan && !placeFailed) choosing = { what, go };
    else go(entry ? (plan?.to.library ?? null) : null);
  }

  // Once Telly knows where the entry goes, a download that waited on it starts.
  $effect(() => {
    if (!choosing || (!plan && !placeFailed)) return;
    const go = choosing.go;
    choosing = null;
    go(plan?.to.library ?? null);
  });

  /** Downloads each of the set's releases, one after another, into the entry's place in `library`. */
  async function downloadAll(s: EpisodeSet, library: number | null) {
    const forEntry = entry;
    const todo = s.releases.filter((r) => !sent[r.id]);
    let [started, failed, folder] = [0, "", ""];
    sending = { done: 0, of: todo.length };
    for (const r of todo) {
      sent[r.id] = "sending";
      try {
        ({ folder } = await call<{ folder: string }>("download", { magnet: r.magnet, torrent: r.torrent, title: r.title, show: null, anilistId: forEntry, library }));
        sent[r.id] = "sent";
        started++;
      } catch (e) {
        delete sent[r.id];
        failed ||= errorText(e);
      }
      sending = { done: todo.indexOf(r) + 1, of: todo.length };
    }
    sending = null;
    if (started) toast({ key: "download-all", state: "done", title: `Downloading ${plural(started, "episode")}`, text: `Into ${folder}. The title bar shows how it’s going.` });
    if (failed) toast({ key: "download", state: "failed", title: `${plural(todo.length - started, "episode")} couldn’t start downloading`, text: failed });
    loadDownloads();
  }

  /** "Episode 7", "Episodes 2, 5 and 9", "Episodes 1–60, 69 and 77". */
  function episodesText(numbers: number[]): string {
    const runs: string[] = [];
    for (let i = 0; i < numbers.length; ) {
      let j = i;
      while (j + 1 < numbers.length && numbers[j + 1] === numbers[j] + 1) j++;
      runs.push(j - i >= 2 ? `${numbers[i]}–${numbers[j]}` : numbers.slice(i, j + 1).join(", "));
      i = j + 1;
    }
    const list = runs.join(", ");
    const at = list.lastIndexOf(", ");
    if (numbers.length === 1) return `Episode ${list}`;
    return `Episodes ${at < 0 ? list : `${list.slice(0, at)} and ${list.slice(at + 2)}`}`;
  }

  /** A release is on its way into `folder`. */
  function started(r: Release, folder: string) {
    sent[r.id] = "sent";
    toast({ key: `download-${r.id}`, state: "done", title: "Downloading", text: `Into ${folder}. The title bar shows how it’s going.` });
    loadDownloads();
  }

  function download(r: Release) {
    // A release for no show yet: its sheet asks which it is, and where it goes.
    if (!show && !entry) {
      asking = r;
      return;
    }
    intoLibrary(r.title, (library) => start(r, library));
  }

  async function start(r: Release, library: number | null) {
    sent[r.id] = "sending";
    try {
      const { folder } = await call<{ folder: string }>("download", { magnet: r.magnet, torrent: r.torrent, title: r.title, show: show?.id ?? null, anilistId: entry, library });
      started(r, folder);
    } catch (e) {
      delete sent[r.id];
      toast({ key: "download", state: "failed", title: "Couldn’t start the download", text: errorText(e) });
    }
  }

  /** Back where Download was opened from: Recommended as you left it, the show's page, or the Library. */
  function back() {
    if (fromRecommended && !show && history.length > 1) history.back();
    else goto(show ? `/show?id=${show.id}` : fromRecommended ? "/recommended" : "/");
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
    <button type="button" class="back" onclick={back}><Icon name="back" /><span>{show ? show.title : fromRecommended ? "Recommended" : "Library"}</span></button>
  </div>
  <div class="dl-head">
    <h2>Download</h2>
    <span>{show ? `Into ${show.title}’s folder` : entry && plan && !choosesLibrary ? `Into ${plan.to.folder}` : entry && plan ? "" : "Telly asks which show a release is, and puts it in its place"}</span>
  </div>
  {#if choosesLibrary && plan}
    {@const p = plan}
    <p class="into">
      <label for="library">Into</label>
      <select id="library" class="input" value={p.to.library ?? 0} onchange={(e) => placeEntry(Number(e.currentTarget.value))} title="The library what you download from here goes in">
        {#each p.libraries as l, i (i)}{@const kind = l.kind === "movies" ? "Movies" : "Series"}<option value={i}>{l.name === kind ? l.name : `${l.name} (${kind})`}</option>{/each}
      </select>
      <span class="mono">{p.to.folder}</span>
    </p>
  {/if}

  {#if view?.error}
    <p class="note">
      <span>{view.error}</span>
      <a href="/settings#downloads" class="link-btn">Settings → Library and downloads</a>
    </p>
  {/if}

  {#if settings && !downloadsOn(settings)}
    <p class="note">
      <span>Downloading is off. Telly has no site of its own to find releases on: add one you have the right to download from, and Download… appears on shows and in Recommended.</span>
      <a href="/settings#downloads" class="link-btn">Settings → Library and downloads</a>
    </p>
  {:else}
  <section aria-label="Find a release">
    <form class="find" onsubmit={(e) => { e.preventDefault(); search(); }}>
      <input class="input grow" bind:value={q} placeholder="Sousou no Frieren 1080p" aria-label="Search for a release" />
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
        <option value="size">Biggest first</option>
      </select>
      <label class="check"><input type="checkbox" bind:checked={trusted} onchange={() => search()} /><span>Trusted uploaders only</span></label>
    </div>
    <p class="hint">The more people share a release, the faster and steadier it downloads. Telly keeps going through dropouts, and picks up where it was when it opens again.</p>

    {#if searchError}<p class="err">{searchError}</p>{/if}
    {#if searched && !releases.length && !searching && !searchError}
      <p class="hint">The search site has nothing for “{searched}”.</p>
    {/if}
    {#if set && entry}
      {@const s = set}
      {@const left = s.releases.filter((r) => sent[r.id] !== "sent").length}
      <div class="set" class:first={!s.batch}>
        <div class="row-body">
          <span class="row-title">{s.missing.length ? plural(s.episodes, "episode") : `All ${plural(s.episodes, "episode")}`}{s.releases.length === s.episodes ? ", one torrent each" : ` in ${plural(s.releases.length, "torrent")}`}</span>
          <span class="row-meta">
            {s.batch ? "" : "No one release on the search site has every episode, so Telly found one for each. "}Named like {s.releases[0].title} ·
            {bytes(s.bytes)} ·
            <span class:warn={s.seeders === 0}>{s.seeders === 0 ? "One of them no one is sharing" : `At least ${plural(s.seeders, "person", "people")} sharing each`}</span>
          </span>
          {#if s.missing.length}<span class="row-meta warn">Telly found no release of {episodesText(s.missing).toLowerCase()} from them on the search site.</span>{/if}
        </div>
        <button type="button" class="btn small" disabled={sending !== null || left === 0} onclick={() => intoLibrary(`All ${plural(s.episodes, "episode")}`, (library) => downloadAll(s, library))} title="Telly downloads each episode’s torrent into its place in your library">
          {#if sending}<span>Starting {Math.min(sending.done + 1, sending.of)} of {sending.of}…</span>{:else if left === 0}<Icon name="check" /><span>Downloading</span>{:else}<Icon name="download" /><span>Download {left === s.releases.length ? "all" : "the other"} {left}</span>{/if}
        </button>
      </div>
    {:else if lookingForSet && entry}
      <div class="set looking" role="status">
        <div class="row-body">
          <span class="row-title"><span class="spin"></span> Looking for a release of each episode…</span>
          <span class="row-meta">Telly is reading the search site’s pages in case no one release has them all, which takes a little while. Wait for it before picking one below: if it finds them, <b>Download all</b> appears here.</span>
          <span class="bar busy" aria-hidden="true"><i></i></span>
        </div>
      </div>
    {/if}
    {#if releases.length}
      <ul class="list" class:waiting={lookingForSet && entry && !set} role="list">
        {#each releases as r (r.id)}
          <li class="row">
            <div class="row-body">
              <button type="button" class="row-title link" onclick={() => openPage(r)} title="Open its page on the search site">{r.title}</button>
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
  {/if}
</div>

{#if choosing && !plan}
  <div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && (choosing = null)} onkeydown={(e) => e.key === "Escape" && (choosing = null)}>
    <div class="sheet" role="dialog" aria-modal="true" aria-label="Finding where it goes">
      <p class="sheet-note"><span class="spin"></span> Finding where it goes in your library…</p>
      <div class="sheet-foot"><button type="button" class="btn quiet" onclick={() => (choosing = null)}>Cancel</button></div>
    </div>
  </div>
{/if}

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
  .into { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 10px; margin: 4px 0 0; font-size: 13px; color: var(--ink-2); }
  .into .mono { overflow-wrap: anywhere; }
  .set { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 16px; align-items: center; padding: 12px 14px; border: 1px solid var(--rule-2); border-radius: 8px; background: var(--surface); }
  .set.first { border-color: var(--ink-3); }
  .set.looking { grid-template-columns: minmax(0, 1fr); }
  .looking .row-title { display: flex; align-items: center; gap: 8px; }
  .bar.busy { margin-top: 4px; }
  .bar.busy i { width: 30%; animation: busy 1.4s ease-in-out infinite; }
  @keyframes busy { from { transform: translateX(-100%); } to { transform: translateX(340%); } }
  .list.waiting { opacity: .5; transition: opacity .2s; }
</style>
