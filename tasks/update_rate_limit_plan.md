# Update Rate-Limit Resilience Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/755

## Overview
`fontaine update` on the dev channel spends several unauthenticated GitHub API calls (60/hour per IP) scanning release pages and can fail with a bare `403 Forbidden`. This plan bypasses the API on the dev channel: the dev workflow publishes a small `latest-dev.json` pointer on the stable `dev` release, and the updater reads it from `github.com/.../releases/download/dev/`, which is not API rate limited. The paged API scan is removed.

## Current State
`latest()` in `rust/fontaine/src/update/release_index.rs` walks `/releases?per_page=100&page=N` for up to `MAX_PAGES = 5` pages and keeps the highest `dev-N`. `get()` sends `Accept: application/vnd.github+json` and turns any non-success status into `IndexError::Status(status, url)`. `latest()` is called once, in `update/mod.rs` (~line 387), and `Release.assets` is looked up by name there (`release.asset(&name)`, `release.asset("<name>.minisig")`). In `.github/workflows/dev.yml` the `stable-installer` job (after `publish`) maintains the long-lived `dev` prerelease, uploads installers with `--clobber`, and is already guarded so it never moves to an older build than the newest published `dev-<n>`. The release channel uses `/releases/latest` (one API call) and is unchanged.

## Design
1. **Pointer asset.** In `stable-installer`, after the guard and the installer upload, generate `latest-dev.json` from the published `dev-<n>` release into `artifacts/latest-dev.json` and upload it to `dev` from there with `--clobber`:
   `{"tag":"dev-N","assets":["<asset name>", ...]}`, built with `gh release view "$DEV_TAG" --repo "$GITHUB_REPOSITORY" --json tagName,assets --jq '{tag: .tagName, assets: [.assets[].name]}'`, like the job's other gh calls. The new step carries `if: steps.guard.outputs.skip != 'true'` (so an older re-run cannot move the pointer back) and `env: DEV_TAG: ${{ needs.plan.outputs.tag }}` (DEV_TAG is set only in the guard step's own env, so it would otherwise expand empty). The existing "Check what users download" loop byte-compares it with the file just uploaded (added to its list of files). The pointer is unsigned: the updater still verifies every archive's minisign signature and signed version, so a tampered pointer can at worst hold updates back (an older or equal build makes `prepare()` report up to date) or steer one to another newer, signed and published dev build, and can never install an older or unsigned one.
2. **Client.** `latest()` for `Channel::Dev` fetches `{download_base}/{REPO}/releases/download/dev/latest-dev.json` (new `DEFAULT_DOWNLOAD_BASE = "https://github.com"`, a parameter alongside `api_base` so tests can mock it), parses it, validates `tag_build(Dev, tag)`, and builds a `Release` whose asset URLs are `{download_base}/{REPO}/releases/download/{tag}/{name}`. The pointer is fetched by a small new helper (not `get()`, which hard-codes the GitHub `Accept` header and maps failures to `Request`/`Status`) that sends a plain GET and maps all its failures into `Pointer`. The release-channel branch (`/releases/latest`) is unchanged.
3. **Errors.** There is no fallback. A new `IndexError::Pointer { url: String, cause: Option<Box<dyn std::error::Error + Send + Sync>> }` variant keeps the underlying cause, and `IndexError::source()` returns it, as `Request`, `Parse` and `DownloadError` do (release_index.rs:125-133 (`Request`, `Parse`) and mod.rs:223-230 (`DownloadError`)). It is used for every pointer failure (fetch/network, non-success status including 404, bad JSON, bad tag). A non-success response keeps its HTTP status: the `cause` is the reqwest error from `response.error_for_status()`, since users see only Display plus the `source()` chain. Its Display says the dev release pointer could not be fetched or is invalid and to retry (a pointer upload with `--clobber` briefly 404s). `index_error()` (mod.rs:182-187) sends it to `UpdateError::Network` via the existing `_` arm. `IndexError::NoRelease` is not used on the dev path. The `/releases` paging code, `PER_PAGE` and `MAX_PAGES` are deleted, along with the now-unread `draft` field of `ApiRelease` (release_index.rs:138-139); `"draft": false` in test JSON can stay.

