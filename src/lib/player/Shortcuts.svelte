<script lang="ts">
  /** The list of the player's keys that ? shows. The card it's in is styled by the player's page. */
  import { onPhone } from "$lib/phone";

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

  /** A controller's buttons, on Telly for Android: watching, then with a word looked up. */
  const BUTTONS: [string, string][] = [
    ["A", "Play or pause"],
    ["D-pad ↑ and ↓", "Previous and next line"],
    ["Start, L3 or D-pad →", "Look up the line’s first word"],
    ["D-pad ←", "Look up the line’s last word"],
    ["L1 and R1, or the left stick", "Back and on 10 seconds"],
    ["Y", "Replay the line"],
    ["X", "Skip the opening or outro when offered, else explain the line"],
    ["Start, held", "Move around the player’s controls"],
    ["Select", "Show the buttons"],
    ["B", "Close what’s open, or leave the episode"],
  ];
  const LOOKING: [string, string][] = [
    ["Left stick ← and →", "Previous and next word"],
    ["Left stick or D-pad ↑ and ↓", "Previous and next line"],
    ["D-pad ← and →", "One character back or on"],
    ["L1 and R1", "Previous and next entry"],
    ["Right stick", "Scroll the definitions"],
    ["A", "Add the entry’s card"],
    ["Y", "Hear the line"],
    ["B, Start or L3", "Close, and carry on"],
  ];
</script>

<div class="endov" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="end-card keys-card" role="dialog" aria-label={onPhone ? "Buttons" : "Shortcuts"}>
    <h3>{onPhone ? "A controller’s buttons" : "Shortcuts"}</h3>
    <dl class="keys">
      {#each onPhone ? BUTTONS : SHORTCUTS as [key, what] (key)}
        <dt>{key}</dt>
        <dd>{what}</dd>
      {/each}
    </dl>
    {#if onPhone}
      <h4>With a word looked up</h4>
      <dl class="keys">
        {#each LOOKING as [key, what] (key)}
          <dt>{key}</dt>
          <dd>{what}</dd>
        {/each}
      </dl>
    {/if}
    <div class="end-actions"><button type="button" class="btn quiet" onclick={onclose}>Close</button></div>
  </div>
</div>

<style>
  .keys-card { max-height: 88vh; overflow: auto; }
  .keys { margin: 0; display: grid; grid-template-columns: auto 1fr; gap: 7px 22px; font-size: 14px; }
  .keys dt { font: 600 13px/1.5 var(--font-mono); white-space: nowrap; }
  .keys dd { margin: 0; color: var(--vid-ink-2); }
  h4 { margin: 4px 0 0; font: 600 13px/1.4 var(--font-ui); color: var(--vid-ink-2); }
</style>
