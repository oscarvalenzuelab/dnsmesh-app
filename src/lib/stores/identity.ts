// Active identity store. `null` means no identity unlocked.

import { writable } from "svelte/store";
import { api, type IdentityInfo, type PublishedStatus } from "$lib/api";

export const activeIdentity = writable<IdentityInfo | null>(null);

// Cached DNS-publish status; `null` is treated as "unknown, show Publish".
export const publishedStatus = writable<PublishedStatus | null>(null);

export async function refreshActiveIdentity(): Promise<void> {
  try {
    const info = await api.getIdentityInfo();
    activeIdentity.set(info);
    if (info) {
      // Fire-and-forget so the network check doesn't block unlock UX.
      void refreshPublishedStatus();
    } else {
      publishedStatus.set(null);
    }
  } catch {
    activeIdentity.set(null);
    publishedStatus.set(null);
  }
}

export async function refreshPublishedStatus(): Promise<void> {
  try {
    const status = await api.isIdentityPublished();
    publishedStatus.set(status);
  } catch {
    publishedStatus.set(null);
  }
}
