<script lang="ts">
  import { onMount } from "svelte";
  import { get } from "svelte/store";
  import { activeIdentity } from "$lib/stores/identity";
  import { hydrateInbox } from "$lib/stores/inbox";
  import { refreshContacts } from "$lib/stores/contacts";
  import { intros, refreshIntros } from "$lib/stores/intros";
  import { api, isCommandError, type IntroView } from "$lib/api";

  let loading = $state<boolean>(false);
  let listError = $state<string>("");

  // Per-row busy + error state. Keyed by intro_id so each row can spin
  // / fail independently.
  let rowBusy = $state<Record<number, boolean>>({});
  let rowError = $state<Record<number, string>>({});
  let rowInfo = $state<Record<number, string>>({});

  // Trust UX 3-A: confirm dialog state. When set, the user is being
  // asked "Trust this sender as <addr>?" before we call intro_trust.
  // `address` is auto-resolved from intro.sender_label.
  let trustPrompt = $state<
    { intro_id: number; address: string } | null
  >(null);

  // Block UX: optional note attached to the local denylist row.
  let blockPrompt = $state<{ intro_id: number; note: string } | null>(null);

  onMount(async () => {
    await refresh();
  });

  async function refresh() {
    listError = "";
    if (!$activeIdentity) {
      intros.set([]);
      return;
    }
    loading = true;
    try {
      await refreshIntros();
    } catch (err) {
      listError = isCommandError(err) ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  function setRowError(id: number, msg: string) {
    rowError = { ...rowError, [id]: msg };
  }

  function clearRow(id: number) {
    const { [id]: _err, ...restErr } = rowError;
    const { [id]: _info, ...restInfo } = rowInfo;
    rowError = restErr;
    rowInfo = restInfo;
  }

  function setRowBusy(id: number, busy: boolean) {
    if (busy) {
      rowBusy = { ...rowBusy, [id]: true };
    } else {
      const { [id]: _, ...rest } = rowBusy;
      rowBusy = rest;
    }
  }

  async function doAccept(intro: IntroView) {
    clearRow(intro.intro_id);
    setRowBusy(intro.intro_id, true);
    // Snapshot identity so a late accept doesn't mutate a different
    // identity's badge if the user switches/locks mid-flight. The
    // intros store is shared with the topbar — same race the inbox
    // store guards against.
    const identityAtStart = get(activeIdentity)?.username ?? null;
    try {
      // intro_accept persists the message to disk in the same Tauri
      // call, so we only need to refresh the in-memory inbox here.
      const delivered = await api.introAccept(intro.intro_id);
      if (get(activeIdentity)?.username !== identityAtStart) return;
      if (!delivered) {
        setRowError(
          intro.intro_id,
          "intro already taken (another window may have accepted it)",
        );
        await refresh();
        return;
      }
      await hydrateInbox();
      if (get(activeIdentity)?.username !== identityAtStart) return;
      intros.update((rows) => rows.filter((i) => i.intro_id !== intro.intro_id));
    } catch (err) {
      if (get(activeIdentity)?.username !== identityAtStart) return;
      setRowError(
        intro.intro_id,
        isCommandError(err) ? err.message : String(err),
      );
    } finally {
      setRowBusy(intro.intro_id, false);
    }
  }

  function openTrust(intro: IntroView) {
    // The envelope verification already pinned the SPK against this
    // address at receive time. Pre-fill the dialog so the user just
    // confirms — they can still edit it if the envelope was absent
    // (no sender_label) or if they want to use an alias.
    trustPrompt = {
      intro_id: intro.intro_id,
      address: intro.sender_label ?? "",
    };
  }

  async function confirmTrust() {
    if (!trustPrompt) return;
    const { intro_id, address } = trustPrompt;
    const trimmed = address.trim().toLowerCase();
    if (!trimmed) {
      setRowError(intro_id, "address required to trust this sender");
      return;
    }
    clearRow(intro_id);
    setRowBusy(intro_id, true);
    trustPrompt = null;
    const identityAtStart = get(activeIdentity)?.username ?? null;
    try {
      // intro_trust persists the message to disk in the same Tauri
      // call (same atomicity reasoning as intro_accept).
      const delivered = await api.introTrust(intro_id, trimmed);
      if (get(activeIdentity)?.username !== identityAtStart) return;
      if (!delivered) {
        setRowError(intro_id, "intro already taken");
        await refresh();
        return;
      }
      await Promise.all([hydrateInbox(), refreshContacts()]);
      if (get(activeIdentity)?.username !== identityAtStart) return;
      intros.update((rows) => rows.filter((i) => i.intro_id !== intro_id));
    } catch (err) {
      if (get(activeIdentity)?.username !== identityAtStart) return;
      setRowError(
        intro_id,
        isCommandError(err) ? err.message : String(err),
      );
    } finally {
      setRowBusy(intro_id, false);
    }
  }

  function openBlock(intro: IntroView) {
    blockPrompt = { intro_id: intro.intro_id, note: "" };
  }

  async function confirmBlock() {
    if (!blockPrompt) return;
    const { intro_id, note } = blockPrompt;
    clearRow(intro_id);
    setRowBusy(intro_id, true);
    blockPrompt = null;
    const identityAtStart = get(activeIdentity)?.username ?? null;
    try {
      await api.introBlock(intro_id, note);
      if (get(activeIdentity)?.username !== identityAtStart) return;
      intros.update((rows) => rows.filter((i) => i.intro_id !== intro_id));
    } catch (err) {
      if (get(activeIdentity)?.username !== identityAtStart) return;
      setRowError(
        intro_id,
        isCommandError(err) ? err.message : String(err),
      );
    } finally {
      setRowBusy(intro_id, false);
    }
  }

  function fmtTs(unix: number): string {
    if (!unix) return "—";
    try {
      return new Date(unix * 1000).toLocaleString();
    } catch {
      return String(unix);
    }
  }

  function preview(s: string): string {
    const trimmed = s.replace(/\s+/g, " ").trim();
    if (trimmed.length <= 200) return trimmed;
    return trimmed.slice(0, 200) + "…";
  }
</script>

<section class="intro-page">
  <header>
    <h1>Intros</h1>
    <p class="subtitle">
      First-contact messages from senders you haven't pinned yet.
      Each row decrypted cleanly — the sender's
      Ed25519 signing key authenticated them — but you haven't told
      this device to trust them, so the message is held here until you
      decide.
    </p>
    <button type="button" class="refresh" onclick={refresh} disabled={loading}>
      {loading ? "Loading…" : "Refresh"}
    </button>
  </header>

  {#if listError}
    <p class="error">{listError}</p>
  {/if}

  {#if !$activeIdentity}
    <p class="empty">Unlock an identity to see pending intros.</p>
  {:else if $intros.length === 0 && !loading}
    <p class="empty">No pending intros.</p>
  {:else}
    <ul class="intro-list">
      {#each $intros as intro (intro.intro_id)}
        <li class="intro-row">
          <header class="row-head">
            <div class="who">
              {#if intro.sender_label}
                <span class="label">{intro.sender_label}</span>
                <span class="meta">verified envelope</span>
              {:else}
                <span class="label">Unknown sender</span>
                <span class="meta spk" title={intro.sender_spk_hex}>
                  {intro.sender_spk_hex.slice(0, 16)}…
                </span>
              {/if}
            </div>
            <div class="ts">
              Received {fmtTs(intro.received_at)}
              <br />
              Expires {fmtTs(intro.expires_at)}
            </div>
          </header>

          <pre class="body">{preview(intro.plaintext_utf8)}</pre>

          {#if rowError[intro.intro_id]}
            <p class="error">{rowError[intro.intro_id]}</p>
          {/if}

          <div class="actions">
            <button
              type="button"
              onclick={() => doAccept(intro)}
              disabled={rowBusy[intro.intro_id]}
              title="Deliver to inbox without pinning the sender"
            >
              Accept
            </button>
            <button
              type="button"
              class="primary"
              onclick={() => openTrust(intro)}
              disabled={rowBusy[intro.intro_id]}
              title="Deliver and pin as a trusted contact"
            >
              Trust
            </button>
            <button
              type="button"
              class="danger"
              onclick={() => openBlock(intro)}
              disabled={rowBusy[intro.intro_id]}
              title="Drop and add the sender to the local denylist"
            >
              Block
            </button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}

  {#if trustPrompt}
    <div class="modal-backdrop" role="presentation">
      <div
        class="modal"
        role="dialog"
        aria-modal="true"
        tabindex="-1"
      >
        <h2>Trust this sender?</h2>
        <p>
          We'll fetch the identity at this address, verify it signs with
          the same key as the queued message, and pin it as a contact.
        </p>
        <label>
          Sender address
          <input
            type="text"
            bind:value={trustPrompt.address}
            placeholder="user@host"
            autocomplete="off"
          />
        </label>
        <div class="modal-actions">
          <button type="button" onclick={() => (trustPrompt = null)}>
            Cancel
          </button>
          <button type="button" class="primary" onclick={confirmTrust}>
            Trust
          </button>
        </div>
      </div>
    </div>
  {/if}

  {#if blockPrompt}
    <div class="modal-backdrop" role="presentation">
      <div
        class="modal"
        role="dialog"
        aria-modal="true"
        tabindex="-1"
      >
        <h2>Block this sender?</h2>
        <p>
          Future messages from this Ed25519 key are dropped on this
          device. Add an optional note (local only) so you remember why.
        </p>
        <label>
          Note
          <textarea
            bind:value={blockPrompt.note}
            placeholder="e.g. spam, wrong person, …"
            rows="3"
          ></textarea>
        </label>
        <div class="modal-actions">
          <button type="button" onclick={() => (blockPrompt = null)}>
            Cancel
          </button>
          <button type="button" class="danger" onclick={confirmBlock}>
            Block
          </button>
        </div>
      </div>
    </div>
  {/if}
</section>

<style>
  .intro-page {
    max-width: 760px;
    margin: 0 auto;
    padding: 1.5rem 1rem 3rem;
  }
  header {
    margin-bottom: 1rem;
  }
  h1 {
    margin: 0 0 0.25rem;
  }
  .subtitle {
    color: var(--fg-muted, #666);
    font-size: 0.9rem;
    margin: 0 0 0.5rem;
  }
  .refresh {
    margin-top: 0.5rem;
  }
  .error {
    color: var(--danger, #b91c1c);
    margin: 0.5rem 0;
  }
  .empty {
    color: var(--fg-muted, #666);
    text-align: center;
    padding: 2rem 0;
  }
  .intro-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .intro-row {
    border: 1px solid var(--border, #ddd);
    border-radius: 6px;
    padding: 0.75rem;
    background: var(--bg-elevated, #fff);
  }
  .row-head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 1rem;
    margin-bottom: 0.5rem;
  }
  .who {
    display: flex;
    flex-direction: column;
  }
  .label {
    font-weight: 600;
  }
  .meta {
    font-size: 0.8rem;
    color: var(--fg-muted, #666);
  }
  .meta.spk {
    font-family: monospace;
  }
  .ts {
    font-size: 0.8rem;
    color: var(--fg-muted, #666);
    text-align: right;
  }
  .body {
    background: var(--bg-muted, #f4f4f4);
    border-radius: 4px;
    padding: 0.5rem;
    margin: 0.5rem 0;
    font-family: inherit;
    font-size: 0.9rem;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 8rem;
    overflow: auto;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
    justify-content: flex-end;
  }
  .actions button.primary {
    background: var(--accent, #2563eb);
    color: white;
  }
  .actions button.danger {
    background: var(--danger, #b91c1c);
    color: white;
  }
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .modal {
    background: var(--bg-elevated, #fff);
    border-radius: 8px;
    padding: 1.25rem;
    max-width: 480px;
    width: calc(100% - 2rem);
    box-shadow: 0 10px 40px rgba(0, 0, 0, 0.2);
  }
  .modal h2 {
    margin: 0 0 0.5rem;
  }
  .modal p {
    margin: 0 0 0.75rem;
    color: var(--fg-muted, #666);
    font-size: 0.9rem;
  }
  .modal label {
    display: block;
    margin-bottom: 0.75rem;
    font-size: 0.85rem;
  }
  .modal input,
  .modal textarea {
    width: 100%;
    margin-top: 0.25rem;
    padding: 0.4rem;
    border: 1px solid var(--border, #ddd);
    border-radius: 4px;
    font-family: inherit;
    font-size: 0.9rem;
    box-sizing: border-box;
  }
  .modal-actions {
    display: flex;
    gap: 0.5rem;
    justify-content: flex-end;
  }
  .modal-actions .primary {
    background: var(--accent, #2563eb);
    color: white;
  }
  .modal-actions .danger {
    background: var(--danger, #b91c1c);
    color: white;
  }
</style>
