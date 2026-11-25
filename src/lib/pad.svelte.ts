// A controller on Telly for Android. Android's side (Pad.kt) says which button
// went down or came up; this turns a held direction into repeats, hands each
// press to whichever page is listening (Start once it's let go or held), and otherwise moves a ring around
// what's on the page: the D-pad or left stick moves it, A presses what it's
// on, B goes back, and the right stick scrolls.
import { invoke } from "@tauri-apps/api/core";
import { firstBox, nextBox, wayOf, type Box, type PadButton, type Press, type Way } from "./pad";

/** Whether a controller is what's being used: a touch turns it off, and takes the ring away. */
export const pad = $state({ used: false });

/** Takes a press and returns whether it used it (the ring gets what it didn't); `scroll` likewise for the right stick, in pixels. */
export interface PadListener {
  press: (press: Press) => boolean;
  scroll?: (by: number) => boolean;
}

const listeners: PadListener[] = [];

/** Listens for presses, the last to ask first, until the function returned is called. */
export function onPad(listener: PadListener): () => void {
  listeners.push(listener);
  return () => {
    const i = listeners.indexOf(listener);
    if (i >= 0) listeners.splice(i, 1);
  };
}

/** A held direction steps again after this long, then this often: LFG's timings. */
const FIRST_REPEAT = 350;
const NEXT_REPEAT = 100;
/** Start held this long is a hold, not a tap: LFG's, where holding its shortcut opens the wheel. */
const START_HOLD = 350;

let held: PadButton | null = null;
let startTimer: ReturnType<typeof setTimeout> | null = null;
let repeatTimer: ReturnType<typeof setTimeout> | null = null;
let speed = 0;
let scrollFrame = 0;
let scrollAt = 0;

function send(press: Press) {
  pad.used = true;
  for (let i = listeners.length - 1; i >= 0; i--) if (listeners[i].press(press)) return;
  page.press(press);
}

function letGo() {
  held = null;
  if (repeatTimer) clearTimeout(repeatTimer);
  repeatTimer = null;
}

function scrolling(at: number) {
  scrollFrame = 0;
  if (!speed) return;
  // A stick pushed all the way scrolls 1200 pixels a second.
  const by = speed * 1.2 * Math.min(64, at - scrollAt);
  scrollAt = at;
  if (!listeners.some((l, i) => listeners[listeners.length - 1 - i].scroll?.(by))) page.scroll?.(by);
  scrollFrame = requestAnimationFrame(scrolling);
}

function heard(e: Event) {
  const said = (e as CustomEvent<{ button: PadButton | "scroll"; down?: boolean; speed?: number }>).detail;
  if (said.button === "scroll") {
    speed = said.speed ?? 0;
    if (speed && !scrollFrame) {
      scrollAt = performance.now();
      scrollFrame = requestAnimationFrame(scrolling);
    }
    return;
  }
  const button = said.button;
  if (button === "start") return startHeard(said.down ?? false);
  if (!said.down) {
    if (held === button) letGo();
    return;
  }
  letGo();
  send({ button, repeat: false });
  if (!wayOf(button)) return;
  held = button;
  const again = () => {
    send({ button, repeat: true });
    repeatTimer = setTimeout(again, NEXT_REPEAT);
  };
  repeatTimer = setTimeout(again, FIRST_REPEAT);
}

/** Start is a tap when let go soon, and a hold once kept down a while; nothing is said as it goes down. */
function startHeard(down: boolean) {
  if (down) {
    if (startTimer) clearTimeout(startTimer);
    startTimer = setTimeout(() => {
      startTimer = null;
      send({ button: "start", repeat: false, held: true });
    }, START_HOLD);
  } else if (startTimer) {
    clearTimeout(startTimer);
    startTimer = null;
    send({ button: "start", repeat: false });
  }
}

// ---- the ring ------------------------------------------------------------------

const PRESSABLE = 'button, a[href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

let on: HTMLElement | null = null;
let ringed: HTMLElement | null = null;
/** Where the ring last was, for when what it was on goes. */
let lastBox: Box | null = null;

/** What the D-pad moves within: the card or panel over the page, when there is one (`data-pad-scope`). */
function scope(): HTMLElement {
  const scopes = document.querySelectorAll<HTMLElement>("[data-pad-scope]");
  return scopes[scopes.length - 1] ?? document.body;
}

function pressable(within: HTMLElement): HTMLElement[] {
  return [...within.querySelectorAll<HTMLElement>(PRESSABLE)].filter((el) => {
    if ((el as HTMLButtonElement).disabled) return false;
    const box = el.getBoundingClientRect();
    if (!box.width || !box.height) return false;
    const style = getComputedStyle(el);
    return style.visibility !== "hidden" && style.pointerEvents !== "none";
  });
}

