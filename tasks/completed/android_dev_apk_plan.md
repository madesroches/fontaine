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
   `dev-installer/` (the download step goes after the "Set the installers aside" step, which runs bare
   `mkdir dev-installer`), apply the Android signature with `apksigner` using the keystore secrets from the
   `dev-signing` environment (same step shape as release.yml), then
   `release-sign.sh dev-installer`, which now also signs the APK and fails if it is missing (as it
   does for each installer). The `dev-installer` artifact thus carries both installers and the APK
   with signatures. The APK is a hard dependency of `sign`: no unsigned or missing APK is published.
3. **Android signing key**: a dedicated dev keystore, distinct from the release one, stored as
   `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` in the `dev-signing`
   environment. It is stable across builds so dev builds update in place. Because the dev APK and the
   release APK are signed by different keys, Android refuses to install one over the other; the docs
   say so.
   **versionCode**: `build-apk.sh` derives `major*1000000 + minor*1000 + patch` from `rust/Cargo.toml`
   (e.g. 1.2.3 gives 1002003), ignoring any prerelease/build suffix, and takes only `<out.apk> [--debug-sign]`
   with `versionName` the bare Cargo version. Every dev build between version bumps would thus share
   one `versionCode` and Android would not update in place. `build-apk.sh` therefore accepts optional
   `FONTAINE_APK_VERSION_CODE` and `FONTAINE_APK_VERSION_NAME`, which `dev.yml`'s `build-android` sets
   (code = dev.yml `run_number`, name = `<version>-dev.<n>+<commit>`, the receipt version of the
   installers). Unset, the script behaves exactly as today (release and `android.yml` unchanged).
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

0. `rust/fontaine-android/version-code.sh` (new, sourceable, no side effects on source): a shell
   function that takes the Cargo.toml path as an argument (`build-apk.sh` passes the real
   `rust/Cargo.toml`), derives `code`/`name` from it and applies the optional
   `FONTAINE_APK_VERSION_CODE` (must be a positive integer below 2100000000, else return 1) and
   `FONTAINE_APK_VERSION_NAME` overrides; unset keeps today's values. `build-apk.sh` sources it
   (today its derivation runs after `cargo ndk`, so CI could not run it without the NDK) and its
   header comment is updated; no new flag.
1. `.github/scripts/release-sign.sh`: in the `dev-installer` branch require and sign
   `$dir/fontaine-android.apk` (`fail "no $apk in '$dir'"` if missing, like the installers); update
   the comments (the "release channel only" comment goes).
