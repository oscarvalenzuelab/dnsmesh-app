<script lang="ts">
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import { activeIdentity } from "$lib/stores/identity";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import { inbox, hydrateInbox } from "$lib/stores/inbox";
  import { api, isCommandError, type ContactView, type InboxRow } from "$lib/api";
  import {
    avatarBackground,
    avatarForeground,
    avatarInitials,
  } from "$lib/avatar";

  let recipient = $state<string>("");
  let body = $state<string>("");
  let busy = $state<boolean>(false);
  let error = $state<string>("");
  let success = $state<string>("");
  // Recipient picker — open by default when no contact is preselected.
  let pickerOpen = $state<boolean>(false);

  // Reply context, populated from `reply_to=<msg_id_hex>` deep-link.
  let replyToFullId = $state<string>("");
  let replyToShort = $state<string>("");
  let replyToSender = $state<string>("");

  // Original message looked up from the persistent inbox; null if the
  // id doesn't resolve (deleted, hand-crafted deep link, etc.).
  let replyToMessage = $derived<InboxRow | null>(
    replyToFullId
      ? $inbox.find(
          (m) => m.msg_id_hex.toLowerCase() === replyToFullId.toLowerCase(),
        ) ?? null
      : null,
  );

  onMount(async () => {
    // Refresh contacts + inbox first so deep-link selection and reply
    // lookup both resolve on a fresh page load.
    await refreshContacts();
    await hydrateInbox();
    const toParam = page.url.searchParams.get("to");
    if (toParam) {
      // Match the contacts store's lowercase normalization.
      recipient = toParam.trim().toLowerCase();
    }
    const replyTo = page.url.searchParams.get("reply_to");
    if (replyTo) {
      replyToFullId = replyTo;
      replyToShort = replyTo.slice(0, 12);
      replyToSender = recipient;
    }
    if (!recipient) pickerOpen = true;
  });

  function formatReplyTimestamp(ts: number): string {
    const d = new Date(ts * 1000);
    return d.toLocaleString();
  }

  const selectedContact = $derived<ContactView | null>(
    recipient
      ? $contacts.find((c) => c.username === recipient) ?? null
      : null,
  );

  function pickRecipient(username: string) {
    recipient = username;
    pickerOpen = false;
    error = "";
  }

  async function send() {
    error = "";
    success = "";
    if (!recipient.trim()) {
      error = "Pick a recipient.";
      return;
    }
    if (!body) {
      error = "Message body must not be empty.";
      return;
    }
    busy = true;
    try {
      const result = await api.sendMessage(recipient.trim(), body);
      success = `Sent. msg_id=${result.msg_id_hex}`;
      body = "";
    } catch (err) {
      // `contact_not_found` usually means a stale contact list or a
      // typo; nudge to Contacts rather than echoing the SDK string.
      if (isCommandError(err) && err.kind === "contact_not_found") {
        const u =
          (err.details &&
            typeof err.details === "object" &&
            (err.details as Record<string, unknown>).username) ||
          recipient.trim();
        error = `No pinned contact named "${u}". Add them on the Contacts page first.`;
      } else {
        error = isCommandError(err) ? err.message : String(err);
      }
    } finally {
      busy = false;
    }
  }

  const charCount = $derived(body.length);
</script>

