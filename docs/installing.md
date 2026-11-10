# Installing Fontaine

Fontaine ships as a single `fontaine` binary. There are three ways to get it: a one-line installer
(a shell script for Linux and macOS, a PowerShell script for Windows), a release archive from GitHub
(every platform), and `cargo install` from source. No tagged release exists yet, so the installers
and the archives are dev builds of `main`.

## The installer (Linux and macOS)

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.sh | sh
```

This installs the latest CI-passing build of `main`. The installer is a file on the
long-lived `dev` prerelease, replaced after every successful dev publish; it downloads the archive
from the immutable `dev-<n>` prerelease of the build it was made for. Its receipt `version` is
`<version>-dev.<n>+<commit>`, so a dev install is recognisable by the `-dev.` in it. `fontaine --version` prints a different
string, `<version>-dev+<commit>`, because the binary computes it at build time: a release build of
the `v<version>` tag prints the plain version. It supports
Linux x86_64 and macOS on Apple silicon and Intel. There is no Linux arm64 build; use the archive
there. [Windows has its own installer](#the-installer-windows).

It needs `curl` or `wget`, and `tar`. It detects your platform, downloads the matching archive from
that `dev-<n>` release, and writes:

- the `fontaine` binary in `~/.fontaine/bin`;
- an install receipt at `${XDG_CONFIG_HOME:-~/.config}/fontaine/fontaine-receipt.json`, recording
  the install directory, the flat layout and the installed binary, so `fontaine update` can tell
  how Fontaine was installed;
- shell setup that puts `~/.fontaine/bin` on your `PATH`: `env` and `env.fish` scripts in
  `~/.fontaine/bin`, a line in your shell's rc files (`.profile`, `.bashrc`, `.bash_profile`,
  `.bash_login`, `.zshrc`, `.zshenv`) that sources `env`, and, for fish,
  `~/.config/fish/conf.d/fontaine.env.fish`. Set `FONTAINE_NO_MODIFY_PATH=1` to skip these edits;
  the installer also skips them when `~/.fontaine/bin` is already on your `PATH`.

The installer does not touch `$CARGO_HOME` and needs no Rust toolchain.

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

The installer writes `~/.fontaine/bin/fontaine` and replaces what is there. `cargo install --git`
writes `$CARGO_HOME/bin/fontaine` (`~/.cargo/bin` by default), so the two do not collide. If both
exist, whichever directory comes first on your `PATH` runs, and only the installer's copy can
`fontaine update`; delete the other to avoid confusion.

### Moving from an earlier installer

Earlier installers wrote `$CARGO_HOME/bin/fontaine`. That install keeps updating itself in place
through its old receipt. Running the current installer adds a second copy in `~/.fontaine/bin` and
overwrites the receipt, after which `fontaine update` updates the new copy. Then delete the old
`$CARGO_HOME/bin/fontaine` (`~/.cargo/bin/fontaine` by default), or the copy first on your `PATH`
keeps running. On a machine with no Rust toolchain, also delete the leftovers: `$CARGO_HOME/env`,
`$CARGO_HOME/env.fish`, the `. "$HOME/.cargo/env"` lines in your rc files and `~/.cargo` itself.
Keep them if rustup or cargo is installed.

### Uninstalling

Remove:

- `~/.fontaine/bin`, which holds the binary and the `env` and `env.fish` scripts;
- the receipt, `${XDG_CONFIG_HOME:-~/.config}/fontaine/fontaine-receipt.json`;
- `~/.config/fish/conf.d/fontaine.env.fish`;
- the lines in your shell's rc files that source `~/.fontaine/bin/env`.

Do not delete `~/.config/fontaine` as a whole; it is also Fontaine's configuration directory. Do not
delete `~/.fontaine` as a whole either; it may also hold home-level configuration and instructions
(`settings.toml`, `FONTAINE.md`, `rules/`, `skills/`, `agents/`). An
install made by an earlier installer is in `$CARGO_HOME/bin`; see
[Moving from an earlier installer](#moving-from-an-earlier-installer).

## The installer (Windows)

```
powershell -ExecutionPolicy Bypass -c "irm https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.ps1 | iex"
```

`-ExecutionPolicy Bypass` is needed because the default client policy, Restricted, rejects the
downloaded script. The command runs the same in PowerShell and in a Command Prompt. This installs the
latest CI-passing build of `main` on Windows x86_64, the one Windows target; there is no Windows
arm64 build. The script is also a file on the long-lived `dev` prerelease, and like the shell
installer it downloads the archive from the immutable `dev-<n>` prerelease of the build it was made
for, with a receipt `version` of `<version>-dev.<n>+<commit>`.

It needs Windows PowerShell 5.1 or PowerShell 7, both of which ship the commands it uses, and it
writes:

- `fontaine.exe` in `%USERPROFILE%\.fontaine\bin`;
- an install receipt at `%LOCALAPPDATA%\fontaine\fontaine-receipt.json`, or in
  `%XDG_CONFIG_HOME%\fontaine` when `XDG_CONFIG_HOME` is set, recording the install directory, the
  flat layout and the installed binary, so `fontaine update` can tell how Fontaine was installed;
- an entry for `%USERPROFILE%\.fontaine\bin` in your user `Path` (the registry key `HKCU\Environment`). Set
  `FONTAINE_NO_MODIFY_PATH=1` to skip it.

Open a new terminal, then run `fontaine --version`. Git for Windows is recommended but not required:
Fontaine runs commands through Git Bash when it finds it (a scoop install is discovered
automatically) and through PowerShell when it does not; see [Shell](tools.md#shell).

### Verification (Windows)

The installer checks the sha256 of the downloaded archive against the digest of the archive CI
published, which is written into the script, before anything is unpacked; a mismatch aborts the
install. dist's own PowerShell installer has no such check, so CI adds it and signs the script
afterwards. The check always runs. It does not verify the minisign signature, because there is no
minisign on your machine, and it does not verify itself: piped to `iex`, nothing on your machine
verifies the script, so its trust is the HTTPS connection that delivered it. The script has a
detached signature, `fontaine-installer.ps1.minisig` beside it on the `dev` release (dev key) and
on each tagged release (release key); to verify the script or an archive by hand, see
[Verifying releases](verifying-releases.md).

Archives downloaded in a browser can trigger Windows SmartScreen warnings: they carry a minisign
signature but are not Authenticode-signed; verify them as described in
[Verifying releases](verifying-releases.md).

### Proxies and certificates (Windows)

The installer ignores Fontaine's `[network]` settings for proxy, CA certificates and mutual TLS. It
honours the standard proxy environment variables such as `HTTPS_PROXY`; a private CA must be in the
Windows certificate store. Otherwise download the archive in a browser and install it manually.

### Overwrite behaviour (Windows)

The installer writes `%USERPROFILE%\.fontaine\bin\fontaine.exe` and replaces what is there.
`cargo install --git` writes `%CARGO_HOME%\bin\fontaine.exe`, so the two do not collide; whichever
directory comes first on your `Path` runs, and only the installer's copy can `fontaine update`.
An install made by an earlier installer is in `%CARGO_HOME%\bin` and keeps updating itself there;
running the current installer adds a copy under `.fontaine` and overwrites the receipt. Delete
`%CARGO_HOME%\bin\fontaine.exe` (`%USERPROFILE%\.cargo\bin\fontaine.exe` by default) afterwards,
together with any `fontaine.exe.old` and `fontaine.exe.old-*` beside it (left by `fontaine update`),
and with no Rust toolchain installed also the `%CARGO_HOME%\bin` entry in your user `Path` and the
`.cargo` directory.

### Uninstalling (Windows)

Remove:

- `%USERPROFILE%\.fontaine\bin`, which holds `fontaine.exe` and, while a session still runs from the
  old binary after `fontaine update`, `fontaine.exe.old` and any `fontaine.exe.old-*`;
- the receipt, `%LOCALAPPDATA%\fontaine\fontaine-receipt.json` (or the `%XDG_CONFIG_HOME%\fontaine`
  one).

Do not delete the `fontaine` directory as a whole; it is also Fontaine's configuration directory.
Likewise keep `%USERPROFILE%\.fontaine` itself; it may also hold home-level configuration and
instructions. Remove `%USERPROFILE%\.fontaine\bin` from your user `Path` (Settings, System, About, Advanced
system settings, Environment Variables).

## Updating

```
fontaine update
```

`fontaine update` replaces the installed binary with the newest build on the channel it was
installed from, prints one line and exits. It starts no session. The channel is not a setting: it
is read from the install receipt, where a version with `-dev.` in it (`<version>-dev.<n>+<commit>`)
is the `dev` channel and any other is `release`. An update never switches channels; to change
channel, run the other channel's installer.

- On `release` it installs the latest published `vX.Y.Z` release; on `dev` the highest published
  `dev-<n>` build, compared by build number.
- A build that is the same or older than the installed one is reported as already up to date, and
  nothing is installed: an update never downgrades.
- The archive and its `.minisig` are downloaded beside the installed binary and checked against the
  channel's compiled-in key before anything is replaced. The signed channel, file name and version
  must match the release that was offered. The `.sha256` files are not used. A failed check leaves
  the installed binary and receipt as they were; see [Verifying releases](verifying-releases.md).
- The binary is replaced atomically on Linux and macOS. On Windows the running `fontaine.exe` is
  renamed to `fontaine.exe.old` and the new one moved into place; the leftover is deleted by the
  next `fontaine update`.
- The receipt's `version` is rewritten to the installed build; its other fields are kept.
- The `[network]` settings (proxy, CA bundle, mutual TLS) apply to the download.

Only an install made by one of the installers above can update itself: the receipt must exist and
describe the running binary. A `cargo install` build, a package-manager copy, a container image, a
manually unpacked archive or a moved binary is refused with an installer command to use instead, and
nothing is written.

| Exit code | Meaning |
| --- | --- |
| 0 | Updated, or already up to date |
| 1 | The update could not be installed (for example the install directory is not writable, the disk is full, no published release exists on the channel, or the release has no archive for this platform) |
| 2 | The configuration could not be loaded |
| 3 | This install cannot update itself |
| 4 | The release list or the download could not be fetched (not when it was fetched and held no release); retrying may help |
| 5 | The download failed verification (missing or bad signature, wrong channel, file or version, or an archive larger than expected) |

## The release archive

Each dev build is a `dev-<n>` [prerelease](https://github.com/madesroches/fontaine/releases) with one
archive per platform: `fontaine-<target>.tar.xz`, or `.zip` on Windows. (The `dev` release itself
holds only the installers.) Unpack it and put `fontaine` on your
`PATH`; it also holds the license, readme and changelog. Use it where there is no installer. Each archive
has a detached signature; see [Verifying releases](verifying-releases.md). A manual install writes
no receipt.

## From source

With the Rust toolchain:

```
cargo install --git https://github.com/madesroches/fontaine fontaine
```

Fontaine is not published to crates.io. A source build has the PromptGuard 2 tool-result scanner
only with `--features pg2`; see [Scanning tool results](guardian.md#scanning-tool-results). A source
install writes no receipt. The checkout contains `.git`, so `fontaine --version` prints
`<version>-dev+<commit>`, with `.dirty` appended when tracked files have uncommitted changes; a
source tarball without `.git` prints the plain version.
