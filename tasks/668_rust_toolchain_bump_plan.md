# Rust Toolchain Bump to Latest Stable Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/668

## Overview
Bump the pinned Rust toolchain from `1.99.1` to the latest stable release (`1.100.0` per
`static.rust-lang.org/dist/channel-rust-stable.toml`, dated 2026-11-12; re-check for a point
release when implementing) and fix whatever new lints or warnings it brings.

## Current State
- `rust/rust-toolchain.toml` pins `channel = "1.99.1"`.
- `rust/Cargo.toml:9` sets workspace `rust-version = "1.99.1"`.
- CI (`.github/workflows/ci.yml`, `dev.yml`, `pg2.yml`) runs `rustup toolchain install` with no
  version, so it follows the toolchain file; no version literal lives in `.github/`.
- `CONTRIBUTING.md` and `docs/installing.md` mention the toolchain only generically.
- `tasks/fontaine_harness_plan.md` lines ~212 and ~214 (workspace layout) carry the old version;
  line ~1157 is a dated historical Decisions entry and stays as is.
- Other crates use `rust-version.workspace = true`; `rust-toolchain.toml` also lists `components`
  and `targets`, which must be kept.

## Design
Change the two version literals to the new release, then make the gates green.

Cargo 1.100 places test executables under `target/<profile>/build/<pkg>/<hash>/out/` instead of
`target/<profile>/deps/`. `fontaine-testkit`'s `Layout::from_exe_with` now accepts both layouts
(the `out` form must sit three levels under a `build` dir, else it is rejected), and
`mcp_resources_headless` finds its example through `example_bin` rather than a path next to the
test exe.

## Implementation Steps
1. Install the new toolchain (`rustup toolchain install <ver>`).
2. Set `channel` in `rust/rust-toolchain.toml` and `rust-version` in `rust/Cargo.toml` to it.
3. From `rust/`, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`, and fix new lints minimally.
4. Run `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings`.
5. Run `cargo test` and `cargo deny check`.
6. Update the version in the workspace layout of `tasks/fontaine_harness_plan.md` (lines ~212,
   ~214) and add a dated line to its `## Decisions` > "Build and release" subsection, following
   the 1.97.1 -> 1.99.1 precedent. Grep `.github/`, `docs/`, `CONTRIBUTING.md`, `README.md`,
   `tasks/` (not `tasks/completed/`) for the old version string; expected hits are only the above.
7. No `CHANGELOG.md` entry: earlier toolchain bumps have none.

## Files to Modify
- `rust/rust-toolchain.toml`
- `rust/Cargo.toml`
- `tasks/fontaine_harness_plan.md`
- `rust/fontaine-testkit/src/lib.rs` and `rust/fontaine/tests/mcp_resources_headless.rs`: Cargo
  1.100 moved test executables to `build/<pkg>/<hash>/out/`, which broke layout detection and the
  example lookup.
- Any source files needing lint fixes.

## Trade-offs
Keeping the exact-version pin (rather than `stable`) preserves reproducible CI, matching the
current convention.

## Testing Strategy
The existing gates (fmt, clippy on Linux and Windows targets, rustdoc, tests, deny) are the
verification, plus new `fontaine-testkit` unit tests for the `build/<pkg>/<hash>/out` layout: no
triple, with triple, configured `CARGO_TARGET_DIR`, rejection when the third ancestor is not
`build`, and rejection of a too-shallow path.
