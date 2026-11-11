<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onDestroy, onMount, tick as nextTick } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, errorText, mediaUrl, setFullscreen, setWindowTitle, type AskProgress, type CardEvent, type CastName, type ClaudeStatus, type Line, type PlayerView, type Recap, type SeekSheet } from "$lib/api";
  import { createRenderer, drawTiles, filters } from "$lib/filters";
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
  let audioMenu = $state(false);
  let help = $state(false);
  let volume = $state(1);
  let muted = $state(false);
  /** Where the pointer is over the seek bar, in seconds. */
  let hoverAt = $state<number | null>(null);
  /** The seek bar's pictures, once they're loaded; `ratio` is one picture's width over its height. */
  let sheet = $state<(SeekSheet & { ratio: number }) | null>(null);
  /** Which of the sheet's pictures is nearest the pointer. */
  const hoverPic = $derived(sheet && hoverAt !== null ? Math.min(Math.round(hoverAt / sheet.every), sheet.columns * sheet.rows - 1) : 0);
  /** The rip's own English subtitles (null until they're read), shown while Tab is held. */
  let translation = $state<Line[] | null>(null);
  let showTranslation = $state(false);
  /** After a change of audio track: where to carry on from, and whether it was paused. */
  let reloadAt: { t: number; paused: boolean } | null = null;
  let videoError = $state("");
  let settings = $state<Record<string, unknown>>({});
  // Filters (V): the picture drawn through one, on a canvas over the video. Kept for each show.
  let filter = $state("none");
  let filterPanel = $state(false);
  let filtered = $state<HTMLCanvasElement | null>(null);
  /** Whether the filter has drawn the frame showing: until its shaders are compiled, the plain picture shows. */
  let filterShown = $state(false);
  const tiles: Record<string, HTMLCanvasElement> = {};
  let repaint: (() => void) | null = null;

  // Explain this line (E): Claude’s answer, shown as it’s written.
  let explaining = $state<{ index: number; text: string; done: boolean; error: string; ask: number } | null>(null);
  let explainResume = false;
  let explainPoll: ReturnType<typeof setTimeout> | null = null;
  // Follow-up questions about the line explained, and their answers.
  let thread = $state<{ q: string; a: string; done: boolean; error: string; ask: number }[]>([]);
  let question = $state("");
  let threadPoll: ReturnType<typeof setTimeout> | null = null;
  /** Goes up when the thread is dropped, so an answer on its way isn't followed. */
  let threadRun = 0;
  let exBody = $state<HTMLDivElement | null>(null);
  // Previously on (P): the moments of the story so far, and the recap card.
  // `full` is the whole story; without it, only the episode just left.
  let recap = $state<{ state: "writing" | "ready" | "error"; error: string; data: Recap | null; full: boolean } | null>(null);
  /** Picking up part-way through: where the episode was left, which the recap runs up to. */
  let recapAt = $state<number | null>(null);
  /** Goes up with each episode, so a wait for the last one's recap stops. */
  let recapRun = 0;
  let montage = $state<{ i: number; t: number } | null>(null);
  let montageVideo = $state<HTMLVideoElement | null>(null);
  /** The recap card is up: the episode waits for it. */
  let holdPlay = false;
  /** This episode started from the end card’s countdown: no recap for it. */
  let cameByAutoplay = false;
  // Characters' names in the subtitles, from the names dictionary.
  let castNames = $state<CastName[]>([]);

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
  /** The rip's English lines showing now. They're timed to the rip, so the nudge doesn't move them. */
  const translated = $derived(showTranslation ? (translation ?? []).filter((l) => l.start <= now && now < l.end) : []);
  /** Chapter starts to mark on the seek bar. */
  const chapterMarks = $derived((view?.chapters ?? []).filter((c) => c.start > 0.5 && c.start < duration));
  const inOpening = $derived(!!view?.opening && now >= view.opening.start && now < view.opening.end - 1);
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

  /** The names to look for, by their first character, longest first. */
  const namesByFirst = $derived.by(() => {
    const map = new Map<string, { chars: string[]; name: CastName; kana: boolean }[]>();
    if (settings["player.names"] === false) return map;
    for (const n of castNames) {
      const chars = [...n.term];
      if (chars.length < 2) continue;
      const list = map.get(chars[0]) ?? [];
      list.push({ chars, name: n, kana: chars.every(isKatakana) });
      map.set(chars[0], list);
    }
    for (const list of map.values()) list.sort((a, b) => b.chars.length - a.chars.length);
    return map;
  });

  /** Katakana and its long-vowel mark: a katakana name only counts when it isn’t part of a longer katakana word. */
  const isKatakana = (c: string | undefined) => !!c && /[ァ-ヺー]/.test(c);

  /** Where characters' names are in a line: start → (end, who). A name never splits a furigana base. */
  function nameMarks(chars: string[], ruby: [number, number, string][]): Map<number, { end: number; name: CastName }> {
    const marks = new Map<number, { end: number; name: CastName }>();
    if (!namesByFirst.size) return marks;
    const crossesRuby = (a: number, b: number) => ruby.some(([at, len]) => (a > at && a < at + len) || (b > at && b < at + len));
    for (let i = 0; i < chars.length; i++) {
      const found = (namesByFirst.get(chars[i]) ?? []).find(
        (n) =>
          n.chars.every((c, k) => chars[i + k] === c) &&
          !crossesRuby(i, i + n.chars.length) &&
          !(n.kana && (isKatakana(chars[i - 1]) || isKatakana(chars[i + n.chars.length]))),
      );
      if (!found) continue;
      marks.set(i, { end: i + found.chars.length, name: found.name });
      i += found.chars.length - 1;
    }
    return marks;
  }

  /** A line as HTML: furigana as ruby, breaks as <br>, characters' names marked. */
  function lineHtml(l: Line): string {
    const chars = [...l.text];
    const ruby = new Map<number, [number, string]>((l.ruby ?? []).map(([at, len, r]) => [at, [len, r]]));
    const marks = nameMarks(chars, l.ruby ?? []);
    let out = "";
    let closeAt = -1;
    for (let i = 0; i < chars.length; ) {
      const m = marks.get(i);
      if (m) {
        out += `<span class="nm" title="${esc(m.name.name)}">`;
        closeAt = m.end;
      }
      const r = ruby.get(i);
      if (r && r[0] > 0) {
        out += `<ruby>${esc(chars.slice(i, i + r[0]).join(""))}<rt>${esc(r[1])}</rt></ruby>`;
        i += r[0];
      } else {
        out += chars[i] === "\n" ? "<br>" : esc(chars[i]);
        i++;
      }
      if (closeAt >= 0 && i >= closeAt) {
        out += "</span>";
        closeAt = -1;
      }
    }
    if (closeAt >= 0) out += "</span>";
    return out;
  }

  function countChars(text: string): number {
    let n = 0;
    for (const c of text) if (/[\p{L}\p{N}]/u.test(c)) n++;
    return n;
  }

  function flash(text: string, ms = 1600) {
    note = text;
    if (noteTimer) clearTimeout(noteTimer);
    noteTimer = setTimeout(() => (note = ""), ms);
  }

  async function load(id: number) {
    error = "";
    videoError = "";
    ended = false;
    countdown = null;
    endingFired = false;
    counted.clear();
    sat = { lines: 0, chars: 0, cards: 0, seconds: 0 };
    closeExplain(false);
    closeRecap(false);
    const autoplayed = cameByAutoplay;
    cameByAutoplay = false;
    try {
      settings = await call<Record<string, unknown>>("settings");
      const v = await call<PlayerView>("openPlayer", { id });
      recapRun++;
      recapAt = v.resume?.possible ? v.episode.position : null;
      const back = wantRecap(v, autoplayed);
      const written = (recapAt === null ? v.recap : v.resume)?.ready === true;
      if (back && written) holdPlay = true;
      view = v;
      castNames = [];
      call<CastName[]>("showNames", { id: v.show.id }).then((n) => (castNames = n)).catch(() => {});
      nudge = v.episode.subsNudgeMs;
      subsOn = settings["player.subtitles"] !== false;
      blur = settings["player.blur"] === true;
      volume = typeof settings["player.volume"] === "number" ? (settings["player.volume"] as number) : 1;
      muted = settings["player.muted"] === true;
      translation = null;
      showTranslation = false;
      call<Line[]>("translation", { id })
        .catch(() => [])
        .then((lines) => {
          if (view?.episode.id === id) translation = lines;
        });
      if (sheet) URL.revokeObjectURL(sheet.src);
      sheet = null;
      if (v.episode.mediaState === "ready") {
        // Held in memory, so a picture is there the moment the pointer is.
        call<SeekSheet>("seekSheet", { id })
          .then(async (s) => {
            const src = URL.createObjectURL(await (await fetch(mediaUrl(s.src))).blob());
            const img = new Image();
            img.onload = () => {
              if (view?.episode.id === id) sheet = { ...s, src, ratio: (img.naturalWidth / s.columns) / (img.naturalHeight / s.rows) };
            };
            img.src = src;
          })
          .catch(() => {});
      }
      filter = filters.find((f) => f.id === settings[`player.filter.${v.show.id}`])?.id ?? "none";
      await setWindowTitle(`${v.show.title} · Episode ${v.episode.number} — Telly`);
      if (v.episode.mediaState !== "ready") {
        error = v.episode.mediaState === "failed" ? `This episode couldn’t be converted: ${v.episode.mediaError}` : "This episode isn’t ready to play yet.";
      } else if (holdPlay) {
        openRecap(true);
      } else if (back) {
        writeRecap();
      }
    } catch (e) {
      error = errorText(e);
    }
  }

  /** Whether this is coming back to a show after a while, to something there's a recap for. */
  function wantRecap(v: PlayerView, autoplayed: boolean): boolean {
    if (autoplayed || settings["recap.enabled"] === false) return false;
    if (!v.recap?.possible && !v.resume?.possible) return false;
    const hours = Number(settings["recap.afterHours"] ?? 8) || 8;
    return v.awayMs != null && v.awayMs >= hours * 3_600_000;
  }

  /** Back after less than this, the recap is only of the episode just left. */
  const LONG_AWAY_MS = 3 * 24 * 3_600_000;

  /** The recap isn’t written yet: the episode starts, and says when it is. */
  async function writeRecap() {
    const claude = await call<ClaudeStatus>("claudeStatus").catch(() => null);
    if (!claude?.available) return;
    const r = await getRecap().catch(() => null);
    if (r?.recap && !recap) flash("Recap ready (P)", 6000);
  }

  function startAt(): number {
    const e = view?.episode;
    if (!e) return 0;
    if (e.watchedAt === null && e.position > 5 && (e.duration <= 0 || e.position < e.duration * 0.95)) return e.position;
    return 0;
  }

  /** Plays the audio track `listed` of those the player lists: a rip that plays as it is has all its tracks, and starts on its first. */
  function pickAudio(el: HTMLVideoElement, listed: number) {
    const tracks = (el as HTMLVideoElement & { audioTracks?: ArrayLike<{ enabled: boolean }> }).audioTracks;
    if (tracks?.[listed] && !tracks[listed].enabled) tracks[listed].enabled = true;
  }

  function onLoaded() {
    if (!video) return;
    if (view) pickAudio(video, view.listedAudio);
    duration = video.duration || view?.episode.duration || 0;
    const reload = reloadAt;
    reloadAt = null;
    const at = reload ? reload.t : startAt();
    if (at > 0) video.currentTime = at;
    lastT = video.currentTime;
    video.playbackRate = rate;
    if (!holdPlay && !reload?.paused) video.play().catch(() => {});
  }

  // ---- the frame loop: subtitles and the lines that played -------------------

  function loop() {
    frame = requestAnimationFrame(loop);
    montageTick();
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
    suggestFilter();
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

  // A click on the picture pauses, a double-click goes fullscreen. The click
  // waits a moment for a second one, so a double-click doesn't pause and play.
  let clickTimer: ReturnType<typeof setTimeout> | null = null;
  let clickToggled = false;

  function picClick(e: MouseEvent) {
    if (e.detail !== 1) return;
    clickToggled = false;
    clickTimer = setTimeout(() => {
      clickTimer = null;
      clickToggled = true;
      togglePlay();
    }, 220);
  }

  function picDoubleClick() {
    if (clickTimer) clearTimeout(clickTimer);
    // A slow double-click: its first click already paused or played, so that's undone.
    else if (clickToggled) togglePlay();
    clickTimer = null;
    clickToggled = false;
    toggleFull();
  }

  // ---- sound ---------------------------------------------------------------------

  function setVolume(v: number) {
    volume = Math.round(Math.min(1, Math.max(0, v)) * 20) / 20;
    if (volume > 0) muted = false;
  }

  function saveSound() {
    call("setSetting", { key: "player.volume", value: volume }).catch(() => {});
    call("setSetting", { key: "player.muted", value: muted }).catch(() => {});
  }

  /** The - and = keys. */
  function stepVolume(by: number) {
    setVolume(volume + by);
    flash(`Volume ${Math.round(volume * 100)}%`);
    saveSound();
  }

  function toggleMute() {
    muted = !muted;
    flash(muted ? "Muted" : `Volume ${Math.round(volume * 100)}%`);
    saveSound();
  }

  async function useAudio(track: number) {
    audioMenu = false;
    if (!view || !video || track === view.audioTrack) return;
    const name = view.audio[track];
    toast({ key: "audio", state: "working", title: "Changing audio", text: name });
    try {
      const v = await call<PlayerView>("setAudio", { id: view.episode.id, track });
      if (view.episode.id !== v.episode.id) return;
      reloadAt = { t: video.currentTime, paused: video.paused };
      view.episode = v.episode;
      view.audioTrack = v.audioTrack;
      view.listedAudio = v.listedAudio;
      view.src = v.src;
      toast({ key: "audio", state: "done", title: "Audio changed", text: name });
    } catch (e) {
      toast({ key: "audio", state: "failed", title: "Couldn’t change audio", text: errorText(e) });
    }
  }

  function toggleSubs() {
    subsOn = !subsOn;
    flash(subsOn ? "Subtitles on" : "Subtitles off");
    call("setSetting", { key: "player.subtitles", value: subsOn }).catch(() => {});
  }

  function toggleBlur() {
    blur = !blur;
    flash(blur ? "Subtitles blurred until hovered" : "Blur off");
    call("setSetting", { key: "player.blur", value: blur }).catch(() => {});
  }

  /** Tab down: the rip's English subtitles show until it's let go. */
  function holdTranslation() {
    if (translation === null) flash("Reading the rip’s English subtitles…");
    else if (!translation.length) flash("This rip has no English text subtitles");
    else showTranslation = true;
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
    flash(ms === 0 ? "Nudge reset" : `Subtitles ${(Math.abs(ms) / 1000).toFixed(1)} seconds ${ms > 0 ? "later" : "earlier"}`);
    if (view) await call("nudge", { id: view.episode.id, ms }).catch(() => {});
  }

  async function lateFromHere(ms: number) {
    if (!view) return;
    subMenu = false;
    try {
      const updated = await call<Line[]>("lateFromHere", { id: view.episode.id, from: t - 1, ms });
      view.lines = updated;
      flash(`Lines from here on ${(Math.abs(ms) / 1000).toFixed(1)} seconds ${ms > 0 ? "later" : "earlier"}`);
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
      toast({ key: "resync", state: "failed", title: "Couldn’t use that file", text: errorText(e) });
    }
  }

  async function mineLine() {
    const l = active[0] ?? dialogue[current];
    if (!l || !view) return;
    try {
      await call("mineLine", { id: view.episode.id, start: l.start, end: l.end, text: l.text });
    } catch (e) {
      toast({ key: "mineline", state: "failed", title: "Couldn’t add the line", text: errorText(e) });
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

  // ---- filters (V) ------------------------------------------------------------------

  /** Keeps the show's filter. "none" is kept too: a show with one kept isn't offered Anime4K. */
  function keepFilter(id: string) {
    if (!view) return;
    const key = `player.filter.${view.show.id}`;
    settings[key] = id;
    call("setSetting", { key, value: id }).catch(() => {});
  }

  function pickFilter(id: string) {
    filter = id;
    keepFilter(id);
  }

  /** Anime4K is for a picture shown at well over its own size: says so, once, for a show with no filter picked. */
  function suggestFilter() {
    if (!view || !video?.videoHeight || recap || settings[`player.filter.${view.show.id}`] !== undefined) return;
    const box = video.parentElement as HTMLElement;
    const stretch = Math.min(box.clientWidth / video.videoWidth, box.clientHeight / video.videoHeight) * devicePixelRatio;
    if (stretch < 1.5) return;
    flash("Low-resolution picture: try Anime4K (V)", 6000);
    keepFilter("none");
  }

  // The filtered picture: drawn again for each frame of the video, and when the window's size changes.
  $effect(() => {
    const v = video;
    const canvas = filtered;
    if (!v || !canvas) return;
    const renderer = createRenderer(canvas);
    const box = canvas.parentElement as HTMLElement;
    const paint = () => {
      if (!v.videoWidth) return;
      const scale = Math.min(box.clientWidth / v.videoWidth, box.clientHeight / v.videoHeight) * devicePixelRatio;
      const w = Math.round(v.videoWidth * scale);
      const h = Math.round(v.videoHeight * scale);
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }
      filterShown = renderer.draw(v, v.videoWidth, v.videoHeight, filter);
      // Paused, no new frame comes to try again on.
      if (!filterShown && !retry) {
        retry = requestAnimationFrame(() => {
          retry = 0;
          paint();
        });
      }
    };
    let retry = 0;
    let handle = v.requestVideoFrameCallback(function next() {
      paint();
      handle = v.requestVideoFrameCallback(next);
    });
    const resized = new ResizeObserver(paint);
    resized.observe(box);
    repaint = paint;
    return () => {
      repaint = null;
      filterShown = false;
      cancelAnimationFrame(retry);
      v.cancelVideoFrameCallback(handle);
      resized.disconnect();
      renderer.dispose();
    };
  });

  // Another filter shows at once, paused or not.
  $effect(() => {
    if (filter !== "none") repaint?.();
  });

  // The panel's tiles show each filter on the frame that was playing when it opened.
  $effect(() => {
    if (filterPanel && !explaining && video) return drawTiles(video, tiles);
  });

  // ---- explain this line (E) ---------------------------------------------------------

  /** The line on screen, or the last one that played: its place in all the lines. */
  function lineHere(): number {
    const l = active[0] ?? dialogue[current];
    return l ? lines.indexOf(l) : -1;
  }

  async function explainLine(index = lineHere()) {
    if (!view) return;
    if (index < 0) return flash("No line here to explain");
    if (explaining?.index === index) return closeExplain();
    stopExplain();
    if (!explaining && video && !video.paused) {
      explainResume = true;
      video.pause();
    }
    pausedByHover = false;
    clearResume();
    explaining = { index, text: "", done: false, error: "", ask: 0 };
    try {
      const r = await call<AskProgress>("explain", { id: view.episode.id, index });
      if (explaining?.index !== index) return;
      if (r.done) explaining = { ...explaining, text: r.text ?? "", done: true };
      else if (r.ask) {
        explaining = { ...explaining, ask: r.ask };
        followExplain(index, r.ask);
      }
    } catch (e) {
      if (explaining?.index === index) explaining = { ...explaining, done: true, error: errorText(e) };
    }
  }

  function followExplain(index: number, ask: number) {
    explainPoll = setTimeout(async () => {
      if (explaining?.index !== index) return;
      try {
        const r = await call<AskProgress>("ask", { ask });
        if (explaining?.index !== index) return;
        explaining = { ...explaining, text: r.text ?? "", done: !!r.done, error: r.error ?? "" };
        if (!r.done) followExplain(index, ask);
      } catch (e) {
        if (explaining?.index === index) explaining = { ...explaining, done: true, error: errorText(e) };
      }
    }, 250);
  }

  /** Stops following an answer, and Claude writing it if it isn’t done. The follow-ups go with it. */
  function stopExplain() {
    if (explainPoll) clearTimeout(explainPoll);
    explainPoll = null;
    if (explaining && !explaining.done && explaining.ask) call("stopAsk", { ask: explaining.ask }).catch(() => {});
    if (threadPoll) clearTimeout(threadPoll);
    threadPoll = null;
    threadRun++;
    for (const item of thread) if (!item.done && item.ask) call("stopAsk", { ask: item.ask }).catch(() => {});
    thread = [];
    question = "";
  }

  /** Asks Claude the question in the box, about the line explained. */
  async function askFollowUp() {
    const q = question.trim();
    if (!q || !view || !explaining?.done || explaining.error || thread.some((item) => !item.done)) return;
    const run = threadRun;
    const earlier = thread.filter((item) => !item.error).map((item) => ({ q: item.q, a: item.a }));
    question = "";
    thread.push({ q, a: "", done: false, error: "", ask: 0 });
    const at = thread.length - 1;
    try {
      const r = await call<AskProgress>("followUp", { id: view.episode.id, index: explaining.index, explanation: explaining.text, thread: earlier, question: q });
      if (run !== threadRun) return;
      thread[at].ask = r.ask ?? 0;
      followThread(at, run);
    } catch (e) {
      if (run === threadRun) thread[at] = { ...thread[at], done: true, error: errorText(e) };
    }
  }

  function followThread(at: number, run: number) {
    threadPoll = setTimeout(async () => {
      if (run !== threadRun) return;
      try {
        const r = await call<AskProgress>("ask", { ask: thread[at].ask });
        if (run !== threadRun) return;
        thread[at] = { ...thread[at], a: r.text ?? "", done: !!r.done, error: r.error ?? "" };
        if (!r.done) followThread(at, run);
      } catch (e) {
        if (run === threadRun) thread[at] = { ...thread[at], done: true, error: errorText(e) };
      }
    }, 250);
  }

  // Keep the newest follow-up in view as it's written.
  $effect(() => {
    if (thread.length && thread[thread.length - 1].a !== undefined) exBody?.scrollTo({ top: exBody.scrollHeight });
  });

  function closeExplain(resume = true) {
    stopExplain();
    explaining = null;
    if (resume && explainResume && video) video.play().catch(() => {});
    explainResume = false;
  }

  /** Claude’s answer: a translation, then "term (reading): note" lines. */
  const explainRows = $derived.by(() => {
    const rows = (explaining?.text ?? "")
      .split("\n")
      .map((r) => r.trim().replace(/^([-•・*]|\d+\.)\s+/, ""))
      .filter(Boolean);
    const notes = rows.slice(1).map((r) => {
      const m = /^(.{1,48}?)\s*[:：]\s+(.+)$/.exec(r);
      return m ? { term: m[1].trim(), text: m[2].trim() } : { term: "", text: r };
    });
    return { translation: rows[0] ?? "", notes };
  });

  // ---- previously on (P) --------------------------------------------------------------

  /** This episode’s recap, once it’s written (a kept one comes straight back). Null when the episode has changed. */
  async function getRecap(): Promise<Recap | null> {
    if (!view) return null;
    const run = recapRun;
    const id = view.episode.id;
    let r = await call<Recap>("recap", { id, start: true, at: recapAt });
    const ask = r.ask;
    while (ask && !r.done) {
      await new Promise((f) => setTimeout(f, 1000));
      if (run !== recapRun) return null;
      r = await call<Recap>("recapAnswer", { id, ask, at: recapAt });
    }
    return run === recapRun ? r : null;
  }

  /** Opens the recap. `back` is for coming back to the show: its moments play first,
   *  and after a short while away it’s only of the episode just left. */
  async function openRecap(back = false) {
    if (!view || recap) return;
    holdPlay = true;
    video?.pause();
    recap = { state: "writing", error: "", data: null, full: !back || (view.awayMs ?? 0) >= LONG_AWAY_MS };
    try {
      const r = await getRecap();
      if (!r || !recap) return;
      if (r.recap) {
        recap = { ...recap, state: "ready", data: r };
        if (back) startMontage();
      } else {
        recap = { ...recap, state: "error", error: r.error ?? "" };
      }
    } catch (e) {
      if (recap) recap = { ...recap, state: "error", error: errorText(e) };
    }
  }

  /** Off the recap and into the episode. A recap still being written is kept for next time. */
  function closeRecap(play = true) {
    const wasOpen = recap !== null;
    recap = null;
    montage = null;
    holdPlay = false;
    if (play && wasOpen && video) video.play().catch(() => {});
  }

  /** The moments to play: those whose episode can play here, of the episode just left unless it’s the whole story. */
  const moments = $derived((recap?.data?.moments ?? []).filter((m) => m.playable && (recap?.full || m.number === recap?.data?.lastNumber)));
  const canRecap = $derived(!!(view?.recap?.possible || view?.resume?.possible));
  const canMontage = $derived(recap?.state === "ready" && moments.length > 0);
  /** The episode the moment playing comes from: switching to another loads it. */
  const montageSrc = $derived(montage ? mediaUrl(moments[montage.i]?.src ?? "") : "");

  function startMontage() {
    if (canMontage) montage = { i: 0, t: moments[0].start };
  }

  /** The moment `i`: straight there in the same episode, or once the other episode has loaded. */
  function playMoment(i: number) {
    const m = moments[i];
    if (!m || !montageVideo) return endMontage(true);
    const sameEpisode = montage !== null && moments[montage.i]?.src === m.src && montageVideo.readyState >= 1;
    montage = { i, t: m.start };
    if (sameEpisode) startMoment();
  }

  /** Seeks to the moment playing and plays it (on load, for another episode). */
  function startMoment() {
    if (!montage || !montageVideo) return;
    pickAudio(montageVideo, moments[montage.i].listedAudio);
    montageVideo.currentTime = moments[montage.i].start;
    montageVideo.play().catch(() => {});
  }

  const startLabel = $derived(`${recapAt === null ? "Start" : "Resume"} Episode ${view?.episode.number ?? ""}`);

  /** "Episode 7", or "Episodes 2–7". */
  function spanLabel(from: number | undefined, to: number | undefined): string {
    if (!to) return "the episodes before";
    return !from || from === to ? `Episode ${to}` : `Episodes ${from}–${to}`;
  }

  /** Follows the moment playing, and moves on at its end. Called every frame, and on the video's
   *  time updates too, which carry on when the window is hidden and frames stop. */
  function montageTick() {
    if (!montage || !montageVideo) return;
    const m = moments[montage.i];
    montage.t = montageVideo.currentTime;
    if (!m) endMontage(true);
    else if (montageVideo.currentTime >= m.end) playMoment(montage.i + 1);
  }

  /** Off the moments: into the episode, or to the recap card. */
  function endMontage(startEpisode: boolean) {
    montage = null;
    if (startEpisode) closeRecap(true);
  }

  const montageLines = $derived.by(() => {
    if (!montage) return [];
    const shift = (moments[montage.i]?.nudgeMs ?? 0) / 1000;
    const t = montage.t;
    return (moments[montage.i]?.lines ?? []).filter((l) => l.start + shift <= t && t < l.end + shift);
  });

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
    cameByAutoplay = true;
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
      if (video && !video.paused && !subMenu && !audioMenu && !help) idle = true;
    }, 2500);
  }

  function onKey(e: KeyboardEvent) {
    const target = e.target as HTMLElement | null;
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    wake();
    if (help) {
      if (e.key === "?" || e.key === "Escape") help = false;
      return;
    }
    // The recap card, or its moments, takes Space, Enter, P and Esc.
    if (recap) {
      if (e.key === " " || e.key === "Enter" || e.key === "Escape") {
        e.preventDefault();
        if (montage) endMontage(true);
        else closeRecap(true);
      } else if (e.key === "p" || e.key === "P") {
        if (montage) endMontage(false);
        else if (canMontage) startMontage();
        else closeRecap(true);
      }
      return;
    }
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
        toggleSubs();
        break;
      case "b":
      case "B":
        toggleBlur();
        break;
      case "-":
        stepVolume(-0.05);
        break;
      case "=":
      case "+":
        stepVolume(0.05);
        break;
      case "0":
        toggleMute();
        break;
      case "Tab":
        e.preventDefault();
        if (!e.repeat) holdTranslation();
        break;
      case "?":
        help = true;
        break;
      case "t":
      case "T":
        transcript = !transcript;
        break;
      case "v":
      case "V":
        filterPanel = !filterPanel;
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
      case "e":
      case "E":
        explainLine();
        break;
      case "p":
      case "P":
        if (canRecap) openRecap();
        else flash("No episode before this one to recap");
        break;
      case "Escape":
        if (explaining) closeExplain();
        else if (countdown !== null) stopCountdown();
        else if (subMenu) subMenu = false;
        else if (filterPanel) filterPanel = false;
        else if (audioMenu) audioMenu = false;
        else if (app.fullscreen) toggleFull();
        break;
    }
  }

  function onKeyUp(e: KeyboardEvent) {
    if (e.key === "Tab") showTranslation = false;
  }
  const dropTranslation = () => (showTranslation = false);

  /** The keys, for the list ? shows. */
  const SHORTCUTS: [string, string][] = [
    ["Space or K", "Play or pause"],
    ["← and →", "Previous and next line"],
    ["↓", "Replay the line"],
    ["- and =", "Volume down and up"],
    ["0", "Mute"],
    ["[ and ]", "Slower and faster"],
    ["S", "Show subtitles"],
    ["B", "Blur subtitles until hovered"],
    ["Hold Tab", "Show the rip’s English subtitles"],
    ["Z and X", "Subtitles earlier and later"],
    ["T", "Show transcript"],
    ["V", "Show filters"],
    ["M", "Add the line to the last card"],
    ["E", "Explain the line"],
    ["P", "Show Previously on"],
    ["F", "Fullscreen"],
    ["Esc", "Close, or leave fullscreen"],
    ["?", "Show shortcuts"],
  ];

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
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", dropTranslation);
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
    closeExplain(false);
    recapRun++;
    cancelAnimationFrame(frame);
    if (tickTimer) clearInterval(tickTimer);
    stopCountdown();
    clearResume();
    window.removeEventListener("keydown", onKey);
    window.removeEventListener("keyup", onKeyUp);
    window.removeEventListener("blur", dropTranslation);
    if (clickTimer) clearTimeout(clickTimer);
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
    // Machine-made subtitles have no sync score, and their file is who made them.
    if (e.subsState === "synced" && e.subsScore === null) return `Subtitles made by ${e.subsFile}`;
    if (e.subsState === "synced") return `Subtitles synced · ${Math.round((e.subsScore ?? 0) * 100)}%`;
    if (e.subsState === "check") return `Subtitle timing unsure · ${Math.round((e.subsScore ?? 0) * 100)}%`;
    if (e.subsState === "pending" || e.subsState === "working") return "Finding subtitles…";
    return e.subsDetail || "No subtitles";
  });

  /** Where the pointer is along the seek bar, in seconds. */
  function pointerTime(e: PointerEvent): number {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) * (duration || 0);
  }
