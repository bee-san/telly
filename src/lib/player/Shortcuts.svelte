<script lang="ts">
  /** The list of the player's keys that ? shows. The card it's in is styled by the player's page. */
  let { onclose }: { onclose: () => void } = $props();

  const SHORTCUTS: [string, string][] = [
  ["Space or K", "Play or pause"],
  ["← and →", "Previous and next line"],
  ["↓", "Replay the line"],
  ["- and =", "Volume down and up"],
  ["0", "Mute"],
  ["D", "Boost dialogue"],
  ["[ and ]", "Slower and faster"],
  ["S", "Show subtitles"],
  ["B", "Blur subtitles until hovered"],
  ["Hold Tab", "Show the rip’s English subtitles"],
  ["Z and X", "Subtitles earlier and later"],
  ["T", "Show transcript"],
  ["V", "Show filters"],
  ["M", "Add the line to the last card"],
  ["E", "Explain the line"],
  ["P", "Show Previously on"],
  ["F", "Fullscreen"],
  ["I", "Small player, on top of other windows"],
  ["Esc", "Close, or leave fullscreen or the small player"],
  ["?", "Show shortcuts"],
  ];
</script>

<div class="endov" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="end-card keys-card" role="dialog" aria-label="Shortcuts">
    <h3>Shortcuts</h3>
    <dl class="keys">
      {#each SHORTCUTS as [key, what] (key)}
        <dt>{key}</dt>
        <dd>{what}</dd>
      {/each}
    </dl>
    <div class="end-actions"><button type="button" class="btn quiet" onclick={onclose}>Close</button></div>
  </div>
</div>

<style>
  .keys-card { max-height: 88vh; overflow: auto; }
  .keys { margin: 0; display: grid; grid-template-columns: auto 1fr; gap: 7px 22px; font-size: 14px; }
  .keys dt { font: 600 13px/1.5 var(--font-mono); white-space: nowrap; }
  .keys dd { margin: 0; color: var(--vid-ink-2); }
</style>
