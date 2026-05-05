// Inbox store. Pairs the SDK's pull-only `receive_messages` with the
// per-identity `inbox.jsonl` file so messages survive identity switches
// (the SDK replay cache returns each message only once).

import { writable, get } from "svelte/store";
import { api, type InboxRow, type InboxMessageView } from "$lib/api";
import { activeIdentity } from "$lib/stores/identity";

export const inbox = writable<InboxRow[]>([]);
export const inboxError = writable<string | null>(null);

// Replace the in-memory store from disk. Failures surface via
// `inboxError`; we never throw, so other pages keep working.
export async function hydrateInbox(): Promise<void> {
  inboxError.set(null);
  const identityAtStart = get(activeIdentity)?.username ?? null;
  try {
    const rows = await api.inboxLoad();
    // Bail if the user locked or switched while the load was in flight
    // — without this, a stale identity's rows would clobber the new
    // identity's already-loaded inbox.
    if (get(activeIdentity)?.username !== identityAtStart) return;
    inbox.set(rows);
  } catch (err) {
    if (get(activeIdentity)?.username !== identityAtStart) return;
    inboxError.set(formatError(err));
  }
}

// Pull fresh messages from the SDK, persist, merge. Returns the count
// new to the persistent inbox (already-known msg ids are silently deduped).
export async function pollInbox(): Promise<number> {
  const identityAtStart = get(activeIdentity)?.username ?? null;
  if (identityAtStart === null) return 0;
  inboxError.set(null);
  try {
    const fresh = await api.receiveMessages();
    // Identity changed mid-poll — drop the result on the floor; a poll
    // for the new identity is already (or will shortly be) in flight
    // from the layout's effect.
    if (get(activeIdentity)?.username !== identityAtStart) return 0;
    let added = 0;
    if (fresh.length > 0) {
      const result = await api.inboxAppend(fresh.map(toPersisted));
      if (get(activeIdentity)?.username !== identityAtStart) return 0;
      added = result.appended;
    }
    inbox.update((existing) => {
      const seen = new Set(existing.map((m) => m.msg_id_hex));
      const merged = [...existing];
      for (const m of fresh) {
        if (!seen.has(m.msg_id_hex)) {
          merged.unshift({ ...toPersisted(m), read: false });
          seen.add(m.msg_id_hex);
        }
      }
      return merged;
    });
    return added;
  } catch (err) {
    if (get(activeIdentity)?.username !== identityAtStart) return 0;
    inboxError.set(formatError(err));
    return 0;
  }
}



// Mark a single message read; persists, then optimistically flips in memory.
export async function markRead(msgIdHex: string): Promise<void> {
  inbox.update((rows) =>
    rows.map((r) => (r.msg_id_hex === msgIdHex ? { ...r, read: true } : r)),
  );
  try {
    await api.inboxMarkRead(msgIdHex);
  } catch (err) {
    console.warn("inbox_mark_read failed", err);
  }
}

// Backend treats unknown ids as a no-op, so this returns true on success
// even when zero rows actually matched.
export async function deleteMessages(msgIdHexes: string[]): Promise<boolean> {
  if (msgIdHexes.length === 0) return true;
  inboxError.set(null);
  try {
    await api.inboxDelete(msgIdHexes);
    const drop = new Set(msgIdHexes.map((id) => id.toLowerCase()));
    inbox.update((rows) =>
      rows.filter((r) => !drop.has(r.msg_id_hex.toLowerCase())),
    );
    return true;
  } catch (err) {
    inboxError.set(formatError(err));
    return false;
  }
}

// Drop the in-memory inbox without touching disk. Called on lock so a
// stale list doesn't bleed across the boundary.
export function clearInbox(): void {
  inbox.set([]);
  inboxError.set(null);
}

function toPersisted(m: InboxMessageView) {
  return {
    sender_signing_pk_hex: m.sender_signing_pk_hex,
    msg_id_hex: m.msg_id_hex,
    timestamp: m.timestamp,
    plaintext_utf8: m.plaintext_utf8,
    plaintext_bytes: m.plaintext_bytes,
  };
}

function formatError(err: unknown): string {
  if (err && typeof err === "object" && "message" in err) {
    return String((err as { message: unknown }).message);
  }
  return String(err);
}
