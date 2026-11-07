# cargo-dist Shell Installer Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/477

## Overview

Linux and macOS users get a one-line install, `curl --proto '=https' --tlsv1.2 -LsSf
https://github.com/madesroches/fontaine/releases/latest/download/fontaine-installer.sh | sh`, produced by
cargo-dist's `shell` installer. The installer writes an install receipt that the self-updater (#175)
reads to decide whether it may replace the running executable. The README install section leads
with this command and states its prerequisites and caveats.

## Current State

- `rust/Cargo.toml:113-127` `[workspace.metadata.dist]` has `installers = []`, four targets
  (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-pc-windows-msvc`) and `allow-dirty = ["ci"]`.
- `.github/workflows/release.yml` is a hand-adapted dist workflow. `build-global-artifacts` already
  runs `dist build --artifacts=global` (checksums today), so a shell installer needs no new job:
  dist emits `fontaine-installer.sh` there and `host` uploads `artifacts/*`.
- `.github/scripts/release-verify.sh` fails on any unrecognised asset (its `case` list), and
  `release-sign.sh` signs only `fontaine-*.tar.xz` / `fontaine-*.zip`. An installer asset in the
  draft would make `verify-draft` fail and nothing would publish.
- `fontaine update` (#175) is not implemented; there is no receipt reader in the tree.
- README `## Install` (README.md:14-31) describes manual archive download and `cargo install`.
  `docs/verifying-releases.md` documents signatures; no `docs/` page covers installing.

## Design

### dist configuration

`installers = ["shell"]`. dist's shell installer downloads the archive for the detected target from
the release it was built for, checks the archive's sha256 (embedded in the script from the build, and only when `sha256sum` is
present), extracts it with `tar`, installs `fontaine` to `$CARGO_HOME/bin` (default `~/.cargo/bin`),
and writes the receipt. From dist v0.32.0's source, the default install-path (`CARGO_HOME`) writes
the receipt at `${XDG_CONFIG_HOME:-$HOME/.config}/fontaine/fontaine-receipt.json` (even with
`install-updater = false`) with `install_prefix` = `$CARGO_HOME` (or `~/.cargo`),
`install_layout = "cargo-home"` and `binaries = ["fontaine"]`, so the receipt describes
`<install_prefix>/bin/fontaine`. Implementation confirms this against a local
`dist build --artifacts=global` output and records it in the docs.

`install-updater` stays at its default of `false`: Fontaine updates itself, so dist's separate
updater binary is not installed.

`install-path` is left at dist's default; no override is needed. `FONTAINE_INSTALL_DIR` and
`CARGO_DIST_FORCE_INSTALL_DIR` record a different `install_prefix` with the same cargo-home layout;
`FONTAINE_UNMANAGED_INSTALL` and `FONTAINE_DISABLE_UPDATE=1` skip the receipt. Both are #175's concern.

### Workflow wiring (hand-ported; `dist generate` would overwrite `release.yml`)

1. Run `dist generate --mode=ci` into a scratch checkout/branch and diff the generated
   `release.yml` against the checked-in one. Port only installer-related differences (expected to be
   few or none, since the global job already exists). Do not commit the generated file.
2. `release-verify.sh`: accept `fontaine-installer.sh` only
   when `channel` is `release`; the dev channel still fails on it. Keep "unrecognised asset" as the
   default failure. Extend the script's inline self-tests in `.github/workflows/ci.yml`'s "Release signing" step
   (`make_fixture`, `expect_fail`; see Testing) accordingly.
3. `release-sign.sh`: unchanged. The installer is not signed (see Trade-offs).
4. `dev.yml`: dist bakes the download URL `.../releases/download/v<version>/` into the installer, and
   a dev build's tag is `dev-<n>`, so that URL does not exist. Delete `fontaine-installer.sh` in both
   the `sign` job and the `host` job's "Remove the dist manifests" step (`host` re-downloads all
   `artifacts-*`, installer included, before `gh release create artifacts/*`, and `verify-draft`
   on the dev channel rejects the installer), with a comment giving that reason.
5. `host` in `release.yml` uploads `artifacts/*`, so the installer reaches the release without
   change; confirm `sha256.sum` and `verify-draft` still pass.

### Verification parity

The installer verifies the archive's sha256, the same checksum the updater compares; both fetch from
the same GitHub release. The check depends on `sha256sum` being present (no `shasum` fallback, which
matters on macOS); otherwise dist's installer skips it with a notice (printed via `say`, so `--quiet` suppresses it) rather than failing. It does
not verify the minisign signature (no minisign on the user's machine). Docs say so and point to
`docs/verifying-releases.md` for manual verification. If #175's updater later adds attestation,
the installer follows in that issue.

### Platforms

No target is added. README lists supported install platforms
explicitly (Linux x86_64, macOS arm64 and x86_64). The shell installer is for Linux and macOS;
Windows keeps the manual archive until its own installer issue.

### Install receipt

Written by dist's installer at the path and with the fields given under dist configuration; Fontaine
adds no code in this issue. #175 owns reading it and refusing
self-update when the receipt does not match the running executable.

## Implementation Steps

1. `rust/Cargo.toml`: set `installers = ["shell"]`.
2. Run `dist plan`, then a local `dist build --artifacts=local` for the host target followed by
   `dist build --artifacts=global` (checksums are embedded only from locally built targets; pinned 0.32.0 dist), to
   inspect the generated `fontaine-installer.sh`: install dir, receipt path/format, whether the
   sha256 is embedded and how the script behaves without `sha256sum`, that `sha256.sum` leaves the
   installer out, prerequisites it checks (`curl`/`wget`, `tar`/`unzip`), proxy behaviour. Record findings
   for docs.
3. Diff `dist generate` output against `.github/workflows/release.yml`; port installer-related
   changes by hand and update the file's header comment if the hand-adaptation list changes.
4. `.github/scripts/release-verify.sh`: allow the installer asset on the `release` channel only.
5. `.github/workflows/dev.yml`: remove the installer asset in both the `sign` job and `host`'s
   "Remove the dist manifests" step.
6. README `## Install`: lead with the one-line installer (Linux, macOS), then the manual archive
   (all platforms, including Windows), then `cargo install --git`. State supported platforms, the
   `curl`/`wget` and `tar` prerequisites, that the sha256 check needs `sha256sum` and is otherwise
   skipped with a notice that `--quiet` suppresses, that the installer and `cargo install --git` both install to
   `$CARGO_HOME/bin` (the installer overwrites a cargo-installed binary; `cargo install --git ... --force`
   is needed to overwrite an installer-installed one), the receipt, and the `[network]` caveat (the installer
   ignores Fontaine's proxy, CA and mTLS config; behind a proxy, set `https_proxy` /
   `SSL_CERT_FILE` for `curl`, or download the archive manually).
7. New `docs/installing.md` (one topic per page): installer command, what it writes, prerequisites,
   `[network]` caveat, verification parity (including the `sha256sum` dependency), the same overwrite
   semantics for `cargo install --git`, uninstall (remove the binary, the receipt
   `~/.config/fontaine/fontaine-receipt.json` and `~/.config/fish/conf.d/fontaine.env.fish`, the only
   Fontaine-specific file; `$CARGO_HOME/env`, `$CARGO_HOME/env.fish` and the `. "$HOME/.cargo/env"` rc
   lines are shared with rustup, so removing them is safe only when rustup/cargo are not installed, as
   it would otherwise take cargo/rustc off PATH; do not delete the whole `~/.config/fontaine`, which is
   also Fontaine's config dir; `NO_MODIFY_PATH` set at install time skips the rc edits), manual and
   source installs, and a link from `docs/index.md`. Update `docs/verifying-releases.md` to say the
   installer is not signed and verifies sha256 only, and reword the line near 61 ("Once Fontaine's
   installers enable GitHub artifact attestations…") so it does not imply the shell installer checks
   attestations.
8. `tasks/design/platforms.md` §Releases: one sentence on the installer, and update "The README install
   section lists both install paths" to name the installer, the manual archive and `cargo install --git`. Master plan `## Decisions`
   (Build and release): dated lines for the unsigned installer, no dev-channel installer, and no
   arm64 Linux target.
9. `CHANGELOG.md`: `Added` entry under `[Unreleased]` (#477).

## Files to Modify

- `rust/Cargo.toml`
- `.github/workflows/release.yml` (only if the generated diff requires it)
- `.github/workflows/dev.yml`
- `.github/scripts/release-verify.sh`
- `.github/workflows/ci.yml` ("Release signing" step: the inline `release-verify.sh` self-tests)
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

- AFK: The installer is itself unsigned and does not verify the minisign signature, the most
  conservative option that satisfies the issue without new key handling.
- AFK: No installer on the dev channel (dist bakes a `v<version>` download URL; dev tags are `dev-<n>`).
- AFK: Uninstall docs remove only the binary, the receipt and `fontaine.env.fish`; the shared `$CARGO_HOME/env`, `env.fish` and `.cargo/env` rc lines are removable only when rustup/cargo are not installed (dist shares them with rustup).
- AFK: Linux arm64 is not claimed as supported, so `aarch64-unknown-linux-gnu` is not added.

## Documentation

`README.md` install section, new `docs/installing.md`, `docs/index.md`, `docs/verifying-releases.md`,
`tasks/design/platforms.md`, `CHANGELOG.md`.

## Testing Strategy

- `release-verify.sh` is a silent-failure surface (a bad asset list either blocks every release or
  lets an unexpected asset through). Add cases to the inline self-tests in the `.github/workflows/ci.yml`
  "Release signing" step (`make_fixture`, `expect_fail`): installer on `dev` fails, using a dev-signed fixture
  (`make_fixture d; sign d main dev`, mirroring the existing helpers): `verify d dev` passes, then with
  `fontaine-installer.sh` added it fails. The release positive case is a direct `verify` on a copy of
  `pristine` plus the installer (`expect_fail` cannot express a pass, and the `pristine` fixture fails
  as dev already for a channel mismatch). An unrelated extra asset still fails.
- CI's `dist plan` job (already run per PR) validates the `Cargo.toml` change.
- Everything about running the installer against a real release needs a real GitHub Release; see
  Manual Verification.

## Manual Verification

Not automatable in PR CI because it needs a published release:

1. On a Linux x86_64 and a macOS arm64 machine, after the first release carrying the installer:
   `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/latest/download/fontaine-installer.sh | sh`;
   expect `fontaine --version` to print the release version and the receipt file to exist.
2. Tamper check: edit the downloaded script's embedded sha256 and run it; expect a checksum failure
   and no installed binary. Run on a machine with `sha256sum`, since the check is skipped without it.
