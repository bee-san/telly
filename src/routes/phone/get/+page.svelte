<script lang="ts">
  import { goto } from "$app/navigation";
  import { onMount } from "svelte";
  import Cover from "$lib/Cover.svelte";
  import { call, errorText, type Show } from "$lib/api";
  import { bytes } from "$lib/format";
  import { phonePlays, type PcEpisode, type PcLibrary, type PhoneState } from "$lib/phone";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  let phone = $state<PhoneState | null>(null);
  let pcs = $state<{ address: string; name: string }[]>([]);
  let looking = $state(false);
  let address = $state("");
  let code = $state("");
  let pairing = $state(false);
  let pairError = $state("");

  let lib = $state<PcLibrary | null>(null);
  let libError = $state("");
  let open = $state<number | null>(null);
  let picked = $state<Set<number>>(new Set());
  let getting = $state(false);

  async function look() {
    looking = true;
    try {
      pcs = await call("phoneFind");
      if (pcs.length && !address) address = pcs[0].address;
    } catch {
      pcs = [];
    } finally {
      looking = false;
    }
  }

  async function loadPc() {
    try {
      lib = await call<PcLibrary>("phonePcLibrary");
      libError = "";
    } catch (e) {
      libError = errorText(e);
    }
  }

  onMount(async () => {
    app.dark = false;
    phone = await call<PhoneState>("phoneState");
    if (phone.pc) loadPc();
    else look();
  });

  async function pair() {
    pairing = true;
    pairError = "";
    try {
      phone = await call<PhoneState>("phonePair", { address, code });
      await loadPc();
    } catch (e) {
      pairError = errorText(e);
    } finally {
      pairing = false;
    }
  }

  async function unpair() {
    phone = await call<PhoneState>("phoneUnpair");
    lib = null;
    look();
  }

  /** Episodes the phone could get: not on it, not being got. */
  const gettable = (e: PcEpisode) => e.onPhone === "no";

  function toggle(id: number) {
    const next = new Set(picked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    picked = next;
  }

  /** Ticks a show's episodes from the first not watched, up to `n` of them. */
  function pickNext(episodes: PcEpisode[], n: number) {
    const next = new Set(picked);
    episodes.filter((e) => gettable(e) && !e.episode.watchedAt).slice(0, n).forEach((e) => next.add(e.episode.id));
    picked = next;
  }

  const all = $derived((lib?.shows ?? []).flatMap((s) => s.episodes));
  /** About how much the picked episodes take: their rips' sizes, which the phone's copies are seldom bigger than. */
  const pickedSize = $derived(all.filter((e) => picked.has(e.episode.id)).reduce((sum, e) => sum + (e.phone.size || e.rip), 0));
  const shows = $derived((lib?.shows ?? []).filter((s) => s.episodes.length).sort((a, b) => (b.show.lastWatchedAt ?? 0) - (a.show.lastWatchedAt ?? 0)));

  async function get() {
    getting = true;
    try {
      await call("phoneGet", { ids: [...picked], plays: await phonePlays() });
      toast({ key: "get", state: "working", title: `Getting ${picked.size === 1 ? "1 episode" : `${picked.size} episodes`}`, text: "Your Library shows how it’s going." }, 4000);
      picked = new Set();
      goto("/phone");
    } catch (e) {
      toast({ key: "get", state: "failed", title: "Those weren’t got", text: errorText(e) });
    } finally {
      getting = false;
    }
  }

  const epText = (e: PcEpisode) => (e.episode.watchedAt ? "Watched" : e.episode.position > 30 ? "Started" : "");
</script>

{#snippet cover(show: Show)}<Cover cover={`/cover/${show.id}`} color={show.coverColor} native={show.titleNative} title={show.title} />{/snippet}

<div class="page">
  <header>
    <button type="button" class="link back" onclick={() => goto("/phone")}>‹ Library</button>
    <h1>Get from PC</h1>
  </header>

  {#if phone && !phone.pc}
    <section class="pair">
      <p>On your PC, open Telly’s <strong>Settings → Playing → Your phone</strong> and turn on lending to your phone. It shows a code to type here.</p>
      {#if looking}
        <p class="muted">Looking for Telly on your Wi-Fi…</p>
      {:else if pcs.length}
        <div class="found">
          {#each pcs as pc (pc.address)}
            <label><input type="radio" bind:group={address} value={pc.address} /> {pc.name} <span class="muted">{pc.address}</span></label>
          {/each}
        </div>
      {:else}
        <p class="muted">Telly on your PC wasn’t found on this Wi-Fi. Type the address Settings → Playing → Your phone shows. <button type="button" class="link" onclick={look}>Look again</button></p>
      {/if}
      <label class="field">Address <input type="text" inputmode="url" autocapitalize="off" placeholder="192.168.1.20:8775" bind:value={address} /></label>
      <label class="field">Code <input type="text" inputmode="numeric" maxlength="6" placeholder="123456" bind:value={code} /></label>
      {#if pairError}<p class="error">{pairError}</p>{/if}
      <button type="button" class="btn primary" disabled={pairing || !address || code.trim().length < 6} onclick={pair}>{pairing ? "Pairing…" : "Pair"}</button>
    </section>
  {:else if phone?.pc}
    <div class="status">
      <span>{phone.pc.name} · {lib ? `${bytes(lib.free)} free on the phone` : ""}</span>
      <button type="button" class="link" onclick={unpair} title="Pair with a PC again">Unpair</button>
    </div>
    {#if libError}
      <p class="error">{libError}</p>
      <button type="button" class="btn" onclick={loadPc}>Try again</button>
    {:else if !lib}
      <p class="muted">Asking your PC…</p>
    {:else}
      <ul class="shows">
        {#each shows as s (s.show.id)}
          {@const left = s.episodes.filter(gettable)}
          <li>
            <button type="button" class="show" onclick={() => (open = open === s.show.id ? null : s.show.id)} aria-expanded={open === s.show.id}>
              <span class="cover">{@render cover(s.show)}</span>
              <span class="show-text">
                <strong>{s.show.title}</strong>
                <span class="muted">{s.episodes.length} {s.episodes.length === 1 ? "episode" : "episodes"} · {s.episodes.filter((e) => e.onPhone === "ready").length} on the phone</span>
              </span>
            </button>
            {#if open === s.show.id}
              <div class="quick">
                <button type="button" class="btn small" disabled={!left.length} onclick={() => pickNext(s.episodes, 3)}>Pick the next 3</button>
              </div>
              <ul class="episodes">
                {#each s.episodes as e (e.episode.id)}
                  <li>
                    <label>
                      <input type="checkbox" disabled={!gettable(e)} checked={picked.has(e.episode.id) || e.onPhone !== "no"} onchange={() => toggle(e.episode.id)} />
                      <span class="ep">
                        <strong>Episode {e.episode.number}</strong>
                        <span class="muted">
                          {e.onPhone === "ready" ? "On the phone" : e.onPhone === "getting" ? "Being got" : bytes(e.phone.size || e.rip)}{epText(e) ? ` · ${epText(e)}` : ""}{!e.ready && e.onPhone === "no" ? " · the PC gets it ready first" : ""}
                        </span>
                      </span>
                    </label>
                  </li>
                {/each}
              </ul>
            {/if}
          </li>
        {/each}
      </ul>
      <div class="getbar">
        <span>{picked.size ? `${picked.size} picked · about ${bytes(pickedSize)}` : "Tick the episodes to copy"}</span>
        <button type="button" class="btn primary" disabled={!picked.size || getting} onclick={get}>Get</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .page { height: 100%; overflow-y: auto; padding: 12px 16px 96px; }
  header { display: flex; align-items: center; gap: 12px; padding: 4px 0 12px; }
  h1 { font: 700 20px/1 var(--font-display); margin: 0; }
  .back { text-decoration: none; font-size: 16px; padding-left: 0; }
  .btn { border: 1px solid var(--rule-2); background: var(--surface); color: var(--ink); border-radius: 10px; padding: 10px 16px; font: 600 15px var(--font-ui); }
  .btn.primary { background: var(--btn); color: var(--btn-ink); border-color: var(--btn); }
  .btn:disabled { opacity: .45; }
  .btn.small { padding: 7px 12px; font-size: 14px; }
  .link { border: 0; background: none; color: var(--ink-2); text-decoration: underline; font: 500 14px var(--font-ui); padding: 6px; }
  .muted { color: var(--ink-3); font-size: 13px; }
  .error { color: var(--bad); }
  .pair { display: flex; flex-direction: column; gap: 14px; }
  .pair p { line-height: 1.5; color: var(--ink-2); margin: 0; }
  .found { display: flex; flex-direction: column; gap: 8px; }
  .field { display: flex; flex-direction: column; gap: 6px; font-weight: 600; }
  .field input { font: 500 18px var(--font-mono); padding: 10px 12px; border-radius: 10px; border: 1px solid var(--rule-2); background: var(--surface); color: var(--ink); }
  .status { display: flex; justify-content: space-between; align-items: center; color: var(--ink-2); font-size: 14px; margin-bottom: 10px; }
  .shows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 10px; }
  .show { width: 100%; display: flex; gap: 12px; align-items: center; text-align: left; background: var(--surface); border: 1px solid var(--rule); border-radius: 12px; padding: 10px; color: var(--ink); }
  .show .cover { width: 46px; flex: none; }
  .show-text { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .quick { padding: 8px 4px 0; }
  .episodes { list-style: none; margin: 4px 0 0; padding: 0 4px; }
  .episodes label { display: flex; align-items: center; gap: 12px; padding: 10px 4px; border-bottom: 1px solid var(--rule); }
  .episodes input { width: 22px; height: 22px; }
  .ep { display: flex; flex-direction: column; gap: 2px; }
  .getbar { position: fixed; left: 0; right: 0; bottom: 0; display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 12px 16px calc(12px + env(safe-area-inset-bottom)); background: var(--surface); border-top: 1px solid var(--rule); }
</style>
