# Platforms and Releases Design

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Platforms and directories

v1 supports **Linux, macOS and Windows**; platform-specific compile, lint and test coverage runs on all
three OSes (`ubuntu-latest`, `macos-latest`, `windows-latest`); only pg2 clippy/test/rustdoc and the
fixture-eval steps are Linux-only. Windows follows Claude Code: the `shell` tool (and hook
commands) run through **Git Bash** when Git for Windows is found (preferred, not a prerequisite: see the fallback table below), so the POSIX shell parser, Tier 0.5, rule matching and the critical-path
check apply unchanged. Discovery: locate `git.exe` on PATH (then the standard Git for Windows install
locations), derive `<Git>\usr\bin` and `<Git>\bin`, and prepend both to the shell launcher's PATH
(rig-tools `Launcher::with_env`) so rig's `sh` resolves; `FONTAINE_GIT_BASH_PATH` overrides discovery
(Claude Code's `CLAUDE_CODE_GIT_BASH_PATH`); one that is not a Git Bash `bash.exe` is a startup error (exit 2). Hook commands use the same discovery.
Startup picks the Windows command shell from what it found:

| Git Bash | PowerShell | Result |
|---|---|---|
| found | any | `shell`, `monitor`, hooks, status line and `!` lines use Git Bash |
| not found | found | starts with one warning; PowerShell is the command shell (Claude Code: "If Git for Windows is not installed, Claude Code uses PowerShell as the shell tool instead") |
| not found | not found | exit 2: `no shell found: install Git for Windows, or set FONTAINE_GIT_BASH_PATH or FONTAINE_POWERSHELL_PATH` (a `FONTAINE_POWERSHELL_PATH` that is not a file is named instead of advised) |

 The command text reaches Git Bash's `sh` in the `FONTAINE_SH_COMMAND` environment variable, which `sh -c` evaluates, rather than on the Windows command line, where the MSYS runtime re-parses it and halves every `\\`; the command that was judged is then the one that runs. A separate Windows-only `powershell` tool (Claude name `PowerShell`) runs
commands in `pwsh.exe` (else `powershell.exe`, else System32's Windows PowerShell; `FONTAINE_POWERSHELL_PATH` overrides), started with
`-NoProfile -NonInteractive -OutputFormat Text -EncodedCommand`, so PowerShell parses exactly the judged text. It is registered only when an
interpreter is found (absence is not a startup error while Git Bash is found; with neither, startup fails), never pre-classifies (`Specifier::None`, outside the shell class), and
`floor::critical_powershell` extends the critical-path floor to `Remove-Item` and its aliases with a token-level scan whose unreadable
cases are critical. In the fallback `shell` and `monitor` are not registered (Claude Code: Monitor needs Git Bash on Windows), hook commands, the status line and `!` lines run in PowerShell through the same encoded invocation, the model-facing text names `powershell` where it would name `shell`, a subagent type that lists bare `shell` gets `powershell`, and skill `!` lines are reported as not run. Path handling (scope validator, protected paths, critical-path) accepts drive
letters, UNC paths, MSYS-style `/c/...` argv paths (translated to drive paths before validation; an
untranslatable one is out of scope), compares case-insensitively, and resolves junctions and
symlinks. Critical paths on Windows include drive roots and `%USERPROFILE%`. The containment probe
(Phase 4 step 6; Linux only, signals defined there) reports "not contained" on macOS and Windows; `--in-container` stays a manual assertion
that is never sufficient alone.

## Releases

semver 0.x, git tags `vX.Y.Z`. GitHub Releases carry prebuilt binaries for Linux, macOS and Windows
built by cargo-dist (exact-pinned version; workflow `.github/workflows/release.yml`, set up in Phase 4,
the first phase that produces a release); on Linux and macOS (Linux x86_64, macOS arm64 and x86_64) dist's `shell` installer, `fontaine-installer.sh`, is attached to tagged releases (signed with the release key, sha256-verified, receipt for the updater), and on Windows x86_64 dist's `powershell` installer, `fontaine-installer.ps1`, is attached too (patched in CI with a sha256 check of the archive, which dist's script lacks, then signed with the release key); the dev channel has rewritten, signed copies of both at the stable `dev` release (see below); `cargo install --git https://github.com/madesroches/fontaine fontaine` also works; v1 does not publish to crates.io. The
maintainer cuts releases — tagging and pushing the tag are human steps, never the agent's.
`CHANGELOG.md` follows Keep a Changelog: entries accumulate under `## [Unreleased]`, which moves under
the version heading at release. The README install section lists the installers (the dev installers until the first tagged release), the manual archive and `cargo install --git`.

Archives carry the root `LICENSE`, `README.md` and `CHANGELOG.md` through `include`. The release
workflow stages the changelog into `rust/` so dist builds the release notes from the version's
section. A tag without that section fails in the `plan` job.

Every archive is signed. Fontaine owns one minisign key per channel: `release` (tagged releases)
and `dev` (the latest CI-passing build of `main`, published by `.github/workflows/dev.yml` as the
prerelease `dev-<run_number>` once the repository variable `FONTAINE_DEV_CHANNEL` is `enabled`; the long-lived prerelease `dev` holds only the installers and their signatures, see below).
The public keys are in `rust/fontaine/keys/` and compiled into the verifier module Fontaine's update path will use (no self-update yet); the private keys exist
only as secrets of the `release-signing` (tags, required reviewer) and `dev-signing` (`main`)
GitHub Environments, which only the `sign` jobs name. The signed trusted comment binds channel,
asset name, version and commit. Publication is draft, verify, publish: `sign` signs after the
build jobs, `host` creates a draft release, `verify-draft` downloads it and checks it with
`.github/scripts/release-verify.sh`, and only then does `publish` make it public, so a failure
leaves nothing public. The `fontaine` crate's `release_signature` module is the one Rust verifier
(for the updater and `fontaine update`); CI checks the archives with the upstream minisign CLI,
pinned and checksum-verified, and an ignored test proves the two agree on the CLI's output.
Key bootstrap, rotation and recovery are in `CONTRIBUTING.md`.

The dev channel has one stable address per installer, `releases/download/dev/fontaine-installer.sh`
and `.../fontaine-installer.ps1`, assets of the long-lived prerelease `dev` (tag created once, never
moved). `dev-<n>` is immutable and has no installer. In `sign`, `.github/scripts/dev-installer.sh`
rewrites each of dist's installers (the mode follows the extension) to fetch the archives of its own
`dev-<n>` and to carry the dev version (`<base>-dev.<n>+<sha7>`) in `APP_VERSION` / `$app_version`, the
receipt and the usage text, failing on any text it does not recognise. The PowerShell installer then
goes through `.github/scripts/windows-installer.sh`, which inserts `Assert-ArchiveSha256`
(`.github/scripts/windows-installer-sha256.ps1`) and a call to it after the archive download, with
the digest from the archive's `.sha256` file; the anchors are matched with exact counts. The
results are signed with the dev key (`release-sign.sh dev-installer`, which requires both) and uploaded as the
`dev-installer` artifact, which `host` and `verify-draft` never see. After `publish`, the
`stable-installer` job verifies them, parses the `.ps1`, skips unless its `dev-<n>` is the highest
published, replaces the four assets and byte-compares what users would download. On the release
channel `sign` runs the same patch before signing and uploads the patched `.ps1` as the
`windows-installer` artifact, which `host` copies over dist's stock one before creating the draft.

Directories come from the `dirs` crate: config dir = `dirs::config_dir()/fontaine` (Linux
`~/.config/fontaine`; macOS and Windows platform defaults, Windows under `%APPDATA%`), data dir =
`dirs::data_dir()/fontaine` (Linux `~/.local/share/fontaine`). `FONTAINE_CONFIG_DIR` and
`FONTAINE_DATA_DIR` override them and are what every test uses (temp dirs). Stored credentials live
at `<data dir>/credentials.toml`. **Owner-only** files — credentials, trust store, audit log,
session ledger, debug file — are mode 0600 on Unix and carry a current-user-only ACL on Windows.
