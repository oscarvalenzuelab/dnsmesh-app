<script lang="ts">
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { activeIdentity } from "$lib/stores/identity";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import { api, isCommandError, type ContactView } from "$lib/api";
  import {
    avatarBackground,
    avatarForeground,
    avatarInitials,
  } from "$lib/avatar";

  let mode = $state<"manual" | "fetch">("fetch");

  // Fetch by address
  let address = $state<string>("");
  let preview = $state<ContactView | null>(null);
  let fetchBusy = $state<boolean>(false);
  let fetchError = $state<string>("");

  // Manual add
  let manualUsername = $state<string>("");
  let manualDomain = $state<string>("");
  let manualX25519 = $state<string>("");
  let manualEd25519 = $state<string>("");
  let manualBusy = $state<boolean>(false);
  let manualError = $state<string>("");

  let lastResult = $state<string>("");

  // Per-page status (currently used by delete).
  let info = $state<string>("");
  let error = $state<string>("");

  onMount(() => {
    refreshContacts();
    // Deep-link from the Inbox "Add to contacts" button: the Inbox
    // only knows the sender's Ed25519 SPK, so prefill it in the
    // manual form and let the user verify against a fresh fetch.
    const ed = page.url.searchParams.get("ed25519_spk_hex");
    if (ed) {
      mode = "manual";
      manualEd25519 = ed;
    }
    const addr = page.url.searchParams.get("address");
    if (addr) {
      mode = "fetch";
      address = addr.trim().toLowerCase();
    }
  });

  // Lowercase + trim. The SDK derives the
  // `id-<sha256(username)[:16]>.<zone>` label from raw bytes, so a
  // `Bob@…` lookup would miss a record published by `bob@…`.
  function normalizeAddress(raw: string): string {
    return raw.trim().toLowerCase();
  }

  async function doFetch() {
    fetchError = "";
    preview = null;
    const normalized = normalizeAddress(address);
    if (!normalized) {
      fetchError = "Enter an address (user@host).";
      return;
    }
    // Reflect the normalized form back so the user sees what we asked.
    address = normalized;
    fetchBusy = true;
    try {
      preview = await api.fetchIdentity(normalized);
    } catch (err) {
      fetchError = isCommandError(err) ? err.message : String(err);
    } finally {
      fetchBusy = false;
    }
  }

  async function pinFetched() {
    if (!preview) return;
    fetchBusy = true;
    fetchError = "";
    try {
      const res = await api.addContact({
        username: preview.username,
        domain: preview.domain,
        x25519_public_key_hex: preview.x25519_public_key_hex,
        ed25519_signing_public_key_hex:
          preview.ed25519_signing_public_key_hex,
      });
      lastResult = res.newly_added
        ? `Pinned ${res.contact.username}@${res.contact.domain}.`
        : `Updated existing pin for ${res.contact.username}@${res.contact.domain}.`;
      preview = null;
      address = "";
      await refreshContacts();
    } catch (err) {
      fetchError = isCommandError(err) ? err.message : String(err);
    } finally {
      fetchBusy = false;
    }
  }

  async function doManualAdd() {
    manualError = "";
    // Same lowercase normalization the SDK publishes under.
    const username = normalizeAddress(manualUsername);
    const domain = normalizeAddress(manualDomain);
    if (
      !username ||
      !domain ||
      !manualX25519.trim() ||
      !manualEd25519.trim()
    ) {
      manualError = "All fields required.";
      return;
    }
    manualUsername = username;
    manualDomain = domain;
    manualBusy = true;
    try {
      const res = await api.addContact({
        username,
        domain,
        x25519_public_key_hex: manualX25519.trim(),
        ed25519_signing_public_key_hex: manualEd25519.trim(),
      });
      lastResult = res.newly_added
        ? `Pinned ${res.contact.username}@${res.contact.domain}.`
        : `Updated existing pin for ${res.contact.username}@${res.contact.domain}.`;
      manualUsername = "";
      manualDomain = "";
      manualX25519 = "";
      manualEd25519 = "";
      await refreshContacts();
    } catch (err) {
      manualError = isCommandError(err) ? err.message : String(err);
    } finally {
      manualBusy = false;
    }
  }

  // Open the chat shell focused on this contact.
  function composeFor(username: string) {
    void goto(`/?contact=${encodeURIComponent(username)}`);
  }

  // Username currently being deleted; empty when no delete is pending.
  let pendingDelete = $state<string>("");

  async function deleteContact(username: string, domain: string) {
    const ok = window.confirm(
      `Delete contact ${username}@${domain}? You can re-add by address later.`,
    );
    if (!ok) return;
    pendingDelete = username;
    error = "";
    info = "";
    try {
      await api.deleteContact(username);
      await refreshContacts();
      info = `Removed ${username}@${domain}.`;
    } catch (err) {
      error = isCommandError(err) ? err.message : String(err);
    } finally {
      pendingDelete = "";
    }
  }
