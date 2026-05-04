# Changelog

All notable changes to this project will be documented in this file.

The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This desktop client embeds the
[`dnsmesh-rs`](https://github.com/oscarvalenzuelab/dnsmesh-rs) SDK,
which is wire-compatible with the Python reference at
[oscarvalenzuelab/DNSMeshProtocol](https://github.com/oscarvalenzuelab/DNSMeshProtocol);
breaking wire-format changes there will be reflected here.

## [Unreleased]

## 0.1.0-alpha.1 — 2026-05-03

First public alpha. Expect rapid churn.

### Added

- Multi-identity support with per-identity Argon2id salt and isolated
  on-disk state (`~/.dmp/identities/<username>/{config.yaml,
  dmp-rs.sqlite, tsig.key, inbox.jsonl, inbox-read.json}`)
- Identity creation, unlock, switch, and lock from the header dropdown
- Inbox with read/unread tracking, single + bulk delete, persistent
  JSONL log, and a Diagnose action for receive-side debugging
- Compose with reply context (sender + timestamp + quoted plaintext)
- Contacts with deterministic-colour avatars, fetch-by-address, manual
  add, and delete
- Settings page: TSIG publish configuration, register / re-register
  against the active node, effective-resolvers display, prekey refresh,
  and backup / restore controls
- Curated DMP node list with live heartbeat checks, merged with the
  upstream directory feed at `dnsmeshprotocol.org/directory/feed.json`
- TSIG-authenticated DNS UPDATE publish (RFC 2136 + RFC 8945) and
  re-publish for key rotation, TTL refresh, and node moves
- Identity backup / restore as a single `.dmp-backup.tar.gz`
- Import existing identity from the dnsmesh CLI's flat `~/.dmp/` layout
- System dark-mode support
- First-run onboarding wizard
- About page

### Known limitations

- macOS and Windows binaries are unsigned. Use the right-click-Open workaround on macOS and "Run anyway" on Windows SmartScreen.
- No auto-update yet — re-download from GH Releases for new versions.
- Passphrase changes are not yet supported (tracked in #1).
- No mobile builds yet.