</script>

<div class="player" class:idle class:paused onpointermove={wake} role="application" aria-label="Player">
  <div class="pic" onpointermove={picMove} onpointerleave={clearResume} onclick={picClick} ondblclick={picDoubleClick} role="presentation">
    {#if view && view.episode.mediaState === "ready"}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video
        bind:this={video}
        class:filtered={filter !== "none" && filterShown}
        crossorigin="anonymous"
        src={mediaUrl(view.src)}
        preload="auto"
        bind:volume
        bind:muted
        onloadedmetadata={onLoaded}
        onplay={() => { paused = false; wake(); }}
        onpause={() => (paused = true)}
        onseeked={() => video && (lastT = video.currentTime)}
        onended={finish}
        ondurationchange={() => video && (duration = video.duration)}
        onerror={() => (videoError = video?.error?.message || "The video couldn’t play.")}
      ></video>
      {#if filter !== "none"}<canvas class="filtered-pic" class:waiting={!filterShown} bind:this={filtered}></canvas>{/if}
    {/if}
  </div>

  <div class="subs" class:off={!subsOn} style:--sub-size="{subSize}px">
    {#each translated as line (line.start + line.text)}
      <p class="tr-line">{line.speaker ? `(${line.speaker}) ` : ""}{line.text}</p>
    {/each}
    {#each active as line (line.start + line.text)}
      <p class="sub-line" class:blur lang="ja" onpointerenter={hoverText}>
        {#if line.speaker}<span class="speaker">{line.speaker}</span>{/if}{@html lineHtml(line)}
      </p>
    {/each}
  </div>

  {#if pausedByHover}<span class="paused-note">Paused for reading · Move back to the picture to continue</span>{/if}
  {#if note}<span class="flash">{note}</span>{/if}
  {#if inOpening && view?.opening && !recap && !ended}
    <button type="button" class="skip" class:aside={transcript || filterPanel || !!explaining} onclick={() => view?.opening && seekTo(view.opening.end)}>Skip opening</button>
  {/if}

  <div class="ptop">
    <button type="button" class="pbtn" aria-label="Back to library" title="Back to library" onclick={() => leave("/")}><Icon name="back" /></button>
    {#if view}
      <span class="ptitle"><b>{view.show.title}</b><span>Episode {view.episode.number} of {view.count}</span></span>
      <span class="grow"></span>
      <span class="subnote" title={view.episode.subsFile}>
        {#if view.episode.subsState === "synced"}<Icon name="check" />{/if}{syncNote}{#if nudge !== 0} · {nudge > 0 ? "+" : ""}{(nudge / 1000).toFixed(1)} s{/if}
      </span>
      <button type="button" class="pbtn txt" onclick={() => leave(`/show?id=${view?.show.id}`)}>Show episodes</button>
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
      onpointerdown={(e) => { (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId); seekTo(pointerTime(e)); }}
      onpointermove={(e) => { hoverAt = pointerTime(e); if (e.buttons === 1) seekTo(hoverAt); }}
      onpointerleave={() => (hoverAt = null)}>
      <span class="track"><i style:width="{duration ? (100 * now) / duration : 0}%"></i></span>
      {#each chapterMarks as c (c.start)}<span class="chapter" style:left="{(100 * c.start) / duration}%"></span>{/each}
      <span class="knob" style:left="{duration ? (100 * now) / duration : 0}%"></span>
      <!-- Always there, so the pictures stay decoded and the first one shows at once. -->
      {#if duration > 0}
        <span class="seek-time" class:pictured={!!sheet} class:away={hoverAt === null} style:--at="{(100 * (hoverAt ?? 0)) / duration}%">
          {#if sheet}
            <span class="seek-pic" style:aspect-ratio={sheet.ratio}>
              <img src={sheet.src} alt="" style:width="{sheet.columns * 100}%"
                style:transform="translate(-{(100 * (hoverPic % sheet.columns)) / sheet.columns}%, -{(100 * Math.floor(hoverPic / sheet.columns)) / sheet.rows}%)" />
            </span>
          {/if}
          {clock(hoverAt ?? 0)}
        </span>
      {/if}
    </div>
    <div class="prow">
      <button type="button" class="pbtn big" aria-label={paused ? "Play" : "Pause"} onclick={togglePlay}><Icon name={paused ? "play" : "pause"} /></button>
      <button type="button" class="pbtn" aria-label="Previous line" title="Previous line (←)" onclick={prevLine}><Icon name="prevLine" /></button>
      <button type="button" class="pbtn" aria-label="Replay the line" title="Replay the line (↓)" onclick={replayLine}><Icon name="replay" /></button>
      <button type="button" class="pbtn" aria-label="Next line" title="Next line (→)" onclick={nextLine}><Icon name="nextLine" /></button>
      <span class="ptime">{clock(now)} / {clock(duration)}</span>
      <button type="button" class="pbtn" aria-label={muted ? "Unmute" : "Mute"} title="{muted ? "Unmute" : "Mute"} (0)" onclick={toggleMute}><Icon name={muted || volume === 0 ? "muted" : "volume"} /></button>
      <input class="vol" type="range" min="0" max="1" step="0.05" value={muted ? 0 : volume} aria-label="Volume" title="Volume (- and =)"
        oninput={(e) => setVolume(Number(e.currentTarget.value))} onchange={(e) => { saveSound(); e.currentTarget.blur(); }} />
      <span class="grow"></span>
      {#if rate !== 1}<button type="button" class="pbtn txt" title="Speed ([ and ])" onclick={() => setRate(1)}>{rate.toFixed(1)}×</button>{/if}
      <button type="button" class="pbtn" class:on={subsOn} aria-label="Subtitles" title="Subtitles (S)" onclick={toggleSubs}><Icon name="subs" /></button>
      <button type="button" class="pbtn" class:on={blur} aria-label="Blur until hovered" title="Blur until hovered (B)" onclick={toggleBlur}><Icon name="blur" /></button>
      <button type="button" class="pbtn" class:on={transcript} aria-label="Transcript" title="Transcript (T)" onclick={() => (transcript = !transcript)}><Icon name="transcript" /></button>
      <button type="button" class="pbtn txt" class:on={!!explaining} title="Explain the line with Claude (E)" onclick={() => explainLine()}>Explain</button>
      {#if canRecap}
        <button type="button" class="pbtn txt" title="Previously on (P)" onclick={() => openRecap()}>Recap</button>
      {/if}
      <button type="button" class="pbtn txt" class:on={filterPanel} aria-expanded={filterPanel} title="Filters (V)" onclick={() => (filterPanel = !filterPanel)}>Filters</button>
      {#if view && view.audio.length > 1}
        <span class="menuwrap">
          <button type="button" class="pbtn txt" class:on={audioMenu} aria-haspopup="menu" aria-expanded={audioMenu} title="Choose the audio track" onclick={() => { audioMenu = !audioMenu; subMenu = false; }}>Audio ▾</button>
          {#if audioMenu}
            <div class="submenu" role="menu">
              <span class="sm-h">Audio track for this show</span>
              {#each view.audio as name, i (i)}
                <button type="button" class="sm-file" class:on={i === view.audioTrack} role="menuitemradio" aria-checked={i === view.audioTrack} onclick={() => useAudio(i)}><span>{name}</span></button>
              {/each}
            </div>
          {/if}
        </span>
      {/if}
      <span class="menuwrap">
        <button type="button" class="pbtn txt" class:on={subMenu} aria-haspopup="menu" aria-expanded={subMenu} title="Fix the subtitle timing" onclick={() => { subMenu = !subMenu; audioMenu = false; }}>Sync ▾</button>
        {#if subMenu && view}
          <div class="submenu" role="menu">
            <span class="sm-h">Nudge every line by 0.1 seconds (Z and X)</span>
            <div class="sm-row">
              <button type="button" onclick={() => setNudge(nudge - 100)}>Earlier</button>
              <button type="button" onclick={() => setNudge(0)}>Reset</button>
              <button type="button" onclick={() => setNudge(nudge + 100)}>Later</button>
            </div>
            <span class="sm-h">Move the lines from here on by half a second</span>
            <div class="sm-row">
              <button type="button" onclick={() => lateFromHere(-500)}>Earlier</button>
              <button type="button" onclick={() => lateFromHere(500)}>Later</button>
            </div>
            {#if view.tried.length}
              <span class="sm-h">Try another file</span>
              {#each view.tried as f (f.path)}
                <button type="button" class="sm-file" class:on={f.name === view.episode.subsFile} title={f.name} onclick={() => useFile(f.path, f.name)} disabled={!!f.error}>
                  <span>{f.name}</span><small>{f.error ? "unreadable" : `${Math.round(f.score * 100)}%`}</small>
                </button>
              {/each}
            {/if}
          </div>
        {/if}
      </span>
      <button type="button" class="pbtn txt" class:on={help} aria-label="Show shortcuts" title="Show shortcuts (?)" onclick={() => (help = !help)}>?</button>
      <button type="button" class="pbtn" aria-label="Fullscreen" title="Fullscreen (F)" onclick={toggleFull}><Icon name={app.fullscreen ? "exitFull" : "full"} /></button>
    </div>
  </div>

  {#if filterPanel && !explaining}
    <aside class="tx" aria-label="Filters">
      <div class="tx-head"><b>Filters</b><button type="button" class="ex-close" title="Close (V)" onclick={() => (filterPanel = false)}>Close</button></div>
      <div class="fx-grid scroll">
        {#each filters as f (f.id)}
          <button type="button" class="fx" aria-pressed={filter === f.id} onclick={() => pickFilter(f.id)}>
            <canvas bind:this={tiles[f.id]} width="160" height="160"></canvas>
            <b>{f.name}</b>
            {#if f.note}<span>{f.note}</span>{/if}
          </button>
        {/each}
      </div>
    </aside>
  {/if}

  {#if transcript && !explaining && !filterPanel}
    <aside class="tx">
      <div class="tx-head"><b>Transcript</b><span>{dialogue.length} lines</span></div>
      <ol class="tx-list scroll" bind:this={txList}>
        {#each dialogue as l, i (i)}
          <li data-i={i} class:cur={i === current}>
            <button type="button" class="tt" onclick={() => seekTo(lineStart(l))}>{clock(l.start)}</button>
            <p class="ln" lang="ja" onpointerenter={hoverText}>{@html lineHtml(l)}</p>
            <button type="button" class="tx-ex" title="Explain this line" onclick={() => explainLine(lines.indexOf(l))}>Explain</button>
          </li>
        {:else}
          <li class="tx-empty">No subtitles for this episode yet.</li>
        {/each}
      </ol>
    </aside>
  {/if}

  {#if explaining && view}
    <aside class="tx ex" aria-label="Explain this line">
      <div class="tx-head"><b>Explain line</b><button type="button" class="ex-close" title="Close (E)" onclick={() => closeExplain()}>Close</button></div>
      <div class="ex-body scroll" bind:this={exBody}>
        {#if lines[explaining.index]}<p class="ex-line" lang="ja" onpointerenter={hoverText}>{@html lineHtml(lines[explaining.index])}</p>{/if}
        {#if explaining.error}
          <p class="ex-err">{explaining.error}</p>
        {:else if !explaining.text}
          <p class="ex-wait">Asking Claude…</p>
        {:else}
          <p class="ex-tr">{explainRows.translation}</p>
          {#if explainRows.notes.length}
            <dl class="ex-notes">
              {#each explainRows.notes as n, k (k)}
                {#if n.term}<dt lang="ja" onpointerenter={hoverText}>{n.term}</dt>{/if}
                <dd class:solo={!n.term}>{n.text}</dd>
              {/each}
            </dl>
          {/if}
          {#if !explaining.done}<p class="ex-wait">…</p>{/if}
          {#each thread as item, k (k)}
            <p class="ex-q">{item.q}</p>
            {#if item.error}
              <p class="ex-err">{item.error}</p>
            {:else}
              <p class="ex-a" onpointerenter={hoverText}>{item.a || "Asking Claude…"}</p>
            {/if}
          {/each}
        {/if}
      </div>
      {#if explaining.done && !explaining.error}
        <form class="ex-ask" onsubmit={(e) => { e.preventDefault(); askFollowUp(); }}>
          <input bind:value={question} placeholder="Ask about this line" aria-label="Follow-up question" />
          <button type="submit" disabled={!question.trim() || thread.some((item) => !item.done)}>Ask</button>
        </form>
      {/if}
      <div class="ex-foot">Answered by Claude, through Claude Code{explaining.done && !explaining.error ? " · saved for next time" : ""}</div>
    </aside>
  {/if}

  {#if help}
    <div class="endov" role="presentation" onclick={(e) => e.target === e.currentTarget && (help = false)}>
      <div class="end-card keys-card" role="dialog" aria-label="Shortcuts">
        <h3>Shortcuts</h3>
        <dl class="keys">
          {#each SHORTCUTS as [key, what] (key)}
            <dt>{key}</dt>
            <dd>{what}</dd>
          {/each}
        </dl>
        <div class="end-actions"><button type="button" class="btn quiet" onclick={() => (help = false)}>Close</button></div>
      </div>
    </div>
  {/if}

  {#if recap && view && !montage}
    <div class="endov recapov">
      <div class="end-card recap-card">
        <span class="small">Previously on</span>
        <h3>{view.show.title}</h3>
        {#if recap.state === "writing"}
          <p class="recap-text wait">Claude is writing the recap…</p>
        {:else if recap.state === "error"}
          <p class="recap-text wait">{recap.error}</p>
        {:else if recap.data}
          <div class="recap-body scroll">
            {#if recap.full}
              {#each recap.data.arcs ?? [] as arc, k (k)}
                <section class="arc">
                  <div class="arc-head"><b>{arc.title}</b><span>{spanLabel(arc.from, arc.to)}</span></div>
                  <p class="recap-text">{arc.summary}</p>
                </section>
              {/each}
            {/if}
            {#if recap.data.last}
              <section class="arc">
                <div class="arc-head"><b>{recapAt === null ? "Last time" : "So far"}</b>{#if recap.data.lastNumber}<span>Episode {recap.data.lastNumber}</span>{/if}</div>
                <p class="recap-text">{recap.data.last}</p>
              </section>
            {/if}
          </div>
        {/if}
        <div class="end-actions">
          {#if recap.state === "ready" && !recap.full && recap.data?.arcs?.length}
            <button type="button" class="btn quiet" onclick={() => recap && (recap.full = true)}>Show story so far</button>
          {/if}
          {#if canMontage}
            <button type="button" class="btn quiet" title="Watch the moments (P)" onclick={startMontage}>Watch the moments</button>
          {/if}
          <button type="button" class="btn" title="{startLabel} (Enter)" onclick={() => closeRecap(true)}><Icon name="play" /><span>{startLabel}</span></button>
        </div>
      </div>
    </div>
  {/if}

  {#if montage && moments[montage.i] && view}
    <div class="montage">
      <!-- svelte-ignore a11y_media_has_caption -->
      <video bind:this={montageVideo} src={montageSrc} preload="auto" bind:volume bind:muted onloadedmetadata={startMoment} ontimeupdate={montageTick} onerror={() => endMontage(false)}></video>
      <div class="montage-top">
        <span class="montage-label">Previously on {view.show.title} · Episode {moments[montage.i].number} · {montage.i + 1} of {moments.length}</span>
        {#if moments[montage.i].caption}<p class="montage-caption" aria-live="polite">{moments[montage.i].caption}</p>{/if}
      </div>
      <div class="subs montage-subs" style:--sub-size="{subSize}px">
        {#each montageLines as l (l.start + l.text)}
          <p class="sub-line" lang="ja">{@html lineHtml(l)}</p>
        {/each}
      </div>
      <div class="montage-actions">
        <button type="button" class="pbtn txt" title="Read the recap (P)" onclick={() => endMontage(false)}>Read recap</button>
        <button type="button" class="pbtn txt" title="{startLabel} (Enter)" onclick={() => endMontage(true)}>{startLabel}</button>
      </div>
    </div>
  {/if}

  {#if ended && view}
    <div class="endov">
      <div class="end-card">
        <span class="small">Episode {view.episode.number} done{sat.seconds > 30 ? ` · logged to Tidepool` : ""}</span>
        <h3>{view.next ? `Next: Episode ${view.next.number}` : `That’s the last episode here`}</h3>
        <div class="end-stats">
          <div><b>{Math.round(sat.seconds / 60)}</b><span>minutes</span></div>
          <div><b>{sat.lines}</b><span>lines</span></div>
          <div><b>{sat.chars}</b><span>characters</span></div>
          <div><b>{sat.cards}</b><span>cards</span></div>
        </div>
        {#if view.next && !view.next.ready}
          <p class="small">Episode {view.next.number} is getting ready{view.next.mediaState === "working" ? ` · ${Math.round(view.next.mediaProgress * 100)}%` : ""}. It’ll start when it can.</p>
        {/if}
        <div class="end-actions">
          <button type="button" class="btn quiet" onclick={() => leave("/")}>Open library</button>
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
  .pic video.filtered { opacity: 0; }
  .filtered-pic.waiting { visibility: hidden; }
  .filtered-pic { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: contain; pointer-events: none; }
  .subs { position: absolute; left: 0; right: 0; bottom: 96px; display: flex; flex-direction: column; align-items: center; gap: 2px; pointer-events: none; z-index: 5; }
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
  .seek .chapter { position: absolute; top: 8px; width: 2px; height: 7px; margin-left: -1px; background: var(--vid); pointer-events: none; }
  .seek-time { position: absolute; left: var(--at); bottom: 24px; transform: translateX(-50%); padding: 5px 8px; border-radius: 6px; font: 500 12.5px/1 var(--font-mono); font-variant-numeric: tabular-nums; background: var(--vid-panel); border: 1px solid var(--vid-edge); pointer-events: none; white-space: nowrap; text-align: center; }
  .seek-time.away { visibility: hidden; }
  /* With a picture the label is 178px wide; it stops at the bar's ends. */
  .seek-time.pictured { left: clamp(89px, var(--at), calc(100% - 89px)); }
  .seek-pic { display: block; width: 160px; margin-bottom: 6px; border-radius: 3px; overflow: hidden; }
  .seek-pic img { display: block; max-width: none; transform-origin: 0 0; }
  .vol { width: 84px; height: 22px; margin: 0 6px 0 2px; accent-color: var(--vid-ink); cursor: pointer; flex: none; }
  .skip { position: absolute; right: 20px; bottom: 236px; z-index: 6; height: 40px; padding: 0 18px; font: 600 14px/1 var(--font-ui); color: var(--vid-ink); background: var(--vid-panel); border: 1px solid rgba(255, 255, 255, .5); border-radius: 6px; cursor: pointer; }
  .skip:hover { background: #2a2a2e; }
  .skip.aside { right: 388px; }
  .tr-line { pointer-events: none; max-width: 86%; margin: 0 0 4px; padding: 4px 12px; border-radius: 6px; font: 500 calc(var(--sub-size, 36px) * .62)/1.35 var(--font-ui); color: var(--vid-ink); background: rgba(0, 0, 0, .72); text-align: center; white-space: pre-line; visibility: visible; }
  .keys-card { width: min(460px, 92%); max-height: 88vh; overflow: auto; }
  .keys-card h3 { padding-right: 0; }
  .keys { margin: 0; display: grid; grid-template-columns: auto 1fr; gap: 7px 22px; font-size: 14px; }
  .keys dt { font: 600 13px/1.5 var(--font-mono); white-space: nowrap; }
  .keys dd { margin: 0; color: var(--vid-ink-2); }
  .ex-q { margin: 8px 0 0; padding-top: 12px; border-top: 1px solid var(--vid-edge); font: 600 14px/1.45 var(--font-ui); user-select: text; -webkit-user-select: text; }
  .ex-a { margin: 0; font: 400 14px/1.55 var(--font-ui); white-space: pre-line; user-select: text; -webkit-user-select: text; cursor: text; }
  .ex-ask { display: flex; gap: 8px; padding: 10px 12px; border-top: 1px solid var(--vid-edge); }
  .ex-ask input { flex: 1; min-width: 0; height: 34px; padding: 0 10px; font: 400 14px/1 var(--font-ui); color: var(--vid-ink); background: rgba(255, 255, 255, .07); border: 1px solid var(--vid-edge); border-radius: 6px; }
  .ex-ask input:focus-visible { outline: 2px solid var(--vid-ink); outline-offset: 1px; }
  .ex-ask button { height: 34px; padding: 0 14px; font: 600 13px/1 var(--font-ui); color: var(--vid); background: var(--vid-ink); border: 0; border-radius: 6px; cursor: pointer; }
  .ex-ask button:disabled { opacity: .42; cursor: default; }
  .prow { display: flex; align-items: center; gap: 2px; margin-top: 4px; }
  .pbtn { display: grid; place-items: center; width: 38px; height: 38px; border: 0; border-radius: 6px; background: transparent; color: var(--vid-ink); cursor: pointer; flex: none; }
  .pbtn:hover { background: var(--vid-hover); }
  .pbtn :global(svg) { width: 20px; height: 20px; }
  .pbtn.big :global(svg) { width: 24px; height: 24px; }
  .pbtn.txt { width: auto; padding: 0 10px; font: 600 13px/1 var(--font-ui); }
  .pbtn.on { background: var(--vid-hover); }
  .ptime { font: 500 12.5px/1 var(--font-mono); color: var(--vid-ink-2); margin-left: 10px; font-variant-numeric: tabular-nums; }
  .menuwrap { position: relative; }
  .submenu { position: absolute; right: 0; bottom: 46px; width: 300px; display: grid; grid-template-columns: minmax(0, 1fr); gap: 6px; padding: 12px; background: var(--vid-panel); border: 1px solid var(--vid-edge); border-radius: 10px; z-index: 9; }
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
  /* Characters' names, from the names dictionary. */
  .sub-line :global(.nm), .ln :global(.nm), .ex-line :global(.nm) { color: var(--sub-name, #9fd0ff); }
  .tx-list li { grid-template-columns: 46px 1fr auto; }
  .tx-ex { all: unset; cursor: pointer; align-self: start; font: 600 11.5px/2.1 var(--font-ui); color: var(--vid-ink-2); visibility: hidden; padding: 0 4px; }
  .tx-list li:hover .tx-ex { visibility: visible; }
  .tx-ex:hover { color: var(--vid-ink); }
  .ex-close { all: unset; cursor: pointer; font: 600 12px/1 var(--font-ui); color: var(--vid-ink-2); }
  .ex-close:hover { color: var(--vid-ink); }
  .ex-body { flex: 1; overflow: auto; padding: 14px 16px; display: grid; align-content: start; gap: 12px; }
  .ex-line { margin: 0; font: 600 21px/1.55 var(--font-jp); user-select: text; -webkit-user-select: text; cursor: text; }
  .ex-line :global(rt) { font-size: .45em; }
  .ex-tr { margin: 0; font: 500 15.5px/1.5 var(--font-ui); }
  .ex-notes { margin: 0; display: grid; gap: 4px; border-top: 1px solid var(--vid-edge); padding-top: 12px; }
  .ex-notes dt { font: 600 15px/1.4 var(--font-jp); margin-top: 8px; user-select: text; -webkit-user-select: text; }
  .ex-notes dd { margin: 0; font: 400 13.5px/1.5 var(--font-ui); color: var(--vid-ink-2); }
  .ex-notes dd.solo { margin-top: 8px; color: var(--vid-ink); }
  .ex-wait { margin: 0; font: 500 13.5px/1.5 var(--font-ui); color: var(--vid-ink-2); }
  .ex-err { margin: 0; font: 500 13.5px/1.5 var(--font-ui); color: #ffb4a8; }
  .ex-foot { padding: 9px 14px; border-top: 1px solid var(--vid-edge); font-size: 11.5px; color: var(--vid-ink-2); }
  .fx-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); align-content: start; gap: 16px 10px; padding: 14px 12px; overflow: auto; flex: 1; }
  .fx { all: unset; cursor: pointer; display: grid; align-content: start; gap: 2px; border-radius: 6px; }
  .fx canvas { display: block; width: 100%; aspect-ratio: 1; margin-bottom: 5px; border-radius: 6px; background: var(--vid); }
  .fx[aria-pressed="true"] canvas { outline: 2px solid var(--vid-ink); outline-offset: 2px; }
  .fx:focus-visible { outline: 2px solid var(--vid-ink); outline-offset: 5px; }
  .fx b { font: 600 12.5px/1.25 var(--font-ui); }
  .fx span { font-size: 11.5px; line-height: 1.25; color: var(--vid-ink-2); }
  .recapov { z-index: 8; background: rgba(0, 0, 0, .72); }
  .recap-card h3 { padding-right: 0; }
  .recap-text { margin: 0; font: 400 16px/1.6 var(--font-ui); white-space: pre-line; }
  .recap-text.wait { color: var(--vid-ink-2); font-size: 14.5px; }
  .recap-card { max-height: 86vh; }
  .recap-body { display: grid; gap: 16px; max-height: 56vh; overflow: auto; padding-right: 4px; }
  .arc { display: grid; gap: 4px; }
  .arc-head { display: flex; align-items: baseline; gap: 10px; }
  .arc-head b { font: 600 15px/1.3 var(--font-ui); }
  .arc-head span { font-size: 12.5px; color: var(--vid-ink-2); white-space: nowrap; }
  .montage { position: absolute; inset: 0; z-index: 9; background: var(--vid); }
  .montage video { width: 100%; height: 100%; object-fit: contain; display: block; }
  .montage-top { position: absolute; top: 18px; left: 20px; right: 20px; display: grid; gap: 10px; justify-items: start; }
  .montage-label { font: 600 13px/1 var(--font-ui); letter-spacing: .02em; color: var(--vid-ink); text-shadow: 0 1px 3px rgba(0, 0, 0, .9); }
  .montage-caption { margin: 0; max-width: 60ch; padding: 8px 12px; font: 400 18px/1.4 var(--font-ui); color: var(--vid-ink); background: var(--vid-panel); border: 1px solid var(--vid-edge); border-radius: 8px; }
  .montage-subs { bottom: 72px; }
  .montage-subs .sub-line { pointer-events: none; }
  .montage-actions { position: absolute; right: 18px; bottom: 18px; display: flex; gap: 10px; }
  .montage-actions .pbtn { background: var(--vid-panel); border: 1px solid var(--vid-edge); }
</style>
