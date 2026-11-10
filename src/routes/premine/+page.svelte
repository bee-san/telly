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
  /** How many episodes from the next one. */
  let count = $state(1);
  let ticked = $state<Set<string>>(new Set());
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** Cards already reported, so each batch is toasted once. */
  let reported = false;

  const key = (w: PremineWord) => `${w.headword}|${w.reading}`;
  const from = $derived(view?.card.next?.number ?? 1);
  const list = $derived(status && status.show === id && status.from === from && status.count === count ? status.list : null);
  const words = $derived(list?.words ?? []);
  const picked = $derived(words.filter((w) => ticked.has(key(w))));
  const allTicked = $derived(words.length > 0 && picked.length === words.length);

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
      status = await call<PremineStatus>("premine", { show: id, from, count });
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

  function choose(n: number) {
    if (n === count) return;
    count = n;
    ticked = new Set();
    start();
  }

  function top(n: number) {
    ticked = new Set(words.slice(0, n).map(key));
  }

  function toggle(w: PremineWord) {
    const next = new Set(ticked);
    if (next.has(key(w))) next.delete(key(w));
    else next.add(key(w));
    ticked = next;
  }

  async function make() {
    if (!picked.length) return;
    reported = false;
    try {
      status = await call<PremineStatus>("premineCards", { words: picked });
      ticked = new Set();
      follow();
    } catch (e) {
      error = errorText(e);
    }
  }

  /** "Episode 4", or "Episodes 4–6". */
  function span(numbers: number[]): string {
    if (!numbers.length) return "these episodes";
    const [a, b] = [numbers[0], numbers[numbers.length - 1]];
    return a === b ? `Episode ${a}` : `Episodes ${a}–${b}`;
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
      <div>
        <h2>Premine</h2>
        <p class="pm-meta"><span lang="ja">{view.card.titleNative || view.card.title}</span> · from Episode {from}</p>
      </div>
      <div class="seg" role="group" aria-label="Episodes">
        {#each [[1, "Next episode"], [3, "Next 3"], [10, "Next 10"]] as [n, label] (n)}
          <button type="button" aria-pressed={count === n} disabled={status?.working} onclick={() => choose(n as number)}>{label}</button>
        {/each}
      </div>
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
    <p class="pm-sum">
      <b>{Math.round(100 * list.known)}%</b> of the words in {span(list.numbers)} are known
      · {plural(words.length, "new word")} · {list.learning} learning
    </p>
    {#if list.withoutSubs.length}
      <p class="pm-note">{listed(list.withoutSubs)} {list.withoutSubs.length === 1 ? "has" : "have"} no subtitles yet, so {list.withoutSubs.length === 1 ? "its" : "their"} words aren’t here.</p>
    {/if}
    {#if status && !status.working && status.skipped.length}
      <div class="pm-skipped">
        <p>{plural(status.skipped.length, "card")} not added:</p>
        <ul>{#each status.skipped as s (s)}<li>{s}</li>{/each}</ul>
      </div>
    {/if}

    {#if words.length}
      <div class="pm-bar-row">
        <div class="seg" role="group" aria-label="Tick">
          {#each [20, 40, 80] as n (n)}
            <button type="button" disabled={status?.working} onclick={() => top(n)}>Top {n}</button>
          {/each}
        </div>
        <span class="grow"></span>
        <span class="pm-deck">Cards go into {list.deck}</span>
        <button type="button" class="btn" disabled={!picked.length || status?.working} onclick={make}>
          {picked.length ? `Make ${plural(picked.length, "card")}` : "Make cards"}
        </button>
      </div>

      <div class="pm-row head">
        <input type="checkbox" checked={allTicked} aria-label="Tick every word" onchange={() => (ticked = allTicked ? new Set() : new Set(words.map(key)))} />
        <span>Word</span><span class="num">Times</span><span class="num">Rank</span><span>First line</span>
      </div>
      {#each words as w (key(w))}
        <label class="pm-row">
          <input type="checkbox" checked={ticked.has(key(w))} disabled={status?.working} onchange={() => toggle(w)} aria-label="Make a card for {w.headword}" />
          <span class="pm-word"><b lang="ja">{w.headword}</b>{#if w.reading && w.reading !== w.headword}<span lang="ja">{w.reading}</span>{/if}</span>
          <span class="num">×{w.count}</span>
          <span class="num rank">{w.rank ? `#${w.rank.toLocaleString("en-GB")}` : "–"}</span>
          <span class="pm-line"><q lang="ja">{w.before}<mark>{w.word}</mark>{w.after}</q><small>E{w.number} · {clock(w.start)}</small></span>
        </label>
      {/each}
    {:else if !status?.working}
      <p class="pm-note">Every word in {span(list.numbers)} has a card already.</p>
    {/if}
  {/if}
</div>

<style>
  .pm { position: absolute; inset: 0; overflow: auto; padding: 18px 32px 40px; }
  .pm-head { display: flex; align-items: end; justify-content: space-between; gap: 16px; flex-wrap: wrap; margin: 14px 0 18px; }
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
  .pm-row { display: grid; grid-template-columns: 24px 180px 60px 84px minmax(0, 1fr); gap: 12px; align-items: center; padding: 8px 4px; border-bottom: 1px solid var(--rule); font-size: 13px; cursor: pointer; }
  .pm-row.head { font: 600 12px/1 var(--font-ui); color: var(--ink-3); padding: 10px 4px; cursor: default; }
  .pm-row input { width: 16px; height: 16px; margin: 0; accent-color: var(--ink); }
  .pm-row:not(.head):hover { background: var(--surface); }
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
