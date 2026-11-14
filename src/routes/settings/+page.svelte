<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import {
    call,
    callKept,
    kept,
    errorText,
    hachidoriInfo,
    hachidoriStart,
    inTauri,
    openHachidoriSettings,
    pickFile,
    pickFolder,
    pickSaveFile,
    relayStatus,
    restartTelly,
    setWindowTitle,
    type Backup,
    type BackupFile,
    type ClaudeStatus,
    type HachidoriInfo,
    type HostStatus,
    type NamesStatus,
    type OwnHachidori,
    type KnownSources, type Settings,
    type MissingRips,
    type ShowMove,
    type Status,
  } from "$lib/api";
  import { STEPS, askAnki, askJimaku, checks, dictionariesCheck, hachidoriCheck, linked as linkedTo, problems, type AnkiFacts } from "$lib/checks";
  import { bytes, plural } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  let settings = $state<Settings>(kept<Settings>("settings") ?? {});
  let status = $state<Status | null>(kept<Status>("status"));
  let hd = $state<HachidoriInfo | null>(null);
  let relay = $state("Off");
  let key = $state(String(kept<Settings>("settings")?.["jimaku.key"] ?? ""));
  let showKey = $state(false);
  let error = $state("");
  let saved = $state<Record<string, boolean>>({});
  let claude = $state<ClaudeStatus | null>(kept<ClaudeStatus>("claudeStatus"));
  let names = $state<NamesStatus | null>(kept<NamesStatus>("namesStatus"));
  let own = $state<OwnHachidori | null>(kept<OwnHachidori>("ownHachidori"));
  /** What Known words chooses from; null while Anki is closed. */
  let sources = $state<KnownSources | null>(kept<KnownSources>("knownSources"));
  /** Whether Jimaku takes the key, and what Anki has, for the list of what's not working. */
  let jimakuWorks = $state<string | null>(null);
  let ankiHas = $state<AnkiFacts | null>(null);
  /** Shows whose rips aren't where Telly has them, and Moved rips' folders and what it found. */
  let away = $state<MissingRips[]>([]);
  let movedFrom = $state("");
  let movedTo = $state("");
  let moved = $state<ShowMove[] | null>(null);

  /** Every part is asked for at once, and shows as it comes. */
  async function load() {
    try {
      await Promise.all([
        callKept<Settings>("settings").then((s) => {
          settings = s;
          key = String(s["jimaku.key"] ?? "");
          askJimaku(s).then((j) => (jimakuWorks = j));
          askAnki(s).then((a) => (ankiHas = a));
        }),
        callKept<Status>("status").then((s) => (status = s)),
        hachidoriInfo().then((h) => (hd = h)),
        relayStatus().then((r) => (relay = r)),
        callKept<NamesStatus>("namesStatus").then((n) => (names = n)),
        callKept<OwnHachidori>("ownHachidori").then((o) => (own = o)),
        callKept<ClaudeStatus>("claudeStatus").then((c) => (claude = c)),
        callKept<KnownSources>("knownSources").then((k) => (sources = k)).catch(() => (sources = null)),
        loadAway(),
      ]);
    } catch (e) {
      error = errorText(e);
    }
  }

  onMount(() => {
    app.dark = false;
    setWindowTitle("Settings — Telly");
    load();
    let unlisten: (() => void) | null = null;
    let left = false;
    if (inTauri) {
      import("@tauri-apps/api/event")
        .then(({ listen }) =>
          listen<HostStatus>("hachidori-status", (e) => {
            if (hd) hd.status = e.payload;
          }),
        )
        // Left before it was listening: it stops at once.
        .then((u) => (left ? u() : (unlisten = u)))
        // Without it, Hachidori's status stays as it was when the page opened.
        .catch(() => {});
    }
    const timer = setInterval(async () => {
      status = await call<Status>("status").catch(() => status);
      relay = await relayStatus().catch(() => relay);
      names = await call<NamesStatus>("namesStatus").catch(() => names);
      own = await call<OwnHachidori>("ownHachidori").catch(() => own);
      if (status?.anki && !ankiHas) ankiHas = await askAnki(settings);
    }, 5000);
    return () => {
      left = true;
      clearInterval(timer);
      unlisten?.();
    };
  });

  async function set(k: string, value: unknown) {
    settings[k] = value;
    try {
      await call("setSetting", { key: k, value });
      saved[k] = true;
      setTimeout(() => (saved[k] = false), 1500);
      // What's not working is checked again for what changed.
      if (k === "jimaku.key") askJimaku(settings).then((j) => (jimakuWorks = j));
      if (k.startsWith("anki.")) askAnki(settings).then((a) => (ankiHas = a));
      if (k === "openrouter.key") callKept<ClaudeStatus>("claudeStatus").then((c) => (claude = c)).catch(() => {});
    } catch (e) {
      toast({ key: "set", state: "failed", title: "Not saved", text: errorText(e) });
    }
  }

  /** A choice of which cards count as known, and how many notes it finds. */
  async function setKnown(k: string, value: unknown) {
    await set(k, value);
    callKept<KnownSources>("knownSources").then((s) => (sources = s)).catch(() => {});
  }

  /** The note types that count: none chosen means every one with the word's field. */
  const knownTypes = $derived(Array.isArray(settings["known.noteTypes"]) ? (settings["known.noteTypes"] as string[]) : []);

  function toggleKnownType(type: string) {
    const all = sources?.noteTypes ?? [];
    const now = knownTypes.length ? knownTypes : all;
    const next = now.includes(type) ? now.filter((t) => t !== type) : [...now, type];
    setKnown("known.noteTypes", next.length === all.length ? [] : next);
  }

  const str = (k: string, d = "") => String(settings[k] ?? d);
  const bool = (k: string, d: boolean) => (typeof settings[k] === "boolean" ? (settings[k] as boolean) : d);

  async function chooseFfmpeg() {
    const path = await pickFile("Choose ffmpeg.exe", ["exe"], "ffmpeg");
    if (!path) return;
    await set("tools.ffmpeg", path);
    try {
      status = await call<Status>("findTools");
    } catch (e) {
      toast({ key: "tools", state: "failed", title: "Couldn’t look for ffmpeg", text: errorText(e) });
    }
  }

  async function lookAgain() {
    try {
      status = await call<Status>("findTools");
      toast({ key: "tools", state: status.tools.ffmpeg ? "done" : "failed", title: status.tools.ffmpeg ? "Found ffmpeg" : "No ffmpeg found", text: status.tools.ffmpeg ?? "Choose ffmpeg.exe yourself." });
    } catch (e) {
      toast({ key: "tools", state: "failed", title: "Couldn’t look for ffmpeg", text: errorText(e) });
    }
  }

  async function kikuFields() {
    try {
      const fields = await call<string[]>("ankiFields", { model: str("anki.noteType", "kiku_typed") });
      const has = (f: string) => fields.includes(f);
      for (const [k, name] of [["anki.field.sentence", "Sentence"], ["anki.field.picture", "Picture"], ["anki.field.audio", "SentenceAudio"], ["anki.field.info", "MiscInfo"]] as const) {
        if (has(name)) await set(k, name);
      }
      toast({ key: "fields", state: "done", title: `${str("anki.noteType", "kiku_typed")} has ${fields.length} fields`, text: fields.join(", ") });
      status = await call<Status>("status");
    } catch (e) {
      toast({ key: "fields", state: "failed", title: "Couldn’t ask Anki", text: errorText(e) });
    }
  }

  async function freeAll() {
    try {
      const r = await call<{ episodes: number; bytes: number }>("freeSpace", { show: null });
      toast({ key: "free", state: "done", title: r.episodes ? `Freed ${bytes(r.bytes)}` : "Nothing to free", text: r.episodes ? `Removed Telly’s copies of ${r.episodes} watched episodes.` : "Only copies of watched episodes are removed." });
    } catch (e) {
      toast({ key: "free", state: "failed", title: "Couldn’t free up space", text: errorText(e) });
    }
    // Asked for again every 5 seconds anyway.
    status = await call<Status>("status").catch(() => status);
  }

  /** Moves Telly's copies of episodes to another folder (null: Telly's own again). */
  async function moveCopies(folder: string | null) {
    try {
      status = await call<Status>("setMediaFolder", { folder });
      settings = await call<Settings>("settings");
      // The title bar takes over from here, so this goes after a few seconds.
      toast({ key: "copies", state: "working", title: "Moving Telly’s copies of episodes", text: "The title bar shows how it’s going." }, 6000);
    } catch (e) {
      toast({ key: "copies", state: "failed", title: "Folder not changed", text: errorText(e) });
    }
  }

  async function chooseCopiesFolder() {
    const folder = await pickFolder("Choose the folder for copies");
    if (folder) await moveCopies(folder);
  }

  /** The folders that shows whose folders aren't there were in, for "They were in". */
  const oldFolders = $derived([...new Set(away.filter((a) => a.folderMissing).map((a) => a.folder.replace(/[\\/][^\\/]*[\\/]?$/, "")))]);

  async function loadAway() {
    away = await call<MissingRips[]>("missingRips").catch(() => away);
    if (!movedFrom && oldFolders.length === 1) movedFrom = oldFolders[0];
  }

  async function chooseMoved(which: "from" | "to") {
    const folder = await pickFolder(which === "from" ? "Choose the folder your rips were in" : "Choose the folder your rips are in now");
    if (!folder) return;
    if (which === "from") movedFrom = folder;
    else movedTo = folder;
    moved = null;
  }

  /** Shows what Telly finds of the rips of every show in "They were in", in "They’re in now", before anything changes. */
  async function findMoved() {
    try {
      moved = await call<ShowMove[]>("relinkLibrary", { from: movedFrom, to: movedTo });
    } catch (e) {
      moved = null;
      toast({ key: "moved", state: "failed", title: "Couldn’t find them", text: errorText(e) });
    }
  }

  async function useMoved() {
    try {
      const done = await call<ShowMove[]>("relinkLibrary", { from: movedFrom, to: movedTo, apply: true });
      const found = done.reduce((n, m) => n + m.found.length, 0);
      const shows = done.filter((m) => m.found.length).length;
      toast({ key: "moved", state: "done", title: `Found ${plural(found, "episode")} of ${plural(shows, "show")}`, text: movedTo });
      moved = null;
    } catch (e) {
      toast({ key: "moved", state: "failed", title: "Nothing was changed", text: errorText(e) });
    }
    loadAway();
  }

  /** Just restored from a backup whose shows' rips aren't where it has them (routes/+page.ts sends you here). */
  const restored = page.url.searchParams.has("restored");

  /** Backing up, and the backup chosen to restore once Telly has read what's in it. */
  let backingUp = $state(false);
  let restoring = $state<BackupFile | null>(null);
  /** "unpacking" while Telly unpacks it; "restart" once only a restart is left to do. */
  let restoreStep = $state<"" | "unpacking" | "restart">("");
  const madeOn = new Intl.DateTimeFormat(undefined, { dateStyle: "long", timeStyle: "short" });

  async function backUp() {
    const d = new Date();
    const day = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
    const path = await pickSaveFile("Save a backup of Telly", ["zip"], "Telly backup", `Telly backup ${day}.zip`);
    if (!path) return;
    backingUp = true;
    try {
      const made = await call<Backup & { path: string; bytes: number }>("backUp", { path });
      toast({ key: "backup", state: "done", title: `Backed up ${plural(made.shows, "show")} (${bytes(made.bytes)})`, text: made.path });
    } catch (e) {
      toast({ key: "backup", state: "failed", title: "Not backed up", text: errorText(e) });
    }
    backingUp = false;
  }

  /** Reads what's in a backup, for Restore and restart to confirm. */
  async function chooseBackup() {
    const path = await pickFile("Choose a backup of Telly", ["zip"], "Telly backup");
    if (!path) return;
    try {
      restoring = await call<BackupFile>("readBackup", { path });
      restoreStep = "";
    } catch (e) {
      toast({ key: "restore", state: "failed", title: "Can’t restore this", text: errorText(e) });
    }
  }

  /** Unpacks the backup beside the library, and starts Telly again: it takes the library's place as Telly starts. */
  async function restore() {
    if (!restoring) return;
    restoreStep = "unpacking";
    try {
      await call("restoreBackup", { path: restoring.path });
    } catch (e) {
      restoreStep = "";
      toast({ key: "restore", state: "failed", title: "Nothing was restored", text: errorText(e) });
      return;
    }
    restoreStep = "restart";
    await restartTelly().catch(() => {});
  }

  const movedFound = $derived(moved?.reduce((n, m) => n + m.found.length, 0) ?? 0);
  const movedTotal = $derived(moved?.reduce((n, m) => n + m.found.length + m.missing.length, 0) ?? 0);
  const movedShows = $derived(moved?.filter((m) => m.found.length).length ?? 0);

  async function chooseDictionaries() {
    const folder = await pickFolder("Choose the dictionaries folder");
    if (!folder) return;
    await set("hachidori.dictionaries", folder);
    own = await call<OwnHachidori>("ownHachidori").catch(() => own);
  }

  const linked = $derived(linkedTo(hd));

  const dictText = $derived(own ? dictionariesCheck(own, hd).text : "");

  const ankiText = $derived.by(() => {
    if (!own || linked) return "";
    if (own.ankiError) return `Hachidori’s Anki setup: ${own.ankiError}`;
    return own.deck
      ? `Hachidori adds cards to ${own.deck}, as ${own.model}.`
      : `Hachidori adds cards to the show’s deck once a show is playing, as ${own.model}.`;
  });

  async function restartHachidori() {
    try {
      await hachidoriStart();
      hd = await hachidoriInfo();
    } catch (e) {
      toast({ key: "hd", state: "failed", title: "Couldn’t restart Hachidori", text: errorText(e) });
    }
  }

  const hdText = $derived(hachidoriCheck(hd).text);

  /** What isn't working, found by the setup's own checks, once enough is known to say. */
  const broken = $derived(status && claude && (hd || !inTauri) ? problems(checks({ settings, status, jimaku: jimakuWorks, anki: ankiHas, own, hd, claude })) : null);

  const relayText = $derived(
    ({ Off: "Off", Running: "Relaying on 127.0.0.1:8771 while Anki is closed", Anki: "Anki is open, so Hachidori Relay does it", PortBusy: "Another program holds port 8771" } as Record<string, string>)[relay] ?? relay,
  );

  async function remakeNames() {
    try {
      names = await call<NamesStatus>("rebuildNames");
      // The title bar takes over from here, so this goes after a few seconds.
      toast({ key: "names", state: "working", title: "Making the character dictionary again", text: "Every cast is fetched from AniList again. The title bar shows how it’s going." }, 6000);
    } catch (e) {
      toast({ key: "names", state: "failed", title: "Couldn’t make the character dictionary again", text: errorText(e) });
    }
  }

  const namesText = $derived.by(() => {
    const n = names;
    if (!n) return "";
    if (!n.enabled) return "Off.";
    if (n.working) return `${n.working.label}…`;
    if (n.buildError) return `Couldn’t make it: ${n.buildError}`;
    if (!n.summary) return "Starts with the first show watched.";
    const what = `${n.summary.characters} characters from ${n.summary.shows.join(", ")} (${bytes(n.summary.bytes)})`;
    if (n.imported) return `In Hachidori${n.importedTo && n.importedTo !== "telly" ? ` in ${n.importedTo}` : ""}: ${what}.`;
    if (n.error) return `Made, but Hachidori didn’t take it yet: ${n.error}. Telly tries again every 5 minutes.`;
    return `Made: ${what}. Waiting for Hachidori to take it (it goes to the browser’s Hachidori once they’re linked).`;
  });

  /** whisper.cpp's multilingual models, smallest first. */
  const WHISPER_MODELS = [
    ["ggml-small-q5_1.bin", "Small · 190 MB"],
    ["ggml-medium-q5_0.bin", "Medium · 539 MB"],
    ["ggml-large-v3-turbo-q5_0.bin", "Large v3 turbo · 574 MB"],
    ["ggml-large-v3-q5_0.bin", "Large v3 · 1.1 GB"],
  ];

  async function chooseWhisperModel(file: string) {
    await set("whisper.model", file);
    // Asked for again every 5 seconds anyway.
    status = await call<Status>("status").catch(() => status);
  }

  async function getWhisperModel() {
    try {
      await call("getWhisperModel");
      // The title bar takes over from here, so this goes after a few seconds.
      toast({ key: "whispermodel", state: "working", title: "Getting Whisper’s Japanese model", text: "The title bar shows how it’s going." }, 6000);
    } catch (e) {
      toast({ key: "whispermodel", state: "failed", title: "Couldn’t get Whisper’s model", text: errorText(e) });
    }
  }

  /** OpenRouter's models for making subtitles, and who each one's are marked as made by. The first is used until one is picked. */
  const ROUTER_MODELS = [
    ["microsoft/mai-transcribe-2", "MAI-Transcribe 2", "MAI-Transcribe"],
    ["elevenlabs/scribe-v2", "ElevenLabs Scribe v2", "ElevenLabs"],
    ["openai/whisper-large-v3", "Whisper large v3", "Whisper"],
  ];
  let showRouterKey = $state(false);
  const routerMaker = $derived((ROUTER_MODELS.find(([id]) => id === str("openrouter.model")) ?? ROUTER_MODELS[0])[2]);

  /** Telly's app on AniList, whose page shows an access token to paste here. */
  const ANILIST_CLIENT_ID = "56672";
  let anilistToken = $state("");

  async function anilistSignIn() {
    try {
      await call("anilistSignIn", { token: anilistToken.trim() });
      anilistToken = "";
      settings = await call<Settings>("settings");
    } catch (e) {
      toast({ key: "anilist", state: "failed", title: "Not signed in", text: errorText(e) });
    }
  }

  async function anilistSignOut() {
    try {
      await call("anilistSignOut");
      settings = await call<Settings>("settings");
    } catch (e) {
      toast({ key: "anilist", state: "failed", title: "Not signed out", text: errorText(e) });
    }
  }

  async function openLink(link: string) {
    try {
      if (inTauri) {
        const { openUrl } = await import("@tauri-apps/plugin-opener");
        await openUrl(link);
      } else window.open(link);
    } catch (e) {
      toast({ key: "link", state: "failed", title: "Couldn’t open the browser", text: errorText(e) });
    }
  }
