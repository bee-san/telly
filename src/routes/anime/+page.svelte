<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import AddSheet from "$lib/AddSheet.svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, callKept, errorText, kept, pickFolder, setWindowTitle, type Library, type ShowCard } from "$lib/api";
  import { bytes, plural, statusOf } from "$lib/format";
  import { animeOf, kindOf, partsText, wordsOf } from "$lib/series";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  // An anime's page: its seasons, movies and specials in the library, in the
  // order they came out. Each opens its own page of episodes.

  const id = $derived(Number(page.url.searchParams.get("id")));
  let lib = $state.raw<Library | null>(kept<Library>("library"));
  let error = $state("");
  let adding = $state<string | null>(null);

  async function load() {
    try {
      lib = await callKept<Library>("library");
      error = "";
    } catch (e) {
      error = errorText(e);
    }
  }

  onMount(() => {
    app.dark = false;
    let timer: ReturnType<typeof setTimeout>;
    // Stopped when the page is left, even with an answer on its way.
    let stopped = false;
    const loop = async () => {
      await load();
      if (stopped) return;
      const busy = anime?.parts.some((s) => s.status === "getting" || s.subsReady < s.episodes) ?? false;
      timer = setTimeout(loop, busy ? 2000 : 8000);
    };
    loop();
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  });

  const anime = $derived(animeOf((lib?.shows ?? []).filter((s) => s.series.id === id))[0] ?? null);
  const first = $derived(anime?.parts[0] ?? null);
  const now = $derived(anime?.current ?? null);
  const words = $derived(anime ? wordsOf(anime) : { uses: 0, known: 0 });

  /** What AniList says the first season or movie is about ("" until it's known, or when it says nothing). */
  let about = $state("");
  let aboutOpen = $state(false);
  let aboutBox = $state<HTMLParagraphElement | undefined>();
  /** The description is longer than the lines it's cut to. */
  let aboutLong = $state(false);
  const firstId = $derived(first?.id ?? null);

  $effect(() => {
    const part = firstId;
    about = "";
    aboutOpen = false;
    if (part === null) return;
    call<{ text: string }>("showAbout", { id: part })
      .then((r) => part === firstId && (about = r.text))
      .catch(() => {});
  });

  $effect(() => {
    if (!aboutBox || aboutOpen || !about) return;
    const box = aboutBox;
    const measure = () => (aboutLong = box.scrollHeight > box.clientHeight + 1);
    measure();
    const watch = new ResizeObserver(measure);
    watch.observe(box);
    return () => watch.disconnect();
  });

  $effect(() => {
    if (anime) setWindowTitle(`${anime.title} — Telly`);
    // Its last show was removed, or moved to another anime.
    else if (lib) goto("/");
  });

  /** What the button that plays the next part says. */
  function playText(s: ShowCard): string {
    const n = s.next;
    if (!n) return "";
    if (!n.ready) return statusOf(s).sub;
    if (s.movie) return n.resume ? "Resume" : "Watch";
    return n.resume ? `Resume Episode ${n.number}` : `Watch Episode ${n.number}`;
  }

  /** How far through a part you are. */
  function progress(s: ShowCard): string {
    if (s.movie) return s.status === "finished" ? "Watched" : s.next?.resume ? "Started" : "Not yet";
    return `${s.watched} of ${s.episodes}`;
  }

  async function add() {
    const folder = await pickFolder();
    if (folder) adding = folder;
  }

  /** Does `work` to every part, each whatever happens to the others; says which parts it couldn't do. */
  async function eachPart(work: (part: ShowCard) => Promise<void>): Promise<string> {
    const failed: string[] = [];
    for (const s of anime?.parts ?? []) {
      try {
        await work(s);
      } catch (e) {
        failed.push(`${s.title}: ${errorText(e)}`);
      }
    }
    return failed.join(" · ");
  }

  async function lookAgain() {
    if (!anime) return;
    toast({ key: "rescan", state: "working", title: "Looking for new episodes", text: anime.title });
    let added = 0;
    const failed = await eachPart(async (s) => void (added += (await call<{ added: number }>("rescan", { id: s.id })).added));
    const title = added ? `Added ${plural(added, "episode")}` : failed ? "Couldn’t read a folder" : "No new episodes";
    const text = failed ? (added ? `Couldn’t read ${failed}` : failed) : added ? "They’ll be ready to play soon." : "Telly checks the folders every 10 minutes too.";
    toast({ key: "rescan", state: failed && !added ? "failed" : "done", title, text });
    load();
  }

  async function findSubtitles() {
    if (!anime) return;
    const failed = await eachPart((s) => call("retrySubs", { show: s.id }));
    if (failed) toast({ key: "showerr", state: "failed", title: "That didn’t work", text: failed });
    load();
  }

  async function freeSpace() {
    if (!anime) return;
    let episodes = 0;
    let freed = 0;
    const failed = await eachPart(async (s) => {
      const r = await call<{ episodes: number; bytes: number }>("freeSpace", { show: s.id });
      episodes += r.episodes;
      freed += r.bytes;
    });
    if (failed && !episodes) toast({ key: "free", state: "failed", title: "Couldn’t free up space", text: failed });
    else toast({ key: "free", state: "done", title: episodes ? `Freed ${bytes(freed)}` : "Nothing to free", text: failed ? `Couldn’t free ${failed}` : episodes ? `Removed Telly’s copies of ${plural(episodes, "watched episode")}. The rips are untouched.` : "Telly only removes its copies of watched episodes." });
    load();
  }

  async function remove() {
    if (!anime) return;
    const a = anime;
    if (!confirm(`Remove ${a.title} and its ${a.parts.length} parts from Telly? The video files stay where they are. Telly’s copies and subtitles are deleted.`)) return;
    const failed = await eachPart((s) => call("removeShow", { id: s.id }));
    if (!failed) return void goto("/");
    toast({ key: "remove", state: "failed", title: `Couldn’t remove all of ${a.title}`, text: failed });
    load();
  }
