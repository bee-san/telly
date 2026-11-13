<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import StatTiles from "$lib/StatTiles.svelte";
  import { callKept, errorText, kept, setWindowTitle, type ShowStats } from "$lib/api";
  import { app } from "$lib/state.svelte";
  import { addDays, count, dayKey, dayName, dayOf, daysBetween, duration, durationParts, num, span, weekdayName } from "$lib/stats";

  /** The charts' heights, and how many mined words show before Show all. */
  const HB = 90;
  const HL = 90;
  const FIRST_WORDS = 8;

  const id = $derived(Number(page.url.searchParams.get("id")));
  let data = $state.raw<ShowStats | null>(null);
  let error = $state("");
  let hover = $state(-1);
  let allWords = $state(false);

  onMount(() => {
    app.dark = false;
  });

  $effect.pre(() => {
    const show = id;
    data = kept<ShowStats>("showStats", { id: show });
    error = "";
    hover = -1;
    allWords = false;
    callKept<ShowStats>("showStats", { id: show })
      .then((d) => {
        if (show !== id) return;
        data = d;
        setWindowTitle(`${d.card.title} · Stats — Telly`);
      })
      .catch((e) => {
        if (show === id) error = errorText(e);
      });
  });

  /** "Episode 3", "Episodes 3–5", "Episodes 3, 5". */
  function episodes(numbers: number[]): string {
    const n = [...numbers].sort((a, b) => a - b);
    if (n.length === 1) return `Episode ${n[0]}`;
    const run = n.every((x, i) => i === 0 || x === n[i - 1] + 1);
    return `Episodes ${run ? `${n[0]}–${n[n.length - 1]}` : n.join(", ")}`;
  }

  const view = $derived.by(() => {
    if (!data) return null;
    const card = data.card;
    const played = data.played;
    const done = data.completion.finishedAt !== null ? [data.completion.finishedAt, ...[...data.completion.rewatches].reverse()] : [];

    let seconds = 0, lines = 0, chars = 0;
    for (const p of played) {
      seconds += p.seconds;
      lines += p.lines;
      chars += p.chars;
    }
    const firstDay = played.length ? dayOf(played[0].day) : null;
    const lastDay = played.length ? dayOf(played[played.length - 1].day) : null;
    const hours = seconds / 3600;
    const tiles = [
      {
        label: "Time watched",
        parts: durationParts(seconds),
        sub: done.length > 1 ? (done.length === 2 ? "Watched twice" : `Watched ${done.length} times`) : firstDay && lastDay ? `Over ${count(daysBetween(firstDay, lastDay) + 1, "day")}` : "",
      },
      { label: "Lines", parts: [{ n: num(lines), unit: "" }], sub: count(chars, "character") },
      { label: "Cards mined", parts: [{ n: num(card.cards), unit: "" }], sub: hours >= 1 ? `About ${num(Math.round(card.cards / hours))} an hour` : "" },
    ];
    if (card.wordsUses > 0) {
      tiles.push({ label: "Words known", parts: [{ n: String(Math.round((100 * card.wordsKnown) / card.wordsUses)), unit: "%" }], sub: `${num(card.wordsKnown)} of ${count(card.wordsUses, "word")}` });
    }

    const meta = [
      card.movie ? "Movie" : count(card.episodes, "episode"),
      done.length > 1
        ? `Finished ${dayName(new Date(done[0]), true)} · Rewatched ${dayName(new Date(done[done.length - 1]), true)}`
        : firstDay
          ? span(firstDay, done.length ? new Date(done[0]) : lastDay!)
          : done.length
            ? `Finished ${dayName(new Date(done[0]), true)}`
            : "",
    ].filter(Boolean).join(" · ");

    // Each episode: its cards, its share of known words, the time it played and the last day it did.
    const cardsBy = new Map<number, number>();
    for (const c of data.cards) cardsBy.set(c.episode, (cardsBy.get(c.episode) ?? 0) + 1);
    const timeBy = new Map<number, number>();
    const dayBy = new Map<number, string>();
    for (const p of played) {
      timeBy.set(p.episode, (timeBy.get(p.episode) ?? 0) + p.seconds);
      dayBy.set(p.episode, p.day);
    }
    const E = data.episodes.length;
    const pcts = data.episodes.map((e) => (e.words?.uses ? Math.round((100 * e.words.known) / e.words.uses) : null));
    const cards = data.episodes.map((e) => cardsBy.get(e.number) ?? 0);
    const cmax = Math.max(0, ...cards);
    const cStep = [2, 5, 10, 20, 50, 100, 200, 500].find((s) => Math.ceil(cmax / s) <= 4) ?? 500;
    const cAxis = Math.max(cStep, Math.ceil(cmax / cStep) * cStep);
    const cGrid = [];
    for (let v = cStep; v <= cAxis; v += cStep) cGrid.push({ label: num(v), y: Math.round((HB * v) / cAxis) });
    const scored = pcts.filter((p): p is number => p !== null);
    // The share's axis fits its episodes, in tens, at least 20 points from bottom to top.
    let lo = Math.max(0, Math.floor((Math.min(100, ...scored) - 2) / 10) * 10);
    let hi = Math.min(100, Math.ceil((Math.max(0, ...scored) + 2) / 10) * 10);
    if (hi - lo < 20) hi = Math.min(100, lo + 20);
    if (hi - lo < 20) lo = hi - 20;
    const kStep = hi - lo <= 50 ? 10 : 20;
    const kGrid = [];
    for (let v = lo + kStep; v <= hi; v += kStep) kGrid.push({ label: `${v}%`, y: Math.round((HL * (v - lo)) / (hi - lo)) });
    const xAt = (i: number) => ((i + 0.5) / E) * 100;
    const yAt = (p: number) => ((hi - p) / (hi - lo)) * 100;
    // The line breaks where an episode's words haven't been worked out.
    const kLine = pcts.map((p, i) => (p === null ? "" : `${i === 0 || pcts[i - 1] === null ? "M" : "L"} ${xAt(i).toFixed(2)} ${yAt(p).toFixed(2)}`)).filter(Boolean).join(" ");
    const xStep = [1, 2, 5, 10, 20, 50, 100].find((k) => E / k <= 12) ?? 100;
    const eps = data.episodes.map((e, i) => ({
      number: e.number,
      pct: pcts[i],
      h: Math.round((HB * cards[i]) / cAxis),
      x: xAt(i),
      y: pcts[i] === null ? 0 : (HL * (pcts[i]! - lo)) / (hi - lo),
      label: (i + 1) % xStep === 0 || (i === 0 && xStep >= 5) ? String(e.number) : "",
      text: [
        `Episode ${e.number}`,
        dayBy.has(e.number) ? weekdayName(dayOf(dayBy.get(e.number)!), true) : "",
        timeBy.get(e.number) ? duration(timeBy.get(e.number)!) : "",
        count(cards[i], "card"),
        pcts[i] === null ? "" : `${pcts[i]}% known`,
      ].filter(Boolean).join(" · "),
    }));
    const most = cards.indexOf(cmax);
    const summary = cmax ? `Most cards: ${cmax}, Episode ${data.episodes[most].number}` : "No cards mined";

    // When you watched: a row for each time through, split at each finish.
    const ends = done.map((at) => dayKey(new Date(at)));
    const passes: { days: Map<string, number[]> }[] = [];
    for (const p of played) {
      let k = ends.findIndex((end) => p.day <= end);
      if (k === -1) k = ends.length;
      while (passes.length <= k) passes.push({ days: new Map() });
      const list = passes[k].days.get(p.day) ?? [];
      list.push(p.episode);
      passes[k].days.set(p.day, list);
    }
    const shade = (n: number, weekly: boolean) => (n >= (weekly ? 8 : 3) ? 1 : n >= (weekly ? 4 : 2) ? 0.62 : 0.34);
    const rows = passes
      .map((pass, k) => ({ pass, k }))
      .filter(({ pass }) => pass.days.size > 0)
      .map(({ pass, k }, _, all) => {
        const keys = [...pass.days.keys()].sort();
        const a = dayOf(keys[0]);
        const b = dayOf(keys[keys.length - 1]);
        const length = daysBetween(a, b) + 1;
        const weekly = length > 84;
        const cells: { title: string; opacity: number }[] = [];
        let on = 0;
        for (let i = 0; i < length; i += weekly ? 7 : 1) {
          const days = weekly ? Array.from({ length: Math.min(7, length - i) }, (_, j) => dayKey(addDays(a, i + j))) : [dayKey(addDays(a, i))];
          const watched = days.flatMap((d) => pass.days.get(d) ?? []);
          const n = new Set(watched).size;
          if (n) on++;
          const what = n ? (weekly ? count(watched.length, "episode") : episodes([...new Set(watched)])) : "Nothing watched";
          cells.push({ title: `${weekly ? `Week of ${dayName(addDays(a, i), true)}` : weekdayName(addDays(a, i))} · ${what}`, opacity: n ? shade(weekly ? watched.length : n, weekly) : 0 });
        }
        return {
          label: all.length > 1 ? (k === 0 ? "First time" : k < done.length ? "Rewatch" : "Watching again") : "",
          head: `On ${num(on)} of ${count(cells.length, weekly ? "week" : "day")} · ${span(a, b)}`,
          cells,
        };
      });

    // The cards mined, the word marked in its sentence when Telly knows the word.
    const words = data.cards.map((c) => {
      const at = c.word ? c.sentence.indexOf(c.word) : -1;
      return {
        note: c.note,
        word: c.word,
        episode: `Episode ${c.episode}`,
        before: at >= 0 ? c.sentence.slice(0, at) : c.sentence,
        mark: at >= 0 ? c.word! : "",
        after: at >= 0 ? c.sentence.slice(at + c.word!.length) : "",
      };
    });

    return { card, tiles, meta, eps, cGrid, kGrid, kLine, summary, rows, words, chart: !card.movie && E > 1 };
  });

  const readout = $derived(view ? (hover >= 0 && hover < view.eps.length ? view.eps[hover].text : view.summary) : "");
  const shownWords = $derived(view ? (allWords ? view.words : view.words.slice(0, FIRST_WORDS)) : []);
