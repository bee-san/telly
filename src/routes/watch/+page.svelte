<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onDestroy, onMount, tick as nextTick } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, errorText, mediaUrl, setFullscreen, setWindowTitle, type CardEvent, type Line, type PlayerView } from "$lib/api";
  import { clock } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  const epId = $derived(Number(page.url.searchParams.get("ep")));

  let view = $state<PlayerView | null>(null);
  let error = $state("");
  let video = $state<HTMLVideoElement | null>(null);
  let txList = $state<HTMLOListElement | null>(null);
  let now = $state(0);
  let duration = $state(0);
  let paused = $state(true);
  let rate = $state(1);
  let subsOn = $state(true);
  let blur = $state(false);
  let transcript = $state(false);
  let idle = $state(false);
  let nudge = $state(0);
  let pausedByHover = $state(false);
  let popupOpen = $state(false);
  let ended = $state(false);
  let countdown = $state<number | null>(null);
  let note = $state("");
  let subMenu = $state(false);
  let videoError = $state("");
  let settings = $state<Record<string, unknown>>({});

  // This sitting, for the end card.
  let sat = $state({ lines: 0, chars: 0, cards: 0, seconds: 0 });

  let resumeTimer: ReturnType<typeof setTimeout> | null = null;
  let idleTimer: ReturnType<typeof setTimeout> | null = null;
  let noteTimer: ReturnType<typeof setTimeout> | null = null;
  let tickTimer: ReturnType<typeof setInterval> | null = null;
  let countTimer: ReturnType<typeof setInterval> | null = null;
  let frame = 0;
  let lastEvent = 0;
  let lastT = 0;
  let endingFired = false;
  let pendingLines: { chars: number }[] = [];
  const counted = new Set<number>();
  let closed = false;

  const lines = $derived(view?.lines ?? []);
  const dialogue = $derived(lines.filter((l) => !l.sign));
  const t = $derived(now - nudge / 1000);
  const active = $derived(dialogue.filter((l) => l.start <= t && t < l.end));
  /** The last line that has started: what ← → and ↓ move from. */
  const current = $derived.by(() => {
    let found = -1;
    for (let i = 0; i < dialogue.length; i++) {
      if (dialogue[i].start <= t + 0.05) found = i;
      else break;
    }
    return found;
  });
  const autoplay = $derived(settings["player.autoplay"] !== false);
  const skipEndings = $derived(settings["player.skipEndings"] === true);
  const subSize = $derived(Number(settings["player.subtitleSize"] ?? 36) || 36);

  function esc(s: string): string {
    return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c] as string);
  }

  /** A line as HTML: furigana as ruby, breaks as <br>. */
  function lineHtml(l: Line): string {
    const chars = [...l.text];
    const ruby = new Map<number, [number, string]>((l.ruby ?? []).map(([at, len, r]) => [at, [len, r]]));
    let out = "";
    for (let i = 0; i < chars.length; ) {
      const r = ruby.get(i);
      if (r && r[0] > 0) {
        out += `<ruby>${esc(chars.slice(i, i + r[0]).join(""))}<rt>${esc(r[1])}</rt></ruby>`;
        i += r[0];
        continue;
      }
      out += chars[i] === "\n" ? "<br>" : esc(chars[i]);
      i++;
    }
    return out;
  }

  function countChars(text: string): number {
    let n = 0;
    for (const c of text) if (/[\p{L}\p{N}]/u.test(c)) n++;
    return n;
  }

  function flash(text: string) {
    note = text;
    if (noteTimer) clearTimeout(noteTimer);
    noteTimer = setTimeout(() => (note = ""), 1600);
  }

  async function load(id: number) {
    error = "";
    videoError = "";
    ended = false;
    countdown = null;
    endingFired = false;
    counted.clear();
    sat = { lines: 0, chars: 0, cards: 0, seconds: 0 };
    try {
      settings = await call<Record<string, unknown>>("settings");
      const v = await call<PlayerView>("openPlayer", { id });
      view = v;
      nudge = v.episode.subsNudgeMs;
      subsOn = settings["player.subtitles"] !== false;
      blur = settings["player.blur"] === true;
      await setWindowTitle(`${v.show.title} · Episode ${v.episode.number} — Telly`);
      if (v.episode.mediaState !== "ready") {
        error = v.episode.mediaState === "failed" ? `This episode couldn't be converted: ${v.episode.mediaError}` : "This episode isn't ready to play yet.";
      }
    } catch (e) {
      error = errorText(e);
    }
  }

  function startAt(): number {
    const e = view?.episode;
    if (!e) return 0;
    if (e.watchedAt === null && e.position > 5 && (e.duration <= 0 || e.position < e.duration * 0.95)) return e.position;
    return 0;
  }

  function onLoaded() {
    if (!video) return;
    duration = video.duration || view?.episode.duration || 0;
    const at = startAt();
    if (at > 0) video.currentTime = at;
    lastT = video.currentTime;
    video.playbackRate = rate;
    video.play().catch(() => {});
  }

  // ---- the frame loop: subtitles and the lines that played -------------------

  function loop() {
    frame = requestAnimationFrame(loop);
    if (!video) return;
    const cur = video.currentTime;
    now = cur;
    const d = cur - lastT;
    if (d > 0 && d < 1.5 && video.playbackRate >= 0.5 && video.playbackRate <= 2) {
      const from = lastT - nudge / 1000;
      const to = cur - nudge / 1000;
      for (let i = 0; i < dialogue.length; i++) {
        const l = dialogue[i];
        if (l.start > to) break;
        const mid = (l.start + l.end) / 2;
        if (mid > from && mid <= to && !counted.has(i)) {
          counted.add(i);
          const chars = countChars(l.text);
          pendingLines.push({ chars });
          sat.lines += 1;
          sat.chars += chars;
        }
      }
      if (!video.paused) sat.seconds += d / video.playbackRate;
    }
    lastT = cur;
    if (skipEndings && !endingFired && view?.episode.endingAt && duration > 0 && view.episode.endingAt > duration * 0.5 && cur >= view.episode.endingAt) {
      endingFired = true;
      finish();
    }
  }

  // ---- talking to the backend every second -------------------------------------

  async function sendTick() {
    if (!view || !video || closed) return;
    const batch = pendingLines.splice(0);
    try {
      const r = await call<{ events: CardEvent[]; watched: boolean }>("tick", {
        id: view.episode.id,
        position: video.currentTime,
        playing: !video.paused,
        rate: video.playbackRate,
        subtitles: subsOn && dialogue.length > 0,
        lines: batch,
        since: lastEvent,
      });
      for (const ev of r.events) handleEvent(ev);
    } catch {
      pendingLines.unshift(...batch);
    }
  }

  function handleEvent(ev: CardEvent) {
    lastEvent = Math.max(lastEvent, ev.id);
    const key = ev.jobId ? `card-${ev.jobId}` : `card-n${ev.noteId}-${ev.id}`;
    if (ev.state === "done") sat.cards += 1;
    toast({
      key,
      state: ev.state,
      title: ev.title,
      text: ev.text,
      ja: true,
      action: ev.state === "failed" && ev.jobId ? { label: "Retry", run: () => call("retryCard", { job: ev.jobId }) } : undefined,
    });
  }

  // ---- pause while you read; resume back on the picture ---------------------------

  function clearResume() {
    if (resumeTimer) clearTimeout(resumeTimer);
    resumeTimer = null;
  }

  function hoverText() {
    clearResume();
    if (video && !video.paused) {
      video.pause();
      pausedByHover = true;
    }
  }

  function picMove() {
    wake();
    if (!pausedByHover || popupOpen || resumeTimer) return;
    resumeTimer = setTimeout(() => {
      resumeTimer = null;
      if (pausedByHover && !popupOpen && video) {
        pausedByHover = false;
        video.play().catch(() => {});
      }
    }, 300);
  }

  function togglePlay() {
    if (!video) return;
    pausedByHover = false;
    clearResume();
    if (video.paused) video.play().catch(() => {});
    else video.pause();
  }

  // ---- lines ---------------------------------------------------------------------

  function seekTo(seconds: number) {
    if (!video) return;
    video.currentTime = Math.max(0, Math.min(seconds, (duration || video.duration || 0) - 0.1));
    lastT = video.currentTime;
    now = video.currentTime;
  }

  function lineStart(l: Line) {
    return l.start + nudge / 1000 + 0.02;
  }

  function prevLine() {
    if (!dialogue.length) return seekTo(now - 5);
    const i = current;
    if (i >= 0 && t - dialogue[i].start > 1.0) seekTo(lineStart(dialogue[i]));
    else if (i > 0) seekTo(lineStart(dialogue[i - 1]));
    else seekTo(0);
  }

  function nextLine() {
    if (!dialogue.length) return seekTo(now + 5);
    const n = dialogue[current + 1];
    if (n) seekTo(lineStart(n));
  }

  function replayLine() {
    const l = dialogue[current];
    if (!l || !video) return;
    seekTo(lineStart(l));
    pausedByHover = false;
    video.play().catch(() => {});
  }

  async function setNudge(ms: number) {
    nudge = ms;
    flash(ms === 0 ? "Subtitles as synced" : `Subtitles ${(Math.abs(ms) / 1000).toFixed(1)} s ${ms > 0 ? "later" : "earlier"}`);
    if (view) await call("nudge", { id: view.episode.id, ms }).catch(() => {});
  }

  async function lateFromHere(ms: number) {
    if (!view) return;
    subMenu = false;
    try {
      const updated = await call<Line[]>("lateFromHere", { id: view.episode.id, from: t - 1, ms });
      view.lines = updated;
      flash(`Lines from here ${(Math.abs(ms) / 1000).toFixed(1)} s ${ms > 0 ? "later" : "earlier"}`);
    } catch (e) {
      flash(errorText(e));
    }
  }

  async function useFile(path: string, name: string) {
    if (!view) return;
    subMenu = false;
    toast({ key: "resync", state: "working", title: "Syncing another file", text: name });
    try {
      const v = await call<PlayerView>("useSubtitle", { id: view.episode.id, path, name });
      view.lines = v.lines;
      view.episode = v.episode;
      toast({ key: "resync", state: "done", title: "Subtitles changed", text: `${name} · ${Math.round((v.episode.subsScore ?? 0) * 100)}%` });
    } catch (e) {
      toast({ key: "resync", state: "failed", title: "Couldn't use that file", text: errorText(e) });
    }
  }

  async function mineLine() {
    const l = active[0] ?? dialogue[current];
    if (!l || !view) return;
    try {
      await call("mineLine", { id: view.episode.id, start: l.start, end: l.end, text: l.text });
    } catch (e) {
      toast({ key: "mineline", state: "failed", title: "Couldn't add the line", text: errorText(e) });
    }
  }

  function setRate(r: number) {
    rate = Math.round(Math.min(2, Math.max(0.5, r)) * 10) / 10;
    if (video) video.playbackRate = rate;
    flash(`${rate.toFixed(1)}×`);
  }

  async function toggleFull() {
    app.fullscreen = !app.fullscreen;
    await setFullscreen(app.fullscreen).catch(() => {});
  }

  // ---- the end of an episode -----------------------------------------------------

  function finish() {
    ended = true;
    if (!view?.next || !autoplay) return;
    waitForNext();
  }

  async function waitForNext() {
    if (!view?.next || !ended) return;
    if (!view.next.ready) {
      try {
        const v = await call<PlayerView>("episode", { id: view.episode.id });
        if (view && v.next) view.next = v.next;
      } catch {}
      if (!view?.next?.ready) {
        setTimeout(waitForNext, 2000);
        return;
      }
    }
    countdown = 8;
    if (countTimer) clearInterval(countTimer);
    countTimer = setInterval(() => {
      if (countdown === null) return;
      countdown -= 1;
      if (countdown <= 0) playNext();
    }, 1000);
  }

  function stopCountdown() {
    countdown = null;
    if (countTimer) clearInterval(countTimer);
    countTimer = null;
  }

  async function playNext() {
    stopCountdown();
    const next = view?.next;
    if (!next) return;
    await close();
    closed = false;
    goto(`/watch?ep=${next.id}`, { replaceState: true });
  }

  async function close() {
    if (!view || closed) return;
    closed = true;
    await sendTick();
    await call("closePlayer", { id: view.episode.id, position: video?.currentTime ?? null }).catch(() => {});
  }

  async function leave(to = "/") {
    stopCountdown();
    await close();
    if (app.fullscreen) await toggleFull();
    goto(to);
  }

  // ---- keys and the idle chrome ----------------------------------------------------

  function wake() {
    idle = false;
    if (idleTimer) clearTimeout(idleTimer);
    idleTimer = setTimeout(() => {
      if (video && !video.paused && !subMenu) idle = true;
    }, 2500);
  }

  function onKey(e: KeyboardEvent) {
    const target = e.target as HTMLElement | null;
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    wake();
    switch (e.key) {
      case " ":
      case "k":
        e.preventDefault();
        togglePlay();
        break;
      case "ArrowLeft":
        e.preventDefault();
        prevLine();
        break;
      case "ArrowRight":
        e.preventDefault();
        nextLine();
        break;
      case "ArrowDown":
        e.preventDefault();
        replayLine();
        break;
      case "s":
      case "S":
        subsOn = !subsOn;
        flash(subsOn ? "Subtitles on" : "Subtitles off");
        call("setSetting", { key: "player.subtitles", value: subsOn }).catch(() => {});
        break;
      case "b":
      case "B":
        blur = !blur;
        flash(blur ? "Blurred until you hover" : "Blur off");
        call("setSetting", { key: "player.blur", value: blur }).catch(() => {});
        break;
      case "t":
      case "T":
        transcript = !transcript;
        break;
      case "[":
        setRate(rate - 0.1);
        break;
      case "]":
        setRate(rate + 0.1);
        break;
      case "f":
      case "F":
        toggleFull();
        break;
      case "z":
      case "Z":
        setNudge(nudge - 100);
        break;
      case "x":
      case "X":
        setNudge(nudge + 100);
        break;
      case "m":
      case "M":
        mineLine();
        break;
      case "Escape":
        if (countdown !== null) stopCountdown();
        else if (subMenu) subMenu = false;
        else if (app.fullscreen) toggleFull();
        break;
    }
  }

  // Keep the transcript on the line playing.
  $effect(() => {
    if (!transcript || !txList || current < 0) return;
    const el = txList.querySelector<HTMLElement>(`[data-i="${current}"]`);
    el?.scrollIntoView({ block: "nearest" });
  });

  // A new episode (autoplay, or the episode buttons) reloads in place.
  let loadedId = 0;
  $effect(() => {
    const id = epId;
    if (id && id !== loadedId) {
      loadedId = id;
      closed = false;
      load(id);
    }
  });

  const popupShown = () => {
    popupOpen = true;
    clearResume();
  };
  const popupHidden = () => (popupOpen = false);

  onMount(() => {
    app.dark = true;
    frame = requestAnimationFrame(loop);
    tickTimer = setInterval(sendTick, 1000);
    window.addEventListener("keydown", onKey);
    window.addEventListener("hachidori-popup-shown", popupShown);
    window.addEventListener("hachidori-popup-hidden", popupHidden);
    const unload = () => {
      if (view && video) call("closePlayer", { id: view.episode.id, position: video.currentTime }).catch(() => {});
    };
    window.addEventListener("beforeunload", unload);
    wake();
    return () => window.removeEventListener("beforeunload", unload);
  });

  onDestroy(() => {
    cancelAnimationFrame(frame);
    if (tickTimer) clearInterval(tickTimer);
    stopCountdown();
    clearResume();
    window.removeEventListener("keydown", onKey);
    window.removeEventListener("hachidori-popup-shown", popupShown);
    window.removeEventListener("hachidori-popup-hidden", popupHidden);
    close();
    app.dark = false;
    if (app.fullscreen) {
      app.fullscreen = false;
      setFullscreen(false).catch(() => {});
    }
  });

  const syncNote = $derived.by(() => {
    const e = view?.episode;
    if (!e) return "";
    if (e.subsState === "synced" && e.subsFile === "Whisper") return "Subtitles made by Whisper";
    if (e.subsState === "synced") return `Subtitles synced · ${Math.round((e.subsScore ?? 0) * 100)}%`;
    if (e.subsState === "check") return `Subtitles need a look · ${Math.round((e.subsScore ?? 0) * 100)}%`;
    if (e.subsState === "pending" || e.subsState === "working") return "Finding subtitles…";
    return e.subsDetail || "No subtitles";
  });

  function seekFromPointer(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    const r = el.getBoundingClientRect();
    seekTo(((e.clientX - r.left) / r.width) * (duration || 0));
  }
