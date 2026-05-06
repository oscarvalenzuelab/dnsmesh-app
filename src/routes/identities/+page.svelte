<script lang="ts">
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import {
    activeIdentity,
    publishedStatus,
    refreshActiveIdentity,
    refreshPublishedStatus,
  } from "$lib/stores/identity";
  import { clearInbox, hydrateInbox } from "$lib/stores/inbox";
  import { clearSent, hydrateSent } from "$lib/stores/sent";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import {
    api,
    isCommandError,
    type DiscoveredNode,
    type IdentitySummary,
    type KnownNodeStatus,
    type RegisteredTsig,
  } from "$lib/api";

  let identities = $state<IdentitySummary[]>([]);
  let busy = $state<boolean>(false);
  let error = $state<string>("");
  let info = $state<string>("");

  // Layout sets `?onboarding=1` for brand-new users. Strip it once an
  // identity exists so the page reverts to its normal layout.
  let onboarding = $derived(
    page.url.searchParams.get("onboarding") === "1" && identities.length === 0,
  );

  $effect(() => {
    if (
      page.url.searchParams.get("onboarding") === "1" &&
      identities.length > 0
    ) {
      void goto("/identities", { replaceState: true });
    }
  });

  // Create-identity wizard. Step 1 picks a node (curated list or
  // custom zone under Advanced); step 2 collects username + passphrase.
  // Submit chains create -> register -> publish.

  type CreateStep = "node" | "details";
  let createStep = $state<CreateStep>("node");

  // Curated DMP node list — default surface for step 1.
  let curatedNodes = $state<KnownNodeStatus[]>([]);
  let curatedLoading = $state<boolean>(false);
  let curatedError = $state<string>("");

  // Pick mode: curated list vs typed custom zone.
  type NodeMode = "curated" | "custom";
  let nodeMode = $state<NodeMode>("curated");

  let selectedCuratedZone = $state<string>("");

  // Custom-zone advanced disclosure state.
  let advancedOpen = $state<boolean>(false);

  // Step-1 state for the custom-zone path.
  let formDomain = $state<string>("");
  let discoveryResults = $state<DiscoveredNode[]>([]);
  let discoveryLoading = $state<boolean>(false);
  let discoveryAttempted = $state<boolean>(false);
  let selectedNodeEndpoint = $state<string>("");
  let discoveryDebounce: ReturnType<typeof setTimeout> | null = null;
  let discoveryToken = 0;

  // Step-2 state. Confirm-passphrase is required because a typo is
  // unrecoverable (passphrase is the KDF input).
  let formUsername = $state<string>("");
  let formPassphrase = $state<string>("");
  let formPassphraseConfirm = $state<string>("");

  // Per-step progress for the create → register → publish chain.
  type StageState = "pending" | "running" | "done" | "failed" | "skipped";
  let stageCreate = $state<StageState>("pending");
  let stageRegister = $state<StageState>("pending");
  let stagePublish = $state<StageState>("pending");
  let publishError = $state<string>("");
  let publishedSubject = $state<string>("");

  let passphraseMismatch = $derived(
    formPassphrase.length > 0 &&
      formPassphraseConfirm.length > 0 &&
      formPassphrase !== formPassphraseConfirm,
  );
  let createSubmitDisabled = $derived(
    formUsername.trim().length === 0 ||
      formPassphrase.length === 0 ||
      formPassphraseConfirm.length === 0 ||
      formPassphrase !== formPassphraseConfirm,
  );

  let selectedCuratedNode = $derived(
    nodeMode === "curated"
      ? curatedNodes.find((n) => n.zone === selectedCuratedZone) ?? null
      : null,
  );

  // Selected node, normalised across curated and custom paths.
  let selectedNode = $derived<DiscoveredNode | null>(
    nodeMode === "curated"
      ? selectedCuratedNode?.live ?? null
      : discoveryResults.find((n) => n.endpoint === selectedNodeEndpoint) ?? null,
  );

  // Zone the chosen node lives under — becomes the identity's domain.
  let selectedZone = $derived<string>(
    nodeMode === "curated"
      ? selectedCuratedNode?.zone ?? ""
      : formDomain.trim(),
  );

  let selectedEndpointDisplay = $derived<string>(
    selectedNode?.endpoint ?? "",
  );

  // Switch / unlock — both `init_or_unlock` under the hood.
  let switchTargetUsername = $state<string>("");
  let switchPassphrase = $state<string>("");

  // Refresh prekeys.
  let prekeyCount = $state<number>(50);
  let prekeyTtl = $state<number>(86400);

  // Import-from-CLI form (collapsed by default).
  let showImportCli = $state<boolean>(false);
  let importCliSourceDir = $state<string>("");
  let importCliOverrideUsername = $state<string>("");
  let importCliBusy = $state<boolean>(false);
  let importCliError = $state<string>("");

  // Import-from-backup form (collapsed by default).
  let showImportBackup = $state<boolean>(false);
  let importBackupArchive = $state<string>("");
  let importBackupOverrideUsername = $state<string>("");
  let importBackupBusy = $state<boolean>(false);
  let importBackupError = $state<string>("");

  // Export backup of the active identity (collapsed by default).
  let showExportBackup = $state<boolean>(false);
  let exportBackupOutputPath = $state<string>("");
  let exportBackupBusy = $state<boolean>(false);
  let exportBackupError = $state<string>("");

  function defaultExportBackupPath(username: string): string {
    const stamp = Math.floor(Date.now() / 1000);
    return `~/Desktop/${username}-${stamp}.dmp-backup.tar.gz`;
  }

  async function submitExportBackup() {
    exportBackupError = "";
    error = "";
    info = "";
    if (!$activeIdentity) {
      exportBackupError = "Unlock an identity first.";
      return;
    }
    const outputPath = exportBackupOutputPath.trim();
    if (!outputPath) {
      exportBackupError = "Output path is required (use Reset to suggest one).";
      return;
    }
    exportBackupBusy = true;
    try {
      const result = await api.exportIdentityBackup({
        username: $activeIdentity.username,
        output_path: outputPath,
      });
      info =
        `Exported backup of ${$activeIdentity.username}@${$activeIdentity.domain} ` +
        `(${result.file_count} files, ${result.total_bytes} bytes) to ${result.archive_path}. ` +
        `Store it in an encrypted vault. The archive is NOT encrypted.`;
      showExportBackup = false;
      exportBackupOutputPath = "";
    } catch (err) {
      exportBackupError = isCommandError(err) ? err.message : String(err);
    } finally {
      exportBackupBusy = false;
    }
  }

  async function submitImportCli() {
    importCliError = "";
    error = "";
    info = "";
    importCliBusy = true;
    try {
      const result = await api.importFromCli({
        source_dir: importCliSourceDir.trim() || null,
        override_username: importCliOverrideUsername.trim() || null,
      });
      const tsigNote = result.publish_imported
        ? " (TSIG secret carried over)"
        : " (read-only; no TSIG was configured in the CLI)";
      info =
        `Imported ${result.username}@${result.domain} from the CLI${tsigNote}. ` +
        `Click Open on the row below to unlock with your existing passphrase.`;
      showImportCli = false;
      importCliSourceDir = "";
      importCliOverrideUsername = "";
      await reloadList();
    } catch (err) {
      importCliError = isCommandError(err) ? err.message : String(err);
    } finally {
      importCliBusy = false;
    }
  }

  async function submitImportBackup() {
    importBackupError = "";
    error = "";
    info = "";
    if (!importBackupArchive.trim()) {
      importBackupError = "Path to the .dmp-backup.tar.gz archive is required.";
      return;
    }
    importBackupBusy = true;
    try {
      const result = await api.importIdentityBackup({
        archive_path: importBackupArchive.trim(),
        override_username: importBackupOverrideUsername.trim() || null,
      });
      info =
        `Restored ${result.username}@${result.domain} from backup ` +
        `(${result.file_count} file(s) extracted). ` +
        `Click Open on the row below to unlock with the original passphrase.`;
      showImportBackup = false;
      importBackupArchive = "";
      importBackupOverrideUsername = "";
      await reloadList();
    } catch (err) {
      importBackupError = isCommandError(err) ? err.message : String(err);
    } finally {
      importBackupBusy = false;
    }
  }

  // 500ms debounce for typed-zone discovery; `discoveryToken` guards
  // against late responses overwriting the list with stale data.
  function scheduleDiscovery(domain: string) {
    if (discoveryDebounce) {
      clearTimeout(discoveryDebounce);
      discoveryDebounce = null;
    }
    selectedNodeEndpoint = "";
    discoveryResults = [];
    discoveryAttempted = false;
    const trimmed = domain.trim();
    if (!trimmed) {
      discoveryLoading = false;
      return;
    }
    discoveryDebounce = setTimeout(() => {
      void runDiscovery(trimmed);
    }, 500);
  }

  async function runDiscovery(domain: string) {
    discoveryToken += 1;
    const myToken = discoveryToken;
    discoveryLoading = true;
    try {
      const found = await api.discoverNodes(domain);
      if (myToken !== discoveryToken) return;
      discoveryResults = found;
      discoveryAttempted = true;
      // Force a fresh pick so Continue doesn't light up before review.
      selectedNodeEndpoint = "";
    } catch (err) {
      if (myToken !== discoveryToken) return;
      console.warn("discover_nodes failed", err);
      discoveryResults = [];
      discoveryAttempted = true;
    } finally {
      if (myToken === discoveryToken) {
        discoveryLoading = false;
      }
    }
  }

  // Manual re-run of discovery without re-typing the domain.
  function manualDiscover() {
    if (discoveryDebounce) {
      clearTimeout(discoveryDebounce);
      discoveryDebounce = null;
    }
    const trimmed = formDomain.trim();
    if (!trimmed) return;
    void runDiscovery(trimmed);
  }

  function formatStaleness(secs: number): string {
    if (secs <= 0) return "expired";
    if (secs >= 3600) {
      const h = Math.floor(secs / 3600);
      const m = Math.floor((secs % 3600) / 60);
      return `${h}h ${m}m`;
    }
    if (secs >= 60) {
      const m = Math.floor(secs / 60);
      const s = secs % 60;
      return `${m}m ${s}s`;
    }
    return `${secs}s`;
  }

  // Domain edits invalidate any node previously selected.
  $effect(() => {
    scheduleDiscovery(formDomain);
  });

  onMount(async () => {
    // Curated-list fetch hits the network; run it in parallel with the
    // sqlite-only identity list.
    await Promise.all([reloadList(), loadCuratedNodes()]);
    syncCuratedSelectionFromUrl();
  });

  // Pre-select the curated entry matching `?node_zone=...`. If the zone
  // isn't curated, flip to custom mode and seed the input.
  function syncCuratedSelectionFromUrl() {
    const zoneParam = page.url.searchParams.get("node_zone");
    if (!zoneParam) return;
    if (curatedNodes.some((n) => n.zone === zoneParam)) {
      nodeMode = "curated";
      selectedCuratedZone = zoneParam;
    } else if (curatedNodes.length > 0 || curatedError) {
      // Wait for the curated list to land before falling back so a
      // slow first load doesn't prematurely flip into custom mode.
      nodeMode = "custom";
      advancedOpen = true;
      if (!formDomain) {
        formDomain = zoneParam;
      }
    }
  }

  async function loadCuratedNodes() {
    curatedLoading = true;
    curatedError = "";
    try {
      curatedNodes = await api.listKnownNodes();
      // Re-apply the URL hint after the list arrives.
      syncCuratedSelectionFromUrl();
    } catch (err) {
      curatedError = isCommandError(err) ? err.message : String(err);
    } finally {
      curatedLoading = false;
    }
  }

  async function reloadList() {
    try {
      identities = await api.listIdentities();
    } catch (err) {
      error = isCommandError(err) ? err.message : String(err);
    }
  }

  function continueToDetails() {
    error = "";
    info = "";
    if (!selectedNode) {
      error =
        nodeMode === "curated"
          ? "Pick a live DMP node before continuing."
          : "Pick a discovered node before continuing.";
      return;
    }
    if (!selectedZone) {
      error = "Selected node has no zone. Pick another node.";
      return;
    }
    createStep = "details";
  }

  function backToNode() {
    createStep = "node";
    // Keep node/domain selection — user is going back to look, not restart.
    error = "";
    info = "";
    publishError = "";
    publishedSubject = "";
    stageCreate = "pending";
    stageRegister = "pending";
    stagePublish = "pending";
  }

  function resetCreateForm() {
    formDomain = "";
    formUsername = "";
    formPassphrase = "";
    formPassphraseConfirm = "";
    discoveryResults = [];
    discoveryAttempted = false;
    selectedNodeEndpoint = "";
    selectedCuratedZone = "";
    nodeMode = "curated";
    createStep = "node";
    publishError = "";
    publishedSubject = "";
    stageCreate = "pending";
    stageRegister = "pending";
    stagePublish = "pending";
  }

  // Mirror the helper in the Settings page; keep them in sync.
  function endpointHostPort(endpoint: string): string {
    try {
      const url = new URL(endpoint);
      return url.host || endpoint;
    } catch {
      return endpoint;
    }
  }

  // Three-stage chain: create -> register TSIG -> publish. On failure we
  // keep what's been built so far (identity is already on disk) and
  // expose a retry for the publish stage.
  async function submitCreate() {
    error = "";
    info = "";
    publishError = "";
    publishedSubject = "";

    // SDK derives the per-identity DNS label from the raw username bytes,
    // so `Alice` and `alice` would publish under different slots. Normalise
    // and reflect the canonical form back so the user sees what we registered.
    const username = formUsername.trim().toLowerCase();
    formUsername = username;
    if (!username) {
      error = "Username required.";
      return;
    }
    if (!formPassphrase) {
      error = "Passphrase required.";
      return;
    }
    if (formPassphrase !== formPassphraseConfirm) {
      error = "Passphrases don't match. Re-type both to continue.";
      return;
    }
    if (!selectedNode || !selectedZone) {
      error = "Pick a node first.";
      createStep = "node";
      return;
    }
    busy = true;
    stageCreate = "running";
    stageRegister = "pending";
    stagePublish = "pending";

    let createdIdentity: { username: string; domain: string } | null = null;
    let registered: RegisteredTsig | null = null;

    try {
      // Stage 1: init_or_unlock — materialise the per-identity dir,
      // sqlite db, and in-memory client.
      const result = await api.initOrUnlock({
        username,
        passphrase: formPassphrase,
        domain: selectedZone,
      });
      createdIdentity = {
        username: result.username,
        domain: result.domain,
      };
      stageCreate = "done";

      // Stage 2: TSIG-register against the chosen node.
      stageRegister = "running";
      try {
        registered = await api.registerTsig({
          endpoint: selectedNode.endpoint,
          subject: `${result.username}@${result.domain}`,
          passphrase: formPassphrase,
        });
        stageRegister = "done";
      } catch (regErr) {
        stageRegister = "failed";
        error =
          "Identity created, but TSIG registration failed: " +
          (isCommandError(regErr) ? regErr.message : String(regErr)) +
          ". You can retry registration from Settings.";
        // Identity is on disk; refresh so it shows up in the list and
        // the user can switch to it without re-creating.
        clearInbox();
        clearSent();
        contacts.set([]);
        await refreshActiveIdentity();
        await reloadList();
        if ($activeIdentity) {
          hydrateSent($activeIdentity.username);
          void hydrateInbox();
          void refreshContacts();
        }
        return;
      }

      // Materialise the publish block. Host writes the secret and drops
      // the in-memory client so new creds don't apply silently.
      try {
        await api.updatePublishConfig({
          username: result.username,
          publish: {
            zone: registered.dns_zone || result.domain,
            server: `${registered.dns_server}:53`,
            tsig_key_name: registered.key_name,
            tsig_algorithm: registered.algorithm,
            tsig_secret_base64: registered.secret_base64,
          },
          resolvers: null,
        });
        // Re-unlock transparently with the still-in-scope passphrase.
        await api.initOrUnlock({
          username,
          passphrase: formPassphrase,
          domain: undefined,
        });
      } catch (cfgErr) {
        stageRegister = "failed";
        error =
          "TSIG key minted, but wiring publish settings failed: " +
          (isCommandError(cfgErr) ? cfgErr.message : String(cfgErr)) +
          ". Open Settings to finish configuring.";
        clearInbox();
        clearSent();
        contacts.set([]);
        await refreshActiveIdentity();
        await reloadList();
        if ($activeIdentity) {
          hydrateSent($activeIdentity.username);
          void hydrateInbox();
          void refreshContacts();
        }
        return;
      }

      // Stage 3: publish. Non-fatal — identity + TSIG are already on disk.
      stagePublish = "running";
      try {
        await api.publishIdentity();
        stagePublish = "done";
        publishedSubject = `${result.username}@${result.domain}`;
        info = `PUBLISHED. ${publishedSubject} is live in DNS.`;
        void refreshPublishedStatus();
      } catch (pubErr) {
        stagePublish = "failed";
        publishError = isCommandError(pubErr) ? pubErr.message : String(pubErr);
        info = `Identity created and TSIG configured, but the initial publish failed. You can retry below, or from Identities/Settings later.`;
      }

      clearInbox();
      clearSent();
      contacts.set([]);
      await refreshActiveIdentity();
      await reloadList();
      if ($activeIdentity) {
        hydrateSent($activeIdentity.username);
        void hydrateInbox();
        void refreshContacts();
      }
    } catch (err) {
      stageCreate = "failed";
      error = isCommandError(err) ? err.message : String(err);
    } finally {
      busy = false;
      // Clear the passphrase fields once the chain is done. The
      // identity (if it succeeded) is already unlocked in memory.
      if (createdIdentity) {
        formPassphrase = "";
        formPassphraseConfirm = "";
      }
    }
  }

  // Retry the publish stage when the initial DNS UPDATE was transient.
  async function retryPublish() {
    publishError = "";
    busy = true;
    stagePublish = "running";
    try {
      await api.publishIdentity();
      stagePublish = "done";
      if ($activeIdentity) {
        publishedSubject = `${$activeIdentity.username}@${$activeIdentity.domain}`;
        info = `PUBLISHED. ${publishedSubject} is live in DNS.`;
      } else {
        info = "PUBLISHED.";
      }
      void refreshPublishedStatus();
    } catch (err) {
      stagePublish = "failed";
      publishError = formatPublishError(err);
    } finally {
      busy = false;
    }
  }

  function beginSwitch(username: string) {
    error = "";
    info = "";
    switchTargetUsername = username;
    switchPassphrase = "";
  }

  function cancelSwitch() {
    switchTargetUsername = "";
    switchPassphrase = "";
  }

  async function submitSwitch() {
    error = "";
    info = "";
    if (!switchTargetUsername) {
      error = "Pick an identity to open.";
      return;
    }
    if (!switchPassphrase) {
      error = "Passphrase required.";
      return;
    }
    busy = true;
    try {
      const result = await api.switchIdentity(
        switchTargetUsername,
        switchPassphrase,
      );
      info = `ACTIVE. ${result.username}@${result.domain}.`;
      switchPassphrase = "";
      switchTargetUsername = "";
      clearInbox();
      clearSent();
      contacts.set([]);
      await refreshActiveIdentity();
      await reloadList();
      if ($activeIdentity) {
        hydrateSent($activeIdentity.username);
        void hydrateInbox();
        void refreshContacts();
      }
    } catch (err) {
      error = isCommandError(err) ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function lock() {
    busy = true;
    try {
      await api.lockIdentity();
      clearInbox();
      clearSent();
      contacts.set([]);
      await refreshActiveIdentity();
      await reloadList();
      info = "Locked.";
    } catch (err) {
      error = isCommandError(err) ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  // Render the typed `publish_unconfigured` error from the SDK as
  // a targeted nudge to Settings instead of the generic toast.
  function formatPublishError(err: unknown): string {
    if (isCommandError(err) && err.kind === "publish_unconfigured") {
      return "No publish (TSIG) destination configured. Open Settings to add one before publishing.";
    }
    return isCommandError(err) ? err.message : String(err);
  }

  async function publishIdentity() {
    error = "";
    info = "";
    busy = true;
    try {
      await api.publishIdentity();
      info = "PUBLISHED.";
      void refreshPublishedStatus();
    } catch (err) {
      error = formatPublishError(err);
    } finally {
      busy = false;
    }
  }

  async function refreshPrekeys() {
    error = "";
    info = "";
    busy = true;
    try {
      const result = await api.refreshPrekeys({
        count: prekeyCount,
        ttl_seconds: prekeyTtl,
      });
      info = `Published ${result.published} prekey(s).`;
    } catch (err) {
      error = formatPublishError(err);
    } finally {
      busy = false;
    }
  }

  function stageLabel(state: StageState): string {
    switch (state) {
      case "pending":
        return "pending";
      case "running":
        return "in progress…";
      case "done":
        return "done";
      case "failed":
        return "failed";
      case "skipped":
        return "skipped";
    }
  }
</script>

<section>
  <header class="page-header">
    <h1>Identities</h1>
  </header>

  {#if onboarding}
    <aside class="welcome-banner">
      <h2 class="welcome-title">Welcome to DNSMesh.</h2>
      <p>
        DNSMesh is end-to-end-encrypted messaging that runs over the
        public DNS system. Every message is signed by you and decrypted
        only by the recipient. Your identity is yours; nobody else holds
        your keys.
      </p>
      <p>To get started:</p>
      <ul>
        <li>
          Pick a DMP node, the operator who'll host your identity records.
          The desktop ships a curated list of public nodes; pick whichever
          looks closest to you.
        </li>
        <li>
          Choose a username and a passphrase. The passphrase is the input
          to the key-derivation function. Write it down, you can't recover
          a forgotten passphrase.
        </li>
        <li>
          We register a TSIG key with the chosen node and publish your
          identity record automatically. Total time: about a minute.
        </li>
      </ul>
    </aside>
  {/if}

  {#if error}<p class="error">{error}</p>{/if}
  {#if info}<p class="pass">{info}</p>{/if}

  <h2>Active</h2>
  {#if $activeIdentity}
    <table class="kv">
      <tbody>
        <tr><th>Username</th><td>{$activeIdentity.username}</td></tr>
        <tr><th>Domain</th><td>{$activeIdentity.domain}</td></tr>
        <tr>
          <th>User ID</th>
          <td><code>{$activeIdentity.user_id_hex}</code></td>
        </tr>
        <tr>
          <th>X25519</th>
          <td><code>{$activeIdentity.x25519_public_key_hex}</code></td>
        </tr>
        <tr>
          <th>Ed25519</th>
          <td>
            <code>{$activeIdentity.ed25519_signing_public_key_hex}</code>
          </td>
        </tr>
        <tr>
          <th>Publish</th>
          <td>
            {#if $activeIdentity.publish_configured}
              <span class="pass">Configured</span>
            {:else}
              <span class="warn">Not configured (read-only mode)</span>
            {/if}
          </td>
        </tr>
        <tr>
          <th>DNS record</th>
          <td>
            {#if $publishedStatus?.status === "published"}
              <span class="badge live">PUBLISHED</span>
            {:else if $publishedStatus?.status === "not_published"}
              <span class="warn">Not in DNS yet</span>
            {:else if $publishedStatus?.status === "unknown"}
              <span class="muted">Unknown ({$publishedStatus.reason})</span>
            {:else}
              <span class="muted">Checking…</span>
            {/if}
          </td>
        </tr>
      </tbody>
    </table>

    <div class="actions">
      {#if $publishedStatus?.status === "published"}
        <button
          class="link-button"
          disabled={!$activeIdentity.publish_configured || busy}
          onclick={publishIdentity}
        >
          Re-publish
        </button>
      {:else}
        <button
          class="primary"
          disabled={!$activeIdentity.publish_configured || busy}
          onclick={publishIdentity}>Publish identity</button
        >
      {/if}
      <button class="danger" disabled={busy} onclick={lock}>Lock active</button>
    </div>

    <h3>Refresh prekeys</h3>
    <form
      class="inline-form"
      onsubmit={(e) => {
        e.preventDefault();
        refreshPrekeys();
      }}
    >
      <label>
        <span>Count</span>
        <input type="number" min="1" max="1000" bind:value={prekeyCount} />
      </label>
      <label>
        <span>TTL (seconds)</span>
        <input type="number" min="60" bind:value={prekeyTtl} />
      </label>
      <button
        type="submit"
        class="primary"
        disabled={!$activeIdentity.publish_configured || busy}
      >
        {busy ? "Working…" : "Refresh"}
      </button>
    </form>
  {:else}
    <p class="muted">No identity unlocked.</p>
  {/if}

  {#if !onboarding}
  <h2>Known identities</h2>
  {/if}
  {#if identities.length === 0}
    {#if !onboarding}
      <p class="muted">None yet. Create one below.</p>
    {/if}
  {:else}
    <table>
      <thead>
        <tr>
          <th>Username</th>
          <th>Domain</th>
          <th>State</th>
          <th>Action</th>
        </tr>
      </thead>
      <tbody>
        {#each identities as ident (ident.username)}
          <tr>
            <td>{ident.username}</td>
            <td>{ident.domain}</td>
            <td>
              {#if ident.is_active}
                <span class="pass">ACTIVE</span>
              {:else}
                <span class="muted">locked</span>
              {/if}
            </td>
            <td>
              {#if ident.is_active}
                <button class="danger" disabled={busy} onclick={lock}>
                  Lock active
                </button>
              {:else}
                <button
                  class="primary"
                  disabled={busy}
                  onclick={() => beginSwitch(ident.username)}
                >
                  Open
                </button>
              {/if}
            </td>
          </tr>
          {#if switchTargetUsername === ident.username && !ident.is_active}
            <tr class="switch-row">
              <td colspan="4">
                <form
                  class="switch-form"
                  onsubmit={(e) => {
                    e.preventDefault();
                    submitSwitch();
                  }}
                >
                  <label>
                    <span>Passphrase for {ident.username}@{ident.domain}</span>
                    <input
                      type="password"
                      bind:value={switchPassphrase}
                      autocomplete="current-password"
                    />
                  </label>
                  <div class="actions">
                    <button class="primary" type="submit" disabled={busy}>
                      {busy ? "Opening…" : "Open"}
                    </button>
                    <button type="button" disabled={busy} onclick={cancelSwitch}>
                      Cancel
                    </button>
                  </div>
                </form>
              </td>
            </tr>
          {/if}
        {/each}
      </tbody>
    </table>
  {/if}

  {#if !onboarding}
  <h2>Import or export</h2>
  <p class="muted small">
    Bring an identity over from the dnsmesh CLI binary, restore one
    from a previously-exported backup archive, or export the
    currently-active identity to a portable archive. The existing
    passphrase still unlocks any imported identity.
  </p>
  <div class="import-actions">
    <button
      type="button"
      onclick={() => {
        showImportCli = !showImportCli;
        importCliError = "";
      }}
      disabled={busy || importCliBusy}
    >
      {showImportCli ? "Hide" : "Import from CLI"}
    </button>
    <button
      type="button"
      onclick={() => {
        showImportBackup = !showImportBackup;
        importBackupError = "";
      }}
      disabled={busy || importBackupBusy}
    >
      {showImportBackup ? "Hide" : "Import from backup"}
    </button>
    <button
      type="button"
      onclick={() => {
        showExportBackup = !showExportBackup;
        exportBackupError = "";
        if (showExportBackup && $activeIdentity && !exportBackupOutputPath) {
          exportBackupOutputPath = defaultExportBackupPath(
            $activeIdentity.username,
          );
        }
      }}
      disabled={busy || exportBackupBusy || !$activeIdentity}
      title={$activeIdentity
        ? "Bundle the active identity's secrets into a portable archive."
        : "Unlock an identity first to export it."}
    >
      {showExportBackup ? "Hide" : "Export backup"}
    </button>
  </div>

  {#if showExportBackup && $activeIdentity}
    <form
      class="add-form"
      onsubmit={(e) => {
        e.preventDefault();
        submitExportBackup();
      }}
    >
      <p class="muted small">
        Bundles <strong>{$activeIdentity.username}@{$activeIdentity.domain}</strong>'s
        config, sqlite database, TSIG secret, and persistent inbox into
        a single <code>.dmp-backup.tar.gz</code> file you can stash in
        a vault and restore on another machine via "Import from backup".
      </p>
      <p class="warn small">
        <strong>The archive is NOT encrypted.</strong>
        It contains every secret the identity needs to send and receive
        messages. Anyone with the archive plus your passphrase can
        impersonate you. Store it in an encrypted vault
        (encrypted disk image, password-manager attachment,
        <code>age</code>/<code>gpg</code> on top, etc.).
      </p>
      <label>
        <span>Output path</span>
        <input
          type="text"
          bind:value={exportBackupOutputPath}
          placeholder="~/Desktop/{$activeIdentity.username}-XXXXXXX.dmp-backup.tar.gz"
          autocomplete="off"
        />
        <small class="muted">
          The host auto-appends <code>.dmp-backup.tar.gz</code> if you
          omit it. Tildes (<code>~</code>) are NOT expanded; supply
          an absolute path on disk.
        </small>
      </label>
      {#if exportBackupError}
        <p class="error small">{exportBackupError}</p>
      {/if}
      <div class="actions">
        <button type="submit" class="primary" disabled={exportBackupBusy}>
          {exportBackupBusy ? "Writing…" : "Export backup"}
        </button>
        <button
          type="button"
          onclick={() => {
            exportBackupOutputPath = $activeIdentity
              ? defaultExportBackupPath($activeIdentity.username)
              : "";
          }}
          disabled={exportBackupBusy}
        >
          Reset path
        </button>
      </div>
    </form>
  {/if}

  {#if showImportCli}
    <form
      class="add-form"
      onsubmit={(e) => {
        e.preventDefault();
        submitImportCli();
      }}
    >
      <p class="muted small">
        Pulls an identity out of the dnsmesh CLI's flat layout (a
        <code>config.yaml</code> + <code>dmp-rs.sqlite</code> + optional
        <code>tsig.key</code> all in one directory) and registers it
        under the desktop's per-identity layout. Default source is
        <code>~/.dmp/</code>; override below if your CLI lives elsewhere.
      </p>
      <label>
        <span>Source directory (optional)</span>
        <input
          type="text"
          bind:value={importCliSourceDir}
          placeholder="~/.dmp (leave blank for default)"
          autocomplete="off"
        />
      </label>
      <label>
        <span>Override username (optional)</span>
        <input
          type="text"
          bind:value={importCliOverrideUsername}
          placeholder="leave blank to keep the CLI's username"
          autocomplete="off"
        />
        <small class="muted">
          Only set this if the CLI's username already exists in the
          desktop, or the import will fail.
        </small>
      </label>
      {#if importCliError}
        <p class="error small">{importCliError}</p>
        {#if importCliError.includes("already exists")}
          <p class="muted small">
            Tip: pick a different name in <strong>Override username</strong>
            and click Import again.
          </p>
        {/if}
      {/if}
      <div class="actions">
        <button type="submit" class="primary" disabled={importCliBusy}>
          {importCliBusy ? "Importing…" : "Import"}
        </button>
        <button
          type="button"
          onclick={() => {
            showImportCli = false;
            importCliError = "";
          }}
          disabled={importCliBusy}
        >
          Cancel
        </button>
      </div>
    </form>
  {/if}

  {#if showImportBackup}
    <form
      class="add-form"
      onsubmit={(e) => {
        e.preventDefault();
        submitImportBackup();
      }}
    >
      <p class="muted small">
        Restores an identity from a <code>.dmp-backup.tar.gz</code>
        archive previously written by Settings → Backup &amp; restore.
        The archive contains every secret needed to send and receive,
        so source it from a trusted location.
      </p>
      <label>
        <span>Archive path</span>
        <input
          type="text"
          bind:value={importBackupArchive}
          placeholder="/path/to/alice-1714000000.dmp-backup.tar.gz"
          autocomplete="off"
        />
      </label>
      <label>
        <span>Override username (optional)</span>
        <input
          type="text"
          bind:value={importBackupOverrideUsername}
          placeholder="leave blank to keep the archive's username"
          autocomplete="off"
        />
      </label>
      {#if importBackupError}
        <p class="error small">{importBackupError}</p>
        {#if importBackupError.includes("already exists")}
          <p class="muted small">
            Tip: pick a different name in <strong>Override username</strong>
            and click Restore again.
          </p>
        {/if}
      {/if}
      <div class="actions">
        <button type="submit" class="primary" disabled={importBackupBusy}>
          {importBackupBusy ? "Restoring…" : "Restore"}
        </button>
        <button
          type="button"
          onclick={() => {
            showImportBackup = false;
            importBackupError = "";
          }}
          disabled={importBackupBusy}
        >
          Cancel
        </button>
      </div>
    </form>
  {/if}
  {/if}

  <h2>Create new identity</h2>
  <ol class="wizard-steps">
    <li class:current={createStep === "node"} class:done={createStep === "details"}>
      1. Pick a node
    </li>
    <li class:current={createStep === "details"}>
      2. Identity details
    </li>
  </ol>

  {#if createStep === "node"}
    <form
      id="create"
      class="add-form"
      onsubmit={(e) => {
        e.preventDefault();
        continueToDetails();
      }}
    >
      <p class="muted small">
        DNSMesh identities live under a DNS zone served by a DMP node
        operator. Pick a curated node from the list below, or expand
        "advanced" to point at any other zone.
      </p>

      <div class="curated-block">
        <div class="discovery-header">
          <span class="muted small">Known DMP nodes</span>
          <button
            type="button"
            onclick={loadCuratedNodes}
            disabled={curatedLoading}
          >
            {curatedLoading ? "Refreshing…" : "Refresh heartbeats"}
          </button>
        </div>

        {#if curatedError}
          <p class="error small">Failed to load curated nodes: {curatedError}</p>
        {/if}

        {#if curatedLoading && curatedNodes.length === 0}
          <p class="muted small">Checking heartbeats…</p>
        {:else if curatedNodes.length === 0 && !curatedError}
          <p class="muted small">No curated DMP nodes are configured.</p>
        {:else}
          <ul class="node-list">
            {#each curatedNodes as node (node.zone)}
              <li>
                <label
                  class="node-pick curated"
                  class:disabled={!node.live}
                >
                  <input
                    type="radio"
                    name="curated-node"
                    value={node.zone}
                    bind:group={selectedCuratedZone}
                    onchange={() => {
                      nodeMode = "curated";
                    }}
                    disabled={!node.live}
                  />
                  <div class="curated-meta">
                    <div class="curated-row">
                      <code class="node-endpoint">{node.zone}</code>
                      {#if node.live}
                        <span class="badge live">LIVE</span>
                      {:else}
                        <span class="badge down">DOWN</span>
                      {/if}
                    </div>
                    <span class="muted small">{node.operator_name}</span>
                    <span class="curated-desc small">{node.description}</span>
                    {#if node.live}
                      <span class="muted small">
                        endpoint <code>{node.live.endpoint}</code> ·
                        {node.live.version || "unknown"} · expires in
                        {formatStaleness(node.live.seconds_until_stale)}
                      </span>
                    {:else}
                      <span class="muted small">
                        No live heartbeat. This node can't be picked
                        right now.
                      </span>
                    {/if}
                  </div>
                </label>
              </li>
            {/each}
          </ul>
        {/if}
      </div>

      <details class="advanced-block" bind:open={advancedOpen}>
        <summary>
          Advanced: enter a custom zone (for self-hosted or non-curated
          DMP nodes)
        </summary>
        <p class="muted small">
          Type the DNS zone the operator publishes their heartbeat
          under, then pick from the discovered nodes. Switching to a
          custom zone clears any curated selection.
        </p>
        <label>
          <span>Custom zone (e.g. mesh.example.com)</span>
          <input
            type="text"
            bind:value={formDomain}
            autocomplete="off"
            placeholder="mesh.example.com"
            onfocus={() => {
              nodeMode = "custom";
              selectedCuratedZone = "";
            }}
          />
        </label>

        {#if formDomain.trim()}
          <div class="discovery-block">
            <div class="discovery-header">
              <span class="muted small">Live nodes in {formDomain.trim()}</span>
              <button
                type="button"
                onclick={manualDiscover}
                disabled={discoveryLoading}
              >
                {discoveryLoading ? "Searching…" : "Discover nodes"}
              </button>
            </div>

            {#if discoveryLoading}
              <p class="muted small">Looking for live nodes…</p>
            {:else if discoveryAttempted && discoveryResults.length === 0}
              <p class="warn small">
                No live DMP nodes found in {formDomain.trim()}. Pick a
                different zone, or contact a node operator to host one.
              </p>
            {:else if discoveryResults.length > 0}
              <ul class="node-list">
                {#each discoveryResults as node (node.endpoint)}
                  <li>
                    <label class="node-pick">
                      <input
                        type="radio"
                        name="node"
                        value={node.endpoint}
                        bind:group={selectedNodeEndpoint}
                        onchange={() => {
                          nodeMode = "custom";
                          selectedCuratedZone = "";
                        }}
                      />
                      <span class="node-endpoint">{node.endpoint}</span>
                      <span class="muted small">
                        {node.version || "unknown"} · expires in {formatStaleness(
                          node.seconds_until_stale,
                        )}
                      </span>
                    </label>
                  </li>
                {/each}
              </ul>
            {:else}
              <p class="muted small">
                Click "Discover nodes" or wait. We auto-search after you
                stop typing the zone.
              </p>
            {/if}
          </div>
        {/if}
      </details>

      <div class="actions">
        <button
          class="primary"
          type="submit"
          disabled={busy || !selectedNode}
        >
          Continue
        </button>
      </div>
    </form>
  {:else}
    <form
      class="add-form"
      onsubmit={(e) => {
        e.preventDefault();
        submitCreate();
      }}
    >
      <div class="picked-node">
        <div>
          <span class="muted small">Selected node</span>
          <div class="node-endpoint">{selectedEndpointDisplay}</div>
          <div class="muted small">
            Zone: {selectedZone}
            {#if selectedNode}
              · {selectedNode.version || "unknown"}
            {/if}
          </div>
        </div>
        <button type="button" onclick={backToNode} disabled={busy}>
          Edit
        </button>
      </div>

      <label>
        <span>Username</span>
        <input
          type="text"
          bind:value={formUsername}
          autocomplete="off"
        />
      </label>
      <label>
        <span>Passphrase</span>
        <input
          type="password"
          bind:value={formPassphrase}
          autocomplete="new-password"
        />
      </label>
      <label>
        <span>Confirm passphrase</span>
        <input
          type="password"
          bind:value={formPassphraseConfirm}
          autocomplete="new-password"
        />
      </label>
      {#if passphraseMismatch}
        <p class="error small">
          Passphrases don't match. Re-type both to continue.
        </p>
      {/if}

      {#if busy || stageCreate !== "pending" || stageRegister !== "pending" || stagePublish !== "pending"}
        <div class="stage-block">
          <p class="stage-line" class:done={stageCreate === "done"}
            class:running={stageCreate === "running"}
            class:failed={stageCreate === "failed"}>
            Creating identity… [{stageLabel(stageCreate)}]
          </p>
          <p class="stage-line" class:done={stageRegister === "done"}
            class:running={stageRegister === "running"}
            class:failed={stageRegister === "failed"}>
            Registering TSIG with {selectedEndpointDisplay}… [{stageLabel(
              stageRegister,
            )}]
          </p>
          <p class="stage-line" class:done={stagePublish === "done"}
            class:running={stagePublish === "running"}
            class:failed={stagePublish === "failed"}>
            Publishing identity record… [{stageLabel(stagePublish)}]
          </p>
        </div>
      {/if}

      {#if publishError}
        <div class="publish-retry">
          <p class="error small">Publish failed: {publishError}</p>
          <p class="muted small">
            The identity and TSIG configuration are saved. You can
            retry publishing now, or later from this page or from
            Settings.
          </p>
          <div class="actions">
            <button
              type="button"
              class="primary"
              onclick={retryPublish}
              disabled={busy}
            >
              Retry publish
            </button>
          </div>
        </div>
      {/if}

      <div class="actions">
        <button
          class="primary"
          type="submit"
          disabled={busy || createSubmitDisabled}
        >
          {busy ? "Working…" : "Create, register, and publish"}
        </button>
        <button type="button" onclick={backToNode} disabled={busy}>
          Back
        </button>
        {#if stageCreate === "done" && stageRegister === "done" && (stagePublish === "done" || stagePublish === "failed")}
          <button type="button" onclick={resetCreateForm} disabled={busy}>
            Create another
          </button>
        {/if}
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
    font-size: 1.4rem;
  }
  h2 {
    margin: 1.5rem 0 0.5rem;
    font-size: 0.95rem;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  h3 {
    margin: 1rem 0 0.5rem;
    font-size: 0.9rem;
  }
  table.kv th {
    width: 110px;
  }
  .actions {
    display: flex;
    gap: 0.4rem;
    margin-top: 0.5em;
    flex-wrap: wrap;
  }
  .add-form,
  .inline-form {
    max-width: 640px;
    margin-bottom: 1.5rem;
    padding: 1rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .inline-form {
    display: grid;
    grid-template-columns: 1fr 1fr auto;
    gap: 0.6rem;
    align-items: end;
  }
  .inline-form button {
    height: 32px;
  }
  .small {
    font-size: 12px;
  }
  .wizard-steps {
    display: flex;
    gap: 1rem;
    list-style: none;
    padding: 0;
    margin: 0 0 0.6rem;
    font-size: 12px;
    color: var(--muted);
  }
  .wizard-steps li {
    padding: 0.2em 0.6em;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--surface);
  }
  .wizard-steps li.current {
    color: var(--accent);
    border-color: var(--accent);
    font-weight: 600;
  }
  .wizard-steps li.done {
    color: var(--pass);
    border-color: var(--pass);
  }
  .discovery-block {
    margin-bottom: 0.85em;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-alt);
  }
  .discovery-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    margin-bottom: 0.5rem;
  }
  .node-list {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .node-list li {
    padding: 0.35em 0;
    border-bottom: 1px solid var(--border);
  }
  .node-list li:last-child {
    border-bottom: none;
  }
  .node-pick {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 0.5rem;
    margin: 0;
    cursor: pointer;
  }
  .node-pick input[type="radio"] {
    width: auto;
  }
  .node-endpoint {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
    word-break: break-all;
  }
  .picked-node {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.6rem;
    margin-bottom: 0.85em;
    padding: 0.6rem 0.75rem;
    background: var(--accent-softer);
    border: 1px solid var(--border-accent);
    border-radius: 6px;
  }
  .picked-node .node-endpoint {
    font-weight: 600;
  }
  .stage-block {
    margin: 0.6rem 0;
    padding: 0.6rem 0.75rem;
    background: var(--surface-alt);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .stage-line {
    margin: 0.15em 0;
    font-size: 12px;
    color: var(--muted);
  }
  .stage-line.running {
    color: var(--text);
  }
  .stage-line.done {
    color: var(--pass);
  }
  .stage-line.failed {
    color: var(--danger);
  }
  .publish-retry {
    margin: 0.6rem 0;
    padding: 0.6rem 0.75rem;
    background: var(--danger-soft);
    border: 1px solid var(--danger-border);
    border-radius: 6px;
  }
  .switch-row {
    background: var(--accent-softer);
  }
  .switch-form {
    margin: 0.5rem 0;
  }
  .curated-block {
    margin-bottom: 0.85em;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-alt);
  }
  .node-pick.curated {
    grid-template-columns: auto 1fr;
    align-items: flex-start;
    gap: 0.6rem;
    padding: 0.4rem 0;
  }
  .node-pick.curated.disabled {
    cursor: not-allowed;
    opacity: 0.7;
  }
  .curated-meta {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    min-width: 0;
  }
  .curated-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .curated-desc {
    color: var(--text);
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
  .badge.down {
    color: var(--danger);
    border-color: var(--danger);
    background: rgba(198, 40, 40, 0.06);
  }
  .advanced-block {
    margin-bottom: 0.85em;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    padding: 0.5rem 0.75rem;
  }
  .advanced-block summary {
    cursor: pointer;
    font-size: 12px;
    color: var(--muted);
    padding: 0.2rem 0;
  }
  .advanced-block[open] summary {
    margin-bottom: 0.5rem;
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
  .import-actions {
    display: flex;
    gap: 0.4rem;
    margin-bottom: 0.85rem;
    flex-wrap: wrap;
  }
  /* First-run welcome banner shown during onboarding. */
  .welcome-banner {
    max-width: 640px;
    margin: 0.4rem 0 1.25rem;
    padding: 1rem 1.1rem;
    background: var(--accent-softer);
    border: 1px solid var(--border-accent);
    border-radius: var(--radius-md);
    color: var(--text);
  }
  .welcome-banner .welcome-title {
    margin: 0 0 0.5rem;
    font-size: 1rem;
    color: var(--accent-strong);
    text-transform: none;
    letter-spacing: 0;
    font-weight: 700;
  }
  .welcome-banner p {
    margin: 0.4rem 0;
    font-size: 13px;
    line-height: 1.5;
  }
  .welcome-banner ul {
    margin: 0.4rem 0 0;
    padding-left: 1.2rem;
    font-size: 13px;
    line-height: 1.5;
  }
  .welcome-banner li {
    margin-bottom: 0.35rem;
  }
</style>
