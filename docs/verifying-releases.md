# Verifying releases

Every release archive, and the shell installer on both channels, has a detached [minisign](https://jedisct1.github.io/minisign/) signature
next to it, `<archive>.minisig`, made in CI with a key Fontaine owns. A release is published only
after CI has downloaded the finished draft and verified every archive, and the installer, against the public key in
this repository, so an unsigned or mis-signed archive or installer is never public.

The archives of a dev build live in the immutable `dev-<n>` prerelease. The long-lived `dev`
prerelease holds only `fontaine-installer.sh` and `fontaine-installer.sh.minisig`, replaced after
every successful dev publish; the installer is the dist script rewritten to download from that
run's `dev-<n>`.

## Channels and keys

| Channel | What it carries | Signed with | Public key |
|---|---|---|---|
| `release` | tagged releases (`vX.Y.Z`) | the release key | [`rust/fontaine/keys/release.pub`](../rust/fontaine/keys/release.pub) |
| `dev` | the latest CI-passing build of `main`, published as the prerelease `dev-<n>` | the dev key | [`rust/fontaine/keys/dev.pub`](../rust/fontaine/keys/dev.pub) |

The two channels use different keys, so a compromised dev key cannot sign a release. The keys are
checked in and compiled into the verifier module that Fontaine's update path will use. Fontaine
does not update itself yet, so today you verify a downloaded archive by hand, as below.

A release is verified against `<channel>.signing.pub`, the key CI signed with, read at that
release's tag (for `dev`, at its commit):
[`release.signing.pub`](../rust/fontaine/keys/release.signing.pub) and
[`dev.signing.pub`](../rust/fontaine/keys/dev.signing.pub). It differs from `<channel>.pub` only in
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

The signed trusted comment binds the signature to one archive, or to the installer:

```
fontaine-signature v1 channel=<release|dev> file=<archive name> version=<version> commit=<git sha>
```

Read it from the third line of the signature file, or from the `Trusted comment:` line minisign
prints. Check that `channel` is the channel you meant, `file` is the archive (or
`fontaine-installer.sh`) you downloaded and
`version` is the one you expected: a validly signed archive for another target fails this check.

Each archive also has a `.sha256` file, and the release has a combined `sha256.sum`:

```
sha256sum -c fontaine-x86_64-unknown-linux-gnu.tar.xz.sha256
sha256sum -c --ignore-missing sha256.sum
```

The checksums catch a corrupted download; only the signature proves who made the archive.

## Verify the installer by hand

Download `fontaine-installer.sh` and `fontaine-installer.sh.minisig` from the
[`dev` release](https://github.com/madesroches/fontaine/releases/tag/dev) and
[`dev.signing.pub`](../rust/fontaine/keys/dev.signing.pub), then:

```
minisign -Vm fontaine-installer.sh -p dev.signing.pub
```

The trusted comment has `channel=dev` and `file=fontaine-installer.sh`; its `version` and `commit`
name the build whose `dev-<n>` archives the script downloads. The installer does not verify this
signature itself; see [Installing Fontaine](installing.md#verification).

The installer attached to a tagged release is signed the same way: download it and
`fontaine-installer.sh.minisig` from that release and verify with `release.signing.pub`; the trusted
comment has `channel=release`, `file=fontaine-installer.sh`, and the release's version and commit.

## What the signature does not cover

The first install is trusted through HTTPS: the public key you verify against comes from this
repository over the same channel as the archive. The shell installer (`fontaine-installer.sh`) is
signed on both channels but does not check the minisign signature itself; it verifies the archive's sha256 only, and
only when `sha256sum` is present (see [Installing Fontaine](installing.md)). To get a signature
check, download the archive and verify it as above. Fontaine does not update itself yet; once
it does, the signature chain will be what keeps an update honest. Once Fontaine's archives enable GitHub artifact attestations, `gh attestation
verify` will apply as a further check; the minisign key stays the root of trust.
