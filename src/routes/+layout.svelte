<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { get } from "svelte/store";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import {
    activeIdentity,
    publishedStatus,
    refreshActiveIdentity,
  } from "$lib/stores/identity";
  import { clearInbox, hydrateInbox, pollInbox } from "$lib/stores/inbox";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import { clearSent, hydrateSent } from "$lib/stores/sent";
  import {
    api,
    isCommandError,
    type IdentitySummary,
  } from "$lib/api";

  let { children } = $props();

  let identities = $state<IdentitySummary[]>([]);
  let switchTarget = $state<string>("");
  let switchPassphrase = $state<string>("");
  let switchBusy = $state<boolean>(false);
  let switchError = $state<string>("");
  let identityMenuOpen = $state<boolean>(false);
  let overflowOpen = $state<boolean>(false);

  let firstRun = $derived(identities.length === 0 && !$activeIdentity);

  // Background poll handle. Started after unlock, cleared on lock.
  let pollHandle: ReturnType<typeof setInterval> | null = null;
  const POLL_INTERVAL_MS = 60_000;

  function startPolling() {
    if (pollHandle !== null) return;
    pollHandle = setInterval(() => {
      void pollInbox();
    }, POLL_INTERVAL_MS);
  }

  function stopPolling() {
    if (pollHandle !== null) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  }

  onMount(async () => {
    await refreshActiveIdentity();
    await reloadList();
    const ident = get(activeIdentity);
    if (ident) {
      hydrateSent(ident.username);
      void hydrateInbox();
      void refreshContacts();
      void pollInbox();
      startPolling();
    } else if (
      identities.length === 0 &&
      page.url.pathname !== "/identities"
    ) {
      void goto("/identities?onboarding=1", { replaceState: true });
    }
  });

  onDestroy(() => stopPolling());

  async function reloadList() {
    try {
      identities = await api.listIdentities();
    } catch {
      identities = [];
    }
  }

  function toggleIdentityMenu() {
    identityMenuOpen = !identityMenuOpen;
    overflowOpen = false;
    switchError = "";
    if (identityMenuOpen) {
      void reloadList();
    } else {
      switchTarget = "";
      switchPassphrase = "";
    }
  }

  function toggleOverflow() {
    overflowOpen = !overflowOpen;
    identityMenuOpen = false;
  }

  function closeMenus() {
    overflowOpen = false;
    identityMenuOpen = false;
  }

  function pickTarget(username: string) {
    switchTarget = username;
    switchPassphrase = "";
    switchError = "";
  }

  async function submitSwitch() {
    if (!switchTarget) {
      switchError = "Pick an identity.";
      return;
    }
    if (!switchPassphrase) {
      switchError = "Passphrase required.";
      return;
    }
    switchBusy = true;
    switchError = "";
    try {
      await api.switchIdentity(switchTarget, switchPassphrase);
      switchPassphrase = "";
      const target = switchTarget;
      switchTarget = "";
      identityMenuOpen = false;
      stopPolling();
      clearInbox();
      clearSent();
      contacts.set([]);
      await refreshActiveIdentity();
      await reloadList();
      hydrateSent(target);
      void hydrateInbox();
      void refreshContacts();
      void pollInbox();
      startPolling();
    } catch (err) {
      switchError = isCommandError(err) ? err.message : String(err);
    } finally {
      switchBusy = false;
    }
  }

  async function lock() {
    activeIdentity.set(null);
    publishedStatus.set(null);
    stopPolling();
    clearInbox();
    clearSent();
    contacts.set([]);
    closeMenus();
    switchTarget = "";
    switchPassphrase = "";

    void goto("/", { replaceState: false });

    switchBusy = true;
    try {
      await api.lockIdentity();
      await reloadList();
    } catch (err) {
      switchError = isCommandError(err) ? err.message : String(err);
    } finally {
      switchBusy = false;
    }
  }

  function navTo(path: string) {
    closeMenus();
    void goto(path);
  }

  // Click-outside handler for menus.
  function handleDocClick(e: MouseEvent) {
    if (!(overflowOpen || identityMenuOpen)) return;
    const target = e.target as HTMLElement | null;
    if (!target) return;
    if (!target.closest(".topbar")) closeMenus();
  }
