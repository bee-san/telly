<script lang="ts">
  import { mediaUrl } from "./api";
  import { inkFor } from "./format";

  let {
    cover = "",
    color = "",
    native = "",
    title = "",
    caption = false,
  }: { cover?: string; color?: string; native?: string; title?: string; caption?: boolean } = $props();

  let failed = $state(false);
  // Telly's own copy of the cover, or one still on AniList (a recommendation's).
  const remote = $derived(cover.startsWith("https://"));
  const src = $derived(remote ? cover : mediaUrl(cover));
  // A stand-in colour from the title when AniList gave none.
  const fallback = $derived.by(() => {
    let h = 0;
    for (const c of title || native) h = (h * 31 + c.charCodeAt(0)) >>> 0;
    const hues = ["#1c2648", "#2b4a1e", "#4a2330", "#283b4f", "#3d3a2a", "#2f2a45", "#174046"];
    return hues[h % hues.length];
  });
  const bg = $derived(color || fallback);
</script>

<span class="cv" class:type={!cover || failed} class:shot={caption && !!cover && !failed} style:--cv-bg={bg} style:--cv-ink={inkFor(bg)}>
  {#if cover && !failed}
    <!-- Telly's own covers are small and on disk: all are loaded at once, so none is blank when scrolled to. -->
    <img {src} alt="" loading={remote ? "lazy" : "eager"} onerror={() => (failed = true)} />
    {#if caption}<span class="cv-en">{title}</span>{/if}
  {:else}
    {#if native}<span class="cv-jp" lang="ja">{native}</span>{/if}
    <span class="cv-en">{title}</span>
  {/if}
</span>
