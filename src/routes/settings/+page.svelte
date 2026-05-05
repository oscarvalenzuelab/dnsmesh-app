<script lang="ts">
  import { onMount } from "svelte";
  import {
    activeIdentity,
    refreshActiveIdentity,
  } from "$lib/stores/identity";
  import { clearInbox } from "$lib/stores/inbox";
  import {
    DEFAULT_SENT_TTL_HOURS,
    getSentTtl,
    hydrateSent,
    setSentTtl,
    type SentTtlHours,
  } from "$lib/stores/sent";
  import {
    api,
    isCommandError,
    type IdentityConfigView,
    type DiscoveredNode,
    type EffectiveResolvers,
  } from "$lib/api";

  let cfg = $state<IdentityConfigView | null>(null);
  let busy = $state<boolean>(false);
  let error = $state<string>("");
  let info = $state<string>("");

  // What resolver pool the active identity is actually hitting. Refreshed
  // alongside the config so the user can always see the source of truth
  // without grepping the YAML on disk.
  let effectiveResolvers = $state<EffectiveResolvers | null>(null);

  // Form bindings
  let publishEnabled = $state<boolean>(false);
  let zone = $state<string>("");
  let server = $state<string>("");
  let tsigKeyName = $state<string>("");
  let tsigAlgorithm = $state<string>("hmac-sha256");
  let tsigSecretPath = $state<string>("");
  let resolversText = $state<string>("");

  // Sent-message retention TTL. Per-identity, localStorage-backed for
  // the proto; promote to a Rust-side setting alongside `sent.jsonl`.
  let sentTtl = $state<SentTtlHours>(DEFAULT_SENT_TTL_HOURS);

  $effect(() => {
    void $activeIdentity;
    loadConfig();
    if ($activeIdentity) {
      sentTtl = getSentTtl($activeIdentity.username);
    }
  });

  function onSentTtlChange(value: string) {
    if (!$activeIdentity) return;
    const parsed = Number(value) as SentTtlHours;
    sentTtl = parsed;
    setSentTtl($activeIdentity.username, parsed);
    // Sweep applies immediately; rehydrate so any expired rows drop now.
    hydrateSent($activeIdentity.username);
  }

  onMount(loadConfig);

  async function loadConfig() {
    if (!$activeIdentity) {
      cfg = null;
      effectiveResolvers = null;
      return;
    }
    try {
      cfg = await api.getIdentityConfig($activeIdentity.username);
      hydrateForm();
    } catch (err) {
      error = isCommandError(err) ? err.message : String(err);
    }
    // Resolver display is best-effort and shouldn't block the rest of
    // the form from rendering if it fails.
    try {
      effectiveResolvers = await api.effectiveResolvers();
    } catch (err) {
      console.warn("effective_resolvers failed", err);
      effectiveResolvers = null;
    }
  }

  function hydrateForm() {
    publishEnabled = cfg?.publish !== null && cfg?.publish !== undefined;
    if (cfg?.publish) {
      zone = cfg.publish.zone;
      server = cfg.publish.server;
      tsigKeyName = cfg.publish.tsig_key_name;
      tsigAlgorithm = cfg.publish.tsig_algorithm;
      tsigSecretPath = cfg.publish.tsig_secret_path;
    } else {
      zone = "";
      server = "";
      tsigKeyName = "";
      tsigAlgorithm = "hmac-sha256";
      tsigSecretPath = "";
    }
    // Hydrating the form is "trust on-disk truth"; drop any pending
    // in-memory secret so a reload after a successful save can't
    // accidentally re-use it.
    pendingSecretBase64 = "";
    registerSuccess = "";
    resolversText = (cfg?.resolvers ?? []).join("\n");
    // Network-driven prefill — fire-and-forget. Only fills fields the
    // user hasn't already typed in / hasn't already had hydrated from
    // disk above. See `prefillFromDiscovery` for the policy.
    void prefillFromDiscovery();
  }

  // Suggested operator endpoint discovered from the active identity's
  // domain heartbeat. Surfaced as an inline note alongside the TSIG
  // fields so the user knows whom to contact for an operator-issued key.
  let suggestedOperator = $state<string>("");

  // Top-of-list discovered node — used as the registration target for
  // the "Register with <node>" button. We hold the whole record (not
  // just the endpoint) so we can show version + freshness info inline.
  let topDiscovered = $state<DiscoveredNode | null>(null);

  // Passphrase is re-prompted on every register (the unlocked client
  // doesn't expose its passphrase to page state).
  let showRegisterForm = $state<boolean>(false);
  let registerPassphrase = $state<string>("");
  let registerBusy = $state<boolean>(false);
  let registerError = $state<string>("");
  let registerSuccess = $state<string>("");

  // Once a publish block is on disk we know a TSIG key is wired; default
  // to a status row + Re-register link instead of the call-to-action.
  let alreadyRegistered = $derived<boolean>(
    !!cfg?.publish?.tsig_key_name && cfg.publish.tsig_key_name.trim().length > 0,
  );
  let showReregister = $state<boolean>(false);

  // Fill in publish defaults from the active identity's domain. Policy
  // is "fill if empty" — typed or hydrated values are never overwritten.
  async function prefillFromDiscovery() {
    if (!$activeIdentity) return;
    const domain = $activeIdentity.domain.trim();
    if (!domain) return;
    if (!zone) {
      zone = domain;
    }
    try {
      const found = await api.discoverNodes(domain);
      if (found.length === 0) {
        topDiscovered = null;
        return;
      }
      const top = found[0];
      topDiscovered = top;
      if (!server) {
        server = endpointHostPort(top.endpoint);
      }
      suggestedOperator = top.endpoint;
    } catch (err) {
      // Discovery is best-effort; surface nothing on failure.
      console.warn("settings prefill discovery failed", err);
    }
    if (!tsigAlgorithm) {
      tsigAlgorithm = "hmac-sha256";
    }
  }

  // Extract host:port from a heartbeat URL. Mirror in the create-identity flow.
  function endpointHostPort(endpoint: string): string {
    try {
      const url = new URL(endpoint);
      return url.host || endpoint;
    } catch {
      return endpoint;
    }
  }

  // Mint a TSIG key against the top-discovered node and auto-fill the
  // publish form. The secret is held in memory until the user clicks Save.
  async function registerWithNode() {
    registerError = "";
    registerSuccess = "";
    if (!$activeIdentity) {
      registerError = "Unlock an identity first.";
      return;
    }
    if (!topDiscovered) {
      registerError = "No live node discovered for this identity's domain.";
      return;
    }
    if (!registerPassphrase) {
      registerError = "Passphrase required to sign the challenge.";
      return;
    }
    registerBusy = true;
    try {
      const result = await api.registerTsig({
        endpoint: topDiscovered.endpoint,
        subject: `${$activeIdentity.username}@${$activeIdentity.domain}`,
        passphrase: registerPassphrase,
      });
      // Auto-fill the publish form; Save materialises the secret to disk.
      publishEnabled = true;
      zone = result.dns_zone || $activeIdentity.domain;
      server = `${result.dns_server}:53`;
      tsigKeyName = result.key_name;
      tsigAlgorithm = result.algorithm || "hmac-sha256";
      // Stash the base64 secret on the form via a hidden field so
      // `save()` can forward it to the host as `tsig_secret_base64`.
      pendingSecretBase64 = result.secret_base64;
      tsigSecretPath = "(will be written to <identity>/tsig.key on Save)";
      registerSuccess = `REGISTERED with ${topDiscovered.endpoint} — TSIG key ${result.key_name} ready. Click Save to materialise.`;
      registerPassphrase = "";
      showRegisterForm = false;
    } catch (err) {
      registerError = isCommandError(err) ? err.message : String(err);
    } finally {
      registerBusy = false;
    }
  }

  // Set on `registerWithNode` success, consumed by `save()`. Cleared on
  // subsequent loads so a stale secret can't be re-submitted.
  let pendingSecretBase64 = $state<string>("");

  // Backup export. Archive contains every secret the identity needs to
  // send and receive; the UI warns and the user owns vault storage.
  let backupOutputPath = $state<string>("");
  let backupBusy = $state<boolean>(false);
  let backupError = $state<string>("");
  let backupSuccess = $state<string>("");

  function defaultBackupOutputPath(username: string): string {
    const stamp = Math.floor(Date.now() / 1000);
    return `~/Desktop/${username}-${stamp}.dmp-backup.tar.gz`;
  }

  async function exportBackup() {
    backupError = "";
    backupSuccess = "";
    if (!$activeIdentity) {
      backupError = "Unlock an identity first.";
      return;
    }
    const outputPath = backupOutputPath.trim();
    if (!outputPath) {
      backupError = "Output path is required.";
      return;
    }
    backupBusy = true;
    try {
      const result = await api.exportIdentityBackup({
        username: $activeIdentity.username,
        output_path: outputPath,
      });
      backupSuccess =
        `Wrote ${result.archive_path} (${result.total_bytes} bytes, ` +
        `${result.file_count} file(s)). ` +
        `WARNING: this archive contains every secret your identity ` +
        `needs to send and receive. Anyone with the archive + your ` +
        `passphrase can impersonate you. Store it in an encrypted vault.`;
      backupOutputPath = "";
    } catch (err) {
      backupError = isCommandError(err) ? err.message : String(err);
    } finally {
      backupBusy = false;
    }
  }

  // Pre-fill the suggested output path whenever the active identity
  // changes (and on first mount).
  $effect(() => {
    if ($activeIdentity && !backupOutputPath) {
      backupOutputPath = defaultBackupOutputPath($activeIdentity.username);
    }
  });

  async function save() {
    if (!$activeIdentity) return;
    error = "";
    info = "";
    busy = true;
    try {
      const resolvers = resolversText
        .split(/\s+/)
        .map((s) => s.trim())
        .filter(Boolean);
      // Two paths: `pendingSecretBase64` set means the host will
      // materialise the secret to disk; otherwise it uses `tsig_secret_path`.
      const publishBlock = publishEnabled
        ? pendingSecretBase64
          ? {
              zone: zone.trim(),
              server: server.trim(),
              tsig_key_name: tsigKeyName.trim(),
              tsig_algorithm: tsigAlgorithm.trim(),
              tsig_secret_base64: pendingSecretBase64,
            }
          : {
              zone: zone.trim(),
              server: server.trim(),
              tsig_key_name: tsigKeyName.trim(),
              tsig_algorithm: tsigAlgorithm.trim(),
              tsig_secret_path: tsigSecretPath.trim(),
            }
        : null;
      const args = {
        username: $activeIdentity.username,
        resolvers: resolvers.length > 0 ? resolvers : null,
        publish: publishBlock,
      };
      cfg = await api.updatePublishConfig(args);
      hydrateForm();
      // Host drops the in-memory client on publish-settings change so
      // new TSIG creds don't get applied silently. Mirror on the
      // frontend: drop inbox + active identity so the UI re-prompts.
      clearInbox();
      await refreshActiveIdentity();
      info =
        "Publish settings saved — please unlock again to apply.";
    } catch (err) {
      error = isCommandError(err) ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<section>
  <header class="page-header">
    <h1>Settings</h1>
  </header>

  {#if !$activeIdentity}
    <p class="muted">
      Unlock an identity from <a href="/identities">Identities</a> to view its
      settings.
    </p>
  {:else}
    {#if error}<p class="error">{error}</p>{/if}
    {#if info}<p class="pass">{info}</p>{/if}

    <h2>Sent-message retention</h2>
    <p class="muted small">
      Sent messages are kept locally so chat threads show both sides. Older
      sends are auto-deleted after this period (24h max).
    </p>
    <label>
      <span>Auto-delete sent messages after</span>
      <select
        value={String(sentTtl)}
        onchange={(e) =>
          onSentTtlChange((e.currentTarget as HTMLSelectElement).value)}
      >
        <option value="1">1 hour</option>
        <option value="6">6 hours</option>
        <option value="12">12 hours</option>
        <option value="24">24 hours</option>
      </select>
    </label>

    <h2>Publish destination (TSIG-signed UPDATE)</h2>
    <p class="muted small">
      Required for publishing your identity, refreshing prekeys, and
      sending. Leaving it disabled keeps the client read-only — useful for
      a fresh install before the operator wires up authority.
    </p>

    {#if topDiscovered}
      <div class="register-box">
        {#if registerSuccess}
          <p class="pass small">{registerSuccess}</p>
        {/if}
        {#if alreadyRegistered && !showRegisterForm && !showReregister}
          <p class="small">
            <span class="badge live">REGISTERED</span>
            with <code>{server || topDiscovered.endpoint}</code>
            {#if cfg?.publish}
              · key <code>{cfg.publish.tsig_key_name}</code>
            {/if}
          </p>
          <button
            type="button"
            class="link-button"
            onclick={() => {
              showReregister = true;
              showRegisterForm = true;
              registerError = "";
            }}
            disabled={busy || registerBusy}
          >
            Re-register
          </button>
        {:else if !showRegisterForm}
          <button
            type="button"
            onclick={() => {
              showRegisterForm = true;
              registerError = "";
            }}
            disabled={busy || registerBusy}
          >
            Register with {topDiscovered.endpoint}
          </button>
          <p class="muted small">
            Provisions a TSIG key on this node in one click — the node
            issues a fresh key tied to your identity. Replaces the
            manual key-name + secret-path fields below.
          </p>
        {:else}
          <p class="muted small">
            Enter your passphrase so we can sign the operator's
            challenge. The passphrase is sent to the local Tauri
            process only — it is not transmitted to the node.
          </p>
          <label>
            <span>Passphrase</span>
            <input type="password" bind:value={registerPassphrase} />
          </label>
          <div class="actions">
            <button
              type="button"
              class="primary"
              onclick={registerWithNode}
              disabled={registerBusy || busy}
            >
              {registerBusy ? "Registering…" : "Register"}
            </button>
            <button
              type="button"
              onclick={() => {
                showRegisterForm = false;
                showReregister = false;
                registerPassphrase = "";
                registerError = "";
              }}
              disabled={registerBusy}
            >
              Cancel
            </button>
          </div>
          {#if registerError}
            <p class="error small">{registerError}</p>
          {/if}
        {/if}
      </div>
    {/if}

    <form
      class="add-form"
      onsubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <label class="check">
        <input type="checkbox" bind:checked={publishEnabled} />
        <span>Enable publishing</span>
      </label>

      {#if publishEnabled}
        <p class="muted small auto-fields-hint">
          The fields below are populated automatically by the
          <strong>Register</strong> flow. You usually don't need to edit them.
        </p>
        <label>
          <span>Zone (e.g. dmp.example.com)</span>
          <input type="text" bind:value={zone} />
        </label>
        <label>
          <span>Server (host:port)</span>
          <input type="text" bind:value={server} placeholder="ns1.example.com:53" />
        </label>
        <label>
          <span>TSIG key name</span>
          <input type="text" bind:value={tsigKeyName} />
        </label>
        <details class="advanced-disclosure">
          <summary>Advanced (manual configuration)</summary>
          <p class="muted small">
            Only edit these if you minted the TSIG key out-of-band — for
            example, you have shell access on the operator's DNS server.
            The Register flow above sets both fields automatically.
          </p>
          <label>
            <span>TSIG algorithm</span>
            <select bind:value={tsigAlgorithm}>
              <option value="hmac-sha256">hmac-sha256</option>
              <option value="hmac-sha384">hmac-sha384</option>
              <option value="hmac-sha512">hmac-sha512</option>
            </select>
          </label>
          <label>
            <span>TSIG secret file path</span>
            <input
              type="text"
              bind:value={tsigSecretPath}
              placeholder="/path/to/tsig.key (base64:, hex:, or raw)"
            />
          </label>
        </details>
      {/if}

      <h2>Resolver overrides</h2>
      {#if effectiveResolvers}
        <p class="muted small effective-line">
          <strong>Currently using:</strong>
          {effectiveResolvers.addresses.join(", ")}
          <span class="muted">
            ({effectiveResolvers.source === "override"
              ? "your override"
              : "well-known public resolvers"})
          </span>
        </p>
      {/if}
      <p class="muted small">
        Optional. One IP literal per line. Leave blank to use the well-known
        public resolvers.
      </p>
      <label>
        <span>Resolvers</span>
        <textarea rows="4" bind:value={resolversText}></textarea>
      </label>

      <div class="actions">
        <button class="primary" type="submit" disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </button>
      </div>
    </form>

    <h2>Backup &amp; restore</h2>
    <p class="muted small">
      Bundles the active identity's config, sqlite database, TSIG
      secret, and persistent inbox into a single
      <code>.dmp-backup.tar.gz</code> archive. Restore happens from the
      <a href="/identities">Identities</a> page.
    </p>
    <p class="warn small backup-warning">
      <strong>The archive is NOT encrypted.</strong>
      It contains every secret the identity needs to send and receive
      messages. Anyone with the archive plus your passphrase can
      impersonate you. Store it in an encrypted vault (e.g. an
      encrypted disk image, a password-manager file attachment, or
      <code>age</code>/<code>gpg</code> on top).
    </p>

    {#if backupError}
      <p class="error small">{backupError}</p>
    {/if}
    {#if backupSuccess}
      <p class="pass small">{backupSuccess}</p>
    {/if}

    <form
      class="add-form"
      onsubmit={(e) => {
        e.preventDefault();
        exportBackup();
      }}
    >
      <label>
        <span>Output path</span>
        <input
          type="text"
          bind:value={backupOutputPath}
          placeholder="~/Desktop/{$activeIdentity.username}-XXXXXXX.dmp-backup.tar.gz"
          autocomplete="off"
        />
        <small class="muted">
          The host auto-appends <code>.dmp-backup.tar.gz</code> if you
          omit it. Tildes (<code>~</code>) are NOT expanded — supply an
          absolute path.
        </small>
      </label>
      <div class="actions">
        <button type="submit" class="primary" disabled={backupBusy}>
          {backupBusy ? "Writing…" : "Export backup"}
        </button>
        <button
          type="button"
          onclick={() => {
            backupOutputPath = $activeIdentity
              ? defaultBackupOutputPath($activeIdentity.username)
              : "";
          }}
          disabled={backupBusy}
        >
          Reset path
        </button>
      </div>
    </form>

    <h2>Change passphrase</h2>
    <p class="warn">Not yet implemented.</p>
    <p class="muted">
      The passphrase is the input to the KDF that derives your long-term
      key, so changing it requires the SDK to re-derive and re-encrypt
      stored material. Tracked on the SDK roadmap.
    </p>
  {/if}
</section>

<style>
  .page-header {
    margin-bottom: 1rem;
  }
  h1 {
    margin: 0;
    font-size: 1.4rem;
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
  .add-form {
    max-width: 640px;
    padding: 1rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  label.check {
    display: flex;
    align-items: center;
    gap: 0.5em;
  }
  label.check input {
    width: auto;
  }
  textarea {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
  }
  .actions {
    margin-top: 0.75em;
    display: flex;
    gap: 0.4rem;
  }
  .register-box {
    max-width: 640px;
    margin-bottom: 1rem;
    padding: 0.8rem 1rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .badge {
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.06em;
    padding: 0.15em 0.5em;
    border-radius: 999px;
    border: 1px solid transparent;
  }
  .badge.live {
    color: var(--pass);
    border-color: var(--pass);
    background: rgba(46, 125, 50, 0.08);
  }
  .link-button {
    background: transparent;
    color: var(--accent);
    border-color: transparent;
    text-decoration: underline;
    padding: 0.4em 0.4em;
  }
  .link-button:hover:not(:disabled) {
    background: var(--accent-softer);
  }
  .auto-fields-hint {
    margin-top: 0;
    margin-bottom: 0.85em;
  }
  .advanced-disclosure {
    margin: 0.5em 0 0.85em;
    padding: 0.5em 0.75em;
    background: var(--surface-alt);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .advanced-disclosure summary {
    cursor: pointer;
    color: var(--muted);
    user-select: none;
    font-size: 12px;
    font-weight: 600;
  }
  .advanced-disclosure[open] summary {
    margin-bottom: 0.6em;
  }
  .effective-line {
    margin-top: 0;
    margin-bottom: 0.4em;
  }
  .backup-warning {
    padding: 0.6rem 0.75rem;
    border: 1px solid var(--danger-border, var(--border));
    border-radius: 6px;
    background: var(--danger-soft, var(--surface-alt));
  }
</style>
