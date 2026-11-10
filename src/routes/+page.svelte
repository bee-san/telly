<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import AddSheet from "$lib/AddSheet.svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, errorText, pickFolder, setWindowTitle, type Library, type ShowCard } from "$lib/api";
  import { statusOf } from "$lib/format";
  import { app } from "$lib/state.svelte";

  let lib = $state<Library | null>(null);
  let error = $state("");
  let q = $state("");
  let adding = $state<string | null>(null);
  let menu = $state<{ show: ShowCard; x: number; y: number } | null>(null);

  async function load() {
    try {
      lib = await call<Library>("library");
      error = "";
    } catch (e) {
      error = errorText(e);
    }
  }

  onMount(() => {
    app.dark = false;
    setWindowTitle("Telly");
    load();
    // /?add=<folder> opens Add media on that folder (handy for testing in a browser).
    const folder = page.url.searchParams.get("add");
    if (folder) adding = folder;
    let timer: ReturnType<typeof setTimeout>;
    const loop = async () => {
      await load();
      const busy = lib?.shows.some((s) => s.status === "getting" || s.subsReady < s.episodes) ?? false;
      timer = setTimeout(loop, busy ? 2000 : 8000);
    };
    timer = setTimeout(loop, 2000);
    const close = () => (menu = null);
    window.addEventListener("click", close);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("click", close);
    };
  });

  function matches(s: ShowCard): boolean {
    const t = q.trim().toLowerCase();
    if (!t) return true;
    return [s.title, s.titleNative, s.titleEnglish, s.titleRomaji].join(" ").toLowerCase().includes(t);
  }

  const shows = $derived((lib?.shows ?? []).filter(matches));
  const watching = $derived(
    shows.filter((s) => s.status === "watching" || (s.status === "getting" && (s.watched > 0 || !!s.next?.resume))).sort((a, b) => (b.lastWatchedAt ?? 0) - (a.lastWatchedAt ?? 0)),
  );
  const rest = $derived(shows.filter((s) => !watching.includes(s)));

  async function add() {
    const folder = await pickFolder();
    if (folder) adding = folder;
  }

  function open(s: ShowCard) {
    if (s.next && s.next.ready) goto(`/watch?ep=${s.next.episodeId}`);
    else goto(`/show?id=${s.id}`);
  }

  function context(e: MouseEvent, s: ShowCard) {
    e.preventDefault();
    e.stopPropagation();
    menu = { show: s, x: e.clientX, y: e.clientY };
  }

  async function remove(s: ShowCard) {
    menu = null;
    if (!confirm(`Remove ${s.title} from Telly? The video files stay where they are. Telly’s copies and subtitles are deleted.`)) return;
    await call("removeShow", { id: s.id });
    load();
  }

  function count(n: number) {
    return `${n} ${n === 1 ? "show" : "shows"}`;
  }
</script>

