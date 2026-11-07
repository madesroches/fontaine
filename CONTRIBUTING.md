# Contributing

## Build and test

From `rust/`:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo deny check
cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture
cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture --profile minimal
cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian none
cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings
```

CI runs `cargo fmt --check`, clippy, `cargo test`, rustdoc with warnings denied, `cargo deny check` and the `--guardian none` eval
on Linux, macOS and Windows; the two `fixture` eval runs, the "Release signing" step (the real minisign CLI and the
`.github/scripts/` signing scripts against disposable keys), plus clippy, tests and rustdoc with
`--features fontaine/pg2`, run on Linux only (macOS and Windows get a `cargo check --features
fontaine/pg2`). Pull requests that touch only `tasks/` or `AGENTS.md` skip the Rust
steps. The last command above is a local check that the Windows code compiles. Tests never call
live model APIs.

## Branches and commits

- Work on a branch; never commit directly to `main`.
- Keep commit subjects short and write the body as one bullet per notable change.
- Do not add AI-generated credits or co-author tags to commit messages.
- Do not push or open pull requests on someone else's behalf without being asked.
- Update the documentation under `docs/` in the same change as the behavior it describes, and add
  an entry under `## [Unreleased]` in `CHANGELOG.md` (see below).

## Changelog entries

Under `## [Unreleased]`, each Keep a Changelog type (`### Added`, `### Changed`, `### Fixed`, …) is
split into `####` subject sections named after the `area:` labels: `Agents`, `Config`, `Core`,
`Extensibility`, `Guardian`, `Headless`, `Integrations`, `MCP`, `Observability`, `Permissions`,
`Providers`, `Sessions`, `Tools` and `TUI`, plus `Build and release` (CI, toolchain, packaging),
`Evals` and `Research`. The subject sections are in alphabetical order, and one exists only while
it has entries.

- Pick the subject from the change's `area:` label; with several labels, take the one the user
  would look under first.
- Add the entry at the **end** of its subject section, not the top. Changes on different subjects
  then edit different parts of the file, so their branches rebase without a conflict.
- Create a missing subject section in its alphabetical place.
- Released sections keep the layout they shipped with.

## Adding a tool

- Add one row to `TOOLS` in `rust/fontaine-tools/src/descriptor.rs`: risk class, whether it names a
  path, how destructiveness is derived, what a rule specifier means, and the Claude Code names.
  The risk class lives only in that table, never on the tool.
- Write the tool's module and construct it: in `fontaine-tools/src/builtins.rs` for a built-in, or
  as an `ExtensionTool::new(...)` in `fontaine-core/src/agent/build.rs` when it needs session state.
- Add its inventory row to `docs/tools.md`. The choke-point and docs tests fail on a tool without a
  row or a docs entry.

## Releasing

Cutting a release is the maintainer's job, and pushing the tag is never done on someone else's
behalf.

1. Every release-readiness item in the release's tracking issue except cutting the tag itself is
   ticked. For 0.1.0 that is #80, mirrored in the master plan's `## Remaining work`.
2. Bump `[workspace.package] version` in `rust/Cargo.toml` if needed. For 0.1.0 it is already
   `0.1.0`.
3. In the release commit, rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD` and add a fresh,
   empty `## [Unreleased]` above it.
4. Install dist with the pinned installer the workflows use
   (`curl --proto '=https' --tlsv1.2 -LsSf https://github.com/axodotdev/cargo-dist/releases/download/v0.32.0/cargo-dist-installer.sh | sh`),
   the version matching `cargo-dist-version` in `rust/Cargo.toml`. From `rust/`, check the notes
   with
   `cp ../CHANGELOG.md CHANGELOG.md && dist plan --tag vX.Y.Z --output-format=json | jq .announcement_changelog`
   (non-null), then remove the copy.
5. Merge to `main`, then tag `vX.Y.Z` on the resulting merge commit on `main` and push the tag.
   The release workflow builds the archives, then pauses the `sign` job for your approval.
