<script lang="ts">
  // Inbox page. Two-pane mailbox; persistence lives in the per-identity
  // `inbox.jsonl` so the SDK's replay cache doesn't eat messages.
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { activeIdentity } from "$lib/stores/identity";
  import {
    inbox,
    inboxBusy,
    inboxError,
    pollInbox,
    hydrateInbox,
    markRead,
    markAllRead,
    deleteMessage,
    deleteMessages,
  } from "$lib/stores/inbox";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import {
    api,
    isCommandError,
    type ReceiveDiagnostic,
  } from "$lib/api";
  import {
    avatarBackground,
    avatarForeground,
    avatarInitials,
  } from "$lib/avatar";

  // Selected message id (msg_id_hex); null when nothing is selected.
  // Selecting also marks the message read.
  let openMsgId = $state<string | null>(null);

  // Diagnostic panel — opens via the overflow menu.
  let diag = $state<ReceiveDiagnostic | null>(null);
  let diagBusy = $state<boolean>(false);
  let diagError = $state<string>("");

  // Overflow menu (Diagnose, Mark all read).
  let menuOpen = $state<boolean>(false);

  // Selected msg_id_hex values for bulk actions. Reassign the binding
  // on mutation so Svelte 5 picks up the change.
  let selected = $state<Set<string>>(new Set());

  // Below the breakpoint, the two-column layout collapses to one and
  // selecting a message swaps the list for the reading pane.
  let isNarrow = $state<boolean>(false);
  const NARROW_BREAKPOINT_PX = 700;

  function syncNarrow() {
    if (typeof window === "undefined") return;
    isNarrow = window.innerWidth < NARROW_BREAKPOINT_PX;
  }

  function selectMessage(msgId: string) {
    openMsgId = msgId;
    void markRead(msgId);
  }

  function closeDetail() {
    openMsgId = null;
  }

  async function refresh() {
    await pollInbox();
  }

  // The diagnostic call drives a real `receive_messages`, so persist
  // anything new via `pollInbox` before rendering the panel.
  async function runDiagnostic() {
    diagError = "";
    diag = null;
    diagBusy = true;
    menuOpen = false;
    try {
      diag = await api.receiveMessagesDiagnostic();
      // Diagnostic flipped the SDK replay cache; mirror to disk.
      await pollInbox();
    } catch (err) {
      diagError = isCommandError(err) ? err.message : String(err);
    } finally {
      diagBusy = false;
    }
  }

  function dismissDiagnostic() {
    diag = null;
    diagError = "";
  }

  async function onMarkAllRead() {
    menuOpen = false;
    await markAllRead();
  }

  // Two-click delete (window.confirm() can be silently suppressed in
  // some Tauri 2 webviews). First click arms, second commits within 4s.
  let pendingDeleteId = $state<string>("");
  let pendingBulkDelete = $state<boolean>(false);
  let pendingTimeout: ReturnType<typeof setTimeout> | null = null;

  function armPending(key: string) {
    if (pendingTimeout) clearTimeout(pendingTimeout);
    if (key === "__bulk__") {
      pendingBulkDelete = true;
      pendingDeleteId = "";
    } else {
      pendingDeleteId = key;
      pendingBulkDelete = false;
    }
    pendingTimeout = setTimeout(() => {
      pendingDeleteId = "";
      pendingBulkDelete = false;
      pendingTimeout = null;
    }, 4000);
  }

  function clearPending() {
    if (pendingTimeout) clearTimeout(pendingTimeout);
    pendingTimeout = null;
    pendingDeleteId = "";
    pendingBulkDelete = false;
  }

  async function onDeleteOne(msgIdHex: string) {
    if (pendingDeleteId !== msgIdHex) {
      armPending(msgIdHex);
      return;
    }
    clearPending();
    const ok = await deleteMessage(msgIdHex);
    if (ok) {
      if (selected.has(msgIdHex)) {
        selected.delete(msgIdHex);
        selected = new Set(selected);
      }
      if (openMsgId === msgIdHex) openMsgId = null;
    }
  }

  async function onDeleteSelected() {
    const ids = Array.from(selected);
    if (ids.length === 0) return;
    if (!pendingBulkDelete) {
      armPending("__bulk__");
      return;
    }
    clearPending();
    const ok = await deleteMessages(ids);
    if (ok) {
      selected = new Set();
      if (openMsgId !== null && ids.includes(openMsgId)) openMsgId = null;
    }
  }

  function toggleSelect(msgId: string, ev: Event) {
    // The checkbox sits inside the row; stop propagation so the row
    // open handler doesn't also fire.
    ev.stopPropagation();
    if (selected.has(msgId)) {
      selected.delete(msgId);
    } else {
      selected.add(msgId);
    }
    selected = new Set(selected);
  }

  function clearSelection() {
    selected = new Set();
  }

  onMount(() => {
    syncNarrow();
    window.addEventListener("resize", syncNarrow);
    refreshContacts();
    if ($activeIdentity) {
      // Hydrate from disk first so older messages render immediately.
      void hydrateInbox().then(() => refresh());
    }
    return () => {
      window.removeEventListener("resize", syncNarrow);
    };
  });

  function senderLabel(spk: string): string {
    const match = $contacts.find(
      (c) => c.ed25519_signing_public_key_hex === spk,
    );
    if (match) {
      return `${match.username}@${match.domain}`;
    }
    return spk.slice(0, 16) + "…";
  }

  function contactForSpk(spk: string) {
    return (
      $contacts.find((c) => c.ed25519_signing_public_key_hex === spk) ?? null
    );
  }

  // Compact timestamp for inbox rows: today → HH:MM, older → MMM D.
  function formatRowTimestamp(ts: number): string {
    if (!ts) return "—";
    const d = new Date(ts * 1000);
    const now = new Date();
    const sameDay =
      d.getFullYear() === now.getFullYear() &&
      d.getMonth() === now.getMonth() &&
      d.getDate() === now.getDate();
    if (sameDay) {
      return d.toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
      });
    }
    return d.toLocaleDateString([], { month: "short", day: "numeric" });
  }

  // Reading-pane header timestamp.
  function formatHeaderTimestamp(ts: number): string {
    if (!ts) return "—";
    const d = new Date(ts * 1000);
    const now = new Date();
    const sameDay =
      d.getFullYear() === now.getFullYear() &&
      d.getMonth() === now.getMonth() &&
      d.getDate() === now.getDate();
    if (sameDay) {
      return (
        "Today, " +
        d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
      );
    }
    return d.toLocaleString([], {
      month: "short",
      day: "numeric",
      year: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  // UTC ISO-ish format for the message-details disclosure.
  function formatTimestampDetail(ts: number): string {
    if (!ts) return "—";
    const d = new Date(ts * 1000);
    const yyyy = d.getUTCFullYear();
    const mm = String(d.getUTCMonth() + 1).padStart(2, "0");
    const dd = String(d.getUTCDate()).padStart(2, "0");
    const hh = String(d.getUTCHours()).padStart(2, "0");
    const mi = String(d.getUTCMinutes()).padStart(2, "0");
    const iso = `${yyyy}-${mm}-${dd} ${hh}:${mi}Z`;
    return `${iso} · ${relativeFromNow(ts)}`;
  }

  function relativeFromNow(ts: number): string {
    const deltaSec = Math.round(Date.now() / 1000 - ts);
    const abs = Math.abs(deltaSec);
    const suffix = deltaSec >= 0 ? "ago" : "from now";
    if (abs < 60) return `${abs} second${abs === 1 ? "" : "s"} ${suffix}`;
    const mins = Math.round(abs / 60);
    if (mins < 60) return `${mins} minute${mins === 1 ? "" : "s"} ${suffix}`;
    const hours = Math.round(abs / 3600);
    if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ${suffix}`;
    const days = Math.round(abs / 86400);
    return `${days} day${days === 1 ? "" : "s"} ${suffix}`;
  }

  // First ~120 chars, single-lined, with ellipsis on overflow.
  function snippet(text: string): string {
    const flat = text.replace(/\s+/g, " ").trim();
    if (flat.length <= 120) return flat;
    return flat.slice(0, 117) + "…";
  }

  // Prefill Compose with `to=<username>` and `reply_to=<msg_id_hex>`.
  // No auto-quoting — payload semantics depend on type.
  function reply(msg: { sender_signing_pk_hex: string; msg_id_hex: string }) {
    const c = contactForSpk(msg.sender_signing_pk_hex);
    if (!c) return;
    const params = new URLSearchParams({
      to: c.username,
      reply_to: msg.msg_id_hex,
    });
    void goto(`/compose?${params.toString()}`);
  }

  // Jump to Contacts pre-filled with the unpinned sender's Ed25519
  // SPK; the user fills in username/domain via a fetch there.
  function addUnpinnedSender(spkHex: string) {
    const params = new URLSearchParams({ ed25519_spk_hex: spkHex });
    void goto(`/contacts?${params.toString()}`);
  }

  // Heuristic for monospace rendering: triple-backtick fences or
  // 3+ leading spaces on the first non-empty line.
  function looksLikeCode(text: string): boolean {
    if (!text) return false;
    if (text.includes("```")) return true;
    const first = text.split("\n").find((l) => l.trim().length > 0) ?? "";
    return /^ {3,}\S/.test(first);
  }

  // Newest first. Read messages stay visible; "read" only changes
  // visual weight.
  const visibleInbox = $derived(
    [...$inbox].sort((a, b) => b.timestamp - a.timestamp),
  );

  const unreadCount = $derived(
    $inbox.reduce((acc, m) => (m.read ? acc : acc + 1), 0),
  );

  const selectedCount = $derived(selected.size);

  const allVisibleSelected = $derived(
    visibleInbox.length > 0 &&
      visibleInbox.every((m) => selected.has(m.msg_id_hex)),
  );

  const someVisibleSelected = $derived(
    visibleInbox.some((m) => selected.has(m.msg_id_hex)) && !allVisibleSelected,
  );

  // Look the open message up from the visible inbox so refresh /
  // delete / switch keep the reading pane in sync.
  const openMsg = $derived(
    openMsgId === null
      ? null
      : visibleInbox.find((m) => m.msg_id_hex === openMsgId) ?? null,
  );

  const showListPane = $derived(!isNarrow || openMsg === null);
  const showReadingPane = $derived(!isNarrow || openMsg !== null);

  // Decision-label helper for the diagnostic table.
  function decisionLabel(d: string): { text: string; cls: string } {
    switch (d) {
      case "deliverable_pinned":
        return { text: "Delivered (pinned)", cls: "pass" };
      case "deliverable_tofu":
        return { text: "Delivered (TOFU)", cls: "pass" };
      case "quarantine_intro":
        return { text: "Quarantined (un-pinned sender)", cls: "warn" };
      case "expired":
        return { text: "Expired", cls: "muted" };
      case "recipient_mismatch":
        return { text: "Recipient mismatch", cls: "error" };
      case "signature_invalid":
        return { text: "Signature invalid", cls: "error" };
      default:
        return { text: d, cls: "muted" };
    }
  }
</script>

<section class="inbox-page">
  <header class="page-header">
    <div class="header-titles">
      <h1>Inbox</h1>
      {#if unreadCount > 0}
        <span class="unread-pill">{unreadCount} unread</span>
      {/if}
    </div>
    <div class="actions">
      <button
        class="primary"
        disabled={!$activeIdentity || $inboxBusy}
        onclick={refresh}
      >
        {$inboxBusy ? "Polling…" : "Refresh"}
      </button>
      <div class="overflow-wrap">
        <button
          type="button"
          class="overflow-button"
          aria-haspopup="true"
          aria-expanded={menuOpen}
          disabled={!$activeIdentity}
          onclick={() => (menuOpen = !menuOpen)}
          title="More actions"
        >
          ···
        </button>
        {#if menuOpen}
          <ul class="overflow-menu" role="menu">
            <li>
              <button
                type="button"
                onclick={runDiagnostic}
                disabled={diagBusy || $inboxBusy}
              >
                {diagBusy ? "Diagnosing…" : "Diagnose"}
              </button>
            </li>
            <li>
              <button
                type="button"
                onclick={onMarkAllRead}
                disabled={unreadCount === 0}
              >
                Mark all read
              </button>
            </li>
          </ul>
        {/if}
      </div>
    </div>
  </header>

  {#if !$activeIdentity}
    <div class="placeholder">
      <p class="muted">
        Unlock an identity from <a href="/identities">Identities</a> to receive
        messages.
      </p>
    </div>
  {:else}
    {#if $inboxError}
      <p class="error banner">Receive failed: {$inboxError}</p>
    {/if}

    {#if diagError}
      <p class="error banner">Diagnostic failed: {diagError}</p>
    {/if}

    {#if diag}
      <div class="diagnostic">
        <div class="diagnostic-header">
          <h3>Receive diagnostic</h3>
          <button type="button" class="dismiss" onclick={dismissDiagnostic}>
            Dismiss
          </button>
        </div>
        <table class="kv">
          <tbody>
            <tr><th>Identity</th><td>{diag.identity}</td></tr>
            <tr>
              <th>Recipient ID</th>
              <td><code>{diag.recipient_id_hex}</code></td>
            </tr>
            <tr>
              <th>Zones polled</th>
              <td>
                {#each diag.zones_polled as z}
                  <code class="zone-pill">{z}</code>
                {/each}
                <span class="muted small">
                  ({diag.slots_per_zone} slots each =
                  {diag.zones_polled.length * diag.slots_per_zone} lookups)
                </span>
              </td>
            </tr>
            <tr>
              <th>Pinned contacts</th>
              <td>
                {diag.pinned_contacts}
                {#if diag.tofu_mode}
                  <span class="warn small">
                    · TOFU mode — any verified manifest is accepted
                  </span>
                {/if}
              </td>
            </tr>
            <tr>
              <th>Manifests on wire</th>
              <td>{diag.manifests_found.length}</td>
            </tr>
            <tr>
              <th>Inbox after walk</th>
              <td>
                {#if diag.inbox_count === 0 && diag.manifests_found.length > 0}
                  <span class="warn">{diag.inbox_count}</span>
                {:else}
                  {diag.inbox_count}
                {/if}
              </td>
            </tr>
          </tbody>
        </table>

        {#if diag.notes.length > 0}
          <p class="muted small"><strong>Notes:</strong></p>
          <ul class="muted small notes">
            {#each diag.notes as note}
              <li>{note}</li>
            {/each}
          </ul>
        {/if}

        {#if diag.manifests_found.length === 0}
          <p class="muted small">
            No slot manifests visible under any polled zone. If a sender
            insists they sent a message: cross-check their identity
            domain matches one of the zones above (the sender publishes
            under <em>their own</em> zone, not yours), and verify
            <code>Diagnose</code> on the SENDER side reports a successful
            publish.
          </p>
        {:else}
          <table class="diag-table">
            <thead>
              <tr>
                <th>Zone</th>
                <th>Slot</th>
                <th>Sender</th>
                <th>msg_id</th>
                <th>Decision</th>
                <th>Note</th>
              </tr>
            </thead>
            <tbody>
              {#each diag.manifests_found as m, i (i)}
                {@const label = decisionLabel(m.decision)}
                <tr>
                  <td><code>{m.zone}</code></td>
                  <td>{m.slot}</td>
                  <td>
                    {#if m.sender_spk_hex}
                      <code title={m.sender_spk_hex}>
                        {senderLabel(m.sender_spk_hex)}
                      </code>
                    {:else}
                      <span class="muted">—</span>
                    {/if}
                  </td>
                  <td>
                    {#if m.msg_id_hex}
                      <code>{m.msg_id_hex.slice(0, 12)}…</code>
                    {:else}
                      <span class="muted">—</span>
                    {/if}
                  </td>
                  <td><span class={label.cls}>{label.text}</span></td>
                  <td class="diag-note">{m.note}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>
    {/if}

    <div
      class="mailbox"
      class:single-pane={isNarrow}
    >
      {#if showListPane}
        <aside class="list-pane">
          {#if visibleInbox.length === 0}
            <div class="placeholder">
              <p class="muted empty-hint">No messages yet.</p>
              <p class="muted small">
                Click Refresh above to check for incoming.
              </p>
            </div>
          {:else}
            {#if selectedCount > 0}
              <div class="bulk-toolbar" role="toolbar" aria-label="Bulk actions">
                <span class="bulk-count">{selectedCount} selected</span>
                <button
                  type="button"
                  class="danger"
                  onclick={onDeleteSelected}
                >
                  {pendingBulkDelete
                    ? `Click again to confirm (${selectedCount})`
                    : `Delete ${selectedCount}`}
                </button>
                <button
                  type="button"
                  class="bulk-clear"
                  onclick={clearSelection}
                >
                  Clear
                </button>
              </div>
            {/if}
            <div class="list-meta">
              <label class="select-all">
                <input
                  type="checkbox"
                  checked={allVisibleSelected}
                  indeterminate={someVisibleSelected}
                  onchange={() => {
                    if (allVisibleSelected) {
                      for (const m of visibleInbox) selected.delete(m.msg_id_hex);
                    } else {
                      for (const m of visibleInbox) selected.add(m.msg_id_hex);
                    }
                    selected = new Set(selected);
                  }}
                  aria-label="Select all visible messages"
                />
                <span>Select all</span>
              </label>
              <span class="muted small">{visibleInbox.length} total</span>
            </div>
            <ul class="msg-list">
              {#each visibleInbox as msg (msg.msg_id_hex)}
                {@const isOpen = openMsgId === msg.msg_id_hex}
                {@const isSelected = selected.has(msg.msg_id_hex)}
                {@const senderContact = contactForSpk(msg.sender_signing_pk_hex)}
                {@const senderName = senderContact
                  ? senderContact.username
                  : null}
                {@const senderFull = senderContact
                  ? `${senderContact.username}@${senderContact.domain}`
                  : "(unpinned sender)"}
                {@const initials = avatarInitials(
                  senderName,
                  msg.sender_signing_pk_hex,
                )}
                {@const avatarBg = avatarBackground(msg.sender_signing_pk_hex)}
                {@const avatarFg = avatarForeground(msg.sender_signing_pk_hex)}
                <li
                  class="msg-item"
                  class:open={isOpen}
                  class:unread={!msg.read}
                  class:selected={isSelected}
                >
                  <!-- div + role="button" so nested checkbox + Delete
                       work reliably across webviews. -->
                  <div
                    class="msg-row"
                    role="button"
                    tabindex="0"
                    aria-pressed={isOpen}
                    onclick={() => selectMessage(msg.msg_id_hex)}
                    onkeydown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        selectMessage(msg.msg_id_hex);
                      }
                    }}
                  >
                    <!-- Avatar swaps to checkbox on hover/select. -->
                    <span
                      class="avatar-slot"
                      class:has-checkbox={isSelected}
                    >
                      <span
                        class="avatar"
                        style="background:{avatarBg};color:{avatarFg};"
                        aria-hidden="true"
                      >{initials}</span>
                      <span class="check-overlay">
                        <input
                          type="checkbox"
                          checked={isSelected}
                          onclick={(e) => toggleSelect(msg.msg_id_hex, e)}
                          aria-label="Select message"
                        />
                      </span>
                    </span>
                    <span class="msg-body">
                      <span class="msg-line-1">
                        <span class="sender">{senderFull}</span>
                        <span class="ts">{formatRowTimestamp(msg.timestamp)}</span>
                      </span>
                      <span class="snippet">{snippet(msg.plaintext_utf8)}</span>
                    </span>
                    <span class="trailing-dot" aria-hidden="true">
                      <span class="unread-dot" class:on={!msg.read}>●</span>
                    </span>
                    <!-- stopPropagation so Delete doesn't also open the row. -->
                    <button
                      type="button"
                      class="row-delete danger"
                      class:armed={pendingDeleteId === msg.msg_id_hex}
                      title={pendingDeleteId === msg.msg_id_hex
                        ? "Click again to confirm"
                        : "Delete this message"}
                      aria-label="Delete this message"
                      onclick={(e) => {
                        e.stopPropagation();
                        void onDeleteOne(msg.msg_id_hex);
                      }}
                      onkeydown={(e) => {
                        // Stop Enter/Space from also opening the row.
                        if (e.key === "Enter" || e.key === " ") {
                          e.stopPropagation();
                        }
                      }}
                    >
                      {pendingDeleteId === msg.msg_id_hex ? "Confirm?" : "Delete"}
                    </button>
                  </div>
                </li>
              {/each}
            </ul>
          {/if}
        </aside>
      {/if}

      {#if showReadingPane}
        <article class="read-pane">
          {#if openMsg === null}
            <div class="reading-empty">
              <div class="reading-empty-bubble" aria-hidden="true">
                <span>No message selected</span>
              </div>
              <p class="muted">
                Pick a message from the list to read it here.
              </p>
            </div>
          {:else}
            {@const msg = openMsg}
            {@const senderContact = contactForSpk(msg.sender_signing_pk_hex)}
            {@const senderName = senderContact
              ? senderContact.username
              : null}
            {@const senderFull = senderContact
              ? `${senderContact.username}@${senderContact.domain}`
              : "(unpinned sender)"}
            {@const senderShort = senderContact
              ? senderContact.username
              : msg.sender_signing_pk_hex.slice(0, 16) + "…"}
            {@const initials = avatarInitials(
              senderName,
              msg.sender_signing_pk_hex,
            )}
            {@const avatarBg = avatarBackground(msg.sender_signing_pk_hex)}
            {@const avatarFg = avatarForeground(msg.sender_signing_pk_hex)}
            {@const isCode = looksLikeCode(msg.plaintext_utf8)}
            <header class="read-header">
              <div class="read-header-left">
                {#if isNarrow}
                  <button
                    type="button"
                    class="back-button"
                    onclick={closeDetail}
                    title="Back to inbox"
                    aria-label="Back to inbox"
                  >
                    ←
                  </button>
                {/if}
                <span
                  class="avatar avatar-lg"
                  style="background:{avatarBg};color:{avatarFg};"
                  aria-hidden="true"
                >{initials}</span>
                <div class="read-sender">
                  <div class="read-sender-name">{senderShort}</div>
                  <div class="read-sender-meta">
                    <span class="muted">{senderFull}</span>
                    <span class="dot-sep" aria-hidden="true">·</span>
                    <span class="muted">
                      {formatHeaderTimestamp(msg.timestamp)}
                    </span>
                  </div>
                </div>
              </div>
              <div class="read-actions">
                <button
                  type="button"
                  class="primary"
                  disabled={!senderContact}
                  title={senderContact
                    ? "Compose a reply to this sender."
                    : "Pin this sender first to reply."}
                  onclick={() => reply(msg)}
                >
                  Reply
                </button>
                <button
                  type="button"
                  class="danger"
                  title="Permanently delete this message."
                  onclick={() => onDeleteOne(msg.msg_id_hex)}
                >
                  {pendingDeleteId === msg.msg_id_hex
                    ? "Click again to confirm"
                    : "Delete"}
                </button>
              </div>
            </header>

            <div class="read-body">
              <div class="read-bubble" class:code={isCode}>
                {msg.plaintext_utf8}
              </div>
              {#if !senderContact}
                <p class="unpinned-hint muted small">
                  This message is from an un-pinned sender. You can
                  <button
                    type="button"
                    class="link-button"
                    onclick={() => addUnpinnedSender(msg.sender_signing_pk_hex)}
                  >
                    add them to contacts
                  </button>
                  to enable Reply.
                </p>
              {/if}
              <details class="meta-disclosure">
                <summary>Message details</summary>
                <table class="kv">
                  <tbody>
                    <tr>
                      <th>Sender</th>
                      <td>
                        {#if senderContact}
                          {senderContact.username}@{senderContact.domain}
                        {:else}
                          <code title={msg.sender_signing_pk_hex}>
                            {msg.sender_signing_pk_hex.slice(0, 32)}…
                          </code>
                        {/if}
                      </td>
                    </tr>
                    <tr>
                      <th>Received</th>
                      <td>{formatTimestampDetail(msg.timestamp)}</td>
                    </tr>
                    <tr>
                      <th>Message ID</th>
                      <td><code class="full-id">{msg.msg_id_hex}</code></td>
                    </tr>
                  </tbody>
                </table>
              </details>
            </div>
          {/if}
        </article>
      {/if}
    </div>
  {/if}
</section>

<style>
  .inbox-page {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .page-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 1rem;
    gap: 0.6rem;
    flex-shrink: 0;
  }
  .header-titles {
    display: flex;
    align-items: baseline;
    gap: 0.6rem;
    min-width: 0;
  }
  h1 {
    margin: 0;
    font-size: 1.5rem;
  }
  .unread-pill {
    font-size: 11px;
    font-weight: 600;
    color: var(--accent-strong);
    background: var(--accent-soft);
    border: 1px solid var(--border-accent);
    border-radius: 999px;
    padding: 0.18em 0.65em;
    text-transform: lowercase;
    letter-spacing: 0.01em;
  }
  .small {
    font-size: 12px;
  }
  .actions {
    display: flex;
    gap: 0.4rem;
    align-items: center;
  }
  .overflow-wrap {
    position: relative;
  }
  .overflow-button {
    font-size: 16px;
    line-height: 1;
    padding: 0.3em 0.7em;
  }
  .overflow-menu {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 10;
    list-style: none;
    margin: 0;
    padding: 0.3rem;
    min-width: 170px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-md);
  }
  .overflow-menu li {
    margin: 0;
  }
  .overflow-menu button {
    width: 100%;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    padding: 0.45em 0.7em;
    font-size: 13px;
  }
  .overflow-menu button:hover:not(:disabled) {
    background: var(--accent-softer);
    border-color: var(--accent);
  }

  .placeholder {
    padding: 2rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    text-align: center;
  }
  .empty-hint {
    margin: 0 0 0.4em;
    font-size: 14px;
  }
  .banner {
    margin: 0 0 0.85rem;
    padding: 0.55em 0.85em;
    border-radius: var(--radius-sm);
    background: var(--danger-soft);
    border: 1px solid var(--danger-border);
    color: var(--danger);
    font-size: 13px;
  }

  /* Two-pane mailbox: list ~360px, reading pane fills the rest. */
  .mailbox {
    display: grid;
    grid-template-columns: 360px 1fr;
    gap: 1rem;
    flex: 1;
    min-height: 0;
  }
  .mailbox.single-pane {
    grid-template-columns: 1fr;
  }
  .list-pane {
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
    box-shadow: var(--shadow-sm);
    min-height: 0;
  }
  .read-pane {
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-sm);
    overflow: hidden;
    min-height: 0;
  }

  /* Bulk-action toolbar, scoped to the list pane. */
  .bulk-toolbar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--accent-soft);
    border-bottom: 1px solid var(--border-accent);
    padding: 0.45rem 0.75rem;
  }
  .bulk-count {
    font-size: 12px;
    font-weight: 600;
    color: var(--accent-strong);
    margin-right: auto;
  }
  .bulk-clear {
    font-size: 12px;
    padding: 0.3em 0.7em;
  }

  .list-meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    padding: 0.55rem 0.75rem;
    border-bottom: 1px solid var(--border-soft);
    background: var(--surface-alt);
  }
  .select-all {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    margin: 0;
    cursor: pointer;
    font-size: 12px;
    color: var(--muted);
    user-select: none;
  }
  .select-all input {
    width: auto;
    margin: 0;
    cursor: pointer;
  }

  .msg-list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .msg-item + .msg-item {
    border-top: 1px solid var(--border-soft);
  }
  .msg-item.unread {
    background: var(--accent-softer);
  }
  .msg-item.selected {
    background: var(--accent-soft);
  }
  .msg-item.open {
    background: var(--accent-soft);
  }
  .msg-row {
    position: relative;
    display: flex;
    align-items: center;
    width: 100%;
    text-align: left;
    background: transparent;
    border: 0;
    padding: 0.7em 0.85em;
    cursor: pointer;
    border-radius: 0;
    gap: 0.7em;
    color: inherit;
  }
  .msg-row:hover {
    background: var(--row-hover);
    color: inherit;
  }
  .msg-row:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .msg-item.open .msg-row {
    background: transparent;
  }

  /* Hover-reveal Delete; opacity flips on row hover or focus. */
  .row-delete {
    margin-left: 0.4em;
    padding: 0.25em 0.7em;
    font-size: 11px;
    line-height: 1.2;
    border-radius: var(--radius-sm);
    opacity: 0;
    transition: opacity 0.12s ease;
    flex-shrink: 0;
  }
  .msg-row:hover .row-delete,
  .msg-row:focus-within .row-delete,
  .row-delete:focus-visible,
  .row-delete.armed {
    opacity: 1;
  }

  /* Avatar slot doubles as the checkbox slot on hover/select. */
  .avatar-slot {
    position: relative;
    width: 36px;
    height: 36px;
    flex-shrink: 0;
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
    letter-spacing: -0.01em;
    line-height: 1;
    user-select: none;
  }
  .avatar-lg {
    width: 44px;
    height: 44px;
    font-size: 16px;
  }
  .check-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--surface);
    border-radius: 50%;
    opacity: 0;
    transition: opacity 0.12s ease;
    pointer-events: none;
  }
  .check-overlay input {
    width: 16px;
    height: 16px;
    cursor: pointer;
    margin: 0;
    pointer-events: auto;
  }
  .avatar-slot.has-checkbox .check-overlay,
  .msg-row:hover .check-overlay,
  .msg-row:focus-visible .check-overlay {
    opacity: 1;
  }

  .msg-body {
    display: flex;
    flex-direction: column;
    gap: 0.18em;
    min-width: 0;
    flex: 1;
  }
  .msg-line-1 {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 0.6em;
  }
  .sender {
    font-size: 13.5px;
    color: var(--text);
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .msg-item.unread .sender {
    font-weight: 700;
  }
  .ts {
    font-size: 11px;
    color: var(--muted);
    white-space: nowrap;
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }
  .snippet {
    font-size: 12.5px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    line-height: 1.4;
  }
  .msg-item.unread .snippet {
    color: var(--muted-strong);
  }
  .trailing-dot {
    width: 0.85em;
    text-align: center;
    flex-shrink: 0;
  }
  .unread-dot {
    color: transparent;
    font-size: 10px;
    line-height: 1;
  }
  .unread-dot.on {
    color: var(--accent);
  }

  .read-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.85rem;
    padding: 0.85rem 1rem;
    border-bottom: 1px solid var(--border-soft);
    background: var(--surface);
  }
  .read-header-left {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    min-width: 0;
    flex: 1;
  }
  .back-button {
    background: transparent;
    border: 1px solid transparent;
    border-radius: 50%;
    width: 34px;
    height: 34px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 18px;
    line-height: 1;
    color: var(--muted-strong);
    flex-shrink: 0;
    padding: 0;
  }
  .back-button:hover:not(:disabled) {
    background: var(--surface-alt);
    border-color: var(--border);
    color: var(--accent);
  }
  .read-sender {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
  }
  .read-sender-name {
    font-size: 15px;
    font-weight: 600;
    color: var(--text-strong);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .read-sender-meta {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 12px;
    flex-wrap: wrap;
  }
  .dot-sep {
    color: var(--muted);
  }
  .read-actions {
    display: flex;
    gap: 0.4rem;
    flex-shrink: 0;
  }
  .read-body {
    flex: 1;
    overflow: auto;
    padding: 1.25rem 1.4rem 1.5rem;
    min-height: 0;
  }
  /* Soft-bubble message body; the code variant swaps to monospace. */
  .read-bubble {
    background: var(--accent-soft);
    border: 1px solid var(--border-accent);
    border-radius: var(--radius-lg);
    padding: 0.95em 1.1em;
    font-size: 14.5px;
    line-height: 1.55;
    color: var(--text-strong);
    white-space: pre-wrap;
    word-wrap: break-word;
    max-width: 720px;
    box-shadow: var(--shadow-sm);
  }
  .read-bubble.code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 13px;
    background: var(--surface-alt);
    border-color: var(--border);
  }
  .unpinned-hint {
    margin: 0.85em 0 0;
    max-width: 720px;
  }
  .link-button {
    background: transparent;
    color: var(--accent);
    border: none;
    padding: 0;
    text-decoration: underline;
    font: inherit;
    cursor: pointer;
  }
  .link-button:hover:not(:disabled) {
    color: var(--accent-strong);
    background: transparent;
  }
  .meta-disclosure {
    margin-top: 1.25em;
    font-size: 12px;
    max-width: 720px;
  }
  .meta-disclosure summary {
    cursor: pointer;
    color: var(--muted);
    user-select: none;
    padding: 0.1em 0;
  }
  .meta-disclosure table.kv {
    margin-top: 0.45em;
  }
  .meta-disclosure table.kv th {
    width: 110px;
    color: var(--muted);
    font-weight: 600;
    text-align: left;
    padding-right: 0.6em;
  }
  .full-id {
    user-select: all;
    word-break: break-all;
  }

  .reading-empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 2rem;
    gap: 1rem;
    text-align: center;
  }
  .reading-empty-bubble {
    background: var(--accent-soft);
    border: 1px dashed var(--border-accent);
    border-radius: var(--radius-lg);
    padding: 1.25em 1.5em;
    font-size: 13px;
    color: var(--muted-strong);
    box-shadow: var(--shadow-sm);
  }

  .diagnostic {
    margin: 0 0 1rem;
    padding: 1rem 1.1rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-sm);
  }
  .diagnostic-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.5rem;
  }
  .diagnostic h3 {
    margin: 0;
    font-size: 0.95rem;
  }
  .diagnostic table.kv th {
    width: 130px;
  }
  .zone-pill {
    margin-right: 0.4em;
  }
  .notes {
    margin-top: 0.2em;
    padding-left: 1.2em;
  }
  .diag-table {
    margin-top: 0.6rem;
    font-size: 12px;
  }
  .diag-note {
    color: var(--muted);
    font-size: 11px;
    max-width: 300px;
  }
  .dismiss {
    font-size: 12px;
    padding: 0.25em 0.7em;
  }

  /* Narrow viewport tightens the read-pane padding. */
  @media (max-width: 700px) {
    .read-header {
      padding: 0.7rem 0.85rem;
    }
    .read-actions {
      gap: 0.3rem;
    }
    .read-body {
      padding: 1rem 0.95rem 1.25rem;
    }
    .read-bubble {
      font-size: 14px;
    }
  }
</style>
