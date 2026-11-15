<script lang="ts">
  import { onMount } from "svelte";
  import { pickFolder, type DownloadLibrary } from "./api";
  import Icon from "./Icon.svelte";

  /** `library` is the one being edited, or none to add one. */
  let { library, onclose, onsave }: { library: DownloadLibrary | null; onclose: () => void; onsave: (library: DownloadLibrary) => void } = $props();

  let name = $state(library?.name ?? "");
  let kind = $state<DownloadLibrary["kind"]>(library?.kind ?? "series");
  let folders = $state<string[]>(library ? [...library.folders] : []);
  let downloads = $state(library?.downloads ?? "");

  async function addFolder() {
    const folder = await pickFolder("Choose a folder for the library");
    if (!folder || folders.includes(folder)) return;
    folders.push(folder);
    downloads ||= folder;
  }

  function removeFolder(folder: string) {
    folders = folders.filter((f) => f !== folder);
    if (downloads === folder) downloads = folders[0] ?? "";
  }

  function save(e: SubmitEvent) {
    e.preventDefault();
    onsave({ name: name.trim(), kind, folders, downloads });
  }

  onMount(() => {
    const key = (e: KeyboardEvent) => e.key === "Escape" && onclose();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  });
</script>

<div class="modal-bg" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <form class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="library-title" onsubmit={save}>
    <h2 id="library-title">{library ? "Edit library" : "Add library"}</h2>
    <label class="field">
      <span>Name</span>
      <input class="input" bind:value={name} placeholder="Series" />
    </label>
    <fieldset class="field">
      <legend>Kind</legend>
      <span class="choices">
        <label class="choice"><input type="radio" name="kind" value="series" bind:group={kind} /><span>Series</span></label>
        <label class="choice"><input type="radio" name="kind" value="movies" bind:group={kind} /><span>Movies</span></label>
      </span>
    </fieldset>
    <fieldset class="field">
      <legend>Folders</legend>
      {#if folders.length}
        <ul class="folders">
          {#each folders as folder (folder)}
            <li>
              <span class="mono">{folder}</span>
              {#if folders.length > 1}<label class="choice"><input type="radio" name="downloads" value={folder} bind:group={downloads} /><span>Downloads go here</span></label>{/if}
              <button type="button" class="btn quiet small" onclick={() => removeFolder(folder)}>Remove</button>
            </li>
          {/each}
        </ul>
      {/if}
      <span><button type="button" class="btn quiet" onclick={addFolder}><Icon name="folder" /><span>Add folder…</span></button></span>
    </fieldset>
    <div class="sheet-foot">
      <button type="button" class="btn quiet" onclick={onclose}>Cancel</button>
      <button type="submit" class="btn" disabled={!name.trim() || !folders.length}>{library ? "Save" : "Add"}</button>
    </div>
  </form>
</div>

<style>
  .narrow { width: min(600px, 100%); }
  h2 { font: 600 18px/1.2 var(--font-display); }
  fieldset { margin: 0; padding: 0; border: 0; min-width: 0; }
  legend { padding: 0 0 6px; font: 600 12.5px/1.2 var(--font-ui); color: var(--ink-2); }
  .choices { display: flex; gap: 22px; }
  .choice { display: inline-flex; align-items: center; gap: 8px; font-size: 13.5px; cursor: pointer; white-space: nowrap; }
  .choice input { margin: 0; accent-color: var(--ink); }
  .folders { display: grid; margin: 0; padding: 0; border: 1px solid var(--rule); border-radius: 7px; }
  .folders li { list-style: none; display: grid; grid-template-columns: minmax(0, 1fr) auto auto; gap: 12px; align-items: center; padding: 8px 10px; }
  .folders li + li { border-top: 1px solid var(--rule); }
  .folders .mono { overflow-wrap: anywhere; }
</style>
