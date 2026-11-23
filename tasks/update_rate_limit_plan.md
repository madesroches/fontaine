# Update Rate-Limit Resilience Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/755

## Overview
`fontaine update` on the dev channel spends several unauthenticated GitHub API calls (60/hour per IP) scanning release pages and can fail with a bare `403 Forbidden`. This plan bypasses the API on the dev channel: the dev workflow publishes a small `latest-dev.json` pointer on the stable `dev` release, and the updater reads it from `github.com/.../releases/download/dev/`, which is not API rate limited. The API scan remains as a fallback, made cheaper and made to report rate limits clearly.

## Current State
`latest()` in `rust/fontaine/src/update/release_index.rs` walks `/releases?per_page=100&page=N` for up to `MAX_PAGES = 5` pages and keeps the highest `dev-N`, stopping only on a short page. `get()` sends `Accept: application/vnd.github+json` and turns any non-success status into `IndexError::Status(status, url)`, displayed as `"{url} answered {status}"`. `latest()` is called once, in `update/mod.rs` (~line 387), and `Release.assets` is looked up by name there (`release.asset(&name)`, `release.asset("<name>.minisig")`). In `.github/workflows/dev.yml` the `stable-installer` job (after `publish`) maintains the long-lived `dev` prerelease, uploads installers with `--clobber`, and is already guarded so it never moves to an older build than the newest published `dev-<n>`.

## Design
1. **Pointer asset (primary path).** In `stable-installer`, after the guard and the installer upload, generate `latest-dev.json` from the published `dev-<n>` release and upload it to `dev` with `--clobber`:
   `{"tag":"dev-N","assets":["<asset name>", ...]}`, built with `gh release view "$DEV_TAG" --json assets --jq ...`. The existing "Check what users download" step also fetches it and checks the tag matches. The pointer is unsigned and needs no signing: the updater still verifies every archive's minisign signature and the signed version, so a tampered pointer can at worst steer to a different published build, and the existing newer-than-installed comparison still applies.
2. **Client.** `latest()` for `Channel::Dev` first fetches `{github_base}/{REPO}/releases/download/dev/latest-dev.json` (a new `DEFAULT_DOWNLOAD_BASE` constant, a parameter alongside `api_base` so tests can mock it), parses it, validates `tag_build(Dev, tag)` and builds a `Release` whose asset URLs are `{download_base}/{REPO}/releases/download/{tag}/{name}` (an asset name must be a plain file name, no `/`). Any failure of this path (404, network error, parse error, bad tag) falls through to the API scan; the pointer fetch uses its own plain `get` without the GitHub `Accept` header.
3. **API fallback, cheaper.** Stop the page scan after the first page that contains a non-draft `dev-N` build (the list is newest-first), instead of scanning up to `MAX_PAGES`.
4. **Clear rate-limit error.** Add `IndexError::RateLimited { url, reset }`. In the API `get()`, a 429, or a 403 with `x-ratelimit-remaining: 0`, becomes `RateLimited`, parsing `x-ratelimit-reset` (epoch seconds). Display: `GitHub API rate limit reached for <url>; resets in N minutes` (a pure `describe_reset(reset_epoch, now)` helper, omitted when the header is missing). Other 403s keep the `Status` message. `index_error()` maps it to `UpdateError::Network`.

## Implementation Steps
1. `.github/workflows/dev.yml` (`stable-installer`): generate and upload `latest-dev.json`; extend the verification loop; update the header comment that lists what `dev` holds.
2. `update/release_index.rs`: pointer fetch + parse + `Release` construction, fallback to the scan, early page break, `RateLimited`, `describe_reset`; add the download-base parameter.
3. `update/mod.rs` (`Params`, call site, ~line 659 default params) and `update/auto.rs` (~line 577): thread the download base.
4. Docs: `docs/verifying-releases.md` (the `dev` release now also holds `latest-dev.json`, unsigned and why that is safe) and a note in `docs/installing.md` if it describes how updates find builds.

## Files to Modify
- `.github/workflows/dev.yml`
- `rust/fontaine/src/update/release_index.rs`
- `rust/fontaine/src/update/mod.rs`
- `rust/fontaine/src/update/auto.rs`
- `docs/verifying-releases.md`

## Trade-offs
- An unsigned pointer was chosen over a signed one: signing needs the `dev-signing` environment and a new signing kind, while every consumed archive is already signature-checked.
- An optional `GITHUB_TOKEN` was dropped: with the pointer, the API is only a fallback, so authentication is not worth the extra surface (and token-leak considerations).

## Decisions
- The API fallback is kept so the first dev build after deploy, or a missing/clobbered pointer (`--clobber` briefly removes the asset), still updates.

## Documentation
`docs/verifying-releases.md`, as listed in Implementation Steps.

## Testing Strategy
wiremock tests in `release_index.rs` (wiremock is a dev-dependency):
- Pointer served → returns the release from the pointer with correct asset URLs and makes zero API requests (API mock expects 0 calls). Pins the reported bug: no API call, so no 403.
- Pointer 404 / malformed / bad tag / asset name containing `/` → falls back to the API scan.
- Fallback: page 1 full with a dev build and page 2 failing the test if hit → exactly one request; page 1 without dev builds continues to page 2.
- 403 with `x-ratelimit-remaining: 0` and reset header, and 429 → `RateLimited` with reset text; plain 403 stays `Status`.
- `describe_reset` pure tests (future, past, missing).

Workflow change is verified by the step's own byte/tag check in CI on the next dev run (it runs only on `main`, so it cannot be exercised pre-merge).

## Manual Verification
After the first `main` build following merge: `curl -fsSL https://github.com/madesroches/fontaine/releases/download/dev/latest-dev.json` returns the latest `dev-N`, then `fontaine update` on the dev channel succeeds. Not automatable pre-merge because the dev workflow only runs on `main`.

## Open Questions
None.
