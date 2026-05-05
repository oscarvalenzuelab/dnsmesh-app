// Derived view that turns the raw inbox + sent rows into per-contact
// conversations. Keyed by recipient/sender username when the SPK
// resolves to a pinned contact, and by a single synthetic
// "unknown-senders" bucket otherwise.

import { derived, type Readable } from "svelte/store";
import { contacts } from "$lib/stores/contacts";
import { inbox } from "$lib/stores/inbox";
import { sent, type SentRow } from "$lib/stores/sent";
import type { ContactView, InboxRow } from "$lib/api";

export const UNKNOWN_KEY = "__unknown__";

export interface ChatMessage {
  msg_id_hex: string;
  timestamp: number;
  plaintext_utf8: string;
  direction: "in" | "out";
  read: boolean;
  // Only set for incoming messages from unpinned senders (when the
  // contact lookup misses) so the thread can show the raw SPK.
  sender_spk_hex?: string;
}

export interface Conversation {
  key: string;
  // Display label. For pinned contacts: `username@domain`. For the
  // unknown bucket: "Unknown senders".
  label: string;
  username: string | null; // null for unknown bucket
  domain: string | null;
  contact: ContactView | null;
  messages: ChatMessage[];
  lastTimestamp: number;
  unread: number;
  preview: string;
}

function previewFromBody(body: string): string {
  const trimmed = body.replace(/\s+/g, " ").trim();
  if (trimmed.length <= 80) return trimmed;
  return trimmed.slice(0, 80) + "…";
}

function inToMessage(
  row: InboxRow,
  contact: ContactView | null,
): ChatMessage {
  return {
    msg_id_hex: row.msg_id_hex,
    timestamp: row.timestamp,
    plaintext_utf8: row.plaintext_utf8,
    direction: "in",
    read: row.read,
    sender_spk_hex: contact ? undefined : row.sender_signing_pk_hex,
  };
}

function outToMessage(row: SentRow): ChatMessage {
  return {
    msg_id_hex: row.msg_id_hex,
    timestamp: row.timestamp,
    plaintext_utf8: row.plaintext_utf8,
    direction: "out",
    read: true,
  };
}

export const conversations: Readable<Conversation[]> = derived(
  [inbox, sent, contacts],
  ([$inbox, $sent, $contacts]) => {
    const bySpk = new Map<string, ContactView>();
    const byUsername = new Map<string, ContactView>();
    for (const c of $contacts) {
      bySpk.set(c.ed25519_signing_public_key_hex.toLowerCase(), c);
      byUsername.set(c.username.toLowerCase(), c);
    }

    const buckets = new Map<string, Conversation>();

    function ensureBucket(
      key: string,
      label: string,
      username: string | null,
      domain: string | null,
      contact: ContactView | null,
    ): Conversation {
      const existing = buckets.get(key);
      if (existing) return existing;
      const fresh: Conversation = {
        key,
        label,
        username,
        domain,
        contact,
        messages: [],
        lastTimestamp: 0,
        unread: 0,
        preview: "",
      };
      buckets.set(key, fresh);
      return fresh;
    }

    for (const row of $inbox) {
      const contact =
        bySpk.get(row.sender_signing_pk_hex.toLowerCase()) ?? null;
      if (contact) {
        const bucket = ensureBucket(
          contact.username.toLowerCase(),
          `${contact.username}@${contact.domain}`,
          contact.username,
          contact.domain,
          contact,
        );
        bucket.messages.push(inToMessage(row, contact));
      } else {
        const bucket = ensureBucket(
          UNKNOWN_KEY,
          "Unknown senders",
          null,
          null,
          null,
        );
        bucket.messages.push(inToMessage(row, null));
      }
    }

    for (const row of $sent) {
      const recipient = row.recipient_username.toLowerCase();
      const contact = byUsername.get(recipient) ?? null;
      const bucket = ensureBucket(
        recipient,
        contact
          ? `${contact.username}@${contact.domain}`
          : recipient,
        contact ? contact.username : recipient,
        contact ? contact.domain : null,
        contact,
      );
      bucket.messages.push(outToMessage(row));
    }

    const list: Conversation[] = [];
    for (const bucket of buckets.values()) {
      bucket.messages.sort((a, b) => a.timestamp - b.timestamp);
      const last = bucket.messages[bucket.messages.length - 1];
      bucket.lastTimestamp = last ? last.timestamp : 0;
      bucket.unread = bucket.messages.filter(
        (m) => m.direction === "in" && !m.read,
      ).length;
      bucket.preview = last ? previewFromBody(last.plaintext_utf8) : "";
      list.push(bucket);
    }

    list.sort((a, b) => b.lastTimestamp - a.lastTimestamp);
    return list;
  },
);
