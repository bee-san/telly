<script module lang="ts">
  type Period = "d7" | "d30" | "m12" | "all";
  /** The period picked last, kept while Telly is open: coming back from a show's page keeps it. */
  let lastPeriod: Period = "d30";
</script>

<script lang="ts">
  import { goto } from "$app/navigation";
  import { onMount } from "svelte";
  import Cover from "$lib/Cover.svelte";
  import Icon from "$lib/Icon.svelte";
  import StatTiles from "$lib/StatTiles.svelte";
  import { callKept, errorText, kept, setWindowTitle, type Stats } from "$lib/api";
  import { app } from "$lib/state.svelte";
  import { MONTHS, WEEKDAYS, addDays, count, dayKey, dayName, dayOf, daysBetween, duration, durationParts, num, span, weekdayName } from "$lib/stats";

  const PERIODS: [Period, string][] = [["d7", "7 days"], ["d30", "30 days"], ["m12", "12 months"], ["all", "All time"]];
  /** The charts' heights, in pixels. */
  const H = 200;
  const WH = 180;

  let stats = $state.raw<Stats | null>(kept<Stats>("stats"));
  let error = $state("");
  let period = $state<Period>(lastPeriod);
  let hover = $state(-1);
  let whover = $state(-1);

  onMount(() => {
    app.dark = false;
    setWindowTitle("Stats — Telly");
    callKept<Stats>("stats")
      .then((s) => {
        stats = s;
        error = "";
      })
      .catch((e) => (error = errorText(e)));
  });

  function pick(p: Period) {
    period = p;
    lastPeriod = p;
    hover = -1;
    whover = -1;
  }

  interface Bucket {
    /** Its first and last day's keys. */
    from: string;
    to: string;
    when: string;
    x: string;
    seconds: number;
    episodes: number;
    cards: number;
  }

  /** "Up 13% on the 30 days before". */
  function change(now: number, before: number, what: string): string {
    if (before === 0) return now === 0 ? "" : `None in ${what}`;
    const d = Math.round((100 * (now - before)) / before);
    return d === 0 ? `Same as ${what}` : `${d > 0 ? "Up" : "Down"} ${Math.abs(d)}% on ${what}`;
  }

  /** A round step for an axis that climbs by about `x` a line. */
  function nice(x: number): number {
    const e = Math.pow(10, Math.floor(Math.log10(Math.max(1, x))));
    const m = x / e;
    return (m <= 1 ? 1 : m <= 2 ? 2 : m <= 5 ? 5 : 10) * e;
  }

  const view = $derived.by(() => {
    if (!stats) return null;
    const today = dayOf(dayKey(new Date()));
    const todayKey = dayKey(today);
    const first = stats.days.length ? dayOf(stats.days[0].day) : today;
    const byDay = period === "d7" || period === "d30";

    // One bar a day, or a month.
    const buckets: Bucket[] = [];
    let start: Date;
    let range: string;
    let since: string;
    let before = "";
    if (byDay) {
      const n = period === "d7" ? 7 : 30;
      start = addDays(today, 1 - n);
      for (let i = 0; i < n; i++) {
        const d = addDays(start, i);
        const x = n <= 7 ? `${WEEKDAYS[d.getDay()].slice(0, 3)} ${d.getDate()}` : d.getDay() === 1 ? `${d.getDate()} ${MONTHS[d.getMonth()].slice(0, 3)}` : "";
        buckets.push({ from: dayKey(d), to: dayKey(d), when: weekdayName(d), x, seconds: 0, episodes: 0, cards: 0 });
      }
      range = span(start, today);
      since = `in these ${n} days`;
      before = `the ${n} days before`;
    } else {
      const from = period === "m12" ? new Date(today.getFullYear(), today.getMonth() - 11, 1) : new Date(first.getFullYear(), first.getMonth(), 1);
      start = period === "m12" ? from : first;
      const months = (today.getFullYear() - from.getFullYear()) * 12 + today.getMonth() - from.getMonth() + 1;
      for (let i = 0; i < months; i++) {
        const m = new Date(from.getFullYear(), from.getMonth() + i, 1);
        const shown = months <= 18 || m.getMonth() % 3 === 0 || i === 0;
        const x = shown ? MONTHS[m.getMonth()].slice(0, 3) + (i === 0 || m.getMonth() === 0 ? ` ${m.getFullYear()}` : "") : "";
        const last = new Date(m.getFullYear(), m.getMonth() + 1, 0);
        buckets.push({ from: dayKey(m), to: dayKey(last), when: `${MONTHS[m.getMonth()]} ${m.getFullYear()}`, x, seconds: 0, episodes: 0, cards: 0 });
      }
      range =
        period === "m12"
          ? `${MONTHS[from.getMonth()]} ${from.getFullYear()} – ${MONTHS[today.getMonth()]} ${today.getFullYear()}`
          : stats.days.length
            ? `Since ${dayName(first, true)}`
            : "Nothing watched yet";
      since = period === "m12" ? "in these 12 months" : stats.days.length ? `since ${dayName(first, true)}` : "";
    }
    const startKey = dayKey(start);
    const bucketOf = new Map(buckets.map((b) => [byDay ? b.from : b.from.slice(0, 7), b]));

    // The days in the period, added up, and the same length of time before it.
    let seconds = 0, episodes = 0, cards = 0, lines = 0, chars = 0, uses = 0, known = 0, watched = 0;
    const shows = new Set<number>();
    const prev = { seconds: 0, episodes: 0, cards: 0 };
    const prevKey = byDay ? dayKey(addDays(start, -buckets.length)) : "";
    for (const d of stats.days) {
      if (d.day >= startKey && d.day <= todayKey) {
        const b = bucketOf.get(byDay ? d.day : d.day.slice(0, 7));
        if (b) {
          b.seconds += d.seconds;
          b.episodes += d.episodes;
          b.cards += d.cards;
        }
        seconds += d.seconds;
        episodes += d.episodes;
        cards += d.cards;
        lines += d.lines;
        chars += d.chars;
        uses += d.uses;
        known += d.known;
        if (d.seconds > 0) watched++;
        d.shows.forEach((s) => shows.add(s));
      } else if (byDay && d.day >= prevKey && d.day < startKey) {
        prev.seconds += d.seconds;
        prev.episodes += d.episodes;
        prev.cards += d.cards;
      }
    }
    const dayCount = daysBetween(start, today) + 1;
    const hours = seconds / 3600;
    const tiles = [
      { label: "Time watched", parts: durationParts(seconds), sub: byDay ? change(seconds, prev.seconds, before) : seconds ? `${duration(seconds / dayCount)} a day on average` : "" },
      { label: "Episodes", parts: [{ n: num(episodes), unit: "" }], sub: byDay ? change(episodes, prev.episodes, before) : shows.size ? `From ${count(shows.size, "show")}` : "" },
      { label: "Lines", parts: [{ n: num(lines), unit: "" }], sub: count(chars, "character") },
      { label: "Cards mined", parts: [{ n: num(cards), unit: "" }], sub: byDay ? change(cards, prev.cards, before) : hours >= 1 ? `About ${num(Math.round(cards / hours))} an hour` : "" },
    ];

    // The time chart: up to four lines, at round hours (or quarter hours, when no bar reaches an hour).
    const top = buckets.reduce((a, b) => (b.seconds > a.seconds ? b : a), buckets[0]);
    const topHours = top.seconds / 3600;
    const steps = topHours <= 1 ? [0.25, 0.5, 1] : [1, 2, 3, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000];
    const step = steps.find((s) => Math.ceil(topHours / s) <= 4) ?? 1000;
    const axis = Math.max(step, Math.ceil(topHours / step) * step);
    const grid = [];
    for (let k = 1; k * step <= axis + 1e-9; k++) {
      const v = k * step;
      grid.push({ label: v < 1 ? `${Math.round(v * 60)} min` : `${num(v)} h`, y: Math.round((H * v) / axis) });
    }
    const bars = buckets.map((b) => ({
      ...b,
      h: Math.round((H * b.seconds) / 3600 / axis),
      text: [b.when, b.seconds > 0 ? duration(b.seconds) : "Nothing watched", b.seconds > 0 ? count(b.episodes, "episode") : "", b.cards ? count(b.cards, "card") : ""]
        .filter(Boolean)
        .join(" · "),
    }));
    const summary = seconds > 0 ? `Watched on ${num(watched)} of ${count(dayCount, "day")} · Most: ${duration(top.seconds)}, ${top.when}` : "Nothing watched";

    // Known words at the end of each bar's day or month, by the day their first card was made.
    const totalKnown = stats.words.reduce((a, w) => a + w.n, 0);
    let earlier = 0;
    for (const w of stats.words) if (w.day < startKey) earlier += w.n;
    const vals = buckets.map((b) => {
      let v = earlier;
      for (const w of stats!.words) if (w.day >= startKey && w.day <= b.to) v += w.n;
      return v;
    });
    const N = vals.length;
    const wStep = nice(Math.max(1, (vals[N - 1] - vals[0]) / 4));
    const lo = Math.floor(vals[0] / wStep) * wStep;
    const hi = Math.max(lo + wStep, Math.ceil(vals[N - 1] / wStep) * wStep);
    const xAt = (i: number) => ((i + 0.5) / N) * 100;
    const yAt = (v: number) => ((hi - v) / (hi - lo)) * 100;
    const wLine = vals.map((v, i) => `${i ? "L" : "M"} ${xAt(i).toFixed(2)} ${yAt(v).toFixed(2)}`).join(" ");
    const wGrid = [];
    for (let v = lo + wStep; v <= hi; v += wStep) wGrid.push({ label: num(v), y: Math.round((WH * (v - lo)) / (hi - lo)) });
    const wPts = vals.map((v, i) => ({
      v,
      x: xAt(i),
      y: (WH * (v - lo)) / (hi - lo),
      text: `${byDay ? buckets[i].when : `End of ${buckets[i].when}`} · ${count(v, "word")} known`,
    }));
    const meters = [
      { label: "In what you watched", uses, known },
      { label: "In your library", uses: stats.library.uses, known: stats.library.known },
    ]
      .filter((m) => m.uses > 0)
      .map((m) => ({ label: m.label, pct: Math.round((100 * m.known) / m.uses) }));

    // Shows finished or rewatched in the period, newest first, each once.
    // All time: every show finished, those marked as watched before anything was played too.
    const startMs = period === "all" ? 0 : start.getTime();
    const latest = new Map<number, { at: number; rewatch: boolean }>();
    for (const f of stats.finished) if (f.at >= startMs && stats.shows[f.show]) latest.set(f.show, { at: f.at, rewatch: f.rewatch });
    const finished = [...latest.entries()]
      .sort((a, b) => b[1].at - a[1].at)
      .map(([id, f]) => {
        const card = stats!.shows[id];
        return {
          card,
          what: `${f.rewatch ? "Rewatched" : "Finished"} ${dayName(new Date(f.at), new Date(f.at).getFullYear() !== today.getFullYear())}`,
          sub: `${card.movie ? "Movie" : count(card.episodes, "episode")} · ${count(card.cards, "card")}`,
        };
      });

    return {
      byDay, range, since, tiles, grid, bars, summary, totalKnown, wGrid, wLine, wPts, meters, finished,
      gained: vals[N - 1] - earlier,
      wSummary: `From ${num(vals[0])} ${byDay ? "on" : "in"} ${buckets[0].when} to ${num(vals[N - 1])}`,
      wArea: `${wLine} L ${xAt(N - 1).toFixed(2)} 100 L ${xAt(0).toFixed(2)} 100 Z`,
      wFrom: buckets[0].when,
      wTo: buckets[N - 1].when,
    };
  });

  const readout = $derived(view ? (hover >= 0 && hover < view.bars.length ? view.bars[hover].text : view.summary) : "");
  const wOn = $derived(!!view && whover >= 0 && whover < view.wPts.length);
  const wReadout = $derived(view ? (wOn ? view.wPts[whover].text : view.wSummary) : "");
