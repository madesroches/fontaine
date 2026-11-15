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

## Design
Change the two version literals to the new release, then make the gates green.

## Implementation Steps
1. Install the new toolchain (`rustup toolchain install <ver>`).
2. Set `channel` in `rust/rust-toolchain.toml` and `rust-version` in `rust/Cargo.toml` to it.
3. From `rust/`, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`, and fix new lints minimally.
4. Run `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings`.
5. Run `cargo test` and `cargo deny check`.
6. Grep `.github/`, `docs/`, `CONTRIBUTING.md`, `README.md` for the old version string and
   update any hit (expected: none).
7. Add a `CHANGELOG.md` entry if the repo's convention calls for one.

## Files to Modify
- `rust/rust-toolchain.toml`
- `rust/Cargo.toml`
- Any source files needing lint fixes; `CHANGELOG.md`.

## Trade-offs
Keeping the exact-version pin (rather than `stable`) preserves reproducible CI, matching the
current convention.

## Testing Strategy
The existing gates (fmt, clippy on Linux and Windows targets, rustdoc, tests, deny) are the
verification; no new tests.
