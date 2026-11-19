import { redirect } from "@sveltejs/kit";
import { call, callKept, type Backup, type MissingRips, type Settings } from "$lib/api";
import { plural } from "$lib/format";
import { onPhone } from "$lib/phone";
import { toast } from "$lib/toast-store.svelte";

/** Whether this run of Telly has looked at whether the setup is done. */
let looked = false;

/** Opening Telly goes to the first-time setup until it's done or skipped.
 *  Just after a backup was restored, it says so, and goes to Settings →
 *  Moved rips when shows' rips aren't where the backup has them. */
export async function load() {
  // The phone has its own Library: the shows it got from your PC.
  if (onPhone) redirect(307, "/phone");
  if (looked) return;
  looked = true;
  const settings = await callKept<Settings>("settings").catch(() => null);
  const restored = settings?.["backup.restored"] as Backup | undefined;
  const failed = settings?.["backup.restoreFailed"] as string | undefined;
  if (restored || failed) {
    await call("setSetting", { key: "backup.restored", value: null }).catch(() => {});
    await call("setSetting", { key: "backup.restoreFailed", value: null }).catch(() => {});
  }
  if (failed) toast({ key: "restore", state: "failed", title: "Your backup wasn’t restored", text: `Your library is as it was. ${failed}` });
  if (restored) {
    const made = new Intl.DateTimeFormat(undefined, { dateStyle: "long" }).format(restored.madeAt);
    toast({ key: "restore", state: "done", title: "Your library is back", text: `As it was on ${made}, with ${plural(restored.shows, "show")}.` }, 10000);
    const away = await call<MissingRips[]>("missingRips").catch(() => []);
    if (away.length) redirect(307, "/settings?restored#moved");
  }
  if (settings && settings["setup.done"] !== true) redirect(307, "/setup");
}
