<script lang="ts">
  // The scene a line is said in, played over the Library: one of the lines a
  // search found, from the line before it to the line after (search.rs).
  import { goto } from "$app/navigation";
  import { onMount, untrack } from "svelte";
  import { call, errorText, mediaUrl, type FoundLine, type SceneView, type Settings, type ShowCard } from "./api";
  import { clock } from "./format";
  import Icon from "./Icon.svelte";
  import { pickAudio } from "./video";

  let {
    lines,
    at = $bindable(),
    showOf,
    settings,
    onclose,
  }: { lines: FoundLine[]; at: number; showOf: (line: FoundLine) => ShowCard | undefined; settings: Settings; onclose: () => void } = $props();

  const found = $derived(lines[at]);
  const show = $derived(found && showOf(found));
  let scene = $state.raw<SceneView | null>(null);
  let error = $state("");
  let video = $state<HTMLVideoElement | null>(null);
  let sheet = $state<HTMLElement | null>(null);
  let t = $state(0);
  let paused = $state(true);
  // As loud as the player was when the scene opened.
  let volume = $state(untrack(() => (typeof settings["player.volume"] === "number" ? (settings["player.volume"] as number) : 1)));
  let muted = $state(untrack(() => settings["player.muted"] === true));

  /** Goes up with each line asked for, so a slow answer for the last one is dropped. */
  let asked = 0;
  $effect(() => {
    const line = found;
    if (!line) return;
    const mine = ++asked;
    call<SceneView>("scene", { id: line.episodeId, line: line.line })
      .then((s) => {
        if (mine !== asked) return;
        // The same episode is there already: straight to the scene. Another one starts once it has loaded.
        const loaded = scene?.playable && scene.src === s.src && (video?.readyState ?? 0) >= 1;
        error = "";
        scene = s;
        t = s.start;
        if (loaded) start();
      })
      .catch((e) => {
        if (mine !== asked) return;
        scene = null;
        error = errorText(e);
      });
  });

  /** Plays the scene from its start. */
  function start() {
    if (!scene || !video) return;
    pickAudio(video, scene.listedAudio);
    video.currentTime = scene.start;
    video.play().catch(() => {});
  }

  /** Follows the video, and stops it at the scene's end. Called every frame, and on the video's
   *  time updates too, which carry on when the window is hidden and frames stop. */
  function follow() {
    if (!scene || !video || video.readyState < 1) return;
    t = video.currentTime;
    if (t >= scene.end && !video.paused) video.pause();
  }

  const over = $derived(!!scene && t >= scene.end);

  function toggle() {
    if (!scene || !video) return;
    if (over) start();
    else if (video.paused) video.play().catch(() => {});
    else video.pause();
  }

  function seek(to: number) {
    if (!video) return;
    video.currentTime = to;
    video.play().catch(() => {});
  }

  function step(by: number) {
    if (lines[at + by]) at += by;
  }

  function watch() {
    if (scene) goto(`/watch?ep=${scene.episodeId}&at=${Math.floor(scene.start * 10) / 10}`);
  }

  onMount(() => {
    // Back to the line that opened it, when it closes.
    const opener = document.activeElement as HTMLElement | null;
    sheet?.focus();
    const key = (e: KeyboardEvent) => {
      // What's typed in the title bar's search is the search's.
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === "Escape") onclose();
      else if (e.key === "ArrowLeft") step(-1);
      else if (e.key === "ArrowRight") step(1);
      else if (e.key === " ") toggle();
      else if (e.key === "r" || e.key === "R") start();
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", key);
    let frame = requestAnimationFrame(function each() {
      follow();
      frame = requestAnimationFrame(each);
    });
    return () => {
      window.removeEventListener("keydown", key);
      cancelAnimationFrame(frame);
      opener?.focus();
    };
  });

  const label = $derived(found && (show?.movie ? "" : `Episode ${found.number}`));
  const showing = $derived((scene?.lines ?? []).map((l, i) => ({ ...l, i })).filter((l) => l.start <= t && t < l.end));
  const played = $derived(scene ? Math.min(1, Math.max(0, (t - scene.start) / (scene.end - scene.start))) : 0);
