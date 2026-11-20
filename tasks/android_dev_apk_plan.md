# Android APK on the Dev Channel Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/716

## Overview

Publish `fontaine-android.apk` (with its `.minisig`) on the long-lived `dev` prerelease next to the
dev installers, so the README can link the stable URL
`https://github.com/madesroches/fontaine/releases/download/dev/fontaine-android.apk`. Today the APK
exists only on tagged releases (`release.yml`), or as an expiring debug-signed workflow artifact
(`android.yml`).

## Current State

- `.github/workflows/dev.yml`: `plan` → `build-local-artifacts`/`build-global-artifacts` → `sign`
  (environment `dev-signing`, the only holder of keys) → `host` (draft `dev-<n>`) → `verify-draft`
  → `publish` → `stable-installer` (replaces the installers on the `dev` release). The installers
  are set aside in `dev-installer/`, signed by `release-sign.sh dev-installer`, uploaded as the
  `dev-installer` artifact (not `artifacts-*`, so `host`/`verify-draft` never see them) and checked by
  `release-verify.sh dev-installer` in `stable-installer` before upload.
- `.github/workflows/release.yml` `build-android` (unsigned APK built by
  `fontaine-android/build-apk.sh`, uploaded as `artifacts-build-android`) and `sign` (apksigner with
  `ANDROID_KEYSTORE_BASE64`/`ANDROID_KEYSTORE_PASSWORD`/`ANDROID_KEY_ALIAS`, then minisign).
- `.github/scripts/release-sign.sh` signs the APK on the `release` channel only (comment says so).
- `.github/scripts/release-verify.sh`: `dev-installer` kind accepts exactly the two installers and
  their signatures; the `dev` archive kind rejects the APK; the release kind accepts it and requires
  it under `FONTAINE_REQUIRE_APK=1`.
- `.github/workflows/ci.yml` (about lines 295-311) holds the fixture tests of the APK on release.
- The master plan `## Decisions` has a 2026-11-18 (#710, AFK) line recording the APK as
  release-channel only.
- Docs say the same: `docs/installing.md` ("not to `dev` builds"), `docs/android.md`,
  `docs/verifying-releases.md`; the README links the releases page.

## Design

The APK follows the installers' path, not the archives': it lives on the stable `dev` release only,
as `dev-<n>` is the immutable archive release and `dev` is the one URL that always means "latest".

1. **Build** (`dev.yml`, new job `build-android`, `needs: plan`): copy of release.yml's job, with
   `ref: ${{ env.HEAD_SHA }}`. It uploads the unsigned APK as artifact `android-unsigned-apk` (not
   `artifacts-*`, so it never lands in the archives set that `dev` verification would reject).
   No keystore is in scope.
2. **Sign** (`sign` job gains `needs: build-android`): download `android-unsigned-apk` into
   `dev-installer/`, apply the Android signature with `apksigner` using the keystore secrets from the
   `dev-signing` environment (same step shape as release.yml), then
   `release-sign.sh dev-installer`, which now also signs the APK. The `dev-installer` artifact thus
   carries both installers and the APK with signatures.
3. **Android signing key**: a dedicated dev keystore, distinct from the release one, stored as
   `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` in the `dev-signing`
   environment. It is stable across builds so dev builds update in place. Because the dev APK and the
   release APK are signed by different keys, Android refuses to install one over the other; the docs
   say so. The `versionCode` must increase per dev build for Android to accept an update: the
   implementer checks how `build-apk.sh` derives it and, if it is not monotonic, derives it from the
   run number (see Open Questions).
4. **Verify** (`release-verify.sh`): the `dev-installer` kind accepts `fontaine-android.apk` and its
   `.minisig` in addition to the installers (trusted comment `channel=dev`), and requires the APK
   under `FONTAINE_REQUIRE_APK=1`. `stable-installer` sets that variable.
5. **Publish** (`stable-installer`): upload the APK and its `.minisig` with the installers via
   `gh release upload dev ... --clobber`, and extend the "Check what users download" byte compare to
   them. The `guard` step already prevents going back to an older build.
6. **Every push to `main`**: yes, same trigger as the installers (the dev workflow runs after each
   passing CI on `main`); the APK build adds no separate trigger.
7. `release-sign.sh` comment and header, and `release-verify.sh` header, change to describe the new
   behavior.

## Implementation Steps

1. `.github/scripts/release-sign.sh`: in the `dev-installer` branch also include
   `$dir/fontaine-android.apk` when present; update the comments (the "release channel only" comment
   goes).
2. `.github/scripts/release-verify.sh`: in the `dev-installer` branch accept the APK and its
   `.minisig`, check it with `check_installer ... dev` when present, require it under
   `FONTAINE_REQUIRE_APK=1`; update headers.
3. `.github/workflows/dev.yml`: add `build-android`; extend `sign` (needs, download, apksigner step,
   `dev-installer` dir); extend `stable-installer` (require APK, upload, byte compare); update the
   top-of-file comment.
4. `.github/workflows/ci.yml`: add dev-installer fixture tests: sign a fixture with the APK, verify
   passes, required-and-missing fails, tampered APK fails, an APK with a `channel=release` signature
   fails.
5. `tasks/fontaine_harness_plan.md` `## Decisions`: update the 2026-11-18 (#710, AFK) line's
   "release channel only" claim and add a dated line for this change (dev keystore, `dev` release
   only), at the end of its subsection.
6. Docs: `docs/android.md`, `docs/installing.md`, `docs/verifying-releases.md` (dev APK, the stable
   URL, `dev.signing.pub`, the key-mismatch note), README link.
7. `CHANGELOG.md` entry.

## Files to Modify

- `.github/scripts/release-sign.sh`, `.github/scripts/release-verify.sh`
- `.github/workflows/dev.yml`, `.github/workflows/ci.yml`
- `tasks/fontaine_harness_plan.md`
- `docs/android.md`, `docs/installing.md`, `docs/verifying-releases.md`, `README.md`, `CHANGELOG.md`

## Trade-offs

- APK on `dev-<n>` as well as `dev`: rejected; the stable URL is the goal and `dev-<n>` archives go
  through the dist-checksum path the APK does not have.
- Reusing the release keystore: rejected; the dev environment must never hold release secrets.
- Debug-signing the dev APK: rejected; each build would need a different or shared public debug key
  and would give no authenticity.

## Decisions

## Documentation

See step 6: the dev APK URL, that it is signed by the dev keys, and that dev and release APKs cannot
update each other.

## Testing Strategy

- `ci.yml` fixture tests (steps above) exercise `release-sign.sh` and `release-verify.sh` for the
  dev-installer kind with the APK: signed, required, tampered, wrong channel.
- `actionlint`-style syntax check of `dev.yml` if the repo already uses one; otherwise the existing
  `.github/scripts` checks.

## Manual Verification

After merge, with the repository variable and `dev-signing` secrets set: run the dev workflow, then
`curl -fLO https://github.com/madesroches/fontaine/releases/download/dev/fontaine-android.apk{,.minisig}`
and `minisign -Vm fontaine-android.apk -p rust/fontaine/keys/dev.signing.pub` must succeed. This needs
the real secrets and the live release, which no fixture has.

## Open Questions

- Does `fontaine-android/build-apk.sh` produce a monotonically increasing `versionCode`? If not,
  dev updates over a previous dev APK are rejected by Android. Settle by reading the script.
- The dev Android keystore and secrets must be created in the `dev-signing` environment by the
  maintainer; the workflow fails closed with an explicit error until they exist.
