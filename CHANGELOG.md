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

## 0.1.0-alpha.2 — 2026-05-04

Pivot release: messenger-style chat UI, Android APK in the build
matrix, and a round of audit fixes.

### Added

- **Messenger-style chat shell.** Replaces the inbox + compose
  mailbox at `/`. Conversation list keyed per pinned contact (plus
  one "Unknown senders" bucket for un-pinned), thread view with
  bubbles, bottom composer, per-thread `⋯` menu with **Clear chat**.
- **Per-identity sent-message store** (localStorage proto;
  `lib/stores/sent.ts`). Auto-deletes after a user-configurable TTL
  (1 / 6 / 12 / 24 hours, default 24h) — surfaced under
  Settings → "Sent-message retention". To be promoted to a
  Rust-side `sent.jsonl` once the wire shape is confirmed.
- **MSN/Trillian-style emoticon shortcodes.** `:)`, `<3`, `(y)`,
  etc. render as unicode glyphs in chat bubbles via
  `lib/emoticons.ts`. Composer also has a 35-glyph emoji picker.
- **Hidden polling.** 60s background tick + 10s "fast" tick while a
  thread is open. Replaces the manual Refresh.
- **+ New chat picker.** Search existing contacts or fetch a new
  contact by `user@domain` inline; opens the conversation directly.
- **Header overflow `⋯` menu** (Chat / Contacts / Identities /
  Settings / About) replaces the always-visible left sidebar.
  Brand mark links home from anywhere.
- **Android APK build.** New `release-android.yml` workflow added
  to the orchestrator; the next `desktop-v*` tag fans out to four
  platforms instead of three. APK is signed with Android's debug
  keystore — sideloadable, won't pass the Play Store. Real signing
  comes when there's a keystore secret to wire in. **Experimental:**
  the build proves the APK compiles; runtime DNS UPDATE behavior
  inside Android's network sandbox is still being validated.

### Changed

- Default window size shrunk from 1024×720 to **680×720** (2/3
  width). `minWidth` dropped from 800 to 360 so the shell collapses
  cleanly on narrow desktops.
- Chat shell breakpoint set to 560px (two-column → single-column),
  with the conversation rail trimmed from 320px to 240px so the
  default window keeps both panes visible.
- Legacy `/compose?to=&reply_to=` URLs now redirect into the chat
  shell with both query params preserved.
- "Compose" button on the Contacts page renamed to "Open chat" and
  routes to `/?contact=<username>`.
- Bumped Rust edition 2021 → 2024 (`src-tauri/Cargo.toml`).

### Fixed

- Send-during-switch race: `send()` now snapshots the active
  identity before awaiting the SDK and drops the row if the
  identity changed mid-flight, instead of writing to the wrong
  per-identity store.
- Identities page switch / lock / create paths now also
  `clearSent` + `hydrateSent` (mirrors the header), so sent rows
  no longer bleed across identities when the switch goes through
  `/identities`.
- `pollInbox` and `hydrateInbox` capture identity at start and
  bail on resolve if it changed, preventing a stale RPC from
  clobbering a freshly-loaded inbox after a lock or switch.
- Mark-read is now a `$effect` keyed on the active conversation's
  message list, so messages arriving via the 10s fast poll mark
  read live without requiring a click.
- New chat picker focuses the search input on open, restores focus
  to the opener on close, and binds Escape to the dialog itself
  (the prior backdrop-bound listener never fired).

### Removed

- Orphan exports from `lib/stores/inbox.ts` (`inboxBusy`,
  `markAllRead`, `deleteMessage`, `inboxSnapshot`) and the orphan
  `receiveMessagesDiagnostic` API + its types from `lib/api.ts`.
  The diagnostic surface can come back if/when needed.

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
