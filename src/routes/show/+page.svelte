<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, callKept, errorText, kept, pickFile, pickFolder, setWindowTitle, type Anime, type EpisodeRow, type Library, type MissingRips, type Settings, type ShowMove, type ShowView, type TmdbTitle, downloadsOn } from "$lib/api";
  import { bytes, clock, mediaLabel, plural, statusOf, subsHelp, subsLabel } from "$lib/format";
  import { animeOf, partsText } from "$lib/series";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  const id = $derived(Number(page.url.searchParams.get("id")));
  let view = $state.raw<ShowView | null>(null);
  let error = $state("");
  let settings = $state.raw<Settings>(kept<Settings>("settings") ?? {});
  let menu = $state<{ ep: EpisodeRow; x: number; y: number } | null>(null);
  let matching = $state(false);
  let query = $state("");
  let found = $state<Anime[]>([]);
  /** What TMDB finds for Change match…: Japanese dramas and films AniList doesn't know. */
  let foundTmdb = $state<TmdbTitle[]>([]);
  let busy = $state(false);
  /** What Find its folder… found in the folder chosen, before anything changes. */
  let moving = $state<ShowMove | null>(null);
  /** Move to another anime… is open: the library's anime to choose from, and what's typed to find one. */
  let regrouping = $state.raw<Library | null>(null);
  let regroupFilter = $state("");
  /** The More menu beside Premine is open. */
  let more = $state(false);
  /** What AniList says the show is about ("" until it's known, or when it says nothing). */
  let about = $state("");
  let aboutOpen = $state(false);
  let aboutBox = $state<HTMLParagraphElement | undefined>();
  /** The description is longer than the lines it's cut to. */
  let aboutLong = $state(false);

  async function load() {
    const show = id;
    try {
      const v = await callKept<ShowView>("show", { id: show });
      // Another show's page has opened meanwhile.
      if (show !== id) return;
      view = v;
      error = "";
    } catch (e) {
      if (show === id) error = errorText(e);
    }
  }

  onMount(() => {
    app.dark = false;
    callKept<Settings>("settings").then((s) => (settings = s)).catch(() => {});
    const close = () => {
      menu = null;
      more = false;
    };
    const escape = (e: KeyboardEvent) => e.key === "Escape" && (menu || more) && close();
    window.addEventListener("click", close);
    window.addEventListener("keydown", escape);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", escape);
    };
  });

  // A long show opens as fast as a short one: its first screen of episodes is
  // drawn at once, and the rest once that's on screen.
  const FIRST_ROWS = 40;
  let allRows = $state(false);
  const episodes = $derived(allRows ? (view?.episodes ?? []) : (view?.episodes ?? []).slice(0, FIRST_ROWS));
  $effect(() => {
    if (!view || allRows) return;
    const frame = requestAnimationFrame(() => setTimeout(() => (allRows = true)));
    return () => cancelAnimationFrame(frame);
  });

  // Going from one show's page to another's (the title bar, or back) keeps this page:
  // the last show is dropped before it's drawn again, and the new one is asked for.
  $effect.pre(() => {
    const show = id;
    view = kept<ShowView>("show", { id: show });
    error = "";
    menu = null;
    matching = false;
    moving = null;
    regrouping = null;
    allRows = false;
    more = false;
    about = "";
    aboutOpen = false;
    call<{ text: string }>("showAbout", { id: show })
      .then((r) => show === id && (about = r.text))
      .catch(() => {});
    let timer: ReturnType<typeof setTimeout>;
    // Stopped when the page is left, even with an answer on its way.
    let stopped = false;
    const loop = async () => {
      await load();
      if (stopped) return;
      if (view) setWindowTitle(`${view.card.title} — Telly`);
      const busy = view?.episodes.some((e) => ["pending", "working"].includes(e.subsState) || e.mediaState === "working") ?? false;
      timer = setTimeout(loop, busy ? 1500 : 6000);
    };
    loop();
    // Each episode's share of known words is worked out again, from Anki as it is now.
    call("wordShares", { show }).catch(() => {});
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  });

  const card = $derived(view?.card ?? null);
  /** Follow new episodes is offered: a series that has episodes still to come. */
  const canFollow = $derived(!!card && !card.movie && (!card.anilistEpisodes || card.episodes < card.anilistEpisodes));

  $effect(() => {
    if (!aboutBox || aboutOpen || !about) return;
    const box = aboutBox;
    const measure = () => (aboutLong = box.scrollHeight > box.clientHeight + 1);
    measure();
    const watch = new ResizeObserver(measure);
    watch.observe(box);
    return () => watch.disconnect();
  });
  const st = $derived(card ? statusOf(card) : null);
  const next = $derived(card?.next ?? null);
  const completion = $derived(view?.completion);
  const date = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });

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
    // Started before the menu closes: its buttons read the episode from the menu.
    const doing = fn();
    menu = null;
    try {
      await doing;
    } catch (e) {
      toast({ key: "showerr", state: "failed", title: "That didn’t work", text: errorText(e) });
    }
    load();
  }

  async function chooseSubtitle(ep: EpisodeRow) {
    try {
      const path = await pickFile("Subtitles for episode " + ep.number, ["srt", "ass", "ssa", "vtt"], "Subtitles");
      if (!path) return;
      const name = path.split(/[\\/]/).pop() ?? "";
      toast({ key: "usesub", state: "working", title: "Syncing the file", text: name });
      await call("useSubtitle", { id: ep.id, path });
      toast({ key: "usesub", state: "done", title: "Subtitles synced", text: name });
    } catch (e) {
      toast({ key: "usesub", state: "failed", title: "Couldn’t use that file", text: errorText(e) });
    }
    load();
  }

  async function search() {
    busy = true;
    try {
      const count = card?.episodes ?? 0;
      const [anilist, tmdb] = await Promise.allSettled([call<Anime[]>("searchAnilist", { query, count }), call<TmdbTitle[]>("searchTmdb", { query, count })]);
      found = anilist.status === "fulfilled" ? anilist.value : [];
      foundTmdb = tmdb.status === "fulfilled" ? tmdb.value : [];
      if (anilist.status === "rejected" && tmdb.status === "rejected") error = errorText(anilist.reason);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  async function rematch(a: Anime) {
    if (!card) return;
    const show = card.id;
    matching = false;
    await act(() => call("rematch", { id: show, anilistId: a.id }));
  }

  async function rematchTmdb(t: TmdbTitle) {
    if (!card) return;
    const show = card.id;
    matching = false;
    await act(() => call("rematch", { id: show, tmdbId: t.id }));
  }

  /** The anime this show could be listed under: every other one in the library, those whose name has what's typed. */
  const otherAnime = $derived.by(() => {
    if (!regrouping || !card) return [];
    const typed = regroupFilter.trim().toLowerCase();
    return animeOf(regrouping.shows.filter((s) => s.id !== card.id))
      .filter((a) => a.id !== card.series.id && (!typed || [a.title, a.native].some((t) => t.toLowerCase().includes(typed))))
      .sort((a, b) => a.title.localeCompare(b.title));
  });

  async function openRegroup() {
    regroupFilter = "";
    try {
      regrouping = await callKept<Library>("library");
    } catch (e) {
      toast({ key: "showerr", state: "failed", title: "That didn’t work", text: errorText(e) });
    }
  }

  /** Lists this show under the anime `to` (its key), on its own (0), or where AniList has it (null). */
  async function regroup(to: number | null) {
    if (!card) return;
    const show = card.id;
    regrouping = null;
    await act(() => call("moveShow", { id: show, to }));
  }

  async function freeSpace() {
    if (!card) return;
    try {
      const r = await call<{ episodes: number; bytes: number }>("freeSpace", { show: card.id });
      toast({ key: "free", state: "done", title: r.episodes ? `Freed ${bytes(r.bytes)}` : "Nothing to free", text: r.episodes ? `Removed Telly’s copies of ${plural(r.episodes, "watched episode")}. The rips are untouched.` : "Telly only removes its copies of watched episodes." });
    } catch (e) {
      toast({ key: "free", state: "failed", title: "Couldn’t free up space", text: errorText(e) });
    }
    load();
  }

  const missing = $derived((view?.episodes ?? []).filter((e) => ["none", "nokey", "failed"].includes(e.subsState)).length);

  /** Who makes subtitles Jimaku doesn't have: "MAI-Transcribe", "ElevenLabs" or "Whisper". */
  const maker = $derived(view?.whisper.maker ?? "Whisper");

  async function whisperAll() {
    if (!card) return;
    try {
      const r = await call<{ queued: number }>("whisperSubs", { show: card.id });
      toast({ key: "whisper", state: "info", title: `${maker} will make subtitles for ${plural(r.queued, "episode")}`, text: "One at a time, a few minutes each. They’re marked as machine-made." });
    } catch (e) {
      toast({ key: "whisper", state: "failed", title: `${maker} can’t make subtitles`, text: errorText(e) });
    }
    load();
  }

  async function getModel() {
    try {
      await call("getWhisperModel");
      // The title bar takes over from here, so this goes after a few seconds.
      toast({ key: "whispermodel", state: "working", title: "Getting Whisper’s Japanese model", text: "The title bar shows how it’s going." }, 6000);
    } catch (e) {
      toast({ key: "whispermodel", state: "failed", title: "Couldn’t get Whisper’s model", text: errorText(e) });
    }
  }

  async function lookAgain() {
    if (!card) return;
    const show = card;
    toast({ key: "rescan", state: "working", title: "Looking for new episodes", text: show.folder });
    try {
      const r = await call<{ added: number }>("rescan", { id: show.id });
      toast({ key: "rescan", state: "done", title: r.added ? `Added ${plural(r.added, "episode")}` : "No new episodes", text: r.added ? "They’ll be ready to play soon." : "Telly checks the folder every 10 minutes too." });
    } catch (e) {
      toast({ key: "rescan", state: "failed", title: "Couldn’t read the folder", text: errorText(e) });
    }
    load();
  }

  /** Follows the show, so its new episodes download themselves, or stops. */
  async function follow(on: boolean) {
    if (!card) return;
    const show = card;
    if (on) toast({ key: "follow", state: "working", title: "Looking for new episodes on your search site", text: show.title });
    try {
      const r = await call<{ following: boolean; looked?: { like: string | null; started: number[] }; error?: string | null }>("follow", { show: show.id, on });
      const hourly = "Telly looks every hour, and downloads each new episode into this show’s folder.";
      if (!r.following) toast({ key: "follow", state: "done", title: "Not following any more", text: "New episodes won’t download themselves." });
      else if (r.error) toast({ key: "follow", state: "failed", title: "Following, but your search site couldn’t be searched", text: `${r.error} Telly looks again in an hour.` });
      else if (!r.looked?.like) toast({ key: "follow", state: "info", title: "Nothing to follow yet", text: "None of this show’s rips is named for its release and episode, as [Group] Show - 05 is, so Telly can’t tell which release to follow." });
      else if (r.looked.started.length) toast({ key: "follow", state: "done", title: `Downloading ${r.looked.started.length > 1 ? "episodes" : "Episode"} ${r.looked.started.join(", ")}`, text: `The same release as ${r.looked.like}. ${hourly}` });
      else toast({ key: "follow", state: "done", title: "Following", text: `Nothing after ${r.looked.like} is out yet. ${hourly}` });
    } catch (e) {
      toast({ key: "follow", state: "failed", title: "That didn’t work", text: errorText(e) });
    }
    load();
  }

  /** What the page says when a show's rips aren't where Telly has them. */
  function awayText(m: MissingRips): string {
    if (m.folderMissing) return "This folder isn’t there: its drive may be unplugged, or the folder moved.";
    if (m.missing === m.episodes) return "Its rips aren’t where they were.";
    return m.missing === 1 ? `1 of its ${m.episodes} rips isn’t where it was.` : `${m.missing} of its ${m.episodes} rips aren’t where they were.`;
  }

  /** Asks which folder the show is in now, and shows what Telly finds of its rips there. */
  async function findFolder() {
    if (!card) return;
    const show = card;
    const folder = await pickFolder(`Find the folder ${show.title} is in now`);
    if (!folder) return;
    try {
      moving = await call<ShowMove>("relinkShow", { id: show.id, folder });
    } catch (e) {
      toast({ key: "relink", state: "failed", title: "Couldn’t look in that folder", text: errorText(e) });
    }
  }

  /** Points the show at the folder found, and its episodes at their rips there. */
  async function useFolder() {
    const found = moving;
    if (!found) return;
    moving = null;
    try {
      const done = await call<ShowMove>("relinkShow", { id: found.showId, folder: found.to, apply: true });
      const left = done.missing.map((m) => m.number).join(", ");
      const text = done.missing.length ? `Not found, and left as before: ${done.missing.length === 1 ? "episode" : "episodes"} ${left}` : done.to;
      toast({ key: "relink", state: "done", title: `Found ${plural(done.found.length, "episode")}`, text });
    } catch (e) {
      toast({ key: "relink", state: "failed", title: "The show’s folder wasn’t changed", text: errorText(e) });
    }
    load();
  }

  async function remove() {
    const show = card;
    if (!show || !confirm(`Remove ${show.title} from Telly? The video files stay where they are. Telly’s copies and subtitles are deleted.`)) return;
    try {
      await call("removeShow", { id: show.id });
      goto("/");
    } catch (e) {
      toast({ key: "remove", state: "failed", title: `Couldn’t remove ${show.title}`, text: errorText(e) });
    }
  }

  function tone(t: string) {
    return t ? `tone-${t}` : "";
  }
</script>

<div class="showpg scroll">
  <div class="sp-hero" style:--tint={card?.coverColor || null}>
  {#if card && card.series.parts > 1}
    <button type="button" class="back" onclick={() => card && goto(`/anime?id=${card.series.id}`)}><Icon name="back" /><span>Back to {card.series.title}</span></button>
  {:else}
    <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Back to library</span></button>
  {/if}
  {#if error}<p class="err">{error}</p>{/if}
  {#if card && st}
      <div class="sp-head">
        <div class="sp-cover"><Cover cover={card.cover} color={card.coverColor} native={card.titleNative} title={card.title} caption={card.coverCaption} /></div>
        <div class="sp-body">
          <p class="sp-chips">
            {#if card.format}<span class="chip">{card.format.replace("_", " ")}</span>{/if}
            {#if card.year}<span class="chip">{card.year}</span>{/if}
            {#if card.titleNative && card.titleNative !== card.title}<span class="chip">{card.title}</span>{/if}
            {#if !card.movie}
              {#if card.anilistEpisodes && card.episodes > card.anilistEpisodes}
                <span class="chip warn" title="Your folder has more episodes than AniList lists for this entry: Change match… if it’s matched to only one part of the show"><Icon name="warn" />{card.episodes} here · AniList lists {card.anilistEpisodes}</span>
              {:else}
                <span class="chip">{card.episodes}{card.anilistEpisodes ? ` of ${card.anilistEpisodes}` : ""} episodes here</span>
              {/if}
            {/if}
          </p>
          <h2 lang="ja">{card.titleNative || card.title}</h2>
          {#if about}
            <div class="sp-about">
              <p class:open={aboutOpen} bind:this={aboutBox}>{about}</p>
              {#if aboutLong || aboutOpen}
                <button type="button" class="sp-more-text" aria-expanded={aboutOpen} onclick={() => (aboutOpen = !aboutOpen)}>{aboutOpen ? "Show less" : "Read more"}</button>
              {/if}
            </div>
          {/if}
          {#if view?.ripsMissing}
            <p class="sp-away">
              <span>{awayText(view.ripsMissing)}</span>
              <button type="button" class="btn small quiet" onclick={findFolder} title="Choose the folder its rips are in now: where you were, what you watched, your cards and subtitles stay">Find its folder…</button>
            </p>
          {/if}
          {#if completion && (completion.finishedAt !== null || completion.rewatches.length)}
            <dl class="sp-history">
              {#if completion.finishedAt !== null}
                <div>
                  <dt>Finished</dt>
                  <dd><time datetime={new Date(completion.finishedAt).toISOString()}>{date.format(completion.finishedAt)}</time></dd>
                </div>
              {/if}
              {#if completion.rewatches.length}
                <div>
                  <dt>Full rewatches</dt>
                  <dd>
                    <ul role="list">
                      {#each completion.rewatches as rewatchedAt}
                        <li><time datetime={new Date(rewatchedAt).toISOString()}>{date.format(rewatchedAt)}</time></li>
                      {/each}
                    </ul>
                  </dd>
                </div>
              {/if}
            </dl>
          {/if}
          {#if !card.movie && card.episodes > 1}
            <div class="sp-progress">
              <p><span><strong>{card.watched}</strong> of {card.episodes} watched</span>{#if next}<span>Next: Episode {next.number}</span>{/if}</p>
              <span class="bar"><i style:width="{Math.round((100 * card.watched) / Math.max(1, card.episodes))}%"></i></span>
            </div>
          {/if}
          <div class="sp-actions">
            {#if next}
              <button type="button" class="btn big go" disabled={!next.ready} onclick={() => next && play({ id: next.episodeId, mediaState: next.mediaState })}>
                <Icon name="play" /><span>{next.ready ? (card.movie ? (next.resume ? "Resume" : "Watch") : next.resume ? `Resume Episode ${next.number}` : `Watch Episode ${next.number}`) : st.sub}</span>
              </button>
            {/if}
            <button type="button" class="btn big quiet" onclick={() => card && goto(`/premine?show=${card.id}`)} title="Make cards for the words that take you to the share of the show you choose"><Icon name="card" /><span>Premine</span></button>
            {#if downloadsOn(settings)}
              <button type="button" class="btn big quiet" onclick={() => card && goto(`/downloads?show=${card.id}`)} title="Find more of it on your search site: Telly downloads it into this show’s folder, and each episode joins as it finishes"><Icon name="download" /><span>Download episodes…</span></button>
            {/if}
            <span class="sp-more">
              <button type="button" class="btn big quiet icon" class:on={more} aria-label="More for this show" aria-haspopup="menu" aria-expanded={more} title="More" onclick={(e) => { e.stopPropagation(); menu = null; more = !more; }}><Icon name="more" /></button>
              {#if more}
                <div class="menu sp-menu" role="menu">
                  <p class="menu-head">Episodes</p>
                  <button type="button" role="menuitem" onclick={() => { more = false; lookAgain(); }}>Look for new episodes<small>Check its folder for rips added since</small></button>
                  {#if view?.following}
                    <button type="button" role="menuitem" onclick={() => { more = false; follow(false); }}>Stop following<small>New episodes stop downloading themselves</small></button>
                  {:else if canFollow && downloadsOn(settings)}
                    <button type="button" role="menuitem" onclick={() => { more = false; follow(true); }}>Follow new episodes<small>Download each new one from your search site as it comes out</small></button>
                  {/if}
                  <button type="button" role="menuitem" onclick={() => { more = false; card && act(() => call("retrySubs", { show: card.id })); }}>Find subtitles again<small>Look on Jimaku for every episode</small></button>
                  <hr />
                  <p class="menu-head">In your library</p>
                  <button type="button" role="menuitem" onclick={() => { more = false; matching = true; query = card?.titleRomaji || card?.title || ""; found = []; foundTmdb = []; search(); }}>Change match…</button>
                  <button type="button" role="menuitem" onclick={() => { more = false; openRegroup(); }} title="List it in the Library under another anime, with that anime’s seasons and movies, or on its own">Move to another anime…</button>
                  <button type="button" role="menuitem" onclick={() => { more = false; freeSpace(); }}>Free up space<small>Delete Telly’s copies of the episodes you’ve watched</small></button>
                  <hr />
                  <button type="button" role="menuitem" class="bad" onclick={() => { more = false; remove(); }}>Remove…</button>
                </div>
              {/if}
            </span>
          </div>
        </div>
      </div>
  {/if}
  </div>
  {#if card && st}
    <div class="sp-status">
      <div class="sp-card">
        {#if card.subsReady < card.episodes}
          <span class="sp-ico wait"><span class="spin"></span></span>
          <span class="sp-card-text"><b>Finding subtitles</b><small>{card.subsReady} of {card.episodes} done</small></span>
        {:else if card.subsFound === card.episodes}
          <span class="sp-ico ok"><Icon name="check" /></span>
          <span class="sp-card-text"><b>Subtitles</b><small>{card.movie ? "Found" : card.episodes === 1 ? "Its episode has them" : "Every episode has them"}</small></span>
        {:else}
          <span class="sp-ico warn"><Icon name="subs" /></span>
          <span class="sp-card-text"><b>Subtitles</b><small>For {card.subsFound} of {plural(card.episodes, "episode")}</small></span>
          {#if missing > 0 && view?.whisper.ready}
            <button type="button" class="btn small quiet" onclick={whisperAll} title="For the {missing} episodes without subtitles; marked as machine-made">Make with {maker}</button>
          {:else if missing > 0 && view?.whisper.needsModel}
            <button type="button" class="btn small quiet" onclick={getModel} title="So Whisper can make subtitles Jimaku doesn’t have">Get Whisper’s model</button>
          {/if}
        {/if}
      </div>
      <div class="sp-card">
        {#if card.mediaReady < card.episodes}
          <span class="sp-ico wait"><span class="spin"></span></span>
          <span class="sp-card-text"><b>Getting video ready</b><small>{card.mediaReady} of {card.episodes} ready</small></span>
        {:else}
          <span class="sp-ico ok"><Icon name="check" /></span>
          <span class="sp-card-text"><b>Video</b><small>{card.episodes === 1 ? "Ready to play" : `All ${card.episodes} ready to play`}</small></span>
        {/if}
      </div>
      {#if view?.following || canFollow}
        <div class="sp-card">
          <span class="sp-ico" class:ok={view?.following}><Icon name="download" /></span>
          <span class="sp-card-text">{#if view?.following}<b>Following</b><small>New episodes download themselves</small>{:else}<b>Not following</b><small>New episodes aren’t downloaded</small>{/if}</span>
        </div>
      {/if}
      <div class="sp-card">
        <span class="sp-ico"><Icon name="folder" /></span>
        <span class="sp-folder mono" title={card.folder}>{card.folder}</span>
      </div>
    </div>
    <div class="eps">
      <div class="ep-row head"><span>#</span><span>Video</span><span>Subtitles</span><span class="right" title={view?.wordsError || "The share of each episode’s words that are known (Settings → Cards and Anki → Known words)"}>Words known</span><span class="right">Watched</span></div>
      {#each episodes as ep (ep.id)}
        {@const m = mediaLabel(ep.mediaState, ep.mediaPlan, ep.mediaProgress)}
        {@const s = subsLabel(ep.subsState, ep.subsScore, ep.subsFile, ep.subsDetail, maker)}
        <button type="button" class="ep-row" class:next={ep.isNext} onclick={() => play(ep)} oncontextmenu={(e) => rowMenu(e, ep)} title={ep.path}>
          <span class="ep-n">{ep.number}</span>
          <span class="cell {tone(m.tone)}" title={ep.mediaError || m.text}>
            {#if m.tone === "ok"}<Icon name="check" />{:else if ep.mediaState === "working"}<span class="spin"></span>{/if}
            <span>{m.text}{ep.duration > 0 ? ` · ${clock(ep.duration)}` : ""}</span>
          </span>
          <span class="cell {tone(s.tone)}" title={subsHelp(ep.subsState, ep.subsDetail, ep.subsFile, maker)}>
            {#if s.tone === "ok"}<Icon name="check" />{:else if ep.subsState === "working"}<span class="spin"></span>{/if}
            <span>{s.text}</span>
          </span>
          <span class="ep-known" title={ep.words ? `${ep.words.known} of ${ep.words.uses} words` : ""}>{ep.words?.uses ? `${Math.round((100 * ep.words.known) / ep.words.uses)}%` : ""}</span>
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
    <button type="button" role="menuitem" onclick={() => act(() => call("markWatched", { id: ep.id, watched: !ep.watched }))}>{ep.watched ? "Mark as unwatched" : "Mark as watched"}</button>
    <button type="button" role="menuitem" onclick={() => chooseSubtitle(ep)}>Use a subtitle file…</button>
    <button type="button" role="menuitem" onclick={() => act(() => call("retrySubs", { id: ep.id }))}>Find subtitles again</button>
    {#if view?.whisper.ready}
      <button type="button" role="menuitem" onclick={() => act(() => call("whisperSubs", { id: ep.id }))}>Make subtitles with {maker}</button>
    {/if}
    {#if ep.mediaState === "failed" || ep.mediaState === "ready"}
      <button type="button" role="menuitem" onclick={() => act(() => call("retryMedia", { id: ep.id }))}>{ep.mediaState === "failed" ? "Try converting again" : "Convert again"}</button>
    {/if}
  </div>
{/if}

{#if moving}
  {@const total = moving.found.length + moving.missing.length}
  <div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && (moving = null)}>
    <div class="sheet" role="dialog" aria-modal="true" aria-label="Find its folder">
      <div class="mv-head">
        <h3>{moving.found.length} of {plural(total, "episode")} found</h3>
        <p class="sheet-note mono">{moving.to}</p>
      </div>
      {#if moving.missing.length}
        <div class="mv-missing">
          <p class="sheet-note">Not found, and left as they are:</p>
          <ul class="mv-list scroll">
            {#each moving.missing as m (m.id)}
              <li><span class="ep-n">{m.number}</span><span class="mono" title={m.path}>{m.path.split(/[\\/]/).pop()}</span></li>
            {/each}
          </ul>
        </div>
      {/if}
      <p class="sheet-note">
        {moving.found.length
          ? "Telly will play them from here, and keep where you were, what you watched, your cards and subtitles. The rips aren’t moved or changed."
          : "None of this show’s rips are in this folder."}
      </p>
      <div class="sheet-foot">
        <button type="button" class="btn quiet" onclick={() => (moving = null)}>Cancel</button>
        <button type="button" class="btn quiet" onclick={findFolder}>Choose another folder…</button>
        <button type="button" class="btn" disabled={!moving.found.length} onclick={useFolder}>Use this folder</button>
      </div>
    </div>
  </div>
{/if}

{#if regrouping && card}
  <div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && (regrouping = null)}>
    <div class="sheet" role="dialog" aria-modal="true" aria-label="Move to another anime">
      <input class="input" bind:value={regroupFilter} placeholder="Search" aria-label="Search the library’s anime" />
      <div class="results scroll">
        {#if view?.seriesMoved}
          <button type="button" class="result plain" onclick={() => regroup(null)}><span class="r-body"><span class="r-en">Put back where AniList has it</span></span></button>
        {/if}
        {#if card.series.parts > 1}
          <button type="button" class="result plain" onclick={() => regroup(0)}><span class="r-body"><span class="r-en">List on its own</span></span></button>
        {/if}
        {#each otherAnime as a (a.id)}
          <button type="button" class="result plain" onclick={() => regroup(a.id)}>
            <span class="r-body">
              <span class="r-en">{a.title}</span>
              <span class="r-meta">{a.parts.length > 1 ? partsText(a.parts) : a.native}</span>
            </span>
          </button>
        {/each}
      </div>
      <div class="sheet-foot"><button type="button" class="btn quiet" onclick={() => (regrouping = null)}>Cancel</button></div>
    </div>
  </div>
{/if}

{#if matching}
  <div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && (matching = false)}>
    <div class="sheet" role="dialog" aria-modal="true" aria-label="Change match">
      <form class="searchrow" onsubmit={(e) => { e.preventDefault(); search(); }}>
        <input class="input grow" bind:value={query} aria-label="Search AniList and TMDB" />
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
        {#if foundTmdb.length}
          <div class="r-source">Dramas and films on TMDB</div>
          {#each foundTmdb as t (t.id)}
            <button type="button" class="result" onclick={() => rematchTmdb(t)}>
              {#if t.cover}<img src={t.cover} alt="" />{:else}<span class="noimg"></span>{/if}
              <span class="r-body">
                <span class="r-jp" lang="ja">{t.native || t.english}</span>
                <span class="r-en">{t.english}</span>
                <span class="r-meta">{[t.format === "MOVIE" ? "Film" : "Series", t.year].filter(Boolean).join(" · ")}</span>
              </span>
            </button>
          {/each}
        {/if}
      </div>
      <div class="sheet-foot"><button type="button" class="btn quiet" onclick={() => (matching = false)}>Cancel</button></div>
    </div>
  </div>
{/if}

<style>
  .showpg { position: absolute; inset: 0; overflow: auto; padding: 18px 32px 40px; }
  /* The top of the page, tinted with a little of the cover's colour. */
  .sp-hero { margin: -18px -32px 0; padding: 18px 32px 30px; background: color-mix(in srgb, var(--tint, var(--bg)) 14%, var(--bg)); border-bottom: 1px solid var(--rule); }
  .sp-head { display: grid; grid-template-columns: 172px minmax(0, 1fr); gap: 32px; align-items: end; max-width: 1180px; margin: 14px auto 0; }
  .sp-cover { align-self: start; border-radius: 10px; overflow: hidden; box-shadow: var(--shadow-lg); }
  .sp-cover :global(.cv) { display: block; width: 100%; }
  .sp-body { display: grid; gap: 12px; min-width: 0; }
  .sp-head h2 { font: 800 clamp(32px, 4.4vw, 54px)/1.08 var(--font-jp-title); letter-spacing: .03em; overflow-wrap: anywhere; }
  .sp-chips { display: flex; flex-wrap: wrap; gap: 7px; }
  .chip { display: inline-flex; align-items: center; gap: 6px; padding: 4px 10px; border-radius: 999px; font-size: 12px; color: var(--ink-2); background: color-mix(in srgb, var(--ink) 6%, transparent); border: 1px solid var(--rule); }
  .chip.warn { color: var(--warn); border-color: color-mix(in srgb, var(--warn-dot) 45%, transparent); background: color-mix(in srgb, var(--warn-dot) 12%, transparent); }
  .chip :global(svg) { width: 13px; height: 13px; }
  .sp-about { display: grid; justify-items: start; gap: 2px; max-width: 68ch; }
  .sp-about p { font-size: 14px; line-height: 1.6; color: var(--ink-2); white-space: pre-line; display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; line-clamp: 3; overflow: hidden; }
  .sp-about p.open { display: block; }
  .sp-more-text { all: unset; margin-left: -6px; padding: 4px 6px; border-radius: 5px; font-weight: 600; font-size: 13px; color: var(--ink); cursor: pointer; }
  .sp-more-text:hover { background: var(--sunk); }
  .sp-more-text:focus-visible { outline: 2px solid var(--ink); }
  .sp-progress { display: grid; gap: 7px; max-width: 520px; }
  .sp-progress p { display: flex; justify-content: space-between; gap: 12px; font-size: 13px; color: var(--ink-2); }
  .sp-progress strong { color: var(--ink); font-weight: 600; }
  .sp-progress .bar { display: block; width: 100%; height: 6px; border-radius: 3px; background: color-mix(in srgb, var(--ink) 12%, transparent); }
  .sp-progress .bar i { min-width: 6px; background: var(--mark); }
  .sp-away { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; font-size: 13px; color: var(--warn); }
  .mv-head { display: grid; gap: 4px; }
  .mv-head h3 { font: 600 17px/1.3 var(--font-ui); }
  .mv-missing { display: grid; gap: 6px; }
  .mv-list { display: grid; max-height: 220px; overflow: auto; margin: 0; padding: 0; border-top: 1px solid var(--rule); }
  .mv-list li { list-style: none; display: grid; grid-template-columns: 44px minmax(0, 1fr); gap: 12px; padding: 7px 6px; border-bottom: 1px solid var(--rule); font-size: 13px; }
  .mv-list .mono { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sp-history { display: grid; gap: 3px; margin: 8px 0 0; font-size: 13px; color: var(--ink-2); }
  .sp-history > div, .sp-history ul { display: flex; flex-wrap: wrap; gap: 2px 9px; }
  .sp-history dt { font-weight: 600; color: var(--ink); }
  .sp-history dd, .sp-history ul { margin: 0; padding: 0; }
  .sp-history li { list-style: none; white-space: nowrap; }
  .sp-history li + li::before { content: "·"; color: var(--ink-3); margin-right: 9px; }
  .sp-actions { display: flex; align-items: center; gap: 10px; margin-top: 4px; flex-wrap: wrap; }
  .btn.go { color: var(--mark-ink); background: var(--mark); border-color: var(--mark); }
  .btn.go:hover { background: color-mix(in srgb, var(--mark) 82%, #ffffff); }
  .btn.icon { width: 42px; padding: 0; }
  .sp-actions .btn :global(svg) { width: 17px; height: 17px; flex: none; }
  .sp-actions .btn.icon :global(svg) { width: 20px; height: 20px; }
  .btn.quiet.on { background: var(--sunk); }
  .sp-more { position: relative; }
  .menu.sp-menu { position: absolute; top: calc(100% + 6px); left: 0; width: 290px; }
  .sp-menu small { display: block; margin-top: 2px; font-size: 12px; color: var(--ink-2); }
  .sp-menu hr { border: 0; border-top: 1px solid var(--rule); margin: 5px 4px; }
  .menu-head { padding: 7px 10px 3px; font: 600 11px/1 var(--font-ui); letter-spacing: .07em; text-transform: uppercase; color: var(--ink-3); }
  .menu button.bad { color: var(--bad); }
  .sp-status { display: grid; grid-template-columns: repeat(auto-fit, minmax(230px, 1fr)); gap: 10px; max-width: 1180px; margin: 20px auto 26px; }
  .sp-card { display: flex; align-items: center; gap: 12px; min-width: 0; padding: 11px 14px; border-radius: 10px; background: var(--surface); border: 1px solid var(--rule); }
  .sp-ico { display: grid; place-items: center; flex: none; width: 32px; height: 32px; border-radius: 8px; color: var(--ink-2); background: var(--sunk); }
  .sp-ico :global(svg) { width: 16px; height: 16px; }
  .sp-ico.ok { color: var(--ok); background: color-mix(in srgb, var(--ok-dot) 14%, transparent); }
  .sp-ico.warn { color: var(--warn); background: color-mix(in srgb, var(--warn-dot) 14%, transparent); }
  .sp-ico .spin { width: 12px; height: 12px; }
  .sp-card-text { display: grid; gap: 2px; flex: 1; min-width: 0; font-size: 13.5px; }
  .sp-card-text small { font-size: 12px; color: var(--ink-2); }
  .sp-folder { flex: 1; min-width: 0; font-size: 12px; color: var(--ink-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .eps { border-top: 1px solid var(--rule); max-width: 1180px; margin: 0 auto; }
  @media (max-width: 720px) {
    .sp-head { grid-template-columns: 112px minmax(0, 1fr); gap: 20px; align-items: start; }
  }
  .ep-row { display: grid; grid-template-columns: 44px minmax(0, 1fr) minmax(0, 1.3fr) 110px 120px; gap: 12px; align-items: center; padding: 9px 6px; border-bottom: 1px solid var(--rule); font-size: 13px; color: var(--ink-2); }
  .ep-row.head { font: 600 12px/1 var(--font-ui); color: var(--ink-3); padding: 10px 6px; }
  button.ep-row { all: unset; box-sizing: border-box; display: grid; grid-template-columns: 44px minmax(0, 1fr) minmax(0, 1.3fr) 110px 120px; gap: 12px; align-items: center; padding: 9px 6px; border-bottom: 1px solid var(--rule); font-size: 13px; color: var(--ink-2); width: 100%; cursor: pointer; }
  /* Rows off screen aren't laid out or painted until they're scrolled to. */
  button.ep-row { content-visibility: auto; contain-intrinsic-size: auto 43px; }
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
  .ep-known { justify-self: end; font: 500 12.5px/1 var(--font-mono); color: var(--ink); font-variant-numeric: tabular-nums; }
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
  .result:focus-visible { outline: 2px solid var(--ink); outline-offset: -2px; }
  .result.plain { grid-template-columns: minmax(0, 1fr); padding: 9px 8px; }
  .result img, .result .noimg { width: 46px; aspect-ratio: 135 / 190; object-fit: cover; border-radius: 2px; background: var(--sunk); display: block; }
  .r-body { display: grid; gap: 2px; min-width: 0; }
  .r-jp { font: 800 15px/1.25 var(--font-jp-title); }
  .r-en { font-size: 13px; }
  .r-meta { font-size: 12px; color: var(--ink-3); }
  .r-source { padding: 12px 6px 4px; font-size: 12px; font-weight: 600; color: var(--ink-3); }
</style>
