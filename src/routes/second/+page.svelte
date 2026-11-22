<script lang="ts">
  // Telly on a second screen, like the AYN Thor's bottom one
  // (second.svelte.ts). While an episode plays it lists the lines, on the one
  // playing: a line's time goes to it, a word in it is looked up, and what
  // the word means shows here, off the picture. Outside the player it offers
  // what you're watching. It shows what the main screen's pages say, and
  // tells them what's touched.
  import { onMount } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import { clock } from "$lib/format";
  import { lineText, markWord, textAt, unmark } from "$lib/line-text";
  import { dictionaryStyles, type Found } from "$lib/lookup.svelte";
  import LookupPopup from "$lib/player/LookupPopup.svelte";
  import type { SecondShows, SecondTold, Touched } from "$lib/second.svelte";
  import { app } from "$lib/state.svelte";

  const shows = $state<SecondShows>({ playing: null, lines: [], at: { line: -1, paused: true }, look: null, styles: [], watching: [] });
  let popup = $state<ReturnType<typeof LookupPopup> | null>(null);
  let list = $state<HTMLOListElement | null>(null);
  let looked = $state<HTMLParagraphElement | null>(null);
  /** When the list was last scrolled by hand: it isn't moved back to the line playing for a while. */
  let scrolledAt = 0;

  /** Android's side of this page (SecondScreen.kt). */
  const android = () => (window as unknown as { TellySecond?: { ready(): void; post(json: string): void; close(): void } }).TellySecond;
  const post = (touched: Touched) => android()?.post(JSON.stringify(touched));

  /** Words tapped in a definition, waiting for the main screen's page to say what they mean. */
  const asked = new Map<number, (found: Found[]) => void>();
  let asks = 0;
  function ask(text: string): Promise<Found[]> {
    const id = ++asks;
    return new Promise((resolve) => {
      asked.set(id, resolve);
      post({ type: "lookup", id, text });
    });
  }

  function told<K extends keyof SecondTold>(about: K, value: SecondTold[K]) {
    if (about === "popup") {
      const press = value as SecondTold["popup"];
      if (press.step) popup?.step(press.step);
      if (press.scroll) popup?.scroll(press.scroll);
      if (press.add) popup?.add();
    } else {
      const answer = value as SecondTold["answer"];
      if (answer.styles.length) dictionaryStyles.list = answer.styles;
      asked.get(answer.id)?.(answer.found);
      asked.delete(answer.id);
    }
  }

  onMount(() => {
    app.dark = true;
    const page = window as unknown as { __tellySecond?: (about: string, value: unknown) => void };
    page.__tellySecond = (about, value) => {
      if (about === "popup" || about === "answer") told(about, value as never);
      else if (about === "styles") dictionaryStyles.list = value as SecondShows["styles"];
      else if (about in shows) Object.assign(shows, { [about]: value });
    };
    android()?.ready();
    return () => delete page.__tellySecond;
  });

  // Keeps the list on the line playing, unless it was just scrolled by hand.
  $effect(() => {
    const at = shows.at.line;
    void shows.lines.length;
    if (!list || at < 0 || Date.now() - scrolledAt < 4000) return;
    list.querySelector(`[data-i="${at}"]`)?.scrollIntoView({ block: "center", behavior: "smooth" });
  });

  // Marks the word looked up in its line.
  $effect(() => {
    const look = shows.look;
    void shows.lines;
    if (look && looked) markWord(lineText(looked).nodes, look.offset, look.length || 1);
    else unmark();
  });

  /** A touch on a line: on a word, it's looked up; beside the words, the episode goes to the line. */
  function touchLine(e: MouseEvent, line: number) {
    const at = textAt(e.currentTarget as HTMLElement, e.clientX, e.clientY);
    post(at ? { type: "look", line, offset: at.offset } : { type: "seek", line });
  }
</script>

