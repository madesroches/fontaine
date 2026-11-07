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
- Keys exist in `rust/fontaine/keys/` (#425 closed). The `dev-signing` environment, its secrets and the
  repository variable are GitHub settings, not repo files.

## Design

### Stable asset, immutable archives

A moving release that carried the archives would make a user's download race an upload (new
installer with old archives, so a sha mismatch). Instead:

- `dev-<n>` stays exactly as today: immutable, signed, draft → verify → publish. It holds the
  archives and signatures and still has no installer.
- A single long-lived prerelease with tag `dev` holds one asset, `fontaine-installer.sh`, replaced on
  every run. The installer is the rewritten dist output: its download base is
  `https://github.com/madesroches/fontaine/releases/download/dev-<n>` and its embedded sha256s are
  those of `dev-<n>`'s archives (the same build, unchanged). It is unsigned and sha256-verified, like
  the release installer.
- The `dev` tag is created once by `gh release create` and never moved or force-pushed: only the
  release's assets change, so no tag-moving rule applies. Its `--target` commit is whatever the first
  run was; the notes say the tag only anchors the release.

### Rewriting the installer

A new script `.github/scripts/dev-installer.sh <installer> <tag> <version>` rewrites two baked values
in dist's output and fails if either substitution does not match exactly once:

1. the download URL `.../releases/download/v<cargo version>` → `.../releases/download/<tag>`;
2. the app version (`APP_VERSION` and therefore the receipt's `version`) → the dev version
   `<base>-dev.<n>+<sha7>` that `plan` already computes. A receipt version with a `-dev.` prerelease
   is how #175 recognises the dev lane (see Decisions).

The exact text of both values is read from a real `dist build --artifacts=global` output during
implementation (dist is not installed here; install v0.32.0 per `release.yml`), not guessed.

### Workflow: new job `stable-installer` in `dev.yml`

`needs: [plan, publish]`, `permissions: contents: write`, same `concurrency` group already serializing
runs. Steps: download the `artifacts-build-global` artifact; run `dev-installer.sh`; `gh release view
dev` or create it (`--prerelease --latest=false`, no `--draft`); `gh release upload dev
fontaine-installer.sh --clobber`; then re-download from the stable URL and check it with `sh -n`, that
it names `dev-<n>`, and that it differs from nothing but those two values (`diff` against the pre-rewrite
file shows only the changed lines). A failure is a red job, which leaves the previous stable installer
in place. The `sign`/`host` deletion of the installer stays, so `dev-<n>` and `release-verify.sh` are
unchanged.

The `plan` job's `FONTAINE_DEV_CHANNEL` gate is unchanged.

### Enabling the channel (manual, not in this PR)

An admin creates the `dev-signing` environment with `MINISIGN_SECRET_KEY` and `MINISIGN_PASSWORD`
(key material for `dev.signing.pub`), then sets the repository variable `FONTAINE_DEV_CHANNEL=enabled`.
`CONTRIBUTING.md` §Releasing is updated to include the stable installer and this checklist; the issue
stays open until it is done and the acceptance check passes.

## Implementation Steps

1. Install dist v0.32.0 locally; run `dist build --artifacts=global` in `rust/` and record the
   exact baked-URL and version lines in the installer.
2. Add `.github/scripts/dev-installer.sh` with the two exact-once rewrites.
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

## Documentation

- `docs/installing.md`: lead with the dev command (the only one that works before the first release),
  show the release command labeled as available once the first tagged release is published; state that
  the dev installer is unsigned, installs the latest CI-passing `main` build, and that its receipt
  version carries `-dev.`. Replace "attached to tagged releases, not to dev builds".
- `README.md` install section: the same pair of commands.
- `docs/verifying-releases.md`: say the stable `dev` release holds only the installer; archives and
  signatures live in `dev-<n>`.
- `CONTRIBUTING.md`: enabling checklist.
- `tasks/design/platforms.md`, master plan: replace "no dev-channel installer" with the stable-asset
  design; add dated Decisions (supersedes the 2026-11-07 "No installer on the dev channel" line).
- `CHANGELOG.md`: Added entry.

## Testing Strategy

- Unit-style self-tests in the CI "Release signing" step for `dev-installer.sh`, matching its existing
  `make_fixture`/`expect_fail` style: a fixture installer containing both baked values is rewritten
  correctly; a fixture missing either value, or containing one twice, fails. These catch the silent
  failure of a dist format change shipping an installer that points at a 404.
- `dev.yml` is only exercisable on `main`; the in-workflow post-upload check (syntax, tag, diff) is
  its automated guard.

## Manual Verification

After an admin enables the channel and a run succeeds: on Linux x86_64 and macOS, run
`curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.sh | sh`,
then `fontaine --version` and check the receipt's `version` contains `-dev.`. Not automated because it
needs the real signed release and a real machine.

## Open Questions

- Should the receipt carry an explicit lane field instead of the `-dev.` version convention? dist's
  receipt has no such field and `fontaine update` (#175) does not exist yet, so it is left to #175.
- Dev-signing key material and the repo variable are a maintainer action (item 1 of the issue).
