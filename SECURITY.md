# Security

This is **non-certified, pre-external-audit software** in an **alpha**
state. The desktop client embeds the in-progress
[`dnsmesh-rs`](https://github.com/oscarvalenzuelab/dnsmesh-rs) SDK,
whose crate boundaries, wire-format compatibility, and on-disk schema
are still moving. **Don't route confidentiality-critical traffic
through `dnsmesh-desktop` until both the wire-format external
cryptographic audit lands (against the Python reference at
[oscarvalenzuelab/DNSMeshProtocol](https://github.com/oscarvalenzuelab/DNSMeshProtocol))
*and* this desktop client has cut a tagged 0.1.0 release.**

`dnsmesh-desktop` is a Tauri 2 desktop client wrapping the `dnsmesh-rs`
SDK. The protocol specification, the authoritative DNS node
implementation, and the federation / cluster code live in the Python
reference repo above. This client consumes the same wire format the
spec defines; the protocol's threat model, known limits, and audit
posture are documented there. **Protocol-level vulnerabilities belong
in the spec repo, not here.** This file covers what is specific to the
desktop client.

## Reporting a vulnerability

Crypto bugs, plaintext-leak bugs, key-handling bugs, and anything else
that could undermine the trust model go to
`oscar.valenzuela.b_AT_gmail.com` (replace `_AT_` with `@`) **privately**
— not to a public GitHub issue. Once this repository is flipped public,
[GitHub's private vulnerability reporting](https://github.com/oscarvalenzuelab/dnsmesh-desktop/security/advisories/new)
becomes the preferred channel.

Include in the report:

- Affected version (release tag, commit SHA, or installer filename).
- Operating system and architecture.
- Minimum reproduction.
- Your assessment of impact.

The disclosure window is **90 days** from acknowledgement. I will
publish a fix and a CVE-style advisory before the window closes;
coordinated extension is fine if the upstream Python reference or the
`dnsmesh-rs` SDK needs to ship a coordinated change first.

For non-security questions, open a regular GitHub issue. Please don't
open a public issue for an unpatched security bug.

## Scope of this repository

`dnsmesh-desktop` ships:

- A **Tauri 2 desktop application** (SvelteKit frontend + Rust host)
  that wraps the `dnsmesh-rs` SDK.
- Per-platform installers (`.dmg`, `.msi`, `.exe`, `.deb`, `.rpm`,
  `.AppImage`) produced by `cargo tauri build` in CI.
- A small command surface (`commands/identity`, `commands/contacts`,
  `commands/messaging`, `commands/inbox`, `commands/nodes`,
  `commands/import_cli`, `commands/backup`) bridging the SDK to the
  IPC layer.

It does **not** ship:

- The SDK itself — that's [`dnsmesh-rs`](https://github.com/oscarvalenzuelab/dnsmesh-rs);
  vulnerabilities in crypto primitives, wire format, or DNS publishing
  belong there.
- The authoritative DNS node, the publish API, federation code, or
  the operator deploy scripts — those live in the
  [Python reference](https://github.com/oscarvalenzuelab/DNSMeshProtocol).
- Mobile builds.

Threat-model questions about the **protocol** — chunking, manifests,
slot semantics, replay defenses, traffic analysis, zone-anchored
identity — are answered in the
[Python repo's SECURITY.md](https://github.com/oscarvalenzuelab/DNSMeshProtocol/blob/main/SECURITY.md).
Vulnerabilities in the embedded crypto stack go to the
[`dnsmesh-rs` SECURITY.md](https://github.com/oscarvalenzuelab/dnsmesh-rs/blob/main/SECURITY.md).

## Cryptographic primitives (inherited from the SDK)

All crypto runs through `dnsmesh-rs`; this client doesn't roll its own.
For completeness, the primitives are:

- **X25519** (RFC 7748).
- **Ed25519** (RFC 8032).
- **ChaCha20-Poly1305 AEAD** (RFC 8439).
- **HKDF-SHA256** (RFC 5869).
- **SHA-256.**
- **Argon2id** (32 MiB / t=2 / p=2 / 32-byte output) for passphrase →
  key derivation.

Each identity gets its own random 32-byte Argon2id salt at create time;
losing the passphrase means losing the identity.

## Identity passphrase

The desktop client never persists the passphrase. It lives only in
process memory while an identity is unlocked, and is wiped on **Lock
active** in the header dropdown. **Loss of the passphrase is loss of
the identity** — there is no recovery, by design. Persist it in a
password manager.

## Known limits (port-specific)

These are limits introduced or surfaced by the desktop client itself.
For SDK-level limits see the `dnsmesh-rs` SECURITY.md; for protocol
limits see the Python reference.

1. **Pre-tag, pre-audit.** No semver guarantee until 0.1.0.
2. **Unsigned macOS and Windows binaries.** Alpha builds ship
   unsigned. macOS users have to right-click → Open the first time;
   Windows users have to click through SmartScreen. The signing
   pipeline is wired in `release.yml` but gated on signing-secret
   presence — flipping the secrets on at any time turns signing on
   without a code change. Until then, **verify the SHA-256 of the
   downloaded asset against the value on the Release page** before
   first launch.
3. **No auto-update yet.** New versions ship as fresh GH Release
   downloads. There is no in-app updater that could be MITM'd, and
   correspondingly no automated way for users to learn about a
   security fix without checking the Releases page.
4. **Identity backup archive is unencrypted.** The `.dmp-backup.tar.gz`
   produced by **Settings → Export backup** contains every secret the
   identity needs to send and receive. The UI surfaces a loud warning;
   users are responsible for storing it in an encrypted vault
   (encrypted disk image, password-manager attachment, `age` / `gpg`
   wrapping).
5. **TSIG secret stored on disk in cleartext.** The TSIG secret minted
   by **Register with `<node>`** lives at
   `<identity-dir>/tsig.key` with default OS permissions. On a shared
   machine, set `chmod 0700 ~/.dmp` after creating the first identity.
   There is no OS keychain integration yet.
6. **Local IPC trust boundary.** The Tauri host trusts any process
   that can reach the IPC channel — which on a single-user desktop
   means any process running as the same OS user. Treat the
   passphrase prompt the same way you treat a sudo prompt.
7. **DNS resolver auto-detect.** When no resolver override is set
   under **Settings → Resolver overrides**, the SDK falls back to a
   well-known public resolver list. On split-horizon networks this may
   not match what the rest of the system queries. Override explicitly
   when in doubt.

## Out-of-scope for this repository

- Cryptographic primitives, wire format, and SDK behaviour — see the
  [`dnsmesh-rs` SECURITY.md](https://github.com/oscarvalenzuelab/dnsmesh-rs/blob/main/SECURITY.md).
- Server-side hardening of DMP nodes — see the
  [Python reference](https://github.com/oscarvalenzuelab/DNSMeshProtocol).
- Wire format and protocol-level threat model — see the Python
  reference's `docs/protocol/` and `SECURITY.md`.
