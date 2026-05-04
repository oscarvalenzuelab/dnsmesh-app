<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "$lib/api";

  // About is reachable without an unlocked identity so onboarding
  // users can read it. Version is fetched from the host via the
  // `version` command rather than hard-coded.

  let version = $state<string>("");
  let versionError = $state<string>("");

  onMount(async () => {
    try {
      version = await api.version();
    } catch (err) {
      versionError = String(err);
    }
  });
</script>

<section>
  <header class="page-header">
    <h1>DNSMesh</h1>
    <p class="tagline">End-to-end encrypted messaging over DNS.</p>
  </header>

  <dl class="meta">
    <dt>Version</dt>
    <dd>
      {#if version}
        <code>{version}</code>
      {:else if versionError}
        <span class="muted small">unavailable ({versionError})</span>
      {:else}
        <span class="muted small">loading…</span>
      {/if}
    </dd>
    <dt>Author</dt>
    <dd>Oscar Valenzuela</dd>
    <dt>License</dt>
    <dd>MIT</dd>
  </dl>

  <h2>Links</h2>
  <ul class="links">
    <li>
      Desktop client repo:
      <a
        href="https://github.com/oscarvalenzuelab/dnsmesh-desktop"
        target="_blank"
        rel="noopener noreferrer"
      >github.com/oscarvalenzuelab/dnsmesh-desktop</a>
    </li>
    <li>
      Rust SDK + CLI:
      <a
        href="https://github.com/oscarvalenzuelab/dnsmesh-rs"
        target="_blank"
        rel="noopener noreferrer"
      >github.com/oscarvalenzuelab/dnsmesh-rs</a>
    </li>
    <li>
      Protocol spec:
      <a
        href="https://github.com/oscarvalenzuelab/DNSMeshProtocol"
        target="_blank"
        rel="noopener noreferrer"
      >github.com/oscarvalenzuelab/DNSMeshProtocol</a>
    </li>
    <li>
      Report a bug:
      <a
        href="https://github.com/oscarvalenzuelab/dnsmesh-desktop/issues"
        target="_blank"
        rel="noopener noreferrer"
      >github.com/oscarvalenzuelab/dnsmesh-desktop/issues</a>
    </li>
  </ul>

  <h2>About DNSMesh</h2>
  <p class="prose">
    DMP is an open protocol for moving end-to-end encrypted messages between
    two people using DNS as the transport. Identities, prekeys, and mailbox
    slots all resolve as DNS records — there is no central server, no app
    store, and no gatekeeper between sender and recipient.
  </p>
</section>

<style>
  section {
    max-width: 640px;
  }
  .page-header {
    margin-bottom: 1.25rem;
  }
  h1 {
    margin: 0;
    font-size: 1.4rem;
  }
  .tagline {
    margin: 0.4rem 0 0;
    color: var(--muted-strong);
    font-size: 14px;
  }
  h2 {
    margin: 1.5rem 0 0.5rem;
    font-size: 0.95rem;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .small {
    font-size: 12px;
  }
  .meta {
    display: grid;
    grid-template-columns: max-content 1fr;
    column-gap: 1.2rem;
    row-gap: 0.4rem;
    margin: 0;
    padding: 1rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .meta dt {
    color: var(--muted);
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    align-self: center;
  }
  .meta dd {
    margin: 0;
    font-size: 13px;
  }
  .links {
    margin: 0;
    padding-left: 1.1rem;
    font-size: 13px;
    line-height: 1.7;
  }
  .links li {
    margin: 0;
  }
  .prose {
    font-size: 13.5px;
    line-height: 1.55;
    color: var(--text);
    max-width: 60ch;
  }
</style>
