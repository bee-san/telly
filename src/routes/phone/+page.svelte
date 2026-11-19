<script lang="ts">
  import { goto } from "$app/navigation";
  import { onMount } from "svelte";
  import { call, errorText, mediaUrl } from "$lib/api";
  import { bytes, clock } from "$lib/format";
  import { gettingText, phonePlays, type PhoneEpisode, type PhoneLibrary, type PhoneShow } from "$lib/phone";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  let lib = $state<PhoneLibrary | null>(null);
  let error = $state("");
  let open = $state<number | null>(null);
  let syncing = $state(false);

  async function load() {
    try {
      lib = await call<PhoneLibrary>("phoneLibrary");
      error = "";
    } catch (e) {
      error = errorText(e);
    }
  }

  /** Gives the PC where you got to, and takes in what changed there. Quiet
   *  when it's not asked for and the PC isn't there (you're out). */
  async function sync(asked: boolean) {
    if (!lib?.state.pc || syncing) return;
    syncing = true;
    try {
      await call("phoneSync");
      await load();
      // Dictionaries the PC has that the phone's lookups haven't installed yet.
      import("$lib/lookup.svelte").then((l) => l.installDictionaries()).catch(() => {});
      if (asked) toast({ key: "sync", state: "done", title: "Up to date with your PC", text: "Where you got to is on the PC, and the PC’s on the phone." });
    } catch (e) {
      if (asked) toast({ key: "sync", state: "failed", title: "Your PC isn’t reachable", text: errorText(e) });
    } finally {
      syncing = false;
    }
  }

  onMount(() => {
    app.dark = false;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const loop = async () => {
      await load();
      if (stopped) return;
      timer = setTimeout(loop, lib?.state.getting.length ? 1500 : 6000);
    };
    loop().then(() => sync(false));
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  });

  const watching = $derived(
    (lib?.shows ?? []).filter((s) => s.show.lastWatchedAt && s.next !== null).slice(0, 6),
  );

  const onPhoneCount = (s: PhoneShow) => s.episodes.filter((e) => e.onPhone).length;
  const nextOf = (s: PhoneShow) => s.episodes.find((e) => e.id === s.next) ?? null;

  function epLabel(s: PhoneShow, e: PhoneEpisode) {
    return s.episodes.length === 1 && s.show.format === "MOVIE" ? "The film" : `Episode ${e.number}`;
  }

  async function act(f: () => Promise<unknown>) {
    try {
      await f();
      await load();
    } catch (e) {
      toast({ key: "phone", state: "failed", title: "That didn’t work", text: errorText(e) });
    }
  }

  const get = (ids: number[]) => act(async () => call("phoneGet", { ids, plays: await phonePlays() }));
  const remove = (ids: number[]) => act(() => call("phoneRemove", { ids }));
  const play = (e: PhoneEpisode) => goto(`/watch?ep=${e.id}`);
</script>

