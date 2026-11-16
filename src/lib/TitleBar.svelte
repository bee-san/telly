<script lang="ts">
  import { goto } from "$app/navigation";
  import { tick } from "svelte";
  import { call, inTauri, type Queue, type Queued } from "./api";
  import Icon from "./Icon.svelte";
  import SearchBox from "./SearchBox.svelte";
  import { app } from "./state.svelte";

  const act = $derived(app.activity);
  /** A newer Telly that's out: said beside the window's buttons, but not over the player. */
  const newer = $derived(app.dark ? null : (act?.newer ?? null));

  async function openNewer() {
    if (!newer) return;
    try {
      if (inTauri) {
        const { openUrl } = await import("@tauri-apps/plugin-opener");
        await openUrl(newer.page);
      } else window.open(newer.page);
    } catch {
      goto("/settings#updates");
    }
  }
  const task = $derived(act?.tasks[0] ?? null);
  /** With no other work, the title bar shows the downloads, and goes to Processing. */
  const downloads = $derived(act?.downloading ?? null);
  /** Nothing else going on, but downloads wait in Processing: to be added, or stopped. */
  const toAdd = $derived(act?.downloadsToAdd ?? 0);
  const stopped = $derived(act?.downloadsStopped ?? 0);
  const onlyDownloads = $derived(!task && (!!downloads || toAdd > 0 || stopped > 0));
  const label = $derived.by(() => {
    if (onlyDownloads && downloads) return `Downloading ${downloads.count === 1 ? "1 release" : `${downloads.count} releases`} · ${Math.floor(downloads.progress * 100)}%`;
    if (onlyDownloads && stopped) return stopped === 1 ? "A download stopped" : `${stopped} downloads stopped`;
    if (onlyDownloads && toAdd) return toAdd === 1 ? "A download to add to the library" : `${toAdd} downloads to add to the library`;
    if (!act || !task) return "";
    if (act.total > 0 && act.done < act.total) {
      const what = act.shows > 1 ? `${act.shows} shows` : act.title;
      return `Getting ${what} ready · ${act.done} of ${act.total}`;
    }
    return task.label;
  });
  const progress = $derived.by(() => {
    if (onlyDownloads && downloads) return downloads.progress;
    if (onlyDownloads && !stopped) return 1;
    if (!act || !task) return 0;
    if (act.total > 0 && act.done < act.total) return act.done / act.total;
    return task.progress;
  });

  // Clicking it lists what each worker is doing and what's waiting, asked
  // for again every 1.5 s while the list is open, and only then.
  let opened = $state(false);
  let queue = $state<Queue | null>(null);
  let wrap = $state<HTMLElement>();
  let button = $state<HTMLButtonElement>();
  let menu = $state<HTMLElement>();
  const now = $derived(queue?.now ?? act?.tasks ?? []);

  async function load() {
    try {
      queue = await call<Queue>("queue");
    } catch {
      queue = null;
    }
  }

  async function open(focus: "first" | "last") {
    await load();
    opened = true;
    await tick();
    move(focus === "first" ? 0 : -1);
  }

  function close(refocus: boolean) {
    opened = false;
    if (refocus) button?.focus();
  }

  $effect(() => {
    if (!opened) return;
    const timer = setInterval(load, 1500);
    const outside = (e: PointerEvent) => {
      if (wrap && !wrap.contains(e.target as Node)) close(false);
    };
    window.addEventListener("pointerdown", outside);
    return () => {
      clearInterval(timer);
      window.removeEventListener("pointerdown", outside);
    };
  });

  // The work ended: nothing to list.
  $effect(() => {
    if (!label) opened = false;
  });

  function items(): HTMLElement[] {
    return menu ? [...menu.querySelectorAll<HTMLElement>('[role="menuitem"]')] : [];
  }

  /** Focuses the `to`th item, counting from the end when it's negative. */
  function move(to: number) {
    const all = items();
    if (all.length === 0) return menu?.focus();
    all[((to % all.length) + all.length) % all.length].focus();
  }

  function onButtonKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      open(e.key === "ArrowDown" ? "first" : "last");
    } else if (e.key === "Escape" && opened) {
      close(true);
    } else if (e.key !== "Enter" && e.key !== " ") {
      return;
    }
    // The player's own keys (Space plays) wait while the list has them.
    e.stopPropagation();
  }

  function onMenuKey(e: KeyboardEvent) {
    const at = items().indexOf(document.activeElement as HTMLElement);
    switch (e.key) {
      case "ArrowDown":
        move(at + 1);
        break;
      case "ArrowUp":
        move(at < 0 ? -1 : at - 1);
        break;
      case "Home":
        move(0);
        break;
      case "End":
        move(-1);
        break;
      case "Escape":
      case "Tab":
        close(true);
        break;
      default:
        e.stopPropagation();
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }

  function toShow(showId: number) {
    if (!showId) return;
    close(false);
    goto(`/show?id=${showId}`);
  }

  /** "Fairy Tail · Episode 12"; a film by its title. */
  const named = (q: Queued) => (q.episodes === 1 ? q.title : `${q.title} · Episode ${q.number}`);

  async function win(action: "minimize" | "toggleMaximize" | "close") {
    if (!inTauri) return;
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow()[action]();
  }
</script>

<header class="wt" class:dark={app.dark} data-tauri-drag-region>
  <span class="wt-left" data-tauri-drag-region>
  <svg class="wt-logo" viewBox="0 0 64 64" aria-hidden="true">
    <path d="M21 4.5 L32 14 L43 4.5" style="fill:none;stroke:var(--logo-bg)" stroke-width="3.6" stroke-linecap="round" stroke-linejoin="round" />
    <rect x="3" y="14" width="58" height="46" rx="12" style="fill:var(--logo-bg)" />
    <path d="M26.5 23.5 L41.5 32 L26.5 40.5 Z" style="fill:var(--logo-fg)" />
    <rect x="17" y="47.5" width="30" height="5" rx="2.5" style="fill:var(--mark)" />
  </svg>
  <span class="wt-name" data-tauri-drag-region>Telly</span>
  {#if label}
    <span class="wt-wrap" bind:this={wrap}>
      <button
        type="button"
        class="wt-act"
        class:on={opened}
        bind:this={button}
        aria-haspopup={onlyDownloads ? undefined : "menu"}
        aria-expanded={onlyDownloads ? undefined : opened}
        title={onlyDownloads ? "Open Processing" : undefined}
        onclick={() => (onlyDownloads ? goto("/processing") : opened ? close(false) : open("first"))}
        onkeydown={onButtonKey}
      >
        {#if !onlyDownloads || downloads}<span class="spin"></span>{/if}
        <span>{label}</span>
        <span class="wt-bar"><i style:width="{Math.round(progress * 100)}%"></i></span>
      </button>
      {#if opened}
        <div class="wt-menu" role="menu" aria-label="What Telly is doing" tabindex="-1" bind:this={menu} onkeydown={onMenuKey}>
          <div class="wq-h" role="presentation">Working on</div>
          {#each now as t, i (i)}
            <button type="button" role="menuitem" class="wq-now" aria-disabled={!t.showId} onclick={() => toShow(t.showId)}>
              <span class="wq-name">{t.label}</span>
              {#if t.progress > 0}
                <small>{Math.round(t.progress * 100)}%</small>
                <span class="wt-bar"><i style:width="{Math.round(t.progress * 100)}%"></i></span>
              {/if}
            </button>
          {:else}
            <div class="wq-more" role="presentation">Nothing at the moment</div>
          {/each}
          {#if queue && queue.waiting.length > 0}
            <div class="wq-h" role="presentation">Waiting</div>
            {#each queue.waiting as w (w.kind)}
              <div role="group" aria-label="{w.what}: {w.count} waiting">
                <div class="wq-kind" role="presentation"><span>{w.what}</span><span>{w.count}</span></div>
                {#each w.first as q (q.episodeId)}
                  <button type="button" role="menuitem" onclick={() => toShow(q.showId)}>
                    <span class="wq-name">{named(q)}</span>
                    {#if q.step}<small>{q.step}</small>{/if}
                  </button>
                {/each}
                {#if w.count > w.first.length}
                  <div class="wq-more" role="presentation">and {w.count - w.first.length} more</div>
                {/if}
              </div>
            {/each}
          {:else if queue}
            <div class="wq-h" role="presentation">Waiting</div>
            <div class="wq-more" role="presentation">Nothing else</div>
          {/if}
          <button type="button" role="menuitem" class="wq-all" onclick={() => { close(false); goto("/processing"); }}>
            <span class="wq-name">Open Processing</span><small>{downloads ? `${downloads.count} downloading` : "Downloads too"}</small>
          </button>
        </div>
      {/if}
    </span>
  {/if}
  </span>
  <SearchBox dark={app.dark} />
  <span class="wt-right" data-tauri-drag-region>
  {#if newer}
    <button type="button" class="wt-new" title="Open its page, to download it" onclick={openNewer}>Telly {newer.version} is out</button>
  {/if}
  {#if inTauri}
    <span class="wt-caps">
      <button type="button" aria-label="Minimise" title="Minimise" onclick={() => win("minimize")}><Icon name="min" /></button>
      <button type="button" aria-label="Maximise" title="Maximise" onclick={() => win("toggleMaximize")}><Icon name="max" /></button>
      <button type="button" class="close" aria-label="Close" title="Close" onclick={() => win("close")}><Icon name="close" /></button>
    </span>
  {/if}
  </span>
</header>

<style>
  /* The search sits in the middle, whatever is beside it. */
  .wt { position: relative; z-index: 70; height: 36px; flex: none; display: grid; grid-template-columns: minmax(0, 1fr) minmax(220px, 520px) minmax(0, 1fr); align-items: center; gap: 16px; padding-left: 12px; background: var(--bg-2); border-bottom: 1px solid var(--rule); user-select: none; -webkit-user-select: none; }
  .wt-left { display: flex; align-items: center; gap: 9px; min-width: 0; }
  .wt-right { display: flex; justify-content: flex-end; height: 100%; }
  .wt-logo { width: 16px; height: 16px; flex: none; }
  .wt-name { font: 400 12px/1 var(--font-ui); color: var(--ink-2); }
  .wt-wrap { position: relative; display: inline-flex; min-width: 0; margin-left: 12px; }
  .wt-act { display: inline-flex; align-items: center; gap: 9px; font: 400 12px/1 var(--font-ui); color: var(--ink-2); background: none; border: 0; padding: 5px 7px; border-radius: 4px; cursor: pointer; min-width: 0; }
  .wt-act span:nth-child(2) { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 420px; }
  .wt-act:hover, .wt-act.on { background: var(--sunk); }
  .wt-act .spin { width: 9px; height: 9px; border-width: 1.5px; }
  .wt-bar { display: block; width: 64px; height: 3px; border-radius: 2px; background: var(--sunk); overflow: hidden; flex: none; }
  .wt-bar i { display: block; height: 100%; background: var(--ink-2); transition: width .4s; }
  .wt-menu { position: absolute; top: calc(100% + 5px); left: 0; width: 380px; max-width: calc(100vw - 24px); max-height: calc(100vh - 60px); overflow-y: auto; display: grid; grid-template-columns: minmax(0, 1fr); padding: 5px; background: var(--surface); border: 1px solid var(--rule-2); border-radius: 8px; box-shadow: var(--shadow-lg); font: 400 13px/1.3 var(--font-ui); color: var(--ink); outline: none; }
  .wt-menu button { all: unset; display: flex; align-items: center; gap: 10px; padding: 6px 10px; border-radius: 5px; cursor: pointer; }
  .wt-menu button:hover, .wt-menu button:focus-visible { background: var(--sunk); }
  .wt-menu button[aria-disabled="true"] { cursor: default; }
  .wt-menu small { flex: none; font-size: 12px; color: var(--ink-3); }
  .wt-menu .wt-bar { width: 48px; }
  .wt-menu [role="group"] { display: grid; grid-template-columns: minmax(0, 1fr); }
  .wq-name { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .wq-h { padding: 9px 10px 3px; font-size: 11.5px; font-weight: 600; color: var(--ink-3); }
  .wq-kind { display: flex; justify-content: space-between; gap: 10px; padding: 6px 10px 2px; font-size: 12.5px; font-weight: 600; color: var(--ink-2); }
  .wq-kind span:first-child { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .wq-more { padding: 3px 10px 6px; font-size: 12px; color: var(--ink-3); }
  .wt-menu .wq-all { margin-top: 4px; border-top: 1px solid var(--rule); border-radius: 0 0 5px 5px; font-weight: 600; }
  .wt-new { align-self: center; margin-right: 8px; font: 500 12px/1 var(--font-ui); color: var(--ink); background: var(--sunk); border: 0; padding: 5px 9px; border-radius: 4px; cursor: pointer; white-space: nowrap; }
  .wt-new:hover { background: var(--rule-2); }
  .wt-caps { display: flex; height: 100%; }
  .wt-caps button { width: 46px; height: 100%; display: grid; place-items: center; color: var(--ink-2); background: none; border: 0; cursor: default; }
  .wt-caps button:hover { background: var(--sunk); color: var(--ink); }
  .wt-caps button.close:hover { background: #c42b1c; color: #fff; }
  .wt-caps :global(svg) { width: 14px; height: 14px; }
  .wt.dark { background: var(--vid-bar); border-bottom-color: var(--vid-edge); }
  .wt.dark .wt-name, .wt.dark .wt-caps button, .wt.dark .wt-act { color: var(--vid-ink-2); }
  .wt.dark .wt-act:hover, .wt.dark .wt-act.on, .wt.dark .wt-caps button:hover { background: var(--vid-hover); color: var(--vid-ink); }
  .wt.dark .wt-bar { background: var(--vid-hover); }
  .wt.dark .wt-bar i { background: var(--vid-ink); }
  .wt.dark .wt-menu { background: var(--vid-panel); border-color: var(--vid-edge); color: var(--vid-ink); }
  .wt.dark .wt-menu button:hover, .wt.dark .wt-menu button:focus-visible { background: var(--vid-hover); }
  .wt.dark .wq-h, .wt.dark .wq-more, .wt.dark .wt-menu small { color: var(--vid-ink-2); }
  .wt.dark .wq-kind { color: var(--vid-ink); }
  .wt.dark .wt-menu .wq-all { border-top-color: var(--vid-edge); }
  .wt.dark .wt-logo { --logo-bg: #ededeb; --logo-fg: #151516; }
  .wt.dark .spin { border-color: rgba(255, 255, 255, .2); border-top-color: var(--vid-ink); }
</style>
