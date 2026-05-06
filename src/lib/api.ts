// Typed wrappers around `invoke()`. Routes call these instead of
// `invoke()` directly so the field names stay matched to the Rust serde
// shapes. All calls are async and may throw a `CommandError`.

import { invoke } from "@tauri-apps/api/core";

// --- Error type -----------------------------------------------------------

// Stable discriminant on `CommandError.kind`. Mirrors the variant set in
// `dnsmesh_ffi::FfiError` plus host-only kinds raised by the Tauri shim.
export type CommandErrorKind =
  // FFI parity
  | "invalid_config"
  | "publish_unconfigured"
  | "contact_not_found"
  | "publish_failed"
  | "no_record_found"
  | "verify_failed"
  | "io"
  | "internal"
  // Host-only (Tauri command shim)
  | "validation"
  | "not_initialized"
  | "config"
  | "net"
  | "sdk";

export interface CommandError {
  kind: CommandErrorKind | string;
  message: string;
  /** Variant-specific structured payload when present. */
  details?: Record<string, unknown> | null;
}

export function isCommandError(value: unknown): value is CommandError {
  return (
    typeof value === "object" &&
    value !== null &&
    "kind" in value &&
    "message" in value
  );
}

// --- Identity -------------------------------------------------------------

export interface IdentityInfo {
  username: string;
  domain: string;
  user_id_hex: string;
  x25519_public_key_hex: string;
  ed25519_signing_public_key_hex: string;
  publish_configured: boolean;
}

export interface IdentitySummary {
  username: string;
  domain: string;
  is_active: boolean;
}

export interface InitOrUnlockArgs {
  username: string;
  passphrase: string;
  /** Required only on first creation. */
  domain?: string;
}

export interface RefreshPrekeysArgs {
  count?: number;
  ttl_seconds?: number;
}

export interface RefreshPrekeysResult {
  published: number;
}

export interface PublishConfigView {
  zone: string;
  server: string;
  tsig_key_name: string;
  tsig_algorithm: string;
  tsig_secret_path: string;
}

export interface IdentityConfigView {
  username: string;
  publish: PublishConfigView | null;
  resolvers: string[];
}

export interface UpdatePublishArgs {
  username: string;
  publish?: {
    zone: string;
    server: string;
    tsig_key_name: string;
    tsig_algorithm?: string;
    /**
     * Path to an on-disk TSIG secret file. Either this OR
     * `tsig_secret_base64` must be set; passing the base64 makes the
     * host materialise `<identity-dir>/tsig.key` (mode 0600 on Unix)
     * and use that path.
     */
    tsig_secret_path?: string;
    tsig_secret_base64?: string | null;
  } | null;
  resolvers?: string[] | null;
}

