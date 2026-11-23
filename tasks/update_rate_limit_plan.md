# Update Rate-Limit Resilience Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/755

## Overview
`fontaine update` on the dev channel spends several unauthenticated GitHub API calls (60/hour per IP) scanning release pages and can fail with a bare `403 Forbidden`. This plan bypasses the API on the dev channel: the dev workflow publishes a small `latest-dev.json` pointer on the stable `dev` release, and the updater reads it from `github.com/.../releases/download/dev/`, which is not API rate limited. The paged API scan is removed.

## Current State
`latest()` in `rust/fontaine/src/update/release_index.rs` walks `/releases?per_page=100&page=N` for up to `MAX_PAGES = 5` pages and keeps the highest `dev-N`. `get()` sends `Accept: application/vnd.github+json` and turns any non-success status into `IndexError::Status(status, url)`. `latest()` is called once, in `update/mod.rs` (~line 387), and `Release.assets` is looked up by name there (`release.asset(&name)`, `release.asset("<name>.minisig")`). In `.github/workflows/dev.yml` the `stable-installer` job (after `publish`) maintains the long-lived `dev` prerelease, uploads installers with `--clobber`, and is already guarded so it never moves to an older build than the newest published `dev-<n>`. The release channel uses `/releases/latest` (one API call) and is unchanged.

## Design
1. **Pointer asset.** In `stable-installer`, after the guard and the installer upload, generate `latest-dev.json` from the published `dev-<n>` release and upload it to `dev` with `--clobber`:
   `{"tag":"dev-N","assets":["<asset name>", ...]}`, built with `gh release view "$DEV_TAG" --json assets --jq ...`. The existing "Check what users download" step also fetches it and checks the tag matches. The pointer is unsigned: the updater still verifies every archive's minisign signature and signed version, so a tampered pointer can at worst steer to a different published build, and the newer-than-installed comparison still applies.
2. **Client.** `latest()` for `Channel::Dev` fetches `{download_base}/{REPO}/releases/download/dev/latest-dev.json` (new `DEFAULT_DOWNLOAD_BASE = "https://github.com"`, a parameter alongside `api_base` so tests can mock it), parses it, validates `tag_build(Dev, tag)`, and builds a `Release` whose asset URLs are `{download_base}/{REPO}/releases/download/{tag}/{name}`. Asset names containing `/` are rejected. The pointer fetch is a plain GET without the GitHub `Accept` header.
3. **Errors.** There is no fallback. A failed or invalid pointer fetch is an `IndexError` mapped to `UpdateError::Network`; the message says the dev release pointer could not be fetched and to retry (a pointer upload with `--clobber` briefly 404s). The `/releases` paging code, `PER_PAGE` and `MAX_PAGES` are deleted.

## Implementation Steps
1. `.github/workflows/dev.yml` (`stable-installer`): generate and upload `latest-dev.json`; extend the verification loop; update the header comment listing what `dev` holds.
2. `update/release_index.rs`: replace the dev branch of `latest()` with the pointer fetch; delete the page scan and its constants; add the download-base parameter.
3. `update/mod.rs` (`Params`, call site, ~line 659 default params) and `update/auto.rs` (~line 577): thread the download base.
4. Docs: `docs/verifying-releases.md` (the `dev` release also holds `latest-dev.json`, unsigned, and why that is safe); `docs/installing.md` (dev installs the build named by the `dev` release's `latest-dev.json`, a missing pointer is a network error, exit 4; the `dev` release also holds `latest-dev.json`; exit-code table "no published release" no longer covers a missing dev pointer).
5. `rust/fontaine/tests/update_cmd.rs` and `tests/update_auto.rs`: set `download_base: &server.uri()` in every `Params`; change `serve()` to mount `/{REPO}/releases/download/dev/latest-dev.json` and assets at `/{REPO}/releases/download/{tag}/{name}` instead of `{REPO}/releases` and `/dl/{tag}/...`; replace or drop the "highest of several dev tags" tests (dev-5/dev-4, `for newest in ["dev-3","dev-2"]`); update the `/dl/` download counters. `update_windows.rs` is unaffected.

## Files to Modify
- `.github/workflows/dev.yml`
- `rust/fontaine/src/update/release_index.rs`
- `rust/fontaine/src/update/mod.rs`
- `rust/fontaine/src/update/auto.rs`
- `docs/verifying-releases.md`
- `docs/installing.md`
- `rust/fontaine/tests/update_cmd.rs`
- `rust/fontaine/tests/update_auto.rs`

## Decisions
- No API fallback (user decision): it would keep the rate-limit failure reachable and most of the old code, to cover only the pre-first-publish window and the brief `--clobber` 404.
- No `GITHUB_TOKEN` support: nothing on the dev path calls the API any more.
- Unsigned pointer: signing would need the `dev-signing` environment and a new signing kind; safety argument in Design 1.

## Testing Strategy
wiremock tests in `release_index.rs` (wiremock is a dev-dependency):
- Pointer served → returns the release with correct asset URLs; an API mock expecting zero calls pins the reported bug (no API call, so no 403).
- Pointer 404, malformed JSON, bad tag, and asset name containing `/` → each an error.

Integration tests `update_cmd.rs` and `update_auto.rs` are migrated to the pointer layout (Implementation step 5); they compile only with the new `download_base` field and no longer cover picking the highest of several dev tags.

## Manual Verification
After the first `main` build following merge: `curl -fsSL https://github.com/madesroches/fontaine/releases/download/dev/latest-dev.json` returns the latest `dev-N`, then `fontaine update` on the dev channel succeeds. Not automatable pre-merge because the dev workflow only runs on `main`.

## Open Questions
None.
