# dictionary-popup, in Telly

LFG's dictionary popup, which the phone's lookups are shown in (`src/lib/player/LookupPopup.svelte`). It's the package [bee-san/dictionary-popup](https://github.com/bee-san/dictionary-popup), a port of the popup in LFG ([bee-san/LearnFromGames](https://github.com/bee-san/LearnFromGames)). The files here are that package's `src/` at `81f92f3`, copied rather than installed, as its repository is private and Telly's builds on GitHub couldn't fetch it.

Change the popup in that package, not here, and copy its `src/` again. Once the package is public, `npm install github:bee-san/dictionary-popup` and import `dictionary-popup` in place of this folder.
