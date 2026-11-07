# cargo-dist Shell Installer Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/477

## Overview

Linux and macOS users get a one-line install, `curl --proto '=https' --tlsv1.2 -LsSf
https://github.com/madesroches/fontaine/releases/latest/download/fontaine-installer.sh | sh`, produced by
cargo-dist's `shell` installer. The installer writes an install receipt that the self-updater (#175)
reads to decide whether it may replace the running executable. The README install section leads
with this command and states its prerequisites and caveats.

## Current State

- `rust/Cargo.toml:113-130` `[workspace.metadata.dist]` has `installers = []`, four targets
  (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-pc-windows-msvc`) and `allow-dirty = ["ci"]`.
- `.github/workflows/release.yml` is a hand-adapted dist workflow. `build-global-artifacts` already
  runs `dist build --artifacts=global` (checksums today), so a shell installer needs no new job:
  dist emits `fontaine-installer.sh` there and `host` uploads `artifacts/*`.
- `.github/scripts/release-verify.sh` fails on any unrecognised asset (its `case` list), and
  `release-sign.sh` signs only `fontaine-*.tar.xz` / `fontaine-*.zip`. An installer asset in the
  draft would make `verify-draft` fail and nothing would publish.
- `.github/workflows/dev.yml` shares the same dist global build, but a dev build's tag is
  `dev-<n>` while dist bakes the download URL `.../releases/download/v<version>/` into the
  installer, which does not exist for a dev build.
- `fontaine update` (#175) is not implemented; there is no receipt reader in the tree.
- README `## Install` (README.md:14-31) describes manual archive download and `cargo install`.
  `docs/verifying-releases.md` documents signatures; no `docs/` page covers installing.

## Design

### dist configuration

`installers = ["shell"]`. dist's shell installer downloads the archive for the detected target from
the release it was built for, checks the archive's sha256 (embedded in the script from the build),
extracts it with `tar`, installs `fontaine` to `$CARGO_HOME/bin` (or the XDG-style location dist
chooses), and writes the receipt. Implementation must confirm the exact install directory,
receipt path and receipt fields from a local `dist build --artifacts=global` output, and
record them in the docs; the plan does not assume them.

`install-updater` stays at its default of `false`: Fontaine updates itself, so dist's separate
updater binary is not installed.

`install-path` is left at dist's default unless the generated script's behaviour conflicts with the
receipt requirement of #175 (check during implementation; record any override as a Decision).

### Workflow wiring (hand-ported; `dist generate` would overwrite `release.yml`)

1. Run `dist generate --mode=ci` into a scratch checkout/branch and diff the generated
   `release.yml` against the checked-in one. Port only installer-related differences (expected to be
   few or none, since the global job already exists). Do not commit the generated file.
2. `release-verify.sh`: accept `fontaine-installer.sh` (and its `.sha256` if dist emits one) only
   when `channel` is `release`; the dev channel still fails on it. Keep "unrecognised asset" as the
   default failure. Extend the script's self-tests (see Testing) accordingly.
3. `release-sign.sh`: unchanged. The installer is not signed (see Trade-offs); it is trusted over
   HTTPS like the first install is.
4. `dev.yml`: delete `fontaine-installer.sh` (and any installer checksum) from `artifacts/` before
   signing, next to the existing "Remove the dist manifests" step, with a comment that the baked
   download URL points at a release tag a dev build does not have.
5. `host` in `release.yml` uploads `artifacts/*`, so the installer reaches the release without
   change; confirm `sha256.sum` and `verify-draft` still pass.

### Verification parity

The installer verifies the archive's sha256, the same checksum the updater compares; both fetch from
the same GitHub release. It does not verify the minisign signature (no minisign on the user's
machine, and the installer itself arrives over HTTPS only). Docs say so and point to
`docs/verifying-releases.md` for manual verification. If #175's updater later adds attestation,
the installer follows in that issue.

### Platforms

No target is added. `aarch64-unknown-linux-gnu` is not built or tested today and the README does not
claim Linux arm64, so it stays out until it is supported; README lists supported install platforms
explicitly (Linux x86_64, macOS arm64 and x86_64). The shell installer is for Linux and macOS;
Windows keeps the manual archive until its own installer issue.

### Install receipt

Written by dist's installer; Fontaine adds no code in this issue. #175 owns reading it and refusing
self-update when the receipt does not match the running executable.

## Implementation Steps

1. `rust/Cargo.toml`: set `installers = ["shell"]`; update the comment above the block if it
   claims no installers.
2. Run `dist plan` and a local `dist build --artifacts=global` (with the pinned 0.32.0 dist) to
   inspect the generated `fontaine-installer.sh`: install dir, receipt path/format, checksum
   handling, prerequisites it checks (`curl`/`wget`, `tar`/`unzip`), proxy behaviour. Record findings
   for docs.
3. Diff `dist generate` output against `.github/workflows/release.yml`; port installer-related
   changes by hand and update the file's header comment if the hand-adaptation list changes.
4. `.github/scripts/release-verify.sh`: allow the installer asset on the `release` channel only.
5. `.github/workflows/dev.yml`: remove the installer asset before signing.
6. README `## Install`: lead with the one-line installer (Linux, macOS), then the manual archive
   (all platforms, including Windows), then `cargo install --git`. State supported platforms, the
   `curl`/`wget` and `tar` prerequisites, the receipt, and the `[network]` caveat (the installer
   ignores Fontaine's proxy, CA and mTLS config; behind a proxy, set `https_proxy` /
   `SSL_CERT_FILE` for `curl`, or download the archive manually).
7. New `docs/installing.md` (one topic per page): installer command, what it writes, prerequisites,
   `[network]` caveat, verification parity, uninstall (remove the binary and receipt), manual and
   source installs, and a link from `docs/index.md`. Update `docs/verifying-releases.md` to say the
   installer is not signed and verifies sha256 only, and to correct "Fontaine does not update itself
   yet" only if #175 has shipped by then (it has not).
8. `tasks/design/platforms.md` §Releases: one sentence on the installer. Master plan `## Decisions`
   (Build and release): dated lines for the unsigned installer, no dev-channel installer, and no
   arm64 Linux target.
9. `CHANGELOG.md`: `Added` entry under `[Unreleased]` (#477).

## Files to Modify

- `rust/Cargo.toml`
- `.github/workflows/release.yml` (only if the generated diff requires it)
- `.github/workflows/dev.yml`
- `.github/scripts/release-verify.sh` (and its tests)
- `README.md`
- `docs/installing.md` (new), `docs/index.md`, `docs/verifying-releases.md`
- `tasks/design/platforms.md`, `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

## Trade-offs

- **Sign the installer with minisign**: rejected for now; the installer carries no trust beyond the
  HTTPS fetch that delivered it, and signing it would not help a user piping it to `sh`. It would
  only help someone who downloads and checks it by hand, who can already verify the archives.
- **Installer on the dev channel**: rejected; dist bakes a `v<version>` download URL and dev tags are
  `dev-<n>`, so the script would 404. Dev users get builds through the updater.
- **Add `aarch64-unknown-linux-gnu`**: deferred; it needs a CI runner, test coverage and a pg2
  decision, none of which this issue asks for unless arm64 is claimed.
- **dist's own updater binary** (`install-updater = true`): rejected; #175 builds Fontaine's updater
  with signature verification.

## Decisions

- AFK: The installer verifies sha256 only and is itself unsigned (trusted via HTTPS), the most
  conservative option that satisfies the issue without new key handling.
- AFK: Linux arm64 is not claimed as supported, so `aarch64-unknown-linux-gnu` is not added.

## Documentation

`README.md` install section, new `docs/installing.md`, `docs/index.md`, `docs/verifying-releases.md`,
`tasks/design/platforms.md`, `CHANGELOG.md`.

## Testing Strategy

- `release-verify.sh` is a silent-failure surface (a bad asset list either blocks every release or
  lets an unexpected asset through). Add automated cases next to the existing script tests
  (locate them under `.github/scripts/` or `rust/fontaine/tests/`; the Release signing CI step
  runs them): installer present on `release` passes; installer on `dev` fails; an unrelated
  extra asset still fails.
- CI's `dist plan` job (already run per PR) validates the `Cargo.toml` change.
- Everything about running the installer against a real release needs a real GitHub Release; see
  Manual Verification.

## Manual Verification

Not automatable in PR CI because it needs a published release and the #175 updater:

1. On a Linux x86_64 and a macOS arm64 machine, after the first release carrying the installer:
   `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/latest/download/fontaine-installer.sh | sh`;
   expect `fontaine --version` to print the release version and the receipt file to exist.
2. Tamper check: edit the downloaded script's embedded sha256 and run it; expect a checksum failure
   and no installed binary.
3. After #175 ships, install release N and run `fontaine update` to N+1; expect success (the issue's
   "done when", blocked on #175 and the second release).

## Open Questions

- Whether dist 0.32.0's shell installer honours an install path that keeps the receipt matching the
  running executable for #175 (settled by Implementation Step 2).
