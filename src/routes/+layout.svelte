<script lang="ts">
  import "@fontsource/zen-kaku-gothic-new/500.css";
  import "@fontsource/zen-kaku-gothic-new/700.css";
  import "@fontsource/shippori-mincho-b1/600.css";
  import "@fontsource/shippori-mincho-b1/800.css";
  import "@fontsource/ibm-plex-mono/400.css";
  import "@fontsource/ibm-plex-mono/500.css";
  import "../app.css";
  import { onMount, type Snippet } from "svelte";
  import TitleBar from "$lib/TitleBar.svelte";
  import Toasts from "$lib/Toasts.svelte";
  import { hachidoriStart } from "$lib/api";
  import { onPhone } from "$lib/phone";
  import { app, watchActivity } from "$lib/state.svelte";

  let { children }: { children: Snippet } = $props();

  onMount(() => {
    // The phone has no background work to show, and no Hachidori: its backend looks words up.
    if (onPhone) {
      // An older Telly kept your dictionaries in the page's storage: that copy goes.
      try {
        indexedDB.deleteDatabase("/dicts");
      } catch {
        // Nothing was kept there.
      }
      return;
    }
    const stop = watchActivity();
    // WebView2 has loaded Hachidori by the time the page is up: start its engine.
    hachidoriStart().catch(() => {});
    return stop;
  });
</script>

<div class="shell" class:dark={app.dark}>
  {#if !app.fullscreen && !app.compact && !onPhone}<TitleBar />{/if}
  <main class="body">
    {@render children()}
  </main>
  <Toasts />
</div>

<style>
  .shell { position: fixed; inset: 0; display: flex; flex-direction: column; background: var(--bg); }
  .shell.dark { background: var(--vid); }
  .body { position: relative; flex: 1; min-height: 0; overflow: hidden; }
</style>