</script>

<section>
  <header class="page-header">
    <h1>Contacts</h1>
  </header>

  {#if info}
    <p class="status-info">{info}</p>
  {/if}
  {#if error}
    <p class="status-error">{error}</p>
  {/if}

  {#if !$activeIdentity}
    <p class="muted">
      Unlock an identity from <a href="/identities">Identities</a> first.
    </p>
  {:else}
    <div class="modes">
      <button
        class:primary={mode === "fetch"}
        onclick={() => (mode = "fetch")}>Fetch by address</button
      >
      <button
        class:primary={mode === "manual"}
        onclick={() => (mode = "manual")}>Pin manually</button
      >
    </div>

    {#if mode === "fetch"}
      <form
        class="add-form"
        onsubmit={(e) => {
          e.preventDefault();
          doFetch();
        }}
      >
        <label>
          <span>Address (user@host)</span>
          <input
            type="text"
            bind:value={address}
            placeholder="bob@mesh.example.com"
          />
          <small class="hint muted">
            Usernames are case-insensitive; your input will be normalized to lowercase.
          </small>
        </label>
        <div class="actions">
          <button type="submit" disabled={fetchBusy}>
            {fetchBusy ? "Fetching…" : "Fetch"}
          </button>
        </div>
        {#if fetchError}
          <p class="error">{fetchError}</p>
        {/if}
      </form>

      {#if preview}
        <div class="preview">
          <h3>Verify before pinning</h3>
          <table>
            <tbody>
              <tr>
                <th>Username</th><td>{preview.username}</td>
              </tr>
              <tr>
                <th>Domain</th><td>{preview.domain}</td>
              </tr>
              <tr>
                <th>X25519</th>
                <td><code>{preview.x25519_public_key_hex}</code></td>
              </tr>
              <tr>
                <th>Ed25519</th>
                <td><code>{preview.ed25519_signing_public_key_hex}</code></td>
              </tr>
            </tbody>
          </table>
          <div class="actions">
            <button class="primary" disabled={fetchBusy} onclick={pinFetched}>
              Pin contact
            </button>
            <button disabled={fetchBusy} onclick={() => (preview = null)}>
              Cancel
            </button>
          </div>
        </div>
      {/if}
    {:else}
      <form
        class="add-form"
        onsubmit={(e) => {
          e.preventDefault();
          doManualAdd();
        }}
      >
        <label>
          <span>Username</span>
          <input type="text" bind:value={manualUsername} />
          <small class="hint muted">
            Case-insensitive; will be normalized to lowercase.
          </small>
        </label>
        <label>
          <span>Domain</span>
          <input type="text" bind:value={manualDomain} />
        </label>
        <label>
          <span>X25519 public key (64-char hex)</span>
          <input type="text" bind:value={manualX25519} />
        </label>
        <label>
          <span>Ed25519 signing public key (64-char hex)</span>
          <input type="text" bind:value={manualEd25519} />
        </label>
        <div class="actions">
          <button class="primary" type="submit" disabled={manualBusy}>
            {manualBusy ? "Pinning…" : "Pin contact"}
          </button>
        </div>
        {#if manualError}
          <p class="error">{manualError}</p>
        {/if}
      </form>
    {/if}

    {#if lastResult}
      <p class="pass">{lastResult}</p>
    {/if}

    <h2>Pinned</h2>
    {#if $contacts.length === 0}
      <p class="muted">No contacts pinned yet.</p>
    {:else}
      <ul class="contact-cards">
        {#each $contacts as c (c.username + "@" + c.domain)}
          {@const initials = avatarInitials(
            c.username,
            c.ed25519_signing_public_key_hex,
          )}
          <li class="contact-card">
            <span
              class="avatar"
              style="background:{avatarBackground(
                c.ed25519_signing_public_key_hex,
              )};color:{avatarForeground(
                c.ed25519_signing_public_key_hex,
              )};"
              aria-hidden="true"
            >{initials}</span>
            <div class="contact-meta">
              <div class="contact-name">{c.username}@{c.domain}</div>
              <div class="contact-keys">
                <code title="X25519 public key">
                  x{c.x25519_public_key_hex.slice(0, 16)}…
                </code>
                <code title="Ed25519 signing public key">
                  ed{c.ed25519_signing_public_key_hex.slice(0, 16)}…
                </code>
              </div>
            </div>
            <div class="row-actions">
              <button
                type="button"
                class="primary compose-btn"
                onclick={() => composeFor(c.username)}
              >
                Open chat
              </button>
              <button
                type="button"
                class="danger"
                disabled={pendingDelete === c.username}
                onclick={() => deleteContact(c.username, c.domain)}
              >
                {pendingDelete === c.username ? "Deleting…" : "Delete"}
              </button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .page-header {
    margin-bottom: 1rem;
  }
  h1 {
    margin: 0;
    font-size: 1.5rem;
  }
  h2 {
    margin: 1.75rem 0 0.6rem;
    font-size: 0.85rem;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-weight: 600;
  }
  h3 {
    margin: 0 0 0.5rem;
    font-size: 0.95rem;
  }
  .modes {
    display: flex;
    gap: 0.4rem;
    margin-bottom: 1rem;
  }
  .add-form,
  .preview {
    max-width: 720px;
    margin-bottom: 1.5rem;
    padding: 1.25rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-sm);
  }
  .actions {
    display: flex;
    gap: 0.4rem;
    margin-top: 0.5em;
  }
  .hint {
    display: block;
    margin-top: 0.3em;
    font-size: 11px;
  }
  .row-actions {
    display: flex;
    gap: 0.4em;
    align-items: center;
  }
  .row-actions .compose-btn,
  .row-actions .danger {
    font-size: 12.5px;
    padding: 0.35em 0.85em;
  }
  .status-info,
  .status-error {
    margin: 0.4em 0 0.8em;
    padding: 0.55em 0.85em;
    border-radius: var(--radius-sm);
    font-size: 13px;
  }
  .status-info {
    background: var(--pass-soft);
    color: var(--pass);
    border: 1px solid var(--pass-border);
  }
  .status-error {
    background: var(--danger-soft);
    color: var(--danger);
    border: 1px solid var(--danger-border);
  }
  .contact-cards {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    max-width: 880px;
  }
  .contact-card {
    display: flex;
    align-items: center;
    gap: 0.85rem;
    padding: 0.7rem 0.95rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-sm);
  }
  .avatar {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    border-radius: 50%;
    font-size: 14px;
    font-weight: 600;
    line-height: 1;
    user-select: none;
    flex-shrink: 0;
  }
  .contact-meta {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }
  .contact-name {
    font-size: 14px;
    font-weight: 600;
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .contact-keys {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
    font-size: 11px;
  }
  .contact-keys code {
    color: var(--muted);
  }
  @media (max-width: 700px) {
    .contact-card {
      flex-wrap: wrap;
    }
    .row-actions {
      width: 100%;
      justify-content: flex-end;
    }
  }
</style>
