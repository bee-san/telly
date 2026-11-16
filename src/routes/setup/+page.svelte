<script lang="ts">
  import { goto } from "$app/navigation";
  import { onMount } from "svelte";
  import AddSheet from "$lib/AddSheet.svelte";
  import Icon from "$lib/Icon.svelte";
  import {
    call,
    callKept,
    kept,
    errorText,
    hachidoriInfo,
    inTauri,
    pickFile,
    pickFolder,
    pickFolders,
    setWindowTitle,
    type Anime,
    type ChosenFolder,
    type ClaudeStatus,
    type FilmFound,
    type HachidoriInfo,
    type HostStatus,
    type Library,
    type OwnHachidori,
    type ScanResult,
    type Settings,
    type ShowAsRead,
    type Status,
  } from "$lib/api";
  import { IN_SETTINGS, askAnki, askJimaku, checks, deck, linked, noteType, type AnkiFacts } from "$lib/checks";
  import { minutes, plural } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  /** The setup's screens, in order. */
  const SCREENS = [
    { id: "anime", name: "Your anime" },
    { id: "keys", name: "Your keys" },
    { id: "rest", name: "The rest" },
    { id: "library", name: "Your library" },
  ] as const;

  let settings = $state<Settings>(kept<Settings>("settings") ?? {});
  let status = $state<Status | null>(kept<Status>("status"));
  let claude = $state<ClaudeStatus | null>(kept<ClaudeStatus>("claudeStatus"));
  let own = $state<OwnHachidori | null>(kept<OwnHachidori>("ownHachidori"));
  let hd = $state<HachidoriInfo | null>(null);
  let anki = $state<AnkiFacts | null>(null);
  /** How many shows the library has. */
  let shows = $state(kept<Library>("library")?.shows.length ?? 0);
  let error = $state("");
  /** Telly was set up before: this is Run setup again, which can be closed. */
  let again = $state(kept<Settings>("settings")?.["setup.done"] === true);

  let at = $state(0);
  const shown = $derived(SCREENS[at].id);
  const str = (k: string, d = "") => String(settings[k] ?? d);

  /** Every part is asked for at once, and shows as it comes. */
  async function load() {
    try {
      await Promise.all([
        callKept<Settings>("settings").then((s) => {
          settings = s;
          jimakuKey = String(s["jimaku.key"] ?? "");
          again = s["setup.done"] === true;
          // Begun: a show added before the end doesn't make it done (see Backend::open).
          if (!again) call("setSetting", { key: "setup.done", value: false }).catch(() => {});
          askJimaku(s).then((j) => (jimaku = j));
          askAnki(s).then((a) => (anki = a));
          askRouter();
        }),
        callKept<Status>("status").then((s) => (status = s)),
        callKept<ClaudeStatus>("claudeStatus").then((c) => (claude = c)),
        callKept<OwnHachidori>("ownHachidori").then((o) => (own = o)),
        callKept<Library>("library").then((l) => (shows = l.shows.length)),
        hachidoriInfo().then((h) => (hd = h)),
      ]);
    } catch (e) {
      error = errorText(e);
    }
  }

  onMount(() => {
    app.dark = false;
    setWindowTitle("Setup — Telly");
    load();
    const stop: (() => void)[] = [];
    let left = false;
    /** Stops listening when the page is left, or at once when it was left before the listening began. */
    const until = (u: () => void) => {
      if (left) u();
      else stop.push(u);
    };
    if (inTauri) {
      import("@tauri-apps/api/event")
        .then(({ listen }) =>
          listen<HostStatus>("hachidori-status", (e) => {
            if (hd) hd.status = e.payload;
          }),
        )
        .then(until)
        // Without it, Hachidori's status stays as it was when the page opened.
        .catch(() => {});
      // Folders dropped on the first screen are chosen.
      import("@tauri-apps/api/webview")
        .then(({ getCurrentWebview }) =>
          getCurrentWebview().onDragDropEvent((e) => {
            if (shown !== "anime") return;
            if (e.payload.type === "drop") {
              dropping = false;
              choose(e.payload.paths);
            } else dropping = e.payload.type !== "leave";
          }),
        )
        .then(until)
        // Without it, folders are chosen with the button.
        .catch(() => {});
    }
    // Anki opened, Beacon heard from, a dictionary installed: each shows within 5 seconds.
    const timer = setInterval(async () => {
      status = await call<Status>("status").catch(() => status);
      own = await call<OwnHachidori>("ownHachidori").catch(() => own);
      if (status?.anki && !anki) anki = await askAnki(settings);
    }, 5000);
    return () => {
      left = true;
      reading = false;
      clearInterval(timer);
      for (const u of stop) u();
    };
  });

  async function set(k: string, value: unknown) {
    settings[k] = value;
    try {
      await call("setSetting", { key: k, value });
    } catch (e) {
      toast({ key: "set", state: "failed", title: "Not saved", text: errorText(e) });
      return;
    }
    if (k === "jimaku.key") {
      jimaku = null;
      jimaku = await askJimaku(settings);
    }
    if (k.startsWith("anki.")) {
      status = await call<Status>("status").catch(() => status);
      anki = await askAnki(settings);
    }
    if (k === "openrouter.key") {
      claude = await callKept<ClaudeStatus>("claudeStatus").catch(() => claude);
      await askRouter();
    }
  }

  /** Asks again about everything. */
  async function checkAgain() {
    jimaku = null;
    anki = null;
    claude = null;
    await load();
  }

  /** Done: the Library from now on. */
  async function finish() {
    await call("setSetting", { key: "setup.done", value: true }).catch(() => {});
    goto("/");
  }

  // ---- Your anime ----

  /** One thing Telly found in a folder you chose, to add as it is: an anime's folder with its seasons and movies, or a film. */
  interface Find {
    path: string;
    name: string;
    /** The anime AniList has it as; null when no name there is much like its own. */
    anime: Anime | null;
    /** The shows it's added as when nothing is changed. */
    asRead: ShowAsRead[];
    /** "3 seasons · 1 movie · 40 episodes". */
    holds: string;
    /** Its folder's scan, for Choose show… to open on; none for a film. */
    scan: Promise<ScanResult> | null;
    /** Why AniList couldn't be asked about it, or it couldn't be added. */
    error: string;
    include: boolean;
    added: boolean;
  }

  /** A folder chosen on the first screen, and what Telly has found in it so far. */
  interface Chosen {
    path: string;
    state: "waiting" | "reading" | "read" | "error";
    error: string;
    finds: Find[];
    /** What's in it that's in Telly already. */
    have: number;
    /** What's still to be read in it. */
    left: number;
  }

  let chosen = $state<Chosen[]>([]);
  /** A folder is being dragged over the window. */
  let dropping = $state(false);
  /** Whether folders are being read; false stops the reading where it is. */
  let reading = $state(false);
  /** The find whose show is being chosen (AddSheet). */
  let choosing = $state<Find | null>(null);

  const finds = $derived(chosen.flatMap((c) => c.finds));
  const unread = $derived(chosen.filter((c) => c.state === "waiting" || c.state === "reading"));

  async function chooseFolders() {
    choose(await pickFolders("Choose the folders your anime is in"));
  }

  /** Takes the folders chosen, and reads them one after another while the setup goes on. */
  function choose(paths: string[]) {
    for (const path of paths) {
      if (!chosen.some((c) => c.path === path)) chosen.push({ path, state: "waiting", error: "", finds: [], have: 0, left: 0 });
    }
    if (!reading) readOn();
  }

  function takeOut(c: Chosen) {
    chosen = chosen.filter((x) => x.path !== c.path);
  }

  /** "2 seasons · 1 movie · 40 episodes" for an anime's folder; "Movie · 1 h 46 min" for a film; "28 episodes" for one show. */
  function holdsOf(asRead: ShowAsRead[], kinds: string[]): string {
    const episodes = asRead.reduce((n, s) => n + s.episodes.length, 0);
    const first = asRead[0]?.episodes[0];
    if (asRead.length === 1 && episodes === 1 && first && first.duration >= 40 * 60) return `Movie · ${minutes(first.duration)}`;
    if (asRead.length < 2) return plural(episodes, "episode");
    const count = (kind: string, one: string, many?: string) => {
      const n = kinds.filter((k) => k === kind).length;
      return n ? [plural(n, one, many)] : [];
    };
    return [...count("season", "season"), ...count("movie", "movie"), ...count("ova", "OVA"), ...count("specials", "set of specials", "sets of specials"), plural(episodes, "episode")].join(" · ");
  }

  /** What a folder's scan found, as something to add. */
  function findOf(path: string, r: ScanResult, scan: Promise<ScanResult>): Find {
    const kinds = r.parts.filter((p) => p.files.some((f) => f.number !== null)).map((p) => p.kind);
    const name = r.anime ? r.anime.english || r.anime.romaji : r.scan.guess || path.split(/[\\/]/).filter(Boolean).pop() || path;
    return { path, name, anime: r.anime, asRead: r.asRead, holds: holdsOf(r.asRead, kinds), scan, error: r.error ?? "", include: true, added: false };
  }

  /** Reads one folder of anime, or one film, in a folder you chose. */
  async function readOne(folder: string, path: string, film: boolean): Promise<Find | null> {
    if (film) {
      const f = await call<FilmFound | null>("scanFilm", { folder, path });
      return f && { path, name: f.anime ? f.anime.english || f.anime.romaji : f.title, anime: f.anime, asRead: f.asRead, holds: holdsOf(f.asRead, []), scan: null, error: "", include: true, added: false };
    }
    const scan = call<ScanResult>("scan", { folder: path, sure: true });
    const r = await scan;
    return r.asRead.length ? findOf(path, r, scan) : null;
  }

  /** Reads the folders that are waiting, one at a time, each anime or film in them one at a time. */
  async function readOn() {
    reading = true;
    for (let c = chosen.find((x) => x.state === "waiting"); c && reading; c = chosen.find((x) => x.state === "waiting")) {
      const folder = c.path;
      c.state = "reading";
      try {
        const inside = await call<ChosenFolder>("chosenFolder", { folder });
        c.have = inside.have;
        const todo = [...inside.folders.map((path) => ({ path, film: false })), ...inside.videos.map((path) => ({ path, film: true }))];
        c.left = todo.length;
        for (const t of todo) {
          // Stopped, or the folder was taken out.
          if (!reading || !chosen.some((x) => x.path === folder)) break;
          try {
            const find = await readOne(folder, t.path, t.film);
            if (find) c.finds.push(find);
          } catch {
            // A folder with no videos in it is no show.
          }
          c.left -= 1;
        }
        c.state = "read";
      } catch (e) {
        c.error = errorText(e);
        c.state = "error";
      }
    }
    // Stopped: what wasn't read isn't waited for.
    for (const c of chosen) if (c.state === "waiting" || c.state === "reading") c.state = "read";
    reading = false;
  }

  /** Asks AniList again about a find it couldn't be asked about. */
  async function readAgain(f: Find) {
    const c = chosen.find((x) => x.finds.some((y) => y.path === f.path));
    if (!c) return;
    f.error = "";
    try {
      const again = await readOne(c.path, f.path, f.scan === null);
      if (again) c.finds[c.finds.findIndex((y) => y.path === f.path)] = again;
    } catch (e) {
      f.error = errorText(e);
    }
  }

  async function chooseFfmpeg() {
    const path = await pickFile("Choose ffmpeg.exe", ["exe"], "ffmpeg");
    if (!path) return;
    await set("tools.ffmpeg", path);
    await lookAgain();
  }

  async function lookAgain() {
    try {
      status = await call<Status>("findTools");
    } catch (e) {
      toast({ key: "tools", state: "failed", title: "Couldn’t look for ffmpeg", text: errorText(e) });
    }
  }

  // ---- Your keys ----

  /** Whether Jimaku takes the key: "" when it does, else why not; null while it's asked or there's none. */
  let jimaku = $state<string | null>(null);
  let jimakuKey = $state(String(kept<Settings>("settings")?.["jimaku.key"] ?? ""));
  /** Whether OpenRouter takes the key, as for Jimaku. */
  let router = $state<string | null>(null);
  let anilistAsking = $state(false);
  let anilistError = $state("");
  /** A key that's been checked is shown again to be changed. */
  let changing = $state<Record<string, boolean>>({});

  type KeyState = "none" | "checking" | "ok" | "bad";
  const stateOf = (key: string, takes: string | null): KeyState => (!key ? "none" : takes === null ? "checking" : takes === "" ? "ok" : "bad");
  const jimakuState = $derived(stateOf(str("jimaku.key"), jimaku));
  const routerState = $derived(stateOf(str("openrouter.key"), router));
  const anilistState = $derived<KeyState>(str("anilist.user") ? "ok" : anilistAsking ? "checking" : anilistError ? "bad" : "none");
  /** Claude Code is on this computer, so Explain and Previously on need no OpenRouter key. */
  const claudeFound = $derived(!!claude?.available && !claude.openrouter);
  /** OpenRouter's key is asked for, but needn't be given, while Claude Code is found. */
  const routerDone = $derived(routerState === "ok" || (claudeFound && routerState === "none"));
  const keysNeeded = $derived(claudeFound ? 2 : 3);
  const keysChecked = $derived([jimakuState === "ok", anilistState === "ok", !claudeFound && routerState === "ok"].filter(Boolean).length);
  const keysDone = $derived(jimakuState === "ok" && anilistState === "ok" && routerDone);
  const toGo = $derived(["None", "One", "Two", "Three"][keysNeeded - keysChecked] ?? "");

  async function askRouter() {
    if (!str("openrouter.key")) return void (router = null);
    router = null;
    router = await call("checkOpenRouter").then(() => "", errorText);
  }

  async function setKey(k: "jimaku.key" | "openrouter.key", value: string) {
    changing[k] = false;
    await set(k, value.trim());
  }

  /** Telly's app on AniList, whose page shows an access token to paste here. */
  const ANILIST_CLIENT_ID = "56672";

  async function anilistSignIn(token: string) {
    if (!token.trim()) return;
    anilistAsking = true;
    anilistError = "";
    try {
      await call("anilistSignIn", { token: token.trim() });
      settings = await call<Settings>("settings");
    } catch (e) {
      anilistError = errorText(e);
    }
    anilistAsking = false;
  }

  async function anilistSignOut() {
    try {
      await call("anilistSignOut");
      settings = await call<Settings>("settings");
    } catch (e) {
      toast({ key: "anilist", state: "failed", title: "Not signed out", text: errorText(e) });
    }
  }

  /** Takes what's on the clipboard as a key; when Telly can't read it, the box is ready for Ctrl+V. */
  async function paste(e: MouseEvent, take: (text: string) => void) {
    const input = (e.currentTarget as HTMLElement).parentElement?.querySelector("input");
    try {
      const text = (await navigator.clipboard.readText()).trim();
      if (text) return take(text);
    } catch {
      toast({ key: "paste", state: "info", title: "Press Ctrl+V to paste it", text: "Telly couldn’t read the clipboard itself." });
    }
    input?.focus();
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

  // ---- The rest ----

  const all = $derived(checks({ settings, status, jimaku, anki, own, hd, claude }));
  /** What Telly found by itself: every check but the keys', which have their own screen. */
  const rest = $derived(all.filter((c) => c.id !== "jimaku" && c.id !== "anilist"));

  /** The card's parts Telly fills, each with the setting its field is kept in. */
  const FIELDS = [
    { key: "anki.field.sentence", part: "sentence", name: "The line" },
    { key: "anki.field.picture", part: "picture", name: "Its moving picture" },
    { key: "anki.field.audio", part: "audio", name: "Its audio" },
    { key: "anki.field.info", part: "info", name: "Where it’s from" },
  ] as const;
  /** Anki has no note type of the name Telly has for cards. */
  const noType = $derived(!!status?.anki && !!anki && !anki.noteTypes.includes(noteType(settings)));
  /** The card's parts whose field the note type doesn't have, so Telly can't match it by name. */
  const noField = $derived(status?.anki && anki && !noType ? FIELDS.filter((f) => !anki?.fields.includes(status?.fields[f.part] ?? "")) : []);
  const noDeck = $derived(all.some((c) => c.id === "deck" && c.state === "warn"));
  const noDictionaries = $derived(!!own && !linked(hd) && (!own.folder || !own.total));
  const noFfmpeg = $derived(!!status && !status.tools.ffmpeg);
  /** How many things Telly couldn't work out by itself, and asks. */
  const asked = $derived([noFfmpeg, noType || noField.length > 0 || noDeck, noDictionaries].filter(Boolean).length);

  async function chooseDictionaries() {
    const folder = await pickFolder("Choose the dictionaries folder");
    if (!folder) return;
    await set("hachidori.dictionaries", folder);
    own = await call<OwnHachidori>("ownHachidori").catch(() => own);
  }

  // ---- Your library ----

  /** Finds no name on AniList is much like, or AniList couldn't be asked about: looked at before they're added. */
  const unsure = $derived(finds.filter((f) => !f.added && (!f.anime || f.error)));
  const matched = $derived(finds.filter((f) => !f.added && f.anime && !f.error));
  const toAdd = $derived(finds.filter((f) => f.include && !f.added && !f.error));
  /** How many of the matched are listed until Show the others is pressed. */
  const FIRST = 12;
  let showAll = $state(false);
  /** How far adding has got; null when nothing is being added. */
  let adding = $state<{ done: number; of: number } | null>(null);
  /** How many couldn't be added, the last time. */
  let failed = $state(0);

  /** Adds everything that's ticked, each as Add media… would when nothing is changed, then opens Telly. */
  async function addAll() {
    const list = toAdd;
    adding = { done: 0, of: list.length };
    failed = 0;
    for (const f of list) {
      try {
        await call("addShows", { shows: f.asRead, anime: f.anime?.id ?? null });
        f.added = true;
      } catch (e) {
        f.error = errorText(e);
        failed += 1;
      }
      adding.done += 1;
    }
    adding = null;
    if (!failed) finish();
  }
</script>

{#snippet dot(state: string)}
  {#if state === "wait"}<span class="spin"></span>{:else}<i class="dot" class:ok={state === "ok"} class:warn={state === "warn"} class:bad={state === "bad"} class:off={state === "off"}></i>{/if}
{/snippet}

{#snippet mark(n: number, state: KeyState)}
  {#if state === "ok"}
    <span class="mark ok"><Icon name="check" /></span>
  {:else if state === "checking"}
    <span class="mark"><span class="spin big"></span></span>
  {:else}
    <span class="mark num" class:bad={state === "bad"}>{n}</span>
  {/if}
{/snippet}

<div class="setup scroll">
  <header class="top">
    <span class="title">Setting up Telly</span>
    <ol class="steps" aria-label="Setup">
      {#each SCREENS as s, i (s.id)}
        <li>
          <button type="button" class="step" aria-current={i === at ? "step" : undefined} disabled={i > 1 && !keysDone && !again} onclick={() => (at = i)}>
            <span class="n" class:done={i < at}>{#if i < at}<Icon name="check" />{:else}{i + 1}{/if}</span><span>{s.name}</span>
          </button>
        </li>
      {/each}
    </ol>
    {#if again}<button type="button" class="link-btn" onclick={() => goto("/settings")}>Close setup</button>{/if}
  </header>

  <main>
    {#if error}<p class="err">{error}</p>{/if}

    {#if shown === "anime"}
      <div class="intro">
        <h1>Where’s your anime?</h1>
        <p class="lede">Choose the folders your rips are in. Telly reads them while you do the rest, and never moves or changes them.</p>
      </div>
      {#if noFfmpeg}
        <section class="card ask">
          <div class="q">
            <h2>Telly can’t find ffmpeg, which it reads your rips with</h2>
            <p>It looks on PATH and where GameSentenceMiner keeps it. Choose ffmpeg.exe (its folder needs ffprobe.exe too).</p>
          </div>
          <div class="actions">
            <button type="button" class="btn" onclick={chooseFfmpeg}>Choose ffmpeg.exe…</button>
            <button type="button" class="btn quiet" onclick={lookAgain}>Look again</button>
          </div>
        </section>
      {/if}
      <div class="drop" class:over={dropping}>
        <span class="drop-ic"><Icon name="folder" /></span>
        {#if inTauri}<b>Drop folders here</b>{/if}
        <button type="button" class="btn big" onclick={chooseFolders}>Choose folders…</button>
        <span class="muted">A folder of anime, a folder of films, or one show’s folder. Add as many as you like.</span>
      </div>
      {#if chosen.length}
        <div class="card">
          {#each chosen as c (c.path)}
            <div class="row">
              {#if c.state === "error"}<span class="state bad"><Icon name="warn" /></span>{:else if c.state === "read"}<span class="state ok"><Icon name="check" /></span>{:else}<span class="spin"></span>{/if}
              <span class="what">
                <span class="mono path">{c.path}</span>
                <small class:wrong={c.state === "error"}>
                  {#if c.state === "error"}{c.error}
                  {:else if c.state === "waiting"}Waiting to be read
                  {:else if c.state === "reading"}Reading… {c.finds.length} found so far{#if c.left}, {c.left} to go{/if}
                  {:else if c.finds.length}{c.finds.length === 1 ? `${c.finds[0].name} · ${c.finds[0].holds}` : `${c.finds.length} found`}{#if c.have} · {c.have} in Telly already{/if}
                  {:else if c.have}All of it is in Telly already
                  {:else}No videos Telly can read in it{/if}
                </small>
              </span>
              <button type="button" class="btn quiet small" onclick={() => takeOut(c)}>Take out</button>
            </div>
          {/each}
        </div>
        <p class="muted">You’ll check what Telly found at the end, before anything is added.</p>
      {:else if shows}
        <p class="muted">Your library has {plural(shows, "show")}.</p>
      {/if}
    {:else if shown === "keys"}
      <div class="intro">
        <h1>{claudeFound ? "Two keys, and one more if you like" : "Three keys"}</h1>
        <p class="lede">Telly needs a key from each of these. <b>Get a key</b> opens the page that has it. Copy it, come back, and press <b>Paste</b>. Telly checks each one as it arrives.</p>
      </div>

      <section class="card key" aria-label="Jimaku">
        <div class="key-head">
          {@render mark(1, jimakuState)}
          <span class="what"><b>Jimaku</b><small>Finds Japanese subtitles for each episode. Free.</small></span>
          {#if jimakuState === "ok" && !changing["jimaku.key"]}
            <span class="took">Jimaku takes this key</span>
            <button type="button" class="btn quiet small" onclick={() => (changing["jimaku.key"] = true)}>Change</button>
          {:else if jimakuState === "checking"}
            <span class="muted">Asking Jimaku about your key…</span>
          {/if}
        </div>
        {#if jimakuState === "none" || jimakuState === "bad" || changing["jimaku.key"]}
          {#if jimakuState === "bad"}<p class="err indent">{jimaku}</p>{/if}
          <div class="key-row">
            <button type="button" class="btn" onclick={() => openLink("https://jimaku.cc/account")}>Get a key</button>
            <input class="input mono grow" type="password" placeholder="Then paste it here" aria-label="Jimaku API key" bind:value={jimakuKey} onchange={() => setKey("jimaku.key", jimakuKey)} />
            <button type="button" class="btn quiet" onclick={(e) => paste(e, (text) => { jimakuKey = text; setKey("jimaku.key", text); })}>Paste</button>
          </div>
        {/if}
      </section>

      <section class="card key" aria-label="AniList">
        <div class="key-head">
          {@render mark(2, anilistState)}
          <span class="what"><b>AniList</b><small>Puts what you watch on your list, and suggests what to watch next from it. Free.</small></span>
          {#if anilistState === "ok"}
            <span class="took">Signed in as {str("anilist.user")}</span>
            <button type="button" class="btn quiet small" onclick={anilistSignOut}>Sign out</button>
          {:else if anilistState === "checking"}
            <span class="muted">Asking AniList about your token…</span>
          {/if}
        </div>
        {#if anilistState === "none" || anilistState === "bad"}
          {#if anilistState === "bad"}<p class="err indent">{anilistError}</p>{/if}
          <div class="key-row">
            <button type="button" class="btn" onclick={() => openLink(`https://anilist.co/api/v2/oauth/authorize?client_id=${ANILIST_CLIENT_ID}&response_type=token`)}>Get a token</button>
            <input class="input mono grow" type="password" placeholder="Then paste it here" aria-label="AniList access token" onchange={(e) => anilistSignIn(e.currentTarget.value)} />
            <button type="button" class="btn quiet" onclick={(e) => paste(e, anilistSignIn)}>Paste</button>
          </div>
          <p class="muted indent">AniList shows the token once you approve Telly.</p>
        {/if}
      </section>

      <section class="card key" aria-label="OpenRouter">
        <div class="key-head">
          {@render mark(3, routerState)}
          <span class="what">
            <b>OpenRouter{#if claudeFound}<span class="optional">Optional</span>{/if}</b>
            <small>
              Makes subtitles for episodes Jimaku has nothing for, about 4 cents an episode, and nothing until then.
              {#if claudeFound}Claude Code is on this computer and answers Explain and Previously on, so Telly works without this key: Whisper on this computer makes those subtitles instead.
              {:else if claude}Claude Code isn’t on this computer, so it also answers Explain and Previously on.
              {:else}Telly is looking for Claude Code: with it on this computer, this key is optional.{/if}
            </small>
          </span>
          {#if routerState === "ok" && !changing["openrouter.key"]}
            <span class="took">OpenRouter takes this key</span>
            <button type="button" class="btn quiet small" onclick={() => (changing["openrouter.key"] = true)}>Change</button>
          {:else if routerState === "checking"}
            <span class="muted">Asking OpenRouter about your key…</span>
          {/if}
        </div>
        {#if routerState === "none" || routerState === "bad" || changing["openrouter.key"]}
          {#if routerState === "bad"}<p class="err indent">{router}</p>{/if}
          <div class="key-row">
            <button type="button" class="btn" class:quiet={claudeFound} onclick={() => openLink("https://openrouter.ai/settings/keys")}>Get a key</button>
            <input class="input mono grow" type="password" placeholder="Then paste it here" aria-label="OpenRouter API key" value={str("openrouter.key")} onchange={(e) => setKey("openrouter.key", e.currentTarget.value)} />
            <button type="button" class="btn quiet" onclick={(e) => paste(e, (text) => setKey("openrouter.key", text))}>Paste</button>
          </div>
        {/if}
      </section>

      <p class="muted">Your keys are kept on this computer, and each goes only to the service it’s for.</p>
    {:else if shown === "rest"}
      <div class="intro">
        <h1>Telly found the rest itself</h1>
        <p class="lede">It looked for ffmpeg, Anki, your dictionaries and Claude Code. It asks only when it can’t work something out{asked ? `, and here that’s ${asked === 1 ? "one thing" : `${asked} things`}` : ", and here there’s nothing to ask"}.</p>
      </div>

      {#if noFfmpeg}
        <section class="card ask">
          <div class="q">
            <h2>Where’s ffmpeg?</h2>
            <p>Telly plays and converts your rips with it, and can’t find it on PATH or where GameSentenceMiner keeps it. Choose ffmpeg.exe (its folder needs ffprobe.exe too).</p>
          </div>
          <div class="actions">
            <button type="button" class="btn" onclick={chooseFfmpeg}>Choose ffmpeg.exe…</button>
            <button type="button" class="btn quiet" onclick={lookAgain}>Look again</button>
          </div>
        </section>
      {/if}
      {#if anki && (noType || noField.length || noDeck)}
        <section class="card ask">
          <div class="q">
            <h2>{noType ? "Which note type do your cards use?" : noField.length ? "Which fields does Telly fill?" : "Which deck do new cards go in?"}</h2>
            <p>
              {#if noType}Telly expected one called {noteType(settings)}, and your Anki doesn’t have it.
              {:else if noField.length}{noteType(settings)} has no field of the name Telly expected for {plural(noField.length, "part")} of a card.
              {:else}Your Anki has no deck called {deck(settings)}.{/if}
            </p>
          </div>
          <div class="fields">
            {#if noType}
              <label class="field"><span>Note type</span>
                <select class="input" value="" onchange={(e) => set("anki.noteType", e.currentTarget.value)}>
                  <option value="" disabled>Choose one</option>
                  {#each anki.noteTypes as t (t)}<option value={t}>{t}</option>{/each}
                </select>
              </label>
            {/if}
            {#each noField as f (f.key)}
              <label class="field"><span>{f.name}</span>
                <select class="input" value="" onchange={(e) => set(f.key, e.currentTarget.value)}>
                  <option value="" disabled>Choose its field</option>
                  {#each anki.fields as name (name)}<option value={name}>{name}</option>{/each}
                </select>
              </label>
            {/each}
            {#if noDeck}
              <label class="field"><span>Deck for new cards</span>
                <select class="input" value="" onchange={(e) => set("anki.deck", e.currentTarget.value)}>
                  <option value="" disabled>Choose one</option>
                  {#each anki.decks as d (d)}<option value={d}>{d}</option>{/each}
                </select>
              </label>
            {/if}
          </div>
        </section>
      {/if}
      {#if noDictionaries}
        <section class="card ask">
          <div class="q">
            <h2>Where are your dictionaries?</h2>
            <p>{own?.folder ? `There are no Yomitan dictionaries (.zip files) in ${own.folder}.` : "Telly looks words up in your Yomitan dictionaries (.zip files), and installs every one in the folder you choose."}</p>
          </div>
          <div class="actions">
            <button type="button" class="btn" onclick={chooseDictionaries}>Choose folder…</button>
          </div>
        </section>
      {/if}

      <section class="card" aria-label="What Telly found">
        {#each rest as c (c.id)}
          <div class="row base">
            {@render dot(c.state)}
            <b class="name">{IN_SETTINGS[c.id]?.name}</b>
            <span class="detail">{c.text}</span>
          </div>
        {/each}
      </section>
      <p class="muted">Anything here can be changed later, in Settings.</p>
    {:else}
      <div class="intro">
        <h1>{finds.length || reading ? "Here’s what Telly found" : "Nothing to add yet"}</h1>
        <p class="lede">
          {#if finds.length}
            {plural(finds.length, "anime or film", "anime and films")} in your {chosen.length === 1 ? "folder" : `${chosen.length} folders`}. Untick anything you don’t want. You can change a match later, from the show’s own page.
          {:else if reading}
            Telly is still reading your folders.
          {:else}
            {chosen.length ? "Telly found no videos it can read in the folders you chose." : "You chose no folders."} <b>Add media…</b> in the Library adds a folder any time.
          {/if}
        </p>
      </div>
      {#if reading}
        <div class="card">
          <div class="row">
            <span class="spin"></span>
            <span class="what"><span>Still reading <span class="mono">{unread[0]?.path}</span></span><small>{finds.length} found so far{#if unread[0]?.left}, {unread[0].left} to go in this folder{/if}{#if unread.length > 1}, and {plural(unread.length - 1, "more folder")} after it{/if}</small></span>
            <button type="button" class="btn quiet small" onclick={() => (reading = false)} title="What isn’t read yet isn’t added. Add media… in the Library adds it later.">Stop reading</button>
          </div>
        </div>
      {/if}

      {#each unsure as f (f.path)}
        <section class="card flag">
          <div class="row">
            <input type="checkbox" bind:checked={f.include} disabled={!!f.error} aria-label="Add {f.name}" />
            <span class="what">
              <b>{f.error ? `Telly couldn’t ask AniList about ${f.name}` : "Telly isn’t sure which show this is"}</b>
              <span class="mono path">{f.path}</span>
              <small class:wrong={!!f.error}>{f.error || `Left as it is, it’s added as “${f.name}”, without an AniList entry. ${f.holds}.`}</small>
            </span>
            {#if f.error}
              <button type="button" class="btn quiet" onclick={() => readAgain(f)}>Try again</button>
            {:else if f.scan}
              <button type="button" class="btn quiet" onclick={() => (choosing = f)}>Choose show…</button>
            {/if}
          </div>
        </section>
      {/each}

      {#if matched.length}
        <section class="found">
          <div class="found-head">
            <h2>Matched on AniList · {matched.length}</h2>
            <span class="grow"></span>
            <button type="button" class="link-btn" onclick={() => { const on = matched.some((f) => !f.include); for (const f of matched) f.include = on; }}>{matched.some((f) => !f.include) ? "Tick all" : "Untick all"}</button>
          </div>
          <div class="grid">
            {#each showAll ? matched : matched.slice(0, FIRST) as f (f.path)}
              <label class="item">
                <input type="checkbox" bind:checked={f.include} />
                {#if f.anime?.cover}<img src={f.anime.cover} alt="" />{:else}<span class="noimg"></span>{/if}
                <span class="item-text">
                  <span class="jp" lang="ja">{f.anime?.native || f.name}</span>
                  <span class="en">{f.name}</span>
                  <span class="holds">{f.holds}</span>
                </span>
              </label>
            {/each}
          </div>
          {#if !showAll && matched.length > FIRST}
            <button type="button" class="btn quiet more" onclick={() => (showAll = true)}>Show the other {matched.length - FIRST}</button>
          {/if}
        </section>
      {/if}
      {#if failed}
        <p class="err">{failed} couldn’t be added: why is beside each above. Try again, or open Telly and add {failed === 1 ? "it" : "them"} later with Add media….</p>
      {/if}
    {/if}
  </main>

  <footer>
    <div class="foot">
      {#if at === 0}
        <span class="muted grow">Used Telly on another computer? <button type="button" class="link-btn" onclick={() => goto("/settings#backup")}>Restore its backup…</button></span>
        {#if !chosen.length && !shows}<button type="button" class="link-btn" onclick={() => (at = 1)}>I’ll add my anime later</button>{/if}
        <button type="button" class="btn big" disabled={!chosen.length && !shows} onclick={() => (at = 1)}>Next: Your keys</button>
      {:else if at === 1}
        <button type="button" class="btn quiet big" onclick={() => (at = 0)}>Back</button>
        <span class="muted grow right">{keysDone ? "All checked." : `${keysChecked} of ${keysNeeded} checked. ${toGo} to go.`}</span>
        <button type="button" class="btn big" disabled={!keysDone} onclick={() => (at = 2)}>Next: The rest</button>
      {:else if at === 2}
        <button type="button" class="btn quiet big" onclick={() => (at = 1)}>Back</button>
        <button type="button" class="btn quiet big" onclick={checkAgain}>Check again</button>
        <span class="grow"></span>
        <button type="button" class="btn big" onclick={() => (at = 3)}>Next: Your library</button>
      {:else}
        <button type="button" class="btn quiet big" disabled={!!adding} onclick={() => (at = 2)}>Back</button>
        <span class="muted grow right">{toAdd.length ? "Telly gets each one ready in the background, next episode first." : ""}</span>
        {#if failed}<button type="button" class="btn quiet big" onclick={finish}>Open Telly</button>{/if}
        {#if adding}
          <button type="button" class="btn big" disabled>Adding {Math.min(adding.done + 1, adding.of)} of {adding.of}…</button>
        {:else if toAdd.length}
          <button type="button" class="btn big" disabled={reading} onclick={addAll} title={reading ? "Telly is still reading your folders" : undefined}>Add {toAdd.length > 1 && toAdd.length === finds.filter((f) => !f.added).length ? `all ${toAdd.length}` : toAdd.length} and open Telly</button>
        {:else}
          <button type="button" class="btn big" disabled={reading} onclick={finish}>Open Telly</button>
        {/if}
      {/if}
    </div>
  </footer>
</div>

{#if choosing}
  {@const f = choosing}
  <AddSheet folder={f.path} scanned={f.scan ?? undefined} onclose={() => (choosing = null)} onadded={() => { f.added = true; choosing = null; }} />
{/if}

<style>
  .setup { position: absolute; inset: 0; overflow: auto; display: flex; flex-direction: column; }
  .top { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 24px; padding: 10px 32px; border-bottom: 1px solid var(--rule); }
  .title { font: 700 16px/1 var(--font-ui); }
  .steps { list-style: none; margin: 0 0 0 auto; padding: 0; display: flex; flex-wrap: wrap; gap: 4px 6px; }
  .step { display: inline-flex; align-items: center; gap: 8px; min-height: 38px; padding: 0 12px; border: 0; border-radius: 6px; background: none; font: 500 13.5px/1.2 var(--font-ui); color: var(--ink-2); cursor: pointer; }
  .step:hover:not(:disabled) { background: var(--sunk); color: var(--ink); }
  .step:disabled { cursor: default; opacity: .55; }
  .step[aria-current="step"] { background: var(--sunk); color: var(--ink); font-weight: 600; }
  .n { display: grid; place-items: center; width: 22px; height: 22px; box-sizing: border-box; border-radius: 50%; border: 1px solid var(--ink-3); font: 700 12px/1 var(--font-ui); }
  .n.done { border-color: var(--ok); color: var(--ok); }
  .n :global(svg) { width: 13px; height: 13px; }
  .step[aria-current="step"] .n { background: var(--btn); border-color: var(--btn); color: var(--btn-ink); }
  main { flex: 1; width: 100%; max-width: 840px; box-sizing: border-box; margin: 0 auto; padding: 48px 32px 32px; display: grid; gap: 20px; align-content: start; }
  .intro { display: grid; gap: 8px; }
  h1 { font: 600 34px/1.15 var(--font-display); }
  .lede { color: var(--ink-2); font-size: 15px; max-width: 64ch; }
  .lede b { color: var(--ink); font-weight: 600; }
  .muted { color: var(--ink-2); font-size: 13px; }
  .right { text-align: right; }
  .drop { display: grid; justify-items: center; gap: 12px; padding: 30px 24px; border: 1.5px dashed var(--rule-2); border-radius: 12px; text-align: center; }
  .drop.over { border-color: var(--ink); background: var(--surface); }
  .drop b { font: 600 15px/1.3 var(--font-ui); }
  .drop-ic { display: grid; color: var(--ink-2); }
  .drop-ic :global(svg) { width: 34px; height: 34px; stroke-width: 1.2; }
  .btn.big { height: 44px; padding: 0 20px; font-size: 14.5px; }
  .card { border: 1px solid var(--rule); border-radius: 10px; background: var(--surface); }
  .card.ask { display: grid; gap: 14px; padding: 18px; border-color: var(--ink-3); }
  .card.flag { border-color: var(--warn-dot); }
  .q { display: grid; gap: 3px; }
  .q h2 { font: 600 16.5px/1.3 var(--font-ui); }
  .q p { font-size: 13.5px; color: var(--ink-2); }
  .actions { display: flex; flex-wrap: wrap; gap: 10px; align-items: center; }
  .fields { display: flex; flex-wrap: wrap; gap: 12px 16px; }
  .fields .field { flex: 1 1 240px; min-width: 0; }
  .fields select { height: 40px; }
  .row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; padding: 13px 18px; }
  .row + .row { border-top: 1px solid var(--rule); }
  .row.base { align-items: baseline; }
  .row > input[type="checkbox"], .item > input { width: 16px; height: 16px; margin: 0; accent-color: var(--ink); flex: none; }
  .what { flex: 1 1 300px; display: grid; gap: 2px; min-width: 0; }
  .what b { font-weight: 600; }
  .what small { font-size: 13px; color: var(--ink-2); }
  .what small.wrong { color: var(--bad); }
  .path { font-size: 12.5px; overflow-wrap: anywhere; }
  .name { flex: 0 0 200px; font-weight: 600; }
  .detail { flex: 1 1 300px; min-width: 0; font-size: 13.5px; color: var(--ink-2); }
  .state { display: grid; }
  .state :global(svg) { width: 18px; height: 18px; }
  .state.ok { color: var(--ok); }
  .state.bad { color: var(--bad); }
  .dot { width: 8px; height: 8px; box-sizing: border-box; border-radius: 50%; background: var(--rule-2); flex: none; }
  .dot.ok { background: var(--ok-dot); }
  .dot.warn { background: var(--warn-dot); }
  .dot.bad { background: var(--bad-dot); }
  .dot.off { background: none; border: 1px solid var(--ink-3); }
  .key { display: grid; gap: 12px; padding: 16px 18px; }
  .key-head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 16px; }
  .key .what b { font-size: 15.5px; }
  .key .what small { font-size: 13.5px; }
  .optional { margin-left: 10px; padding: 2px 7px; border: 1px solid var(--rule-2); border-radius: 4px; font: 600 11.5px/1.3 var(--font-ui); color: var(--ink-2); vertical-align: 1px; }
  .mark { display: grid; place-items: center; width: 28px; height: 28px; box-sizing: border-box; border-radius: 50%; flex: none; }
  .mark.num { border: 1px solid var(--ink-3); font: 700 13px/1 var(--font-ui); }
  .mark.num.bad { border-color: var(--bad-dot); color: var(--bad); }
  .mark.ok { background: var(--ok-dot); color: var(--surface); }
  .mark :global(svg) { width: 16px; height: 16px; stroke-width: 2.6; }
  .spin.big { width: 18px; height: 18px; }
  .took { font: 600 13.5px/1.3 var(--font-ui); color: var(--ok); }
  .key-row { display: flex; flex-wrap: wrap; gap: 8px; padding-left: 44px; }
  .key-row .btn, .key-row .input { height: 42px; }
  .key-row .input { min-width: 200px; }
  .indent { padding-left: 44px; }
  .found { display: grid; gap: 10px; }
  .found-head { display: flex; align-items: center; gap: 10px; }
  .found h2 { font: 600 12.5px/1.2 var(--font-ui); color: var(--ink-2); }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(340px, 1fr)); gap: 8px 12px; }
  .item { display: flex; align-items: center; gap: 12px; padding: 8px 12px 8px 10px; border: 1px solid var(--rule); border-radius: 8px; background: var(--surface); cursor: pointer; min-width: 0; }
  .item img, .noimg { width: 36px; height: 54px; object-fit: cover; border-radius: 3px; background: var(--sunk); flex: none; display: block; }
  .item-text { flex: 1; display: grid; gap: 1px; min-width: 0; }
  .jp { font: 800 15px/1.3 var(--font-jp-title); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .en { font-size: 12.5px; color: var(--ink-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .holds { font: 500 12.5px/1.3 var(--font-ui); }
  .more { justify-self: start; }
  footer { border-top: 1px solid var(--rule); }
  .foot { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 16px; max-width: 840px; box-sizing: border-box; margin: 0 auto; padding: 16px 32px; }
</style>
