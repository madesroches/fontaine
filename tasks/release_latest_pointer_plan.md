# Release Channel `latest.json` Pointer Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/787

## Overview
The release channel of `fontaine update` and the background update pass still call
`GET api.github.com/repos/.../releases/latest` unauthenticated (60 requests an hour per IP), so users behind a
shared IP can get the bare `403` the dev channel had before #755. This plan publishes a `latest.json` pointer
(same schema as `latest-dev.json`) on every tagged release, reads it through GitHub's
`releases/latest/download/latest.json` redirect, and makes `latest()` one code path for both channels. It has to
ship before the 0.1.0 tag: every later update starts from the updater in that build.

## Current State
- `rust/fontaine/src/update/release_index.rs`: `latest(client, api_base, download_base, channel)`. The `Release`
  arm calls the API through `get()` (GitHub `Accept` header, `IndexError::Status`/`Request`/`Parse`) and
  deserialises `ApiRelease`/`ApiAsset`. The `Dev` arm fetches `{download_base}/{REPO}/releases/download/dev/latest-dev.json`
  with `fetch_pointer`, validates `tag_build(Dev, tag)` and builds asset URLs `{base}/{REPO}/releases/download/{tag}/{name}`.
  `IndexError` has `Request`, `Status`, `Parse`, `NoRelease(Channel)` and `Pointer { url, cause }` (dev only).
- `DEFAULT_API_BASE` and `Params.api_base` (`update/mod.rs:109`, default at ~661) and `update/auto.rs:577`.
  `latest()` is called at `update/mod.rs:389`. `index_error()` maps every non-`NoRelease` variant to `UpdateError::Network` (exit 4).
- `.github/workflows/release.yml`: `host` creates the draft with `gh release create ... artifacts/*` after
  removing the dist manifests; `verify-draft` runs `gh release download` then `.github/scripts/release-verify.sh release draft ...`,
  whose asset loop rejects any name it does not recognise (`*) fail "unrecognised asset"`). `publish` flips the draft public.
  Prerelease tags (`v0.2.0-rc.1`) are published with `--prerelease`.
- `dev.yml`'s `stable-installer` writes `latest-dev.json` with `gh release view --json tagName,assets --jq`; that
  precedent is in `tasks/completed/update_rate_limit_plan.md`.
- Tests: `rust/fontaine/tests/update_cmd.rs` (`serve()` mounts the dev pointer, assets, and a `/repos/.../releases/latest` API mock for the release channel; `Params` set `api_base`) and `tests/update_auto.rs` (dev only; `Params` set `api_base`).

## Design
1. **Publish.** In `release.yml` `host`, after `gh release create`, add a step that builds
   `latest.json` from the draft with
   `gh release view "$TAG" --repo "$GITHUB_REPOSITORY" --json tagName,assets --jq '{tag: .tagName, assets: [.assets[].name]}'`
   into `latest.json` outside `artifacts/` and uploads it with `gh release upload "$TAG" latest.json`. It is generated from
   the draft, so it names exactly the assets that exist and not itself. It is unsigned, as `latest-dev.json` is:
   archives are minisign-verified against the channel key with signed channel, file name and version, and `tag_build(Release, ..)`
   validates the tag, so a tampered pointer can at worst hold updates back or steer to another newer, signed, published release.
2. **Verify.** `release-verify.sh`: in the release-channel asset loop, accept `latest.json` (any other channel still rejects it,
   like the installers). After the loop, when `latest.json` is present, check it: `jq` parses it, `tag` is the expected tag, and
   `assets` equals the sorted set of other file names in the directory. The script takes `<version>` (no `v`) so the tag is `v$version`.
   `FONTAINE_REQUIRE_APK`-style strictness: add `FONTAINE_REQUIRE_POINTER=1` set by `verify-draft`, making it mandatory there, so a
   missing pointer cannot be published. The script's existing test harness (see `.github/scripts/` tests) gets cases for the new rules.
3. **Client.** `latest()` becomes `latest(client, download_base, channel)`. A `pointer_url(base, channel)` helper returns
   `{base}/{REPO}/releases/latest/download/latest.json` for `Release` and `{base}/{REPO}/releases/download/dev/latest-dev.json` for
   `Dev`. The shared body is today's dev arm: `fetch_pointer`, `tag_build(channel, tag)`, asset URLs under `releases/download/{tag}/{name}`.
   Delete `get()`, `ApiRelease`, `ApiAsset`, `DEFAULT_API_BASE`, `Params.api_base`, `IndexError::Request`, `Status`, `Parse` and
   `NoRelease` if nothing else uses them (check `mod.rs` `index_error` and its unit tests); `Pointer`'s Display says "release pointer" with the channel name.
   No API fallback: a missing pointer is a network error (exit 4).
