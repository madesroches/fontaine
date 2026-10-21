# Platforms and Releases Design

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Platforms and directories

v1 supports **Linux, macOS and Windows**; CI runs all four steps on `ubuntu-latest`,
`macos-latest` and `windows-latest`. Windows follows Claude Code: the `shell` tool (and hook
commands) run through **Git Bash** — Git for Windows is a prerequisite — so the POSIX shell parser, Tier 0.5, rule matching and the critical-path
check apply unchanged. Discovery: locate `git.exe` on PATH (then the standard Git for Windows install
locations), derive `<Git>\usr\bin` and `<Git>\bin`, and prepend both to the shell launcher's PATH
(rig-tools `Launcher::with_env`) so rig's `sh` resolves; `FONTAINE_GIT_BASH_PATH` overrides discovery
(Claude Code's `CLAUDE_CODE_GIT_BASH_PATH`); not found ⇒ startup error on Windows (exit 2, message
naming Git for Windows and the override). Hook commands use the same discovery. A PowerShell tool is backlog (GitHub issue #28); it would never
pre-classify. Path handling (scope validator, protected paths, critical-path) accepts drive
letters, UNC paths, MSYS-style `/c/...` argv paths (translated to drive paths before validation; an
untranslatable one is out of scope), compares case-insensitively, and resolves junctions and
symlinks. Critical paths on Windows include drive roots and `%USERPROFILE%`. The containment probe
(Phase 4 step 5; Linux cgroup-namespace and `/.dockerenv` checks only) reports "not contained" on macOS and Windows; `--in-container` stays a manual assertion
that is never sufficient alone.

## Releases

semver 0.x, git tags `vX.Y.Z`. GitHub Releases carry prebuilt binaries for Linux, macOS and Windows
built by cargo-dist (exact-pinned version; workflow `.github/workflows/release.yml`, set up in Phase 4,
the first phase that produces a release); `cargo install` from git/crates.io also works. The
maintainer cuts releases — tagging and pushing the tag are human steps, never the agent's.
`CHANGELOG.md` follows Keep a Changelog: entries accumulate under `## [Unreleased]`, which moves under
the version heading at release. The README install section lists both install paths.

Directories come from the `dirs` crate: config dir = `dirs::config_dir()/fontaine` (Linux
`~/.config/fontaine`; macOS and Windows platform defaults, Windows under `%APPDATA%`), data dir =
`dirs::data_dir()/fontaine` (Linux `~/.local/share/fontaine`). `FONTAINE_CONFIG_DIR` and
`FONTAINE_DATA_DIR` override them and are what every test uses (temp dirs). Stored credentials live
at `<data dir>/credentials.toml`. **Owner-only** files — credentials, trust store, audit log,
session ledger, debug file — are mode 0600 on Unix and carry a current-user-only ACL on Windows.
