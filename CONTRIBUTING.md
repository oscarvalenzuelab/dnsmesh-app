# Contributing

Contributions welcome. Before opening a pull request, please run:

```sh
bun install
bun run check
cd src-tauri
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo check
cargo test
```

The desktop app depends on the [`dnsmesh-rs`](https://github.com/oscarvalenzuelab/dnsmesh-rs)
SDK for protocol logic. UI changes should target Tauri commands defined in
`src-tauri/src/commands/`. Keep frontend code free of protocol logic; call
into Rust via `@tauri-apps/api` (wrapped in `src/lib/api.ts`) instead.

## Building locally

The current `src-tauri/Cargo.toml` declares the dnsmesh-rs crates by
**path**, expecting the two repos to be sibling clones:

```
~/Projects/.../DMP/
├── dnsmesh-rs/         # SDK (Rust workspace)
└── dnsmesh-app/        # this repo
```

Layout assumed:

- `dnsmesh-app/src-tauri/Cargo.toml` references
  `../../dnsmesh-rs/crates/{dnsmesh-core,dnsmesh-net,dnsmesh-storage,dnsmesh-client}`.
- Each crate must be on its `main` branch (or whichever branch carries
  the SDK changes you want to consume).

If you don't have `dnsmesh-rs` checked out next to this repo, clone it
there before running `cargo build`.

### Producing a local installer (macOS)

`cargo tauri build --debug` will build the binary, then produce both
a `.app` bundle and a `.dmg` archive. The DMG step shells out to
[`create-dmg`](https://github.com/create-dmg/create-dmg), which is not
included in macOS by default:

```sh
brew install create-dmg
```

Without it, the `.app` still builds (it lands at
`src-tauri/target/{debug,release}/bundle/macos/DNSMesh.app` and you can
double-click to launch it) but the DMG step fails. CI runners install
this dependency automatically.

## Production / CI builds

Path dependencies are convenient for iteration but unsuitable for a
release build (they require both repos on the build host at fixed
relative paths). For production builds, swap each `path = "..."` line in
`src-tauri/Cargo.toml` for a git tag pin:

```toml
dnsmesh-core = { git = "ssh://git@github.com/oscarvalenzuelab/dnsmesh-rs.git", tag = "sdk-v0.1.0", package = "dnsmesh-core" }
dnsmesh-net = { git = "ssh://git@github.com/oscarvalenzuelab/dnsmesh-rs.git", tag = "sdk-v0.1.0", package = "dnsmesh-net" }
dnsmesh-storage = { git = "ssh://git@github.com/oscarvalenzuelab/dnsmesh-rs.git", tag = "sdk-v0.1.0", package = "dnsmesh-storage" }
dnsmesh-client = { git = "ssh://git@github.com/oscarvalenzuelab/dnsmesh-rs.git", tag = "sdk-v0.1.0", package = "dnsmesh-client" }
```

(Replace `sdk-v0.1.0` with the current `sdk-v*` tag.) Tags are minted by
`dnsmesh-rs`'s release workflow; if no tag exists yet, a SHA pin works as
an interim measure:

```toml
dnsmesh-core = { git = "ssh://git@github.com/oscarvalenzuelab/dnsmesh-rs.git", rev = "<sha>", package = "dnsmesh-core" }
```

## Architecture overview

- **Frontend**: SvelteKit (Svelte 5 runes) under `src/`. Each route is a
  thin shell that calls into `src/lib/api.ts`, which wraps Tauri's
  `invoke()`.
- **Backend**: Tauri 2 host under `src-tauri/`. `src-tauri/src/lib.rs`
  registers commands; `src-tauri/src/commands/` holds the per-domain
  command modules (identity, contacts, messaging, nodes).
- **State**: a single `AppState` (in `src-tauri/src/state.rs`) is
  managed by Tauri. It holds the active `DmpClient` behind a
  `tokio::sync::RwLock` and resolves the on-disk identity index at
  `~/.dmp/identities/` (overridable via `DMP_DESKTOP_HOME` /
  `DMP_CONFIG_HOME`).
- **Multi-identity**: each identity owns a directory under
  `~/.dmp/identities/<username>/` containing its sqlite db and
  per-identity `config.yaml` (publish settings, resolver overrides).
  Switching identities drops the active client and opens a new one.

## Releases and the `.dnsmesh-rs-ref` SDK pin

The release workflow (`.github/workflows/release.yml`) clones the
sibling `dnsmesh-rs` repository at the SHA stored in `.dnsmesh-rs-ref`
at the repo root. This avoids letting whatever main HEAD happened to be
at tag-push time silently determine the release contents.

To bump the pin:

```sh
# in dnsmesh-rs:
git rev-parse main

# in dnsmesh-app, paste the SHA:
echo "<sha>" > .dnsmesh-rs-ref
git add .dnsmesh-rs-ref
git commit -m "Bump dnsmesh-rs pin to <sha>"
```

The bump is an explicit reviewable commit. CI fails fast if the file is
missing or empty.

## Content-Security-Policy

`src-tauri/tauri.conf.json` ships a restrictive CSP. One known
relaxation: `style-src` includes `'unsafe-inline'` because Svelte's
scoped style emission may produce inline `<style>` blocks during dev
server hot-reload and for some component patterns. If you can confirm
the production bundle no longer needs it (e.g. by auditing the bundler
output), tighten this string and remove the relaxation.

The CSP also allows `connect-src 'self' ipc: http://ipc.localhost`
which is the standard Tauri 2 IPC origin. `frame-ancestors 'none'`
plus `object-src 'none'` shut down clickjacking / plugin embedding
respectively.

## Android release signing (stable alpha keystore)

Android refuses to upgrade an APK whose signing certificate doesn't
match the one already installed (`INSTALL_FAILED_UPDATE_INCOMPATIBLE`),
and falling back to "uninstall first" wipes user identities, contacts,
and inbox.

For sideload upgrades to work across alpha releases, the
`release-android.yml` workflow signs every APK with a stable debug
keystore stored as the `ANDROID_DEBUG_KEYSTORE_B64` repo secret. If
the secret is missing the workflow falls back to an ephemeral keystore
and warns loudly — useful for PR / fork builds, broken for releases.

To provision (one-time, by a repo admin):

```sh
# 1. Generate the keystore locally. The credentials below are the
#    well-known public Android debug values — DO NOT use these for a
#    production signing keystore. This is an alpha-phase stopgap.
keytool -genkeypair -v -keystore alpha-debug.keystore \
  -storepass android -alias androiddebugkey -keypass android \
  -keyalg RSA -keysize 2048 -validity 10000 \
  -dname "CN=DNSMesh Alpha Debug,O=DNSMesh,C=US"

# 2. Base64-encode (single line; macOS `base64` already emits one).
base64 -i alpha-debug.keystore | tr -d '\n' > alpha-debug.keystore.b64

# 3. Paste the file contents as a new repo secret named
#    ANDROID_DEBUG_KEYSTORE_B64 under
#    https://github.com/oscarvalenzuelab/dnsmesh-app/settings/secrets/actions

# 4. Stash the original `alpha-debug.keystore` somewhere safe (password
#    manager, vault). Re-creating it would change the SHA-256
#    fingerprint and break upgrades for everyone who already installed
#    an APK signed by the previous one. Wipe the local copy from disk
#    once stashed:
shred -u alpha-debug.keystore alpha-debug.keystore.b64
```

To verify the secret works after provisioning, re-run the
`release-android` workflow against a previous tag and check the log
for `Using ANDROID_DEBUG_KEYSTORE_B64 secret` + the printed SHA-256
fingerprint.

When the secret is missing, the workflow **fails by default** rather
than silently shipping a fresh-fingerprint APK that would force every
user to uninstall + reinstall (wiping their data). For PR / fork /
local-experiment runs that genuinely cannot read the secret, dispatch
the workflow manually with `allow_ephemeral_signing=true` — the APK
will build, install standalone on a fresh device, and emit warnings
about not being upgrade-compatible. **Never set
`allow_ephemeral_signing=true` from the orchestrating `release.yml`**;
that path is reserved for tagged release runs and must hard-fail on
a missing secret.

When the alpha graduates and a real signing keystore exists, replace
the value of `ANDROID_DEBUG_KEYSTORE_B64` with the production keystore
and tighten the passphrase handling in `release-android.yml` (the
current hard-coded `pass:android` is fine for the well-known debug
identity but not for a real signing key).

## Code style

- `cargo fmt --all` for Rust; SvelteKit code follows the project's
  default Prettier config.
- No emojis in source files.
- No AI-attribution trailers in commits or PRs.