</script>

<div class="st scroll">
  <div class="st-in">
    <div class="st-top">
      <button type="button" class="back" onclick={() => goto("/")}><Icon name="back" /><span>Back to library</span></button>
      <div class="st-head">
        <div class="st-title">
          <h1>Stats</h1>
          {#if view}<p>{view.range}</p>{/if}
        </div>
        <div class="seg" role="group" aria-label="Period">
          {#each PERIODS as [p, label] (p)}
            <button type="button" aria-pressed={period === p} onclick={() => pick(p)}>{label}</button>
          {/each}
        </div>
      </div>
    </div>

    {#if error}<p class="err">{error}</p>{/if}

    {#if view}
      <StatTiles tiles={view.tiles} />

      <section aria-labelledby="h-time">
        <div class="sec-head">
          <h2 id="h-time">{view.byDay ? "Each day" : "Each month"}</h2>
          <p class="readout" aria-live="polite">{readout}</p>
        </div>
        <div class="chart">
          <div class="axis" aria-hidden="true" style:height="{H}px">
            {#each view.grid as g (g.y)}<span style:bottom="{g.y}px">{g.label}</span>{/each}
          </div>
          <div class="plot-col">
            <div class="plot" style:height="{H}px">
              {#each view.grid as g (g.y)}<span class="gl" aria-hidden="true" style:bottom="{g.y}px"></span>{/each}
              <div class="cols">
                {#each view.bars as b, i (b.from)}
                  <button type="button" class="hit bar-hit" aria-label={b.text} onmouseenter={() => (hover = i)} onfocus={() => (hover = i)} onmouseleave={() => (hover = -1)} onblur={() => (hover = -1)}>
                    <span class="bar" class:dim={hover !== -1 && hover !== i} style:height="{b.h}px"></span>
                  </button>
                {/each}
              </div>
            </div>
            <div class="xl" aria-hidden="true">
              {#each view.bars as b (b.from)}<span><span>{b.x}</span></span>{/each}
            </div>
          </div>
        </div>
      </section>

      <section aria-labelledby="h-words">
        <div class="sec-head">
          <h2 id="h-words">Words</h2>
          {#if view.totalKnown}<p class="readout" aria-live="polite">{wReadout}</p>{/if}
        </div>
        {#if !view.totalKnown}
          <p class="note">No known words yet. Telly reads them from Anki, as Settings → Cards and Anki → Known words says.</p>
        {:else}
          <div class="words">
            <div class="w-side">
              <div class="w-total">
                <p><span class="w-n">{num(view.totalKnown)}</span><span>words known</span></p>
                <span class="w-gain">+{num(view.gained)} {view.since}</span>
              </div>
              {#each view.meters as m (m.label)}
                <div class="meter">
                  <div class="m-text"><span>{m.label}</span><span class="m-pct">{m.pct}% known</span></div>
                  <span class="m-bar" aria-hidden="true"><i style:width="{m.pct}%"></i></span>
                </div>
              {/each}
            </div>
            <div class="chart w-chart">
              <div class="axis" aria-hidden="true" style:height="{WH}px">
                {#each view.wGrid as g (g.y)}<span style:bottom="{g.y}px">{g.label}</span>{/each}
              </div>
              <div class="plot-col">
                <div class="plot" style:height="{WH}px">
                  {#each view.wGrid as g (g.y)}<span class="gl" aria-hidden="true" style:bottom="{g.y}px"></span>{/each}
                  <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
                    <path class="w-area" d={view.wArea}></path>
                    <path class="w-line" d={view.wLine}></path>
                  </svg>
                  {#if wOn}
                    {@const p = view.wPts[whover]}
                    <span class="cross" aria-hidden="true" style:left="{p.x}%"></span>
                    <span class="dot" aria-hidden="true" style:left="{p.x}%" style:bottom="{p.y}px"></span>
                  {/if}
                  <div class="cols">
                    {#each view.wPts as p, i (i)}
                      <button type="button" class="hit" aria-label={p.text} onmouseenter={() => (whover = i)} onfocus={() => (whover = i)} onmouseleave={() => (whover = -1)} onblur={() => (whover = -1)}></button>
                    {/each}
                  </div>
                </div>
                <div class="w-ends" aria-hidden="true"><span>{view.wFrom}</span><span>{view.wTo}</span></div>
              </div>
            </div>
          </div>
        {/if}
      </section>

      <section aria-labelledby="h-fin">
        <div class="sec-head start">
          <h2 id="h-fin">Finished</h2>
          {#if view.finished.length}<span class="sec-count">{num(view.finished.length)}</span>{/if}
        </div>
        {#if view.finished.length}
          <div class="shelf">
            {#each view.finished as f (f.card.id)}
              <!-- A show removed from the library has no page of its own any more. -->
              <button type="button" class="fin" disabled={f.card.removed} onclick={() => goto(`/stats/show?id=${f.card.id}`)}>
                <span aria-hidden="true"><Cover cover={f.card.cover} color={f.card.coverColor} native={f.card.titleNative} title={f.card.title} caption={f.card.coverCaption} /></span>
                <span class="f-title" title={f.card.title}>{f.card.title}</span>
                <span class="f-what">{f.what}</span>
                <span class="f-sub">{f.sub}</span>
              </button>
            {/each}
          </div>
        {:else}
          <p class="note shelf-empty">Nothing finished {view.since || "yet"}.</p>
        {/if}
      </section>
    {/if}
  </div>
</div>

<style>
  .st { position: absolute; inset: 0; overflow: auto; padding: 22px 32px 56px; }
  .st-in { max-width: 1080px; margin: 0 auto; display: flex; flex-direction: column; gap: 36px; }
  .st-top { display: flex; flex-direction: column; gap: 14px; }
  .st-top .back { align-self: flex-start; }
  .st-head { display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px; }
  .st-title { display: flex; flex-direction: column; gap: 4px; }
  h1 { font: 600 30px/1.2 var(--font-display); letter-spacing: -.01em; }
  .st-title p { font-size: 13.5px; color: var(--ink-2); }
  section { display: flex; flex-direction: column; gap: 14px; }
  .sec-head { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: 6px 16px; }
  .sec-head.start { justify-content: flex-start; gap: 10px; }
  h2 { font: 600 17px/1.2 var(--font-ui); }
  .readout, .sec-count { font-size: 13px; color: var(--ink-2); }
  .note { font-size: 14px; color: var(--ink-2); }

  /* charts: hairline grid, labels in mono at the left, a hit area as wide as each column */
  .chart { display: flex; gap: 10px; }
  .axis { position: relative; flex: none; width: 40px; }
  .axis span { position: absolute; right: 0; transform: translateY(50%); font: 11px/1 var(--font-mono); color: var(--ink-2); white-space: nowrap; }
  .plot-col { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 8px; }
  .plot { position: relative; border-bottom: 1px solid var(--rule-2); }
  .gl { position: absolute; left: 0; right: 0; border-top: 1px solid var(--rule); }
  .cols { position: absolute; inset: 0; display: flex; }
  .hit { flex: 1 1 0; min-width: 0; height: 100%; padding: 0; border: 0; background: transparent; cursor: default; }
  .hit:focus-visible { outline-offset: -2px; }
  .bar-hit { display: flex; align-items: flex-end; justify-content: center; }
  .bar { display: block; width: min(24px, 70%); border-radius: 4px 4px 0 0; background: var(--ink); }
  .bar.dim { opacity: .2; }
  .xl { display: flex; height: 15px; }
  .xl > span { flex: 1 1 0; min-width: 0; position: relative; }
  .xl > span > span { position: absolute; left: 50%; top: 0; transform: translateX(-50%); white-space: nowrap; font-size: 11.5px; line-height: 1.2; color: var(--ink-2); }

  /* words */
  .words { display: flex; flex-wrap: wrap; align-items: flex-start; gap: 28px 48px; }
  .w-side { flex: 1 1 260px; min-width: 0; display: flex; flex-direction: column; gap: 18px; }
  .w-total { display: flex; flex-direction: column; gap: 4px; }
  .w-total p { display: flex; flex-wrap: wrap; align-items: baseline; gap: 10px; font-size: 15px; color: var(--ink-2); }
  .w-n { font: 600 40px/1.05 var(--font-display); letter-spacing: -.02em; color: var(--ink); }
  .w-gain { font-size: 12.5px; color: var(--ink-2); }
  .meter { display: flex; flex-direction: column; gap: 7px; }
  .m-text { display: flex; justify-content: space-between; gap: 12px; font-size: 13.5px; }
  .m-pct { font-weight: 600; font-variant-numeric: tabular-nums; }
  .m-bar { display: block; height: 8px; border-radius: 4px; overflow: hidden; background: var(--sunk); }
  .m-bar i { display: block; height: 100%; border-radius: 4px; background: var(--ink); }
  .w-chart { flex: 2 1 420px; min-width: 0; }
  .plot svg { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; }
  .w-area { fill: var(--ink); fill-opacity: .08; stroke: none; }
  .w-line { fill: none; stroke: var(--ink); stroke-width: 2px; stroke-linejoin: round; stroke-linecap: round; vector-effect: non-scaling-stroke; }
  .cross { position: absolute; top: 0; bottom: 0; border-left: 1px solid var(--rule-2); pointer-events: none; }
  .dot { position: absolute; width: 9px; height: 9px; margin: 0 0 -4.5px -4.5px; border-radius: 50%; background: var(--ink); box-shadow: 0 0 0 2px var(--bg); pointer-events: none; }
  .w-ends { display: flex; justify-content: space-between; gap: 12px; font-size: 11.5px; line-height: 1.2; color: var(--ink-2); }

  /* finished */
  .shelf { display: grid; grid-template-columns: repeat(auto-fill, minmax(128px, 1fr)); gap: 26px 20px; padding-top: 14px; border-top: 1px solid var(--rule); }
  .shelf-empty { padding-top: 14px; border-top: 1px solid var(--rule); }
  .fin { all: unset; box-sizing: border-box; display: flex; flex-direction: column; gap: 3px; min-width: 0; cursor: pointer; }
  .fin :global(.cv) { transition: transform .15s; }
  .fin:not(:disabled):hover :global(.cv) { transform: translateY(-2px); }
  .fin:disabled { cursor: default; }
  .fin:focus-visible { outline: 2px solid var(--ink); outline-offset: 4px; border-radius: 4px; }
  .f-title { margin-top: 9px; font: 600 13.5px/1.3 var(--font-ui); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .f-what { font-size: 12.5px; line-height: 1.3; color: var(--ink-2); }
  .f-sub { font-size: 12px; line-height: 1.3; color: var(--ink-2); }
</style>
