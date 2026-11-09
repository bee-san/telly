// Tauri has no Node.js server for SSR, so SvelteKit runs as a single-page app
// (adapter-static with an index.html fallback).
export const ssr = false;
export const prerender = false;