</script>

<div class="set scroll">
  <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Back to library</span></button>
  <h1>Settings</h1>
  {#if error}<p class="err">{error}</p>{/if}

  <section>
    <h2>Setup</h2>
    {#if !broken}
      <p class="lede">Checking what works…</p>
    {:else if broken.length}
      <p class="lede">What’s not working:</p>
      <div class="status-list">
        {#each broken as c (c.id)}
          <p><i class:warn={c.state === "warn"} class:bad={c.state === "bad"}></i><span>{c.text} <button type="button" class="link-btn" onclick={() => goto(`/setup?step=${c.step}`)}>{STEPS.find((s) => s.id === c.step)?.setUp}</button></span></p>
        {/each}
      </div>
    {:else}
      <p class="lede">Everything Telly checks is working.</p>
    {/if}
    <div class="actions">
      <button type="button" class="btn quiet" onclick={() => goto("/setup")}>Run setup again</button>
      <button type="button" class="btn quiet" onclick={load}>Check again</button>
    </div>
  </section>

  <section>
    <h2>Subtitles</h2>
    <div class="rows">
      <label class="field">
        <span>Jimaku API key</span>
        <span class="inline">
          <input class="input mono grow" type={showKey ? "text" : "password"} bind:value={key} onchange={() => set("jimaku.key", key.trim())} />
          <button type="button" class="btn quiet" onclick={() => (showKey = !showKey)}>{showKey ? "Hide" : "Show"}</button>
          <button type="button" class="btn quiet" onclick={() => openLink("https://jimaku.cc/account")}>Get a key</button>
        </span>
        <small>{saved["jimaku.key"] ? "Saved. Telly is looking for subtitles again." : "A free Jimaku account comes with a key. Telly uses it to find each episode’s subtitles and sync them to the rips."}</small>
      </label>
      <label class="field">
        <span>Subtitle size</span>
        <select class="input" value={str("player.subtitleSize", "36")} onchange={(e) => set("player.subtitleSize", Number(e.currentTarget.value))}>
          {#each [28, 32, 36, 40, 44, 50] as n}<option value={String(n)}>{n} pixels</option>{/each}
        </select>
      </label>
    </div>
  </section>

  <section>
    <h2>Cards</h2>
    <p class="lede">Hachidori makes the card as usual. Telly then adds a moving picture of the line and its audio, and fills only fields that are empty.</p>
    <div class="rows two">
      <label class="field"><span>AnkiConnect address</span><input class="input mono" value={str("anki.url", "http://127.0.0.1:8765")} onchange={(e) => set("anki.url", e.currentTarget.value.trim())} /></label>
      <label class="field"><span>Note type</span>
        <span class="inline"><input class="input grow" value={str("anki.noteType", "kiku_typed")} onchange={(e) => set("anki.noteType", e.currentTarget.value.trim())} /><button type="button" class="btn quiet" onclick={kikuFields}>Check its fields</button></span>
      </label>
      <label class="field"><span>Deck</span><input class="input" value={str("anki.deck", "例文マイニング")} onchange={(e) => set("anki.deck", e.currentTarget.value.trim())} title={"{show} is the title of the show being watched"} /></label>
      <label class="field"><span>Sentence field</span><input class="input" value={status?.fields.sentence ?? "Sentence"} onchange={(e) => set("anki.field.sentence", e.currentTarget.value.trim())} /></label>
      <label class="field"><span>Picture field</span><input class="input" value={status?.fields.picture ?? "Picture"} onchange={(e) => set("anki.field.picture", e.currentTarget.value.trim())} /></label>
      <label class="field"><span>Audio field</span><input class="input" value={status?.fields.audio ?? "SentenceAudio"} onchange={(e) => set("anki.field.audio", e.currentTarget.value.trim())} /></label>
      <label class="field"><span>Source field</span><input class="input" value={status?.fields.info ?? "MiscInfo"} onchange={(e) => set("anki.field.info", e.currentTarget.value.trim())} /></label>
    </div>
    <div class="field">
      <span>Extra around the line</span>
      <div class="seg" role="group" aria-label="Clip length">
        {#each [["250", "250 milliseconds"], ["500", "500 milliseconds"], ["prev", "From the line before"]] as [v, label]}
          <button type="button" aria-pressed={str("clip.padding", "250") === v} onclick={() => set("clip.padding", v)}>{label}</button>
        {/each}
      </div>
    </div>
    <div class="status-list">
      <p><i class:ok={status?.anki} class:bad={status && !status.anki}></i><span>{status?.anki ? `Anki is open (AnkiConnect at ${status.ankiUrl})` : "Anki isn’t open, or AnkiConnect isn’t installed"}</span></p>
      <p><i class:ok={status?.mining?.beacon} class:warn={status?.mining && !status.mining.beacon}></i>
        <span>{#if status?.mining?.beacon}Anki Beacon is telling Telly about new cards{:else if status?.mining?.listening}Listening for Anki Beacon at <code>{status.beaconAddress}</code>. Add that address to Beacon’s config in Anki (Tools → Add-ons → Anki Beacon → Config). Until then, Telly asks AnkiConnect every 2 seconds during playback.{:else}{status?.mining?.listenError ?? "Not listening for Anki Beacon"}{/if}</span>
      </p>
      <p><i class:ok={status?.tools.canClip} class:bad={status && !status.tools.canClip}></i><span>{status?.tools.canClip ? "ffmpeg can make animated AVIF and Opus" : "This ffmpeg can’t make AVIF and Opus (it needs libsvtav1 and libopus)"}</span></p>
    </div>
  </section>

  <section>
    <h2>Known words</h2>
    <p class="lede">A word is known when Anki has a card for it that counts. Telly reads these cards and never changes them.</p>
    {#if sources}
      <div class="rows two">
        <label class="field"><span>Deck</span>
          <select class="input" value={str("known.deck")} onchange={(e) => setKnown("known.deck", e.currentTarget.value)}>
            <option value="">Every deck</option>
            {#each sources.decks as deck (deck)}<option value={deck}>{deck}</option>{/each}
          </select>
        </label>
        <label class="field"><span>Word field</span><input class="input" value={str("known.field") || sources.field} onchange={(e) => setKnown("known.field", e.currentTarget.value.trim())} /></label>
      </div>
      <div class="field">
        <span>Note types</span>
        {#each sources.noteTypes as type (type)}
          <label class="check"><input type="checkbox" checked={knownTypes.length === 0 || knownTypes.includes(type)} onchange={() => toggleKnownType(type)} /><span>{type}</span></label>
        {/each}
      </div>
      <div class="field">
        <span>A card counts when it’s</span>
        <div class="seg" role="group" aria-label="A card counts when it’s">
          {#each [["studied", "Studied"], ["mature", "Mature"]] as [value, label] (value)}
            <button type="button" aria-pressed={str("known.when", "studied") === value} onclick={() => setKnown("known.when", value)}>{label}</button>
          {/each}
        </div>
        <small class="muted">Studied: reviewed at least once. Mature: its next review is 21 days or more away, as Anki counts it.</small>
      </div>
      <p class="muted">{sources.studied.toLocaleString("en-GB")} {sources.studied === 1 ? "note" : "notes"} studied, {sources.mature.toLocaleString("en-GB")} of them mature. {#if sources.suspended}{sources.suspended.toLocaleString("en-GB")} {sources.suspended === 1 ? "is suspended and doesn’t" : "are suspended and don’t"} count.{/if}</p>
    {:else}
      <p class="muted">Open Anki to choose which cards count.</p>
    {/if}
  </section>

  <section>
    <h2>Hachidori</h2>
    <p class="lede">{hdText}</p>
    <div class="rows">
      <label class="check"><input type="checkbox" checked={bool("hachidori.link", false)} onchange={async (e) => { await set("hachidori.link", e.currentTarget.checked); restartHachidori(); }} /><span>Link to the Hachidori in the browser<small>Dictionaries, settings, and card format come from it, through Hachidori Relay.</small></span></label>
      <label class="check"><input type="checkbox" checked={bool("hachidori.relay", true)} onchange={(e) => set("hachidori.relay", e.currentTarget.checked)} /><span>Run Telly’s own relay while Anki is closed<small>{relayText}</small></span></label>
    </div>
    <div class="field">
      <span>Dictionaries</span>
      <span class="inline"><input class="input grow mono" value={own?.folder ?? ""} placeholder="C:\Users\you\Documents\dictionaries" onchange={async (e) => { await set("hachidori.dictionaries", e.currentTarget.value.trim()); own = await call<OwnHachidori>("ownHachidori").catch(() => own); }} /><button type="button" class="btn quiet" onclick={chooseDictionaries}>Choose folder…</button></span>
      {#if dictText}<small class="muted">{dictText}</small>{/if}
      {#if ankiText}<small class="muted">{ankiText}</small>{/if}
    </div>
    <div class="actions">
      <button type="button" class="btn quiet" onclick={() => openHachidoriSettings().catch((e) => toast({ key: "hd", state: "failed", title: "Hachidori", text: errorText(e) }))} disabled={!hd?.prepared}>Open Hachidori Settings</button>
      <button type="button" class="btn quiet" onclick={restartHachidori} disabled={!hd?.prepared}>Restart Hachidori</button>
      {#if hd?.version}<span class="muted">Hachidori {hd.version}</span>{/if}
    </div>
  </section>

  <section>
    <h2>Character names</h2>
    <p class="lede">Telly makes a dictionary of the characters in the shows being watched, from AniList, the way characterdictionary.tokyo does, and adds it to Hachidori as “Telly characters”. Hovering a name shows who it is, with their picture and how they’re being addressed. Their names are also coloured in the subtitles.</p>
    <p class="lede">{namesText}</p>
    <div class="rows">
      <label class="check"><input type="checkbox" checked={bool("names.enabled", true)} onchange={async (e) => { await set("names.enabled", e.currentTarget.checked); names = await call<NamesStatus>("namesStatus").catch(() => names); }} /><span>Make the character dictionary<small>The casts of the six most recently watched shows.</small></span></label>
      <label class="check"><input type="checkbox" checked={bool("names.spoilers", false)} onchange={(e) => set("names.spoilers", e.currentTarget.checked)} /><span>Include spoilers<small>AniList marks some of a character’s description and names as spoilers. They’re left out unless this is on.</small></span></label>
      <label class="check"><input type="checkbox" checked={bool("player.names", true)} onchange={(e) => set("player.names", e.currentTarget.checked)} /><span>Colour characters’ names in the subtitles</span></label>
    </div>
    <div class="actions">
      <button type="button" class="btn quiet" onclick={remakeNames} disabled={!names?.enabled}>Rebuild dictionary</button>
    </div>
  </section>

  <section>
    <h2>Claude</h2>
    <p class="lede">
      {#if claude?.openrouter}
        <b>Explain</b> (E) explains the line on screen, and <b>Previously on</b> (P) recaps the story so far, with its key moments to watch again. Claude Code isn’t on this computer, so both use gpt-oss on OpenRouter, with the OpenRouter key under Machine-made subtitles, and the subtitles they’re about are sent to it. Answers are saved, so asking again is instant.
      {:else if claude?.available}
        <b>Explain</b> (E) explains the line on screen, and <b>Previously on</b> (P) recaps the story so far, with its key moments to watch again. Both use the Claude Code on this computer, with its own sign-in ({claude.version}). Answers are saved, so asking again is instant.
      {:else if claude}
        <b>Explain</b> and <b>Previously on</b> use Claude Code, and Telly can’t find it: {claude.detail} Install Claude Code, sign in, and come back here, or add an OpenRouter API key under Machine-made subtitles and gpt-oss on OpenRouter answers instead.
      {:else}
        Looking for Claude Code…
      {/if}
    </p>
    <div class="rows two">
      <label class="field"><span>Model</span>
        <select class="input" value={str("claude.model", "")} onchange={(e) => set("claude.model", e.currentTarget.value)} disabled={claude?.openrouter}>
          <option value="">Haiku 5.5 (fastest, cheapest)</option>
          <option value="claude-sonnet-5-5">Sonnet 5.5</option>
          <option value="claude-opus-5-5">Opus 5.5</option>
          <option value="default">Claude Code’s default</option>
        </select>
      </label>
      <label class="field"><span>Answer language</span>
        <select class="input" value={str("claude.language", "en")} onchange={(e) => set("claude.language", e.currentTarget.value)}>
          <option value="en">English</option>
          <option value="ja">Simple Japanese</option>
        </select>
      </label>
    </div>
    <p class="muted">Telly keeps one Claude Code started while the player is open, so Explain answers in a second or two with Haiku. Bigger models think longer before answering.</p>
    <div class="rows">
      <label class="check"><input type="checkbox" checked={bool("recap.enabled", true)} onchange={(e) => set("recap.enabled", e.currentTarget.checked)} /><span>Show “Previously on” after time away from a show<small>It’s written when the player closes, so it’s ready next time.</small></span></label>
      <label class="field narrow"><span>Time away</span>
        <select class="input" value={String(settings["recap.afterHours"] ?? 8)} onchange={(e) => set("recap.afterHours", Number(e.currentTarget.value))} disabled={!bool("recap.enabled", true)}>
          {#each [[1, "1 hour"], [4, "4 hours"], [8, "8 hours"], [24, "1 day"], [72, "3 days"]] as [h, label]}<option value={String(h)}>{label} or more</option>{/each}
        </select>
      </label>
    </div>
  </section>

  <section>
    <h2>Playing</h2>
    <div class="rows">
      <label class="check"><input type="checkbox" checked={bool("player.autoplay", true)} onchange={(e) => set("player.autoplay", e.currentTarget.checked)} /><span>Play the next episode when one ends<small>After an 8-second countdown. Esc stops it.</small></span></label>
      <label class="check"><input type="checkbox" checked={bool("player.skipEndings", false)} onchange={(e) => set("player.skipEndings", e.currentTarget.checked)} /><span>Skip endings<small>The countdown starts when the ending does, from the disc’s chapters, else where AniSkip’s users marked it.</small></span></label>
      <label class="field narrow"><span>Convert ahead</span>
        <select class="input" value={String(settings["convert.ahead"] ?? 10)} onchange={(e) => set("convert.ahead", Number(e.currentTarget.value))}>
          {#each [3, 5, 10, 20, 50] as n}<option value={String(n)}>{n} episodes</option>{/each}
        </select>
        <small>Episodes WebView2 can’t play are converted this far ahead of the one playing.</small>
      </label>
      <label class="check"><input type="checkbox" checked={bool("convert.rest", false)} onchange={(e) => set("convert.rest", e.currentTarget.checked)} /><span>Convert the rest while nothing plays<small>Uses more disk space, so nothing waits later.</small></span></label>
    </div>
  </section>

  <section>
    <h2>Games</h2>
    <div class="rows">
      <label class="check"><input type="checkbox" checked={bool("games.wait", true)} onchange={(e) => set("games.wait", e.currentTarget.checked)} /><span>Wait while a game is running<small>No episode starts converting, getting its subtitles or its seek bar’s pictures until the game closes, so the game has the processor and the drive. A game is a program in a Steam, Epic, GOG, Riot or Xbox games folder, or World of Warcraft.</small></span></label>
      <label class="field"><span>More games</span><input class="input mono" value={str("games.names")} placeholder="ffxiv_dx11.exe, eldenring.exe" onchange={(e) => set("games.names", e.currentTarget.value.trim())} /><small>Program names, separated by commas, as Task Manager’s Details tab lists them.</small></label>
    </div>
  </section>

  <section>
    <h2>Machine-made subtitles</h2>
    {#if str("openrouter.key")}
      <p class="lede">Ready. On a show’s page, <b>Make subtitles with {routerMaker}</b> covers the episodes Jimaku has nothing for. They’re marked as machine-made.</p>
    {:else if !status?.tools.whisper}
      <p class="lede">Whisper can make subtitles for episodes Jimaku has nothing for. Telly uses the whisper.cpp Tidepool downloads for voice input, and Tidepool hasn’t downloaded it on this computer yet.</p>
    {:else if status.tools.whisperModel}
      <p class="lede">Ready. On a show’s page, <b>Make subtitles with Whisper</b> covers the episodes Jimaku has nothing for. They’re marked as machine-made.</p>
    {:else}
      <p class="lede">Whisper can make subtitles for episodes Jimaku has nothing for, marked as machine-made. It needs a model that knows Japanese, from whisper.cpp’s own models.</p>
    {/if}
    <label class="field">
      <span>OpenRouter API key</span>
      <span class="inline">
        <input class="input mono grow" type={showRouterKey ? "text" : "password"} value={str("openrouter.key")} onchange={(e) => set("openrouter.key", e.currentTarget.value.trim())} />
        <button type="button" class="btn quiet" onclick={() => (showRouterKey = !showRouterKey)}>{showRouterKey ? "Hide" : "Show"}</button>
        <button type="button" class="btn quiet" onclick={() => openLink("https://openrouter.ai/settings/keys")}>Get a key</button>
      </span>
      <small>With a key, a model on OpenRouter makes the subtitles, and each episode’s audio is sent to it. Without one, Whisper on this computer does. Without Claude Code on this computer, gpt-oss on OpenRouter also answers Explain and Previously on.</small>
    </label>
    {#if str("openrouter.key")}
      <label class="field narrow"><span>Model</span>
        <select class="input" value={str("openrouter.model", ROUTER_MODELS[0][0])} onchange={(e) => set("openrouter.model", e.currentTarget.value)}>
          {#each ROUTER_MODELS as [id, label]}<option value={id}>{label}</option>{/each}
        </select>
        <small>A 24-minute episode costs about $0.04 with MAI-Transcribe, $0.09 with ElevenLabs and $0.01 with Whisper.</small>
      </label>
    {:else if status?.tools.whisper}
      <label class="field narrow"><span>Model</span>
        <select class="input" value={str("whisper.model", WHISPER_MODELS[0][0])} onchange={(e) => chooseWhisperModel(e.currentTarget.value)}>
          {#each WHISPER_MODELS as [file, label]}<option value={file}>{label}</option>{/each}
        </select>
        <small>Bigger models take longer for each episode.</small>
      </label>
      {#if status.tools.whisperModel}
        <p class="muted mono path">{status.tools.whisperModel}</p>
      {:else}
        <div class="actions">
          <button type="button" class="btn quiet" onclick={getWhisperModel}>Get model</button>
        </div>
      {/if}
    {/if}
  </section>

  <section>
    <h2>ffmpeg</h2>
    {#if status?.tools.ffmpeg}
      <p class="lede">Using <code>{status.tools.ffmpeg}</code><br /><span class="muted">{status.tools.version} · converts with {status.tools.encoder}</span></p>
    {:else}
      <p class="lede">Telly can’t find ffmpeg. It looks on PATH and where GameSentenceMiner keeps it. Choose ffmpeg.exe (its folder needs ffprobe.exe too).</p>
    {/if}
    <div class="actions">
      <button type="button" class="btn quiet" onclick={chooseFfmpeg}>Choose ffmpeg.exe…</button>
      <button type="button" class="btn quiet" onclick={lookAgain}>Look again</button>
      {#if str("tools.ffmpeg")}<button type="button" class="link-btn" onclick={async () => { await set("tools.ffmpeg", null); lookAgain(); }}>Forget my choice</button>{/if}
    </div>
  </section>

  <section>
    <h2>Disk space</h2>
    <p class="lede">Telly’s copies of episodes use {bytes(status?.spaceUsed ?? 0)}. The rips stay where they are.</p>
    <div class="actions">
      <button type="button" class="btn quiet" onclick={freeAll}>Free up space</button>
      <span class="muted">Removes Telly’s copies of watched episodes.</span>
    </div>
    <div class="field">
      <span>Copies folder</span>
      <span class="inline"><input class="input grow mono" value={status?.mediaDir ?? ""} readonly aria-label="Copies folder" /><button type="button" class="btn quiet" onclick={chooseCopiesFolder}>Choose folder…</button></span>
      {#if str("media.folder")}<span><button type="button" class="link-btn" onclick={() => moveCopies(null)}>Use Telly’s own folder</button></span>{/if}
    </div>
    <p class="muted">The library and subtitles stay in:</p>
    <p class="muted mono path">{status?.dataDir}</p>
  </section>

  <section id="moved">
    <h2>Moved rips</h2>
    {#if restored && away.length}
      <p class="lede"><b>Your library is back.</b> The rips of {plural(away.length, "show")} aren’t where they were when the backup was made. Say where they are now here, or with Find its folder… on each show’s page.</p>
    {/if}
    <p class="lede">If your rips moved to another folder or drive, or the drive’s letter changed, Telly can find them there and keep where you were, what you watched, your cards and subtitles. It matches each episode to a file in the new folder; the rips aren’t moved or changed.</p>
    {#if away.length}
      <p class="muted">Not where they were: {away.map((a) => a.title).join(", ")}</p>
    {/if}
    <div class="rows two">
      <div class="field">
        <span>They were in</span>
        <span class="inline"><input class="input grow mono" list="moved-from" bind:value={movedFrom} oninput={() => (moved = null)} placeholder="D:\Anime" aria-label="The folder your rips were in" /><button type="button" class="btn quiet" onclick={() => chooseMoved("from")}>Choose…</button></span>
      </div>
      <div class="field">
        <span>They’re in now</span>
        <span class="inline"><input class="input grow mono" bind:value={movedTo} oninput={() => (moved = null)} placeholder="E:\Anime" aria-label="The folder your rips are in now" /><button type="button" class="btn quiet" onclick={() => chooseMoved("to")}>Choose…</button></span>
      </div>
    </div>
    <datalist id="moved-from">
      {#each oldFolders as folder (folder)}<option value={folder}></option>{/each}
    </datalist>
    {#if moved}
      <div class="moved">
        <p><b>{movedFound} of {plural(movedTotal, "episode")} found</b>, in {plural(moved.length, "show")}</p>
        <ul class="moved-list">
          {#each moved as m (m.showId)}
            <li>
              <span>{m.title}</span>
              <span class="right">{m.found.length} of {m.found.length + m.missing.length}</span>
              <span class="muted mono path">{m.to}</span>
              {#if m.missing.length}<span class="muted">Not found, left as they are: {m.missing.length === 1 ? "episode" : "episodes"} {m.missing.map((x) => x.number).join(", ")}</span>{/if}
            </li>
          {/each}
        </ul>
        <div class="actions">
          <button type="button" class="btn" disabled={!movedShows} onclick={useMoved}>Use the new folder for {plural(movedShows, "show")}</button>
          <button type="button" class="btn quiet" onclick={() => (moved = null)}>Cancel</button>
        </div>
      </div>
    {:else}
      <div class="actions">
        <button type="button" class="btn quiet" disabled={!movedFrom.trim() || !movedTo.trim()} onclick={findMoved}>Find them</button>
      </div>
    {/if}
  </section>

  <section>
    <h2>AniList</h2>
    {#if str("anilist.user")}
      <p class="lede">Signed in as {str("anilist.user")}. Episodes watched from now on go on your AniList list.</p>
      {#if str("anilist.error")}<p class="err">Your list wasn’t updated: {str("anilist.error")}. Telly tries again every 10 minutes.</p>{/if}
      <div class="actions"><button type="button" class="btn quiet" onclick={anilistSignOut}>Sign out</button></div>
    {:else}
      <label class="field">
        <span>Access token</span>
        <span class="inline">
          <input class="input mono grow" type="password" bind:value={anilistToken} />
          <button type="button" class="btn quiet" onclick={() => openLink(`https://anilist.co/api/v2/oauth/authorize?client_id=${ANILIST_CLIENT_ID}&response_type=token`)}>Get a token</button>
          <button type="button" class="btn" onclick={anilistSignIn} disabled={!anilistToken.trim()}>Sign in</button>
        </span>
        <small>Signed in, each episode watched in Telly goes on your AniList list.</small>
      </label>
    {/if}
  </section>

  <section>
    <h2>Tidepool</h2>
    <p class="lede">Watching is logged for Tidepool’s Japanese log: each sitting, and the lines and characters that played, never the text itself.</p>
    <p class="muted mono path">{status?.logPath}</p>
  </section>

  <section id="backup">
    <h2>Backup</h2>
    <p class="lede">A backup is one file with what Telly can’t make again: your shows, where you are in each and what you watched, your settings, the subtitles and your nudges to their timing, Tidepool’s log, the words you know and your AniList list. Restore it on a new computer or after reinstalling. Telly’s copies of episodes, covers and the seek bar’s pictures aren’t in it, as Telly makes them again, and nor are your rips.</p>
    <p class="lede">It holds your Jimaku and OpenRouter keys and your AniList token, so keep it where only you can open it.</p>
    {#if restoring}
      <div class="moved">
        <p><b>Made by Telly {restoring.telly} on {madeOn.format(restoring.madeAt)}</b>, with {plural(restoring.shows, "show")} and {plural(restoring.episodes, "episode")}</p>
        <span class="muted mono path">{restoring.path}</span>
        {#if restoreStep === "restart"}
          <p>{inTauri ? "Restart Telly to finish restoring." : "Restart the dev server to finish restoring."}</p>
        {:else}
          <p>Restoring puts this library in place of the one here{restoring.showsHere ? ` (${plural(restoring.showsHere, "show")})` : ""}, and Telly restarts into it. Your rips stay as they are. If they aren’t where they were when the backup was made, Telly asks where they are now.</p>
          <div class="actions">
            <button type="button" class="btn" disabled={restoreStep === "unpacking"} onclick={restore}>{restoreStep === "unpacking" ? "Restoring…" : "Restore and restart"}</button>
            <button type="button" class="btn quiet" disabled={restoreStep === "unpacking"} onclick={() => (restoring = null)}>Cancel</button>
          </div>
        {/if}
      </div>
    {:else}
      <div class="actions">
        <button type="button" class="btn quiet" disabled={backingUp} onclick={backUp}>{backingUp ? "Backing up…" : "Back up…"}</button>
        <button type="button" class="btn quiet" onclick={chooseBackup}>Restore…</button>
      </div>
    {/if}
  </section>
</div>

<style>
  .set { position: absolute; inset: 0; overflow: auto; padding: 18px 32px 60px; }
  h1 { font: 600 28px/1.2 var(--font-display); margin: 14px 0 8px; }
  section { max-width: 820px; display: grid; gap: 14px; padding: 22px 0; border-bottom: 1px solid var(--rule); }
  h2 { font: 600 17px/1.2 var(--font-ui); }
  .lede { color: var(--ink-2); font-size: 14px; max-width: 70ch; }
  .rows { display: grid; gap: 14px; }
  .rows.two { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .inline { display: flex; gap: 8px; align-items: center; }
  .field small { color: var(--ink-3); font-size: 12.5px; }
  .field.narrow { max-width: 360px; }
  select.input { height: 36px; }
  .actions { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .muted { color: var(--ink-3); font-size: 12.5px; }
  .path { overflow-wrap: anywhere; }
  .status-list { display: grid; gap: 6px; font-size: 13.5px; color: var(--ink-2); }
  .status-list p { display: flex; gap: 9px; align-items: baseline; }
  .status-list i { width: 8px; height: 8px; border-radius: 50%; background: var(--rule-2); flex: none; transform: translateY(-1px); }
  .status-list i.ok { background: var(--ok-dot); }
  .status-list i.warn { background: var(--warn-dot); }
  .status-list i.bad { background: var(--bad-dot); }
  .moved { display: grid; gap: 10px; font-size: 13.5px; }
  .moved-list { display: grid; margin: 0; padding: 0; border-top: 1px solid var(--rule); }
  .moved-list li { list-style: none; display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 2px 12px; padding: 8px 2px; border-bottom: 1px solid var(--rule); }
  .moved-list .right { justify-self: end; font-variant-numeric: tabular-nums; }
  .moved-list .muted { grid-column: 1 / -1; }
  code { background: var(--sunk); padding: .1em .36em; border-radius: 4px; overflow-wrap: anywhere; }
</style>
