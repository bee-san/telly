# dictionary-popup, in Telly

Hachidori's dictionary popup, which the phone's lookups are shown in (`src/lib/player/LookupPopup.svelte`), so a word looks on the phone as it does on the PC. It's the package [bee-san/dictionary-popup](https://github.com/bee-san/dictionary-popup), which carries the popup's own code and stylesheet from [Hachidori](https://github.com/bee-san/hachidori) (`hachidori/`, at the commit `hachidori/source.json` names) and the little a page needs to use them without the extension. The files here are that package's `src/`, copied rather than installed: npm 12 refuses a package from git unless the project allows it.

Change the popup in Hachidori, bring it into the package with its `scripts/sync-from-hachidori.mjs`, and copy the package's `src/` here again. Nothing in this folder is edited in Telly.
