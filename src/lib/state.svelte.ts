// What every page shares: the background work in the title bar, and whether
// the player (dark chrome) is up, full screen or the small player.
import { call, type Activity } from "./api";

export const app = $state<{ activity: Activity | null; dark: boolean; fullscreen: boolean; compact: boolean }>({
  activity: null,
  dark: false,
  fullscreen: false,
  compact: false,
});

let timer: ReturnType<typeof setTimeout> | null = null;

export async function refreshActivity(): Promise<void> {
  try {
    app.activity = await call<Activity>("activity");
  } catch {
    app.activity = null;
  }
}

/** Keeps the title bar's activity current: often while work runs, rarely when idle. */
export function watchActivity(): () => void {
  let stopped = false;
  const loop = async () => {
    if (stopped) return;
    await refreshActivity();
    const busy = (app.activity?.tasks.length ?? 0) > 0 || !!app.activity?.downloading;
    timer = setTimeout(loop, busy ? 1200 : 4000);
  };
  loop();
  return () => {
    stopped = true;
    if (timer) clearTimeout(timer);
  };
}
