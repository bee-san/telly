<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onDestroy, onMount, tick as nextTick } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, errorText, mediaUrl, setFullscreen, setWindowTitle, type AskProgress, type CardEvent, type CastName, type ClaudeStatus, type Line, type PlayerView, type Recap } from "$lib/api";
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

  // Explain this line (E): Claude’s answer, shown as it’s written.
  let explaining = $state<{ index: number; text: string; done: boolean; error: string; ask: number } | null>(null);
  let explainResume = false;
  let explainPoll: ReturnType<typeof setTimeout> | null = null;
  // Previously on (P): the recap card, then the moments of the episode before.
  let recap = $state<{ state: "writing" | "ready" | "error"; text: string; error: string; data: Recap | null } | null>(null);
  let recapPoll: ReturnType<typeof setTimeout> | null = null;
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
    closeExplain(false);
    closeRecap(false);
    const autoplayed = cameByAutoplay;
    cameByAutoplay = false;
    try {
      settings = await call<Record<string, unknown>>("settings");
      const v = await call<PlayerView>("openPlayer", { id });
      if (await wantRecap(v, autoplayed)) holdPlay = true;
      view = v;
      castNames = [];
      call<CastName[]>("showNames", { id: v.show.id }).then((n) => (castNames = n)).catch(() => {});
      nudge = v.episode.subsNudgeMs;
      subsOn = settings["player.subtitles"] !== false;
      blur = settings["player.blur"] === true;
      await setWindowTitle(`${v.show.title} · Episode ${v.episode.number} — Telly`);
      if (v.episode.mediaState !== "ready") {
        error = v.episode.mediaState === "failed" ? `This episode couldn’t be converted: ${v.episode.mediaError}` : "This episode isn’t ready to play yet.";
      } else if (holdPlay) {
        openRecap();
      }
    } catch (e) {
      error = errorText(e);
    }
  }

  /** Whether to open on "Previously on": back after a while, at the start of an episode with one before it. */
  async function wantRecap(v: PlayerView, autoplayed: boolean): Promise<boolean> {
    if (autoplayed || settings["recap.enabled"] === false || !v.recap?.possible) return false;
    const hours = Number(settings["recap.afterHours"] ?? 8) || 8;
    if (v.awayMs == null || v.awayMs < hours * 3_600_000) return false;
    const e = v.episode;
    const resuming = e.watchedAt === null && e.position > 30 && (e.duration <= 0 || e.position < e.duration * 0.95);
    if (resuming) return false;
    if (v.recap.ready) return true;
    const claude = await call<ClaudeStatus>("claudeStatus").catch(() => null);
    return !!claude?.available;
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
    if (!holdPlay) video.play().catch(() => {});
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

  /** Stops following an answer, and Claude writing it if it isn’t done. */
  function stopExplain() {
    if (explainPoll) clearTimeout(explainPoll);
    explainPoll = null;
    if (explaining && !explaining.done && explaining.ask) call("stopAsk", { ask: explaining.ask }).catch(() => {});
  }

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

  async function openRecap() {
    if (!view || recap) return;
    holdPlay = true;
    video?.pause();
    recap = { state: "writing", text: "", error: "", data: null };
    try {
      settleRecap(await call<Recap>("recap", { id: view.episode.id, start: true }));
    } catch (e) {
      recap = { state: "error", text: "", error: errorText(e), data: null };
    }
  }

  function settleRecap(r: Recap) {
    if (!recap) return;
    if (r.recap) recap = { state: "ready", text: r.recap, error: "", data: r };
    else if (r.error) recap = { state: "error", text: "", error: r.error, data: null };
    else if (r.ask) followRecap(r.ask);
  }

  function followRecap(ask: number) {
    recapPoll = setTimeout(async () => {
      if (!recap || !view) return;
      try {
        const r = await call<Recap>("recapAnswer", { id: view.episode.id, ask });
        if (r.done) settleRecap(r);
        else followRecap(ask);
      } catch (e) {
        if (recap) recap = { state: "error", text: "", error: errorText(e), data: null };
      }
    }, 1000);
  }

  /** Off the recap and into the episode. A recap still being written is kept for next time. */
  function closeRecap(play = true) {
    if (recapPoll) clearTimeout(recapPoll);
    recapPoll = null;
    const wasOpen = recap !== null;
    recap = null;
    montage = null;
    holdPlay = false;
    if (play && wasOpen && video) video.play().catch(() => {});
  }

  /** The moments to play: only those whose episode can play here. */
  const moments = $derived((recap?.data?.moments ?? []).filter((m) => m.playable));
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
    montageVideo.currentTime = moments[montage.i].start;
    montageVideo.play().catch(() => {});
  }

  /** "Episode 7", or "Episodes 2–7". */
  function spanLabel(from: number | undefined, to: number | undefined): string {
    if (!to) return "the episodes before";
    return !from || from === to ? `Episode ${to}` : `Episodes ${from}–${to}`;
  }

  /** "Episode 3", "Episodes 3–5", "Episodes 3, 5 and 6". */
  function episodesLabel(list: number[]): string {
    const sorted = [...list].sort((a, b) => a - b);
    if (!sorted.length) return "";
    if (sorted.length === 1) return `Episode ${sorted[0]}`;
    const run = sorted.every((n, i) => i === 0 || n === sorted[i - 1] + 1);
    if (run) return `Episodes ${sorted[0]}–${sorted[sorted.length - 1]}`;
    return `Episodes ${sorted.slice(0, -1).join(", ")} and ${sorted[sorted.length - 1]}`;
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
      if (video && !video.paused && !subMenu) idle = true;
    }, 2500);
  }

  function onKey(e: KeyboardEvent) {
    const target = e.target as HTMLElement | null;
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    wake();
    // The recap card, or its moments, takes Space, Enter, P and Esc.
    if (recap) {
      if (e.key === " " || e.key === "Enter" || e.key === "Escape") {
        e.preventDefault();
        if (montage) endMontage(true);
        else closeRecap(true);
      } else if ((e.key === "p" || e.key === "P") && !montage) {
        if (canMontage) startMontage();
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
        subsOn = !subsOn;
        flash(subsOn ? "Subtitles on" : "Subtitles off");
        call("setSetting", { key: "player.subtitles", value: subsOn }).catch(() => {});
        break;
      case "b":
      case "B":
        blur = !blur;
        flash(blur ? "Subtitles blurred until hovered" : "Blur off");
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
      case "e":
      case "E":
        explainLine();
        break;
      case "p":
      case "P":
        if (view?.recap?.possible) openRecap();
        else flash("No episode before this one to recap");
        break;
      case "Escape":
        if (explaining) closeExplain();
        else if (countdown !== null) stopCountdown();
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
    closeExplain(false);
    if (recapPoll) clearTimeout(recapPoll);
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
    if (e.subsState === "check") return `Subtitle timing unsure · ${Math.round((e.subsScore ?? 0) * 100)}%`;
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
        onerror={() => (videoError = video?.error?.message || "The video couldn’t play.")}
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

  {#if pausedByHover}<span class="paused-note">Paused for reading · Move back to the picture to continue</span>{/if}
  {#if note}<span class="flash">{note}</span>{/if}

  <div class="ptop">
    <button type="button" class="pbtn" aria-label="Back to library" title="Back to library" onclick={() => leave("/")}><Icon name="back" /></button>
    {#if view}
      <span class="ptitle"><b>{view.show.title}</b><span>Episode {view.episode.number} of {view.count}</span></span>
      <span class="grow"></span>
      <span class="subnote" title={view.episode.subsFile}>
        {#if view.episode.subsState === "synced"}<Icon name="check" />{/if}{syncNote}{#if nudge !== 0} · {nudge > 0 ? "+" : ""}{(nudge / 1000).toFixed(1)} s{/if}
      </span>
      <button type="button" class="pbtn txt" onclick={() => leave(`/show?id=${view?.show.id}`)}>Show Episodes</button>
    {/if}
  </div>

  {#if error || videoError}
    <div class="perr">
      <p>{error || videoError}</p>
      <div class="end-actions">
        {#if videoError && view}
          <button type="button" class="btn" onclick={async () => { if (view) { await call("retryMedia", { id: view.episode.id, force: true }); leave(`/show?id=${view.show.id}`); } }}>Convert This Episode</button>
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
      <button type="button" class="pbtn" class:on={blur} aria-label="Blur until hovered" title="Blur until hovered (B)" onclick={() => (blur = !blur)}><Icon name="blur" /></button>
      <button type="button" class="pbtn" class:on={transcript} aria-label="Transcript" title="Transcript (T)" onclick={() => (transcript = !transcript)}><Icon name="transcript" /></button>
      <button type="button" class="pbtn txt" class:on={!!explaining} title="Explain the line with Claude (E)" onclick={() => explainLine()}>Explain</button>
      {#if view?.recap?.possible}
        <button type="button" class="pbtn txt" title="Previously on: a recap of {spanLabel(view.recap.fromNumber, view.recap.beforeNumber)} (P)" onclick={openRecap}>Recap</button>
      {/if}
      <span class="menuwrap">
        <button type="button" class="pbtn txt" class:on={subMenu} aria-haspopup="menu" aria-expanded={subMenu} title="Fix the subtitle timing" onclick={() => (subMenu = !subMenu)}>Sync ▾</button>
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

  {#if transcript && !explaining}
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
      <div class="tx-head"><b>Explain Line</b><button type="button" class="ex-close" title="Close (E)" onclick={() => closeExplain()}>Close</button></div>
      <div class="ex-body scroll">
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
        {/if}
      </div>
      <div class="ex-foot">Answered by Claude, through Claude Code{explaining.done && !explaining.error ? " · saved for next time" : ""}</div>
    </aside>
  {/if}

  {#if recap && view && !montage}
    <div class="endov recapov">
      <div class="end-card recap-card">
        <span class="small">Previously on</span>
        <h3>{view.show.title}</h3>
        {#if recap.state === "writing"}
          <p class="recap-text wait">Claude is writing a recap of {spanLabel(view.recap?.fromNumber, view.recap?.beforeNumber)} from their subtitles…</p>
        {:else if recap.state === "error"}
          <p class="recap-text wait">{recap.error}</p>
        {:else if recap.data?.arcs || recap.data?.last}
          <div class="recap-body scroll">
            {#each recap.data.arcs ?? [] as arc, k (k)}
              <section class="arc">
                <div class="arc-head"><b>{arc.title}</b><span>{episodesLabel(arc.episodes)}</span></div>
                <p class="recap-text">{arc.summary}</p>
              </section>
            {/each}
            {#if recap.data.last}
              <section class="arc">
                <div class="arc-head"><b>Last time</b>{#if recap.data.lastNumber}<span>Episode {recap.data.lastNumber}</span>{/if}</div>
                <p class="recap-text">{recap.data.last}</p>
              </section>
            {/if}
          </div>
        {:else}
          <p class="recap-text">{recap.text}</p>
        {/if}
        <div class="end-actions">
          {#if canMontage}
            <button type="button" class="btn quiet" title="Watch the moments (P)" onclick={startMontage}>Watch the moments</button>
          {/if}
          <button type="button" class="btn" title="Start the episode (Enter)" onclick={() => closeRecap(true)}><Icon name="play" /><span>Start Episode {view.episode.number}</span></button>
        </div>
      </div>
    </div>
  {/if}

  {#if montage && moments[montage.i] && view}
    <div class="montage">
      <!-- svelte-ignore a11y_media_has_caption -->
      <video bind:this={montageVideo} src={montageSrc} preload="auto" onloadedmetadata={startMoment} ontimeupdate={montageTick} onerror={() => endMontage(false)}></video>
      <span class="montage-label">Previously on {view.show.title} · Episode {moments[montage.i].number} · {montage.i + 1} of {moments.length}</span>
      <div class="subs montage-subs" style:--sub-size="{subSize}px">
        {#each montageLines as l (l.start + l.text)}
          <p class="sub-line" lang="ja">{@html lineHtml(l)}</p>
        {/each}
      </div>
      <button type="button" class="pbtn txt montage-skip" title="Start the episode (Enter)" onclick={() => endMontage(true)}>Start Episode {view.episode.number}</button>
    </div>
  {/if}

  {#if ended && view}
    <div class="endov">
      <div class="end-card">
        <span class="small">Episode {view.episode.number} done{sat.seconds > 30 ? ` · logged to Tidepool` : ""}</span>
        <h3>{view.next ? `Next: Episode ${view.next.number}` : `That’s the Last Episode Here`}</h3>
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
          <button type="button" class="btn quiet" onclick={() => leave("/")}>Open Library</button>
          {#if view.next}
            <button type="button" class="btn quiet" onclick={stopCountdown} disabled={countdown === null}>Stop</button>
            <button type="button" class="btn" onclick={playNext} disabled={!view.next.ready}><Icon name="play" /><span>Play Now</span></button>
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
  .montage-label { position: absolute; top: 18px; left: 20px; font: 600 13px/1 var(--font-ui); letter-spacing: .02em; color: var(--vid-ink); text-shadow: 0 1px 3px rgba(0, 0, 0, .9); }
  .montage-subs { bottom: 72px; }
  .montage-subs .sub-line { pointer-events: none; }
  .montage-skip { position: absolute; right: 18px; bottom: 18px; background: var(--vid-panel); border: 1px solid var(--vid-edge); }
</style>
