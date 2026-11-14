// The small player: Telly's window shrunk to the picture, without its border
// and on top of other windows, to watch beside a game. It's the same window, so
// the episode, the warm Claude Code and Hachidori carry on as they were. In a
// browser (the dev server) only the player's layout changes.
import { call, inTauri } from "./api";
import { app } from "./state.svelte";

const tauri = () => import("@tauri-apps/api/window");

/** Setting: where the small player was last time and how wide, in physical pixels. */
const PLACE = "player.compact";
/** The window's smallest size outside the small player, as lib.rs makes it (logical pixels). */
const MIN_WIDTH = 960;
const MIN_HEIGHT = 600;
/** The small player's narrowest (logical pixels), so Hachidori's popup, 400 wide unless you change it, fits across. */
const SMALL_MIN = 400;
/** Its width the first time (logical pixels), or a quarter of the screen's if that's wider. */
const SMALL_WIDTH = 480;
/** Where the window was before the small player, kept in the page's session so a reload can put it back. */
const BEFORE = "telly.beforeCompact";

interface Place {
  x: number;
  y: number;
  width: number;
}

interface Before extends Place {
  height: number;
  maximized: boolean;
  fullscreen: boolean;
}

/** The picture's shape (its width over its height), and the size the small player was last given. */
let shape = 16 / 9;
let last = { width: 0, height: 0 };
let listening: (() => void)[] = [];
let reshapeTimer: ReturnType<typeof setTimeout> | null = null;
let saveTimer: ReturnType<typeof setTimeout> | null = null;
/** Each change to the window waits for the one before, so I pressed twice can't mix them up. */
let turn: Promise<unknown> = Promise.resolve();

function inTurn(change: () => Promise<void>): Promise<void> {
  const next = turn.then(change);
  turn = next.catch(() => {});
  return next;
}

function kept(): Before | null {
  try {
    return JSON.parse(sessionStorage.getItem(BEFORE) ?? "null");
  } catch {
    return null;
  }
}

function keep(before: Before | null) {
  try {
    if (before) sessionStorage.setItem(BEFORE, JSON.stringify(before));
    else sessionStorage.removeItem(BEFORE);
  } catch {}
}

const clamp = (n: number, low: number, high: number) => Math.max(low, Math.min(n, high));

/** Shrinks the window to the small player, the shape of the picture (`ratio`, its width over its
 *  height): where it was last time and as wide, else in the bottom-right corner of the screen. */
export function enterCompact(ratio: number): Promise<void> {
  return inTurn(async () => {
    if (app.compact) return;
    app.compact = true;
    shape = ratio;
    if (!inTauri) return;
    const { getCurrentWindow, LogicalSize, PhysicalPosition, PhysicalSize } = await tauri();
    const win = getCurrentWindow();
    // Out of full screen and maximised first, so what's kept is the window's own place and size.
    const fullscreen = app.fullscreen;
    if (fullscreen) {
      app.fullscreen = false;
      await win.setFullscreen(false);
    }
    const maximized = await win.isMaximized();
    if (maximized) await win.unmaximize();
    const at = await win.outerPosition();
    const size = await win.innerSize();
    keep({ x: at.x, y: at.y, width: size.width, height: size.height, maximized, fullscreen });
    const place = await placeFor(ratio, at);
    last = { width: place.width, height: Math.round(place.width / ratio) };
    await win.setMinSize(new LogicalSize(SMALL_MIN, Math.round(SMALL_MIN / ratio)));
    await win.setAlwaysOnTop(true);
    // Moved before it's sized: on a screen scaled differently, Windows scales the window as it arrives.
    await win.setPosition(new PhysicalPosition(place.x, place.y));
    await win.setSize(new PhysicalSize(last.width, last.height));
    listening = [
      await win.onResized(({ payload: size }) => {
        // Minimised.
        if (!size.width || !size.height) return;
        if (reshapeTimer) clearTimeout(reshapeTimer);
        reshapeTimer = setTimeout(() => inTurn(() => reshape(size.width, size.height)).catch(() => {}), 300);
      }),
      await win.onMoved(() => {
        if (saveTimer) clearTimeout(saveTimer);
        saveTimer = setTimeout(() => inTurn(savePlace).catch(() => {}), 500);
      }),
    ];
  });
}

/** Puts the window back as it was before the small player: its place and size, and maximised or
 *  full screen. With `fullscreen` false it stays out of full screen, as leaving the player does.
 *  After a reload in the small player, it puts the window back all the same. */
