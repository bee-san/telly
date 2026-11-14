// Small line icons, drawn on a 24-unit grid with the current colour.
const s = (body: string) =>
  `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;

export const icons = {
  play: `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7.5 4.8v14.4a.8.8 0 0 0 1.2.7l11.3-7.2a.8.8 0 0 0 0-1.4L8.7 4.1a.8.8 0 0 0-1.2.7z" fill="currentColor"/></svg>`,
  pause: `<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="4.5" width="4.2" height="15" rx="1" fill="currentColor"/><rect x="13.8" y="4.5" width="4.2" height="15" rx="1" fill="currentColor"/></svg>`,
  plus: s(`<path d="M12 5v14M5 12h14"/>`),
  search: s(`<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/>`),
  back: s(`<path d="M15 5l-7 7 7 7"/>`),
  check: s(`<path d="M5 12.5l4.5 4.5L19 7.5"/>`),
  x: s(`<path d="M6 6l12 12M18 6L6 18"/>`),
  gear: s(`<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>`),
  prevLine: s(`<path d="M7 5v14M18 6l-8 6 8 6z"/>`),
  nextLine: s(`<path d="M17 5v14M6 6l8 6-8 6z"/>`),
  replay: s(`<path d="M4 12a8 8 0 1 0 2.4-5.7L4 8.7"/><path d="M4 4v4.7h4.7"/>`),
  subs: s(`<rect x="3" y="5" width="18" height="14" rx="2.5"/><path d="M7 14.5h4M13 14.5h4M7 11h10"/>`),
  blur: s(`<path d="M4 12h3M17 12h3M12 4v3M12 17v3"/><circle cx="12" cy="12" r="3.2"/>`),
  transcript: s(`<path d="M5 6h14M5 10h14M5 14h9M5 18h6"/>`),
  volume: s(`<path d="M4 9.5v5h3.5l4.5 4v-13l-4.5 4z"/><path d="M15.5 9a4.2 4.2 0 0 1 0 6M18.2 6.5a7.8 7.8 0 0 1 0 11"/>`),
  muted: s(`<path d="M4 9.5v5h3.5l4.5 4v-13l-4.5 4z"/><path d="M16 9.5l5 5M21 9.5l-5 5"/>`),
  full: s(`<path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/>`),
  exitFull: s(`<path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5"/>`),
  mini: s(`<rect x="3" y="5" width="18" height="14" rx="2"/><rect x="12" y="11.5" width="6.5" height="5" rx="1" fill="currentColor" stroke="none"/>`),
  more: `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="5.5" cy="12" r="1.7" fill="currentColor"/><circle cx="12" cy="12" r="1.7" fill="currentColor"/><circle cx="18.5" cy="12" r="1.7" fill="currentColor"/></svg>`,
  grip: `<svg viewBox="0 0 24 24" aria-hidden="true"><g fill="currentColor"><circle cx="9" cy="6" r="1.5"/><circle cx="15" cy="6" r="1.5"/><circle cx="9" cy="12" r="1.5"/><circle cx="15" cy="12" r="1.5"/><circle cx="9" cy="18" r="1.5"/><circle cx="15" cy="18" r="1.5"/></g></svg>`,
  warn: s(`<path d="M12 4 2.8 19.5h18.4z"/><path d="M12 10v4.2M12 17h.01"/>`),
  card: s(`<rect x="4" y="3.5" width="16" height="17" rx="2"/><path d="M8 8.5h8M8 12h8M8 15.5h5"/>`),
  folder: s(`<path d="M3.5 7.5a2 2 0 0 1 2-2h4l2 2h7a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/>`),
  speed: s(`<path d="M12 13l4-4"/><path d="M4.5 18a9 9 0 1 1 15 0"/>`),
  min: s(`<path d="M5 12h14"/>`),
  max: s(`<rect x="5" y="5" width="14" height="14" rx="1.5"/>`),
  close: s(`<path d="M6 6l12 12M18 6L6 18"/>`),
};

export type IconName = keyof typeof icons;
