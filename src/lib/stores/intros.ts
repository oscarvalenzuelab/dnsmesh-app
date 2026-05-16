// Pending-intros store. The SDK's quarantine queue holds messages from
// un-pinned senders that the receiver hasn't yet accepted/trusted/blocked.
// `receive_messages` populates the queue server-side on each poll; this
// store mirrors the count + list so the topbar can show a badge and the
// inbox can show a banner without forcing the user onto /intro to discover
// pending work.

import { writable, derived, get } from "svelte/store";
import { api, type IntroView } from "$lib/api";
import { activeIdentity } from "$lib/stores/identity";

export const intros = writable<IntroView[]>([]);
export const introCount = derived(intros, ($i) => $i.length);

// Refresh the in-memory list from the backend. Identity-change-mid-flight
// guard mirrors the inbox/sent stores so a stale identity's intros never
// clobber the new identity's freshly-loaded list.
export async function refreshIntros(): Promise<void> {
  const identityAtStart = get(activeIdentity)?.username ?? null;
  if (identityAtStart === null) {
    intros.set([]);
    return;
  }
  try {
    const rows = await api.introList();
    if (get(activeIdentity)?.username !== identityAtStart) return;
    intros.set(rows);
  } catch (err) {
    // Refresh failures are non-fatal — the badge falls back to whatever
    // count was last known good. /intro itself surfaces the error inline.
    console.warn("intro refresh failed", err);
  }
}

// Drop the in-memory list on lock so a stale count doesn't bleed across
// the identity boundary.
export function clearIntros(): void {
  intros.set([]);
}