/** A tick box or a field is ringed with its label. */
const ringOf = (el: HTMLElement) => el.closest<HTMLElement>("label") ?? el;

function unring() {
  ringed?.classList.remove("pad-focus");
  ringed = null;
}

function ring(el: HTMLElement) {
  unring();
  on = el;
  ringed = ringOf(el);
  ringed.classList.add("pad-focus");
  el.focus({ preventScroll: true });
  ringed.scrollIntoView({ block: "nearest", inline: "nearest" });
  lastBox = ringed.getBoundingClientRect();
}

/** What the ring is on, if it's still on the page and within what the D-pad moves in. */
function ringedNow(list: HTMLElement[]): HTMLElement | null {
  return on && ringed?.classList.contains("pad-focus") && list.includes(on) ? on : null;
}

/** Where the ring starts: what the page says (`data-pad-first`), else nearest where it last was, else the top left. */
function start(within: HTMLElement, list: HTMLElement[]): HTMLElement | null {
  const first = within.querySelector<HTMLElement>("[data-pad-first]");
  if (first && list.includes(first)) return first;
  const boxes = list.map((el) => ringOf(el).getBoundingClientRect());
  if (lastBox && within === document.body) {
    const [x, y] = [(lastBox.left + lastBox.right) / 2, (lastBox.top + lastBox.bottom) / 2];
    const far = boxes.map((b) => Math.hypot((b.left + b.right) / 2 - x, (b.top + b.bottom) / 2 - y));
    return list[far.indexOf(Math.min(...far))] ?? null;
  }
  return list[firstBox(boxes)] ?? null;
}

function move(way: Way) {
  const within = scope();
  const list = pressable(within);
  const from = ringedNow(list);
  if (!from) {
    const first = start(within, list);
    if (first) ring(first);
    return;
  }
  const next = nextBox(ringOf(from).getBoundingClientRect(), list.map((el) => ringOf(el).getBoundingClientRect()), way);
  if (next >= 0) ring(list[next]);
}

/**
 * Puts the ring where it starts on what's now up (a card that just opened, a
 * new page), if a controller is being used. A page that listens for itself
 * has the ring only on its cards and panels.
 */
export function padStart() {
  if (!pad.used) return;
  const within = scope();
  const list = pressable(within);
  // Off a card or panel, a page that listens for itself has no ring: it goes with what it was on.
  if (within === document.body && listeners.length) return unring();
  if (ringedNow(list)) return;
  unring();
  if (within !== document.body) return void (list.length && ring(start(within, list) ?? list[0]));
  lastBox = null;
  const first = start(within, list);
  if (first) ring(first);
}

const TYPED = new Set(["text", "search", "url", "tel", "email", "password", "number"]);

function pressIt() {
  const within = scope();
  const list = pressable(within);
  const el = ringedNow(list);
  if (!el) {
    const first = start(within, list);
    if (first) ring(first);
    return;
  }
  if (el instanceof HTMLTextAreaElement || (el instanceof HTMLInputElement && TYPED.has(el.type))) {
    // A field is typed in with Android's keyboard.
    el.focus();
    invoke("plugin:telly|keyboard").catch(() => {});
  } else el.click();
}

/** The part of the page that scrolls: the one the ring is in, else the first in what the D-pad moves within. */
function scroller(): HTMLElement | null {
  const scrolls = (el: HTMLElement) => el.scrollHeight > el.clientHeight + 1 && /auto|scroll/.test(getComputedStyle(el).overflowY);
  const within = scope();
  for (let el = on && within.contains(on) ? on.parentElement : null; el && el !== document.body; el = el.parentElement) if (scrolls(el)) return el;
  if (scrolls(within)) return within;
  return [...within.querySelectorAll<HTMLElement>("*")].find(scrolls) ?? null;
}

let scrolled: HTMLElement | null = null;
let scrolledAt = 0;

const page: PadListener = {
  press({ button }) {
    const way = wayOf(button);
    if (way) move(way);
    else if (button === "a") pressIt();
    else if (button === "b" && location.pathname !== "/phone") history.back();
    return true;
  },
  scroll(by) {
    // Found once for each push of the stick.
    const now = performance.now();
    if (!scrolled?.isConnected || now - scrolledAt > 300) scrolled = scroller();
    scrolledAt = now;
    scrolled?.scrollBy(0, by);
    return true;
  },
};

/** Starts listening to the controller. */
export function startPad(): () => void {
  const touched = () => {
    pad.used = false;
    unring();
  };
  window.addEventListener("telly-pad", heard);
  window.addEventListener("pointerdown", touched, true);
  return () => {
    letGo();
    if (startTimer) clearTimeout(startTimer);
    startTimer = null;
    speed = 0;
    window.removeEventListener("telly-pad", heard);
    window.removeEventListener("pointerdown", touched, true);
  };
}