</script>

<div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="scene" role="dialog" aria-label="Scene" aria-modal="true" tabindex="-1" bind:this={sheet}>
    <div class="sc-head">
      <span class="sc-title" lang={show?.titleNative ? "ja" : undefined} title={show?.titleNative || show?.title}>{show?.titleNative || show?.title || ""}</span>
      {#if found}<span class="sc-where">{label ? `${label} · ` : ""}{clock(found.start)}</span>{/if}
      <span class="grow"></span>
      <button type="button" class="pbtn" aria-label="Close" title="Close (Esc)" onclick={onclose}><Icon name="x" /></button>
    </div>

    <div class="sc-video">
      {#if scene?.playable}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video
          bind:this={video}
          src={mediaUrl(scene.src)}
          preload="auto"
          bind:volume
          bind:muted
          bind:paused
          onloadedmetadata={start}
          ontimeupdate={follow}
          onerror={() => (error = "The video couldn’t be played. Open the show to check the episode.")}
          onclick={toggle}
        ></video>
      {/if}
      {#if error}
        <p class="sc-note">{error}</p>
      {:else if scene && !scene.playable}
        <p class="sc-note">{label ? `Getting ${label} ready` : "Getting ready"}</p>
      {/if}
      <div class="sc-subs" aria-live="off">
        {#each showing as l (l.i)}
          <p class="sub-line" lang="ja">{#if scene && found && l.i === scene.line}{found.before}<mark>{found.word}</mark>{found.after}{:else}{l.text}{/if}</p>
        {/each}
      </div>
    </div>

    <div class="sc-seek">
      <span class="sc-clock">{clock(t)}</span>
      <span class="track"><i style:width="{played * 100}%"></i></span>
      <span class="sc-clock">{clock(scene?.end ?? 0)}</span>
    </div>

    <div class="sc-ctl">
      <button type="button" class="pbtn" aria-label={paused ? "Play" : "Pause"} title="{paused ? "Play" : "Pause"} (Space)" disabled={!scene?.playable} onclick={toggle}><Icon name={paused ? "play" : "pause"} /></button>
      <button type="button" class="pbtn" aria-label="Replay" title="Replay (R)" disabled={!scene?.playable} onclick={start}><Icon name="replay" /></button>
      <span class="sep"></span>
      <button type="button" class="pbtn" aria-label="Show previous scene" title="Show previous scene (←)" disabled={at <= 0} onclick={() => step(-1)}><Icon name="back" /></button>
      <span class="sc-pos">{at + 1} of {lines.length}</span>
      <button type="button" class="pbtn flip" aria-label="Show next scene" title="Show next scene (→)" disabled={at >= lines.length - 1} onclick={() => step(1)}><Icon name="back" /></button>
      <span class="grow"></span>
      <button type="button" class="btn" disabled={!scene?.playable} onclick={watch}>Watch from here</button>
    </div>

    {#if scene}
      <ol class="sc-lines">
        {#each scene.lines as l, i (i)}
          <li>
            <button type="button" class="sc-line" class:on={l.start <= t && t < l.end} disabled={!scene.playable} onclick={() => seek(l.start)}>
              <span class="mono">{clock(l.start)}</span>
              <span lang="ja">{#if found && i === scene.line}{found.before}<mark>{found.word}</mark>{found.after}{:else}{l.text}{/if}</span>
            </button>
          </li>
        {/each}
      </ol>
    {/if}
  </div>
</div>

<style>
  /* The player is always dark, and so is a scene. */
  .scene { width: min(880px, 100%); max-height: 100%; overflow: auto; background: var(--vid-panel); border: 1px solid var(--vid-edge); border-radius: 12px; color: var(--vid-ink); box-shadow: var(--shadow-lg); scrollbar-width: thin; }
  .scene:focus { outline: none; }
  .scene :focus-visible { outline-color: var(--vid-ink); }
  .sc-head { display: flex; align-items: center; gap: 12px; padding: 8px 8px 8px 18px; }
  .sc-title { font: 800 16px/1.3 var(--font-jp-title); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .sc-where { font-size: 13px; color: var(--vid-ink-2); white-space: nowrap; font-variant-numeric: tabular-nums; }
  .pbtn { display: grid; place-items: center; width: 38px; height: 38px; padding: 0; border: 0; border-radius: 6px; background: transparent; color: var(--vid-ink); cursor: pointer; flex: none; }
  .pbtn:hover:not(:disabled) { background: var(--vid-hover); }
  .pbtn:disabled { opacity: .35; cursor: default; }
  .pbtn :global(svg) { width: 20px; height: 20px; }
  .pbtn.flip :global(svg) { transform: scaleX(-1); }
  /* As tall as leaves room for the rest of the sheet in a small window. */
  .sc-video { position: relative; aspect-ratio: 16 / 9; max-height: calc(100vh - 340px); min-height: 160px; margin: 0 auto; background: var(--vid); }
  .sc-video video { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: contain; cursor: pointer; }
  .sc-note { position: absolute; inset: 0; display: grid; place-items: center; margin: 0; padding: 0 24px; text-align: center; font-size: 14px; color: var(--vid-ink-2); }
  .sc-subs { position: absolute; left: 0; right: 0; bottom: 7%; display: flex; flex-direction: column; align-items: center; gap: 2px; pointer-events: none; }
  .sub-line { max-width: 86%; margin: 0; padding: 2px 14px 4px; font: 700 28px/1.42 var(--font-jp); color: var(--sub-ink); text-align: center; letter-spacing: .03em; text-shadow: 1px 1px 0 var(--sub-edge), -1px 1px 0 var(--sub-edge), 1px -1px 0 var(--sub-edge), -1px -1px 0 var(--sub-edge), 0 2px 0 var(--sub-edge), 0 -2px 0 var(--sub-edge), 2px 0 0 var(--sub-edge), -2px 0 0 var(--sub-edge), 0 3px 10px rgba(0, 0, 0, .55); }
  .sub-line mark { background: none; color: var(--mark); }
  .sc-seek { display: flex; align-items: center; gap: 12px; padding: 12px 18px 2px; }
  .sc-clock { font: 500 12px/1 var(--font-mono); font-variant-numeric: tabular-nums; color: var(--vid-ink-2); }
  .track { flex: 1; height: 3px; border-radius: 2px; background: rgba(255, 255, 255, .22); overflow: hidden; }
  .track i { display: block; height: 100%; background: var(--vid-ink); }
  .sc-ctl { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; padding: 6px 12px 12px; }
  .sc-pos { min-width: 62px; padding: 0 4px; text-align: center; font: 500 13px/1 var(--font-ui); color: var(--vid-ink-2); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .sep { width: 1px; height: 22px; margin: 0 6px; background: var(--vid-edge); }
  .sc-ctl .btn { margin-right: 6px; color: var(--vid); background: var(--vid-ink); border-color: var(--vid-ink); }
  .sc-ctl .btn:hover:not(:disabled) { background: #fff; }
  .sc-lines { list-style: none; display: grid; gap: 1px; margin: 0; padding: 8px 8px 10px; border-top: 1px solid var(--vid-edge); }
  .sc-line { all: unset; box-sizing: border-box; display: grid; grid-template-columns: 64px minmax(0, 1fr); gap: 10px; align-items: baseline; width: 100%; padding: 7px 10px; border-radius: 6px; color: var(--vid-ink-2); cursor: pointer; }
  .sc-line:hover:not(:disabled) { background: var(--vid-hover); }
  .sc-line:disabled { cursor: default; }
  .sc-line:focus-visible { outline: 2px solid var(--vid-ink); outline-offset: -2px; }
  .sc-line.on { color: var(--vid-ink); }
  .sc-line .mono { font: 500 12px/1 var(--font-mono); font-variant-numeric: tabular-nums; }
  .sc-line span[lang] { font: 500 15px/1.45 var(--font-jp); }
  .sc-line mark { font-weight: 700; color: inherit; background: linear-gradient(transparent 58%, rgba(255, 216, 74, .32) 58%); }
</style>
