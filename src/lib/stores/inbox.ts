// Inbox store. Pairs the SDK's pull-only `receive_messages` with the
// per-identity `inbox.jsonl` file so messages survive identity switches
// (the SDK replay cache returns each message only once).

import { writable, get } from "svelte/store";
import { api, type InboxRow, type InboxMessageView } from "$lib/api";

export const inbox = writable<InboxRow[]>([]);
export const inboxError = writable<string | null>(null);
export const inboxBusy = writable<boolean>(false);

// Replace the in-memory store from disk. Failures surface via
// `inboxError`; we never throw, so other pages keep working.
export async function hydrateInbox(): Promise<void> {
  inboxError.set(null);
  try {
    const rows = await api.inboxLoad();
    inbox.set(rows);
  } catch (err) {
    inboxError.set(formatError(err));
  }
}

// Pull fresh messages from the SDK, persist, merge. Returns the count
// new to the persistent inbox (already-known msg ids are silently deduped).
export async function pollInbox(): Promise<number> {
  inboxBusy.set(true);
  inboxError.set(null);
  try {
    const fresh = await api.receiveMessages();
    let added = 0;
    if (fresh.length > 0) {
      const result = await api.inboxAppend(fresh.map(toPersisted));
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
    inboxError.set(formatError(err));
    return 0;
  } finally {
    inboxBusy.set(false);
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

export async function markAllRead(): Promise<void> {
  inbox.update((rows) => rows.map((r) => ({ ...r, read: true })));
  try {
    await api.inboxMarkAllRead();
  } catch (err) {
    console.warn("inbox_mark_all_read failed", err);
  }
}

export async function deleteMessage(msgIdHex: string): Promise<boolean> {
  return deleteMessages([msgIdHex]);
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

export function inboxSnapshot(): InboxRow[] {
  return get(inbox);
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
