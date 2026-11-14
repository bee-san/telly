import { redirect } from "@sveltejs/kit";
import { callKept, type Settings } from "$lib/api";

/** Whether this run of Telly has looked at whether the setup is done. */
let looked = false;

/** Opening Telly goes to the first-time setup until it's done or skipped. */
export async function load() {
  if (looked) return;
  looked = true;
  const settings = await callKept<Settings>("settings").catch(() => null);
  if (settings && settings["setup.done"] !== true) redirect(307, "/setup");
}
