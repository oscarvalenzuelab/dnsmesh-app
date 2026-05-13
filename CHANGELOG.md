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

### Added

- **Auto-refresh identity on unlock + 24h heartbeat.** Alpha testers who
  open the app sporadically were falling out of DNS between sessions —
  new contacts trying to fetch their address got NXDOMAIN until the
  user clicked Re-publish. The desktop now fires `publish_identity`
  once after unlock/switch and again every 24h while the app stays
  open. Skips silently for identities without a TSIG block; logs
  transient publish failures to the console without surfacing a modal.
  (#4)

## 0.1.0-alpha.6 — 2026-05-06

Network resilience release. Fixes the Android-after-VPN-disconnect lockup
and adds a manual recovery button.

### Fixed

- **Resolver pool no longer wedges after a network change.** Hickory's
  UDP sockets stay bound to whichever interface was active at
  construction time; on Android, dropping a VPN left them pointing at
  a dead tunnel and queries silently failed until the user locked +
  unlocked the identity. The unlocked client now sits behind a
  `RefreshableReader` whose inner pool can be swapped at runtime
  without rebuilding the SDK client. (#15)

### Added

- **Settings → Refresh network** button. Rebuilds the resolver pool's
  UDP sockets to recover from VPN drops or other network changes.
- Inbox poll auto-fires the refresh after two consecutive failures. If
  failures keep happening past the auto-refresh, a banner on the chat
  list points at Settings.

## 0.1.0-alpha.5 — 2026-05-06

First release under the new `dnsmesh-app` name.

### Changed

- Project renamed from `dnsmesh-desktop` to `dnsmesh-app`. Cargo
  crate, Rust lib, Tauri identifier (`io.dnsmesh.desktop` →
  `io.dnsmesh.app`), `package.json` name, release tag prefix
  (`desktop-v*` → `v*`), and all repo URLs in README, SECURITY,
  CONTRIBUTING, and the in-app About page point at the new
  GitHub URL. Old `dnsmesh-desktop` URLs auto-redirect on GitHub.
- Releases no longer auto-mark alpha/beta/rc tags as pre-release.

## 0.1.0-alpha.4 — 2026-05-06

Android UX polish.

### Changed

- Android header now sits below the system status bar instead of
  being painted over by the clock and notification icons. CSS uses
  `env(safe-area-inset-*)`; desktop is unchanged.
- Header tap targets bumped to 40px so the menu button is reachable
  on phones.
- Global-nav glyph swapped from `⋯` to `☰`. Hamburger is the
  recognized affordance for primary navigation; the per-thread
  `⋯` (Clear chat) stays as kebab since it is contextual.
- The committed Android icons in `src-tauri/icons/android/` now
  ship with the APK. Tauri's mobile init was generating a
  placeholder icon set that the release was using instead.
- Em-dash sweep across the chat shell, About, Contacts, Settings,
  and Identities pages.

## 0.1.0-alpha.3 — 2026-05-05

Android-becomes-functional release. The `.apk` shipped in alpha.2
installed but crashed on launch and required out-of-band re-signing
to install at all; with the fixes in this tag, sideloading and
running on Android 14 actually works end-to-end.

### Changed

- **Android identities root** now resolves via Tauri's
  `app_local_data_dir()` instead of `$HOME` (which Android sandboxes
  don't expose). Desktop behavior unchanged. (#7)
- **Android APK is now debug-signed by CI.** Generates the public
  Android debug keystore on the fly, signs with `apksigner`, and
  uploads only the signed asset. Sideload via `adb install` now works
  out of the box. (#9)

### Fixed

- **Clear chat button.** The Tauri webview was silently returning
  `false` from `window.confirm()`, so the click chain never reached
  the actual clear path. Replaced with a two-click inline confirm
  (`Clear chat` → `Yes, clear / Cancel`) that resets cleanly across
  conversation switches and identity switches. (#6)
- **Android crash at first launch.** Workaround for a wry < 0.55.x
  ProGuard regression: R8 was stripping `WryActivity.getId()` (the
  Kotlin auto-generated getter no Java/Kotlin code calls but tao's
  JNI bridge does at `onActivityCreate`), causing `tao` to panic
  with `JavaException: NoSuchMethodError`. The release-android
  workflow now patches the keep rule, busts R8's incremental cache,
  re-runs gradle, and verifies via `dexdump` that `getId()` actually
  landed inside the WryActivity class block. The workaround
  self-removes once wry > 0.55.x flows in transitively (upstream
  fix: tauri-apps/wry#1721). (#8)

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
  to the orchestrator; the next `v*` tag fans out to four
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
