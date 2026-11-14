<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
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
    type AskProgress,
    type ClaudeStatus,
    type HachidoriInfo,
    type HostStatus,
    type Library,
    type OwnHachidori,
    type ScanResult,
    type Settings,
    type Status,
  } from "$lib/api";
  import { STEPS, askAnki, askJimaku, checks, deck, noteType, stepState, type AnkiFacts, type Step } from "$lib/checks";
  import { plural } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  let settings = $state<Settings>(kept<Settings>("settings") ?? {});
  let status = $state<Status | null>(kept<Status>("status"));
  let claude = $state<ClaudeStatus | null>(kept<ClaudeStatus>("claudeStatus"));
  let own = $state<OwnHachidori | null>(kept<OwnHachidori>("ownHachidori"));
  let hd = $state<HachidoriInfo | null>(null);
  let jimaku = $state<string | null>(null);
  let anki = $state<AnkiFacts | null>(null);
  /** How many shows the library has. */
  let shows = $state(kept<Library>("library")?.shows.length ?? 0);
  let key = $state(String(kept<Settings>("settings")?.["jimaku.key"] ?? ""));
  let showKey = $state(false);
  let showRouterKey = $state(false);
  let error = $state("");

  /** The screen showing: the one Settings asked for (/setup?step=anki), else the first. */
  let at = $state(Math.max(0, STEPS.findIndex((s) => s.id === page.url.searchParams.get("step"))));
  const step = $derived(STEPS[at]);
  const str = (k: string, d = "") => String(settings[k] ?? d);

  /** Every part is asked for at once, and shows as it comes. */
  async function load() {
    try {
      await Promise.all([
        callKept<Settings>("settings").then((s) => {
          settings = s;
          key = String(s["jimaku.key"] ?? "");
          // Begun: a show added on the first screen doesn't make it done (see Backend::open).
          if (s["setup.done"] !== true) call("setSetting", { key: "setup.done", value: false }).catch(() => {});
          askJimaku(s).then((j) => (jimaku = j));
          askAnki(s).then((a) => (anki = a));
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
    // Anki opened, Beacon heard from, a dictionary installed: each shows within 5 seconds.
    const timer = setInterval(async () => {
      status = await call<Status>("status").catch(() => status);
      own = await call<OwnHachidori>("ownHachidori").catch(() => own);
      if (status?.anki && !anki) anki = await askAnki(settings);
    }, 5000);
    return () => {
      left = true;
      clearInterval(timer);
      clearTimeout(testTimer);
      unlisten?.();
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
    if (k === "openrouter.key") claude = await callKept<ClaudeStatus>("claudeStatus").catch(() => claude);
  }

  /** Asks again about everything on this screen. */
  async function checkAgain() {
    jimaku = null;
    anki = null;
    claude = null;
    await load();
  }

  /** Done, or skipped: the Library from now on. */
  async function finish() {
    await call("setSetting", { key: "setup.done", value: true }).catch(() => {});
    goto("/");
  }

  // ---- Your rips ----

  /** A folder chosen on the first screen: its scan, started at once, and the shows added from it. */
  interface Folder {
    path: string;
    scan: Promise<ScanResult>;
    result: ScanResult | null;
    error: string;
    added: string[];
  }
  let folders = $state<Folder[]>([]);
  const waiting = $derived(folders.filter((f) => f.result && !f.added.length));
  /** Every check, and on the first screen the folders read and still to be added. */
  const all = $derived([
    ...checks({ settings, status, jimaku, anki, own, hd, claude }),
    ...(waiting.length ? [{ id: "waiting", step: "rips", state: "warn", text: `${plural(waiting.length, "folder")} read and waiting for you to check and add.` } as const] : []),
  ]);
  /** The folder whose scan is being checked and added (AddSheet). */
  let adding = $state<Folder | null>(null);

  /** What Telly took a folder to be: its show on AniList, else the name it guessed. */
  function nameOf(f: Folder): string {
    const m = f.result?.matches[0];
    return (m ? m.english || m.romaji : f.result?.scan.guess) || f.path.split(/[\\/]/).filter(Boolean).pop() || f.path;
  }

  const episodesOf = (r: ScanResult) => r.scan.files.filter((f) => f.number !== null).length;

  /** Whether this screen's part works, so its button goes on rather than skips. */
  const works = $derived(!["bad", "warn"].includes(stepState(all, step.id)) && (step.id !== "rips" || shows > 0 || folders.length > 0));

  /** Starts reading each folder chosen; each is checked and added once it's read, while the rest of the setup goes on. */
  async function chooseFolders() {
    for (const path of await pickFolders("Choose the folders of your rips")) {
      if (folders.some((f) => f.path === path)) continue;
      folders.push({ path, scan: call<ScanResult>("scan", { folder: path }), result: null, error: "", added: [] });
      const f = folders[folders.length - 1];
      f.scan.then(
        (r) => {
          f.result = r;
          // Read while another screen shows: it says so there.
          if (step.id !== "rips" && !adding)
            toast({ key: `read ${path}`, state: "done", title: `Telly has read ${nameOf(f)}`, text: "Check what it found, and add it to your library.", action: { label: "Check and add", run: () => (adding = f) } }, 15000);
        },
        (e) => (f.error = errorText(e)),
      );
    }
  }

  async function added(f: Folder, id: number) {
    adding = null;
    try {
      const l = await callKept<Library>("library");
      shows = l.shows.length;
      f.added.push(l.shows.find((s) => s.id === id)?.title ?? nameOf(f));
    } catch {
      f.added.push(nameOf(f));
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

  // ---- Subtitles and AniList ----

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

  // ---- Anki ----

  async function copyBeacon() {
    try {
      await navigator.clipboard.writeText(status?.beaconAddress ?? "");
      toast({ key: "beacon", state: "done", title: "Copied Anki Beacon’s address", text: "Paste it into Beacon’s config in Anki." });
    } catch {
      toast({ key: "beacon", state: "failed", title: "Couldn’t copy it", text: "Select the address and press Ctrl+C." });
    }
  }

  // ---- Lookups ----

  async function chooseDictionaries() {
    const folder = await pickFolder("Choose the dictionaries folder");
    if (!folder) return;
    await set("hachidori.dictionaries", folder);
    own = await call<OwnHachidori>("ownHachidori").catch(() => own);
  }

  // ---- Explain and Previously on ----

  /** The test question's answer so far. */
  let test = $state<{ text: string; done: boolean; error: string } | null>(null);
  let testTimer: ReturnType<typeof setTimeout> | undefined;

  async function askTest() {
    clearTimeout(testTimer);
    test = { text: "", done: false, error: "" };
    try {
      const { ask } = await call<{ ask: number }>("testClaude");
      const follow = async () => {
        try {
          const r = await call<AskProgress>("ask", { ask });
          test = { text: r.text ?? "", done: !!r.done, error: r.error ?? "" };
        } catch (e) {
          test = { text: "", done: true, error: errorText(e) };
        }
        if (!test.done) testTimer = setTimeout(follow, 400);
      };
      follow();
    } catch (e) {
      test = { text: "", done: true, error: errorText(e) };
    }
  }
</script>

{#snippet found(id: Step)}
  <div class="status-list">
    {#each all.filter((c) => c.step === id) as c (c.id)}
      <p><i class={c.state}></i><span>{c.text}</span></p>
    {/each}
  </div>
{/snippet}

<div class="setup scroll">
  <div class="inner">
    <div class="top">
      <ol class="steps" aria-label="Setup">
        {#each STEPS as s, i (s.id)}
          <li>
            <button type="button" class="step" aria-current={i === at ? "step" : undefined} onclick={() => (at = i)}>
              <i class={stepState(all, s.id)}></i><span>{s.name}</span>
            </button>
          </li>
        {/each}
      </ol>
      <button type="button" class="link-btn" onclick={finish}>Skip setup</button>
    </div>

    <section>
      <span class="kicker">Setting up Telly · {at + 1} of {STEPS.length}</span>
      <h1>{step.name}</h1>
      {#if error}<p class="err">{error}</p>{/if}

      {#if step.id === "rips"}
        <p class="lede">Choose the folders your anime is in. Telly reads them while you go on with the rest of the setup, then shows what it found for you to check. Your rips stay where they are, unchanged.</p>
        <p class="muted">Used Telly on another computer? <button type="button" class="link-btn" onclick={() => goto("/settings#backup")}>Restore its backup…</button></p>
        {#if status && !status.tools.ffmpeg}
          <div class="actions">
            <button type="button" class="btn quiet" onclick={chooseFfmpeg}>Choose ffmpeg.exe…</button>
            <button type="button" class="btn quiet" onclick={lookAgain}>Look again</button>
          </div>
        {/if}
        {@render found("rips")}
        <div class="actions">
          <button type="button" class="btn" onclick={chooseFolders}><Icon name="folder" /><span>Choose folders…</span></button>
          {#if shows}<span class="muted">Your library has {plural(shows, "show")}.</span>{/if}
        </div>
        {#if folders.length}
          <div class="folders">
            {#each folders as f (f.path)}
              <div class="folder">
                <span class="f-body">
                  {#if f.error}
                    <span class="f-name">Couldn’t read this folder</span>
                    <span class="err">{f.error}</span>
                  {:else if !f.result}
                    <span class="f-name"><span class="spin"></span> Reading…</span>
                  {:else}
                    <span class="f-name">{nameOf(f)}</span>
                    <span class="muted">{plural(episodesOf(f.result), "episode")}{f.result.scan.groups.length >= 2 ? ` · ${f.result.scan.groups.length} seasons` : ""}{f.added.length ? ` · Added ${f.added.join(", ")}` : ""}</span>
                  {/if}
                  <span class="mono path">{f.path}</span>
                </span>
                {#if f.result && (!f.added.length || f.result.scan.groups.length >= 2)}
                  <button type="button" class="btn small" onclick={() => (adding = f)}>{f.added.length ? "Add another season" : "Check and add"}</button>
                {:else if f.added.length}
                  <span class="done"><Icon name="check" /><span>Added</span></span>
                {/if}
              </div>
            {/each}
          </div>
          <p class="muted">Each show added is got ready in the background, next episode first; the title bar shows how far it’s got.</p>
        {/if}
      {:else if step.id === "subtitles"}
        <p class="lede">Telly finds Japanese subtitles for each episode on Jimaku and syncs them to your rips. A free Jimaku account comes with an API key.</p>
        <label class="field">
          <span>Jimaku API key</span>
          <span class="inline">
            <input class="input mono grow" type={showKey ? "text" : "password"} bind:value={key} onchange={() => set("jimaku.key", key.trim())} />
            <button type="button" class="btn quiet" onclick={() => (showKey = !showKey)}>{showKey ? "Hide" : "Show"}</button>
            <button type="button" class="btn quiet" onclick={() => openLink("https://jimaku.cc/account")}>Get a key</button>
          </span>
          <small>It’s on your account page on jimaku.cc, which Get a key opens.</small>
        </label>
        {@render found("subtitles")}
        <label class="field">
          <span>OpenRouter API key (optional)</span>
          <span class="inline">
            <input class="input mono grow" type={showRouterKey ? "text" : "password"} value={str("openrouter.key")} onchange={(e) => set("openrouter.key", e.currentTarget.value.trim())} />
            <button type="button" class="btn quiet" onclick={() => (showRouterKey = !showRouterKey)}>{showRouterKey ? "Hide" : "Show"}</button>
            <button type="button" class="btn quiet" onclick={() => openLink("https://openrouter.ai/settings/keys")}>Get a key</button>
          </span>
          <small>This can wait. With a key, a model on OpenRouter makes subtitles for the episodes Jimaku has nothing for, for a few cents an episode, and each episode’s audio is sent to it. Without one, Whisper on this computer does. Without Claude Code on this computer, gpt-oss on OpenRouter also answers Explain and Previously on.</small>
        </label>
      {:else if step.id === "anki"}
        <p class="lede">Cards you mine with Hachidori go to Anki, through the AnkiConnect add-on, and Telly adds a moving picture of the line and its audio. Telly fills only fields that are empty.</p>
        {@render found("anki")}
        <div class="rows two">
          <label class="field"><span>Deck</span><input class="input" list="anki-decks" value={deck(settings)} onchange={(e) => set("anki.deck", e.currentTarget.value.trim())} title={"{show} is the title of the show being watched"} /></label>
          <label class="field"><span>Note type</span><input class="input" list="anki-types" value={noteType(settings)} onchange={(e) => set("anki.noteType", e.currentTarget.value.trim())} /></label>
          <label class="field"><span>Sentence field</span><input class="input" value={status?.fields.sentence ?? "Sentence"} onchange={(e) => set("anki.field.sentence", e.currentTarget.value.trim())} /></label>
          <label class="field"><span>Picture field</span><input class="input" value={status?.fields.picture ?? "Picture"} onchange={(e) => set("anki.field.picture", e.currentTarget.value.trim())} /></label>
          <label class="field"><span>Audio field</span><input class="input" value={status?.fields.audio ?? "SentenceAudio"} onchange={(e) => set("anki.field.audio", e.currentTarget.value.trim())} /></label>
          <label class="field"><span>Source field</span><input class="input" value={status?.fields.info ?? "MiscInfo"} onchange={(e) => set("anki.field.info", e.currentTarget.value.trim())} /></label>
        </div>
        <datalist id="anki-decks">{#each anki?.decks ?? [] as d (d)}<option value={d}></option>{/each}</datalist>
        <datalist id="anki-types">{#each anki?.noteTypes ?? [] as t (t)}<option value={t}></option>{/each}</datalist>
        <div class="field">
          <span>Anki Beacon’s address</span>
          <span class="inline">
            <input class="input mono grow" value={status?.beaconAddress ?? ""} readonly aria-label="Anki Beacon’s address" onfocus={(e) => e.currentTarget.select()} />
            <button type="button" class="btn quiet" onclick={copyBeacon}>Copy</button>
          </span>
          <small>Anki Beacon, an Anki add-on, tells Telly the moment a card is added. Add this address to its config (Tools → Add-ons → Anki Beacon → Config).</small>
        </div>
      {:else if step.id === "lookups"}
        <p class="lede">Hachidori looks up the words in the subtitles as you hover over them, and makes cards in Anki. Telly’s own Hachidori installs every Yomitan dictionary (.zip file) in your dictionaries folder, word dictionaries first.</p>
        {@render found("lookups")}
        <div class="field">
          <span>Dictionaries</span>
          <span class="inline"><input class="input grow mono" value={own?.folder ?? ""} placeholder="C:\Users\you\Documents\dictionaries" onchange={async (e) => { await set("hachidori.dictionaries", e.currentTarget.value.trim()); own = await call<OwnHachidori>("ownHachidori").catch(() => own); }} /><button type="button" class="btn quiet" onclick={chooseDictionaries}>Choose folder…</button></span>
          {#if own?.folder}<small>Telly found {plural(own.total, "dictionary", "dictionaries")} in this folder.</small>{/if}
        </div>
      {:else if step.id === "claude"}
        <p class="lede"><b>Explain</b> (E) explains the line on screen, and <b>Previously on</b> (P) recaps the story so far when you come back to a show. Both use the Claude Code on this computer, with its own sign-in, so they need no API key. Without Claude Code, they use gpt-oss on OpenRouter, with the OpenRouter key from Subtitles.</p>
        {@render found("claude")}
        <div class="actions">
          <button type="button" class="btn quiet" onclick={askTest} disabled={!claude?.available || (test !== null && !test.done)}>Ask a test question</button>
        </div>
        {#if claude?.openrouter}
          <small class="muted">It asks gpt-oss on OpenRouter what はじめまして means, to see that your OpenRouter key works. It costs a fraction of a cent.</small>
        {:else}
          <small class="muted">It asks what はじめまして means, with the model chosen in Settings → Claude, to see that Claude Code is signed in and answers. It uses your Claude Code, so it costs a little.</small>
        {/if}
        {#if test}
          <div class="status-list">
            {#if !test.done}
              <p><i class="wait"></i><span>{claude?.openrouter ? "Asking gpt-oss on OpenRouter…" : "Asking Claude Code… It takes a few seconds to start."}</span></p>
            {:else if test.error}
              <p><i class="bad"></i><span>{test.error}</span></p>
            {:else}
              <p><i class="ok"></i><span>{claude?.openrouter ? "gpt-oss on OpenRouter answered:" : "Claude Code is signed in, and answered:"}</span></p>
            {/if}
          </div>
          {#if test.text && !test.error}<p class="answer">{test.text}</p>{/if}
        {/if}
      {:else}
        <p class="lede">Optional. Signed in, each episode you watch in Telly goes on your AniList list. Recommended works without it.</p>
        {@render found("anilist")}
        {#if !str("anilist.user")}
          <label class="field">
            <span>Access token</span>
            <span class="inline">
              <input class="input mono grow" type="password" bind:value={anilistToken} />
              <button type="button" class="btn quiet" onclick={() => openLink(`https://anilist.co/api/v2/oauth/authorize?client_id=${ANILIST_CLIENT_ID}&response_type=token`)}>Get a token</button>
              <button type="button" class="btn" onclick={anilistSignIn} disabled={!anilistToken.trim()}>Sign in</button>
            </span>
            <small>Get a token opens AniList, which shows an access token once you approve Telly. Paste it here.</small>
          </label>
        {/if}
      {/if}
      {#if at === STEPS.length - 1 && waiting.length}
        <div class="status-list">
          <p><i class="warn"></i><span>{plural(waiting.length, "folder")} read and waiting for you to check and add. <button type="button" class="link-btn" onclick={() => (at = 0)}>Back to your rips</button></span></p>
        </div>
      {/if}
    </section>

    <div class="foot">
      {#if at > 0}<button type="button" class="btn quiet" onclick={() => (at -= 1)}><Icon name="back" /><span>Back</span></button>{/if}
      <button type="button" class="btn quiet" onclick={checkAgain}>Check again</button>
      <span class="grow"></span>
      {#if at < STEPS.length - 1}
        <button type="button" class="btn" onclick={() => (at += 1)}>{works ? `Next: ${STEPS[at + 1].name}` : `Skip to ${STEPS[at + 1].name}`}</button>
      {:else}
        <button type="button" class="btn" onclick={finish}>Go to the library</button>
      {/if}
    </div>
  </div>
</div>

{#if adding}
  {@const f = adding}
  <AddSheet folder={f.path} scanned={f.scan} onclose={() => (adding = null)} onadded={(id) => added(f, id)} />
{/if}

<style>
  .setup { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 60px; }
  .inner { max-width: 820px; margin: 0 auto; display: grid; gap: 8px; }
  .top { display: flex; align-items: center; gap: 16px; flex-wrap: wrap; padding-bottom: 14px; border-bottom: 1px solid var(--rule); }
  .steps { list-style: none; padding: 0; display: flex; flex-wrap: wrap; gap: 4px; flex: 1; }
  .step { display: inline-flex; align-items: center; gap: 7px; padding: 6px 10px; border: 0; border-radius: 6px; background: none; font: 500 13px/1.2 var(--font-ui); color: var(--ink-2); cursor: pointer; }
  .step:hover { background: var(--sunk); color: var(--ink); }
  .step[aria-current="step"] { background: var(--surface); color: var(--ink); font-weight: 600; box-shadow: inset 0 0 0 1px var(--rule-2); }
  section { display: grid; gap: 16px; padding: 22px 0; }
  .kicker { font-size: 12.5px; color: var(--ink-3); }
  h1 { font: 600 28px/1.2 var(--font-display); margin: -10px 0 0; }
  .lede { color: var(--ink-2); font-size: 14px; max-width: 70ch; }
  .rows { display: grid; gap: 14px; }
  .rows.two { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .inline { display: flex; gap: 8px; align-items: center; }
  .field small { color: var(--ink-3); font-size: 12.5px; }
  .actions { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .muted { color: var(--ink-3); font-size: 12.5px; }
  .path { overflow-wrap: anywhere; color: var(--ink-3); font-size: 12px; }
  .status-list { display: grid; gap: 6px; font-size: 13.5px; color: var(--ink-2); }
  .status-list p { display: flex; gap: 9px; align-items: baseline; }
  .answer { padding: 10px 14px; border-left: 3px solid var(--rule-2); color: var(--ink); font-size: 14px; white-space: pre-wrap; max-width: 70ch; }
  i { width: 8px; height: 8px; border-radius: 50%; background: var(--rule-2); flex: none; transform: translateY(-1px); display: inline-block; }
  i.ok { background: var(--ok-dot); }
  i.warn { background: var(--warn-dot); }
  i.bad { background: var(--bad-dot); }
  .folders { display: grid; border-top: 1px solid var(--rule); }
  .folder { display: flex; align-items: center; gap: 14px; padding: 10px 2px; border-bottom: 1px solid var(--rule); }
  .f-body { flex: 1; min-width: 0; display: grid; gap: 2px; }
  .f-name { display: inline-flex; align-items: center; gap: 8px; font: 600 14px/1.3 var(--font-ui); }
  .done { display: inline-flex; align-items: center; gap: 6px; font: 600 13px/1 var(--font-ui); color: var(--ok); }
  .done :global(svg) { width: 16px; height: 16px; }
  .foot { display: flex; align-items: center; gap: 10px; padding-top: 18px; border-top: 1px solid var(--rule); }
</style>
