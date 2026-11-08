# Dev Build Version String Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/508

## Overview

A binary built from a work branch reports the same bare `CARGO_PKG_VERSION` as the release, so
bug reports and "which binary am I running" are ambiguous. Compute one display version in
`build.rs`: `0.1.0-dev+<sha12>` (plus `.dirty` when the tree has uncommitted changes) for dev
builds, plain `0.1.0` for release builds. Every place that shows the version reads that single
value.

## Current State

- `rust/fontaine/build.rs` emits `FONTAINE_GIT_SHA` (12-char short SHA, `unknown` without git)
  and `FONTAINE_BUILD_TARGET`, and re-runs on `HEAD`, `packed-refs` and the current branch ref.
- Display sites reading bare `CARGO_PKG_VERSION`:
  - `rust/fontaine/src/cli.rs:8` — clap `version` (`--version`).
  - `rust/fontaine/src/doctor_cmd.rs:60` — `fontaine doctor`.
  - `rust/fontaine/src/lib.rs:444-448` — `set_build_info(BuildInfo { version, git_sha, target })`.
  - `rust/fontaine-tui/src/ui/commands/bug.rs:258` — `/bug` (falls back to `CARGO_PKG_VERSION`
    when `BUILD_INFO` is unset).
  - `rust/fontaine-tui/src/ui/commands/reports.rs:17` — `/status`.
  - `rust/fontaine-core/src/bug_report.rs:131` — renders `{version} ({git_sha})`; with the SHA
    in the version it would print the SHA twice.
- Other `CARGO_PKG_VERSION` uses are protocol or identity strings in library crates, which have
  no `build.rs`: the web User-Agent (`fontaine-tools/src/lib.rs:38`), LSP `clientInfo`
  (`lsp/client.rs:505`), MCP `Implementation` (`fontaine-mcp/src/servers.rs:78`), the
  claude-subscription inventory `serverInfo`, and the statusline payload `version`
  (`build_statusline_payload` at `fontaine-tui/src/ui/mod.rs:185`, a Claude Code–compatible
  field; `fontaine-core/src/statusline.rs:422` is only a test).
  Telemetry `service.version` is in the bin crate (`fontaine/src/telemetry/otlp.rs:155`).
- The release signature's `version=` is minted by `release.yml` from the tag
  (`${GITHUB_REF_NAME#v}`), never from the binary, so it already keeps the released semver.
- `.github/workflows/dev.yml:88` mints `<base>-dev.<run_number>+<sha7>` as the dev channel's
  signature/receipt version, and `docs/installing.md` (lines ~17, 32, 89, 102) describes it.
- `release.yml`'s `build-local-artifacts` writes an untracked `rust/dist-manifest.json` before
  `build.rs` runs.
- `cargo-dist` builds the release archives from a checkout of the `v<version>` tag; the dev
  channel (`dev.yml`) builds `main` commits.

## Design

`build.rs` computes `FONTAINE_VERSION` and emits `cargo:rustc-env=FONTAINE_VERSION=...`.

Release signal, first match wins:
1. `FONTAINE_RELEASE_BUILD` env var set and non-empty → plain version (explicit escape hatch for
   packagers and any workflow that does not build from the tag).
2. No git information (no `.git`, e.g. a source tarball) → plain version; there is no SHA to add.
3. `v<CARGO_PKG_VERSION>` is among the tags listed by `git tag --points-at HEAD` (`describe`
   returns one tag only, and `dev.yml` also tags `main` commits `dev-<n>`) and the tree has no
   tracked changes → plain version. This is what the dist build sees, so `release.yml` needs
   no change.
4. Otherwise → `<version>-dev+<sha12>`, with `.dirty` appended to the build metadata
   (`0.1.0-dev+91550f07ab12.dirty`) when tracked files have changes. The dirty probe is
   `git --no-optional-locks status --porcelain --untracked-files=no`: untracked files are
   ignored (the release workflow writes `rust/dist-manifest.json` before `build.rs` runs), and
   `--no-optional-locks` keeps plain `status` from rewriting `.git/index`, which `build.rs`
   watches. The result is valid semver (build metadata allows `[0-9A-Za-z-.]`).

The decision is a pure function in `build.rs` of `(pkg_version, sha, exact_tag, dirty,
release_env)` so it is unit-testable. Since `build.rs` cannot be imported by tests, put the
function in a small module file (`rust/fontaine/build/version.rs`, `include!`d by `build.rs`
and `#[path]`-included by a `#[cfg(test)] mod` in the bin crate), keeping the logic in one place.

