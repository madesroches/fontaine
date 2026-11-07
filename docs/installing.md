# Installing Fontaine

Fontaine ships as a single `fontaine` binary. There are three ways to get it: the one-line installer
(Linux and macOS), a release archive from GitHub (every platform), and `cargo install` from source.
No tagged release exists yet, so the installer and the archives are dev builds of `main`.

## The installer

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.sh | sh
```

This installs the latest CI-passing build of `main`. The installer is the single file on the
long-lived `dev` prerelease, replaced after every successful dev publish; it downloads the archive
from the immutable `dev-<n>` prerelease of the build it was made for. Its receipt `version` is
`<version>-dev.<n>+<commit>`, so a dev install is recognisable by the `-dev.` in it. It supports
Linux x86_64 and macOS on Apple silicon and Intel. There is no Linux arm64 build and no Windows installer; use the archive
there.

It needs `curl` or `wget`, and `tar`. It detects your platform, downloads the matching archive from
that `dev-<n>` release, and writes:

- the `fontaine` binary in `$CARGO_HOME/bin` (`~/.cargo/bin` when `CARGO_HOME` is unset);
- an install receipt at `${XDG_CONFIG_HOME:-~/.config}/fontaine/fontaine-receipt.json`, recording
  the install prefix (`$CARGO_HOME`, or `~/.cargo`), the cargo-home layout and the installed
  binary, so a future self-updater can tell how Fontaine was installed;
- shell setup that puts `$CARGO_HOME/bin` on your `PATH`: `$CARGO_HOME/env` and `env.fish`, a line
  in your shell's rc files that sources it, and, for fish,
  `~/.config/fish/conf.d/fontaine.env.fish`. Set `FONTAINE_NO_MODIFY_PATH=1` to skip these edits.

Open a new shell, then run `fontaine --version`.

### Verification

The installer checks the archive against the sha256 embedded in the script, the same checksum the
release publishes. The check runs only when `sha256sum` is on the `PATH`; without it (macOS ships
`shasum` instead) the installer prints a notice and continues, and `--quiet` suppresses that notice.
It does not verify the minisign signature, because the installer has no minisign to run. The
installer does not check its own signature either: piped to `sh`, nothing on your machine verifies
it, so its trust is the HTTPS connection that delivered it. The installer does have a detached
signature, `fontaine-installer.sh.minisig` beside it on the `dev` release (dev key) and on each
tagged release (release key); if you download the script instead of piping it, you can verify it by
hand, and the signed trusted comment binds the channel, file, version and commit.
To check an archive's signature, download it and follow [Verifying releases](verifying-releases.md).

### Proxies and certificates

The installer is a shell script that runs `curl` (or `wget`). It ignores Fontaine's `[network]`
settings for proxy, CA certificates and mutual TLS. Behind a proxy set `https_proxy`, and for a
private CA set `SSL_CERT_FILE`, in the environment of that `curl`; or download the archive in a
browser and install it manually.

### Overwrite behaviour

The installer and `cargo install --git` both write `$CARGO_HOME/bin/fontaine`. The installer
replaces a cargo-installed binary. `cargo install` refuses to replace a binary it did not install,
so after the installer add `--force`:

```
cargo install --git https://github.com/madesroches/fontaine fontaine --force
```

### Uninstalling

Remove:

- `$CARGO_HOME/bin/fontaine` (`~/.cargo/bin/fontaine` by default);
- the receipt, `${XDG_CONFIG_HOME:-~/.config}/fontaine/fontaine-receipt.json`;
- `~/.config/fish/conf.d/fontaine.env.fish`, the one shell file specific to Fontaine.

Do not delete `~/.config/fontaine` as a whole; it is also Fontaine's configuration directory.

`$CARGO_HOME/env`, `$CARGO_HOME/env.fish` and the `. "$HOME/.cargo/env"` lines in your rc files are
shared with rustup. Remove them only if neither rustup nor cargo is installed; otherwise you take
`cargo` and `rustc` off your `PATH`.

## The release archive

Each dev build is a `dev-<n>` [prerelease](https://github.com/madesroches/fontaine/releases) with one
archive per platform: `fontaine-<target>.tar.xz`, or `.zip` on Windows. (The `dev` release itself
holds only the installer.) Unpack it and put `fontaine` on your
`PATH`; it also holds the license, readme and changelog. This is the path for Windows. Each archive
has a detached signature; see [Verifying releases](verifying-releases.md). A manual install writes
no receipt.

## From source

With the Rust toolchain:

```
cargo install --git https://github.com/madesroches/fontaine fontaine
```

Fontaine is not published to crates.io. A source build has the PromptGuard 2 tool-result scanner
only with `--features pg2`; see [Scanning tool results](guardian.md#scanning-tool-results). A source
install writes no receipt.
