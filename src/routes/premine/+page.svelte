<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import { call, errorText, setWindowTitle, type PremineStatus, type PremineWord, type ShowView } from "$lib/api";
  import { clock, plural } from "$lib/format";
  import { app } from "$lib/state.svelte";
  import { toast } from "$lib/toast-store.svelte";

  const id = $derived(Number(page.url.searchParams.get("show")));
  let view = $state<ShowView | null>(null);
  let status = $state<PremineStatus | null>(null);
  let error = $state("");
  /** The share of the show's words to know, in per cent. */
  let target = $state<number | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** Cards already reported, so each batch is toasted once. */
  let reported = false;

  const key = (w: PremineWord) => `${w.headword}|${w.reading}`;
  const list = $derived(status && status.show === id ? status.list : null);
  /** The share known now, in per cent. */
  const known = $derived(list && list.uses ? (100 * list.knownUses) / list.uses : 0);
  /** The lowest whole target above it. */
  const lowest = $derived(Math.min(100, Math.floor(known) + 1));
  /** Once its cards are made, the target stays put, with nothing left to make. */
  const aim = $derived(Math.min(100, target ?? Math.max(90, lowest)));
  /** The fewest words, most used first, that reach the target, in the order they first come up. */
  const needed = $derived.by(() => {
    if (!list) return [];
    const out: PremineWord[] = [];
    let uses = list.knownUses;
    for (const w of list.words) {
      if (uses * 100 >= aim * list.uses) break;
      out.push(w);
      uses += w.count;
    }
    return out.sort((a, b) => a.number - b.number || a.start - b.start);
  });

  onMount(() => {
    app.dark = false;
    (async () => {
      try {
        view = await call<ShowView>("show", { id });
        setWindowTitle(`Premine ${view.card.title} — Telly`);
        await start();
      } catch (e) {
        error = errorText(e);
      }
    })();
    return () => clearTimeout(timer);
  });

  async function start() {
    error = "";
    try {
      status = await call<PremineStatus>("premine", { show: id });
      follow();
    } catch (e) {
      error = errorText(e);
    }
  }

  function follow() {
    clearTimeout(timer);
    if (!status?.working) return report();
    timer = setTimeout(async () => {
      try {
        status = await call<PremineStatus>("premineStatus");
      } catch (e) {
        error = errorText(e);
        return;
      }
      follow();
    }, 500);
  }

  function report() {
    if (!status || reported || status.added === 0) return;
    reported = true;
    toast({
      key: "premined",
      state: "done",
      title: `${plural(status.added, "card")} added`,
      text: `In ${list?.deck ?? "Anki"}. Telly is adding their pictures and audio.`,
    });
  }

  async function make() {
    if (!needed.length) return;
    reported = false;
    try {
      status = await call<PremineStatus>("premineCards", { words: needed });
      follow();
    } catch (e) {
      error = errorText(e);
    }
  }

  /** "Episodes 7 and 8", "Episode 9". */
  function listed(numbers: number[]): string {
    if (numbers.length === 1) return `Episode ${numbers[0]}`;
    return `Episodes ${numbers.slice(0, -1).join(", ")} and ${numbers[numbers.length - 1]}`;
  }
</script>

