# Stable Dev Install URL Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/487

## Overview

The documented installer URL (`releases/latest/download/fontaine-installer.sh`) 404s because no tagged
release exists. The dev channel publishes `dev-<n>` prereleases with no installer, and `releases/latest`
skips prereleases anyway. This plan makes each successful dev publish also refresh one asset at a fixed
address, `https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.sh`: a dist
installer rewritten to fetch the archives of the immutable `dev-<n>` release it was built with.
Until the first tagged release, only the dev command is documented.

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
- A single long-lived prerelease with tag `dev` holds two assets, `fontaine-installer.sh` and
  `fontaine-installer.sh.minisig`, replaced on every run. The installer is the rewritten dist output:
  its download base is `https://github.com/madesroches/fontaine/releases/download/dev-<n>` and its
  embedded sha256s are those of `dev-<n>`'s archives (the same build, unchanged). It checks each downloaded archive
  against its embedded sha256, and is itself signed with the dev key (see Signing the dev installer).
- The `dev` tag is created once by `gh release create` and never moved. Its `--target` commit is whatever the first
  run was; the notes say the tag only anchors the release.

### Rewriting the installer

A new script `.github/scripts/dev-installer.sh <installer> <tag> <version>`, run by the `sign` job,
applies a fixed set of named substitutions to dist's output, each matched by its surrounding context, and fails on any
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

### Signing the dev installer

In `sign` (the only job with the dev key), instead of deleting the global installer: move it from
`artifacts/` to a separate directory `dev-installer/`, run `dev-installer.sh` on it (tag and version
from the `plan` outputs; `sign` adds `plan.outputs.tag` to its use), then sign the rewritten file with
`release-sign.sh dev-installer dev-installer rust/fontaine/keys/dev.signing.pub "$SIGN_VERSION" "$SIGN_COMMIT"`.
The signature therefore covers exactly the uploaded bytes. The trusted comment is
`fontaine-signature v1 channel=dev file=fontaine-installer.sh version=<v> commit=<sha>`.

`release-sign.sh` and `release-verify.sh` gain a third first-argument kind, `dev-installer`: the
directory must hold exactly `fontaine-installer.sh` and its `.minisig`, signed under channel `dev`.
The `release` and `dev` kinds are unchanged, including the "unrecognised asset" failure on `dev` for
an installer in a draft directory, so `dev-<n>` still has no installer (`host` keeps removing it from
`artifacts/`).

The directory is uploaded as artifact `dev-installer`, a name that does not match `artifacts-*`, so
`host` and `verify-draft` never pick it up.

### Workflow: new job `stable-installer` in `dev.yml`

`needs: [plan, publish]`, `permissions: contents: write`, no job-level `concurrency` (the workflow-level
`dev-channel` group already serializes runs). Like `verify-draft`, it checks out with `actions/checkout@v6` at `${{ env.HEAD_SHA }}` with
`persist-credentials: false` (for `release-verify.sh` and the public key only), sets
`defaults.run.working-directory: ${{ github.workspace }}` and `env.GH_TOKEN`. Steps: install minisign;
download the `dev-installer` artifact with `path: artifacts` (as `sign` and `host` do); run
`release-verify.sh dev-installer artifacts rust/fontaine/keys/dev.signing.pub` with the `plan`
version and commit before any upload; run `sh -n artifacts/fontaine-installer.sh`;
then the regression guard: list published (non-draft) releases with
`gh release list --json tagName,isDraft --limit 1000 --repo "$GITHUB_REPOSITORY"`, take the highest `<n>` among
`dev-<n>` tags, and if it exceeds this run's `${{ needs.plan.outputs.tag }}` number skip the remaining steps
with a notice (a fetch failure fails the job); then `gh release view dev` or create it (`--prerelease --latest=false`, no `--draft`, with
`--target "$HEAD_SHA" --repo "$GITHUB_REPOSITORY"`); `gh release upload dev artifacts/fontaine-installer.sh
artifacts/fontaine-installer.sh.minisig --clobber --repo "$GITHUB_REPOSITORY"`; then re-download both
from the stable URL and byte-compare them with the local files. A failed check before the upload
leaves the previous stable installer in place. A skipped older run leaves the newer installer. Accepted risk: `--clobber` deletes the old asset before
uploading, so the stable URL can 404 briefly, and a failed upload after the delete can leave no installer
(the byte-compare fails the job and the next run restores it).

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
   exact text of every substitution in the installer, and the URL occurrence count. A bare local
   global-only build has no checksum lines (CI downloads the local manifests first), so do not copy
   checksum lines from it into the fixture.
2. Add `.github/scripts/dev-installer.sh` with the named substitutions.
3. Add the `dev-installer` kind to `release-sign.sh` and `release-verify.sh`.
4. In `dev.yml`, change `sign` (rewrite, sign, upload `dev-installer`), add the `stable-installer`
   job (with the highest-published-`dev-<n>` guard) and update the header comment.
5. Add self-tests for the scripts to the "Release signing" step in `.github/workflows/ci.yml`
   (see Testing).
6. Update `docs/installing.md`, `README.md`, `docs/verifying-releases.md`, `CONTRIBUTING.md`,
   `tasks/design/platforms.md`, `CHANGELOG.md` and the master plan's Decisions (see Documentation).

## Files to Modify

- `.github/scripts/dev-installer.sh` (new), `.github/scripts/release-sign.sh`, `.github/scripts/release-verify.sh`
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
- **Sign the installer**: signed on the dev channel only. Self-verification by the installer was
  rejected: the embedded key and sha would arrive in the same HTTPS fetch as the script, so it adds no
  trust. The release installer was initially left unsigned; it is signed too (see the platforms design doc).