6. Approve the `release-signing` deployment in the run. `sign` signs every archive, `host` creates
   a draft release, `verify-draft` downloads the draft and verifies it, and `publish` makes it
   public. If anything fails the draft stays a draft and nothing is public; delete the draft and
   the tag before retrying.

### Signing keys and environments

Every archive is signed with a minisign key owned by Fontaine, one per channel: `release` for
tagged releases and `dev` for the latest CI-passing build of `main`. The public keys are checked in
under `rust/fontaine/keys/` and compiled into the verifier module Fontaine's update path will use
(Fontaine does not update itself yet); how users verify is in
[docs/verifying-releases.md](docs/verifying-releases.md). The private keys live only in two GitHub
Environments, which are configured in GitHub, not in the repository:

| Environment | Secrets | Deployment rule | Reviewers |
|---|---|---|---|
| `release-signing` | `MINISIGN_SECRET_KEY`, `MINISIGN_PASSWORD` (release key) | tags matching `v*` only | maintainer, required |
| `dev-signing` | same names (dev key) | branch `main` only | none |

Only the `sign` jobs name an environment, and the deployment rules refuse runs from any other ref,
pull requests included.

Until the keys are bootstrapped, the four files under `rust/fontaine/keys/` hold a placeholder
comment, every verification fails with "no key for this channel", and the signing jobs fail their
post-sign check, so nothing can be published.

**Bootstrap.** For each channel, with the stock minisign CLI, run
`minisign -G -p <channel>.pub -s <channel>.key`. Commit the `.pub` file over both
`rust/fontaine/keys/<channel>.pub` and `rust/fontaine/keys/<channel>.signing.pub`, put the key
file's content and its password into that channel's environment secrets, then move the secret key
to an encrypted offline backup and delete the working copy. The two channels must use different
keys. Never generate production keys in CI or with an agent.

**Rotation (per channel).** Generate the new pair. Commit the new public key to
`<channel>.pub` only, leaving the old key in `<channel>.signing.pub`. Release N (for `dev`, the
first build of a commit with the new `<channel>.pub`) is signed with the old key, checked against
`<channel>.signing.pub`, and embeds the new key. After N is published, swap the environment secret
and commit the new key to `<channel>.signing.pub`; builds started between the two steps fail their
post-sign check and publish nothing. N+1 and later are signed with the new key. Binaries older than
N cannot verify N+1, so they must update to N first or reinstall.

**Revocation and recovery (compromise or loss).** There is no online revocation list. Remove the
environment secret at once, publish a security advisory, generate a new pair, commit it to both
`<channel>.pub` and `<channel>.signing.pub`, put it in the environment secrets, and ship a release
signed with and embedding it. Installed binaries cannot verify that release, so users must reinstall
through the HTTPS installer or a manual download verified against the new key in the repository. A
dev-key compromise never affects `release`.

**Enabling the dev channel.** Create the `dev-signing` environment (deployment branch `main`, no
reviewers), bootstrap the dev key, then set the repository variable `FONTAINE_DEV_CHANNEL` to
`enabled`. Until it is set, `.github/workflows/dev.yml` skips every run and publishes nothing. A newer
build replaces a pending one, so intermediate builds may be skipped and the channel always
publishes the latest CI-passing build of `main`, as the prerelease `dev-<run number>`. Each run also
replaces `fontaine-installer.sh` and its `.minisig` on the long-lived `dev` prerelease, the stable
installer address, unless a newer `dev-<n>` is already published. Old dev prereleases are not
pruned automatically.

## Golden-dataset cases

The dataset in `evals/dataset/` is a public asset, and new cases are welcome: realistic benign
commands, attack variants in any of the attack classes, injection attempts inside tool arguments,
and ambiguous-intent calls. The format is documented in [evals/README.md](evals/README.md). Run the
fixture and `--guardian none` commands above before opening a pull request.

If you found a way to get a harmful call past the guardian, do not open a public pull request for
it. Report it through [SECURITY.md](SECURITY.md). A dataset pull request for a bypass is welcome
only after a fix has shipped.
