<script lang="ts">
  import { goto } from "$app/navigation";
  import { onMount } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import {
    call,
    errorText,
    hachidoriInfo,
    hachidoriStart,
    inTauri,
    openHachidoriSettings,
    pickFile,
    relayStatus,
    setWindowTitle,
    type HachidoriInfo,
    type HostStatus,
    type Settings,
    type Status,
  } from "$lib/api";
  import { bytes } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  let settings = $state<Settings>({});
  let status = $state<Status | null>(null);
  let hd = $state<HachidoriInfo | null>(null);
  let relay = $state("Off");
  let key = $state("");
  let showKey = $state(false);
  let error = $state("");
  let saved = $state<Record<string, boolean>>({});

  async function load() {
    try {
      settings = await call<Settings>("settings");
      key = String(settings["jimaku.key"] ?? "");
      status = await call<Status>("status");
      hd = await hachidoriInfo();
      relay = await relayStatus();
    } catch (e) {
      error = errorText(e);
    }
  }

  onMount(() => {
    app.dark = false;
    setWindowTitle("Settings — Telly");
    load();
    let unlisten: (() => void) | null = null;
    if (inTauri) {
      import("@tauri-apps/api/event").then(({ listen }) =>
        listen<HostStatus>("hachidori-status", (e) => {
          if (hd) hd.status = e.payload;
        }).then((u) => (unlisten = u)),
      );
    }
    const timer = setInterval(async () => {
      status = await call<Status>("status").catch(() => status);
      relay = await relayStatus().catch(() => relay);
    }, 5000);
    return () => {
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
    } catch (e) {
      toast({ key: "set", state: "failed", title: "Not saved", text: errorText(e) });
    }
  }

  const str = (k: string, d = "") => String(settings[k] ?? d);
  const bool = (k: string, d: boolean) => (typeof settings[k] === "boolean" ? (settings[k] as boolean) : d);

  async function chooseFfmpeg() {
    const path = await pickFile("Choose ffmpeg.exe", ["exe"], "ffmpeg");
    if (!path) return;
    await set("tools.ffmpeg", path);
    status = await call<Status>("findTools");
  }

  async function lookAgain() {
    status = await call<Status>("findTools");
    toast({ key: "tools", state: status.tools.ffmpeg ? "done" : "failed", title: status.tools.ffmpeg ? "Found ffmpeg" : "No ffmpeg found", text: status.tools.ffmpeg ?? "Choose ffmpeg.exe yourself." });
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
      toast({ key: "fields", state: "failed", title: "Couldn't ask Anki", text: errorText(e) });
    }
  }

  async function freeAll() {
    const r = await call<{ episodes: number; bytes: number }>("freeSpace", { show: null });
    toast({ key: "free", state: "done", title: r.episodes ? `Freed ${bytes(r.bytes)}` : "Nothing to free", text: r.episodes ? `Removed Telly's copies of ${r.episodes} watched episodes.` : "Only copies of episodes you've watched are removed." });
    status = await call<Status>("status");
  }

  async function restartHachidori() {
    await hachidoriStart();
    hd = await hachidoriInfo();
  }

  const hdText = $derived.by(() => {
    if (!inTauri) return "Hachidori runs in the app only.";
    if (!hd?.prepared) return "Not set up. Run npm run prepare:hachidori, then restart Telly.";
    const s = hd.status;
    if (!s) return "Starting…";
    switch (s.state) {
      case "linked":
        return `Linked to your browser's Hachidori${s.host ? ` (${s.host})` : ""}: its dictionaries, settings and Anki setup.`;
      case "connecting":
        return `Connecting to your browser's Hachidori…${s.detail ? ` ${s.detail}` : ""}`;
      case "standalone":
        return "Running on its own, not linked.";
      case "link-failed":
        return `Couldn't link: ${s.detail}. Open Anki (with Hachidori Relay), or leave Telly's relay on, and open Hachidori in your browser.`;
      case "error":
        return s.detail;
      default:
        return "Starting…";
    }
  });

  const relayText = $derived(
    ({ Off: "Off", Running: "Relaying on 127.0.0.1:8771 while Anki is closed", Anki: "Anki is open, so Hachidori Relay does it", PortBusy: "Another program holds port 8771" } as Record<string, string>)[relay] ?? relay,
  );

  async function openJimaku() {
    if (inTauri) {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl("https://jimaku.cc/account");
    } else window.open("https://jimaku.cc/account");
  }
