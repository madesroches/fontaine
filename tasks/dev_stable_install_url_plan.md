# Stable Dev Install URL Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/487

## Overview

The documented installer URL (`releases/latest/download/fontaine-installer.sh`) 404s because no tagged
release exists. The dev channel publishes `dev-<n>` prereleases with no installer, and `releases/latest`
skips prereleases anyway. This plan makes each successful dev publish also refresh one asset at a fixed
address, `https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.sh`: a dist
installer rewritten to fetch the archives of the immutable `dev-<n>` release it was built with.
The dev command is documented next to the release one.

## Current State

- `.github/workflows/dev.yml` builds, signs (`sign`, environment `dev-signing`), drafts (`host`),
  verifies (`verify-draft`) and publishes (`publish`) the prerelease `dev-<run_number>`. Its `plan`
  job is gated on `vars.FONTAINE_DEV_CHANNEL == 'enabled'`, which is unset.
- `build-global-artifacts` already makes `fontaine-installer.sh` (`installers = ["shell"]`), but
  `sign` and `host` delete it: dist bakes `.../releases/download/v<cargo version>` into it, which does
  not exist for a dev tag. `release-verify.sh` accepts the installer on `release` only.
- The dist installer writes the receipt (`fontaine-receipt.json`) with the version it was built for.
  `fontaine update` (#175) does not exist yet, so no code reads a receipt or knows about lanes.
- `docs/installing.md` and `README.md` document only the `releases/latest` command.
- All four key files in `rust/fontaine/keys/` are placeholders ("key not yet bootstrapped"). The `dev-signing` environment, its secrets and the
  repository variable are GitHub settings, not repo files.

## Design

### Stable asset, immutable archives

- `dev-<n>` stays exactly as today: immutable, signed, draft → verify → publish. It holds the
  archives and signatures and still has no installer.
- A single long-lived prerelease with tag `dev` holds one asset, `fontaine-installer.sh`, replaced on
  every run. The installer is the rewritten dist output: its download base is
  `https://github.com/madesroches/fontaine/releases/download/dev-<n>` and its embedded sha256s are
  those of `dev-<n>`'s archives (the same build, unchanged). It is unsigned and sha256-verified, like
  the release installer.
- The `dev` tag is created once by `gh release create` and never moved. Its `--target` commit is whatever the first
  run was; the notes say the tag only anchors the release.

### Rewriting the installer

A new script `.github/scripts/dev-installer.sh <installer> <tag> <version>` applies a fixed set of
named substitutions to dist's output, each matched by its surrounding context, and fails on any
mismatch:

1. download URL: every occurrence of `.../releases/download/v<cargo version>` → `.../releases/download/<tag>`.
   dist writes it several times (the GHE branch, the GitHub base-URL override branch, the default
   URL and the usage text; 4 in v0.32.0; the override occurrences carry no `https://github.com` prefix). The script requires the occurrence count confirmed in
   Step 1 and fails on any other count;
2. `APP_VERSION=` → the dev version `<base>-dev.<n>+<sha7>` that `plan` already computes;
3. the receipt JSON's top-level `"version"` (mandatory; not `provider.version`, which is the dist
   version and stays) → the same dev version. In dist v0.32.0 the receipt version is a separate
   literal from `APP_VERSION`;
4. the usage text "The installer for <app> <ver>" → the same dev version.

Substitutions 2-4 must each match exactly once. A receipt version with a `-dev.` prerelease is the dev
lane marker (see Decisions).

The exact text of every value is read from a real `dist build --artifacts=global` output during
implementation (dist is not installed here; install v0.32.0 per `release.yml`), not guessed.

### Workflow: new job `stable-installer` in `dev.yml`

`needs: [plan, publish]`, `permissions: contents: write`, no job-level `concurrency` (the workflow-level
`dev-channel` group already serializes runs). Like `verify-draft`, it checks out with `actions/checkout@v6` at `${{ env.HEAD_SHA }}` with
`persist-credentials: false`, sets `defaults.run.working-directory: ${{ github.workspace }}` and
`env.GH_TOKEN`. Steps: download the `artifacts-build-global` artifact; run `dev-installer.sh`; run `sh -n` on the
local rewritten file;
then `gh release view dev` or create it (`--prerelease --latest=false`, no `--draft`); `gh release
upload dev fontaine-installer.sh --clobber`; then re-download from the stable URL and byte-compare it
with the local file. A failed check before the upload leaves the previous stable installer in place; a
failed upload after gh's delete can leave none. The `sign`/`host` deletion of the installer stays, so
`dev-<n>` and `release-verify.sh` are unchanged.

The `plan` job's `FONTAINE_DEV_CHANNEL` gate is unchanged.

### Enabling the channel (manual, not in this PR)