</script>

<div class="player" class:idle class:paused onpointermove={wake} role="application" aria-label="Player">
  <div class="pic" onpointermove={picMove} onpointerleave={clearResume} onclick={togglePlay} ondblclick={toggleFull} role="presentation">
    {#if view && view.episode.mediaState === "ready"}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video
        bind:this={video}
        src={mediaUrl(view.src)}
        preload="auto"
        onloadedmetadata={onLoaded}
        onplay={() => { paused = false; wake(); }}
        onpause={() => (paused = true)}
        onseeked={() => video && (lastT = video.currentTime)}
        onended={finish}
        ondurationchange={() => video && (duration = video.duration)}
        onerror={() => (videoError = video?.error?.message || "The video couldn't play.")}
      ></video>
    {/if}
  </div>

  <div class="subs" class:off={!subsOn} style:--sub-size="{subSize}px">
    {#each active as line (line.start + line.text)}
      <p class="sub-line" class:blur lang="ja" onpointerenter={hoverText}>
        {#if line.speaker}<span class="speaker">{line.speaker}</span>{/if}{@html lineHtml(line)}
      </p>
    {/each}
  </div>

  {#if pausedByHover}<span class="paused-note">Paused while you read · move back to the picture to carry on</span>{/if}
  {#if note}<span class="flash">{note}</span>{/if}

  <div class="ptop">
    <button type="button" class="pbtn" aria-label="Back to the library" onclick={() => leave("/")}><Icon name="back" /></button>
    {#if view}
      <span class="ptitle"><b>{view.show.title}</b><span>Episode {view.episode.number} of {view.count}</span></span>
      <span class="grow"></span>
      <span class="subnote" title={view.episode.subsFile}>
        {#if view.episode.subsState === "synced"}<Icon name="check" />{/if}{syncNote}{#if nudge !== 0} · {nudge > 0 ? "+" : ""}{(nudge / 1000).toFixed(1)} s{/if}
      </span>
      <button type="button" class="pbtn txt" onclick={() => leave(`/show?id=${view?.show.id}`)}>Episodes</button>
    {/if}
  </div>

  {#if error || videoError}
    <div class="perr">
      <p>{error || videoError}</p>
      <div class="end-actions">
        {#if videoError && view}
          <button type="button" class="btn" onclick={async () => { if (view) { await call("retryMedia", { id: view.episode.id, force: true }); leave(`/show?id=${view.show.id}`); } }}>Convert this episode</button>
        {/if}
        <button type="button" class="btn quiet" onclick={() => leave(view ? `/show?id=${view.show.id}` : "/")}>Back</button>
      </div>
    </div>
  {/if}

  <div class="pctl">
    <div class="seek" role="slider" tabindex="-1" aria-label="Seek" aria-valuemin={0} aria-valuemax={duration} aria-valuenow={now}
      onpointerdown={(e) => { (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId); seekFromPointer(e); }}
      onpointermove={(e) => e.buttons === 1 && seekFromPointer(e)}>
      <span class="track"><i style:width="{duration ? (100 * now) / duration : 0}%"></i></span>
      <span class="knob" style:left="{duration ? (100 * now) / duration : 0}%"></span>
    </div>
    <div class="prow">
      <button type="button" class="pbtn big" aria-label={paused ? "Play" : "Pause"} onclick={togglePlay}><Icon name={paused ? "play" : "pause"} /></button>
      <button type="button" class="pbtn" aria-label="Previous line" title="Previous line (←)" onclick={prevLine}><Icon name="prevLine" /></button>
      <button type="button" class="pbtn" aria-label="Replay the line" title="Replay the line (↓)" onclick={replayLine}><Icon name="replay" /></button>
      <button type="button" class="pbtn" aria-label="Next line" title="Next line (→)" onclick={nextLine}><Icon name="nextLine" /></button>
      <span class="ptime">{clock(now)} / {clock(duration)}</span>
      <span class="grow"></span>
      {#if rate !== 1}<button type="button" class="pbtn txt" title="Speed ([ and ])" onclick={() => setRate(1)}>{rate.toFixed(1)}×</button>{/if}
      <button type="button" class="pbtn" class:on={subsOn} aria-label="Subtitles" title="Subtitles (S)" onclick={() => (subsOn = !subsOn)}><Icon name="subs" /></button>
      <button type="button" class="pbtn" class:on={blur} aria-label="Blur until hover" title="Blur until you hover (B)" onclick={() => (blur = !blur)}><Icon name="blur" /></button>
      <button type="button" class="pbtn" class:on={transcript} aria-label="Transcript" title="Transcript (T)" onclick={() => (transcript = !transcript)}><Icon name="transcript" /></button>
      <span class="menuwrap">
        <button type="button" class="pbtn txt" class:on={subMenu} onclick={() => (subMenu = !subMenu)}>Sync</button>
        {#if subMenu && view}
          <div class="submenu" role="menu">
            <span class="sm-h">Nudge every line (Z and X)</span>
            <div class="sm-row">
              <button type="button" onclick={() => setNudge(nudge - 100)}>−0.1 s</button>
              <button type="button" onclick={() => setNudge(0)}>Reset</button>
              <button type="button" onclick={() => setNudge(nudge + 100)}>+0.1 s</button>
            </div>
            <span class="sm-h">Lines from here on</span>
            <div class="sm-row">
              <button type="button" onclick={() => lateFromHere(-500)}>0.5 s earlier</button>
              <button type="button" onclick={() => lateFromHere(500)}>0.5 s later</button>
            </div>
            {#if view.tried.length}
              <span class="sm-h">Try another file</span>
              {#each view.tried as f (f.path)}
                <button type="button" class="sm-file" class:on={f.name === view.episode.subsFile} onclick={() => useFile(f.path, f.name)} disabled={!!f.error}>
                  <span>{f.name}</span><small>{f.error ? "unreadable" : `${Math.round(f.score * 100)}%`}</small>
                </button>
              {/each}
            {/if}
          </div>
        {/if}
      </span>
      <button type="button" class="pbtn" aria-label="Fullscreen" title="Fullscreen (F)" onclick={toggleFull}><Icon name={app.fullscreen ? "exitFull" : "full"} /></button>
    </div>
  </div>

  {#if transcript}
    <aside class="tx">
      <div class="tx-head"><b>Transcript</b><span>{dialogue.length} lines</span></div>
      <ol class="tx-list scroll" bind:this={txList}>
        {#each dialogue as l, i (i)}
          <li data-i={i} class:cur={i === current}>
            <button type="button" class="tt" onclick={() => seekTo(lineStart(l))}>{clock(l.start)}</button>
            <p class="ln" lang="ja" onpointerenter={hoverText}>{@html lineHtml(l)}</p>
          </li>
        {:else}
          <li class="tx-empty">No subtitles for this episode yet.</li>
        {/each}
      </ol>
    </aside>
  {/if}

  {#if ended && view}
    <div class="endov">
      <div class="end-card">
        <span class="small">Episode {view.episode.number} done{sat.seconds > 30 ? ` · logged to Tidepool` : ""}</span>
        <h3>{view.next ? `Next: Episode ${view.next.number}` : `That's the last episode here`}</h3>
        <div class="end-stats">
          <div><b>{Math.round(sat.seconds / 60)}</b><span>minutes</span></div>
          <div><b>{sat.lines}</b><span>lines</span></div>
          <div><b>{sat.chars}</b><span>characters</span></div>
          <div><b>{sat.cards}</b><span>cards</span></div>
        </div>
        {#if view.next && !view.next.ready}
          <p class="small">Episode {view.next.number} is getting ready{view.next.mediaState === "working" ? ` · ${Math.round(view.next.mediaProgress * 100)}%` : ""}. It'll start when it can.</p>
        {/if}
        <div class="end-actions">
          <button type="button" class="btn quiet" onclick={() => leave("/")}>Library</button>
          {#if view.next}
            <button type="button" class="btn quiet" onclick={stopCountdown} disabled={countdown === null}>Stop</button>
            <button type="button" class="btn" onclick={playNext} disabled={!view.next.ready}><Icon name="play" /><span>Play now</span></button>
          {/if}
        </div>
        {#if countdown !== null}
          <span class="cd" aria-label="Next episode in {countdown} seconds">
            <svg viewBox="0 0 44 44"><circle class="cd-bg" cx="22" cy="22" r="18" /><circle class="cd-fg" cx="22" cy="22" r="18" /></svg>
            <b>{countdown}</b>
          </span>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .player { position: absolute; inset: 0; background: var(--vid); color: var(--vid-ink); overflow: hidden; user-select: none; -webkit-user-select: none; }
  .player.idle { cursor: none; }
  .pic { position: absolute; inset: 0; display: grid; place-items: center; cursor: pointer; }
  .pic video { width: 100%; height: 100%; object-fit: contain; display: block; background: var(--vid); }
  .subs { position: absolute; left: 0; right: 0; bottom: 96px; display: flex; flex-direction: column; align-items: center; gap: 2px; pointer-events: none; z-index: 5; }
  .player.idle .subs { bottom: 56px; }
  .sub-line { pointer-events: auto; max-width: 86%; margin: 0; padding: 2px 14px 4px; border-radius: 6px; font: 700 var(--sub-size, 36px)/1.42 var(--font-jp); color: var(--sub-ink); text-align: center; letter-spacing: .03em; user-select: text; -webkit-user-select: text; cursor: text; text-shadow: 1px 1px 0 var(--sub-edge), -1px 1px 0 var(--sub-edge), 1px -1px 0 var(--sub-edge), -1px -1px 0 var(--sub-edge), 0 2px 0 var(--sub-edge), 0 -2px 0 var(--sub-edge), 2px 0 0 var(--sub-edge), -2px 0 0 var(--sub-edge), 0 3px 10px rgba(0, 0, 0, .55); transition: filter .2s; }
  .sub-line :global(rt) { font-size: .42em; font-weight: 500; letter-spacing: 0; }
  .sub-line.blur { filter: blur(9px); }
  .sub-line.blur:hover { filter: none; }
  .subs.off .sub-line { visibility: hidden; }
  .speaker { font-size: .5em; font-weight: 500; opacity: .75; margin-right: .5em; vertical-align: middle; }
  .paused-note { position: absolute; top: 62px; left: 20px; z-index: 6; font: 500 13px/1 var(--font-ui); color: var(--vid-ink-2); text-shadow: 0 1px 3px rgba(0, 0, 0, .9); pointer-events: none; }
  .flash { position: absolute; top: 62px; left: 50%; transform: translateX(-50%); z-index: 8; font: 600 14px/1 var(--font-ui); background: var(--vid-panel); border: 1px solid var(--vid-edge); padding: 9px 14px; border-radius: 8px; pointer-events: none; }
  .ptop { position: absolute; left: 0; right: 0; top: 0; z-index: 6; display: flex; align-items: center; gap: 10px; padding: 10px 14px 34px; background: linear-gradient(180deg, rgba(0, 0, 0, .6), transparent); transition: opacity .25s; }
  .ptitle { display: flex; align-items: baseline; gap: 10px; min-width: 0; }
  .ptitle b { font: 600 15px/1.2 var(--font-ui); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .ptitle span { font-size: 13px; color: var(--vid-ink-2); white-space: nowrap; }
  .subnote { display: inline-flex; align-items: center; gap: 6px; font: 500 12.5px/1 var(--font-ui); color: var(--vid-ink-2); margin-right: 4px; white-space: nowrap; }
  .subnote :global(svg) { width: 13px; height: 13px; color: var(--vid-ok); }
  .pctl { position: absolute; left: 0; right: 0; bottom: 0; z-index: 6; padding: 46px 16px 8px; background: linear-gradient(180deg, transparent, rgba(0, 0, 0, .75)); transition: opacity .25s; }
  .player.idle .ptop, .player.idle .pctl { opacity: 0; pointer-events: none; }
  .seek { position: relative; height: 22px; cursor: pointer; margin: 0 6px; touch-action: none; }
  .seek .track { position: absolute; left: 0; right: 0; top: 10px; height: 3px; border-radius: 2px; background: rgba(255, 255, 255, .22); overflow: hidden; }
  .seek:hover .track { height: 5px; top: 9px; }
  .seek .track i { display: block; height: 100%; background: var(--vid-ink); }
  .seek .knob { position: absolute; top: 6px; width: 11px; height: 11px; margin-left: -5.5px; border-radius: 50%; background: var(--vid-ink); }
  .prow { display: flex; align-items: center; gap: 2px; margin-top: 4px; }
  .pbtn { display: grid; place-items: center; width: 38px; height: 38px; border: 0; border-radius: 6px; background: transparent; color: var(--vid-ink); cursor: pointer; flex: none; }
  .pbtn:hover { background: var(--vid-hover); }
  .pbtn :global(svg) { width: 20px; height: 20px; }
  .pbtn.big :global(svg) { width: 24px; height: 24px; }
  .pbtn.txt { width: auto; padding: 0 10px; font: 600 13px/1 var(--font-ui); }
  .pbtn.on { background: var(--vid-hover); }
  .ptime { font: 500 12.5px/1 var(--font-mono); color: var(--vid-ink-2); margin-left: 10px; font-variant-numeric: tabular-nums; }
  .menuwrap { position: relative; }
  .submenu { position: absolute; right: 0; bottom: 46px; width: 300px; display: grid; gap: 6px; padding: 12px; background: var(--vid-panel); border: 1px solid var(--vid-edge); border-radius: 10px; z-index: 9; }
  .sm-h { font-size: 12px; color: var(--vid-ink-2); margin-top: 4px; }
  .sm-row { display: flex; gap: 6px; }
  .sm-row button, .sm-file { flex: 1; font: 600 12.5px/1 var(--font-ui); color: var(--vid-ink); background: var(--vid-hover); border: 0; border-radius: 6px; padding: 9px 8px; cursor: pointer; }
  .sm-row button:hover, .sm-file:hover { background: rgba(255, 255, 255, .18); }
  .sm-file { display: flex; justify-content: space-between; gap: 10px; text-align: left; }
  .sm-file span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 500; }
  .sm-file small { color: var(--vid-ink-2); flex: none; }
  .sm-file.on { outline: 1px solid var(--vid-ok); }
  .sm-file:disabled { opacity: .5; cursor: default; }
  .tx { position: absolute; top: 58px; right: 14px; bottom: 112px; width: 360px; z-index: 6; display: flex; flex-direction: column; background: var(--vid-panel); border: 1px solid var(--vid-edge); border-radius: 10px; overflow: hidden; }
  .tx-head { display: flex; justify-content: space-between; align-items: baseline; gap: 10px; padding: 12px 14px; border-bottom: 1px solid var(--vid-edge); }
  .tx-head span { font-size: 12px; color: var(--vid-ink-2); }
  .tx-list { list-style: none; margin: 0; padding: 6px; overflow: auto; flex: 1; }
  .tx-list li { display: grid; grid-template-columns: 46px 1fr; gap: 8px; padding: 6px 8px; border-radius: 6px; }
  .tx-list li:hover { background: var(--vid-hover); }
  .tx-list li.cur { background: rgba(255, 255, 255, .08); }
  .tt { all: unset; cursor: pointer; font: 500 11.5px/2.1 var(--font-mono); color: var(--vid-ink-2); }
  .tt:hover { color: var(--vid-ink); }
  .ln { margin: 0; font: 500 15px/1.6 var(--font-jp); user-select: text; -webkit-user-select: text; cursor: text; }
  .ln :global(rt) { font-size: .5em; }
  .tx-empty { display: block !important; font-size: 13px; color: var(--vid-ink-2); }
  .perr { position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%); z-index: 7; width: min(520px, 90%); display: grid; gap: 16px; padding: 22px; background: var(--vid-panel); border: 1px solid var(--vid-edge); border-radius: 12px; }
  .endov { position: absolute; inset: 0; z-index: 7; display: grid; place-items: center; background: rgba(0, 0, 0, .6); }
  .end-card { position: relative; width: min(540px, 92%); display: grid; gap: 16px; padding: 24px; background: var(--vid-panel); border: 1px solid var(--vid-edge); border-radius: 12px; color: var(--vid-ink); }
  .small { font-size: 13px; color: var(--vid-ink-2); }
  .end-card h3 { font: 600 22px/1.25 var(--font-display); padding-right: 60px; }
  .end-stats { display: grid; grid-template-columns: repeat(4, 1fr); border-top: 1px solid var(--vid-edge); border-bottom: 1px solid var(--vid-edge); }
  .end-stats div { display: grid; gap: 2px; padding: 12px 0; }
  .end-stats b { font: 600 20px/1.1 var(--font-ui); font-variant-numeric: tabular-nums; }
  .end-stats span { font-size: 12px; color: var(--vid-ink-2); }
  .end-actions { display: flex; justify-content: flex-end; gap: 10px; }
  .end-actions :global(.btn) { color: var(--vid); background: var(--vid-ink); border-color: var(--vid-ink); }
  .end-actions :global(.btn.quiet) { color: var(--vid-ink); background: transparent; border-color: var(--vid-edge); }
  .end-actions :global(.btn.quiet:hover) { background: var(--vid-hover); }
  .cd { position: absolute; top: 22px; right: 24px; width: 44px; height: 44px; }
  .cd svg { width: 44px; height: 44px; transform: rotate(-90deg); }
  .cd circle { fill: none; stroke-width: 3; }
  .cd .cd-bg { stroke: rgba(255, 255, 255, .14); }
  .cd .cd-fg { stroke: var(--vid-ink); stroke-linecap: round; stroke-dasharray: 113.1; animation: cd 8s linear forwards; }
  @keyframes cd { to { stroke-dashoffset: 113.1; } }
  .cd b { position: absolute; inset: 0; display: grid; place-items: center; font: 600 15px/1 var(--font-ui); font-variant-numeric: tabular-nums; }
</style>