</script>

<div class="set scroll">
  <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Library</span></button>
  <h1>Settings</h1>
  {#if error}<p class="err">{error}</p>{/if}

  <section>
    <h2>Subtitles</h2>
    <div class="rows">
      <label class="field">
        <span>Jimaku API key</span>
        <span class="inline">
          <input class="input mono grow" type={showKey ? "text" : "password"} bind:value={key} placeholder="From jimaku.cc → Account → API" onchange={() => set("jimaku.key", key.trim())} />
          <button type="button" class="btn quiet small" onclick={() => (showKey = !showKey)}>{showKey ? "Hide" : "Show"}</button>
          <button type="button" class="btn quiet small" onclick={openJimaku}>Get a key</button>
        </span>
        <small>{saved["jimaku.key"] ? "Saved. Telly is looking for subtitles again." : "A free account gives you a key. Telly finds every episode's subtitles with it and syncs them to your rips."}</small>
      </label>
      <label class="field">
        <span>Subtitle size</span>
        <select class="input" value={str("player.subtitleSize", "36")} onchange={(e) => set("player.subtitleSize", Number(e.currentTarget.value))}>
          {#each [28, 32, 36, 40, 44, 50] as n}<option value={String(n)}>{n} px</option>{/each}
        </select>
      </label>
    </div>
  </section>

  <section>
    <h2>Cards</h2>
    <p class="lede">Hachidori makes the card as usual. Telly then adds a moving picture of the line and its audio, and fills only fields that are empty.</p>
    <div class="rows two">
      <label class="field"><span>AnkiConnect</span><input class="input mono" value={str("anki.url", "http://127.0.0.1:8765")} onchange={(e) => set("anki.url", e.currentTarget.value.trim())} /></label>
      <label class="field"><span>Note type</span>
        <span class="inline"><input class="input grow" value={str("anki.noteType", "kiku_typed")} onchange={(e) => set("anki.noteType", e.currentTarget.value.trim())} /><button type="button" class="btn quiet small" onclick={kikuFields}>Check its fields</button></span>
      </label>
      <label class="field"><span>Sentence field</span><input class="input" value={status?.fields.sentence ?? "Sentence"} onchange={(e) => set("anki.field.sentence", e.currentTarget.value.trim())} /></label>
      <label class="field"><span>Picture field</span><input class="input" value={status?.fields.picture ?? "Picture"} onchange={(e) => set("anki.field.picture", e.currentTarget.value.trim())} /></label>
      <label class="field"><span>Audio field</span><input class="input" value={status?.fields.audio ?? "SentenceAudio"} onchange={(e) => set("anki.field.audio", e.currentTarget.value.trim())} /></label>
      <label class="field"><span>Where it's from</span><input class="input" value={status?.fields.info ?? "MiscInfo"} onchange={(e) => set("anki.field.info", e.currentTarget.value.trim())} /></label>
    </div>
    <div class="field">
      <span>How much of the line</span>
      <div class="seg" role="group" aria-label="Clip length">
        {#each [["250", "250 ms either side"], ["500", "500 ms either side"], ["prev", "With the line before"]] as [v, label]}
          <button type="button" aria-pressed={str("clip.padding", "250") === v} onclick={() => set("clip.padding", v)}>{label}</button>
        {/each}
      </div>
    </div>
    <div class="status-list">
      <p><i class:ok={status?.anki} class:bad={status && !status.anki}></i>{status?.anki ? `Anki is open (AnkiConnect at ${status.ankiUrl})` : "Anki isn't open, or AnkiConnect isn't installed"}</p>
      <p><i class:ok={status?.mining?.beacon} class:warn={status?.mining && !status.mining.beacon}></i>
        {#if status?.mining?.beacon}Anki Beacon is telling Telly about new cards{:else if status?.mining?.listening}Listening for Anki Beacon at <code>{status.beaconAddress}</code>. Add that address to Beacon's config in Anki (Tools → Add-ons → Anki Beacon → Config). Until then Telly asks AnkiConnect every 2 seconds while you watch.{:else}{status?.mining?.listenError ?? "Not listening for Anki Beacon"}{/if}
      </p>
      <p><i class:ok={status?.tools.canClip} class:bad={status && !status.tools.canClip}></i>{status?.tools.canClip ? "ffmpeg can make animated AVIF and Opus" : "This ffmpeg can't make AVIF and Opus (it needs libsvtav1 and libopus)"}</p>
    </div>
  </section>

  <section>
    <h2>Hachidori</h2>
    <p class="lede">{hdText}</p>
    <div class="rows">
      <label class="check"><input type="checkbox" checked={bool("hachidori.link", true)} onchange={async (e) => { await set("hachidori.link", e.currentTarget.checked); restartHachidori(); }} /><span>Link to the Hachidori in your browser<small>Your dictionaries, settings and card format come from it, through Hachidori Relay.</small></span></label>
      <label class="check"><input type="checkbox" checked={bool("hachidori.relay", true)} onchange={(e) => set("hachidori.relay", e.currentTarget.checked)} /><span>Run Telly's own relay while Anki is closed<small>{relayText}</small></span></label>
    </div>
    <div class="actions">
      <button type="button" class="btn quiet" onclick={() => openHachidoriSettings().catch((e) => toast({ key: "hd", state: "failed", title: "Hachidori", text: errorText(e) }))} disabled={!hd?.prepared}>Hachidori settings</button>
      <button type="button" class="btn quiet" onclick={restartHachidori} disabled={!hd?.prepared}>Restart Hachidori</button>
      {#if hd?.version}<span class="muted">Hachidori {hd.version}</span>{/if}
    </div>
  </section>

  <section>
    <h2>Playing</h2>
    <div class="rows">
      <label class="check"><input type="checkbox" checked={bool("player.autoplay", true)} onchange={(e) => set("player.autoplay", e.currentTarget.checked)} /><span>Play the next episode when one ends<small>After an 8-second countdown. Esc stops it.</small></span></label>
      <label class="check"><input type="checkbox" checked={bool("player.skipEndings", false)} onchange={(e) => set("player.skipEndings", e.currentTarget.checked)} /><span>Skip endings<small>The countdown starts when the ending does, using the disc's chapters.</small></span></label>
      <label class="field narrow"><span>Convert ahead</span>
        <select class="input" value={String(settings["convert.ahead"] ?? 10)} onchange={(e) => set("convert.ahead", Number(e.currentTarget.value))}>
          {#each [3, 5, 10, 20, 50] as n}<option value={String(n)}>{n} episodes</option>{/each}
        </select>
        <small>Episodes WebView2 can't play are converted this far ahead of where you are.</small>
      </label>
      <label class="check"><input type="checkbox" checked={bool("convert.rest", false)} onchange={(e) => set("convert.rest", e.currentTarget.checked)} /><span>Convert the rest while nothing plays<small>Uses more disk space, so nothing waits later.</small></span></label>
    </div>
  </section>

  <section>
    <h2>ffmpeg</h2>
    {#if status?.tools.ffmpeg}
      <p class="lede">Using <code>{status.tools.ffmpeg}</code><br /><span class="muted">{status.tools.version} · converts with {status.tools.encoder}</span></p>
    {:else}
      <p class="lede">Telly can't find ffmpeg. It looks on PATH and where GameSentenceMiner keeps it. Choose ffmpeg.exe (its folder needs ffprobe.exe too).</p>
    {/if}
    <div class="actions">
      <button type="button" class="btn quiet" onclick={chooseFfmpeg}>Choose ffmpeg.exe…</button>
      <button type="button" class="btn quiet" onclick={lookAgain}>Look again</button>
      {#if str("tools.ffmpeg")}<button type="button" class="link-btn" onclick={async () => { await set("tools.ffmpeg", null); lookAgain(); }}>Forget my choice</button>{/if}
    </div>
  </section>

  <section>
    <h2>Space</h2>
    <p class="lede">Telly's copies of your episodes use {bytes(status?.spaceUsed ?? 0)}. Your rips stay where they are.</p>
    <div class="actions">
      <button type="button" class="btn quiet" onclick={freeAll}>Free up space</button>
      <span class="muted">Removes copies of episodes you've watched.</span>
    </div>
    <p class="muted mono path">{status?.dataDir}</p>
  </section>

  <section>
    <h2>Tidepool</h2>
    <p class="lede">What you watch is logged for Tidepool's Japanese log, as Watching: each sitting, and the lines and characters that played. Never the text itself.</p>
    <p class="muted mono path">{status?.logPath}</p>
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
  code { background: var(--sunk); padding: .1em .36em; border-radius: 4px; overflow-wrap: anywhere; }
</style>