<div class="lib scroll">
  <div class="toolbar">
    <label class="search">
      <Icon name="search" />
      <input type="search" bind:value={q} placeholder="Search" aria-label="Search shows" />
    </label>
    <span class="grow"></span>
    <a class="btn quiet" href="/settings"><Icon name="gear" /><span>Settings</span></a>
    <button type="button" class="btn" onclick={add}><Icon name="plus" /><span>Add media…</span></button>
  </div>

  {#if lib && (!lib.setup.ffmpeg || !lib.setup.jimaku)}
    <p class="setup">
      {#if !lib.setup.ffmpeg}Telly needs ffmpeg to play and convert rips.{/if}
      {#if !lib.setup.jimaku}Add a Jimaku API key so Telly can find subtitles.{/if}
      <a href="/settings" class="link-btn">Open Settings</a>
    </p>
  {/if}
  {#if error}<p class="err">{error}</p>{/if}

  {#if lib && lib.shows.length === 0}
    <div class="first">
      <h2>No shows yet</h2>
      <p>Press <b>Add media…</b> and pick a folder of rips. Telly works out the show, finds Japanese subtitles, syncs them, and gets each episode ready to play. The files stay where they are.</p>
    </div>
  {:else if lib}
    <div class="shelf-head"><h2>Currently watching</h2><span>{count(watching.length)}</span></div>
    {#if watching.length}
      <div class="watching">
        {#each watching as s (s.id)}
          {@const st = statusOf(s)}
          <button type="button" class="wcard" onclick={() => open(s)} oncontextmenu={(e) => context(e, s)} aria-label="{s.title}. {st.text}. {st.sub}">
            <span class="wcv">
              <Cover cover={s.cover} color={s.coverColor} native={s.titleNative} title={s.title} />
              <span class="wplay"><Icon name="play" /></span>
            </span>
            <span class="w-body">
              <span class="w-jp" lang="ja">{s.titleNative || s.title}</span>
              <span class="w-en">{s.titleNative ? s.title : ""}</span>
              <span class="w-next">{st.text}</span>
              <span class="w-sub">{st.sub}</span>
              <span class="bar"><i style:width="{Math.round((100 * s.watched) / Math.max(1, s.episodes))}%"></i></span>
            </span>
          </button>
        {/each}
      </div>
    {:else}
      <p class="empty">{q ? "No show in progress matches." : "Nothing in progress. Pick something from the library."}</p>
    {/if}

    <div class="shelf-head"><h2>Library</h2><span>{count(rest.length)}</span></div>
    {#if rest.length}
      <div class="libgrid">
        {#each rest as s (s.id)}
          {@const st = statusOf(s)}
          <button type="button" class="lcard" onclick={() => open(s)} oncontextmenu={(e) => context(e, s)} aria-label="{s.title}. {st.text}. {st.sub}">
            <Cover cover={s.cover} color={s.coverColor} native={s.titleNative} title={s.title} />
            <span class="l-title">{s.title}</span>
            <span class="l-status" class:ok={st.ok}>{st.text}</span>
            <span class="l-sub">{st.sub}</span>
            {#if s.status === "getting" && s.next}
              <span class="bar"><i style:width="{Math.round(s.next.mediaProgress * 100)}%"></i></span>
            {/if}
          </button>
        {/each}
      </div>
    {:else}
      <p class="empty">{q ? "No shows match." : "Every show in the library is in progress."}</p>
    {/if}
  {/if}
</div>

{#if menu}
  <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px" role="menu">
    <button type="button" role="menuitem" onclick={() => menu && goto(`/show?id=${menu.show.id}`)}>Show episodes</button>
    {#if menu.show.next?.ready}
      <button type="button" role="menuitem" onclick={() => menu?.show.next && goto(`/watch?ep=${menu.show.next.episodeId}`)}>Play Episode {menu.show.next.number}</button>
    {/if}
    <button type="button" role="menuitem" class="danger" onclick={() => menu && remove(menu.show)}>Remove from Telly…</button>
  </div>
{/if}

{#if adding}
  <AddSheet folder={adding} onclose={() => (adding = null)} onadded={() => { adding = null; load(); }} />
{/if}

<style>
  .lib { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 40px; }
  .toolbar { display: flex; align-items: center; gap: 12px; margin-bottom: 24px; }
  .search { display: flex; align-items: center; gap: 8px; width: 320px; height: 38px; padding: 0 12px; border: 1px solid var(--rule-2); border-radius: 6px; background: var(--surface); color: var(--ink-3); }
  .search:focus-within { border-color: var(--ink); }
  .search :global(svg) { width: 16px; height: 16px; flex: none; }
  .search input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; font: 14px/1 var(--font-ui); color: var(--ink); }
  .toolbar a.btn { text-decoration: none; }
  .setup { display: flex; flex-wrap: wrap; gap: 6px 10px; align-items: baseline; padding: 10px 14px; margin: -8px 0 22px; border: 1px solid var(--rule-2); border-radius: 8px; background: var(--surface); font-size: 13.5px; color: var(--ink-2); }
  .first { max-width: 60ch; padding: 60px 0; display: grid; gap: 10px; }
  .first h2 { font: 600 22px/1.25 var(--font-display); }
  .first p { color: var(--ink-2); font-size: 15px; }
  .shelf-head { display: flex; align-items: baseline; gap: 10px; margin: 0 0 14px; }
  .shelf-head h2 { font: 600 17px/1.2 var(--font-ui); }
  .shelf-head span { font-size: 13px; color: var(--ink-3); }
  .watching { display: grid; grid-template-columns: repeat(auto-fill, minmax(330px, 1fr)); gap: 16px; margin-bottom: 30px; }
  .wcard { all: unset; box-sizing: border-box; display: grid; grid-template-columns: 92px minmax(0, 1fr); gap: 16px; align-items: center; padding: 12px 16px 12px 12px; border-radius: 10px; background: var(--surface); border: 1px solid var(--rule); cursor: pointer; transition: border-color .15s, box-shadow .15s; }
  .wcard:hover { border-color: var(--rule-2); box-shadow: var(--shadow); }
  .wcard:focus-visible { outline: 2px solid var(--ink); outline-offset: 2px; }
  .wcv { position: relative; display: block; }
  .wplay { position: absolute; left: 50%; top: 50%; width: 36px; height: 36px; margin: -18px 0 0 -18px; border-radius: 50%; display: grid; place-items: center; background: rgba(0, 0, 0, .62); color: #fff; opacity: 0; transition: opacity .15s; }
  .wplay :global(svg) { width: 16px; height: 16px; margin-left: 2px; }
  .wcard:hover .wplay, .wcard:focus-visible .wplay { opacity: 1; }
  .w-body { display: grid; gap: 3px; min-width: 0; }
  .w-jp { font: 800 16px/1.3 var(--font-jp-title); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .w-en { font-size: 12.5px; color: var(--ink-3); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .w-next { font: 600 15.5px/1.3 var(--font-ui); margin-top: 8px; }
  .w-sub { font-size: 12.5px; color: var(--ink-2); }
  .w-body .bar { margin-top: 8px; }
  .libgrid { display: grid; grid-template-columns: repeat(auto-fill, minmax(132px, 1fr)); gap: 22px 20px; }
  .lcard { all: unset; box-sizing: border-box; display: grid; gap: 3px; align-content: start; cursor: pointer; min-width: 0; }
  .lcard :global(.cv) { transition: transform .15s; }
  .lcard:hover :global(.cv) { transform: translateY(-2px); }
  .lcard:focus-visible { outline: 2px solid var(--ink); outline-offset: 4px; border-radius: 4px; }
  .l-title { font: 600 13.5px/1.3 var(--font-ui); margin-top: 9px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .l-status { font: 500 12.5px/1.3 var(--font-ui); }
  .l-status.ok { color: var(--ok); }
  .l-sub { font-size: 12px; color: var(--ink-3); }
  .lcard .bar { margin-top: 5px; }
  .empty { font-size: 14px; color: var(--ink-2); margin-bottom: 34px; }
  .menu { position: fixed; z-index: 80; min-width: 190px; display: grid; padding: 5px; background: var(--surface); border: 1px solid var(--rule-2); border-radius: 8px; box-shadow: var(--shadow-lg); }
  .menu button { all: unset; padding: 8px 10px; border-radius: 5px; font-size: 13.5px; cursor: pointer; }
  .menu button:hover, .menu button:focus-visible { background: var(--sunk); }
  .menu .danger { color: var(--bad); }
</style>
