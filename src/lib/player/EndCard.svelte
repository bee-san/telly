<script lang="ts">
  /** The card at the end of an episode: what you watched, what's next and its countdown. The card it's in is styled by the player's page. */
  import type { AfterCredits } from "$lib/ending";
  import type { EpisodeLite, PlayerView } from "$lib/api";
  import Icon from "$lib/Icon.svelte";

  let {
    view,
    sat,
    countdown,
    more,
    onlibrary,
    onkeepwatching,
    onstop,
    onplaynext,
  }: {
    view: PlayerView;
    /** What this sitting came to. */
    sat: { lines: number; chars: number; cards: number; seconds: number };
    /** Seconds until the next episode starts; null when it isn't counting down. */
    countdown: number | null;
    /** What's after the credits, when the card opened before it. */
    more: AfterCredits | null;
    onlibrary: () => void;
    onkeepwatching: () => void;
    onstop: () => void;
    onplaynext: () => void;
  } = $props();

  /** What's next, by name: "Episode 5", or the next season's first ("Overlord II · Episode 1") after a show's last. */
  function nextName(next: EpisodeLite): string {
    if (!next.show) return `Episode ${next.number}`;
    return next.movie ? next.show : `${next.show} · Episode ${next.number}`;
  }
</script>

<div class="endov">
  <div class="end-card">
    <span class="small">{view.movie ? view.show.title : `Episode ${view.episode.number}`} done{sat.seconds > 30 ? ` · logged to Tidepool` : ""}</span>
    <h3>{view.next ? `Next: ${nextName(view.next)}` : `That’s the last episode here`}</h3>
    <div class="end-stats">
      <div><b>{Math.round(sat.seconds / 60)}</b><span>minutes</span></div>
      <div><b>{sat.lines}</b><span>lines</span></div>
      <div><b>{sat.chars}</b><span>characters</span></div>
      <div><b>{sat.cards}</b><span>cards</span></div>
    </div>
    {#if view.next && !view.next.ready}
      <p class="small">{nextName(view.next)} is getting ready{view.next.mediaState === "working" ? ` · ${Math.round(view.next.mediaProgress * 100)}%` : ""}. It’ll start when it can.</p>
    {/if}
    <div class="end-actions">
      <button type="button" class="btn quiet" onclick={onlibrary}>Open library</button>
      {#if more}
        <button type="button" class="btn quiet" onclick={onkeepwatching}>{more.preview ? "Watch the preview" : "Watch the scene after the credits"}</button>
      {/if}
      {#if view.next}
        <button type="button" class="btn quiet" onclick={onstop} disabled={countdown === null}>Stop</button>
        <button type="button" class="btn" onclick={onplaynext} disabled={!view.next.ready}><Icon name="play" /><span>Play now</span></button>
      {/if}
    </div>
    {#if countdown !== null}
      <span class="cd" aria-label="Next episode in {countdown} seconds">
        <svg viewBox="0 0 44 44"><circle class="cd-bg" cx="22" cy="22" r="18" /><circle class="cd-fg" cx="22" cy="22" r="18" /></svg>
        <b>{countdown}</b>
      </span>
    {/if}
  </div>
</div>

<style>
  .end-stats { display: grid; grid-template-columns: repeat(4, 1fr); border-top: 1px solid var(--vid-edge); border-bottom: 1px solid var(--vid-edge); }
  .end-stats div { display: grid; gap: 2px; padding: 12px 0; }
  .end-stats b { font: 600 20px/1.1 var(--font-ui); font-variant-numeric: tabular-nums; }
  .end-stats span { font-size: 12px; color: var(--vid-ink-2); }
  .cd { position: absolute; top: 22px; right: 24px; width: 44px; height: 44px; }
  .cd svg { width: 44px; height: 44px; transform: rotate(-90deg); }
  .cd circle { fill: none; stroke-width: 3; }
  .cd .cd-bg { stroke: rgba(255, 255, 255, .14); }
  .cd .cd-fg { stroke: var(--vid-ink); stroke-linecap: round; stroke-dasharray: 113.1; animation: cd 8s linear forwards; }
  @keyframes cd { to { stroke-dashoffset: 113.1; } }
  .cd b { position: absolute; inset: 0; display: grid; place-items: center; font: 600 15px/1 var(--font-ui); font-variant-numeric: tabular-nums; }
  /* In the small player. */
  :global(.player.compact) .end-stats div { padding: 6px 0; }
  :global(.player.compact) .end-stats b { font-size: 16px; }
  :global(.player.compact) .cd { top: 10px; right: 14px; }
</style>