</script>

<svelte:document onclick={handleDocClick} />

<div class="app">
  <header class="topbar">
    <a class="brand" href="/" onclick={closeMenus} title="Back to chats">DNSMesh</a>
    <div class="header-actions">
      {#if !firstRun}
        <div class="identity-wrap">
          <button
            type="button"
            class="identity-button"
            onclick={toggleIdentityMenu}
            aria-haspopup="true"
            aria-expanded={identityMenuOpen}
          >
            {#if $activeIdentity}
              <span class="user">{$activeIdentity.username}</span>
              <span class="at">@</span>
              <span class="domain">{$activeIdentity.domain}</span>
            {:else}
              <span class="locked">Locked</span>
            {/if}
            <span class="chev" aria-hidden="true">{identityMenuOpen ? "▲" : "▼"}</span>
          </button>
          {#if identityMenuOpen}
            <div class="menu identity-menu" role="menu">
              {#if identities.length === 0}
                <p class="muted small menu-empty">
                  No identities yet. Open Identities from the menu.
                </p>
              {:else}
                <ul class="menu-list">
                  {#each identities as ident (ident.username)}
                    <li>
                      <button
                        type="button"
                        class="menu-item"
                        class:active={ident.is_active}
                        class:selected={switchTarget === ident.username}
                        onclick={() => pickTarget(ident.username)}
                        disabled={ident.is_active}
                      >
                        <span class="menu-name">
                          {ident.username}@{ident.domain}
                        </span>
                        {#if ident.is_active}
                          <span class="menu-tag pass">ACTIVE</span>
                        {/if}
                      </button>
                    </li>
                  {/each}
                </ul>
                {#if switchTarget && switchTarget !== $activeIdentity?.username}
                  <form
                    class="menu-form"
                    onsubmit={(e) => {
                      e.preventDefault();
                      submitSwitch();
                    }}
                  >
                    <label>
                      <span>Passphrase for {switchTarget}</span>
                      <input
                        type="password"
                        bind:value={switchPassphrase}
                        autocomplete="current-password"
                      />
                    </label>
                    {#if switchError}
                      <p class="error small">{switchError}</p>
                    {/if}
                    <div class="menu-actions">
                      <button class="primary" type="submit" disabled={switchBusy}>
                        {switchBusy ? "Opening…" : "Open"}
                      </button>
                      <button
                        type="button"
                        onclick={() => {
                          switchTarget = "";
                          switchPassphrase = "";
                          switchError = "";
                        }}
                        disabled={switchBusy}
                      >
                        Cancel
                      </button>
                    </div>
                  </form>
                {/if}
                {#if $activeIdentity}
                  <div class="menu-actions menu-footer">
                    <button
                      type="button"
                      class="danger"
                      onclick={lock}
                      disabled={switchBusy}
                    >
                      Lock active
                    </button>
                  </div>
                {/if}
              {/if}
            </div>
          {/if}
        </div>
      {/if}
      <div class="overflow-wrap">
        <button
          type="button"
          class="icon-button"
          onclick={toggleOverflow}
          aria-haspopup="true"
          aria-expanded={overflowOpen}
          aria-label="Open menu"
          title="Menu"
        >
          ⋯
        </button>
        {#if overflowOpen}
          <div class="menu overflow-menu" role="menu">
            <button type="button" class="overflow-item" onclick={() => navTo("/")}>Chat</button>
            <button type="button" class="overflow-item" onclick={() => navTo("/contacts")}>Contacts</button>
            <button type="button" class="overflow-item" onclick={() => navTo("/identities")}>Identities</button>
            <button type="button" class="overflow-item" onclick={() => navTo("/settings")}>Settings</button>
            <button type="button" class="overflow-item" onclick={() => navTo("/about")}>About</button>
          </div>
        {/if}
      </div>
    </div>
  </header>

  <main class="content" class:full={page.url.pathname !== "/"}>
    {@render children()}
  </main>
</div>

<style>
  :global(:root) {
    font-family:
      -apple-system,
      BlinkMacSystemFont,
      "Segoe UI",
      Roboto,
      Inter,
      Helvetica,
      Arial,
      sans-serif;
    font-size: 14px;
    color: var(--text);
    background: var(--bg);
    --text: #1a1a1f;
    --text-strong: #15161c;
    --border: #e2e3e8;
    --border-soft: #ecedf1;
    --border-accent: #d6e0f9;
    --muted: #6b6b73;
    --muted-strong: #4a4a52;
    --accent: #2c5fe1;
    --accent-strong: #1f4bc6;
    --accent-soft: #eaf0ff;
    --accent-softer: #f4f7ff;
    --danger: #c62828;
    --danger-soft: #fdecea;
    --danger-border: #f7c8c4;
    --warn: #c08800;
    --pass: #2e7d32;
    --pass-soft: #e6f4ea;
    --pass-border: #cdebd6;
    --bg: #f4f5f8;
    --surface: #ffffff;
    --surface-alt: #fafbfc;
    --bubble-mine: #2c5fe1;
    --bubble-mine-text: #ffffff;
    --bubble-theirs: #ffffff;
    --bubble-theirs-text: #1a1a1f;
    --code-bg: rgba(15, 18, 30, 0.05);
    --row-hover: rgba(15, 18, 30, 0.03);
    --shadow-sm: 0 1px 2px rgba(15, 18, 30, 0.04);
    --shadow-md: 0 4px 16px rgba(15, 18, 30, 0.08);
    --radius-sm: 6px;
    --radius-md: 10px;
    --radius-lg: 14px;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --text: #e6e8ed;
      --text-strong: #f4f6fa;
      --border: #353c4a;
      --border-soft: #2c3340;
      --border-accent: #3a4870;
      --muted: #8a93a3;
      --muted-strong: #b0b8c6;
      --accent: #6691ff;
      --accent-strong: #82a4ff;
      --accent-soft: #2a3450;
      --accent-softer: #232b3f;
      --danger: #ef6b6b;
      --danger-soft: #3a2326;
      --danger-border: #5a2e30;
      --warn: #e6b566;
      --pass: #6cd28f;
      --pass-soft: #1f3327;
      --pass-border: #2f5a3c;
      --bg: #1a1d24;
      --surface: #232730;
      --surface-alt: #2a2f3a;
      --bubble-mine: #4f7bff;
      --bubble-mine-text: #ffffff;
      --bubble-theirs: #2a2f3a;
      --bubble-theirs-text: #e6e8ed;
      --code-bg: rgba(255, 255, 255, 0.07);
      --row-hover: rgba(255, 255, 255, 0.04);
      --shadow-sm: 0 1px 2px rgba(0, 0, 0, 0.3);
      --shadow-md: 0 6px 18px rgba(0, 0, 0, 0.45);
    }
  }
  :global(body) {
    margin: 0;
    padding: 0;
  }
  :global(*) {
    box-sizing: border-box;
  }
  :global(button) {
    font: inherit;
    padding: 0.45em 0.95em;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--text);
    cursor: pointer;
    min-height: 36px;
    transition: border-color 0.12s ease, background 0.12s ease,
      box-shadow 0.12s ease, color 0.12s ease;
  }
  :global(button:hover:not(:disabled)) {
    border-color: var(--accent);
    color: var(--accent);
  }
  :global(button:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  :global(button:disabled) {
    cursor: not-allowed;
    opacity: 0.55;
  }
  :global(button.primary) {
    background: var(--accent);
    color: #fff;
    border-color: var(--accent);
    box-shadow: var(--shadow-sm);
  }
  :global(button.primary:hover:not(:disabled)) {
    background: var(--accent-strong);
    border-color: var(--accent-strong);
    color: #fff;
  }
  :global(button.danger) {
    background: #d93636;
    color: #fff;
    border-color: #b71d1d;
  }
  :global(button.danger:hover:not(:disabled)) {
    background: #b71d1d;
    color: #fff;
  }
  :global(input[type="text"], input[type="password"], input[type="number"], input[type="email"], textarea, select) {
    font: inherit;
    padding: 0.5em 0.7em;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--text);
    width: 100%;
    transition: border-color 0.12s ease, box-shadow 0.12s ease;
  }
  :global(input::placeholder, textarea::placeholder) {
    color: var(--muted);
    opacity: 0.7;
  }
  :global(input[type="text"]:focus, input[type="password"]:focus, input[type="number"]:focus, input[type="email"]:focus, textarea:focus, select:focus) {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  :global(table) {
    width: 100%;
    border-collapse: collapse;
  }
  :global(th, td) {
    text-align: left;
    padding: 0.55em 0.75em;
    border-bottom: 1px solid var(--border-soft);
    font-size: 13px;
  }
  :global(th) {
    color: var(--muted);
    font-weight: 600;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  :global(code) {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px;
    background: var(--code-bg);
    padding: 0.12em 0.35em;
    border-radius: 4px;
    word-break: break-all;
  }
  :global(a) {
    color: var(--accent);
    text-decoration: none;
  }
  :global(a:hover) {
    text-decoration: underline;
  }
  :global(.error) {
    color: var(--danger);
  }
  :global(.warn) {
    color: var(--warn);
  }
  :global(.pass) {
    color: var(--pass);
  }
  :global(.muted) {
    color: var(--muted);
  }
  :global(label) {
    display: block;
    margin-bottom: 0.85em;
  }
  :global(label > span) {
    display: block;
    font-size: 12px;
    color: var(--muted);
    margin-bottom: 0.3em;
    font-weight: 500;
  }
  :global(h1) {
    font-weight: 700;
    letter-spacing: -0.01em;
  }
  :global(h2, h3) {
    letter-spacing: -0.005em;
  }

  .app {
    display: grid;
    grid-template-rows: 52px 1fr;
    height: 100vh;
    background: var(--bg);
  }
  .topbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 1rem;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    position: relative;
    z-index: 5;
  }
  .brand {
    font-weight: 700;
    font-size: 15px;
    letter-spacing: -0.01em;
    color: var(--text-strong);
    text-decoration: none;
  }
  .brand:hover {
    text-decoration: none;
    color: var(--accent);
  }
  .brand::first-letter {
    color: var(--accent);
  }
  .header-actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .identity-wrap, .overflow-wrap {
    position: relative;
  }
  .identity-button {
    background: var(--surface-alt);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0.35em 0.85em;
    display: inline-flex;
    align-items: center;
    gap: 0.35em;
    font-size: 12.5px;
    min-height: 32px;
  }
  .identity-button .user {
    font-weight: 600;
  }
  .identity-button .at,
  .identity-button .domain {
    color: var(--muted);
  }
  .identity-button .locked {
    color: var(--danger);
  }
  .identity-button .chev {
    color: var(--muted);
    font-size: 10px;
    margin-left: 0.2em;
  }
  .icon-button {
    width: 36px;
    min-height: 36px;
    padding: 0;
    border-radius: 50%;
    font-size: 18px;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 10;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-md);
    padding: 0.4rem;
  }
  .identity-menu {
    width: 320px;
    padding: 0.5rem;
  }
  .overflow-menu {
    min-width: 180px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .overflow-item {
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    padding: 0.5em 0.7em;
    border-radius: 6px;
  }
  .overflow-item:hover:not(:disabled) {
    background: var(--accent-softer);
    border-color: var(--accent);
  }
  .menu-empty {
    margin: 0;
    padding: 0.4rem 0.6rem;
  }
  .menu-list {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .menu-list li {
    margin: 0;
  }
  .menu-item {
    width: 100%;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 6px;
    padding: 0.4em 0.6em;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5em;
  }
  .menu-item:hover:not(:disabled) {
    background: var(--accent-softer);
    border-color: var(--accent);
  }
  .menu-item.selected {
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .menu-item.active {
    background: var(--surface-alt);
  }
  .menu-name {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
  }
  .menu-tag {
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.04em;
  }
  .menu-form {
    margin-top: 0.5rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
  }
  .menu-actions {
    display: flex;
    gap: 0.4rem;
  }
  .menu-footer {
    margin-top: 0.5rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
  }
  .small {
    font-size: 12px;
  }
  .content {
    overflow: hidden;
    min-height: 0;
  }
  /* Sub-routes (Settings, Identities, etc.) keep the legacy padded
     layout. The chat shell at `/` paints edge-to-edge. */
  .content.full {
    overflow: auto;
    padding: 1.5rem 1.75rem 2.25rem;
  }
  @media (max-width: 700px) {
    .content.full {
      padding: 1rem 1rem 1.5rem;
    }
  }
</style>
