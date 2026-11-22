# dictionary-popup, in Telly

LFG's dictionary popup, which the phone's lookups are shown in (`src/lib/player/LookupPopup.svelte`). It's the package [bee-san/dictionary-popup](https://github.com/bee-san/dictionary-popup), copied at `67ab267` rather than installed, as that repository is private and Telly's builds on GitHub couldn't fetch it:

- `core.js` and `core.d.ts` here are its `src/core.js` and `src/index.d.ts`.
- `static/dictionary-popup/` is its `popup/` folder, the page the popup's iframe loads.

To update, copy those files again from a checkout of the package. Once the package is public, `npm install github:bee-san/dictionary-popup` and import `dictionary-popup/core` in their place.