## Decisions

- 2026-11-07: the `dev` tag is created once and never moved; only the release's assets change.
- 2026-11-07: the dev installer is signed with the dev key (trusted comment binds file, version and commit), uploaded beside the installer on `dev`; the release installer was later signed too.
- 2026-11-07: documentation shows only the dev command until the first tagged release, per #487 Ask 3.
- 2026-11-07: the dev installer points at the immutable `dev-<n>` archives.
- 2026-11-07: supersedes the 2026-11-07 (#477) "No installer on the dev channel" decision in `tasks/fontaine_harness_plan.md`.
- 2026-11-07: AFK: `gh release upload --clobber` kept; the brief 404 and a failed-upload-after-delete window are accepted and recorded, detected by the post-upload byte-compare and healed by the next run.
- 2026-11-07: AFK: `stable-installer` skips its upload unless its `dev-<n>` is the highest published (non-draft) `dev-*` release, so out-of-order runs and re-runs of older runs cannot regress the stable installer.
- 2026-11-07: the dev installer writes `<base>-dev.<n>+<sha7>` as the receipt version; channel detection is left to #175.

## Documentation

- `docs/installing.md`: document only the dev command until the first tagged release (drop the
  release command, and any text implying it works today, such as "attached to tagged releases"); also fix
  the passage that the installer "downloads the matching archive from the release it belongs to" (the dev
  installer is hosted on `dev` but downloads from `dev-<n>`) and "Each GitHub Release has one archive per
  platform" (the `dev` release holds only the installer); state that
  the dev installer installs the latest CI-passing `main` build and that its receipt version carries
  `-dev.`. State the signing limit: the installer does not check its own signature (piped to `sh`,
  nothing on the user's machine verifies); the `.minisig` lets someone who downloads the installer
  verify it by hand with minisign, and its trusted comment binds file, version and commit. The release
  installer is unsigned. Also replace the "It does not verify ... is itself unsigned" passage accordingly.
- `README.md` install section: the dev command only; reword line 16 ("one command installs the
  latest release") to say it installs the latest dev build; also reword lines ~30-34 ("use the archive attached to each GitHub Release",
  "Each archive has a detached signature"): until a tagged release exists the archives are on the
  `dev-<n>` prereleases and the `dev` release holds only the installer.
- The dev URL works once the channel is enabled (the manual step following `CONTRIBUTING.md`); the
  issue stays open until then.
- `docs/verifying-releases.md`: say the stable `dev` release holds only the installer and its
  signature (with `minisign -Vm` instructions and the signing limit above, by reference to
  `installing.md`); archives and their signatures live in `dev-<n>`. Also amend line ~60 ("The shell
  installer (`fontaine-installer.sh`) is not signed and does not check the minisign signature"): the
  dev installer is signed, the release installer is not; and widen lines ~38-41 (trusted comment "binds
  the signature to one archive", `file=<archive name>`) to cover the installer.
- `CONTRIBUTING.md`: extend the existing "Enabling the dev channel" paragraph (under Signing keys and
  environments) with the fact that each run also replaces `fontaine-installer.sh` on the long-lived `dev`
  release.
- `tasks/design/platforms.md`: also update line ~34 (README lists "the installer": say it is the dev
  installer until the first tagged release) and lines ~40-42 (dev channel described only as `dev-<run_number>`: mention the stable `dev`
  release), along with line ~31. Master plan: also update the workspace-layout line describing
  `.github/scripts/` ("minisign install, sign and verify scripts") to include the dev installer rewrite
  script. Both: replace "no dev-channel installer" with the stable-asset
  design; add dated Decisions (supersedes the 2026-11-07 "No installer on the dev channel" line).
- `CHANGELOG.md`: Added entry (the stable dev installer URL and its signature).

## Testing Strategy

- Unit-style self-tests in the CI "Release signing" step for `dev-installer.sh`, in the same style,
  with its own installer-text fixture and a fail helper that calls `dev-installer.sh`: a fixture
  containing all the substitutions (the URL at the Step 1 count, `APP_VERSION=`, the receipt
  `"version"`, the usage text) is rewritten correctly; a fixture missing any of them, containing a
  version value twice, or with a different URL count fails (the receipt one included). The tests
  prove the script rejects unexpected text; after a dist version bump changes the format, the
  `dev.yml` run fails in `sign`, before any upload.
- Self-tests for the `dev-installer` kind: an installer signs and verifies; a tampered installer or a
  wrong file name fails; the existing "installer rejected on dev" test in `ci.yml` stays unchanged.
- Guard: the release-number comparison is kept in a small shell step, checked by inspection and in the
  first real runs (an older tag skips, the newest uploads); re-running an older run's `stable-installer`
  must skip while a newer `dev-<n>` is published.
- `dev.yml` is only exercisable on `main`; the pre-upload signature verification and syntax check and
  the post-upload byte-compare are its automated guard.

## Manual Verification

After an admin enables the channel and a run succeeds: on Linux x86_64 and macOS, run
`curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.sh | sh`,
then `fontaine --version`, which only confirms the binary runs (it prints the base `CARGO_PKG_VERSION`); the dev-lane assertion is that the receipt's `version` contains `-dev.`. Also download `fontaine-installer.sh` and `fontaine-installer.sh.minisig` from the `dev` release and run
`minisign -Vm fontaine-installer.sh -p rust/fontaine/keys/dev.signing.pub`. Not automated because it
needs the real signed release and a real machine.

## Open Questions

None.
