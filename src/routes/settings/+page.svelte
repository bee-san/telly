<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount, tick } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import LibrarySheet from "$lib/LibrarySheet.svelte";
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
    type DownloadsView,
    type HachidoriInfo,
    type HostStatus,
    type NamesStatus,
    type OwnHachidori,
    type KnownSources, type Settings,
    type DownloadLibrary,
    type LibraryView,
    type MissingRips,
    type ShowMove,
    type Status,
  } from "$lib/api";
  import { IN_SETTINGS, askAnki, askJimaku, checks, dictionariesCheck, hachidoriCheck, linked as linkedTo, noteType as noteTypeOf, type AnkiFacts, type Check } from "$lib/checks";
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
  let libraries = $state<LibraryView[]>(kept<LibraryView[]>("libraries") ?? []);
  /** The computer's network connections, for Downloads to choose the one to download through. */
  let connections = $state<{ name: string; connected: boolean }[]>([]);
  /** The library being added (at: null) or edited in its sheet. */
  let editing = $state<{ at: number | null } | null>(null);

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
        callKept<DownloadsView>("downloads").then((d) => (downloadsFolder = d.folder)).catch(() => {}),
        callKept<LibraryView[]>("libraries").then((l) => (libraries = l)),
        call<{ name: string; connected: boolean }[]>("connections").then((c) => (connections = c)).catch(() => {}),
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
      // A library being read: its count goes up as its shows are added.
      if (libraries.some((l) => l.adding)) libraries = await call<LibraryView[]>("libraries").catch(() => libraries);
      if (status?.anki && !ankiHas) ankiHas = await askAnki(settings);
      // A VPN connecting or dropping shows in the list of connections.
      if (pane === "library") connections = await call<{ name: string; connected: boolean }[]>("connections").catch(() => connections);
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
  /** A speed limit as typed, in whole KB a second: 0 (no limit) for nothing or less. */
  const speed = (typed: string) => Math.max(0, Math.round(Number(typed) || 0));
  const bool = (k: string, d: boolean) => (typeof settings[k] === "boolean" ? (settings[k] as boolean) : d);

  /** Settings' pages, as the list at its side has them. */
  const PAGES = [
    { id: "overview", name: "Overview" },
    { id: "library", name: "Library and downloads" },
    { id: "subtitles", name: "Subtitles" },
    { id: "playing", name: "Playing" },
    { id: "cards", name: "Cards and Anki" },
    { id: "lookups", name: "Lookups and names" },
    { id: "explain", name: "Explain and Previously on" },
    { id: "keys", name: "Keys and accounts" },
    { id: "backup", name: "Backup and your data" },
  ];
  const pageName = (id: string) => PAGES.find((p) => p.id === id)?.name ?? "";

  /** What Find a setting looks through: each setting, its page, and the group it's in there, which a link to Settings can name (/settings#moved). `words` are what else it might be looked for by. */
  const FIND: { label: string; page: string; group: string; words?: string }[] = [
    { label: "Libraries", page: "library", group: "libraries", words: "folders series movies add library" },
    { label: "Folder for new shows", page: "library", group: "downloads", words: "downloads" },
    { label: "nyaa address", page: "library", group: "downloads", words: "blocked provider" },
    { label: "Download and upload speed, at most", page: "library", group: "downloads", words: "torrent limit rate bandwidth" },
    { label: "Keep sharing a download once it’s finished", page: "library", group: "downloads", words: "torrent seeding upload stop" },
    { label: "Ask the router to let others connect", page: "library", group: "downloads", words: "torrent upnp port forwarding" },
    { label: "Download only through one connection", page: "library", group: "downloads", words: "torrent vpn network adapter bind" },
    { label: "Keep free on the drive", page: "library", group: "downloads", words: "torrent disk space room" },
    { label: "Moved rips", page: "library", group: "moved", words: "drive letter folder find" },
    { label: "Free up space", page: "library", group: "disk", words: "disk copies" },
    { label: "Copies folder", page: "library", group: "disk", words: "disk space" },
    { label: "Jimaku", page: "subtitles", group: "finding" },
    { label: "Subtitle size", page: "subtitles", group: "finding" },
    { label: "Machine-made subtitles", page: "subtitles", group: "machine", words: "whisper openrouter model mai-transcribe elevenlabs" },
    { label: "Play the next episode when one ends", page: "playing", group: "play", words: "autoplay countdown" },
    { label: "Skip endings", page: "playing", group: "play", words: "outro credits" },
    { label: "Convert ahead", page: "playing", group: "play" },
    { label: "Convert the rest while nothing plays", page: "playing", group: "play" },
    { label: "Wait while a game is running", page: "playing", group: "games" },
    { label: "More games", page: "playing", group: "games" },
    { label: "ffmpeg", page: "playing", group: "ffmpeg", words: "ffprobe" },
    { label: "AnkiConnect’s address", page: "cards", group: "connection" },
    { label: "Anki Beacon", page: "cards", group: "connection" },
    { label: "Deck", page: "cards", group: "cards-where" },
    { label: "Note type", page: "cards", group: "cards-where" },
    { label: "The fields Telly fills in", page: "cards", group: "cards-fields", words: "sentence picture audio source miscinfo" },
    { label: "Extra around the line", page: "cards", group: "cards-fields", words: "clip length" },
    { label: "Known words", page: "cards", group: "known", words: "studied mature deck note types word field" },
    { label: "Link to the Hachidori in the browser", page: "lookups", group: "hachidori" },
    { label: "Telly’s own relay", page: "lookups", group: "hachidori" },
    { label: "Dictionaries", page: "lookups", group: "hachidori", words: "yomitan folder" },
    { label: "Hachidori’s own Settings", page: "lookups", group: "hachidori" },
    { label: "Character names", page: "lookups", group: "names", words: "spoilers dictionary colour" },
    { label: "Model", page: "explain", group: "answers", words: "claude haiku sonnet opus" },
    { label: "Answer language", page: "explain", group: "answers", words: "english japanese" },
    { label: "Previously on", page: "explain", group: "recap", words: "recap time away" },
    { label: "Jimaku API key", page: "keys", group: "api-keys" },
    { label: "OpenRouter API key", page: "keys", group: "api-keys" },
    { label: "AniList", page: "keys", group: "accounts", words: "sign in token list" },
    { label: "Claude Code", page: "keys", group: "accounts" },
    { label: "Back up and restore", page: "backup", group: "backup" },
    { label: "Tidepool", page: "backup", group: "tidepool", words: "log" },
    { label: "Where Telly keeps its data", page: "backup", group: "data", words: "folder" },
  ];

  /** What the address names after its #: a page, or a group on one. */
  const named = $derived(decodeURIComponent(page.url.hash.slice(1)));
  const pane = $derived(PAGES.some((p) => p.id === named) ? named : (FIND.find((f) => f.group === named)?.page ?? "overview"));
  /** The page's scrolling box. */
  let box = $state<HTMLElement | null>(null);

  // A page opens at its top; a group named in the address is scrolled to.
  $effect(() => {
    const id = named;
    tick().then(() => {
      const group = PAGES.some((p) => p.id === id) ? null : document.getElementById(id);
      if (group) group.scrollIntoView({ block: "start" });
      else if (box) box.scrollTop = 0;
    });
  });

  /** What's typed in Find a setting, and the settings it finds (null when nothing is typed). */
  let finding = $state("");
  const found = $derived.by(() => {
    const words = finding.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length) return null;
    return FIND.filter((f) => {
      const text = `${f.label} ${f.words ?? ""} ${pageName(f.page)}`.toLowerCase();
      return words.every((w) => text.includes(w));
    });
  });

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

  /** Chooses the field of the note type that one of a card's parts goes in. */
  async function setField(k: string, field: string) {
    await set(k, field);
    status = await call<Status>("status").catch(() => status);
  }

  const noteType = $derived(noteTypeOf(settings));

  async function copyBeacon() {
    try {
      await navigator.clipboard.writeText(status?.beaconAddress ?? "");
      toast({ key: "beacon", state: "done", title: "Copied Anki Beacon’s address", text: "Paste it into Beacon’s config in Anki." });
    } catch {
      toast({ key: "beacon", state: "failed", title: "Couldn’t copy it", text: "Select the address and press Ctrl+C." });
    }
  }

  async function freeAll() {
    try {
      const r = await call<{ episodes: number; bytes: number }>("freeSpace", { show: null });
      toast({ key: "free", state: "done", title: r.episodes ? `Freed ${bytes(r.bytes)}` : "Nothing to free", text: r.episodes ? `Removed Telly’s copies of ${plural(r.episodes, "watched episode")}.` : "Only copies of watched episodes are removed." });
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

  /** Every check, the setup's own, once enough is known to say. */
  const all = $derived(status && claude && (hd || !inTauri) ? checks({ settings, status, jimaku: jimakuWorks, anki: ankiHas, own, hd, claude }) : null);
  const check = (id: string): Check | undefined => all?.find((c) => c.id === id);
  const jimaku = $derived(check("jimaku"));
  /** The Overview's three lists: what isn't working, what is, and what's optional and not set up or still being found out. */
  const broken = $derived(all?.filter((c) => c.state === "bad" || c.state === "warn") ?? []);
  const working = $derived(all?.filter((c) => c.state === "ok") ?? []);
  const idle = $derived(all?.filter((c) => c.state === "off" || c.state === "wait") ?? []);
  /** The pages with something not working on them, and how badly. */
  const marks = $derived.by(() => {
    const on: Record<string, "bad" | "warn"> = {};
    for (const c of broken) {
      const at = IN_SETTINGS[c.id]?.page;
      if (at && on[at] !== "bad") on[at] = c.state === "bad" ? "bad" : "warn";
    }
    return on;
  });
  const summary = $derived.by(() => {
    if (!all) return "Checking what works…";
    if (!broken.length) return "Everything Telly checks is working.";
    const rest = working.length ? ` ${working.length === 1 ? "The other one is" : `The other ${working.length} are`} working.` : "";
    return `${broken.length === 1 ? "One thing needs" : `${broken.length} things need`} a look.${rest}`;
  });

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
  /** Where new shows are downloaded when no folder is chosen. */
  let downloadsFolder = $state("");
  /** Saves the list of libraries, then lists them again with what's in them. */
  async function saveLibraries(list: DownloadLibrary[]) {
    editing = null;
    await set("libraries", list.map(({ name, kind, folders, downloads }) => ({ name, kind, folders, downloads })));
    libraries = await call<LibraryView[]>("libraries").catch(() => libraries);
  }
  /** Puts the library from its sheet in the list: in its place when edited, else at the end. */
  function saveLibrary(library: DownloadLibrary) {
    const at = editing?.at ?? null;
    saveLibraries(at === null ? [...libraries, library] : libraries.map((l, i) => (i === at ? library : l)));
  }
  async function chooseDownloadsFolder() {
    const folder = await pickFolder("Choose where new shows are downloaded");
    if (folder) await setDownloadsFolder(folder);
  }

  /** Chooses where new shows go (null: Downloads\Telly), and shows where that is now. */
  async function setDownloadsFolder(folder: string | null) {
    await set("downloads.folder", folder);
    downloadsFolder = await call<DownloadsView>("downloads").then((d) => d.folder).catch(() => downloadsFolder);
  }
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

{#snippet dot(state: string)}
  <i class="dot" class:ok={state === "ok"} class:warn={state === "warn"} class:bad={state === "bad"} class:off={state === "off"}></i>
{/snippet}

{#snippet toggle(key: string, fallback: boolean, label: string, help: string, changed?: (on: boolean) => void)}
  <label class="row">
    <span class="what"><b>{label}</b>{#if help}<small>{help}</small>{/if}</span>
    <input type="checkbox" checked={bool(key, fallback)} onchange={async (e) => { const on = e.currentTarget.checked; await set(key, on); changed?.(on); }} />
  </label>
{/snippet}

{#snippet cardField(key: string, value: string, label: string, help: string)}
  <label class="row">
    <span class="what"><b>{label}</b><small>{help}</small></span>
    {#if ankiHas?.fields.length}
      <select class="input ctl" {value} onchange={(e) => setField(key, e.currentTarget.value)}>
        {#if !ankiHas.fields.includes(value)}<option {value}>{value}</option>{/if}
        {#each ankiHas.fields as f (f)}<option value={f}>{f}</option>{/each}
      </select>
    {:else}
      <input class="input ctl" {value} onchange={(e) => setField(key, e.currentTarget.value.trim())} />
    {/if}
  </label>
{/snippet}

{#snippet checkRows(list: Check[])}
  <div class="card">
    {#each list as c (c.id)}
      <a class="row" href="#{IN_SETTINGS[c.id]?.page}" data-sveltekit-replacestate>
        {#if c.state === "wait"}<span class="spin"></span>{:else}{@render dot(c.state)}{/if}
        <b class="name">{IN_SETTINGS[c.id]?.name}</b>
        <span class="detail">{c.text}</span>
        <span class="go"><Icon name="back" /></span>
      </a>
    {/each}
  </div>
{/snippet}

<div class="set scroll" bind:this={box}>
  <div class="cols">
    <aside class="side">
      <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Back to library</span></button>
      <h1>Settings</h1>
      <label class="find">
        <Icon name="search" />
        <input type="search" bind:value={finding} placeholder="Find a setting" aria-label="Find a setting" onkeydown={(e) => { if (e.key === "Escape") finding = ""; }} />
      </label>
      {#if found}
        <nav class="pages" aria-label="Settings found">
          {#each found as f (f.group + f.label)}
            <a href="#{f.group}" data-sveltekit-replacestate onclick={() => (finding = "")}><span class="grow">{f.label}<small>{pageName(f.page)}</small></span></a>
          {:else}
            <p class="note">No setting is called that.</p>
          {/each}
        </nav>
      {:else}
        <nav class="pages" aria-label="Settings">
          {#each PAGES as p (p.id)}
            <a href="#{p.id}" data-sveltekit-replacestate aria-current={pane === p.id ? "page" : undefined}>
              <span class="grow">{p.name}</span>
              {#if marks[p.id]}<i class="dot" class:warn={marks[p.id] === "warn"} class:bad={marks[p.id] === "bad"} role="img" aria-label="Needs a look"></i>{/if}
            </a>
          {/each}
        </nav>
      {/if}
    </aside>

    <main>
      {#if error}<p class="err">{error}</p>{/if}

      {#if pane === "overview"}
        <div class="head">
          <div>
            <h2>Overview</h2>
            <p class="lede">{summary}</p>
          </div>
          <button type="button" class="btn quiet" onclick={load}>Check again</button>
          <button type="button" class="btn quiet" onclick={() => goto("/setup")}>Run setup again</button>
        </div>
        {#if broken.length}
          <section class="group">
            <h3>Needs a look</h3>
            {#each broken as c (c.id)}
              <div class="card alert" class:bad={c.state === "bad"}>
                <div class="row">
                  {@render dot(c.state)}
                  <span class="what"><b>{IN_SETTINGS[c.id]?.name}</b><small>{c.text}</small></span>
                  <a class="btn" href="#{IN_SETTINGS[c.id]?.page}" data-sveltekit-replacestate>Open {pageName(IN_SETTINGS[c.id]?.page ?? "")}</a>
                </div>
              </div>
            {/each}
          </section>
        {/if}
        {#if working.length}
          <section class="group">
            <h3>Working</h3>
            {@render checkRows(working)}
          </section>
        {/if}
        {#if idle.length}
          <section class="group">
            <h3>Not set up</h3>
            {@render checkRows(idle)}
          </section>
        {/if}
        <section class="group">
          <h3>Space</h3>
          <div class="card">
            <div class="row">
              <span class="what"><b>Telly’s copies of episodes use {bytes(status?.spaceUsed ?? 0)}</b><small>Your rips stay where they are. Freeing space removes Telly’s copies of episodes you’ve watched.</small></span>
              <button type="button" class="btn quiet" onclick={freeAll}>Free up space</button>
            </div>
          </div>
        </section>
      {:else if pane === "library"}
        <div class="head"><div><h2>Library and downloads</h2></div></div>

        <section class="group" id="libraries">
          <div class="head-row">
            <h3>Libraries</h3>
            <button type="button" class="btn quiet" onclick={() => (editing = { at: null })}><Icon name="plus" /><span>Add library…</span></button>
          </div>
          <p class="lede">Telly adds the shows in a library’s folders when it’s added. A download goes there once you’ve said which show it is: Telly makes the folders and the release keeps its file names; nothing already there is moved.</p>
          {#if libraries.length}
            <div class="card">
              {#each libraries as l, i (i)}
                <div class="row top">
                  <span class="what">
                    <span><b>{l.name}</b> <span class="muted">{l.kind === "movies" ? "Movies" : "Series"} · {plural(l.shows, "show")}{#if l.adding} · Adding shows…{/if}</span></span>
                    {#each l.folders as folder (folder)}
                      <span class="path"><span class="mono">{folder}</span>{#if l.folders.length > 1 && folder === l.downloads} <span class="muted">Downloads go here</span>{/if}</span>
                    {/each}
                  </span>
                  <button type="button" class="btn quiet small" onclick={() => (editing = { at: i })}>Edit…</button>
                  <button type="button" class="btn quiet small" onclick={() => saveLibraries(libraries.filter((_, j) => j !== i))}>Remove</button>
                </div>
              {/each}
            </div>
          {:else}
            <p class="muted">No libraries. Downloads go in the folder for new shows.</p>
          {/if}
        </section>
        {#if editing}
          <LibrarySheet library={editing.at === null ? null : libraries[editing.at]} onclose={() => (editing = null)} onsave={saveLibrary} />
        {/if}

        <section class="group" id="downloads">
          <h3>Downloads</h3>
          <p class="lede">Find a show on nyaa.si with <b>Download…</b> in Recommended, the title bar’s search or Processing, or <b>Download episodes…</b> on a show’s page, and Telly downloads it itself, picking up where it was whenever the connection drops and when Telly opens again. Each episode joins the library as it finishes. Processing shows how they’re going.</p>
          <div class="card">
            <div class="row tall">
              <span class="what"><b>Folder for new shows</b><small>A new show gets a folder of its own in here when no library takes it. More episodes of a show in the library go in its folder.</small></span>
              <span class="inline"><input class="input grow mono" value={str("downloads.folder") || downloadsFolder} readonly aria-label="Folder for new shows" /><button type="button" class="btn quiet" onclick={chooseDownloadsFolder}>Choose folder…</button></span>
              {#if str("downloads.folder")}<span><button type="button" class="link-btn" onclick={() => setDownloadsFolder(null)}>Use Downloads\Telly in your home folder</button></span>{/if}
            </div>
            <label class="row">
              <span class="what"><b>nyaa address</b><small>Another address for nyaa, if your internet provider blocks nyaa.si.</small></span>
              <input class="input mono ctl" value={str("nyaa.site")} placeholder="https://nyaa.si" onchange={(e) => set("nyaa.site", e.currentTarget.value.trim())} />
            </label>
          </div>
          <div class="card">
            <label class="row">
              <span class="what"><b>Download speed, at most</b><small>In KB a second. Empty for no limit.</small></span>
              <input class="input ctl" type="number" min="0" step="100" placeholder="No limit" value={Number(settings["downloads.downLimit"]) || ""} onchange={(e) => set("downloads.downLimit", speed(e.currentTarget.value))} />
            </label>
            <label class="row">
              <span class="what"><b>Upload speed, at most</b><small>In KB a second. Empty for no limit. Telly shares what it has of a release with the others getting it.</small></span>
              <input class="input ctl" type="number" min="0" step="100" placeholder="No limit" value={Number(settings["downloads.upLimit"]) || ""} onchange={(e) => set("downloads.upLimit", speed(e.currentTarget.value))} />
            </label>
            {@render toggle("downloads.share", true, "Keep sharing a download once it’s finished", "While Telly is open, others get it from you as you got it from them. Off, each download stops being shared the moment it finishes. One still downloading shares what it has, as torrents do: the upload limit slows that.")}
            {@render toggle("downloads.upnp", true, "Ask the router to let others connect to Telly", "With UPnP, your router opens a port to Telly, so the people sharing a release can connect to Telly as well as Telly to them, which makes downloads faster. Off, Telly only connects out. Changing this stops the downloads for a moment.")}
            <label class="row">
              <span class="what"><b>Download only through</b><small>For a VPN: choose its connection, and Telly downloads and shares only while it’s connected. Within a second of it dropping, Telly’s downloader stops altogether, and it starts again when the connection is back.</small></span>
              <select class="input ctl" value={str("downloads.adapter")} onchange={(e) => set("downloads.adapter", e.currentTarget.value)}>
                <option value="">Any connection</option>
                {#if str("downloads.adapter") && !connections.some((c) => c.name === str("downloads.adapter"))}<option value={str("downloads.adapter")}>{str("downloads.adapter")} (not connected)</option>{/if}
                {#each connections as c (c.name)}<option value={c.name}>{c.name}{c.connected ? "" : " (not connected)"}</option>{/each}
              </select>
            </label>
            <label class="row">
              <span class="what"><b>Keep free on the drive</b><small>Telly doesn’t start a download there isn’t room for, counting what your other downloads on that drive still need.</small></span>
              <select class="input ctl" value={String(settings["downloads.keepFree"] ?? 1)} onchange={(e) => set("downloads.keepFree", Number(e.currentTarget.value))}>
                <option value="0">Nothing</option>
                {#each [1, 5, 10, 20, 50] as gb}<option value={String(gb)}>{gb} GB</option>{/each}
              </select>
            </label>
          </div>
        </section>

        <section class="group" id="moved">
          <h3>Moved rips</h3>
          {#if restored && away.length}
            <p class="lede"><b>Your library is back.</b> The rips of {plural(away.length, "show")} aren’t where they were when the backup was made. Say where they are now here, or with Find its folder… on each show’s page.</p>
          {/if}
          <p class="lede">If your rips moved to another folder or drive, or the drive’s letter changed, Telly can find them there and keep where you were, what you watched, your cards and subtitles. It matches each episode to a file in the new folder; the rips aren’t moved or changed.</p>
          {#if away.length}
            <p class="muted">Not where they were: {away.map((a) => a.title).join(", ")}</p>
          {/if}
          <div class="card">
            <div class="row tall">
              <span class="what"><b>They were in</b></span>
              <span class="inline"><input class="input grow mono" list="moved-from" bind:value={movedFrom} oninput={() => (moved = null)} placeholder="D:\Anime" aria-label="The folder your rips were in" /><button type="button" class="btn quiet" onclick={() => chooseMoved("from")}>Choose…</button></span>
            </div>
            <div class="row tall">
              <span class="what"><b>They’re in now</b></span>
              <span class="inline"><input class="input grow mono" bind:value={movedTo} oninput={() => (moved = null)} placeholder="E:\Anime" aria-label="The folder your rips are in now" /><button type="button" class="btn quiet" onclick={() => chooseMoved("to")}>Choose…</button></span>
            </div>
            {#if moved}
              <div class="row tall moved">
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
              <div class="row">
                <button type="button" class="btn quiet" disabled={!movedFrom.trim() || !movedTo.trim()} onclick={findMoved}>Find them</button>
              </div>
            {/if}
          </div>
          <datalist id="moved-from">
            {#each oldFolders as folder (folder)}<option value={folder}></option>{/each}
          </datalist>
        </section>

        <section class="group" id="disk">
          <h3>Disk space</h3>
          <div class="card">
            <div class="row">
              <span class="what"><b>Telly’s copies of episodes use {bytes(status?.spaceUsed ?? 0)}</b><small>The rips stay where they are. Freeing space removes Telly’s copies of watched episodes.</small></span>
              <button type="button" class="btn quiet" onclick={freeAll}>Free up space</button>
            </div>
            <div class="row tall">
              <span class="what"><b>Copies folder</b></span>
              <span class="inline"><input class="input grow mono" value={status?.mediaDir ?? ""} readonly aria-label="Copies folder" /><button type="button" class="btn quiet" onclick={chooseCopiesFolder}>Choose folder…</button></span>
              {#if str("media.folder")}<span><button type="button" class="link-btn" onclick={() => moveCopies(null)}>Use Telly’s own folder</button></span>{/if}
            </div>
          </div>
        </section>
      {:else if pane === "subtitles"}
        <div class="head"><div><h2>Subtitles</h2></div></div>

        <section class="group" id="finding">
          <h3>Finding and showing them</h3>
          <div class="card">
            <div class="row">
              {#if jimaku}{#if jimaku.state === "wait"}<span class="spin"></span>{:else}{@render dot(jimaku.state)}{/if}{/if}
              <span class="what"><b>Jimaku</b><small>{jimaku?.text ?? "Telly finds each episode’s Japanese subtitles on Jimaku and syncs them to your rips."}</small></span>
              <a class="btn quiet" href="#api-keys" data-sveltekit-replacestate>{str("jimaku.key") ? "Change the key" : "Add a key"}</a>
            </div>
            <label class="row">
              <span class="what"><b>Subtitle size</b></span>
              <select class="input ctl" value={str("player.subtitleSize", "36")} onchange={(e) => set("player.subtitleSize", Number(e.currentTarget.value))}>
                {#each [28, 32, 36, 40, 44, 50] as n}<option value={String(n)}>{n} pixels</option>{/each}
              </select>
            </label>
          </div>
        </section>

        <section class="group" id="machine">
          <h3>Machine-made subtitles</h3>
          {#if str("openrouter.key")}
            <p class="lede">Ready. On a show’s page, <b>Make subtitles with {routerMaker}</b> covers the episodes Jimaku has nothing for. They’re marked as machine-made.</p>
          {:else if !status?.tools.whisper}
            <p class="lede">Whisper can make subtitles for episodes Jimaku has nothing for. Telly uses the whisper.cpp Tidepool downloads for voice input, and Tidepool hasn’t downloaded it on this computer yet.</p>
          {:else if status.tools.whisperModel}
            <p class="lede">Ready. On a show’s page, <b>Make subtitles with Whisper</b> covers the episodes Jimaku has nothing for. They’re marked as machine-made.</p>
          {:else}
            <p class="lede">Whisper can make subtitles for episodes Jimaku has nothing for, marked as machine-made. It needs a model that knows Japanese, from whisper.cpp’s own models.</p>
          {/if}
          <div class="card">
            <div class="row">
              <span class="what"><b>Who makes them</b><small>{str("openrouter.key") ? "A model on OpenRouter, with your OpenRouter key. Each episode’s audio is sent to it." : "Whisper on this computer. With an OpenRouter key, a model on OpenRouter makes them instead."}</small></span>
              <a class="btn quiet" href="#api-keys" data-sveltekit-replacestate>{str("openrouter.key") ? "Change the key" : "Add an OpenRouter key"}</a>
            </div>
            {#if str("openrouter.key")}
              <label class="row">
                <span class="what"><b>Model</b><small>A 24-minute episode costs about $0.04 with MAI-Transcribe, $0.09 with ElevenLabs and $0.01 with Whisper.</small></span>
                <select class="input ctl" value={str("openrouter.model", ROUTER_MODELS[0][0])} onchange={(e) => set("openrouter.model", e.currentTarget.value)}>
                  {#each ROUTER_MODELS as [id, label]}<option value={id}>{label}</option>{/each}
                </select>
              </label>
            {:else if status?.tools.whisper}
              <label class="row">
                <span class="what"><b>Model</b><small>Bigger models take longer for each episode.</small></span>
                <select class="input ctl" value={str("whisper.model", WHISPER_MODELS[0][0])} onchange={(e) => chooseWhisperModel(e.currentTarget.value)}>
                  {#each WHISPER_MODELS as [file, label]}<option value={file}>{label}</option>{/each}
                </select>
              </label>
              <div class="row">
                {#if status.tools.whisperModel}
                  <span class="muted mono path">{status.tools.whisperModel}</span>
                {:else}
                  <button type="button" class="btn quiet" onclick={getWhisperModel}>Get model</button>
                {/if}
              </div>
            {/if}
          </div>
        </section>
      {:else if pane === "playing"}
        <div class="head"><div><h2>Playing</h2></div></div>

        <section class="group" id="play">
          <h3>Episodes</h3>
          <div class="card">
            {@render toggle("player.autoplay", true, "Play the next episode when one ends", "After an 8-second countdown. Esc stops it.")}
            {@render toggle("player.skipEndings", false, "Skip endings", "The countdown starts when the ending does, from the disc’s chapters, else where AniSkip’s users marked it.")}
            <label class="row">
              <span class="what"><b>Convert ahead</b><small>Episodes WebView2 can’t play are converted this far ahead of the one playing.</small></span>
              <select class="input ctl" value={String(settings["convert.ahead"] ?? 10)} onchange={(e) => set("convert.ahead", Number(e.currentTarget.value))}>
                {#each [3, 5, 10, 20, 50] as n}<option value={String(n)}>{n} episodes</option>{/each}
              </select>
            </label>
            {@render toggle("convert.rest", false, "Convert the rest while nothing plays", "Uses more disk space, so nothing waits later.")}
          </div>
        </section>

        <section class="group" id="games">
          <h3>Games</h3>
          <div class="card">
            {@render toggle("games.wait", true, "Wait while a game is running", "No episode starts converting, getting its subtitles or its seek bar’s pictures until the game closes, so the game has the processor and the drive. A game is a program in a Steam, Epic, GOG, Riot or Xbox games folder, or World of Warcraft.")}
            <label class="row tall">
              <span class="what"><b>More games</b><small>Program names, separated by commas, as Task Manager’s Details tab lists them.</small></span>
              <input class="input mono" value={str("games.names")} placeholder="ffxiv_dx11.exe, eldenring.exe" onchange={(e) => set("games.names", e.currentTarget.value.trim())} />
            </label>
          </div>
        </section>

        <section class="group" id="ffmpeg">
          <h3>ffmpeg</h3>
          <div class="card">
            <div class="row top">
              {@render dot(status ? (status.tools.ffmpeg ? "ok" : "bad") : "")}
              <span class="what">
                {#if status?.tools.ffmpeg}
                  <b class="mono path">{status.tools.ffmpeg}</b>
                  <small>{status.tools.version} · converts with {status.tools.encoder}</small>
                {:else}
                  <b>Telly can’t find ffmpeg</b>
                  <small>It looks on PATH and where GameSentenceMiner keeps it. Choose ffmpeg.exe (its folder needs ffprobe.exe too).</small>
                {/if}
              </span>
              <button type="button" class="btn quiet" onclick={chooseFfmpeg}>Choose ffmpeg.exe…</button>
              <button type="button" class="btn quiet" onclick={lookAgain}>Look again</button>
            </div>
            {#if status?.tools.ffmpeg}
              <div class="row">
                {@render dot(status.tools.canClip ? "ok" : "bad")}
                <span class="what"><small>{status.tools.canClip ? "It can make animated AVIF and Opus for cards." : "This ffmpeg can’t make AVIF and Opus for cards (it needs libsvtav1 and libopus)."}</small></span>
                {#if str("tools.ffmpeg")}<button type="button" class="link-btn" onclick={async () => { await set("tools.ffmpeg", null); lookAgain(); }}>Forget my choice</button>{/if}
              </div>
            {/if}
          </div>
        </section>
      {:else if pane === "cards"}
        <div class="head">
          <div>
            <h2>Cards and Anki</h2>
            <p class="lede">Hachidori makes the card as usual. Telly then adds a moving picture of the line and its audio, and fills only fields that are empty.</p>
          </div>
        </div>

        <section class="group" id="connection">
          <h3>Connection</h3>
          <div class="card">
            <label class="row">
              {@render dot(status ? (status.anki ? "ok" : "warn") : "")}
              <span class="what"><b>{status?.anki ? "Anki is open" : "Anki isn’t open, or AnkiConnect isn’t installed"}</b><small>AnkiConnect’s address</small></span>
              <input class="input mono ctl" value={str("anki.url", "http://127.0.0.1:8765")} onchange={(e) => set("anki.url", e.currentTarget.value.trim())} />
            </label>
            <div class="row top">
              {@render dot(status?.mining?.beacon ? "ok" : status?.mining?.listening ? "warn" : status?.mining?.listenError ? "bad" : "off")}
              <span class="what">
                {#if status?.mining?.beacon}
                  <b>Anki Beacon is telling Telly about new cards</b>
                {:else if status?.mining?.listening}
                  <b>Anki Beacon isn’t connected</b>
                  <small>Until it is, Telly asks AnkiConnect every 2 seconds while you watch. To connect it, add this address to Beacon’s config in Anki (Tools → Add-ons → Anki Beacon → Config).</small>
                  <span class="inline beacon"><input class="input mono grow" value={status.beaconAddress} readonly aria-label="Anki Beacon’s address" onfocus={(e) => e.currentTarget.select()} /><button type="button" class="btn" onclick={copyBeacon}>Copy address</button></span>
                {:else}
                  <b>{status?.mining?.listenError ?? "Not listening for Anki Beacon"}</b>
                {/if}
              </span>
            </div>
          </div>
        </section>

        <section class="group" id="cards-where">
          <h3>Where cards go</h3>
          <div class="card">
            <label class="row">
              <span class="what"><b>Deck</b><small>{check("deck")?.state === "warn" ? check("deck")?.text : "Type {show} in the name to give each show a deck of its own."}</small></span>
              <input class="input ctl" list="anki-decks" value={str("anki.deck", "例文マイニング")} onchange={(e) => set("anki.deck", e.currentTarget.value.trim())} />
            </label>
            <label class="row">
              <span class="what"><b>Note type</b><small class:good={check("fields")?.state === "ok"} class:wrong={check("fields")?.state === "bad"}>{check("fields")?.text ?? ""}</small></span>
              {#if ankiHas?.noteTypes.length}
                <select class="input ctl" value={noteType} onchange={(e) => set("anki.noteType", e.currentTarget.value)}>
                  {#if !ankiHas.noteTypes.includes(noteType)}<option value={noteType}>{noteType}</option>{/if}
                  {#each ankiHas.noteTypes as t (t)}<option value={t}>{t}</option>{/each}
                </select>
              {:else}
                <input class="input ctl" value={noteType} onchange={(e) => set("anki.noteType", e.currentTarget.value.trim())} />
              {/if}
            </label>
          </div>
          <datalist id="anki-decks">{#each ankiHas?.decks ?? [] as d (d)}<option value={d}></option>{/each}</datalist>
        </section>

        <section class="group" id="cards-fields">
          <h3>What Telly fills in</h3>
          <div class="card">
            {@render cardField("anki.field.sentence", status?.fields.sentence ?? "Sentence", "The line", "The sentence, as the subtitles have it.")}
            {@render cardField("anki.field.picture", status?.fields.picture ?? "Picture", "Its moving picture", "An animated AVIF of the line being said.")}
            {@render cardField("anki.field.audio", status?.fields.audio ?? "SentenceAudio", "Its audio", "The line’s sound, as Opus.")}
            {@render cardField("anki.field.info", status?.fields.info ?? "MiscInfo", "Where it’s from", "The show, the episode and the time.")}
            <div class="row">
              <span class="what"><b id="clip-label">Extra around the line</b><small>How much the picture and audio run before and after it.</small></span>
              <div class="seg" role="group" aria-labelledby="clip-label">
                {#each [["250", "250 milliseconds"], ["500", "500 milliseconds"], ["prev", "From the line before"]] as [v, label]}
                  <button type="button" aria-pressed={str("clip.padding", "250") === v} onclick={() => set("clip.padding", v)}>{label}</button>
                {/each}
              </div>
            </div>
          </div>
        </section>

        <section class="group" id="known">
          <h3>Known words</h3>
          <p class="lede">A word is known when Anki has a card for it that counts. Telly reads these cards and never changes them.</p>
          {#if sources}
            <div class="card">
              <label class="row">
                <span class="what"><b>Deck</b></span>
                <select class="input ctl" value={str("known.deck")} onchange={(e) => setKnown("known.deck", e.currentTarget.value)}>
                  <option value="">Every deck</option>
                  {#each sources.decks as deck (deck)}<option value={deck}>{deck}</option>{/each}
                </select>
              </label>
              <label class="row">
                <span class="what"><b>Word field</b><small>The field of a note that holds its word.</small></span>
                <input class="input ctl" value={str("known.field") || sources.field} onchange={(e) => setKnown("known.field", e.currentTarget.value.trim())} />
              </label>
              <div class="row top">
                <span class="what"><b id="known-types">Note types</b></span>
                <div class="ctl types" role="group" aria-labelledby="known-types">
                  {#each sources.noteTypes as type (type)}
                    <label class="check"><input type="checkbox" checked={knownTypes.length === 0 || knownTypes.includes(type)} onchange={() => toggleKnownType(type)} /><span>{type}</span></label>
                  {/each}
                </div>
              </div>
              <div class="row">
                <span class="what"><b id="known-when">A card counts when it’s</b><small>Studied: reviewed at least once. Mature: its next review is 21 days or more away, as Anki counts it.</small></span>
                <div class="seg" role="group" aria-labelledby="known-when">
                  {#each [["studied", "Studied"], ["mature", "Mature"]] as [value, label] (value)}
                    <button type="button" aria-pressed={str("known.when", "studied") === value} onclick={() => setKnown("known.when", value)}>{label}</button>
                  {/each}
                </div>
              </div>
              <p class="row note">{sources.studied.toLocaleString("en-GB")} {sources.studied === 1 ? "note" : "notes"} studied, {sources.mature.toLocaleString("en-GB")} of them mature. {#if sources.suspended}{sources.suspended.toLocaleString("en-GB")} {sources.suspended === 1 ? "is suspended and doesn’t" : "are suspended and don’t"} count.{/if}</p>
            </div>
          {:else}
            <p class="muted">Open Anki to choose which cards count.</p>
          {/if}
        </section>
      {:else if pane === "lookups"}
        <div class="head"><div><h2>Lookups and names</h2></div></div>

        <section class="group" id="hachidori">
          <h3>Hachidori</h3>
          <p class="lede">{hdText}</p>
          <div class="card">
            {@render toggle("hachidori.link", false, "Link to the Hachidori in the browser", "Dictionaries, settings, and card format come from it, through Hachidori Relay.", restartHachidori)}
            {@render toggle("hachidori.relay", true, "Run Telly’s own relay while Anki is closed", relayText)}
            <div class="row tall">
              <span class="what"><b>Dictionaries</b>{#if dictText}<small>{dictText}</small>{/if}{#if ankiText}<small>{ankiText}</small>{/if}</span>
              <span class="inline"><input class="input grow mono" value={own?.folder ?? ""} placeholder="C:\Users\you\Documents\dictionaries" aria-label="Dictionaries folder" onchange={async (e) => { await set("hachidori.dictionaries", e.currentTarget.value.trim()); own = await call<OwnHachidori>("ownHachidori").catch(() => own); }} /><button type="button" class="btn quiet" onclick={chooseDictionaries}>Choose folder…</button></span>
            </div>
            <div class="row">
              <button type="button" class="btn quiet" onclick={() => openHachidoriSettings().catch((e) => toast({ key: "hd", state: "failed", title: "Hachidori", text: errorText(e) }))} disabled={!hd?.prepared}>Open Hachidori Settings</button>
              <button type="button" class="btn quiet" onclick={restartHachidori} disabled={!hd?.prepared}>Restart Hachidori</button>
              {#if hd?.version}<span class="muted">Hachidori {hd.version}</span>{/if}
            </div>
          </div>
        </section>

        <section class="group" id="names">
          <h3>Character names</h3>
          <p class="lede">Telly makes a dictionary of the characters in the shows being watched, from AniList, the way characterdictionary.tokyo does, and adds it to Hachidori as “Telly characters”. Hovering a name shows who it is, with their picture and how they’re being addressed. Their names are also coloured in the subtitles.</p>
          <p class="lede">{namesText}</p>
          <div class="card">
            {@render toggle("names.enabled", true, "Make the character dictionary", "The casts of the six most recently watched shows.", async () => { names = await call<NamesStatus>("namesStatus").catch(() => names); })}
            {@render toggle("names.spoilers", false, "Include spoilers", "AniList marks some of a character’s description and names as spoilers. They’re left out unless this is on.")}
            {@render toggle("player.names", true, "Colour characters’ names in the subtitles", "")}
            <div class="row">
              <button type="button" class="btn quiet" onclick={remakeNames} disabled={!names?.enabled}>Rebuild dictionary</button>
            </div>
          </div>
        </section>
      {:else if pane === "explain"}
        <div class="head">
          <div>
            <h2>Explain and Previously on</h2>
            <p class="lede">
              {#if claude?.openrouter}
                <b>Explain</b> (E) explains the line on screen, and <b>Previously on</b> (P) recaps the story so far, with its key moments to watch again. Claude Code isn’t on this computer, so both use gpt-oss on OpenRouter, with your OpenRouter key, and the subtitles they’re about are sent to it. Answers are saved, so asking again is instant.
              {:else if claude?.available}
                <b>Explain</b> (E) explains the line on screen, and <b>Previously on</b> (P) recaps the story so far, with its key moments to watch again. Both use the Claude Code on this computer, with its own sign-in ({claude.version}). Answers are saved, so asking again is instant.
              {:else if claude}
                <b>Explain</b> and <b>Previously on</b> use Claude Code, and Telly can’t find it: {claude.detail} Install Claude Code, sign in, and come back here, or add an OpenRouter API key in <a href="#api-keys" data-sveltekit-replacestate>Keys and accounts</a> and gpt-oss on OpenRouter answers instead.
              {:else}
                Looking for Claude Code…
              {/if}
            </p>
          </div>
        </div>

        <section class="group" id="answers">
          <h3>Answers</h3>
          <div class="card">
            <label class="row">
              <span class="what"><b>Model</b><small>Telly keeps one Claude Code started while the player is open, so Explain answers in a second or two with Haiku. Bigger models think longer before answering.</small></span>
              <select class="input ctl" value={str("claude.model", "")} onchange={(e) => set("claude.model", e.currentTarget.value)} disabled={claude?.openrouter}>
                <option value="">Haiku 5.5 (fastest, cheapest)</option>
                <option value="claude-sonnet-5-5">Sonnet 5.5</option>
                <option value="claude-opus-5-5">Opus 5.5</option>
                <option value="default">Claude Code’s default</option>
              </select>
            </label>
            <label class="row">
              <span class="what"><b>Answer language</b></span>
              <select class="input ctl" value={str("claude.language", "en")} onchange={(e) => set("claude.language", e.currentTarget.value)}>
                <option value="en">English</option>
                <option value="ja">Simple Japanese</option>
              </select>
            </label>
          </div>
        </section>

        <section class="group" id="recap">
          <h3>Previously on</h3>
          <div class="card">
            {@render toggle("recap.enabled", true, "Show “Previously on” after time away from a show", "It’s written when the player closes, so it’s ready next time.")}
            <label class="row">
              <span class="what"><b>Time away</b></span>
              <select class="input ctl" value={String(settings["recap.afterHours"] ?? 8)} onchange={(e) => set("recap.afterHours", Number(e.currentTarget.value))} disabled={!bool("recap.enabled", true)}>
                {#each [[1, "1 hour"], [4, "4 hours"], [8, "8 hours"], [24, "1 day"], [72, "3 days"]] as [h, label]}<option value={String(h)}>{label} or more</option>{/each}
              </select>
            </label>
          </div>
        </section>
      {:else if pane === "keys"}
        <div class="head"><div><h2>Keys and accounts</h2></div></div>

        <section class="group" id="api-keys">
          <h3>Keys</h3>
          <div class="card">
            <label class="row tall">
              <span class="what"><b>Jimaku API key</b><small class:good={jimaku?.state === "ok"} class:wrong={jimaku?.state === "bad" && !!str("jimaku.key")}>{saved["jimaku.key"] ? "Saved. Telly is looking for subtitles again." : str("jimaku.key") && jimaku ? jimaku.text : "A free Jimaku account comes with a key. Telly uses it to find each episode’s subtitles and sync them to the rips."}</small></span>
              <span class="inline">
                <input class="input mono grow" type={showKey ? "text" : "password"} bind:value={key} onchange={() => set("jimaku.key", key.trim())} />
                <button type="button" class="btn quiet" onclick={() => (showKey = !showKey)}>{showKey ? "Hide" : "Show"}</button>
                <button type="button" class="btn quiet" onclick={() => openLink("https://jimaku.cc/account")}>Get a key</button>
              </span>
            </label>
            <label class="row tall">
              <span class="what"><b>OpenRouter API key</b><small>With a key, a model on OpenRouter makes subtitles for the episodes Jimaku has nothing for, and each episode’s audio is sent to it. Without one, Whisper on this computer does. Without Claude Code on this computer, gpt-oss on OpenRouter also answers Explain and Previously on.</small></span>
              <span class="inline">
                <input class="input mono grow" type={showRouterKey ? "text" : "password"} value={str("openrouter.key")} onchange={(e) => set("openrouter.key", e.currentTarget.value.trim())} />
                <button type="button" class="btn quiet" onclick={() => (showRouterKey = !showRouterKey)}>{showRouterKey ? "Hide" : "Show"}</button>
                <button type="button" class="btn quiet" onclick={() => openLink("https://openrouter.ai/settings/keys")}>Get a key</button>
              </span>
            </label>
          </div>
        </section>

        <section class="group" id="accounts">
          <h3>Accounts</h3>
          <div class="card">
            {#if str("anilist.user")}
              <div class="row">
                {@render dot(str("anilist.error") ? "warn" : "ok")}
                <span class="what"><b>AniList: signed in as {str("anilist.user")}</b><small class:wrong={!!str("anilist.error")}>{str("anilist.error") ? `Your list wasn’t updated: ${str("anilist.error")}. Telly tries again every 10 minutes.` : "Episodes watched from now on go on your AniList list."}</small></span>
                <button type="button" class="btn quiet" onclick={anilistSignOut}>Sign out</button>
              </div>
            {:else}
              <label class="row tall">
                <span class="what"><b>AniList access token</b><small>Signed in, each episode watched in Telly goes on your AniList list. Get a token opens AniList, which shows a token once you approve Telly.</small></span>
                <span class="inline">
                  <input class="input mono grow" type="password" bind:value={anilistToken} />
                  <button type="button" class="btn quiet" onclick={() => openLink(`https://anilist.co/api/v2/oauth/authorize?client_id=${ANILIST_CLIENT_ID}&response_type=token`)}>Get a token</button>
                  <button type="button" class="btn" onclick={anilistSignIn} disabled={!anilistToken.trim()}>Sign in</button>
                </span>
              </label>
            {/if}
            <div class="row">
              {#if check("claude")?.state === "wait"}<span class="spin"></span>{:else}{@render dot(check("claude")?.state ?? "")}{/if}
              <span class="what"><b>Claude Code</b><small>{check("claude")?.text ?? "Looking for Claude Code…"} It uses its own sign-in on this computer, so it needs no key here.</small></span>
              <a class="btn quiet" href="#explain" data-sveltekit-replacestate>Open Explain and Previously on</a>
            </div>
          </div>
        </section>
      {:else}
        <div class="head"><div><h2>Backup and your data</h2></div></div>

        <section class="group" id="backup">
          <h3>Backup</h3>
          <p class="lede">A backup is one file with what Telly can’t make again: your shows, where you are in each and what you watched, your settings, the subtitles and your nudges to their timing, Tidepool’s log, the words you know and your AniList list. Restore it on a new computer or after reinstalling. Telly’s copies of episodes, covers and the seek bar’s pictures aren’t in it, as Telly makes them again.</p>
          <p class="lede">It holds your Jimaku and OpenRouter keys and your AniList token, so keep it where only you can open it.</p>
          <div class="card">
            {#if restoring}
              <div class="row tall moved">
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
              <div class="row">
                <button type="button" class="btn quiet" disabled={backingUp} onclick={backUp}>{backingUp ? "Backing up…" : "Back up…"}</button>
                <button type="button" class="btn quiet" onclick={chooseBackup}>Restore…</button>
              </div>
            {/if}
          </div>
        </section>

        <section class="group" id="tidepool">
          <h3>Tidepool</h3>
          <div class="card">
            <div class="row tall">
              <span class="what"><b>Watching is logged for Tidepool’s Japanese log</b><small>Each sitting, and the lines and characters that played, never the text itself.</small></span>
              <span class="muted mono path">{status?.logPath}</span>
            </div>
          </div>
        </section>

        <section class="group" id="data">
          <h3>Where Telly keeps its data</h3>
          <div class="card">
            <div class="row tall">
              <span class="what"><b>The library and subtitles</b></span>
              <span class="muted mono path">{status?.dataDir}</span>
            </div>
            <div class="row tall">
              <span class="what"><b>Telly’s copies of episodes</b><small>Library and downloads → Disk space can move them to another folder.</small></span>
              <span class="muted mono path">{status?.mediaDir}</span>
            </div>
          </div>
        </section>
      {/if}
    </main>
  </div>
</div>

<style>
  .set { position: absolute; inset: 0; overflow: auto; padding: 18px 32px 60px; }
  .cols { display: flex; flex-wrap: wrap; align-items: flex-start; gap: 24px 40px; max-width: 1180px; margin: 0 auto; }
  /* The list of pages stays put while a page scrolls beside it. */
  .side { flex: 1 1 220px; max-width: 260px; display: grid; gap: 14px; justify-items: start; position: sticky; top: 0; }
  h1 { font: 600 28px/1.2 var(--font-display); }
  .find { display: flex; align-items: center; gap: 9px; width: 100%; height: 38px; box-sizing: border-box; padding: 0 12px; border: 1px solid var(--rule-2); border-radius: 6px; background: var(--surface); color: var(--ink-3); }
  .find:focus-within { border-color: var(--ink); }
  .find :global(svg) { width: 16px; height: 16px; flex: none; }
  .find input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; font: 14px/1 var(--font-ui); color: var(--ink); }
  .pages { display: grid; gap: 2px; width: 100%; }
  .pages a { display: flex; align-items: center; gap: 10px; min-height: 38px; padding: 5px 12px; box-sizing: border-box; border-radius: 6px; font: 500 14px/1.25 var(--font-ui); color: var(--ink-2); text-decoration: none; }
  .pages a:hover { background: var(--sunk); color: var(--ink); }
  .pages a[aria-current="page"] { background: var(--sunk); color: var(--ink); font-weight: 600; }
  .pages small { display: block; font-size: 12px; font-weight: 400; color: var(--ink-3); }
  main { flex: 999 1 560px; min-width: 0; display: grid; gap: 28px; align-content: start; padding-top: 44px; }
  .head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
  .head > div { flex: 1 1 320px; display: grid; gap: 6px; }
  a.btn { text-decoration: none; }
  h2 { font: 600 22px/1.2 var(--font-ui); }
  .group { display: grid; gap: 10px; scroll-margin-top: 14px; }
  h3 { font: 600 12.5px/1.2 var(--font-ui); color: var(--ink-2); }
  .head-row { display: flex; justify-content: space-between; align-items: center; gap: 10px; flex-wrap: wrap; }
  .lede { color: var(--ink-2); font-size: 14px; max-width: 70ch; }
  .card { border: 1px solid var(--rule); border-radius: 10px; background: var(--surface); }
  .card.alert { border-color: var(--warn-dot); }
  .card.alert.bad { border-color: var(--bad-dot); }
  .row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; padding: 13px 18px; margin: 0; }
  .row + .row { border-top: 1px solid var(--rule); }
  .row.top { align-items: flex-start; }
  .row.top > .dot { margin-top: 6px; }
  /* A row whose control is too wide to sit beside its name: a path, a key. */
  .row.tall { display: grid; gap: 9px; }
  a.row { color: inherit; text-decoration: none; }
  a.row:hover { background: var(--sunk); }
  a.row:first-child { border-radius: 9px 9px 0 0; }
  a.row:last-child { border-radius: 0 0 9px 9px; }
  a.row:only-child { border-radius: 9px; }
  .what { flex: 1 1 300px; display: grid; gap: 2px; min-width: 0; }
  .what b, .name { font-weight: 600; }
  .what small, .note { font-size: 13px; color: var(--ink-2); }
  .what small.good { color: var(--ok); }
  .what small.wrong { color: var(--bad); }
  .name { flex: 0 0 210px; }
  .detail { flex: 1 1 260px; min-width: 0; font-size: 13.5px; color: var(--ink-2); }
  .go { display: grid; color: var(--ink-3); transform: scaleX(-1); }
  .go :global(svg) { width: 16px; height: 16px; }
  .ctl { flex: 0 1 300px; min-width: 0; box-sizing: border-box; }
  select.input { height: 36px; }
  .row > input[type="checkbox"] { width: 18px; height: 18px; margin: 0; accent-color: var(--ink); flex: none; }
  label.row:has(> input[type="checkbox"]) { cursor: pointer; }
  .types { display: grid; gap: 8px; }
  .inline { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }
  .inline .grow { min-width: 180px; }
  .beacon { margin-top: 8px; max-width: 520px; }
  .actions { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .muted { color: var(--ink-3); font-size: 12.5px; }
  .path { overflow-wrap: anywhere; }
  .dot { width: 8px; height: 8px; box-sizing: border-box; border-radius: 50%; background: var(--rule-2); flex: none; }
  .dot.ok { background: var(--ok-dot); }
  .dot.warn { background: var(--warn-dot); }
  .dot.bad { background: var(--bad-dot); }
  .dot.off { background: none; border: 1px solid var(--ink-3); }
  .moved { font-size: 13.5px; }
  .moved-list { display: grid; margin: 0; padding: 0; border-top: 1px solid var(--rule); }
  .moved-list li { list-style: none; display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 2px 12px; padding: 8px 2px; border-bottom: 1px solid var(--rule); }
  .moved-list .right { justify-self: end; font-variant-numeric: tabular-nums; }
  .moved-list .muted { grid-column: 1 / -1; }
  @media (max-width: 760px) {
    .side { position: static; max-width: none; }
    main { padding-top: 0; }
  }
</style>