<div class="page">
  <header>
    <div class="brand"><span class="logo">T</span><h1>Telly</h1></div>
    <button type="button" class="btn" onclick={() => goto("/phone/get")} title="Choose episodes to copy from your PC">Get from PC</button>
  </header>

  {#if error}<p class="error">{error}</p>{/if}

  {#if lib && !lib.state.pc}
    <section class="empty">
      <h2>Pair with Telly on your PC</h2>
      <p>Your PC gets shows ready. Pair the phone with it once, then copy the episodes you want to watch here, with or without a signal.</p>
      <button type="button" class="btn primary" onclick={() => goto("/phone/get")}>Pair with your PC</button>
    </section>
  {:else if lib}
    <div class="status">
      <span>{lib.state.pc?.name} · {bytes(lib.state.free)} free</span>
      <button type="button" class="link" disabled={syncing} onclick={() => sync(true)}>{syncing ? "Syncing…" : "Sync now"}</button>
    </div>

    {#if watching.length}
      <h2 class="section">Currently watching</h2>
      <div class="row">
        {#each watching as s (s.show.id)}
          {@const n = nextOf(s)}
          {#if n}
            <article class="next">
              <img src={mediaUrl(`/cover/${s.show.id}`)} alt="" />
              <div class="next-text">
                <strong>{s.show.title}</strong>
                <span>{epLabel(s, n)}{n.position > 30 ? ` · from ${clock(n.position)}` : ""}</span>
                {#if n.onPhone}
                  <button type="button" class="btn primary" onclick={() => play(n)}>{n.position > 30 ? "Resume" : "Play"}</button>
                {:else if n.getting}
                  <span class="muted">{gettingText(n.getting)}</span>
                {:else}
                  <button type="button" class="btn" onclick={() => get([n.id])}>Get it from your PC</button>
                {/if}
              </div>
            </article>
          {/if}
        {/each}
      </div>
    {/if}

    <h2 class="section">On your phone</h2>
    {#if !lib.shows.length}
      <p class="muted pad">Nothing yet. Get from PC copies the episodes you pick.</p>
    {/if}
    <ul class="shows">
      {#each lib.shows as s (s.show.id)}
        <li>
          <button type="button" class="show" onclick={() => (open = open === s.show.id ? null : s.show.id)} aria-expanded={open === s.show.id}>
            <img src={mediaUrl(`/cover/${s.show.id}`)} alt="" />
            <span class="show-text">
              <strong>{s.show.title}</strong>
              {#if s.show.titleNative}<span class="jp">{s.show.titleNative}</span>{/if}
              <span class="muted">{onPhoneCount(s)} of {s.episodes.length} on the phone · {s.episodes.filter((e) => e.watched).length} watched</span>
            </span>
          </button>
          {#if open === s.show.id}
            <ul class="episodes">
              {#each s.episodes as e (e.id)}
                <li class:watched={e.watched}>
                  <span class="ep">
                    <strong>{epLabel(s, e)}</strong>
                    <span class="muted">
                      {#if e.getting}{gettingText(e.getting)}{:else if e.watched}Watched{:else if e.position > 30}{clock(e.position)} of {clock(e.duration)}{:else}{clock(e.duration)}{/if}
                      {#if !e.onPhone && !e.getting} · on your PC{/if}
                    </span>
                  </span>
                  {#if e.onPhone}
                    <button type="button" class="btn small" onclick={() => play(e)}>Play</button>
                    <button type="button" class="link" onclick={() => remove([e.id])} title="Delete the phone’s copy; it stays on your PC">Remove</button>
                  {:else if e.getting?.state === "failed"}
                    <button type="button" class="btn small" onclick={() => get([e.id])}>Try again</button>
                  {:else if !e.getting}
                    <button type="button" class="btn small" onclick={() => get([e.id])}>Get</button>
                  {/if}
                </li>
              {/each}
            </ul>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .page { height: 100%; overflow-y: auto; padding: 12px 16px 32px; }
  header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 4px 0 12px; }
  .brand { display: flex; align-items: center; gap: 10px; }
  .logo { width: 30px; height: 30px; border-radius: 8px; display: grid; place-items: center; background: var(--logo-bg); color: var(--logo-fg); font-weight: 700; }
  h1 { font: 700 22px/1 var(--font-display); margin: 0; }
  .section { font: 600 13px/1.2 var(--font-ui); text-transform: uppercase; letter-spacing: .06em; color: var(--ink-3); margin: 22px 0 10px; }
  .status { display: flex; justify-content: space-between; align-items: center; color: var(--ink-2); font-size: 14px; }
  .btn { border: 1px solid var(--rule-2); background: var(--surface); color: var(--ink); border-radius: 10px; padding: 10px 14px; font: 600 15px var(--font-ui); }
  .btn.primary { background: var(--btn); color: var(--btn-ink); border-color: var(--btn); }
  .btn.small { padding: 7px 12px; font-size: 14px; }
  .link { border: 0; background: none; color: var(--ink-2); text-decoration: underline; font: 500 14px var(--font-ui); padding: 6px; }
  .muted { color: var(--ink-3); font-size: 13px; }
  .pad { padding: 4px 0; }
  .error { color: var(--bad); }
  .empty { margin-top: 18vh; text-align: center; padding: 0 8px; }
  .empty h2 { font: 700 24px/1.2 var(--font-display); }
  .empty p { color: var(--ink-2); line-height: 1.5; }
  .row { display: flex; gap: 12px; overflow-x: auto; scroll-snap-type: x mandatory; padding-bottom: 4px; }
  .next { flex: 0 0 86%; scroll-snap-align: start; display: flex; gap: 12px; background: var(--surface); border: 1px solid var(--rule); border-radius: 14px; padding: 12px; }
  .next img { width: 84px; height: 120px; object-fit: cover; border-radius: 8px; background: var(--sunk); }
  .next-text { display: flex; flex-direction: column; gap: 6px; min-width: 0; justify-content: center; align-items: flex-start; }
  .next-text strong { font-size: 16px; }
  .shows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 10px; }
  .show { width: 100%; display: flex; gap: 12px; align-items: center; text-align: left; background: var(--surface); border: 1px solid var(--rule); border-radius: 12px; padding: 10px; color: var(--ink); }
  .show img { width: 52px; height: 74px; object-fit: cover; border-radius: 6px; background: var(--sunk); flex: none; }
  .show-text { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .jp { font-family: var(--font-jp); color: var(--ink-2); font-size: 13px; }
  .episodes { list-style: none; margin: 6px 0 0; padding: 0 4px; }
  .episodes li { display: flex; align-items: center; gap: 8px; padding: 10px 6px; border-bottom: 1px solid var(--rule); }
  .episodes li.watched strong { color: var(--ink-3); }
  .ep { flex: 1; display: flex; flex-direction: column; gap: 2px; min-width: 0; }
</style>