export const api = {
  version: (): Promise<string> => invoke("version"),

  // identity
  initOrUnlock: (args: InitOrUnlockArgs): Promise<IdentityInfo> =>
    invoke("init_or_unlock", { args }),
  getIdentityInfo: (): Promise<IdentityInfo | null> =>
    invoke("get_identity_info"),
  publishIdentity: (): Promise<void> => invoke("publish_identity"),
  refreshPrekeys: (args: RefreshPrekeysArgs): Promise<RefreshPrekeysResult> =>
    invoke("refresh_prekeys", { args }),
  listIdentities: (): Promise<IdentitySummary[]> => invoke("list_identities"),
  switchIdentity: (
    username: string,
    passphrase: string,
  ): Promise<IdentityInfo> =>
    invoke("switch_identity", { args: { username, passphrase } }),
  lockIdentity: (): Promise<void> => invoke("lock_identity"),
  getIdentityConfig: (username: string): Promise<IdentityConfigView> =>
    invoke("get_identity_config", { username }),
  updatePublishConfig: (args: UpdatePublishArgs): Promise<IdentityConfigView> =>
    invoke("update_publish_config", { args }),
  isIdentityPublished: (): Promise<PublishedStatus> =>
    invoke("is_identity_published"),

  // contacts
  listContacts: (): Promise<ContactView[]> => invoke("list_contacts"),
  addContact: (args: AddContactArgs): Promise<AddContactResult> =>
    invoke("add_contact", { args }),
  deleteContact: (username: string): Promise<DeleteContactResult> =>
    invoke("delete_contact", { args: { username } }),
  fetchIdentity: (address: string): Promise<ContactView> =>
    invoke("fetch_identity", { args: { address } }),
  fetchAndAddContact: (address: string): Promise<AddContactResult> =>
    invoke("fetch_and_add_contact", { args: { address } }),

  // messaging
  sendMessage: (
    recipient_username: string,
    plaintext: string,
  ): Promise<SendMessageResult> =>
    invoke("send_message", {
      args: { recipient_username, plaintext },
    }),
  receiveMessages: (): Promise<InboxMessageView[]> =>
    invoke("receive_messages"),

  // nodes
  listKnownResolvers: (): Promise<ResolverInfo[]> =>
    invoke("list_known_resolvers"),
  listKnownNodes: (): Promise<KnownNodeStatus[]> =>
    invoke("list_known_nodes"),
  doctor: (): Promise<DoctorReport> => invoke("doctor"),
  discoverNodes: (zone: string): Promise<DiscoveredNode[]> =>
    invoke("discover_nodes", { zone }),
  registerTsig: (args: RegisterTsigArgs): Promise<RegisteredTsig> =>
    invoke("register_tsig", args as unknown as Record<string, unknown>),
  effectiveResolvers: (): Promise<EffectiveResolvers> =>
    invoke("effective_resolvers"),
  refreshNetwork: (): Promise<EffectiveResolvers> => invoke("refresh_network"),

  // inbox (per-identity persistent store)
  inboxLoad: (): Promise<InboxRow[]> => invoke("inbox_load"),
  inboxAppend: (
    messages: PersistedInboxMessage[],
  ): Promise<InboxAppendResult> =>
    invoke("inbox_append", { args: { messages } }),
  inboxMarkRead: (msg_id_hex: string): Promise<void> =>
    invoke("inbox_mark_read", { args: { msg_id_hex } }),
  inboxDelete: (msg_id_hexes: string[]): Promise<InboxDeleteResult> =>
    invoke("inbox_delete", { args: { msg_id_hexes } }),

  // import / backup
  importFromCli: (args: ImportFromCliArgs): Promise<ImportFromCliResult> =>
    invoke("import_from_cli", { args }),
  exportIdentityBackup: (
    args: ExportBackupArgs,
  ): Promise<ExportBackupResult> =>
    invoke("export_identity_backup", { args }),
  importIdentityBackup: (
    args: ImportBackupArgs,
  ): Promise<ImportBackupResult> =>
    invoke("import_identity_backup", { args }),
};

// --- Contacts -------------------------------------------------------------

export interface ContactView {
  username: string;
  domain: string;
  x25519_public_key_hex: string;
  ed25519_signing_public_key_hex: string;
}

export interface AddContactArgs {
  username: string;
  domain: string;
  x25519_public_key_hex: string;
  ed25519_signing_public_key_hex: string;
}

export interface AddContactResult {
  newly_added: boolean;
  contact: ContactView;
}

export interface DeleteContactResult {
  /** True iff the username was present and removed; false on a missing
   * username (the backend treats it as idempotent). */
  removed: boolean;
}

// --- Messaging ------------------------------------------------------------

export interface SendMessageResult {
  msg_id_hex: string;
}

export interface InboxMessageView {
  sender_signing_pk_hex: string;
  msg_id_hex: string;
  timestamp: number;
  plaintext_utf8: string;
  plaintext_bytes: number[];
}

// Persisted inbox row from `<identity-dir>/inbox.jsonl`. Same payload as
// `InboxMessageView`; persisted so identity switches don't need re-decrypt.
export interface PersistedInboxMessage {
  sender_signing_pk_hex: string;
  msg_id_hex: string;
  timestamp: number;
  plaintext_utf8: string;
  plaintext_bytes: number[];
}