An admin first bootstraps the dev key and commits `dev.pub` and `dev.signing.pub` (per
`CONTRIBUTING.md`'s Bootstrap procedure), then creates the `dev-signing` environment with `MINISIGN_SECRET_KEY` and `MINISIGN_PASSWORD`
(key material for `dev.signing.pub`), then sets the repository variable `FONTAINE_DEV_CHANNEL=enabled`.
The existing "Enabling the dev channel" paragraph of `CONTRIBUTING.md` is extended with the fact that each
run also replaces `fontaine-installer.sh` on the long-lived `dev` release; the issue
stays open until it is done and the acceptance check passes.

## Implementation Steps

1. Install dist v0.32.0 locally; run `dist build --artifacts=global` in `rust/` and record the
   exact text of every substitution in the installer, and the URL occurrence count.
2. Add `.github/scripts/dev-installer.sh` with the named substitutions.
3. Add the `stable-installer` job to `.github/workflows/dev.yml`; update the header comment.
4. Add self-tests for the script to the "Release signing" step in `.github/workflows/ci.yml`
   (see Testing).
5. Update `docs/installing.md`, `README.md`, `docs/verifying-releases.md`, `CONTRIBUTING.md`,
   `tasks/design/platforms.md`, `CHANGELOG.md` and the master plan's Decisions (see Documentation).

## Files to Modify

- `.github/scripts/dev-installer.sh` (new)
- `.github/workflows/dev.yml`, `.github/workflows/ci.yml`
- `docs/installing.md`, `README.md`, `docs/verifying-releases.md`, `CONTRIBUTING.md`
- `tasks/fontaine_harness_plan.md`, `tasks/design/platforms.md`, `CHANGELOG.md`

## Trade-offs

- **Moving `dev` release carrying archives** (the issue's option 1 as written): rejected; non-atomic
  uploads race users, and replacing archives under a published release defeats the
  draft → verify → publish gate.
- **Redirect/manifest the installer resolves** (option 2): needs a hosting place and a resolver in
  the shell script; the one-asset release is the same effect with no new infrastructure.
- **Re-point the `dev` tag each run**: needs a force-push of a tag, which the project forbids without
  an explicit instruction, and is unnecessary.
- **Sign the installer**: no minisign on the user's machine; same stance as the release installer.

## Decisions

- 2026-11-07: the `dev` tag is created once and never moved; only the release's assets change.
- 2026-11-07: the dev installer is unsigned and sha256-verified, like the release installer.
- 2026-11-07: the dev installer points at the immutable `dev-<n>` archives.
- 2026-11-07: supersedes the 2026-11-07 (#477) "No installer on the dev channel" decision in `tasks/fontaine_harness_plan.md`.
- 2026-11-07: the dev installer writes `<base>-dev.<n>+<sha7>` as the receipt version; channel detection is left to #175.

## Documentation

- `docs/installing.md`: lead with the dev command (the only one that works before the first release),
  show the release command labeled as available once the first tagged release is published; also fix
  the passage that the installer "downloads the matching archive from the release it belongs to" (the dev
  installer is hosted on `dev` but downloads from `dev-<n>`) and "Each GitHub Release has one archive per
  platform" (the `dev` release holds only the installer); state that
  the dev installer is unsigned, installs the latest CI-passing `main` build, and that its receipt
  version carries `-dev.`. Replace "attached to tagged releases, not to dev builds".
- `README.md` install section: the same pair of commands.
- `docs/verifying-releases.md`: say the stable `dev` release holds only the installer; archives and
  signatures live in `dev-<n>`.
- `CONTRIBUTING.md`: extend the existing "Enabling the dev channel" paragraph (under Signing keys and
  environments) with the fact that each run also replaces `fontaine-installer.sh` on the long-lived `dev`
  release.
- `tasks/design/platforms.md`: also update line ~34 (README lists "the installer": now two installer
  commands) and lines ~40-42 (dev channel described only as `dev-<run_number>`: mention the stable `dev`
  release), along with line ~31. Master plan: also update the workspace-layout line describing
  `.github/scripts/` ("minisign install, sign and verify scripts") to include the dev installer rewrite
  script. Both: replace "no dev-channel installer" with the stable-asset
  design; add dated Decisions (supersedes the 2026-11-07 "No installer on the dev channel" line).
- `CHANGELOG.md`: Added entry.

## Testing Strategy

- Unit-style self-tests in the CI "Release signing" step for `dev-installer.sh`, in the same style,
  with its own installer-text fixture and a fail helper that calls `dev-installer.sh`: a fixture
  containing all the substitutions (the URL at the Step 1 count, `APP_VERSION=`, the receipt
  `"version"`, the usage text) is rewritten correctly; a fixture missing any of them, containing a
  version value twice, or with a different URL count fails (the receipt one included). These catch the
  silent failure of a dist format change shipping an installer that points at a 404.
- `dev.yml` is only exercisable on `main`; the pre-upload syntax check and the
  post-upload byte-compare are its automated guard.

## Manual Verification

After an admin enables the channel and a run succeeds: on Linux x86_64 and macOS, run
`curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.sh | sh`,
then `fontaine --version`, which only confirms the binary runs (it prints the base `CARGO_PKG_VERSION`); the dev-lane assertion is that the receipt's `version` contains `-dev.`. Not automated because it
needs the real signed release and a real machine.

## Open Questions

None.