2. `.github/scripts/release-verify.sh`: in the `dev-installer` branch accept the APK and its
   `.minisig`, check it with `check_installer ... dev` when present, require it under
   `FONTAINE_REQUIRE_APK=1`; fail if `fontaine-android.apk.minisig` exists without the APK (mirror
   the release kind's "has no archive" check); update headers.
3. `.github/workflows/dev.yml`: add `build-android` (sets the two version env vars); extend `sign`
   (needs, download, apksigner step, `dev-installer` dir); extend `stable-installer` (require APK, upload, byte compare); update the
   top-of-file comment, the `sign` job comment ("runs no build tools..." is false once `apksigner`
   runs there), the stable-installer "Replace the installers" step name and the dev release notes
   text ("latest dev installer"), which now also cover the APK.
4. `.github/workflows/ci.yml` (dev-installer fixtures, about lines 578-650): the signer now requires
   the APK, so the existing dev-installer sign fixtures (`signed_installer`, `other_signed`,
   `one_missing`) get a fixture APK added. APK-less fixtures stay valid for the verifier when
   `FONTAINE_REQUIRE_APK` is unset (only the signer demands the APK). APK cases mirror the installer
   set: tampered, missing `.minisig`, renamed APK, wrong channel (`channel=release` signature), plus
   required-and-missing under `FONTAINE_REQUIRE_APK=1`, and a signer-missing-APK case beside the
   signer-missing-installer loop. Also pin that the `dev` archive kind of `release-verify.sh` still
   rejects `fontaine-android.apk` (the existing `channel = release` guard), add the fixture case of a
   `.minisig` for the APK with no APK (verifier must fail), and add a shell check of the version
   code by sourcing `fontaine-android/version-code.sh` (no NDK needed): `FONTAINE_APK_VERSION_CODE`
   of `0`, a non-numeric value and `2100000000` are rejected, and unset derives the code from a
   fixture Cargo.toml the check writes itself (`version = "1.2.3"` gives 1002003), never from the real
   `rust/Cargo.toml`, so a version bump cannot turn the check red.
5. `tasks/fontaine_harness_plan.md` `## Decisions`: update the 2026-11-18 (#710, AFK) line's
   "release channel only" claim and add a dated line for this change (dev keystore, `dev` release
   only), at the end of its subsection.
6. Docs: `docs/android.md`, README link, and the stale lines of:
   - `CONTRIBUTING.md` lines ~117-118 (signing keys table: `dev-signing` holds only the minisign
     secrets; add the three `ANDROID_*` secrets naming the dev keystore, a separate key from the
     release one; the `release-signing` row also lacks the same three `ANDROID_*` secrets, which
     release.yml's `sign` job already reads for the release keystore) and ~149-154 ("Enabling the dev channel" lists only the installers; add the APK
     and its `.minisig`, and the three secrets to create).
   - `tasks/design/platforms.md` line ~51 (private keys exist only as the `release-signing` and
     `dev-signing` secrets: mention the Android keystore alongside the minisign keys), line ~49 (`dev` "holds only the installers") and lines ~71-80
     (signer "requires both" installers, "replaces the four assets": now the APK too, six assets).
   - `docs/verifying-releases.md` lines 7-9 ("holds only `fontaine-installer.sh` and ...").
   - `docs/verifying-releases.md` line 3 ("Android APK of a tagged release"), lines ~89-99 (APK
     described as release key only; add the dev APK, its Android signature from the dev keystore and
     the key-mismatch note), line ~32 and lines ~72-76 (dev installers under `dev.signing.pub`; add
     the APK).
   - `docs/installing.md` line ~248-249 ("The `dev` release itself holds only the installers.") and
     lines ~256-257 ("not to `dev` builds"): add the stable dev APK URL.
   - the master plan's workspace-layout description of `dev.yml` (line ~207, "the signed dev
     channel"): mention the dev APK.
7. `CHANGELOG.md` entry.

## Files to Modify

- `rust/fontaine-android/build-apk.sh`
- `.github/scripts/release-sign.sh`, `.github/scripts/release-verify.sh`
- `.github/workflows/dev.yml`, `.github/workflows/ci.yml`
- `rust/fontaine-android/version-code.sh` (new)
- `tasks/fontaine_harness_plan.md`, `tasks/design/platforms.md`, `CONTRIBUTING.md`
- `docs/android.md`, `docs/installing.md`, `docs/verifying-releases.md`, `README.md`, `CHANGELOG.md`

## Trade-offs

- APK on `dev-<n>` as well as `dev`: rejected; the stable URL is the goal and `dev-<n>` archives go
  through the dist-checksum path the APK does not have.
- Reusing the release keystore: rejected; the dev environment must never hold release secrets.
- Debug-signing the dev APK: rejected; each build would need a different or shared public debug key
  and would give no authenticity.

## Decisions

- AFK: dev `versionCode` is the dev.yml `run_number` (monotonic across Cargo bumps; dev and release APKs use different keys and never update each other, so it need not relate to the Cargo-derived code); it stays far below Android's 2100000000 limit. `base*1000+run` was rejected: it collides once run numbers pass 999.
- AFK: the APK is a hard dependency of `sign` (fail closed): until the maintainer creates the dev Android keystore secrets in `dev-signing`, the dev channel stops publishing; they must exist before merge.
- AFK: `release-sign.sh dev-installer` requires the APK; `release-verify.sh dev-installer` requires it only under `FONTAINE_REQUIRE_APK=1`, which `stable-installer` sets; the `dev` archive kind keeps rejecting the APK.
- AFK: the dev keystore is created by the maintainer, outside CI and agents; CONTRIBUTING.md documents no Android keystore procedure for the release keystore either, so it gets only the three secret names, not a generation, validity or backup policy.

## Documentation

See step 6 (exact stale lines listed there): the dev APK URL, that it is signed by the dev keys, and that dev and release APKs cannot
update each other.

## Testing Strategy

- `ci.yml` fixture tests (steps above) exercise `release-sign.sh` and `release-verify.sh` for the
  dev-installer kind with the APK: signed, required, tampered, wrong channel.

## Manual Verification

After merge, with the repository variable and `dev-signing` secrets set: run the dev workflow, then
`curl -fLO https://github.com/madesroches/fontaine/releases/download/dev/fontaine-android.apk{,.minisig}`
and `minisign -Vm fontaine-android.apk -p rust/fontaine/keys/dev.signing.pub` must succeed. This needs
the real secrets and the live release, which no fixture has.

## Open Questions

None.