// Inbox row + `read` flag derived from the per-identity read-state file.
export interface InboxRow extends PersistedInboxMessage {
  read: boolean;
}

export interface InboxAppendResult {
  appended: number;
  total: number;
}

// `inbox_delete` is idempotent; `removed` counts the ids that were present.
export interface InboxDeleteResult {
  removed: number;
}

// --- Nodes ----------------------------------------------------------------

export interface ResolverInfo {
  address: string;
  operator: string;
}

export type CheckStatus = "pass" | "warn" | "fail";

export interface CheckResult {
  name: string;
  status: CheckStatus;
  message: string;
}

export interface DoctorReport {
  checks: CheckResult[];
  overall: CheckStatus;
}

// One verified DMP operator surfaced by `discover_nodes`, built from a
// single signed heartbeat under `_dnsmesh-heartbeat.<zone>`.
export interface DiscoveredNode {
  endpoint: string;
  operator_spk_hex: string;
  version: string;
  claim_provider_zone: string;
  /** Seconds remaining before this heartbeat expires (always > 0). */
  seconds_until_stale: number;
}

// Curated DMP node + live heartbeat status. `live: null` means the node
// is down, unreachable, or failed verification; the UI uses this as the
// "OK to use" gate. `source` is `"curated"` for hard-coded entries and
// `"directory"` for ones pulled from the public directory feed.
export interface KnownNodeStatus {
  zone: string;
  operator_name: string;
  description: string;
  live: DiscoveredNode | null;
  source: "curated" | "directory" | string;
}

// Effective resolver pool. `source` is `"override"` when the identity
// config has a non-empty `resolvers:` list, else `"well_known"`.
export interface EffectiveResolvers {
  addresses: string[];
  source: "override" | "well_known" | string;
}

// Args for `register_tsig`. The passphrase signs the operator's
// challenge transiently; it is never persisted.
export interface RegisterTsigArgs {
  endpoint: string;
  subject: string;
  passphrase: string;
}

// Minted TSIG material from `register_tsig`. The base64 secret is
// forwarded as `tsig_secret_base64` on `update_publish_config`; the host
// then writes it to `<identity-dir>/tsig.key`.
export interface RegisteredTsig {
  key_name: string;
  algorithm: string;
  secret_base64: string;
  dns_zone: string;
  dns_server: string;
  expires_at: number | null;
}

// --- Identity publish status ---------------------------------------------

// `published` — record is live; `not_published` — NXDOMAIN-equivalent;
// `unknown` — lookup failed for any other reason. The UI keeps the
// Publish button visible on `unknown`.
export type PublishedStatus =
  | { status: "published" }
  | { status: "not_published" }
  | { status: "unknown"; reason: string };

// --- Import / backup -----------------------------------------------------

// Args for `import_from_cli`. `source_dir` defaults to `$DMP_CONFIG_HOME`
// then `$HOME/.dmp` on the host side. `override_username` is required on
// collision with an existing identity.
export interface ImportFromCliArgs {
  source_dir?: string | null;
  override_username?: string | null;
}

export interface ImportFromCliResult {
  username: string;
  domain: string;
  /** True iff a tsig.key file was carried over from the CLI dir. */
  publish_imported: boolean;
}

// Args for `export_identity_backup`. The host auto-appends
// `.dmp-backup.tar.gz` if the supplied path lacks the suffix.
export interface ExportBackupArgs {
  username: string;
  output_path: string;
}

export interface ExportBackupResult {
  archive_path: string;
  total_bytes: number;
  /** Includes `backup-meta.json` plus every per-identity file written. */
  file_count: number;
}

// Args for `import_identity_backup`. `override_username` is required
// when the archive's username would collide with an existing identity.
export interface ImportBackupArgs {
  archive_path: string;
  override_username?: string | null;
}

export interface ImportBackupResult {
  username: string;
  domain: string;
  /** Excludes `backup-meta.json`; only counts files written to disk. */
  file_count: number;
}