4. **Redirect.** `releases/latest/download/<asset>` answers `302` to `releases/download/<tag>/<asset>`. reqwest follows redirects by
   default and the update client is the one built with the `[network]` settings, as the dev pointer fetch already is; verify in code
   that the update client does not set `redirect(Policy::none())` and add a wiremock test with a 302.
5. **Prerelease tags.** GitHub's `latest` skips prereleases and drafts, so `v0.2.0-rc.1` and `dev-<n>` are never the target, and
   `tag_build(Release, ..)` rejects a suffixed tag anyway (already unit-tested, `v1.2.3-rc.1`).

## Implementation Steps
1. `release_index.rs`: restructure `latest()` and delete the API code as in Design 3; update the doc comment and `Pointer` Display.
2. `update/mod.rs` and `update/auto.rs`: drop `api_base` from `Params` and the defaults; adjust `index_error()` and its unit tests (`a_missing_release_is_not_retryable` and neighbours).
3. `release.yml` (`host` pointer step, header comment) and `.github/scripts/release-verify.sh` plus its tests; `verify-draft` sets `FONTAINE_REQUIRE_POINTER=1`.
4. Tests (see Testing Strategy).
5. Docs and Decisions (see below), and a `### Changed` line under `## [Unreleased]` in `CHANGELOG.md`.

## Files to Modify
- `rust/fontaine/src/update/release_index.rs`, `update/mod.rs`, `update/auto.rs`
- `rust/fontaine/tests/update_cmd.rs`, `tests/update_auto.rs`
- `.github/workflows/release.yml`, `.github/scripts/release-verify.sh` (and its test script)
- `docs/installing.md`, `docs/verifying-releases.md`, `tasks/design/platforms.md`, `CONTRIBUTING.md` (§Releasing)
- `tasks/fontaine_harness_plan.md` (`## Decisions`), `CHANGELOG.md`

## Trade-offs
- API with a `GITHUB_TOKEN`: rejected, anonymous users and shared hosts would still fail; two code paths stay.
- Unsigned pointer: same safety argument as #755; signing would need a new signing kind in the `sign` job for no gain over the per-archive signatures.
- Pointer generated outside `dist`: dist has no hook for extra assets, and `host` already owns the draft.

## Decisions
- AFK: `FONTAINE_REQUIRE_POINTER=1` makes the pointer mandatory in `verify-draft` (conservative: a release without one would strand 0.1.0 users).

## Documentation
- `docs/installing.md`: release channel reads `latest.json` from the latest release; missing pointer is exit 4; adjust the exit-code rows that mention a missing release.
- `docs/verifying-releases.md`: releases also hold the unsigned `latest.json` and why it is safe.
- `tasks/design/platforms.md`: release assets and `host`/`verify-draft` steps mention `latest.json`.
- `CONTRIBUTING.md` §Releasing: step 6 mentions the pointer and that verify-draft checks it.
- `tasks/fontaine_harness_plan.md` `## Decisions`: a dated (#787) line in the same subsection as the #755 line recording the release pointer, unsigned, no API fallback, amending that line's "release channel unchanged" scope.

## Testing Strategy
- `release_index.rs` wiremock unit tests: release pointer served (with an API mock expecting zero calls); served through a 302 from `releases/latest/download/latest.json` to the tag's pointer; 404, malformed JSON, and a bad tag (`dev-3`, `v1.0.0-rc.1`) each an error with the 404 kept in the chain.
- `tests/update_cmd.rs` and `tests/update_auto.rs`: switch the release-channel mock to the pointer path, remove `api_base` from `Params`, and keep their existing assertions; update_auto gains a release-channel case if it only covers dev today.
- `release-verify.sh` tests: valid pointer accepted; missing pointer fails under `FONTAINE_REQUIRE_POINTER=1`; wrong tag, an asset list that differs from the directory, and `latest.json` on the dev channel fail.
- `cargo fmt --check`, clippy `-D warnings`, `cargo test`.

## Manual Verification
After the first tagged release's `verify-draft`, `curl -sIL https://github.com/madesroches/fontaine/releases/latest/download/latest.json` shows the redirect to the tag. Not automatable before a release exists; the `update-e2e.yml` release leg needs two tagged releases.

## Open Questions
None.