</script>

<div class="st scroll">
  <div class="st-in">
    <div class="st-top">
      <button type="button" class="back" onclick={() => goto("/stats")}><Icon name="back" /><span>Back to stats</span></button>
      {#if view}
        <div class="sh-head">
          <span class="sh-cover" aria-hidden="true"><Cover cover={view.card.cover} color={view.card.coverColor} native={view.card.titleNative} title={view.card.title} caption={view.card.coverCaption} /></span>
          <div class="sh-text">
            <h1 lang={view.card.titleNative ? "ja" : undefined}>{view.card.titleNative || view.card.title}</h1>
            {#if view.card.titleNative}<p>{view.card.title}</p>{/if}
            {#if view.meta}<p class="sh-meta">{view.meta}</p>{/if}
          </div>
        </div>
      {/if}
    </div>

    {#if error}<p class="err">{error}</p>{/if}

    {#if view}
      <StatTiles tiles={view.tiles} />

      {#if view.chart}
        <section aria-labelledby="h-ep">
          <div class="sec-head">
            <h2 id="h-ep">Each episode</h2>
            <p class="readout" aria-live="polite">{readout}</p>
          </div>
          <div class="eps" class:many={view.eps.length > 60}>
            <span class="eh">Cards mined</span>
            <div class="chart">
              <div class="axis" aria-hidden="true" style:height="{HB}px">
                {#each view.cGrid as g (g.y)}<span style:bottom="{g.y}px">{g.label}</span>{/each}
              </div>
              <div class="plot" style:height="{HB}px">
                {#each view.cGrid as g (g.y)}<span class="gl" aria-hidden="true" style:bottom="{g.y}px"></span>{/each}
                <div class="cols" aria-hidden="true">
                  {#each view.eps as e, i (i)}
                    <span class="col"><span class="bar" class:dim={hover !== -1 && hover !== i} style:height="{e.h}px"></span></span>
                  {/each}
                </div>
              </div>
            </div>
            <span class="eh second">Words known</span>
            <div class="chart">
              <div class="axis" aria-hidden="true" style:height="{HL}px">
                {#each view.kGrid as g (g.y)}<span style:bottom="{g.y}px">{g.label}</span>{/each}
              </div>
              <div class="plot" style:height="{HL}px">
                {#each view.kGrid as g (g.y)}<span class="gl" aria-hidden="true" style:bottom="{g.y}px"></span>{/each}
                <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true"><path class="k-line" d={view.kLine}></path></svg>
                {#each view.eps as e, i (i)}
                  {#if e.pct !== null}
                    <span class="dot" class:dim={hover !== -1 && hover !== i} class:big={hover === i} aria-hidden="true" style:left="{e.x}%" style:bottom="{e.y}px"></span>
                  {/if}
                {/each}
              </div>
            </div>
            <div class="xl" aria-hidden="true">
              {#each view.eps as e, i (i)}<span><span>{e.label}</span></span>{/each}
            </div>
            <div class="cols hits">
              {#each view.eps as e, i (i)}
                <button type="button" class="hit" aria-label={e.text} onmouseenter={() => (hover = i)} onfocus={() => (hover = i)} onmouseleave={() => (hover = -1)} onblur={() => (hover = -1)}></button>
              {/each}
            </div>
          </div>
        </section>
      {/if}

      <section aria-labelledby="h-when">
        <h2 id="h-when">When you watched</h2>
        {#if !view.rows.length}
          <p class="note">Nothing of this show is in Telly’s log of what was watched.</p>
        {/if}
        {#each view.rows as r, i (i)}
          <div class="pass">
            <p class="readout">{#if r.label}<b>{r.label}</b>{" · "}{/if}{r.head}</p>
            <div class="cells" role="img" aria-label={r.head}>
              {#each r.cells as c, j (j)}<span title={c.title} class:on={c.opacity > 0} style:--o={c.opacity}></span>{/each}
            </div>
          </div>
        {/each}
      </section>

      {#if view.words.length}
        <section aria-labelledby="h-mined">
          <div class="sec-head start">
            <h2 id="h-mined">Words you mined</h2>
            <span class="readout">{num(view.words.length)}</span>
          </div>
          <ul class="mined">
            {#each shownWords as w (w.note)}
              <li>
                <span class="m-word" lang="ja">{w.word ?? ""}</span>
                <span class="m-line" lang="ja">「{w.before}{#if w.mark}<mark>{w.mark}</mark>{/if}{w.after}」</span>
                <span class="m-ep">{w.episode}</span>
              </li>
            {/each}
          </ul>
          {#if !allWords && view.words.length > FIRST_WORDS}
            <div class="more">
              <span>{count(view.words.length - FIRST_WORDS, "more word")}</span>
              <button type="button" class="link-btn" onclick={() => (allWords = true)}>Show all</button>
            </div>
          {/if}
        </section>
      {/if}
    {/if}
  </div>
</div>

<style>
  .st { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 56px; }
  .st-in { max-width: 1080px; margin: 0 auto; display: flex; flex-direction: column; gap: 36px; }
  .st-top { display: flex; flex-direction: column; gap: 14px; }
  .st-top .back { align-self: flex-start; }
  .sh-head { display: flex; flex-wrap: wrap; align-items: center; gap: 22px; }
  .sh-cover { flex: none; width: 100px; }
  .sh-text { flex: 1 1 300px; min-width: 0; display: flex; flex-direction: column; gap: 4px; }
  h1 { font: 600 30px/1.2 var(--font-display); }
  h1[lang="ja"] { font: 800 30px/1.2 var(--font-jp-title); letter-spacing: .02em; }
  .sh-text p { font-size: 14px; color: var(--ink-2); }
  .sh-text .sh-meta { margin-top: 6px; font-size: 13.5px; }
  section { display: flex; flex-direction: column; gap: 14px; }
  .sec-head { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: 6px 16px; }
  .sec-head.start { justify-content: flex-start; gap: 10px; }
  h2 { font: 600 17px/1.2 var(--font-ui); }
  .readout { font-size: 13px; color: var(--ink-2); }
  .readout b { font-weight: 600; color: var(--ink); }
  .note { font-size: 14px; color: var(--ink-2); }

  /* each episode: two charts sharing one row of episodes, and one hit area for both */
  .eps { position: relative; display: flex; flex-direction: column; gap: 8px; }
  .eh { padding-left: 44px; font: 600 13px/1.3 var(--font-ui); color: var(--ink-2); }
  .eh.second { margin-top: 16px; }
  .chart { display: flex; gap: 10px; }
  .axis { position: relative; flex: none; width: 34px; }
  .axis span { position: absolute; right: 0; transform: translateY(50%); font: 11px/1 var(--font-mono); color: var(--ink-2); white-space: nowrap; }
  .plot { position: relative; flex: 1; min-width: 0; border-bottom: 1px solid var(--rule-2); }
  .gl { position: absolute; left: 0; right: 0; border-top: 1px solid var(--rule); }
  .cols { position: absolute; inset: 0; display: flex; }
  .col { flex: 1 1 0; min-width: 0; height: 100%; display: flex; align-items: flex-end; justify-content: center; }
  .bar { display: block; width: min(24px, 70%); border-radius: 4px 4px 0 0; background: var(--ink); }
  .many .bar { border-radius: 1px 1px 0 0; }
  .bar.dim, .dot.dim { opacity: .2; }
  .plot svg { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; }
  .k-line { fill: none; stroke: var(--ink); stroke-width: 2px; stroke-linejoin: round; stroke-linecap: round; vector-effect: non-scaling-stroke; }
  .dot { position: absolute; width: 8px; height: 8px; margin: 0 0 -4px -4px; border-radius: 50%; background: var(--ink); box-shadow: 0 0 0 2px var(--bg); pointer-events: none; }
  .dot.big { width: 10px; height: 10px; margin: 0 0 -5px -5px; }
  .many .dot:not(.big) { display: none; }
  .xl { display: flex; height: 15px; margin-left: 44px; }
  .xl > span { flex: 1 1 0; min-width: 0; position: relative; }
  .xl > span > span { position: absolute; left: 50%; top: 0; transform: translateX(-50%); white-space: nowrap; font: 11px/1.3 var(--font-mono); color: var(--ink-2); }
  .hits { left: 44px; }
  .hit { flex: 1 1 0; min-width: 0; height: 100%; padding: 0; border: 0; background: transparent; cursor: default; }
  .hit:focus-visible { outline-offset: -2px; }

  /* when you watched: a square a day (or a week), darker for more episodes */
  .pass { display: flex; flex-direction: column; gap: 8px; }
  .cells { display: flex; flex-wrap: wrap; gap: 3px; }
  .cells span { display: block; width: 16px; height: 16px; border-radius: 3px; background: var(--sunk); }
  .cells span.on { background: var(--ink); opacity: var(--o); }

  /* words you mined */
  .mined { list-style: none; padding: 0; border-top: 1px solid var(--rule); }
  .mined li { display: grid; grid-template-columns: minmax(120px, 200px) minmax(0, 1fr) auto; align-items: center; gap: 6px 20px; padding: 10px 4px; border-bottom: 1px solid var(--rule); }
  .m-word { font: 800 20px/1.25 var(--font-jp-title); min-width: 0; overflow-wrap: anywhere; }
  .m-line { min-width: 0; font: 500 15px/1.5 var(--font-jp); }
  .m-line mark { font-weight: 700; color: inherit; background: linear-gradient(transparent 58%, var(--mark-soft) 58%); }
  .m-ep { font-size: 13px; color: var(--ink-2); white-space: nowrap; }
  .more { display: flex; align-items: center; gap: 14px; padding: 4px 4px 0; font-size: 13.5px; color: var(--ink-2); }
  .more .link-btn { color: var(--ink); }
</style>
