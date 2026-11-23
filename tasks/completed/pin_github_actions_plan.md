# Pin GitHub Actions to SHAs Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/760

## Overview

Every `uses:` in `.github/workflows/` points at a mutable tag, there is no `dependabot.yml`, and the
repo's Actions policy is `allowed_actions: all`, `sha_pinning_required: false`. A moved tag would
execute in CI and in `dev.yml` build jobs whose artifacts get signed. This pins every action to a full
commit SHA with a version comment, adds Dependabot to keep pins fresh, and documents the repo-settings
change (allowlist plus `sha_pinning_required`) that only the maintainer can apply.

## Current State

Third-party and first-party actions in use (count of `uses:` lines across all workflows):

- `actions/checkout` (`@v4` x5, `@v6` x16), `actions/upload-artifact` (`@v7` x16, `@v4` x1),
  `actions/download-artifact@v8` x16, `actions/setup-node@v4`, `actions/setup-python@v5`,
  `actions/deploy-pages@v5`, `actions/upload-pages-artifact@v5`
- `Swatinem/rust-cache@v2` x2, `taiki-e/install-action@v2` x3

Files with `uses:` lines: `android.yml`, `ci.yml`, `dev.yml`, `docs.yml`, `pg2.yml`, `release.yml` (`update-e2e.yml` has none).
`release.yml` is hand-adapted from dist's generated file (`allow-dirty = ["ci"]`), so editing it by hand
is the established practice. `dev.yml` and `release.yml` also install `dist` via a `curl | sh` of a
version-pinned GitHub release URL; that is not an action and is out of scope.
Repo policy (`gh api repos/madesroches/fontaine/actions/permissions`): `allowed_actions: all`,
`sha_pinning_required: false`.

## Design

1. **Pin by SHA.** Replace each `uses: owner/repo@vX` with `uses: owner/repo@<40-hex-sha> # vX.Y.Z`,
   where the SHA is the commit the tag currently resolves to (annotated tags dereferenced to the
   commit) and the comment names the full release version, so Dependabot can bump both. The same tag
   keeps the same SHA everywhere. Existing differing majors (`checkout@v4` vs `@v6`,
   `upload-artifact@v4` vs `@v7`) are kept as-is: pinning must not change behavior; aligning them is
   a separate Dependabot-driven bump.
2. **Dependabot.** Add `.github/dependabot.yml` (version 2) with the `github-actions` ecosystem at
   directory `/`, weekly, with a grouped update for minor/patch to limit PR noise. Add the `cargo`
   ecosystem at directory `/rust`, weekly, grouped minor/patch. rig crates are pinned to exact
   versions on purpose (AGENTS.md), so `ignore` the `rig-*` dependencies for cargo to keep Dependabot from
   proposing bumps that break that pin policy.
3. **Repo policy (maintainer step, not code).** Set `allowed_actions: selected` with
   `github_owned_allowed: true` and `patterns_allowed: ["Swatinem/rust-cache@*", "taiki-e/install-action@*"]`,
   and `sha_pinning_required: true`. This is a live-repo setting, so it is documented as an exact `gh api`
   command in the PR for the maintainer rather than run by the agent. It must be applied after the pinned
   workflows merge, since turning on `sha_pinning_required` first would fail every tag-referenced run.
4. **Guard against regression.** Add a CI step in `ci.yml` (Linux only) running a small script
   `.github/scripts/check-pinned-actions.sh` that fails when any `uses:` in `.github/workflows/*.yml`
   that is not a local (`./`) or `docker://` reference lacks a 40-hex SHA. It parses lines with a
   fixed `uses:` prefix per line; it does not touch tool-argument classification, so the no-regex-on-tool-arguments
   rule is unaffected. Once `sha_pinning_required` is on, GitHub enforces this itself, but the script gives a
   fast local failure and covers the window before the setting is flipped.

## Implementation Steps

1. Resolve each distinct `owner/repo@tag` to its commit SHA and full version with `gh api
   repos/<owner>/<repo>/git/ref/tags/<tag>` (dereference annotated tags via `git/tags/<sha>`); pick the
   latest release within the same major so the comment's version is exact.
2. Rewrite every `uses:` in the six workflow files per Design 1.
3. Add `.github/dependabot.yml`.
4. Add `.github/scripts/check-pinned-actions.sh` and a `ci.yml` step that runs it.
5. Update docs (below) and `CHANGELOG.md`.
6. Verify (Testing Strategy), then hand the repo-settings command to the maintainer in the PR body.

## Files to Modify

- `.github/workflows/{android,ci,dev,docs,pg2,release}.yml`
- `.github/dependabot.yml` (new)
- `.github/scripts/check-pinned-actions.sh` (new)
- `CONTRIBUTING.md` (short paragraph under "Build and test"), `CHANGELOG.md`

## Trade-offs

- Pinning to the current tag's SHA rather than upgrading to the latest: keeps the change behavior-neutral.
- A pin-check script versus relying only on `sha_pinning_required`: the setting is not in the repo's
  version control and may be off in forks, so the script keeps the invariant reviewable in-tree.
- Dependabot `cargo`: included because the issue says "if wanted" and cargo versions are otherwise never
  bumped; grouped and rig-excluded to stay compatible with the exact-pin policy.

## Decisions

## Documentation

`CONTRIBUTING.md`, under "Build and test": one short note that actions are SHA-pinned with a version comment, Dependabot bumps
them, and new actions must be added to the allowlist in repo settings. No `docs/` page covers CI
internals, so none changes.

## Testing Strategy

- `.github/scripts/check-pinned-actions.sh` run against the rewritten workflows passes; run against a
  scratch copy with one unpinned `uses:` it fails naming the file and line (checked once by hand in the
  scratch dir; the script is a few lines of shell, so no test harness is added).
- `actionlint`, if available locally, over the workflows; otherwise YAML parse check.
- The PR's own CI run is the end-to-end check: a wrong SHA fails at "Set up job". `ci.yml`, `docs.yml`,
  `android.yml`, `release.yml` (`plan` job) and `pg2.yml` (its temporary path-filtered PR trigger) run on
  the PR; only `dev.yml` does not, so its SHAs are verified once by hand with `gh api` against their
  commented tags (same-tag-same-SHA makes this a small check).

## Manual Verification

After merge, the maintainer runs (settings cannot be changed from a PR, and the call changes live repo policy):

```
gh api -X PUT repos/madesroches/fontaine/actions/permissions \
  -F enabled=true -f allowed_actions=selected -F sha_pinning_required=true
gh api -X PUT repos/madesroches/fontaine/actions/permissions/selected-actions \
  -F github_owned_allowed=true -f 'patterns_allowed[]=Swatinem/rust-cache@*' \
  -f 'patterns_allowed[]=taiki-e/install-action@*'
```

Expected: a following `gh api repos/madesroches/fontaine/actions/permissions` shows `selected` and
`sha_pinning_required: true`, and the next `CI` run passes.

## Open Questions

None.
