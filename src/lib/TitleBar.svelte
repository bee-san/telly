<script lang="ts">
  import { goto } from "$app/navigation";
  import { inTauri } from "./api";
  import Icon from "./Icon.svelte";
  import { app } from "./state.svelte";

  const act = $derived(app.activity);
  const task = $derived(act?.tasks[0] ?? null);
  const label = $derived.by(() => {
    if (!act || !task) return "";
    if (act.total > 0 && act.done < act.total) {
      const what = act.shows > 1 ? `${act.shows} shows` : act.title;
      return `Getting ${what} ready · ${act.done} of ${act.total}`;
    }
    return task.label;
  });
  // Several episodes are worked on at once: the tooltip says what each is at.
  const doing = $derived(act?.tasks.map((t) => t.label).join("\n") ?? "");
  const progress = $derived.by(() => {
    if (!act || !task) return 0;
    if (act.total > 0 && act.done < act.total) return act.done / act.total;
    return task.progress;
  });

  async function win(action: "minimize" | "toggleMaximize" | "close") {
    if (!inTauri) return;
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow()[action]();
  }
</script>

<header class="wt" class:dark={app.dark} data-tauri-drag-region>
  <svg class="wt-logo" viewBox="0 0 64 64" aria-hidden="true">
    <path d="M21 4.5 L32 14 L43 4.5" style="fill:none;stroke:var(--logo-bg)" stroke-width="3.6" stroke-linecap="round" stroke-linejoin="round" />
    <rect x="3" y="14" width="58" height="46" rx="12" style="fill:var(--logo-bg)" />
    <path d="M26.5 23.5 L41.5 32 L26.5 40.5 Z" style="fill:var(--logo-fg)" />
    <rect x="17" y="47.5" width="30" height="5" rx="2.5" style="fill:var(--mark)" />
  </svg>
  <span class="wt-name" data-tauri-drag-region>Telly</span>
  {#if label}
    <button type="button" class="wt-act" onclick={() => act?.showId && goto(`/show?id=${act.showId}`)} title={doing}>
      <span class="spin"></span>
      <span>{label}</span>
      <span class="wt-bar"><i style:width="{Math.round(progress * 100)}%"></i></span>
    </button>
  {/if}
  <span class="grow" data-tauri-drag-region></span>
  {#if inTauri}
    <span class="wt-caps">
      <button type="button" aria-label="Minimise" title="Minimise" onclick={() => win("minimize")}><Icon name="min" /></button>
      <button type="button" aria-label="Maximise" title="Maximise" onclick={() => win("toggleMaximize")}><Icon name="max" /></button>
      <button type="button" class="close" aria-label="Close" title="Close" onclick={() => win("close")}><Icon name="close" /></button>
    </span>
  {/if}
</header>

<style>
  .wt { position: relative; z-index: 70; height: 36px; flex: none; display: flex; align-items: center; gap: 9px; padding-left: 12px; background: var(--bg-2); border-bottom: 1px solid var(--rule); user-select: none; -webkit-user-select: none; }
  .wt-logo { width: 16px; height: 16px; flex: none; }
  .wt-name { font: 400 12px/1 var(--font-ui); color: var(--ink-2); }
  .wt-act { display: inline-flex; align-items: center; gap: 9px; margin-left: 12px; font: 400 12px/1 var(--font-ui); color: var(--ink-2); background: none; border: 0; padding: 5px 7px; border-radius: 4px; cursor: pointer; min-width: 0; }
  .wt-act span:nth-child(2) { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 420px; }
  .wt-act:hover { background: var(--sunk); }
  .wt-act .spin { width: 9px; height: 9px; border-width: 1.5px; }
  .wt-bar { display: block; width: 64px; height: 3px; border-radius: 2px; background: var(--sunk); overflow: hidden; flex: none; }
  .wt-bar i { display: block; height: 100%; background: var(--ink-2); transition: width .4s; }
  .wt-caps { display: flex; height: 100%; }
  .wt-caps button { width: 46px; height: 100%; display: grid; place-items: center; color: var(--ink-2); background: none; border: 0; cursor: default; }
  .wt-caps button:hover { background: var(--sunk); color: var(--ink); }
  .wt-caps button.close:hover { background: #c42b1c; color: #fff; }
  .wt-caps :global(svg) { width: 14px; height: 14px; }
  .wt.dark { background: var(--vid-bar); border-bottom-color: var(--vid-edge); }
  .wt.dark .wt-name, .wt.dark .wt-caps button, .wt.dark .wt-act { color: var(--vid-ink-2); }
  .wt.dark .wt-act:hover, .wt.dark .wt-caps button:hover { background: var(--vid-hover); color: var(--vid-ink); }
  .wt.dark .wt-bar { background: var(--vid-hover); }
  .wt.dark .wt-bar i { background: var(--vid-ink); }
  .wt.dark .wt-logo { --logo-bg: #ededeb; --logo-fg: #151516; }
  .wt.dark .spin { border-color: rgba(255, 255, 255, .2); border-top-color: var(--vid-ink); }
</style>
