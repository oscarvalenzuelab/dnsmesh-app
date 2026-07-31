// Per-identity outgoing-message store, backed by the Rust side's
// `sent.jsonl` — one sealed record per line under the identity's at-rest
// key, the same treatment `inbox.jsonl` gets.
//
// This previously kept rows in localStorage. That left outgoing message
// bodies outside the identity directory entirely and in the clear, so
// encrypting the database and the inbox would still have left sent
// messages as a readable copy of every conversation.
//
// Each row is keyed by the SDK's `msg_id_hex`, so the backend dedupes on
// append. The TTL preference stays in localStorage: it is a retention
// number, not message content, and keeping it client-side avoids a config
// migration for something the user can simply re-pick.

import { writable, get } from "svelte/store";
import { api } from "$lib/api";

export interface SentRow {
  msg_id_hex: string;
  recipient_username: string;
  timestamp: number; // unix seconds
  plaintext_utf8: string;
}

// Numbers are hours. Max retention is capped at 24h by product policy —
// sent messages are best-effort local context, not a transcript.
export type SentTtlHours = 1 | 6 | 12 | 24;
export const DEFAULT_SENT_TTL_HOURS: SentTtlHours = 24;
const TTL_HOUR_OPTIONS: ReadonlyArray<number> = [1, 6, 12, 24];

export const sent = writable<SentRow[]>([]);

// `null` until the first identity hydrate; consumers should handle empty.
let activeKey: string | null = null;

// v2 is hours-based; v1 (days) entries are silently ignored on read.
function ttlKey(identity: string): string {
  return `dnsmesh.sent.ttl.v2.${identity}`;
}

// Where rows lived before they moved to the sealed backend store.
function legacyRowsKey(identity: string): string {
  return `dnsmesh.sent.v1.${identity}`;
}

// Move any rows left in localStorage into the sealed store, then drop the
// key. Without this an upgraded install both loses the history from the UI
// and keeps plaintext message bodies in localStorage indefinitely — which
// would defeat the point of sealing the file.
async function migrateLegacyRows(identity: string): Promise<void> {
  if (typeof localStorage === "undefined") return;
  const raw = localStorage.getItem(legacyRowsKey(identity));
  if (!raw) return;
  let rows: SentRow[] = [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (Array.isArray(parsed)) rows = parsed as SentRow[];
  } catch {
    // Unreadable legacy blob: nothing to carry over, but still clear it
    // rather than leaving it sitting there.
  }
  try {
    for (const row of rows) {
      // `sentAppend` writes to whichever identity the backend currently
      // has unlocked, not to `identity`. If the user switches or locks
      // mid-migration, continuing would file the remaining rows under
      // someone else's log. Bail and keep the legacy key so the next
      // hydrate for this identity can finish the job.
      if (activeKey !== identity) return;
      if (row && typeof row.msg_id_hex === "string") {
        await api.sentAppend(identity, row);
      }
    }
    // Re-check before dropping the source: the last append may have
    // landed after a switch.
    if (activeKey !== identity) return;
  } catch {
    // Persisting failed — keep the legacy key so a later attempt can
    // retry rather than dropping the rows on the floor.
    return;
  }
  localStorage.removeItem(legacyRowsKey(identity));
}

export function getSentTtl(identity: string): SentTtlHours {
  if (typeof localStorage === "undefined") return DEFAULT_SENT_TTL_HOURS;
  const raw = localStorage.getItem(ttlKey(identity));
  const parsed = Number(raw);
  return TTL_HOUR_OPTIONS.includes(parsed)
    ? (parsed as SentTtlHours)
    : DEFAULT_SENT_TTL_HOURS;
}

export function setSentTtl(identity: string, ttl: SentTtlHours): void {
  if (typeof localStorage !== "undefined") {
    localStorage.setItem(ttlKey(identity), String(ttl));
  }
  // Re-hydrating applies the new window immediately: the backend sweeps
  // on load and rewrites the file when rows fall outside it.
  void hydrateSent(identity);
}

/// Load this identity's sent log, letting the backend sweep expired rows.
export async function hydrateSent(identity: string): Promise<void> {
  activeKey = identity;
  try {
    await migrateLegacyRows(identity);
    const rows = await api.sentLoad(getSentTtl(identity));
    // A slower hydrate must not clobber a newer identity's rows.
    if (activeKey !== identity) return;
    sent.set(rows);
  } catch {
    if (activeKey === identity) sent.set([]);
  }
}

export function clearSent(): void {
  activeKey = null;
  sent.set([]);
}

// Pass the identity explicitly so a missed `hydrateSent` (e.g. race at
// unlock) can't make sends silently vanish.
export async function appendSent(
  identity: string,
  row: SentRow,
): Promise<void> {
  try {
    const rows = await api.sentAppend(identity, row);
    if (activeKey !== null && activeKey !== identity) return;
    activeKey = identity;
    sent.set(rows);
  } catch {
    // Keep the optimistic row visible even if persistence failed, so the
    // message doesn't vanish from the thread the user is looking at.
    sent.set([...get(sent), row]);
  }
}

// Drop every sent row addressed to `recipient` (case-insensitive). Used by
// "Clear chat" so one conversation can be wiped without touching others.
export async function removeSentByRecipient(
  identity: string,
  recipient: string,
): Promise<void> {
  try {
    const rows = await api.sentRemoveByRecipient(identity, recipient);
    if (activeKey !== null && activeKey !== identity) return;
    activeKey = identity;
    sent.set(rows);
  } catch {
    const lower = recipient.toLowerCase();
    sent.set(
      get(sent).filter((r) => r.recipient_username.toLowerCase() !== lower),
    );
  }
}