Single consumption point: `pub const VERSION: &str = env!("FONTAINE_VERSION")` in
`rust/fontaine/src/lib.rs`, used by:
- clap: `#[command(version = crate::VERSION)]` in `cli.rs`.
- `doctor_cmd.rs`.
- `BuildInfo.version` (so `/bug` shows it).
- telemetry `service.version`.

TUI crate (no `build.rs`): add `fontaine_tui::build_version() -> &'static str` returning
`BUILD_INFO.version` or `CARGO_PKG_VERSION` when unset (tests), delegating to a pure fallback
over `Option<&BuildInfo>`, and use it in `bug.rs` and `reports.rs` callers so `/status` and
`/bug` agree with `--version`. `bug_report.rs` prints the version without the appended
`(git_sha)`. The statusline payload in `ui/mod.rs:185` stays on `CARGO_PKG_VERSION`.

`build.rs` re-run triggers: also watch the git `index` file so `.dirty` flips after staging
(plain working-tree edits cannot be watched cheaply; see Decisions).

## Implementation Steps

1. `rust/fontaine/build.rs`: add the version computation (`describe`, `dirty` helpers using the
   existing `git()`), emit `FONTAINE_VERSION`, `rerun-if-env-changed=FONTAINE_RELEASE_BUILD`,
   watch `index`.
2. Extract the pure decision function and unit-test it (see Testing).
3. `rust/fontaine/src/lib.rs`: add `pub const VERSION`; use it in `set_build_info`.
4. `cli.rs`, `doctor_cmd.rs`, `telemetry/otlp.rs`: switch to `VERSION`.
5. `fontaine-tui`: add `build_version()`; switch `bug.rs`, `reports.rs` callers. `fontaine-core`:
   drop the appended `(git_sha)` in `bug_report.rs`.
6. Docs and changelog.

## Files to Modify

- `rust/fontaine/build.rs` (+ shared version module)
- `rust/fontaine/src/lib.rs`, `cli.rs`, `doctor_cmd.rs`, `telemetry/otlp.rs`
- `rust/fontaine-tui/src/lib.rs`, `ui/commands/bug.rs`, `ui/commands/reports.rs`
- `rust/fontaine-core/src/bug_report.rs`
- `docs/installing.md`, and `docs/doctor.md` if it shows version output
- `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Tag match vs. CI env var as the release signal**: the tag check needs no workflow edit and
  cannot drift from what dist builds; the env var is kept only as an override.

## Decisions

- Format `<version>-dev+<sha12>[.dirty]`; Claude Code documents no dev-build format, so this is
  Fontaine's own (recorded in the master plan's Decisions under Build and release).
- Release signature `version=` stays tag-derived and is untouched.
- Protocol identity strings (UA, LSP, MCP, claude-subscription inventory) and the Claude
  Code–compatible statusline `version` field keep the plain semver; library crates have no
  `build.rs`, and peers parse these strings as versions.
- `--version` (`-dev+<sha12>`) deliberately differs from the dev channel's receipt version
  (`-dev.<run_number>+<sha7>`, minted by `dev.yml`).
- `.dirty` is computed at build time only; edits to tracked files without rebuilding-triggering
  changes do not re-run `build.rs`. Accepted: a stale clean/dirty flag is better than
  re-probing git on every cargo invocation.

## Documentation

Update `docs/installing.md` to describe what `fontaine --version` prints for a dev install
(`-dev+<sha12>`, distinct from the receipt version); update `docs/doctor.md` if it shows example
version output; add a `CHANGELOG.md` entry under Unreleased.

## Testing Strategy

- Unit tests of the pure decision function: release env → plain; no git → plain; exact tag +
  clean → plain; tag + untracked file only → plain; exact tag + dirty → dev+sha.dirty; no tag →
  dev+sha; tag for a different version → dev+sha; result's build metadata is non-empty and
  matches `[0-9A-Za-z-.]` (no new dependency).
- Unit test in `fontaine-tui` of the pure fallback over `Option<&BuildInfo>` with constructed
  values: returns the info's version when `Some`, the package version when `None`. Tests never
  set the process-wide `BUILD_INFO` `OnceLock`.
- `rust/fontaine/tests/exit_code.rs` `help_and_version_exit_zero` only checks exit 0 and
  non-empty stdout, so it needs no change; add an assertion that `fontaine --version` output
  equals `fontaine doctor`'s reported version via the headless surface.

## Manual Verification

1. `cargo run -p fontaine -- --version` on a feature branch → `fontaine 0.1.0-dev+<sha12>`
   (`.dirty` after editing and staging a file). Why manual: needs a real git checkout and tag,
   which the unit tests model only as inputs.

## Open Questions

None.
