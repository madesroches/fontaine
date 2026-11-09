# Verifying releases

Every release archive, and each installer (shell and PowerShell) on both channels, has a detached [minisign](https://jedisct1.github.io/minisign/) signature
next to it, `<file>.minisig`, made in CI with a key Fontaine owns. A release is published only
after CI has downloaded the finished draft and verified every archive, and each installer, against the public key in
this repository, so an unsigned or mis-signed archive or installer is never public.

The archives of a dev build live in the immutable `dev-<n>` prerelease. The long-lived `dev`
prerelease holds only `fontaine-installer.sh` and `fontaine-installer.ps1` with their `.minisig`
files, replaced after every successful dev publish; each installer is the dist script rewritten to
download from that run's `dev-<n>`. The PowerShell installer is also patched, before it is signed, with a
sha256 check of the archive, which dist's own script lacks; the signature covers exactly the script
you download.

## Channels and keys

| Channel | What it carries | Signed with | Public key |
|---|---|---|---|
| `release` | tagged releases (`vX.Y.Z`) | the release key | [`rust/fontaine/keys/release.pub`](https://github.com/madesroches/fontaine/blob/main/rust/fontaine/keys/release.pub) |
| `dev` | the latest CI-passing build of `main`, published as the prerelease `dev-<n>` | the dev key | [`rust/fontaine/keys/dev.pub`](https://github.com/madesroches/fontaine/blob/main/rust/fontaine/keys/dev.pub) |

The two channels use different keys, so a compromised dev key cannot sign a release. The keys are
checked in and compiled into Fontaine. `fontaine update` verifies every archive it downloads with
the key of the channel the install came from, requires the signed channel, file name and version to
match the release it was offered, and replaces nothing if any check fails (see
[Updating](installing.md#updating)). To verify an archive you downloaded yourself, do it by hand, as
below.

A release is verified against `<channel>.signing.pub`, the key CI signed with, read at that
release's tag (for `dev`, at its commit):
[`release.signing.pub`](https://github.com/madesroches/fontaine/blob/main/rust/fontaine/keys/release.signing.pub) and
[`dev.signing.pub`](https://github.com/madesroches/fontaine/blob/main/rust/fontaine/keys/dev.signing.pub). It differs from `<channel>.pub` only in
a key-rotation release, where `<channel>.pub` already holds the next key.

## Verify an archive by hand

Install `minisign`, then download an archive, its `.minisig` and the channel's signing key from the
release's tag:

```
minisign -Vm fontaine-x86_64-unknown-linux-gnu.tar.xz -p release.signing.pub
```

Expected output includes `Signature and comment signature verified`, followed by the `Trusted
comment:` line. The signature file must be
named `<archive>.minisig` and sit next to the archive, or be given with `-x`.

The signed trusted comment binds the signature to one archive, or to one installer:

```
fontaine-signature v1 channel=<release|dev> file=<archive name> version=<version> commit=<git sha>
```

Read it from the third line of the signature file, or from the `Trusted comment:` line minisign
prints. Check that `channel` is the channel you meant, `file` is the archive (or
`fontaine-installer.sh` or `fontaine-installer.ps1`) you downloaded and
`version` is the one you expected: a validly signed archive for another target fails this check.

Each archive also has a `.sha256` file, and the release has a combined `sha256.sum`:

```
sha256sum -c fontaine-x86_64-unknown-linux-gnu.tar.xz.sha256
sha256sum -c --ignore-missing sha256.sum
```

The checksums catch a corrupted download; only the signature proves who made the archive.

## Verify the installer by hand

Download `fontaine-installer.sh` (or `fontaine-installer.ps1` for Windows) and its `.minisig` from the
[`dev` release](https://github.com/madesroches/fontaine/releases/tag/dev) and
[`dev.signing.pub`](https://github.com/madesroches/fontaine/blob/main/rust/fontaine/keys/dev.signing.pub), then:

```
minisign -Vm fontaine-installer.sh -p dev.signing.pub
minisign -Vm fontaine-installer.ps1 -p dev.signing.pub
```

The trusted comment has `channel=dev` and `file=fontaine-installer.sh` (or `.ps1`); its `version` and
`commit` name the build whose `dev-<n>` archives the script downloads. The installers do not verify
this signature themselves; see [Installing Fontaine](installing.md#verification) and [on Windows](installing.md#verification-windows).

The installers attached to a tagged release are signed the same way: download one and its
`.minisig` from that release and verify with `release.signing.pub`; the trusted
comment has `channel=release`, the installer's `file`, and the release's version and commit.

## What the signature does not cover

The first install is trusted through HTTPS: the public key you verify against comes from this
repository over the same channel as the archive. The installers (`fontaine-installer.sh` and
`fontaine-installer.ps1`) are signed on both channels but do not check the minisign signature
themselves; they verify the archive's sha256 only, which the shell installer does only when `sha256sum` is
present and the PowerShell installer always does (see [Installing Fontaine](installing.md)). To get a signature
check, download the archive and verify it as above; `fontaine update` does this itself for the
archive it installs. Once Fontaine's archives enable GitHub artifact attestations, `gh attestation
verify` will apply as a further check; the minisign key stays the root of trust.
