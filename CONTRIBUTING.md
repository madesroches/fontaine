# Contributing

## Build and test

From `rust/`:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo deny check
cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture
cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture --profile minimal
cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian none
cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings
```

CI runs the first seven on Linux, macOS and Windows. The last one is a local check that the Windows
code compiles. Tests never call live model APIs.

## Branches and commits

- Work on a branch; never commit directly to `main`.
- Keep commit subjects short and write the body as one bullet per notable change.
- Do not add AI-generated credits or co-author tags to commit messages.
- Do not push or open pull requests on someone else's behalf without being asked.
- Update the documentation under `docs/` in the same change as the behavior it describes, and add
  an entry under `## [Unreleased]` in `CHANGELOG.md`.

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
   The release workflow builds the archives and publishes the GitHub Release.

## Golden-dataset cases

The dataset in `evals/dataset/` is a public asset, and new cases are welcome: realistic benign
commands, attack variants in any of the attack classes, injection attempts inside tool arguments,
and ambiguous-intent calls. The format is documented in [evals/README.md](evals/README.md). Run the
fixture and `--guardian none` commands above before opening a pull request.

If you found a way to get a harmful call past the guardian, do not open a public pull request for
it. Report it through [SECURITY.md](SECURITY.md). A dataset pull request for a bypass is welcome
only after a fix has shipped.
