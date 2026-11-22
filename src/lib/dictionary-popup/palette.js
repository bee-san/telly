// @ts-nocheck: plain JavaScript; src/index.d.ts says what it exports.
// LFG's colours, for one colour scheme in light or dark (a port of the parts
// of its Palette.kt an entry draws from). Dark is true black, for AMOLED
// screens. The pitch-accent colours are not here: they mean something, so they
// stay the same in every scheme.
// SPDX-License-Identifier: GPL-3.0-or-later

const NAMES = ["bg", "surface", "border", "borderStrong", "text", "dim", "accent", "soft", "ok", "okSoft", "chip", "freqName", "freqNameText"];
const palette = (...colours) => Object.fromEntries(NAMES.map((name, i) => [name, colours[i]]));

/** PALETTES[theme][scheme]: theme "dark" or "light"; scheme "pink", "mono", "amber", "teal" or "purple". */
export const PALETTES = {
  dark: {
    pink: palette("#000000", "#0f0d0e", "#2e2a2c", "#4a4347", "#f5eff2", "#a39aa0", "#ff85b8", "#2a0e1b", "#5fd38d", "#0e2117", "#1c181a", "#3a1426", "#ffd1e6"),
    mono: palette("#000000", "#0c0c0d", "#2a2a2d", "#48484c", "#f2f2f2", "#9a9aa0", "#f2f2f2", "#1c1c1e", "#5fd38d", "#0e2117", "#1c1c1e", "#2a2a2d", "#f2f2f2"),
    amber: palette("#000000", "#0e0e0f", "#2a2a2e", "#48474a", "#f0eee9", "#9d9b96", "#f5a524", "#241906", "#4cc38a", "#0c2117", "#1a1a1c", "#0f3a35", "#c9f5ef"),
    teal: palette("#000000", "#0d0f10", "#263033", "#41504f", "#eef2f2", "#95a0a2", "#3dd6c6", "#072421", "#9be37a", "#13220c", "#16191a", "#3a2a0c", "#ffd79a"),
    // The colours LFG had before schemes: Hachidori's default theme.
    purple: palette("#22232c", "#2b2b36", "#454653", "#747586", "#e8e5eb", "#aaa8b2", "#b1a5ee", "#353247", "#8bb79b", "#2b3530", "#2f303b", "#347d35", "#ffffff"),
  },
  light: {
    pink: palette("#f8f2f5", "#ffffff", "#eadde3", "#c9b8c0", "#1f1a1d", "#675e63", "#c2185b", "#fde7f0", "#1d8048", "#e5f4ea", "#f6eef2", "#fcd9e8", "#7a1240"),
    mono: palette("#f2f2f2", "#ffffff", "#dedede", "#b8b8bc", "#111111", "#5c5c63", "#111111", "#f0f0f0", "#1d8048", "#e5f4ea", "#f0f0f0", "#e2e2e4", "#111111"),
    amber: palette("#f4f2ee", "#ffffff", "#e2dfd8", "#bdb8ae", "#1d1b16", "#66625a", "#a86300", "#fff3df", "#1d8048", "#e3f5ea", "#f3f1ec", "#d5f2ee", "#0b4f48"),
    teal: palette("#f0f4f4", "#ffffff", "#d9e1e2", "#b2bec0", "#121718", "#5b6567", "#0b7a70", "#e0f5f2", "#4a7f1c", "#ebf5e3", "#eef2f2", "#fbe7c4", "#5a3d00"),
    purple: palette("#f4f3f8", "#ffffff", "#dcdae6", "#b9b6ca", "#1e1d26", "#5f5d6b", "#5b4bc4", "#eeebfb", "#2a7a4b", "#e6f4ec", "#f0eff5", "#2f7d4f", "#ffffff"),
  },
};

const VARIABLES = {
  bg: "--bg", surface: "--surface", border: "--border", borderStrong: "--border-strong", text: "--text", dim: "--dim", accent: "--accent",
  soft: "--soft", ok: "--ok", okSoft: "--ok-soft", chip: "--chip", freqName: "--freq-name", freqNameText: "--freq-name-text",
};

/** The colours of `scheme` in `theme`, with any of them replaced by `colors`. */
export function paletteOf({ theme = "dark", scheme = "pink", colors = {} } = {}) {
  const dark = theme !== "light";
  const base = PALETTES[dark ? "dark" : "light"];
  return { dark, ...(base[scheme] ?? base.pink), ...colors };
}

/** A palette as the CSS variables the entry's styles read. A colour can't close the rule it's in. */
export function colours(p) {
  const safe = (value) => String(value).replace(/[;{}<>]/g, "");
  return `:host{color-scheme:${p.dark ? "dark" : "light"};` + Object.entries(VARIABLES).map(([name, variable]) => `${variable}:${safe(p[name])}`).join(";") + "}";
}