export function leaveCompact(fullscreen = true): Promise<void> {
  return inTurn(async () => {
    const before = kept();
    if (!app.compact && !before) return;
    if (app.compact) await savePlace().catch(() => {});
    app.compact = false;
    quiet();
    keep(null);
    if (!inTauri || !before) return;
    const { getCurrentWindow, LogicalSize, PhysicalPosition, PhysicalSize } = await tauri();
    const win = getCurrentWindow();
    await win.setAlwaysOnTop(false);
    await win.setPosition(new PhysicalPosition(before.x, before.y));
    await win.setSize(new PhysicalSize(before.width, before.height));
    await win.setMinSize(new LogicalSize(MIN_WIDTH, MIN_HEIGHT));
    if (before.maximized) await win.maximize();
    if (before.fullscreen && fullscreen) {
      app.fullscreen = true;
      await win.setFullscreen(true);
    }
  });
}

/** Another episode's picture has another shape: the small player takes it, as wide as it is. */
export function setCompactShape(ratio: number): Promise<void> {
  return inTurn(async () => {
    if (!app.compact || Math.abs(ratio - shape) < 0.01) return;
    shape = ratio;
    if (!inTauri) return;
    const { getCurrentWindow, LogicalSize, PhysicalSize } = await tauri();
    const win = getCurrentWindow();
    const size = await win.innerSize();
    last = { width: size.width, height: Math.round(size.width / ratio) };
    await win.setMinSize(new LogicalSize(SMALL_MIN, Math.round(SMALL_MIN / ratio)));
    await win.setSize(new PhysicalSize(last.width, last.height));
  });
}

/** Moves the window with the pointer, as a title bar does. */
export async function dragWindow(): Promise<void> {
  if (!inTauri) return;
  await (await tauri()).getCurrentWindow().startDragging();
}

/** Where the small player goes: where it was last time, if that's still on a screen, else the
 *  bottom-right corner of the screen the window is on. Kept on the screen either way. */
async function placeFor(ratio: number, at: { x: number; y: number }): Promise<Place> {
  const { currentMonitor, monitorFromPoint } = await tauri();
  const settings = await call<Record<string, unknown>>("settings").catch(() => ({}) as Record<string, unknown>);
  const s = settings[PLACE] as Partial<Place> | undefined;
  const saved = s && [s.x, s.y, s.width].every((n) => typeof n === "number" && Number.isFinite(n)) ? (s as Place) : null;
  const there = saved ? await monitorFromPoint(saved.x + saved.width / 2, saved.y + saved.width / ratio / 2) : null;
  const lastTime = there ? saved : null;
  const monitor = there ?? (await currentMonitor());
  if (!monitor) return { x: saved?.x ?? at.x, y: saved?.y ?? at.y, width: saved?.width ?? SMALL_WIDTH };
  const { position: area, size: room } = monitor.workArea;
  const scale = monitor.scaleFactor;
  const first = Math.max(SMALL_WIDTH * scale, room.width / 4);
  const width = Math.round(clamp(lastTime ? lastTime.width : first, SMALL_MIN * scale, Math.min(room.width, room.height * ratio)));
  const height = Math.round(width / ratio);
  const margin = Math.round(16 * scale);
  const x = lastTime ? lastTime.x : area.x + room.width - width - margin;
  const y = lastTime ? lastTime.y : area.y + room.height - height - margin;
  return { x: Math.round(clamp(x, area.x, area.x + room.width - width)), y: Math.round(clamp(y, area.y, area.y + room.height - height)), width };
}

/** Puts the small player back in the picture's shape once it's been resized: the side that moved most leads. */
async function reshape(width: number, height: number) {
  if (!app.compact) return;
  const byWidth = Math.abs(width - last.width) >= Math.abs(height - last.height) * shape;
  last = byWidth ? { width, height: Math.round(width / shape) } : { width: Math.round(height * shape), height };
  if (Math.abs(last.width - width) > 1 || Math.abs(last.height - height) > 1) {
    const { getCurrentWindow, PhysicalSize } = await tauri();
    await getCurrentWindow().setSize(new PhysicalSize(last.width, last.height));
  }
  await savePlace();
}

/** Keeps where the small player is and how wide, for next time. */
async function savePlace() {
  if (!app.compact || !inTauri) return;
  const win = (await tauri()).getCurrentWindow();
  // Minimised, Windows puts it far off the screen.
  if (await win.isMinimized()) return;
  const at = await win.outerPosition();
  const size = await win.innerSize();
  await call("setSetting", { key: PLACE, value: { x: at.x, y: at.y, width: size.width } });
}

/** Stops following the small player's moves and resizes. */
function quiet() {
  for (const stop of listening) stop();
  listening = [];
  if (reshapeTimer) clearTimeout(reshapeTimer);
  if (saveTimer) clearTimeout(saveTimer);
  reshapeTimer = saveTimer = null;
}
