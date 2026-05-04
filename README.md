# dnsmesh-desktop

**A desktop client for the
[DNS Mesh Protocol](https://github.com/oscarvalenzuelab/DNSMeshProtocol) —
end-to-end encrypted messaging delivered over DNS.**

## Status

Alpha. The current release is
[`desktop-v0.1.0-alpha.1`](https://github.com/oscarvalenzuelab/dnsmesh-desktop/releases/latest).
Expect rapid churn between alpha tags. The desktop client embeds the
in-progress [`dnsmesh-rs`](https://github.com/oscarvalenzuelab/dnsmesh-rs)
SDK; until that crate cuts a stable release, on-disk layout, wire format,
and SDK API can move between versions.

## Download

Pre-built installers are published on every `desktop-v*` tag at
[github.com/oscarvalenzuelab/dnsmesh-desktop/releases](https://github.com/oscarvalenzuelab/dnsmesh-desktop/releases).
Pick the asset that matches your machine.

### macOS

Download the `.dmg`, mount it, and drag **DNSMesh** into Applications.

Alpha builds are **unsigned**. macOS Gatekeeper will refuse to open it
on a normal double-click. The first time you launch it:

1. Right-click (or Control-click) `DNSMesh` in Applications and pick **Open**.
2. Confirm the warning dialog. macOS remembers the decision after the
   first launch, so subsequent opens behave normally.

If the right-click prompt doesn't appear (rare on later macOS versions),
strip the quarantine attribute manually:

```sh
xattr -d com.apple.quarantine /Applications/DNSMesh.app
```

### Windows

Download the `.msi` (preferred) or the `.exe` installer.

Alpha builds are **unsigned**. SmartScreen will warn that the publisher
is unrecognised. Click **More info** → **Run anyway** to proceed.

### Linux

Pick the format that matches your distro:

- `.deb` — Debian, Ubuntu, and derivatives. `sudo apt install ./DNSMesh_*.deb`.
- `.rpm` — Fedora, RHEL, and derivatives. `sudo dnf install ./DNSMesh-*.rpm`.
- `.AppImage` — universal. `chmod +x DNSMesh-*.AppImage && ./DNSMesh-*.AppImage`.

## What this is

DMP is an open protocol for moving end-to-end encrypted messages between
two people using DNS as the transport. The recipient's identity, prekeys,
and mailbox slots all resolve like any other DNS record; there is no
central server, no app store, and no gatekeeper between sender and
recipient.

`dnsmesh-desktop` is a Tauri 2 app (SvelteKit frontend + Rust host) that
wraps the [`dnsmesh-rs`](https://github.com/oscarvalenzuelab/dnsmesh-rs)
SDK behind a familiar mail-style UI. The protocol specification, the
authoritative DNS node implementation, and the federation / cluster code
live in the Python reference at
[oscarvalenzuelab/DNSMeshProtocol](https://github.com/oscarvalenzuelab/DNSMeshProtocol).

## Features

What ships in `0.1.0-alpha.1`:

- **Multi-identity support.** Each identity gets its own per-identity
  Argon2id salt and on-disk directory; switching is one click in the
  header.
- **Inbox** with read / unread tracking, single + bulk delete, and a
  persistent JSONL log per identity so messages survive restarts.
- **Compose** with reply context — replying quotes the original sender,
  timestamp, and plaintext.
- **Contacts** with avatars, fetch-by-address (one-shot identity lookup
  + pin), manual add, and delete.
- **TSIG-authenticated DNS UPDATE publish** (RFC 2136 + RFC 8945) for
  identity records, prekey RRsets, and mailbox slots.
- **Identity backup / restore** as a single `.dmp-backup.tar.gz`
  archive (config + keystore + sqlite + persistent inbox).
- **Import from CLI.** Pulls an existing identity out of the
  `dnsmesh-rs` CLI's `~/.dmp/` layout in one click.

## Building

Prerequisites:
[Bun](https://bun.sh),
[Rust stable](https://rustup.rs),
and the
[Tauri 2 system prerequisites](https://tauri.app/start/prerequisites/)
for your platform.

The desktop crate consumes the `dnsmesh-rs` SDK as a path dependency
during local development, so the two repos must be cloned **as
siblings** in the same parent directory:

```sh
mkdir -p ~/code/DMP && cd ~/code/DMP
git clone https://github.com/oscarvalenzuelab/dnsmesh-rs
git clone https://github.com/oscarvalenzuelab/dnsmesh-desktop
cd dnsmesh-desktop
bun install
bun run tauri dev      # development build with hot-reload
bun run tauri build    # release bundle
```

CI clones the SDK at the SHA pinned in `.dnsmesh-rs-ref` so release
contents don't drift with whatever `main` happened to be at tag-push
time. Bumping the pin is an explicit commit reviewers can see in the
diff — see
[`CONTRIBUTING.md`](https://github.com/oscarvalenzuelab/dnsmesh-desktop/blob/main/CONTRIBUTING.md)
for the procedure.

## Platforms

CI builds the full release matrix on every `desktop-v*` tag:

| Platform | Asset |
|---|---|
| macOS — Apple Silicon (and Intel via Rosetta) | `.dmg`, `.app.tar.gz` |
| macOS — Intel | `.dmg`, `.app.tar.gz` |
| Linux — x86_64 | `.deb`, `.rpm`, `.AppImage` |
| Linux — aarch64 | `.deb`, `.rpm`, `.AppImage` |
| Windows — x86_64 | `.msi`, `.exe` |

Code-signing is wired but gated on signing-secret presence; alpha
builds ship unsigned with the warnings noted in **Download** above.

## Reporting issues

Bug reports, feature requests, and rough edges go to the
[issue tracker](https://github.com/oscarvalenzuelab/dnsmesh-desktop/issues).
Security-sensitive reports — anything that could leak plaintext,
private keys, or undermine the trust model — go to the email address
in [`SECURITY.md`](./SECURITY.md), **not** a public issue.

## License

This desktop client is licensed under the
[MIT License](./LICENSE). This is intentionally asymmetric with the
Python reference, which is licensed AGPL-3.0: the desktop is an
end-user application meant to be redistributable, repackagable, and
forkable by anyone wanting to ship a custom DMP client — without
imposing AGPL obligations on downstream distributors. The licence is
consistent with the [`dnsmesh-rs`](https://github.com/oscarvalenzuelab/dnsmesh-rs)
SDK it builds on.
