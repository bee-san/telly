// What both Stats pages write the same way: numbers, lengths of time and days.

export const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
export const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/** "1,234". */
export function num(n: number): string {
  return n.toLocaleString("en-GB");
}

/** "1,234 episodes", "1 card". */
export function count(n: number, one: string, many = one + "s"): string {
  return `${num(n)} ${n === 1 ? one : many}`;
}

/** "2 h 54 min", "2 h", "35 min". */
export function duration(seconds: number): string {
  const m = Math.round(seconds / 60);
  const h = Math.floor(m / 60);
  const r = m % 60;
  return h && r ? `${num(h)} h ${r} min` : h ? `${num(h)} h` : `${r} min`;
}

/** A length of time as numbers and their units, for a big figure. */
export function durationParts(seconds: number): { n: string; unit: string }[] {
  const m = Math.round(seconds / 60);
  const h = Math.floor(m / 60);
  const r = m % 60;
  const parts = [];
  if (h) parts.push({ n: num(h), unit: "h" });
  if (r || !h) parts.push({ n: String(r), unit: "min" });
  return parts;
}

/** A day's key, as the backend's days are: "2026-11-13", in local time. */
export function dayKey(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** The day a key stands for, at midnight local time. */
export function dayOf(key: string): Date {
  const [y, m, d] = key.split("-").map(Number);
  return new Date(y, m - 1, d);
}

/** `d` moved by `n` days. */
export function addDays(d: Date, n: number): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n);
}

/** Whole days from `a` to `b`. */
export function daysBetween(a: Date, b: Date): number {
  return Math.round((Date.UTC(b.getFullYear(), b.getMonth(), b.getDate()) - Date.UTC(a.getFullYear(), a.getMonth(), a.getDate())) / 86400000);
}

/** "12 November", or "12 November 2026" with the year. */
export function dayName(d: Date, year = false): string {
  return `${d.getDate()} ${MONTHS[d.getMonth()]}${year ? ` ${d.getFullYear()}` : ""}`;
}

/** "Saturday 31 October", with the year when asked. */
export function weekdayName(d: Date, year = false): string {
  return `${WEEKDAYS[d.getDay()]} ${dayName(d, year)}`;
}

/** "7–13 November 2026", "15 October – 13 November 2026", "2 June 2025 – 13 November 2026". */
export function span(a: Date, b: Date): string {
  if (a.getFullYear() !== b.getFullYear()) return `${dayName(a, true)} – ${dayName(b, true)}`;
  if (a.getMonth() !== b.getMonth()) return `${dayName(a)} – ${dayName(b, true)}`;
  if (a.getDate() !== b.getDate()) return `${a.getDate()}–${dayName(b, true)}`;
  return dayName(b, true);
}
