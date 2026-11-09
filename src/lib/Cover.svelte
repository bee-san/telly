<script lang="ts">
  import { mediaUrl } from "./api";
  import { inkFor } from "./format";

  let {
    cover = "",
    color = "",
    native = "",
    title = "",
  }: { cover?: string; color?: string; native?: string; title?: string } = $props();

  let failed = $state(false);
  // A stand-in colour from the title when AniList gave none.
  const fallback = $derived.by(() => {
    let h = 0;
    for (const c of title || native) h = (h * 31 + c.charCodeAt(0)) >>> 0;
    const hues = ["#1c2648", "#2b4a1e", "#4a2330", "#283b4f", "#3d3a2a", "#2f2a45", "#174046"];
    return hues[h % hues.length];
  });
  const bg = $derived(color || fallback);
</script>

<span class="cv" class:type={!cover || failed} style:--cv-bg={bg} style:--cv-ink={inkFor(bg)}>
  {#if cover && !failed}
    <img src={mediaUrl(cover)} alt="" loading="lazy" onerror={() => (failed = true)} />
  {:else}
    {#if native}<span class="cv-jp" lang="ja">{native}</span>{/if}
    <span class="cv-en">{title}</span>
  {/if}
</span>