</script>

<div class="animepg scroll">
  <div class="an-hero" style:--tint={first?.coverColor || null}>
  <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Back to library</span></button>
  {#if error}<p class="err">{error}</p>{/if}
  {#if anime && first && now}
    <div class="an-head">
      <div class="an-cover"><Cover cover={first.cover} color={first.coverColor} native={anime.native} title={anime.title} caption={first.coverCaption} /></div>
      <div class="an-body">
        <h2 lang={anime.native ? "ja" : undefined}>{anime.native || anime.title}</h2>
        <p class="an-meta">{anime.title} · {partsText(anime.parts)}{words.uses ? ` · ${Math.round((100 * words.known) / words.uses)}% of its words known` : ""}</p>
        {#if about}
          <div class="an-about">
            <p class:open={aboutOpen} bind:this={aboutBox}>{about}</p>
            {#if aboutLong || aboutOpen}
              <button type="button" class="an-more-text" aria-expanded={aboutOpen} onclick={() => (aboutOpen = !aboutOpen)}>{aboutOpen ? "Show less" : "Read more"}</button>
            {/if}
          </div>
        {/if}
        <div class="an-actions">
          {#if now.next}
            <button type="button" class="btn" disabled={!now.next.ready} onclick={() => now.next && goto(`/watch?ep=${now.next.episodeId}`)}>
              <Icon name="play" /><span>{playText(now)}</span>
            </button>
            <span class="an-now">{now.title}</span>
          {/if}
          <span class="grow"></span>
          <button type="button" class="btn quiet" onclick={() => goto(`/premine?anime=${anime.id}`)} title="Make cards for the words that take you to the share of the whole anime you choose">Premine</button>
          <button type="button" class="btn quiet" onclick={add} title="Pick the folder of another season or movie: it joins this anime when AniList has it as one of its seasons or movies">Add a season or movie…</button>
          <button type="button" class="btn quiet" onclick={lookAgain}>Look for new episodes</button>
          <button type="button" class="btn quiet" onclick={findSubtitles}>Find subtitles again</button>
          <button type="button" class="btn quiet" onclick={freeSpace}>Free up space</button>
          <button type="button" class="btn danger" onclick={remove}>Remove…</button>
        </div>
      </div>
    </div>
  {/if}
  </div>
  {#if anime && first && now}
    <div class="parts">
      <div class="part-row head"><span>Part</span><span></span><span>Watched</span><span class="right">Words known</span><span></span></div>
      {#each anime.parts as s (s.id)}
        {@const st = statusOf(s)}
        <a class="part-row" class:next={s.id === now.id && s.status !== "finished"} href="/show?id={s.id}" title={s.folder}>
          <span class="p-title"><b>{s.title}</b>{#if s.titleNative}<span lang="ja">{s.titleNative}</span>{/if}</span>
          <span>{[kindOf(s), s.year].filter(Boolean).join(" · ")}</span>
          <span class="p-num">{progress(s)}</span>
          <span class="p-num right">{s.wordsUses ? `${Math.round((100 * s.wordsKnown) / s.wordsUses)}%` : ""}</span>
          <span class="p-state" class:ok={st.ok}>{st.text === "Movie" ? "" : st.text}</span>
        </a>
      {/each}
    </div>
  {/if}
</div>

{#if adding}
  <AddSheet folder={adding} onclose={() => (adding = null)} onadded={() => { adding = null; load(); }} />
{/if}

<style>
  .animepg { position: absolute; inset: 0; overflow: auto; padding: 18px 32px 40px; }
  /* The top of the page, tinted with a little of the cover's colour, as a show's page is. */
  .an-hero { margin: -18px -32px 26px; padding: 18px 32px 30px; background: color-mix(in srgb, var(--tint, var(--bg)) 14%, var(--bg)); border-bottom: 1px solid var(--rule); }
  .an-head { display: grid; grid-template-columns: 172px minmax(0, 1fr); gap: 32px; align-items: end; margin-top: 14px; }
  .an-cover { border-radius: 10px; overflow: hidden; box-shadow: var(--shadow-lg); }
  .an-cover :global(.cv) { display: block; width: 100%; }
  .an-body { display: grid; gap: 12px; min-width: 0; }
  .an-head h2 { font: 800 clamp(32px, 4.4vw, 54px)/1.08 var(--font-jp-title); letter-spacing: .03em; overflow-wrap: anywhere; }
  .an-meta { font-size: 13.5px; color: var(--ink-2); }
  .an-about { display: grid; justify-items: start; gap: 2px; max-width: 68ch; }
  .an-about p { font-size: 14px; line-height: 1.6; color: var(--ink-2); white-space: pre-line; display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; line-clamp: 3; overflow: hidden; }
  .an-about p.open { display: block; }
  .an-more-text { all: unset; margin-left: -6px; padding: 4px 6px; border-radius: 5px; font-weight: 600; font-size: 13px; color: var(--ink); cursor: pointer; }
  .an-more-text:hover { background: var(--sunk); }
  .an-more-text:focus-visible { outline: 2px solid var(--ink); }
  .an-actions { display: flex; align-items: center; gap: 12px; margin-top: 4px; flex-wrap: wrap; }
  @media (max-width: 720px) {
    .an-head { grid-template-columns: 112px minmax(0, 1fr); gap: 20px; align-items: start; }
  }
  .an-now { font-size: 13px; color: var(--ink-2); }
  .parts { border-top: 1px solid var(--rule); }
  .part-row { display: grid; grid-template-columns: minmax(0, 2.2fr) minmax(0, 1fr) 110px 110px minmax(0, 1fr); gap: 12px; align-items: center; padding: 10px 6px; border-bottom: 1px solid var(--rule); font-size: 13px; color: var(--ink-2); text-decoration: none; }
  .part-row.head { font: 600 12px/1 var(--font-ui); color: var(--ink-3); }
  a.part-row:hover, a.part-row.next { background: var(--surface); }
  a.part-row:focus-visible { outline: 2px solid var(--ink); outline-offset: -2px; }
  .p-title { display: flex; align-items: baseline; gap: 10px; min-width: 0; }
  .p-title b { flex: 0 1 auto; max-width: 75%; font-weight: 600; color: var(--ink); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .p-title span { flex: 1 1 0; min-width: 0; font: 800 12.5px/1.3 var(--font-jp-title); color: var(--ink-3); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .p-num { font-variant-numeric: tabular-nums; }
  .right { text-align: right; }
  .p-state.ok { color: var(--ok); }
</style>
