# `fontaine update` Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/509

## Overview

Add a `fontaine update` subcommand that updates the installed binary to the newest build on the
channel it was installed from (`release` or `dev`), reports the outcome and exits. It is the manual
path of #175; it never starts the TUI or a session, never switches channels, and verifies every
archive against the channel's embedded minisign key before touching the installed binary.

Scope is the manual command only. The background startup check, the staged/next-launch swap, the
Windows updater process and the TUI notice stay in #175.

## Current State

- No update command exists. `cli::Command` (`rust/fontaine/src/cli.rs:146`) has `auto-mode`,
  `plugin`, `models`, `doctor`; dispatch is a chain of `if let Some(cli::Command::X(args))` in
  `rust/fontaine/src/lib.rs:323-351`.
- `rust/fontaine/src/release_signature.rs` already verifies a downloaded archive: `Channel`,
  `TrustedKeys::embedded()`, `verify_file(keys, channel, archive, expected_asset, signature)`
  returning `Verified { version, commit, .. }`. It enforces neither downgrade nor channel-switch;
  its docs say the caller does.
- Installers (`.github/scripts/`, `docs/installing.md`) write a receipt
  `fontaine-receipt.json` under `${XDG_CONFIG_HOME:-~/.config}/fontaine/` (Windows:
  `%LOCALAPPDATA%\fontaine\`, or `%XDG_CONFIG_HOME%\fontaine\` when that is set) in cargo-dist's
  schema: `install_prefix`, `install_layout`, `binaries`, `version` and the provider.
  A dev install's version is `<base>-dev.<n>+<sha7>`; a release install's is plain semver.
- Release assets: `fontaine-<target>.tar.xz` (`.zip` on Windows) plus `<asset>.minisig`, `.sha256`
  and `sha256.sum`. Tagged releases are `vX.Y.Z`; dev builds are immutable prereleases `dev-<n>`
  and the moving `dev` prerelease holds only installers.
- HTTP clients go through `fontaine_tools::tls::build_client(network, ...)` so `[network]`
  (proxy, CA bundle, mTLS) applies. `reqwest` is a workspace dependency; `sha2` and
  `minisign-verify` are too.
- `fontaine_core::file_lock` exists for locked access to persisted data (`lock_registry` in
  `rust/fontaine/src/plugin.rs` is the precedent); there is no helper for replacing an executable
  or for atomically rewriting a single file.

## Design

Channel is derived, not configured: **the current channel is the channel of the install receipt**
(`-dev.` in the receipt version means `dev`, otherwise `release`). This honours #175's "changing
channels is explicit and never a side effect of an update" without adding a config key; a
`--channel` flag is deliberately not added here.

New module `rust/fontaine/src/update/mod.rs` (CLI glue, with submodules) over small, separately testable pieces:

```
receipt  -> InstallInfo { channel, version, binary_path }  (binary_path = install_prefix/bin/<binaries[0]>)       (None => not self-updatable)
resolve  -> latest release on channel: tag, version, asset URL   (GitHub releases API via build_client)
download -> archive + .minisig into a fresh temp dir beside the binary
verify   -> release_signature::verify_file + version/channel policy
install  -> extract binary, atomically replace the running executable (unix rename; see Windows)
report   -> one line per outcome, exit code
```

- **Receipt match.** The only receipt fields read are `install_prefix`, `binaries` and `version`;
  `install_layout` is ignored. The parser rejects a relative or `~` `install_prefix` and accepts a
  `binaries[0]` that already ends in `.exe`. Self-replace only when a receipt exists, parses, and its binary,
  `install_prefix/bin/<binaries[0]>` (`.exe` on Windows), canonicalised, equals the canonicalised
  `std::env::current_exe()`. Otherwise (cargo install, package manager,
  container, moved binary) print the upgrade hint and exit non-zero without writing anything.
  Hint selection reuses nothing speculative: a generic message naming the installer one-liner for
  the active channel from `docs/installing.md`.
- **Latest on channel.** `release`: `/releases/latest` (the latest non-prerelease `vX.Y.Z`).
  `dev`: list releases, keep non-draft tags matching `^dev-[0-9]+$`, take the max `n` numerically
  over a bounded number of pages. The client sets the User-Agent as `fontaine-tools` webfetch does
  and has a deliberate redirect policy. Compared to the installed version: semver for `release`;
  for `dev`, the build number `n` numerically (not semver; the receipt version
  `<base>-dev.<n>+<sha7>` supplies the installed `n`). Same or older is "already up to date"
  (exit 0); a downgrade is never installed. The binary does not embed its build number
  (`build.rs` emits only the git sha and target; `--version` prints `CARGO_PKG_VERSION`), so the
  receipt `version` is the only source of the installed version.
- **Verification policy** (the caller's half of `verify_file`): `Verified.channel` equals the
  receipt's channel, `Verified.file` equals the expected asset, and the `.minisig` is mandatory.
  For `release`, `Verified.version` equals the tag minus its leading `v`. For `dev`,
  `Verified.version` matches `<semver>-dev.<m>+<hex>` with `m` equal to the tag's `n`. Any failure (missing signature, bad signature, no key for the channel,
  I/O) leaves the installed binary untouched and prints why. The `.sha256` is not trusted.
- **Install, unix.** Extract the `fontaine` binary from the archive into a temp file in the same
  directory as the target (same filesystem), created exclusively. Only the single member named
  `fontaine` (`fontaine.exe`) is extracted, as a regular file; archive paths and links are never
  followed. `chmod` to the old mode, then `rename` over the
  target. The running process keeps its inode. Temp files are removed on every exit path.
- **Install, Windows.** The running exe cannot be replaced by itself; the long-term design is
  #175's separate updater process. For this command: rename the running exe to
  `fontaine.exe.old`, move the new one into place, and best-effort delete `.old` at the next
  `fontaine update`; if the rename fails, report it and leave the install untouched. Any stale
  `fontaine.exe.old` is deleted at the start of the run, before the rename, so the rename never
  collides with it. Recorded as a Decisions line because it is narrower than #175.
- **Receipt after install.** Read-modify-write of only `version`, set to `Verified.version` (not a
  tag-derived value); every other field is kept, the write goes through a temp file in the same
  directory plus `rename`, and an unparseable version is rejected. The whole update holds a
  `fontaine_core::file_lock` beside the receipt (as `lock_registry` does in
  `rust/fontaine/src/plugin.rs`) so concurrent updates serialise. Failure to rewrite warns via
  `print_warnings`/stderr and is not silent.
- **Exit codes.** `0` updated or already up to date; the existing config exit code for
  unreadable config (`Command::Update` dispatches after `load_config`, so `[network]` applies and a
  config error exits with the config exit code); a new nonzero for "not self-updatable", download failure and verification
  failure (reuse existing constants in `lib.rs` where one fits).
- **Error text** carries the chain via `fontaine_tools::error_chain::chain`; typed errors only for
  the kinds the caller dispatches on (`NotSelfUpdatable`, `Verify`, `Network`).
- **Headless/guardian.** The command is a user-run CLI subcommand outside any agent session, so no
  tool risk class applies; it is not exposed as a tool.

## Implementation Steps

1. `rust/fontaine/src/cli.rs`: add `Command::Update(UpdateArgs)`.
2. `rust/fontaine/src/update/` (`mod.rs`, `receipt.rs`, `release_index.rs`, `install.rs`): the
   pieces above. `release_index` takes the API base URL as a parameter for tests.
3. `rust/fontaine/src/lib.rs`: dispatch `Command::Update` beside the other subcommands, after
   `load_config`, building the client from the loaded `[network]`; returns before any session setup.
4. Cargo: add archive extraction deps only if none of the workspace's already cover `tar.xz`/`zip`
   (check `Cargo.lock` first; keep `cargo deny check` green).
5. Docs and changelog: see Documentation.

## Files to Modify

- `rust/fontaine/src/cli.rs`, `rust/fontaine/src/lib.rs`
- new `rust/fontaine/src/update/{mod,receipt,release_index,install}.rs`
- `rust/fontaine/Cargo.toml` (and `rust/Cargo.toml` if a dependency is added)
- `docs/installing.md`, `docs/verifying-releases.md`, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Decisions

- 2026-11-08 (#509): `fontaine update` updates only on a matching install receipt and derives the
  channel from the receipt version (not config: no new user-scope key, and project config cannot
  flip it); it never switches channels. Windows swaps via rename-aside
  rather than #175's separate updater process.

## Documentation

- `docs/installing.md`: new "Updating" section (channel semantics, not-self-updatable installs,
  exit codes, `[network]` applies).
- `docs/verifying-releases.md`: replace "Fontaine does not update itself yet" with what
  `fontaine update` verifies.
- `CHANGELOG.md` entry.

## Testing Strategy

- Unit: receipt parsing and channel derivation (release, dev, malformed, missing, binary
  mismatch, relative or `~` `install_prefix`, `binaries[0]` already ending in `.exe`, ignored
  `install_layout`); version/build-number comparison (newer, equal, older); policy rejecting a
  wrong-channel, wrong-version or unsigned archive; a hostile archive (traversal path, symlink or
  extra members) installs nothing but the `fontaine` regular file; hint text per channel.
- Integration (wiremock for the release index and assets, a temp "install" directory with a
  receipt and a fake `fontaine` binary signed by a test key through `TrustedKeys::from_pub_files`):
  update succeeds and replaces the binary and receipt version; bad signature leaves the binary
  byte-identical; already up to date; not self-updatable exits non-zero with no writes. Pins the silent-failure class (a corrupt or unsigned binary installed).
- Windows rename-aside is compile-checked with the `x86_64-pc-windows-gnu` clippy target.

## Manual Verification

1. Install a dev build with the installer, run `fontaine update`; expect an "updated to dev.<n>" output line and the
   receipt `version` set to the new `<base>-dev.<n>+<sha7>`. Not
   automated because it needs a real published `dev-<n>` signed by the real key (the end-to-end
   check #175 requires on all three OSes before release).
2. Compare a receipt freshly written by a real installer (shell and PowerShell) with the fields the
   parser reads (`install_prefix`, `binaries`, `version`), to confirm the schema matches real
   cargo-dist output, not just the test fixtures.
