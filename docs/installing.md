# Installing Fontaine

Fontaine ships as a single `fontaine` binary. There are three ways to get it: the one-line installer
(Linux and macOS), the archive attached to a GitHub Release (every platform), and `cargo install`
from source.

## The installer

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/latest/download/fontaine-installer.sh | sh
```

The installer is attached to tagged releases, not to dev builds. It supports Linux x86_64 and macOS
on Apple silicon and Intel. There is no Linux arm64 build and no Windows installer; use the archive
there.

It needs `curl` or `wget`, and `tar`. It detects your platform, downloads the matching archive from
the release it belongs to, and writes:

- the `fontaine` binary in `$CARGO_HOME/bin` (`~/.cargo/bin` when `CARGO_HOME` is unset);
- an install receipt at `${XDG_CONFIG_HOME:-~/.config}/fontaine/fontaine-receipt.json`, recording
  the install prefix (`$CARGO_HOME`, or `~/.cargo`), the cargo-home layout and the installed
  binary, so `fontaine update` can tell an installer-managed binary from any other;
- shell setup that puts `$CARGO_HOME/bin` on your `PATH`: `$CARGO_HOME/env` and `env.fish`, a line
  in your shell's rc files that sources it, and, for fish,
  `~/.config/fish/conf.d/fontaine.env.fish`. Set `FONTAINE_NO_MODIFY_PATH=1` to skip these edits.

Open a new shell, then run `fontaine --version`.

### Verification

The installer checks the archive against the sha256 embedded in the script, the same checksum the
release publishes. The check runs only when `sha256sum` is on the `PATH`; without it (macOS ships
`shasum` instead) the installer prints a notice and continues, and `--quiet` suppresses that notice.
It does not verify the minisign signature, because the installer has no minisign to run and is
itself unsigned: its trust is the HTTPS connection that delivered it. To check a signature, download
the archive and follow [Verifying releases](verifying-releases.md).

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
- the receipt, `~/.config/fontaine/fontaine-receipt.json`;
- `~/.config/fish/conf.d/fontaine.env.fish`, the one shell file specific to Fontaine.

Do not delete `~/.config/fontaine` as a whole; it is also Fontaine's configuration directory.

`$CARGO_HOME/env`, `$CARGO_HOME/env.fish` and the `. "$HOME/.cargo/env"` lines in your rc files are
shared with rustup. Remove them only if neither rustup nor cargo is installed; otherwise you take
`cargo` and `rustc` off your `PATH`.

## The release archive

Each [GitHub Release](https://github.com/madesroches/fontaine/releases) has one archive per
platform: `fontaine-<target>.tar.xz`, or `.zip` on Windows. Unpack it and put `fontaine` on your
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
