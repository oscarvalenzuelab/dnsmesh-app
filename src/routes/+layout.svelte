<script lang="ts">
  import { onMount } from "svelte";
  import { get } from "svelte/store";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import {
    activeIdentity,
    publishedStatus,
    refreshActiveIdentity,
  } from "$lib/stores/identity";
  import { clearInbox, hydrateInbox } from "$lib/stores/inbox";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import {
    api,
    isCommandError,
    type IdentitySummary,
  } from "$lib/api";

  let { children } = $props();

  // Header switcher state.
  let identities = $state<IdentitySummary[]>([]);
  let switchTarget = $state<string>("");
  let switchPassphrase = $state<string>("");
  let switchBusy = $state<boolean>(false);
  let switchError = $state<string>("");
  let menuOpen = $state<boolean>(false);

  // True when the user has no identities and nothing unlocked. Drives
  // sidebar dimming and the bounce to the onboarding wizard.
  let firstRun = $derived(identities.length === 0 && !$activeIdentity);

  onMount(async () => {
    await refreshActiveIdentity();
    await reloadList();
    if (get(activeIdentity)) {
      // Hydrate so the Inbox shows pinned-contact attribution on first paint.
      void hydrateInbox();
      void refreshContacts();
    } else if (
      identities.length === 0 &&
      page.url.pathname !== "/identities"
    ) {
      // No identities on disk; bounce to the onboarding wizard.
      void goto("/identities?onboarding=1", { replaceState: true });
    }
  });

  // No-op for dimmed sidebar links during onboarding.
  function blockNav(e: Event) {
    e.preventDefault();
  }

  async function reloadList() {
    try {
      identities = await api.listIdentities();
    } catch {
      // Best-effort: the Identities page surfaces real errors.
      identities = [];
    }
  }

  function toggleMenu() {
    menuOpen = !menuOpen;
    switchError = "";
    if (menuOpen) {
      void reloadList();
    } else {
      switchTarget = "";
      switchPassphrase = "";
    }
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
      switchTarget = "";
      menuOpen = false;
      // Wipe in-memory state so the previous identity doesn't bleed through.
      clearInbox();
      await refreshActiveIdentity();
      await reloadList();
      void hydrateInbox();
      contacts.set([]);
      void refreshContacts();
    } catch (err) {
      switchError = isCommandError(err) ? err.message : String(err);
    } finally {
      switchBusy = false;
    }
  }

  async function lock() {
    // Clear the store first so the UI flips immediately even if the
    // backend call hangs; every route gates on $activeIdentity.
    activeIdentity.set(null);
    publishedStatus.set(null);
    clearInbox();
    contacts.set([]);
    menuOpen = false;
    switchTarget = "";
    switchPassphrase = "";

    // Bounce to Inbox; Compose/Settings retain form state on a gate flip.
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

  // `alwaysEnabled: true` exempts an entry from first-run dimming.
  const navItems = [
    { href: "/", label: "Inbox" },
    { href: "/compose", label: "Compose" },
    { href: "/contacts", label: "Contacts" },
    { href: "/identities", label: "Identities" },
    { href: "/settings", label: "Settings" },
    { href: "/about", label: "About", alwaysEnabled: true },
  ];
</script>

<div class="app">
  <header class="topbar">
    <div class="brand">DNSMesh</div>
    <div class="active-identity">
      {#if !firstRun}
      <button
        type="button"
        class="identity-button"
        onclick={toggleMenu}
        aria-haspopup="true"
        aria-expanded={menuOpen}
      >
        {#if $activeIdentity}
          <span class="user">{$activeIdentity.username}</span>
          <span class="at">@</span>
          <span class="domain">{$activeIdentity.domain}</span>
        {:else}
          <span class="locked">No identity unlocked</span>
        {/if}
        <span class="chev" aria-hidden="true">{menuOpen ? "▲" : "▼"}</span>
      </button>
      {#if menuOpen}
        <div class="identity-menu" role="menu">
          {#if identities.length === 0}
            <p class="muted small menu-empty">
              No identities yet. Create one from the Identities page.
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
      {/if}
    </div>
  </header>

  <nav class="sidebar">
    {#each navItems as item}
      {@const dimmed = firstRun && !item.alwaysEnabled}
      <a
        href={item.href}
        class:active={page.url.pathname === item.href && !dimmed}
        class:disabled={dimmed}
        title={dimmed
          ? "Create your first identity to continue."
          : undefined}
        onclick={dimmed ? blockNav : undefined}
        data-sveltekit-preload-data="off">{item.label}</a
      >
    {/each}
  </nav>

  <main class="content">
    {@render children()}
  </main>
</div>

<style>
  :global(:root) {
    /* System font stack with Inter as fallback. */
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
    --code-bg: rgba(15, 18, 30, 0.05);
    --row-hover: rgba(15, 18, 30, 0.03);
    --shadow-sm: 0 1px 2px rgba(15, 18, 30, 0.04);
    --shadow-md: 0 4px 16px rgba(15, 18, 30, 0.08);
    --radius-sm: 6px;
    --radius-md: 10px;
    --radius-lg: 14px;
  }
  /* OS-driven dark mode (no manual toggle). */
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
    grid-template-columns: 208px 1fr;
    grid-template-rows: 52px 1fr;
    grid-template-areas:
      "topbar topbar"
      "sidebar content";
    height: 100vh;
    background: var(--bg);
  }
  .topbar {
    grid-area: topbar;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 1.25rem;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    position: relative;
  }
  .brand {
    font-weight: 700;
    font-size: 15px;
    letter-spacing: -0.01em;
    color: var(--text-strong);
  }
  .brand::first-letter {
    color: var(--accent);
  }
  .active-identity {
    font-size: 13px;
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
  .identity-menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 10;
    width: 320px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-md);
    padding: 0.5rem;
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
  .sidebar {
    grid-area: sidebar;
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border-right: 1px solid var(--border);
    padding: 0.75rem 0.55rem;
    gap: 1px;
  }
  .sidebar a {
    color: var(--text);
    text-decoration: none;
    padding: 0.55em 0.85rem;
    border-radius: var(--radius-sm);
    font-size: 13px;
    font-weight: 500;
    display: flex;
    align-items: center;
    gap: 0.55rem;
    transition: background 0.12s ease, color 0.12s ease;
  }
  .sidebar a:hover {
    background: var(--surface-alt);
    text-decoration: none;
  }
  .sidebar a.active {
    background: var(--accent-soft);
    color: var(--accent-strong);
    font-weight: 600;
  }
  /* First-run: nav links render dimmed and clicks are no-ops. */
  .sidebar a.disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .sidebar a.disabled:hover {
    background: transparent;
    text-decoration: none;
  }
  .content {
    grid-area: content;
    overflow: auto;
    padding: 1.5rem 1.75rem 2.25rem;
  }
  /* Narrow viewport: collapse the sidebar into a horizontal strip. */
  @media (max-width: 700px) {
    .app {
      grid-template-columns: 1fr;
      grid-template-rows: 52px auto 1fr;
      grid-template-areas:
        "topbar"
        "sidebar"
        "content";
    }
    .sidebar {
      flex-direction: row;
      overflow-x: auto;
      padding: 0.4rem 0.6rem;
      border-right: none;
      border-bottom: 1px solid var(--border);
      gap: 0.25rem;
    }
    .sidebar a {
      white-space: nowrap;
      padding: 0.45em 0.75rem;
      font-size: 12.5px;
    }
    .content {
      padding: 1rem 1rem 1.5rem;
    }
  }
</style>
