// Contacts store. Cached locally; refresh after add / fetch.

import { writable } from "svelte/store";
import { api, type ContactView } from "$lib/api";

export const contacts = writable<ContactView[]>([]);

export async function refreshContacts(): Promise<void> {
  try {
    const list = await api.listContacts();
    contacts.set(list);
  } catch {
    contacts.set([]);
  }
}
