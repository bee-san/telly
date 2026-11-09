// Toasts, not dialogs: "Adding picture and audio", then "Card ready".
export interface Toast {
  key: string;
  state: "working" | "done" | "failed" | "info";
  title: string;
  text: string;
  /** Japanese text gets Japanese type. */
  ja?: boolean;
  action?: { label: string; run: () => void };
}

export const toasts = $state<{ list: Toast[] }>({ list: [] });
const timers = new Map<string, ReturnType<typeof setTimeout>>();

export function toast(t: Toast, ttl = t.state === "working" ? 0 : t.state === "failed" ? 12000 : 4500): void {
  const i = toasts.list.findIndex((x) => x.key === t.key);
  if (i >= 0) toasts.list[i] = t;
  else toasts.list = [t, ...toasts.list].slice(0, 5);
  clearTimeout(timers.get(t.key));
  if (ttl > 0) timers.set(t.key, setTimeout(() => dismiss(t.key), ttl));
}

export function dismiss(key: string): void {
  clearTimeout(timers.get(key));
  timers.delete(key);
  toasts.list = toasts.list.filter((t) => t.key !== key);
}
