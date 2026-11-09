# Installer Install Path Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/527

## Overview

The shell and PowerShell installers put `fontaine` in `$CARGO_HOME/bin` because
`[workspace.metadata.dist]` in `rust/Cargo.toml` sets no `install-path`. People without a Rust
toolchain get a `.cargo` directory and a user `Path` edit for tooling they do not have. Set
`install-path = "~/.fontaine/bin"` (a directory of our own, easy to uninstall, the Deno/Bun
convention) and make `fontaine update` find the binary under the layout the new receipt records.

## Current State

- `rust/Cargo.toml:120-134`: `[workspace.metadata.dist]` has `installers = ["shell", "powershell"]`
  and no `install-path`, so dist uses its `CARGO_HOME` layout.
- `rust/fontaine/src/update/receipt.rs` `parse()` reads `install_prefix` and `binaries[0]` and
  always computes `<install_prefix>/bin/<file>`, deliberately ignoring `install_layout` (test
  `exe_suffix_is_not_doubled_and_layout_is_ignored`). That is right for a cargo-home receipt
  (prefix `~/.cargo`) and wrong for a custom `install-path`, where dist writes a flat layout whose
  `install_prefix` is the directory the binary sits in. Unfixed, `fontaine update` would report the
  receipt as describing another binary (`OtherBinary`) and refuse to update.
- The fixtures in `receipt.rs` tests and `rust/fontaine/tests/update_cmd.rs` write
  `"install_layout": "flat"` with a `bin/` subdirectory, which does not match what dist emits.
- `.github/scripts/dev-installer.sh` and `windows-installer.sh` rewrite the generated installers
  for dev tags; they must keep working on installers with a custom install path.
- Docs that assume `$CARGO_HOME`: `README.md:22-38,57-58`, `docs/installing.md:26-30,58-76,96-101,
  132-147`, `CHANGELOG.md:40`.

## Design

1. **dist config.** Add `install-path = "~/.fontaine/bin"` to `[workspace.metadata.dist]`, with a
   one-line comment on why it is not `$CARGO_HOME`. A directory of our own is chosen over
   `~/.local/bin` because it is not shared with other tools, is trivial to remove, and works the
   same on Windows.
2. **Receipt parsing honors the layout.** `parse()` reads `install_layout`:
   `"flat"` → `<install_prefix>/<file>`; anything else, or absent (cargo-home and hierarchical
   receipts written by installs made before this change) → `<install_prefix>/bin/<file>`.
   Existing installs therefore keep updating in place. The "unknown means `bin/`" default is the
   backward-compatible one; it is not a guess for new receipts, which dist writes as `flat`.
3. **Fixtures match reality.** Test fixtures use a flat tree (binary directly in the prefix) for
   `flat` and keep a `bin/` tree for the legacy layout.
4. **Migration.** No automatic migration: an existing install in `$CARGO_HOME/bin` updates itself
   in place via its old receipt. Running the new installer installs a second copy under
   `~/.fontaine/bin` and overwrites the receipt, after which `fontaine update` updates the new copy.
   The docs tell users to delete the old `$CARGO_HOME/bin/fontaine` (and, on a machine with no Rust
   toolchain, the `.cargo` leftovers) and say which copy wins on `Path` order.
5. **`cargo install --git`.** Left as is; it still writes to `$CARGO_HOME/bin` and writes no
   receipt. The two locations no longer collide, so the "installer overwrites" note is replaced by
   a note that having both means whichever directory is first on `Path` runs, and only the
   installer copy can `fontaine update`.

## Implementation Steps

1. `rust/Cargo.toml`: add `install-path`.
2. Regenerate or hand-check the installers: run `dist plan` and `dist build --artifacts=global`
   (or `dist generate`) if available and inspect the produced `fontaine-installer.sh`/`.ps1` and the
   receipt template for the recorded `install_prefix`/`install_layout`. Adjust step 3 if dist
   records something other than `flat` for a plain directory path. Confirm `release.yml` needs no
   change (it is allow-dirty and hand-adapted) and that `dev-installer.sh`/`windows-installer.sh`
   still apply cleanly to the new installer text.
3. `rust/fontaine/src/update/receipt.rs`: layout-aware binary path; update the doc comment.
4. Update receipt unit tests and `update_cmd.rs` fixtures; add tests listed below.
5. Docs: `README.md`, `docs/installing.md` (install, overwrite and uninstall sections, both
   platforms; the uninstall `Path` text becomes "remove `~/.fontaine/bin` from your `Path`" with
   no rustup caveat), and `CHANGELOG.md` entry noting the default location change and the
   migration step.

## Files to Modify

- `rust/Cargo.toml`
- `rust/fontaine/src/update/receipt.rs`, `rust/fontaine/tests/update_cmd.rs`
- `README.md`, `docs/installing.md`, `CHANGELOG.md`
- `.github/scripts/*installer*.sh` only if the dist output requires it

## Trade-offs

- `~/.local/bin` (uv, Claude Code) is more likely already on `Path`, but it is shared, and
  uninstalling means deleting one file rather than a directory; `~/.fontaine/bin` is ours.
- Migrating old installs automatically (moving the binary, rewriting the receipt) would touch files
  the installer does not own; documenting the one-time cleanup is cheaper and safe.

## Decisions

- 2026-11-08: `install-path = "~/.fontaine/bin"`, chosen over `~/.local/bin` (see Trade-offs).

## Documentation

`README.md`, `docs/installing.md`, `CHANGELOG.md` as listed above. No plan Decisions-section entry
in the master plan is needed: Claude Code's own installer also uses a directory of its own.

## Testing Strategy

Unit tests in `receipt.rs`:
- a flat receipt resolves to `<prefix>/<file>` and `locate` accepts the running binary there;
- a cargo-home/hierarchical receipt and one with no `install_layout` still resolve to
  `<prefix>/bin/<file>` (pins the regression for installs made before this change);
- `.exe` suffix is not doubled under either layout.

`update_cmd.rs`: the end-to-end update runs against a flat tree and still against a legacy `bin/`
tree.

## Manual Verification

1. Run the generated installer against a scratch `HOME`: `HOME=$(mktemp -d) sh fontaine-installer.sh`
   (from a built `dist` artifact) and check the binary is at `$HOME/.fontaine/bin/fontaine` and the
   receipt has `install_layout` `flat`. Not automated because it needs a published release archive.

## Open Questions

- Does dist 0.32 record `install_layout = "flat"` and `install_prefix = <the directory>` for
  `install-path = "~/.fontaine/bin"`? Step 2 of Implementation settles it from the generated
  installer before the parser change is finalized.
