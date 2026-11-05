# Verifying releases

Every release archive has a detached [minisign](https://jedisct1.github.io/minisign/) signature
next to it, `<archive>.minisig`, made in CI with a key Fontaine owns. A release is published only
after CI has downloaded the finished draft and verified every archive against the public key in
this repository, so an unsigned or mis-signed archive is never public.

## Channels and keys

| Channel | What it carries | Signed with | Public key |
|---|---|---|---|
| `release` | tagged releases (`vX.Y.Z`) | the release key | [`rust/fontaine/keys/release.pub`](../rust/fontaine/keys/release.pub) |
| `dev` | the latest CI-passing build of `main`, published as the prerelease `dev-<n>` | the dev key | [`rust/fontaine/keys/dev.pub`](../rust/fontaine/keys/dev.pub) |

The two channels use different keys, so a compromised dev key cannot sign a release. The keys are
compiled into the `fontaine` binary.

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

Expected output ends with `Signature and comment signature verified`. The signature file must be
named `<archive>.minisig` and sit next to the archive, or be given with `-x`.

The signed trusted comment binds the signature to one archive:

```
fontaine-signature v1 channel=<release|dev> file=<archive name> version=<version> commit=<git sha>
```

Read it from the third line of the signature file, or from the `Trusted comment:` line minisign
prints. Check that `channel` is the channel you meant, `file` is the archive you downloaded and
`version` is the one you expected: a validly signed archive for another target fails this check.

Each archive also has a `.sha256` file, and the release has a combined `sha256.sum`:

```
sha256sum -c fontaine-x86_64-unknown-linux-gnu.tar.xz.sha256
sha256sum -c --ignore-missing sha256.sum
```

The checksums catch a corrupted download; only the signature proves who made the archive.

## What the signature does not cover

The first install is trusted through HTTPS: the public key you verify against comes from this
repository over the same channel as the archive. After that, the signature chain is what keeps an
update honest. Once Fontaine's installers enable GitHub artifact attestations, `gh attestation
verify` will apply as a further check; the minisign key stays the root of trust.