## Implementation Steps
1. `.github/workflows/dev.yml` (`stable-installer`): generate `latest-dev.json` into `artifacts/` and upload it from there; add it to the "Check what users download" byte-compare loop; update every dev.yml place that describes what `dev` holds: the header comment (dev.yml:6-12, which says "holds only" and needs rewording), and the stable-installer job comment (414-415). The new pointer upload step carries its own comment noting that `--clobber` briefly 404s.
2. `update/release_index.rs`: replace the dev branch of `latest()` with the pointer fetch; add `IndexError::Pointer`; delete the page scan, its constants and the unread `ApiRelease.draft` field (see Design 3); add the download-base parameter and update the `latest()` doc comment (release_index.rs:185-186, which says `api_base` is the mockable parameter) to cover it.
3. `update/mod.rs` (`Params`, call site, ~line 659 default params) and `update/auto.rs` (~line 577): thread the download base.
4. Docs: `docs/verifying-releases.md` (the `dev` release also holds `latest-dev.json`, unsigned, and why that is safe); `docs/installing.md` (dev installs the build named by the `dev` release's `latest-dev.json`, a missing pointer is a network error, exit 4; the `dev` release also holds `latest-dev.json`; exit-code row 1 no longer treats a missing dev pointer as "no published release"; row 4 (release list or download could not be fetched) also covers the dev release pointer); `tasks/design/platforms.md` (~line 49 says dev holds only installers/APK/signatures; ~74-75 says stable-installer replaces the six assets: mention `latest-dev.json`); `CONTRIBUTING.md` (~155-156 lists files replaced on `dev`: add `latest-dev.json`).
5. `rust/fontaine/tests/update_cmd.rs` and `tests/update_auto.rs`: set `download_base` to the same value as `api_base` in each `Params` (the `Box::leak(...)` in update_auto.rs's `params()` helper, ~143-145, and the literal `http://127.0.0.1:1` address in `an_offline_pass_still_writes_the_stamp`, ~407-428) rather than `&server.uri()` everywhere; change `serve()` to mount `/madesroches/fontaine/releases/download/dev/latest-dev.json` and assets at `/madesroches/fontaine/releases/download/{tag}/{name}` (spelled literally: the test files' `REPO` is `"/repos/madesroches/fontaine"` at update_cmd.rs:16 and update_auto.rs:33, unlike the client's `"madesroches/fontaine"`; in update_cmd.rs keep `REPO` for the `/releases/latest` mock at :154 and leave the release-channel branch of `serve()` unchanged; in update_auto.rs `REPO` is used only at :195 for the dev `{REPO}/releases` listing mock and `serve()` has no release-channel branch, so delete the now-unused `REPO` const there, as CI's `clippy -D warnings` would fail on dead code) instead of `{REPO}/releases` and `/dl/{tag}/...`; replace the dev-5/dev-4 test `dev_update_replaces_the_binary_and_the_receipt_version` (update_cmd.rs:187-218) with a single-release dev-5 pointer case keeping all its current assertions (binary bytes replaced, install_layout "flat", report "updated to dev.5 ...", no leftovers), removing only the dev-4 sibling (it is the only end-to-end test of a successful manual dev update); the `for newest in ["dev-3","dev-2"]` loops (up-to-date/older-build, zero downloads) only move to the new `serve()` layout and keep their coverage; update the `/dl/` download counters (update_cmd.rs:355 `the_same_or_an_older_build_is_up_to_date_and_downloads_nothing`, and the `downloads()` helper at update_auto.rs:235-239 behind the `== 0` check at :373) to match asset paths only, e.g. prefix `/madesroches/fontaine/releases/download/dev-` (or exclude paths ending in `latest-dev.json`), so the always-made pointer fetch is not counted. `update_windows.rs` is unaffected.
6. `.github/workflows/update-e2e.yml`: set NEWEST from the `tag` field of `latest-dev.json` (fetched from the `dev` release download URL) instead of the highest `dev-N` in `gh release list` (~lines 33-37), and choose `previous` as the highest published `dev-N` strictly below the pointer's tag (not the second-highest from `gh release list`), failing if there is none; the requirement that the update lands on it (~95-96) stays, now against that value; update the header comment ("update to the newest one").
7. `CHANGELOG.md`: add a `### Fixed` entry under `## [Unreleased]` for the dev-channel update rate-limit fix (#755).
8. `tasks/fontaine_harness_plan.md` `## Decisions`: add a dated (#755) line recording the `latest-dev.json` pointer, that it is unsigned, and that there is no API fallback, amending the lines (~474, ~501) describing what `dev` holds.

## Files to Modify
- `.github/workflows/dev.yml`
- `.github/workflows/update-e2e.yml`
- `rust/fontaine/src/update/release_index.rs`
- `rust/fontaine/src/update/mod.rs`
- `rust/fontaine/src/update/auto.rs`
- `docs/verifying-releases.md`
- `docs/installing.md`
- `tasks/design/platforms.md`
- `CONTRIBUTING.md`
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md`
- `rust/fontaine/tests/update_cmd.rs`
- `rust/fontaine/tests/update_auto.rs`

## Decisions
- No API fallback (user decision): it would keep the rate-limit failure reachable and most of the old code, to cover only the pre-first-publish window and the brief `--clobber` 404.
- No `GITHUB_TOKEN` support: nothing on the dev path calls the API any more.
- Unsigned pointer: signing would need the `dev-signing` environment and a new signing kind; safety argument in Design 1.
- AFK: a dedicated `IndexError::Pointer` for all pointer failures (conservative: keeps them retryable as `UpdateError::Network` rather than the non-retryable `NoRelease` Install error).

## Testing Strategy
wiremock tests in `release_index.rs` (wiremock is a dev-dependency):
- Pointer served → returns the release with correct asset URLs; an API mock expecting zero calls pins the reported bug (no API call, so no 403).
- Pointer 404, malformed JSON, and bad tag → each an error. The 404 test also asserts that `fontaine_sdk::tools::error_chain::chain(&err)` (rust/fontaine-sdk/src/tools/error_chain.rs:5) contains "404".

- mod.rs unit test `a_missing_release_is_not_retryable` (mod.rs:741-747): change it to use `NoRelease(Channel::Release)`, and add an assertion that `IndexError::Pointer { .. }` maps to `EXIT_UPDATE_NETWORK` next to the existing `Status` assertion.

Integration tests: see Implementation step 5.

## Manual Verification
After the first `main` build following merge: `fontaine update` on the dev channel succeeds. Not automatable pre-merge because the dev workflow only runs on `main`. `update-e2e.yml` covers the update on the real feed from the second post-merge dev build onward, so this manual check is only needed for the first build.

## Open Questions
None.
