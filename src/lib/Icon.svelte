<script lang="ts">
  import { icons, type IconName } from "./icons";
  let { name }: { name: IconName } = $props();
</script>

{@html icons[name]}
