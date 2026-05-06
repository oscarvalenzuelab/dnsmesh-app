<script lang="ts">
  import { onMount, tick } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { activeIdentity } from "$lib/stores/identity";
  import {
    inbox,
    inboxError,
    pollInbox,
    markRead,
    deleteMessages,
  } from "$lib/stores/inbox";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import { appendSent, removeSentByRecipient } from "$lib/stores/sent";
  import {
    conversations,
    type ChatMessage,
    type Conversation,
    UNKNOWN_KEY,
  } from "$lib/stores/conversations";
  import { api, isCommandError, type ContactView } from "$lib/api";
  import { PICKER_EMOJI, renderEmoticons } from "$lib/emoticons";

  // Selected conversation key. `null` means show the conversation list
  // on mobile / show an empty placeholder pane on desktop.
  let activeKey = $state<string | null>(null);

  // Composer state.
  let draft = $state<string>("");
  let sending = $state<boolean>(false);
  let sendError = $state<string>("");
  let replyTo = $state<ChatMessage | null>(null);

  // Picker state for "+ New chat".
  let pickerOpen = $state<boolean>(false);
  let pickerQuery = $state<string>("");
  let pickerFetchAddress = $state<string>("");
  let pickerBusy = $state<boolean>(false);
  let pickerError = $state<string>("");

  // Responsive state — collapse to single-column under 560px so the
  // default 680px window keeps both columns visible.
  let isNarrow = $state<boolean>(false);
  const NARROW_BREAKPOINT_PX = 560;

  let composerEl: HTMLTextAreaElement | undefined = $state();
  let threadEl: HTMLDivElement | undefined = $state();
  let emojiOpen = $state<boolean>(false);
  let threadMenuOpen = $state<boolean>(false);
  let clearing = $state<boolean>(false);
  // Inline confirm step. The Tauri webview silently returned `false`
  // from `window.confirm`, so the click chain never reached the actual
  // clear path. Use UI state for a two-click flow that works in every
  // webview.
  let confirmingClear = $state<boolean>(false);
  let confirmClearBtn: HTMLButtonElement | undefined = $state();

  // Fast-poll cadence while a thread is focused. SDK pulls are global,
  // so this just shortens latency for whichever chat the user is
  // looking at; the layout's 60s poll keeps the rest of the inbox warm.
  const ACTIVE_POLL_INTERVAL_MS = 10_000;
  let activePollHandle: ReturnType<typeof setInterval> | null = null;

  $effect(() => {
    if (activeKey) {
      if (activePollHandle === null) {
        activePollHandle = setInterval(() => {
          void pollInbox();
        }, ACTIVE_POLL_INTERVAL_MS);
      }
    } else if (activePollHandle !== null) {
      clearInterval(activePollHandle);
      activePollHandle = null;
    }
  });

  function toggleThreadMenu() {
    threadMenuOpen = !threadMenuOpen;
    // Reset the confirm step whenever the menu opens, so it always
    // starts at "Clear chat" not the half-armed "Confirm" view.
    if (threadMenuOpen) confirmingClear = false;
  }

  function requestClearChat() {
    confirmingClear = true;
    // Move focus onto the destructive button so the next Enter / Space
    // confirms (or Escape via the menu cancels). Without this, focus
    // stays on the now-removed "Clear chat" button and falls back to
    // the document.
    tick().then(() => confirmClearBtn?.focus());
  }

  function cancelClearChat() {
    confirmingClear = false;
  }

  async function confirmClearChat() {
    confirmingClear = false;
    threadMenuOpen = false;
    if (!activeConversation) return;
    clearing = true;
    try {
      const incomingIds = activeConversation.messages
        .filter((m) => m.direction === "in")
        .map((m) => m.msg_id_hex);
      if (incomingIds.length > 0) {
        await deleteMessages(incomingIds);
      }
      if ($activeIdentity && activeConversation.username) {
        removeSentByRecipient(
          $activeIdentity.username,
          activeConversation.username,
        );
      }
      activeKey = null;
    } finally {
      clearing = false;
    }
  }

  function toggleEmoji() {
    emojiOpen = !emojiOpen;
  }

  function insertEmoji(glyph: string) {
    const el = composerEl;
    if (!el) {
      draft = draft + glyph;
    } else {
      const start = el.selectionStart ?? draft.length;
      const end = el.selectionEnd ?? draft.length;
      draft = draft.slice(0, start) + glyph + draft.slice(end);
      // Restore caret right after the inserted glyph.
      tick().then(() => {
        if (composerEl) {
          const pos = start + glyph.length;
          composerEl.focus();
          composerEl.setSelectionRange(pos, pos);
        }
      });
    }
    emojiOpen = false;
  }

  function syncNarrow() {
    if (typeof window === "undefined") return;
    isNarrow = window.innerWidth < NARROW_BREAKPOINT_PX;
  }

  onMount(() => {
    syncNarrow();
    window.addEventListener("resize", syncNarrow);
    // Inbox/contacts/sent hydrate is owned by the layout — it runs on
    // mount and on identity switch/lock from anywhere. Avoiding a
    // duplicate hydrate here prevents a late `inbox_load` from
    // clobbering a fresh `pollInbox` merge when both fire on startup.
    const contactParam = page.url.searchParams.get("contact");
    const replyToParam = page.url.searchParams.get("reply_to");
    if (contactParam) {
      void (async () => {
        await openConversation(contactParam.trim().toLowerCase());
        if (!replyToParam) return;
        // Resolve the reply target once the conversation hydrates;
        // the row may not be in the inbox yet on a cold-launch deep
        // link, so wait one tick after activeConversation is set.
        await tick();
        const target = activeConversation?.messages.find(
          (m) => m.msg_id_hex.toLowerCase() === replyToParam.toLowerCase(),
        );
        if (target) replyTo = target;
      })();
    }
    return () => {
      window.removeEventListener("resize", syncNarrow);
      if (activePollHandle !== null) {
        clearInterval(activePollHandle);
        activePollHandle = null;
      }
    };
  });

  // Look up a contact by the conversation key (which is the username).
  // Falls back to a case-insensitive match so deep-link params work.
  function contactByKey(key: string): ContactView | null {
    const lower = key.toLowerCase();
    return (
      $contacts.find((c) => c.username.toLowerCase() === lower) ?? null
    );
  }

  // Synthesize an empty Conversation when activeKey points at a known
  // contact with no messages yet (e.g. just-added or just-picked from
  // + New chat). Without this the thread pane would render as if no
  // conversation were selected.
  function virtualConversation(key: string): Conversation | null {
    if (key === UNKNOWN_KEY) return null;
    const c = contactByKey(key);
    if (!c) return null;
    return {
      key: c.username,
      label: `${c.username}@${c.domain}`,
      username: c.username,
      domain: c.domain,
      contact: c,
      messages: [],
      lastTimestamp: 0,
      unread: 0,
      preview: "",
    };
  }

  const activeConversation = $derived<Conversation | null>(
    activeKey
      ? $conversations.find((c) => c.key === activeKey) ??
          virtualConversation(activeKey)
      : null,
  );

  // Auto-scroll the thread to the latest message when it changes.
  $effect(() => {
    void activeConversation?.messages.length;
    tick().then(() => {
      if (threadEl) {
        threadEl.scrollTop = threadEl.scrollHeight;
      }
    });
  });

  async function openConversation(key: string) {
    activeKey = key;
    replyTo = null;
    sendError = "";
    // Don't carry per-thread overflow state across a switch — a
    // half-armed "Yes, clear" from chat A would otherwise act on
    // chat B once it became active.
    threadMenuOpen = false;
    confirmingClear = false;
    await tick();
    if (composerEl) composerEl.focus();
    // Mark-read is handled by the $effect below so messages that
    // arrive via polling while the thread is already open also flip
    // to read without requiring a click.
  }

  // Auto-mark unread incoming messages as read whenever the active
  // thread's message list changes — covers both the initial open and
  // late-arriving messages from the 10s fast poll.
  $effect(() => {
    const conv = activeConversation;
    if (!conv) return;
    for (const m of conv.messages) {
      if (m.direction === "in" && !m.read) {
        void markRead(m.msg_id_hex);
      }
    }
  });

  // Identity switch / lock from anywhere (header pill, /identities
  // page) drops the transient per-thread menu state. Without this, a
  // half-armed "Yes, clear" arming under identity A could carry into
  // identity B and act on B's data if the new identity happens to
  // share the same activeKey (same contact username).
  $effect(() => {
    void $activeIdentity;
    threadMenuOpen = false;
    confirmingClear = false;
  });

  function closeConversation() {
    activeKey = null;
    replyTo = null;
    threadMenuOpen = false;
    confirmingClear = false;
  }

  async function send() {
    sendError = "";
    if (!activeConversation || !activeConversation.username) {
      sendError = "Pick a contact to send to.";
      return;
    }
    const recipient = activeConversation.username;
    const body = replyTo
      ? quoteReply(replyTo) + "\n\n" + draft
      : draft;
    if (!body.trim()) {
      sendError = "Message body must not be empty.";
      return;
    }
    // Snapshot the identity *before* the await — if the user locks or
    // switches mid-send, the row would otherwise land in whichever
    // identity is active when the SDK reply arrives. We bind the row
    // to the identity that authorised the send, regardless.
    const identityAtSend = $activeIdentity?.username ?? null;
    if (!identityAtSend) {
      sendError = "No identity unlocked.";
      return;
    }
    sending = true;
    try {
      const result = await api.sendMessage(recipient, body);
      // Only persist the sent row if that identity is still active. A
      // mid-send switch means the row belongs to a different per-
      // identity store and should be dropped (or, in a future revision,
      // deferred until that identity is next unlocked).
      const identityNow = $activeIdentity?.username ?? null;
      if (identityNow === identityAtSend) {
        appendSent(identityAtSend, {
          msg_id_hex: result.msg_id_hex,
          recipient_username: recipient,
          timestamp: Math.floor(Date.now() / 1000),
          plaintext_utf8: body,
        });
      }
      // Pull immediately so any inbound reply that already landed
      // before the next 60s tick shows up alongside the outgoing row.
      void pollInbox();
      draft = "";
      replyTo = null;
    } catch (err) {
      if (isCommandError(err) && err.kind === "contact_not_found") {
        sendError = `No pinned contact named "${recipient}". Add them from + New chat.`;
      } else {
        sendError = isCommandError(err) ? err.message : String(err);
      }
    } finally {
      sending = false;
    }
  }

  function quoteReply(m: ChatMessage): string {
    const head = `> [reply to ${m.msg_id_hex.slice(0, 12)} @ ${formatTimestamp(m.timestamp)}]`;
    const body = m.plaintext_utf8
      .split("\n")
      .map((line) => "> " + line)
      .join("\n");
    return head + "\n" + body;
  }

  function startReply(m: ChatMessage) {
    replyTo = m;
    if (composerEl) composerEl.focus();
  }

  function cancelReply() {
    replyTo = null;
  }

  // Element refs + return-focus target for the picker modal.
  let pickerSearchEl: HTMLInputElement | undefined = $state();
  let pickerOpenerEl: HTMLElement | null = null;

  function openPicker(e?: MouseEvent) {
    if (e?.currentTarget instanceof HTMLElement) {
      pickerOpenerEl = e.currentTarget;
    }
    pickerOpen = true;
    pickerQuery = "";
    pickerFetchAddress = "";
    pickerError = "";
    void refreshContacts();
    // Hand focus to the search input on next paint so screen readers
    // announce the dialog and keyboard users land in the search field.
    tick().then(() => pickerSearchEl?.focus());
  }

  function closePicker() {
    pickerOpen = false;
    pickerError = "";
    // Restore focus to whatever opened the picker so keyboard users
    // don't get dumped at the top of the document.
    const opener = pickerOpenerEl;
    pickerOpenerEl = null;
    tick().then(() => opener?.focus());
  }

  // Escape handler attached to the dialog itself rather than the
  // backdrop — the backdrop has tabindex=-1 and never receives
  // keyboard focus, so the prior wiring was a no-op.
  function onPickerKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      closePicker();
    }
  }

  function pickContact(c: ContactView) {
    closePicker();
    void openConversation(c.username.toLowerCase());
  }

  async function fetchAndOpen() {
    pickerError = "";
    const addr = pickerFetchAddress.trim();
    if (!addr) {
      pickerError = "Enter user@domain.";
      return;
    }
    pickerBusy = true;
    try {
      const result = await api.fetchAndAddContact(addr);
      await refreshContacts();
      closePicker();
      void openConversation(result.contact.username.toLowerCase());
    } catch (err) {
      pickerError = isCommandError(err) ? err.message : String(err);
    } finally {
      pickerBusy = false;
    }
  }

  function formatTimestamp(ts: number): string {
    if (!ts) return "";
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

  function formatBubbleTimestamp(ts: number): string {
    const d = new Date(ts * 1000);
    return d.toLocaleString([], {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  function onComposerKeydown(e: KeyboardEvent) {
    // Enter sends; Shift+Enter inserts a newline.
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void send();
    }
  }

  const filteredContacts = $derived<ContactView[]>(
    pickerQuery
      ? $contacts.filter((c) =>
          `${c.username}@${c.domain}`
            .toLowerCase()
            .includes(pickerQuery.toLowerCase()),
        )
      : $contacts,
  );

  // Conversation-row preview prefix for outgoing-last messages.
  function previewPrefix(c: Conversation): string {
    const last = c.messages[c.messages.length - 1];
    if (!last) return "";
    return last.direction === "out" ? "You: " : "";
  }

  const showList = $derived(!isNarrow || activeKey === null);
  const showThread = $derived(!isNarrow || activeKey !== null);
</script>

{#if !$activeIdentity}
  <div class="locked-pane">
    <div class="locked-card">
      <h2>No identity unlocked</h2>
      <p class="muted">
        Open the identity menu in the header to unlock or switch identities.
      </p>
      <button class="primary" onclick={() => goto("/identities")}>
        Manage identities
      </button>
    </div>
  </div>
{:else}
  <div class="chat" class:narrow={isNarrow}>
    {#if showList}
      <aside class="sidebar">
        <div class="sidebar-head">
          <h2>Chats</h2>
          <button
            type="button"
            class="primary new-chat"
            onclick={openPicker}
            title="New chat"
          >+ New chat</button>
        </div>
        {#if $inboxError}
          <p class="error small inline-msg">{$inboxError}</p>
        {/if}
        {#if $conversations.length === 0}
          <div class="empty-list">
            <p class="muted">No conversations yet.</p>
            <p class="muted small">Use <strong>+ New chat</strong> to start one.</p>
          </div>
        {:else}
          <ul class="conv-list">
            {#each $conversations as conv (conv.key)}
              <li>
                <button
                  type="button"
                  class="conv-row"
                  class:active={conv.key === activeKey}
                  onclick={() => openConversation(conv.key)}
                >
                  <div class="conv-row-top">
                    <span class="conv-label" class:unknown={conv.key === UNKNOWN_KEY}>
                      {conv.label}
                    </span>
                    <span class="conv-time">{formatTimestamp(conv.lastTimestamp)}</span>
                  </div>
                  <div class="conv-row-bottom">
                    <span class="conv-preview">
                      {previewPrefix(conv)}{conv.preview || "(no messages yet)"}
                    </span>
                    {#if conv.unread > 0}
                      <span class="unread-dot" aria-label={`${conv.unread} unread`}>
                        {conv.unread}
                      </span>
                    {/if}
                  </div>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </aside>
    {/if}

    {#if showThread}
      <section class="thread-pane">
        {#if !activeConversation}
          <div class="empty-thread">
            <p class="muted">Select a conversation to start chatting.</p>
          </div>
        {:else}
          <header class="thread-head">
            {#if isNarrow}
              <button
                type="button"
                class="back-button"
                onclick={closeConversation}
                aria-label="Back to conversations"
              >←</button>
            {/if}
            <div class="thread-title">
              <span class="thread-name">{activeConversation.label}</span>
              {#if activeConversation.key === UNKNOWN_KEY}
                <span class="thread-sub muted small">
                  Senders not pinned to a contact
                </span>
              {/if}
            </div>
            <div class="thread-menu-wrap">
              <button
                type="button"
                class="icon-btn-sm"
                onclick={toggleThreadMenu}
                aria-haspopup="true"
                aria-expanded={threadMenuOpen}
                aria-label="Conversation actions"
                title="Actions"
                disabled={clearing}
              >⋯</button>
              {#if threadMenuOpen}
                <div class="thread-menu" role="menu">
                  {#if !confirmingClear}
                    <button
                      type="button"
                      class="thread-menu-item danger-text"
                      onclick={requestClearChat}
                      disabled={clearing || activeConversation.messages.length === 0}
                    >
                      {clearing ? "Clearing…" : "Clear chat"}
                    </button>
                  {:else}
                    <p class="thread-menu-prompt">Clear all messages with this contact?</p>
                    <button
                      bind:this={confirmClearBtn}
                      type="button"
                      class="thread-menu-item danger-text"
                      onclick={confirmClearChat}
                      disabled={clearing}
                    >
                      {clearing ? "Clearing…" : "Yes, clear"}
                    </button>
                    <button
                      type="button"
                      class="thread-menu-item"
                      onclick={cancelClearChat}
                      disabled={clearing}
                    >
                      Cancel
                    </button>
                  {/if}
                </div>
              {/if}
            </div>
          </header>

          <div class="messages" bind:this={threadEl}>
            {#if activeConversation.messages.length === 0}
              <div class="empty-thread inline">
                <p class="muted small">No messages yet. Say hi.</p>
              </div>
            {/if}
            {#each activeConversation.messages as m (m.msg_id_hex)}
              <div class="bubble-row" class:out={m.direction === "out"}>
                <div class="bubble" class:out={m.direction === "out"}>
                  {#if m.sender_spk_hex}
                    <div class="bubble-sender">
                      <code>{m.sender_spk_hex.slice(0, 16)}…</code>
                    </div>
                  {/if}
                  <div class="bubble-body">{renderEmoticons(m.plaintext_utf8)}</div>
                  <div class="bubble-meta">
                    <span class="bubble-time">{formatBubbleTimestamp(m.timestamp)}</span>
                    {#if m.direction === "in" && activeConversation.key !== UNKNOWN_KEY}
                      <button
                        type="button"
                        class="bubble-action"
                        onclick={() => startReply(m)}
                      >Reply</button>
                    {/if}
                  </div>
                </div>
              </div>
            {/each}
          </div>

          {#if activeConversation.key === UNKNOWN_KEY}
            <div class="composer-disabled">
              <p class="muted small">
                Pin one of these senders as a contact to reply.
              </p>
            </div>
          {:else}
            <form class="composer" onsubmit={(e) => { e.preventDefault(); void send(); }}>
              {#if replyTo}
                <div class="reply-pill">
                  <div class="reply-text">
                    <span class="reply-tag">Replying to</span>
                    <code>{replyTo.msg_id_hex.slice(0, 12)}…</code>
                    <span class="reply-preview">
                      {replyTo.plaintext_utf8.slice(0, 80)}
                      {replyTo.plaintext_utf8.length > 80 ? "…" : ""}
                    </span>
                  </div>
                  <button type="button" class="reply-cancel" onclick={cancelReply}>×</button>
                </div>
              {/if}
              {#if sendError}
                <p class="error small inline-msg">{sendError}</p>
              {/if}
              <div class="composer-row">
                <div class="emoji-wrap">
                  <button
                    type="button"
                    class="emoji-button"
                    onclick={toggleEmoji}
                    aria-haspopup="true"
                    aria-expanded={emojiOpen}
                    title="Emoji"
                    disabled={sending}
                  >🙂</button>
                  {#if emojiOpen}
                    <div class="emoji-panel" role="dialog" aria-label="Emoji picker">
                      {#each PICKER_EMOJI as glyph}
                        <button
                          type="button"
                          class="emoji-cell"
                          onclick={() => insertEmoji(glyph)}
                        >{glyph}</button>
                      {/each}
                    </div>
                  {/if}
                </div>
                <textarea
                  bind:this={composerEl}
                  bind:value={draft}
                  onkeydown={onComposerKeydown}
                  placeholder={`Message ${activeConversation.username}…`}
                  rows="1"
                  disabled={sending}
                ></textarea>
                <button
                  type="submit"
                  class="primary send-button"
                  disabled={sending || !draft.trim()}
                >
                  {sending ? "Sending…" : "Send"}
                </button>
              </div>
            </form>
          {/if}
        {/if}
      </section>
    {/if}
  </div>

  {#if pickerOpen}
    <div
      class="picker-backdrop"
      onclick={closePicker}
      aria-hidden="true"
    ></div>
    <div
      class="picker"
      role="dialog"
      aria-modal="true"
      aria-label="New chat"
      tabindex="-1"
      onkeydown={onPickerKeydown}
    >
      <header class="picker-head">
        <h3>New chat</h3>
        <button type="button" class="icon-close" onclick={closePicker} aria-label="Close">×</button>
      </header>
      <div class="picker-body">
        <label class="search-label">
          <span>Search contacts</span>
          <input
            bind:this={pickerSearchEl}
            type="text"
            bind:value={pickerQuery}
            placeholder="username or username@domain"
          />
        </label>
        {#if filteredContacts.length === 0}
          <p class="muted small">No matching contacts.</p>
        {:else}
          <ul class="picker-list">
            {#each filteredContacts as c (c.username)}
              <li>
                <button type="button" class="picker-row" onclick={() => pickContact(c)}>
                  <span class="picker-name">{c.username}@{c.domain}</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
        <hr class="divider" />
        <form class="fetch-form" onsubmit={(e) => { e.preventDefault(); void fetchAndOpen(); }}>
          <label>
            <span>Or fetch a new contact by address</span>
            <input
              type="text"
              bind:value={pickerFetchAddress}
              placeholder="user@example.com"
              disabled={pickerBusy}
            />
          </label>
          {#if pickerError}
            <p class="error small inline-msg">{pickerError}</p>
          {/if}
          <button type="submit" class="primary" disabled={pickerBusy}>
            {pickerBusy ? "Fetching…" : "Fetch and open"}
          </button>
        </form>
      </div>
    </div>
  {/if}
{/if}

<style>
  .locked-pane {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    padding: 2rem;
  }
  .locked-card {
    text-align: center;
    max-width: 360px;
  }
  .locked-card h2 {
    margin: 0 0 0.5em;
  }

  .chat {
    display: grid;
    grid-template-columns: 240px 1fr;
    height: 100%;
    background: var(--bg);
    overflow: hidden;
  }
  .chat.narrow {
    grid-template-columns: 1fr;
  }

  .sidebar {
    border-right: 1px solid var(--border);
    background: var(--surface);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .sidebar-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.85rem 1rem;
    border-bottom: 1px solid var(--border-soft);
  }
  .sidebar-head h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 700;
    letter-spacing: -0.005em;
  }
  .new-chat {
    padding: 0.4em 0.75em;
    font-size: 12.5px;
    min-height: 32px;
  }
  .empty-list {
    padding: 1.5rem 1rem;
    text-align: center;
  }
  .empty-list p {
    margin: 0.25em 0;
  }
  .conv-list {
    list-style: none;
    margin: 0;
    padding: 0.25rem 0.4rem;
    overflow-y: auto;
    flex: 1 1 auto;
  }
  .conv-list li {
    margin: 0;
  }
  .conv-row {
    width: 100%;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    padding: 0.6rem 0.7rem;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    cursor: pointer;
    min-height: 56px;
  }
  .conv-row:hover {
    background: var(--surface-alt);
    border-color: var(--border-soft);
  }
  .conv-row.active {
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .conv-row-top {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .conv-label {
    font-weight: 600;
    font-size: 13px;
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .conv-label.unknown {
    color: var(--muted-strong);
    font-style: italic;
  }
  .conv-time {
    font-size: 11px;
    color: var(--muted);
    flex-shrink: 0;
  }
  .conv-row-bottom {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
  }
  .conv-preview {
    color: var(--muted);
    font-size: 12.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1 1 auto;
  }
  .unread-dot {
    background: var(--accent);
    color: #fff;
    font-size: 10px;
    font-weight: 700;
    padding: 1px 7px;
    border-radius: 999px;
    min-width: 18px;
    text-align: center;
    flex-shrink: 0;
  }

  .thread-pane {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg);
  }
  .thread-head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.85rem 1rem;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .thread-menu-wrap {
    position: relative;
    flex-shrink: 0;
  }
  .icon-btn-sm {
    width: 32px;
    min-height: 32px;
    padding: 0;
    border-radius: 50%;
    font-size: 16px;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .thread-menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 30;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-md);
    padding: 0.4rem;
    min-width: 160px;
  }
  .thread-menu-item {
    width: 100%;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    padding: 0.5em 0.7em;
    border-radius: 6px;
  }
  .thread-menu-prompt {
    margin: 0 0 0.4rem;
    padding: 0.4em 0.7em 0;
    font-size: 12px;
    color: var(--muted-strong);
  }
  .thread-menu-item:hover:not(:disabled) {
    background: var(--accent-softer);
    border-color: var(--accent);
  }
  .danger-text {
    color: var(--danger);
  }
  .danger-text:hover:not(:disabled) {
    background: var(--danger-soft);
    border-color: var(--danger);
    color: var(--danger);
  }
  .back-button {
    width: 36px;
    min-height: 36px;
    padding: 0;
    border-radius: 50%;
    font-size: 18px;
  }
  .thread-title {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    flex: 1 1 auto;
  }
  .thread-name {
    font-weight: 700;
    font-size: 14px;
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .thread-sub {
    font-style: italic;
  }
  .messages {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 1rem 1rem 0.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .empty-thread {
    flex: 1 1 auto;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 2rem;
  }
  .empty-thread.inline {
    flex: 0 0 auto;
    padding: 0.5rem;
  }
  .bubble-row {
    display: flex;
    justify-content: flex-start;
  }
  .bubble-row.out {
    justify-content: flex-end;
  }
  .bubble {
    max-width: min(560px, 70%);
    padding: 0.55rem 0.75rem;
    border-radius: 14px;
    background: var(--bubble-theirs);
    color: var(--bubble-theirs-text);
    border: 1px solid var(--border-soft);
    box-shadow: var(--shadow-sm);
    border-bottom-left-radius: 4px;
  }
  .bubble.out {
    background: var(--bubble-mine);
    color: var(--bubble-mine-text);
    border-color: transparent;
    border-bottom-left-radius: 14px;
    border-bottom-right-radius: 4px;
  }
  .bubble-sender {
    font-size: 11px;
    color: var(--muted);
    margin-bottom: 0.25rem;
  }
  .bubble-body {
    font-size: 13.5px;
    white-space: pre-wrap;
    word-wrap: break-word;
    line-height: 1.4;
  }
  .bubble-meta {
    margin-top: 0.35rem;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
  }
  .bubble-time {
    font-size: 10.5px;
    color: var(--muted);
    opacity: 0.85;
  }
  .bubble.out .bubble-time {
    color: rgba(255, 255, 255, 0.78);
  }
  .bubble-action {
    background: transparent;
    border: 0;
    padding: 0 0.2rem;
    min-height: 0;
    color: var(--accent);
    font-size: 11px;
  }
  .bubble-action:hover {
    text-decoration: underline;
    border: 0;
    color: var(--accent);
  }

  .composer {
    /* Pad to clear Android's gesture nav bar / iOS home indicator;
       desktop has 0 inset and uses the static 0.85rem. */
    padding:
      0.5rem
      max(0.75rem, env(safe-area-inset-right))
      max(0.85rem, env(safe-area-inset-bottom))
      max(0.75rem, env(safe-area-inset-left));
    background: var(--surface);
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .composer-disabled {
    padding:
      1rem
      max(1rem, env(safe-area-inset-right))
      max(1rem, env(safe-area-inset-bottom))
      max(1rem, env(safe-area-inset-left));
    text-align: center;
    border-top: 1px solid var(--border);
    background: var(--surface);
  }
  .reply-pill {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.5rem;
    padding: 0.4rem 0.6rem;
    background: var(--accent-softer);
    border: 1px solid var(--border-accent);
    border-radius: var(--radius-sm);
    font-size: 12px;
  }
  .reply-text {
    overflow: hidden;
  }
  .reply-tag {
    color: var(--muted);
    margin-right: 0.4em;
  }
  .reply-preview {
    color: var(--muted-strong);
    margin-left: 0.4em;
  }
  .reply-cancel {
    background: transparent;
    border: 0;
    padding: 0;
    width: 22px;
    min-height: 22px;
    line-height: 1;
    font-size: 16px;
    color: var(--muted);
  }
  .composer-row {
    display: flex;
    gap: 0.5rem;
    align-items: flex-end;
  }
  .composer-row textarea {
    resize: vertical;
    min-height: 40px;
    max-height: 160px;
    width: auto;
    flex: 1 1 auto;
  }
  .send-button {
    flex-shrink: 0;
    min-height: 40px;
  }
  .emoji-wrap {
    position: relative;
    flex-shrink: 0;
  }
  .emoji-button {
    width: 40px;
    min-height: 40px;
    padding: 0;
    border-radius: var(--radius-sm);
    font-size: 18px;
    line-height: 1;
  }
  .emoji-panel {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 0;
    z-index: 30;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-md);
    padding: 0.4rem;
    display: grid;
    grid-template-columns: repeat(7, 32px);
    gap: 2px;
  }
  .emoji-cell {
    width: 32px;
    height: 32px;
    min-height: 32px;
    padding: 0;
    border: 1px solid transparent;
    background: transparent;
    border-radius: 6px;
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
  }
  .emoji-cell:hover {
    background: var(--accent-softer);
    border-color: var(--accent);
  }
  .inline-msg {
    margin: 0;
  }

  .picker-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(15, 18, 30, 0.4);
    z-index: 50;
    border: 0;
  }
  .picker {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(440px, calc(100vw - 2rem));
    max-height: calc(100vh - 4rem);
    background: var(--surface);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-md);
    display: flex;
    flex-direction: column;
    z-index: 51;
  }
  .picker-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1rem 1.1rem;
    border-bottom: 1px solid var(--border-soft);
  }
  .picker-head h3 {
    margin: 0;
    font-size: 15px;
  }
  .icon-close {
    width: 32px;
    min-height: 32px;
    padding: 0;
    border-radius: 50%;
    font-size: 18px;
    line-height: 1;
  }
  .picker-body {
    padding: 1rem 1.1rem 1.1rem;
    overflow-y: auto;
  }
  .search-label {
    margin-bottom: 0.6rem;
  }
  .picker-list {
    list-style: none;
    padding: 0;
    margin: 0 0 0.6rem;
    max-height: 200px;
    overflow-y: auto;
  }
  .picker-row {
    width: 100%;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    padding: 0.5em 0.7em;
    border-radius: 6px;
    min-height: 40px;
  }
  .picker-row:hover {
    background: var(--accent-softer);
    border-color: var(--accent);
  }
  .picker-name {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12.5px;
  }
  .divider {
    margin: 0.75rem 0;
    border: 0;
    border-top: 1px solid var(--border-soft);
  }
  .fetch-form button {
    width: 100%;
  }

  @media (max-width: 560px) {
    .chat:not(.narrow) {
      grid-template-columns: 1fr;
    }
  }
</style>
