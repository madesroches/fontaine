# cargo-dist PowerShell Installer Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/478

## Overview

Windows x86_64 users get a one-line install, `powershell -ExecutionPolicy Bypass -c "irm https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.ps1 | iex"`
(the `releases/latest/download/` form once a tagged release exists), produced by cargo-dist's
`powershell` installer. It writes the install receipt that the self-updater (#175) reads, and, like
the shell installer, it checks the archive's sha256 before installing. dist's stock PowerShell
installer does no checksum check at all, so a small repo-owned patch step adds one. The installer is
signed on both channels and the dev channel republishes a rewritten copy at the stable `dev` release,
mirroring the shell installer (#477, #487).

## Current State

- `rust/Cargo.toml:113-127` `[workspace.metadata.dist]` has `installers = ["shell"]`, the target
  `x86_64-pc-windows-msvc` and `allow-dirty = ["ci"]`.
- dist v0.32.0's `installer.ps1.j2` (read from the dist source): installs to `$env:CARGO_HOME\bin`
  (default `$HOME\.cargo\bin`), edits the user `Path` registry key unless `FONTAINE_NO_MODIFY_PATH` is
  set, writes the receipt to `${env:XDG_CONFIG_HOME}\fontaine` if set, else `${env:LOCALAPPDATA}\fontaine`,
  with `install_layout = "cargo-home"`, downloads `fontaine-x86_64-pc-windows-msvc.zip` from an
  `$ArtifactDownloadUrls` base, and unpacks with `Expand-Archive`. It contains **no checksum
  verification** (no `Get-FileHash`), unlike the shell installer, which embeds the archive sha256.
- The shell installer is already wired: `build-global-artifacts` runs `dist build --artifacts=global`
  (the global job emits installers), `release-sign.sh` signs `fontaine-installer.sh` on `release`,
  `release-verify.sh` accepts it on `release` and requires a valid `.minisig`, and `dev.yml` sets
  it aside in `sign`, rewrites it with `.github/scripts/dev-installer.sh` (download URL x4,
  `APP_VERSION`, receipt `version`, usage text), signs it with `release-sign.sh dev-installer`, and
  the `stable-installer` job replaces it, with its `.minisig`, on the long-lived `dev` release.
  `host` removes it from `artifacts/` so `dev-<n>` stays installer-free.
- `fontaine update` (#175) is not implemented; no code reads a receipt.
- `docs/installing.md` and the README install section say "no Windows installer" and send Windows
  users to the archive; `docs/verifying-releases.md` and `tasks/design/platforms.md` describe the
  shell installer only.

## Design

### dist configuration

`installers = ["shell", "powershell"]`. `install-updater` stays at its default (`false`), so dist's
own updater is not installed; Fontaine updates itself (#175). No `install-path` override: the
receipt lands in `%LOCALAPPDATA%\fontaine\fontaine-receipt.json`, the location the issue names.
`FONTAINE_INSTALL_DIR`, `FONTAINE_UNMANAGED_INSTALL` and `FONTAINE_DISABLE_UPDATE` keep dist's
meanings and are #175's concern. Implementation confirms the receipt path, fields and the stock
installer text against a real `dist build --artifacts=global` output (dist is not installed here;
install v0.32.0 per `release.yml`).

### Workflow wiring (hand-ported; `dist generate` would overwrite `release.yml`)

`dist generate --mode=ci` in a scratch checkout, diffed against `release.yml`; port installer-related
differences only (expected none: the global job already exists).

### The sha256 check (patch of dist's output)

New script `.github/scripts/windows-installer.sh <installer> <sha256 file>`, run in `sign` on both
channels before signing. It inserts, immediately after the single stock line
`  Invoke-DownloadFile -client $wc -url $url -path $dir_path` in `Download` (the updater download uses a
different `-path`), a call `Assert-ArchiveSha256 -Path $dir_path -Expected '<hex>'`, and adds the
function definition immediately before the single `function Download(` line (exact-count anchor; the
template ends with a top-level `Install-Binary "$Args"` call and `param(...)` must stay first). The hex is read from the `fontaine-x86_64-pc-windows-msvc.zip.sha256` file passed as `<sha256 file>`, so it is the digest of the archive that will be published. The function lives
in a repo file, `.github/scripts/windows-installer-sha256.ps1`, inserted verbatim (single source for
the shipped code and its test); it compares `(Get-FileHash -Algorithm SHA256 $Path).Hash` with the
expected value case-insensitively and `throw`s on mismatch, which aborts the install before anything is
unpacked. It runs unconditionally: Windows PowerShell 5.1 and PowerShell 7 both ship `Get-FileHash`,
so there is no "skipped without `sha256sum`" case as with the shell installer.

The script matches the anchor line literally and requires exactly one occurrence, and takes the
digest from the explicit `<sha256 file>`, the first field of dist's `<hex> *<name>` line, which must be one
line with a 64-hex digest naming `fontaine-x86_64-pc-windows-msvc.zip`; any other result (missing or empty
file, more than one line, malformed digest, another listed name) fails the run before signing, so a dist upgrade that changes the installer's text breaks `sign`, not
a user. The release installer is therefore signed after modification (the shell installer is signed
unmodified); the signature covers exactly the uploaded bytes.

### Dev channel

`dev-installer.sh` is generalised to the PowerShell text: its replacements are chosen by the
installer's extension (`.sh` today's four; `.ps1` the equivalents: the download base URL
occurrences, `$app_version = '...'`, the receipt JSON `version`, the usage text), each matched
literally with an expected count read from real output, failing on any other count. The version
validation stays the same.

`sign` now sets both installers aside in `dev-installer/`, rewrites each, runs
`windows-installer.sh` on the `.ps1`, and signs the directory with `release-sign.sh dev-installer`.
The `dev-installer` kind in `release-sign.sh` and `release-verify.sh` accepts exactly
`fontaine-installer.sh` and `fontaine-installer.ps1`, each with its `.minisig` (the `.sh` pair
mandatory as today, so a missing shell installer still fails; the `.ps1` pair mandatory too), each
signed under channel `dev` with `file=<its name>` in the trusted comment. `stable-installer`
verifies the directory, runs `sh -n` on the `.sh` and a parse-only check on the `.ps1`
(`pwsh -NoProfile -Command` with `[System.Management.Automation.Language.Parser]::ParseFile`, pwsh is
preinstalled on `ubuntu-22.04` runners), uploads all four files to `dev` with `--clobber`, and
byte-compares them from the stable URL. `host` also removes `fontaine-installer.ps1` from
`artifacts/` so `dev-<n>` and `verify-draft` (which still reject an installer on `dev`) are unchanged.

### Release channel

`release-sign.sh release` signs `fontaine-installer.ps1` when present, as it does the `.sh`.
`release-verify.sh` accepts `fontaine-installer.ps1` and its `.minisig` on `release` only, and
requires a valid `.minisig` for it when present (same trusted-comment check as the shell installer;
factor the existing installer block into a loop over both names rather than copying it). `sign` in
`release.yml` runs `windows-installer.sh` before signing and uploads the patched, signed
`fontaine-installer.ps1` as an artifact not named `artifacts-*` (the `dev.yml` precedent; a duplicate name
in the `artifacts-*` merge would be ambiguous). `host` downloads it after the `artifacts-*` fetch and
overwrites the stock copy in `artifacts/` (and in `rust/target/distrib/` if `dist host` needs it) before
`gh release create`.

### Verification parity

The installer verifies the archive's sha256, the checksum the updater compares and the one in
`sha256.sum`, both fetched from the same GitHub release. It does not verify the minisign signature (no
minisign on the user's machine) and does not verify itself: piped to `iex`, its trust is the HTTPS
connection that delivered it. Docs say so and point to `docs/verifying-releases.md` for manual
verification of the archive and the installer.

### Platforms

Windows x86_64 only (the one Windows target). No Windows arm64 build.

## Implementation Steps

1. `rust/Cargo.toml`: `installers = ["shell", "powershell"]`.
2. Install dist 0.32.0; `dist plan`, local then global build (`--artifacts=local`, then
   `--artifacts=global`; on Linux the Windows target cannot be built locally, so supply a fixture
   zip and `.sha256` in `target/distrib/` or inspect the `dist build --artifacts=global` output as
   far as it goes). Record the exact text of the anchor line, the download URL occurrences, `$app_version`,
   the receipt JSON and the usage text, and confirm `sha256.sum` leaves the installer out. Confirm the documented one-liner against the
   hint dist prints (the default Windows client policy, Restricted, makes a bare `irm | iex` throw).
3. Diff `dist generate` against `release.yml`; port installer-related changes only.
4. Add `.github/scripts/windows-installer-sha256.ps1` and `.github/scripts/windows-installer.sh`.
5. Generalise `.github/scripts/dev-installer.sh` to `.ps1`.
6. `release-sign.sh` / `release-verify.sh`: `.ps1` on `release`, and the `dev-installer` kind with both
   installers (loop, not copy); update their shell-only header comments (`release-sign.sh` lines 3, 6, 44;
   `release-verify.sh` lines 4-6) and `dev-installer.sh` lines 2-3. `release.yml` `sign`: run
   `windows-installer.sh` first and upload the patched, signed `.ps1` as a non-`artifacts-*` artifact;
   `host`: download it after the `artifacts-*` fetch and overwrite the stock copy; update the header
   comments (lines 24, 259).
7. `dev.yml`: `sign` sets aside, rewrites, patches and signs both; `host` removes the `.ps1`;
   `stable-installer` verifies, checks and uploads both; update the header comment.
8. CI "Release signing" step: self-tests (see Testing).
9. Docs, master plan Decisions, `CHANGELOG.md` (see Documentation).

## Files to Modify

- `rust/Cargo.toml`
- `.github/scripts/windows-installer.sh` (new), `.github/scripts/windows-installer-sha256.ps1` (new),
  `.github/scripts/dev-installer.sh`, `.github/scripts/release-sign.sh`, `.github/scripts/release-verify.sh`
- `.github/workflows/release.yml`, `.github/workflows/dev.yml`, `.github/workflows/ci.yml`
- `README.md`, `docs/installing.md`, `docs/verifying-releases.md`, `CONTRIBUTING.md`
- `tasks/design/platforms.md`, `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

## Trade-offs

- **Ship dist's installer as is (no sha256 check)**: rejected; the issue requires the installer to
  verify what the updater verifies, and the stock script verifies nothing.
- **Embed the check by forking dist's template**: not available (templates are compiled into dist);
  patching the generated text with exact-count anchors fails loudly on a dist bump, as `dev-installer.sh`
  already does.
- **Verify the minisign signature in the installer**: rejected, for the reasons in the shell
  installer plan (no minisign on the machine; key and script would arrive in the same fetch).
- **Separate Windows dev-installer script**: rejected; one rewrite script keyed by extension keeps the
  version validation and counting logic in one place.

## Decisions

- 2026-11-07: dist's PowerShell output is patched with exact-count anchors, so a dist upgrade that changes the text fails `sign`, not a user.
- 2026-11-07: the `.ps1` is signed after modification, so the signature covers exactly the uploaded bytes.
- 2026-11-07: one rewrite script (`dev-installer.sh`) keyed by file extension handles both installers.

## Documentation

- `README.md` install section: add the Windows command (`powershell -ExecutionPolicy Bypass -c "irm https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.ps1 | iex"`) alongside the
  shell one; fix lines ~30-32 ("Windows, and any platform without an installer, use the archive", "the dev release holds only the installer"); Windows prerequisites and caveats: **Git for Windows is required at runtime**, the
  installer ignores `[network]` proxy/CA/mTLS settings (set `HTTPS_PROXY` or install the CA in the
  Windows store, or download the archive manually), and manual-download users may see SmartScreen
  warnings (the archives are unsigned by Authenticode; verify with the minisign signature, see
  `docs/verifying-releases.md`).
- `docs/installing.md`: a Windows installer section: command, what it writes (`%USERPROFILE%\.cargo\bin\fontaine.exe`,
  the user `Path` registry edit unless `FONTAINE_NO_MODIFY_PATH=1`, the receipt at
  `%LOCALAPPDATA%\fontaine\fontaine-receipt.json`), PowerShell version, the sha256 check and its
  limits, SmartScreen, uninstall (binary, receipt, the `Path` entry; the cargo `bin` directory is shared
  with rustup), overwrite semantics with `cargo install`; remove "no Windows installer" and the Windows
  archive-only passage (line 3 and lines ~80-82); the command is `powershell -ExecutionPolicy Bypass -c "irm https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.ps1 | iex"`.
- `docs/verifying-releases.md`: the `.ps1` and its signature on both channels, with the same signing
  limit; the stable `dev` release now holds both installers; line 9 and the "Verify the installer by hand"
  section (add a `.ps1` variant).
- `CONTRIBUTING.md`: the "Enabling the dev channel" paragraph mentions both installers.
- `tasks/design/platforms.md` (installer sentence, README sentence, dev installer description) and
  master plan Decisions (Build and release): dated lines (PowerShell installer ships with a repo-patched
  sha256 check; the .ps1 is signed after modification; the `dev` release holds both installers); lines
  ~447, 449 and 453 ("signed unmodified", `.sh`-only dev release/signing) are amended or superseded.
- `CHANGELOG.md`: `Added` entry under `[Unreleased]` (#478).

## Testing Strategy

- `windows-installer-sha256.ps1` is a silent-failure surface (a bad compare skips verification); test it
  with `pwsh` in the Linux "Release signing" CI step (pwsh is preinstalled on ubuntu runners): dot-source
  the snippet, assert a correct digest passes (case-insensitive), and a wrong digest, a missing file
  and an empty expected value all throw. This is the shipped code, not a copy.
- `windows-installer.sh` self-tests with a fixture of the stock text from step 2: output contains the
  call after the anchor exactly once and the function; a fixture missing the anchor, with it twice, with
  a missing or empty sha256 file, more than one line, a malformed digest, or a listed name other than
  `fontaine-x86_64-pc-windows-msvc.zip` fails. The patched output parses under pwsh's parser, and the
  definition precedes the final `Install-Binary` call.
- `dev-installer.sh` `.ps1` self-tests in the existing style (all substitutions rewritten correctly;
  missing or duplicated text or a different URL count fails).
- `release-sign.sh` / `release-verify.sh`: `.ps1` signs and verifies on `release`; unsigned, tampered,
  renamed-signature and wrong trusted comments fail (extend `expect_rel_installer_fail` over both
  installers); a `.ps1` on `dev` fails; the `dev-installer` kind requires both installers and rejects
  extras and a tampered `.ps1`.
- CI's `dist plan` already validates the `Cargo.toml` change.

## Manual Verification

Needs a published release and Windows, so not automatable in PR CI:

1. On Windows x86_64, after the dev channel publishes: run the `powershell -ExecutionPolicy Bypass -c "irm https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.ps1 | iex"` command, open a new
   terminal, `fontaine --version` prints the version, and `%LOCALAPPDATA%\fontaine\fontaine-receipt.json`
   exists with `install_layout` `cargo-home` and, on the dev channel, a `version` containing `-dev.`.
2. Tamper check: download `fontaine-installer.ps1`, change the embedded digest, run it; expect a
   sha256 mismatch error and no `fontaine.exe` installed or replaced.
3. Install release N, update to N+1: needs #175's updater; this issue's "done when" half is
   exercised there, and this issue ships the installer and receipt it depends on.

## Open Questions

None.