<div class="pm scroll">
  <button type="button" class="back" onclick={() => goto(`/show?id=${id}`)}><Icon name="back" /><span>Back</span></button>
  {#if view}
    <div class="pm-head">
      <h2>Premine</h2>
      <p class="pm-meta" lang="ja">{view.card.titleNative || view.card.title}</p>
    </div>
  {/if}

  {#if error}
    <p class="err">{error}</p>
  {/if}

  {#if status?.working}
    <p class="pm-step"><span class="spin"></span>{status.step}</p>
    <span class="bar pm-bar"><i style:width="{Math.round(100 * status.progress)}%"></i></span>
  {:else if status?.error}
    <p class="err">{status.error}</p>
    <button type="button" class="btn quiet" onclick={start}>Try again</button>
  {/if}

  {#if list}
    {#if list.withoutSubs.length}
      <p class="pm-note">{listed(list.withoutSubs)} {list.withoutSubs.length === 1 ? "has" : "have"} no subtitles yet. Premining needs subtitles for every episode.</p>
    {:else}
      <p class="pm-sum"><b>{Math.floor(known)}%</b> of the words in the show are known</p>
      {#if status && !status.working && status.skipped.length}
        <div class="pm-skipped">
          <p>{plural(status.skipped.length, "card")} not added:</p>
          <ul>{#each status.skipped as s (s)}<li>{s}</li>{/each}</ul>
        </div>
      {/if}

      {#if known < 100}
        <div class="pm-bar-row">
          <label class="pm-target">
            <span>Target</span>
            <input class="input" type="number" min={lowest} max="100" step="1" value={aim} disabled={status?.working}
              oninput={(e) => { const n = e.currentTarget.valueAsNumber; if (!Number.isNaN(n)) target = n; }} />
            <span>%</span>
          </label>
          <span class="pm-deck">{plural(needed.length, "word")} · cards go into {list.deck}</span>
          <span class="grow"></span>
          <button type="button" class="btn" disabled={!needed.length || status?.working} onclick={make}>
            {needed.length ? `Make ${plural(needed.length, "card")}` : "Make cards"}
          </button>
        </div>

        <div class="pm-row head">
          <span>Word</span><span class="num">Times</span><span class="num">Rank</span><span>First line</span>
        </div>
        {#each needed as w (key(w))}
          <div class="pm-row">
            <span class="pm-word"><b lang="ja">{w.headword}</b>{#if w.reading && w.reading !== w.headword}<span lang="ja">{w.reading}</span>{/if}</span>
            <span class="num">×{w.count}</span>
            <span class="num rank">{w.rank ? `#${w.rank.toLocaleString("en-GB")}` : "–"}</span>
            <span class="pm-line"><q lang="ja">{w.before}<mark>{w.word}</mark>{w.after}</q><small>E{w.number} · {clock(w.start)}</small></span>
          </div>
        {/each}
      {/if}
    {/if}
  {/if}
</div>

<style>
  .pm { position: absolute; inset: 0; overflow: auto; padding: 18px 32px 40px; }
  .pm-head { margin: 14px 0 18px; }
  .pm-head h2 { font: 600 26px/1.2 var(--font-ui); }
  .pm-meta { font-size: 13.5px; color: var(--ink-2); margin-top: 4px; }
  .pm-step { display: flex; align-items: center; gap: 10px; font-size: 14px; color: var(--ink-2); margin: 6px 0 8px; }
  .pm-bar { width: 240px; margin-bottom: 18px; }
  .pm-sum { font-size: 14.5px; margin: 4px 0 6px; }
  .pm-sum b { font-variant-numeric: tabular-nums; }
  .pm-note { font-size: 13px; color: var(--ink-2); margin: 4px 0 10px; }
  .pm-skipped { font-size: 13px; color: var(--ink-2); margin: 6px 0 10px; }
  .pm-skipped ul { margin: 4px 0 0; padding-left: 20px; }
  .pm-bar-row { display: flex; align-items: center; gap: 14px; flex-wrap: wrap; padding: 12px 0; margin-top: 10px; border-top: 1px solid var(--rule); border-bottom: 1px solid var(--rule); }
  .pm-deck { font-size: 13px; color: var(--ink-2); }
  .pm-target { display: flex; align-items: center; gap: 6px; font-size: 13.5px; }
  .pm-target input { width: 72px; font-variant-numeric: tabular-nums; }
  .pm-row { display: grid; grid-template-columns: 180px 60px 84px minmax(0, 1fr); gap: 12px; align-items: center; padding: 8px 4px; border-bottom: 1px solid var(--rule); font-size: 13px; }
  .pm-row.head { font: 600 12px/1 var(--font-ui); color: var(--ink-3); padding: 10px 4px; }
  .pm-word { display: flex; align-items: baseline; gap: 8px; min-width: 0; }
  .pm-word b { font: 700 18px/1.25 var(--font-jp); }
  .pm-word span { font: 500 12.5px/1 var(--font-jp); color: var(--ink-2); }
  .num { text-align: right; font-variant-numeric: tabular-nums; color: var(--ink-2); }
  .rank { font-family: var(--font-mono); font-size: 12.5px; color: var(--ink-3); }
  .pm-line { display: flex; align-items: baseline; gap: 10px; min-width: 0; }
  .pm-line q { font: 500 14.5px/1.45 var(--font-jp); quotes: "「" "」"; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .pm-line mark { font-weight: 700; color: inherit; background: linear-gradient(transparent 58%, var(--mark-soft) 58%); }
  .pm-line small { font-size: 12px; color: var(--ink-3); white-space: nowrap; }
</style>
