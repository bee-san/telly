<script lang="ts">
  import Icon from "./Icon.svelte";
  import { dismiss, toasts } from "./toast-store.svelte";
  import { app } from "./state.svelte";
</script>

<div class="toasts" class:over-player={app.dark} aria-live="polite">
  {#each toasts.list as t (t.key)}
    <div class="toast {t.state}">
      <span class="t-ic">
        {#if t.state === "working"}<span class="spin"></span>
        {:else if t.state === "done"}<Icon name="check" />
        {:else if t.state === "failed"}<Icon name="warn" />
        {:else}<Icon name="card" />{/if}
      </span>
      <span class="t-b">
        <b>{t.title}</b>
        {#if t.text}<span class="t-txt" lang={t.ja ? "ja" : undefined}>{t.text}</span>{/if}
        {#if t.action}
          <span class="t-act"><button type="button" class="btn small quiet" onclick={() => { t.action?.run(); dismiss(t.key); }}>{t.action.label}</button></span>
        {/if}
      </span>
      <button type="button" class="t-x" aria-label="Dismiss" onclick={() => dismiss(t.key)}><Icon name="x" /></button>
    </div>
  {/each}
</div>