<div class="second">
  <header>
    <span class="logo">T</span>
    <span class="what">
      <b>{shows.playing?.title ?? "Telly"}</b>
      {#if shows.playing?.episode}<span>{shows.playing.episode}</span>{/if}
    </span>
    <button type="button" class="ibtn" aria-label="Put this screen away" title="Put this screen away until Telly is opened again" onclick={() => android()?.close()}><Icon name="x" /></button>
  </header>

  {#if shows.playing && shows.look}
    {@const look = shows.look}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
    <p class="looked" lang="ja" bind:this={looked} onclick={(e) => touchLine(e, look.line)}>{@html shows.lines[look.line]?.html ?? ""}</p>
    <div class="means">
      <LookupPopup bind:this={popup} fill found={look.found} error={look.error} {ask} onadd={(word) => post({ type: "add", word })} />
    </div>
    <nav class="foot">
      <button type="button" class="wide" onclick={() => post({ type: "close" })}>Back to the lines</button>
    </nav>
  {:else if shows.playing}
    <ol class="lines" bind:this={list} ontouchmove={() => (scrolledAt = Date.now())} onwheel={() => (scrolledAt = Date.now())}>
      {#each shows.lines as line, i (i)}
        <li data-i={i} class:cur={i === shows.at.line} class:said={i < shows.at.line}>
          <button type="button" class="time" title="Go to this line" onclick={() => post({ type: "seek", line: i })}>{clock(line.start)}</button>
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
          <p lang="ja" onclick={(e) => touchLine(e, i)}>{#if line.speaker}<span class="speaker">{line.speaker}</span>{/if}{@html line.html}</p>
        </li>
      {:else}
        <li class="none">No subtitles for this episode yet.</li>
      {/each}
    </ol>
    <nav class="foot">
      <button type="button" aria-label="Previous line" title="Previous line" onclick={() => post({ type: "press", button: "previous" })}><Icon name="prevLine" /></button>
      <button type="button" class="big" aria-label={shows.at.paused ? "Play" : "Pause"} onclick={() => post({ type: "press", button: "play" })}><Icon name={shows.at.paused ? "play" : "pause"} /></button>
      <button type="button" aria-label="Replay the line" title="Replay the line" onclick={() => post({ type: "press", button: "replay" })}><Icon name="replay" /></button>
      <button type="button" aria-label="Next line" title="Next line" onclick={() => post({ type: "press", button: "next" })}><Icon name="nextLine" /></button>
    </nav>
  {:else if shows.watching.length}
    <h2>Carry on watching</h2>
    <ul class="watching">
      {#each shows.watching as w (w.episode)}
        <li>
          <button type="button" onclick={() => post({ type: "watch", episode: w.episode })}>
            <img src={w.cover} alt="" />
            <span class="w-text"><b>{w.title}</b><span>{w.label}</span></span>
            <Icon name="play" />
          </button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="hint">The lines of what you’re watching show here.</p>
  {/if}
</div>

<style>
  .second { position: absolute; inset: 0; display: flex; flex-direction: column; background: var(--vid); color: var(--vid-ink); user-select: none; -webkit-user-select: none; }
  header { flex: none; display: flex; align-items: center; gap: 10px; padding: 10px 8px 10px 14px; border-bottom: 1px solid var(--vid-edge); }
  .logo { flex: none; width: 24px; height: 24px; border-radius: 6px; display: grid; place-items: center; background: var(--vid-ink); color: var(--vid); font: 700 13px/1 var(--font-ui); }
  .what { flex: 1; min-width: 0; display: flex; align-items: baseline; gap: 8px; }
  .what b { font: 600 15px/1.2 var(--font-ui); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .what span { flex: none; font-size: 13px; color: var(--vid-ink-2); }
  .ibtn { flex: none; width: 40px; height: 40px; display: grid; place-items: center; border: 0; border-radius: 8px; background: none; color: var(--vid-ink-2); }
  .ibtn :global(svg) { width: 18px; height: 18px; }

  .lines { flex: 1; min-height: 0; overflow-y: auto; list-style: none; margin: 0; padding: 30vh 0; }
  .lines li { display: flex; align-items: flex-start; gap: 4px; padding: 0 12px 0 0; border-left: 3px solid transparent; }
  .lines li.cur { border-left-color: var(--mark); background: rgba(255, 255, 255, .07); }
  .time { flex: none; width: 58px; padding: 15px 0 12px; border: 0; background: none; color: var(--vid-ink-2); font: 500 12px/1.4 var(--font-mono); text-align: center; }
  .lines p { flex: 1; min-width: 0; margin: 0; padding: 10px 0; font: 600 20px/1.5 var(--font-jp); color: var(--vid-ink); }
  .lines li.said p { color: var(--vid-ink-2); }
  .lines li.cur p { font-size: 23px; color: #fff; }
  .lines p :global(rt), .looked :global(rt) { font-size: .46em; font-weight: 500; }
  .lines p :global(.nm), .looked :global(.nm) { color: var(--sub-name, #9fd0ff); }
  .speaker { margin-right: .6em; font-size: .6em; font-weight: 500; opacity: .7; }
  .none, .hint { padding: 24px 16px; color: var(--vid-ink-2); font-size: 15px; }
  .lines .none { border: 0; }

  .looked { flex: none; margin: 0; padding: 12px 16px; font: 700 24px/1.5 var(--font-jp); border-bottom: 1px solid var(--vid-edge); }
  /* --mark and --mark-ink, written out: an Android WebView before 111 reads no var() in a highlight. */
  :global(::highlight(telly-look)) { background-color: #ffd84a; color: #151516; }
  .means { flex: 1; min-height: 0; display: flex; flex-direction: column; padding: 8px; }

  .foot { flex: none; display: flex; justify-content: center; gap: 8px; padding: 8px 12px 10px; border-top: 1px solid var(--vid-edge); }
  .foot button { min-width: 64px; height: 48px; display: grid; place-items: center; border: 0; border-radius: 10px; background: rgba(255, 255, 255, .08); color: var(--vid-ink); font: 600 15px var(--font-ui); }
  .foot button:active, .watching button:active { background: rgba(255, 255, 255, .18); }
  .foot .big { min-width: 96px; }
  .foot .wide { flex: 1; }
  .foot :global(svg) { width: 22px; height: 22px; }

  h2 { margin: 18px 16px 8px; font: 600 13px/1.2 var(--font-ui); text-transform: uppercase; letter-spacing: .06em; color: var(--vid-ink-2); }
  .watching { flex: 1; min-height: 0; overflow-y: auto; list-style: none; margin: 0; padding: 0 12px 12px; display: flex; flex-direction: column; gap: 8px; }
  .watching button { width: 100%; display: flex; align-items: center; gap: 12px; padding: 8px 14px 8px 8px; border: 1px solid var(--vid-edge); border-radius: 12px; background: var(--vid-panel); color: var(--vid-ink); text-align: left; }
  .watching img { flex: none; width: 46px; height: 64px; object-fit: cover; border-radius: 6px; background: rgba(255, 255, 255, .08); }
  .w-text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .w-text b { font: 600 16px/1.25 var(--font-ui); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .w-text span { font-size: 13px; color: var(--vid-ink-2); }
  .watching :global(svg) { flex: none; width: 22px; height: 22px; }
</style>