<section>
  <header class="page-header">
    <h1>Compose</h1>
  </header>

  {#if !$activeIdentity}
    <div class="placeholder">
      <p class="muted">
        Unlock an identity from <a href="/identities">Identities</a> first.
      </p>
    </div>
  {:else if !$activeIdentity.publish_configured}
    <div class="placeholder">
      <p class="warn">
        Sending requires a TSIG-signed publish destination. Configure it in
        <a href="/settings">Settings</a>.
      </p>
    </div>
  {:else}
    {#if replyToShort}
      <aside class="reply-context" aria-label="Original message">
        <header class="reply-context-header">
          <span class="reply-label">Replying to</span>
          {#if replyToSender}
            <span class="reply-from"><strong>{replyToSender}</strong></span>
          {/if}
          {#if replyToMessage}
            <span class="reply-when muted">
              {formatReplyTimestamp(replyToMessage.timestamp)}
            </span>
          {/if}
          <span class="reply-id muted">
            <code>{replyToShort}</code>
          </span>
        </header>
        {#if replyToMessage}
          <blockquote class="reply-quote">{replyToMessage.plaintext_utf8}</blockquote>
        {:else}
          <p class="reply-missing muted">
            Original message no longer available (deleted or not yet
            synced to this identity's inbox).
          </p>
        {/if}
      </aside>
    {/if}
    <form
      class="compose-card"
      onsubmit={(e) => {
        e.preventDefault();
        send();
      }}
    >
      <div class="field">
        <label class="field-label" for="recipient-section">To</label>
        <div id="recipient-section" class="recipient-row">
          {#if selectedContact && !pickerOpen}
            {@const c = selectedContact}
            {@const initials = avatarInitials(
              c.username,
              c.ed25519_signing_public_key_hex,
            )}
            <span class="recipient-chip">
              <span
                class="avatar"
                style="background:{avatarBackground(
                  c.ed25519_signing_public_key_hex,
                )};color:{avatarForeground(
                  c.ed25519_signing_public_key_hex,
                )};"
                aria-hidden="true"
              >{initials}</span>
              <span class="chip-text">
                <span class="chip-name">{c.username}</span>
                <span class="chip-domain">@{c.domain}</span>
              </span>
            </span>
            <button
              type="button"
              class="link-button"
              onclick={() => (pickerOpen = true)}
            >
              Change recipient
            </button>
          {:else if $contacts.length === 0}
            <p class="muted small no-contacts-hint">
              No pinned contacts yet. <a href="/contacts">Add a contact</a> first.
            </p>
          {:else}
            <ul class="contact-picker">
              {#each $contacts as c (c.username + "@" + c.domain)}
                {@const initials = avatarInitials(
                  c.username,
                  c.ed25519_signing_public_key_hex,
                )}
                <li>
                  <button
                    type="button"
                    class="contact-pick"
                    class:selected={recipient === c.username}
                    onclick={() => pickRecipient(c.username)}
                  >
                    <span
                      class="avatar"
                      style="background:{avatarBackground(
                        c.ed25519_signing_public_key_hex,
                      )};color:{avatarForeground(
                        c.ed25519_signing_public_key_hex,
                      )};"
                      aria-hidden="true"
                    >{initials}</span>
                    <span class="contact-pick-text">
                      <span class="chip-name">{c.username}</span>
                      <span class="chip-domain">@{c.domain}</span>
                    </span>
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>

      <div class="field">
        <div class="field-label-row">
          <label class="field-label" for="body-textarea">Message</label>
          <span class="char-counter muted small" class:dim={charCount === 0}>
            {charCount} char{charCount === 1 ? "" : "s"}
          </span>
        </div>
        <textarea
          id="body-textarea"
          class="body-textarea"
          bind:value={body}
          rows="10"
          placeholder="Write your message in plain text…"
        ></textarea>
      </div>

      {#if error}
        <p class="error inline-status">{error}</p>
      {/if}
      {#if success}
        <p class="pass inline-status">{success}</p>
      {/if}

      <div class="compose-footer">
        <span class="muted small footer-hint">
          Plain text only. End-to-end encrypted on send.
        </span>
        <button
          class="primary send-button"
          type="submit"
          disabled={busy || !recipient || !body}
        >
          {busy ? "Sending…" : "Send"}
        </button>
      </div>
    </form>
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
  .placeholder {
    padding: 2rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    text-align: center;
    max-width: 720px;
  }

  .compose-card {
    max-width: 720px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 1.25rem 1.4rem;
    box-shadow: var(--shadow-sm);
    display: flex;
    flex-direction: column;
    gap: 1.1rem;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .field-label-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .field-label {
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .char-counter.dim {
    opacity: 0.6;
  }

  .recipient-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
    min-height: 38px;
  }
  .recipient-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.3em 0.85em 0.3em 0.35em;
    background: var(--accent-soft);
    border: 1px solid var(--border-accent);
    border-radius: 999px;
  }
  .avatar {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    font-size: 12px;
    font-weight: 600;
    line-height: 1;
    user-select: none;
  }
  .chip-text {
    display: inline-flex;
    align-items: baseline;
    gap: 0.15rem;
    font-size: 13px;
  }
  .chip-name {
    font-weight: 600;
    color: var(--text-strong);
  }
  .chip-domain {
    color: var(--muted);
  }
  .link-button {
    background: transparent;
    color: var(--accent);
    border: none;
    padding: 0;
    text-decoration: underline;
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
  }
  .link-button:hover:not(:disabled) {
    color: var(--accent-strong);
    background: transparent;
  }
  .no-contacts-hint {
    margin: 0;
  }

  .contact-picker {
    list-style: none;
    margin: 0;
    padding: 0.3rem;
    width: 100%;
    background: var(--surface-alt);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 260px;
    overflow-y: auto;
  }
  .contact-pick {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    width: 100%;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    padding: 0.45em 0.55em;
    font: inherit;
    cursor: pointer;
    color: inherit;
  }
  .contact-pick:hover:not(:disabled) {
    background: var(--accent-softer);
    border-color: transparent;
    color: inherit;
  }
  .contact-pick.selected {
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .contact-pick-text {
    display: inline-flex;
    align-items: baseline;
    gap: 0.15rem;
    font-size: 13px;
    min-width: 0;
  }

  .body-textarea {
    font: inherit;
    font-size: 14px;
    line-height: 1.55;
    padding: 0.85em 1em;
    resize: vertical;
    min-height: 220px;
    border-radius: var(--radius-md);
  }

  .reply-context {
    max-width: 720px;
    margin: 0 0 1rem;
    padding: 0.7em 0.95em 0.85em;
    background: var(--accent-softer);
    border: 1px solid var(--border-accent);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius-md);
    font-size: 13px;
  }
  .reply-context-header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.6em;
    margin-bottom: 0.5em;
    font-size: 12px;
  }
  .reply-label {
    color: var(--muted-strong);
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
  }
  .reply-from {
    font-size: 13px;
    color: var(--text);
  }
  .reply-when,
  .reply-id {
    margin-left: auto;
    font-size: 11.5px;
  }
  .reply-id code {
    background: transparent;
    padding: 0;
    color: var(--muted-strong);
  }
  .reply-quote {
    margin: 0;
    padding: 0;
    color: var(--text);
    white-space: pre-wrap;
    word-wrap: break-word;
    font-family: inherit;
    font-size: 13px;
    line-height: 1.45;
    max-height: 12em;
    overflow-y: auto;
  }
  .reply-missing {
    margin: 0;
    font-style: italic;
    font-size: 12.5px;
  }

  .inline-status {
    margin: 0;
    padding: 0.5em 0.85em;
    border-radius: var(--radius-sm);
    font-size: 13px;
  }
  .inline-status.error {
    background: var(--danger-soft);
    border: 1px solid var(--danger-border);
    color: var(--danger);
  }
  .inline-status.pass {
    background: var(--pass-soft);
    border: 1px solid var(--pass-border);
    color: var(--pass);
  }

  .compose-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding-top: 0.6rem;
    border-top: 1px solid var(--border-soft);
  }
  .footer-hint {
    margin: 0;
  }
  .send-button {
    padding: 0.55em 1.4em;
    font-weight: 600;
  }

  @media (max-width: 700px) {
    .compose-card {
      padding: 1rem 1rem;
    }
    .compose-footer {
      flex-direction: column-reverse;
      align-items: stretch;
    }
    .send-button {
      width: 100%;
    }
    .footer-hint {
      text-align: center;
    }
  }
</style>
