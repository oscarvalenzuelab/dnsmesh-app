// Proto-only outgoing-message store. The Rust backend persists received
// messages in `inbox.jsonl` but does not yet persist sends, so the chat-
// shell prototype keeps a per-identity sent log in localStorage. Promote
// to a Rust-side `sent.jsonl` once the wire shape is confirmed.
//
// Each row is keyed by the SDK's `msg_id_hex` so dedupe across reloads
// is trivial. A TTL setting per identity drives a sweep on hydrate.

import { writable, get } from "svelte/store";

export interface SentRow {
  msg_id_hex: string;
  recipient_username: string;
  timestamp: number; // unix seconds
  plaintext_utf8: string;
}

// "never" disables the sweep; numbers are hours. Max retention is
// capped at 24h by product policy — sent messages are best-effort
// local context, not a transcript.
export type SentTtlHours = 1 | 6 | 12 | 24;
export const DEFAULT_SENT_TTL_HOURS: SentTtlHours = 24;
const TTL_HOUR_OPTIONS: ReadonlyArray<number> = [1, 6, 12, 24];

export const sent = writable<SentRow[]>([]);

// `null` until the first identity hydrate; consumers should handle empty.
let activeKey: string | null = null;

function rowsKey(identity: string): string {
  return `dnsmesh.sent.v1.${identity}`;
}

// v2 is hours-based; v1 (days) entries are silently ignored on read.
function ttlKey(identity: string): string {
  return `dnsmesh.sent.ttl.v2.${identity}`;
}

function readRows(identity: string): SentRow[] {
  if (typeof localStorage === "undefined") return [];
  const raw = localStorage.getItem(rowsKey(identity));
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(isSentRow);
  } catch {
    return [];
  }
}

function writeRows(identity: string, rows: SentRow[]): void {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(rowsKey(identity), JSON.stringify(rows));
}

function isSentRow(value: unknown): value is SentRow {
  if (!value || typeof value !== "object") return false;
  const r = value as Record<string, unknown>;
  return (
    typeof r.msg_id_hex === "string" &&
    typeof r.recipient_username === "string" &&
    typeof r.timestamp === "number" &&
    typeof r.plaintext_utf8 === "string"
  );
}

export function getSentTtl(identity: string): SentTtlHours {
  if (typeof localStorage === "undefined") return DEFAULT_SENT_TTL_HOURS;
  const raw = localStorage.getItem(ttlKey(identity));
  const n = Number(raw);
  if (TTL_HOUR_OPTIONS.includes(n)) return n as SentTtlHours;
  // Migration: any stored value above the cap (or "never") collapses
  // to the 24h max so we honor the new product policy.
  return DEFAULT_SENT_TTL_HOURS;
}

export function setSentTtl(identity: string, ttl: SentTtlHours): void {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(ttlKey(identity), String(ttl));
  // Re-sweep with the new policy so the change is visible immediately.
  hydrateSent(identity);
}

// Drop rows older than the TTL. Returns the survivors.
function sweep(rows: SentRow[], ttl: SentTtlHours): SentRow[] {
  const cutoff = Date.now() / 1000 - ttl * 3600;
  return rows.filter((r) => r.timestamp >= cutoff);
}

export function hydrateSent(identity: string): void {
  activeKey = identity;
  const ttl = getSentTtl(identity);
  const fresh = sweep(readRows(identity), ttl);
  writeRows(identity, fresh);
  sent.set(fresh);
}

export function clearSent(): void {
  activeKey = null;
  sent.set([]);
}

// Pass the identity explicitly so a missed `hydrateSent` (e.g. race
// at unlock) can't make sends silently vanish. If the store hasn't
// been hydrated for this identity yet, hydrate first so the in-memory
// view stays consistent with what we're about to write.
export function appendSent(identity: string, row: SentRow): void {
  if (typeof localStorage === "undefined") return;
  if (activeKey !== identity) {
    hydrateSent(identity);
  }
  const next = [...get(sent), row];
  writeRows(identity, next);
  sent.set(next);
}

// Drop every sent row whose recipient matches `recipient` (case-
// insensitive). Used by "Clear chat" so a conversation can be wiped
// from the local sent log without touching other threads.
export function removeSentByRecipient(
  identity: string,
  recipient: string,
): void {
  if (typeof localStorage === "undefined") return;
  if (activeKey !== identity) {
    hydrateSent(identity);
  }
  const lower = recipient.toLowerCase();
  const next = get(sent).filter(
    (r) => r.recipient_username.toLowerCase() !== lower,
  );
  writeRows(identity, next);
  sent.set(next);
}
