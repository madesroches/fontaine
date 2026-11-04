# Marketplace List Catalog Count Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/280

## Overview

`fontaine plugin marketplace list` prints a per-marketplace "N plugin(s)" that counts the plugins
*installed* from that marketplace, while `marketplace add` prints the size of the catalog. A freshly
added marketplace therefore reports `(3 plugin(s))` on add and `0 plugin(s)` on list. List will
show both numbers, labelled: `3 offered, 0 installed`. This also settles what the original
plugins plan's "plugin count" meant: the catalog size, with the installed count shown beside it.

## Current State

- `marketplace_list` (`rust/fontaine/src/plugin.rs:453`) counts `registry.plugin` entries whose
  `marketplace` equals the marketplace name and prints it as `{n} plugin(s)`.
- `marketplace_add` (`plugin.rs:447`) prints `catalog.plugins.len()` from `read_catalog`, which
  reads `.claude-plugin/marketplace.json` in a checkout (`plugin.rs:281`).
- A registered marketplace's pinned checkout lives at
  `plugins::marketplace_dir(config_dir, name)` (`fontaine-core/src/plugins.rs:299`).
- `docs/plugins.md:117` describes the command as "Name, url, pin, plugin count."
- Tests: `rust/fontaine/tests/plugin.rs:40` and `:305` only assert on the name and the
  `auto-update off` marker; nothing pins the count.

## Design

In `marketplace_list`, for each marketplace read its catalog with `read_catalog(&marketplace_dir)`
and print:

```
<name> — <url> — pin <short> — <offered> offered, <installed> installed[ — auto-update off]
```

A catalog that cannot be read (checkout deleted by hand, file corrupted) must not make `list`
fail or hide the other marketplaces, and must not be shown as `0 offered`, which would be
indistinguishable from an empty catalog. Print `catalog unreadable` in place of the offered count
(`catalog unreadable, <installed> installed`) and send the cause, rendered with
`fontaine_tools::error_chain::chain`, to stderr as a warning line; the command still exits 0.
The installed count is unchanged.

To keep the formatting unit-testable without a filesystem, factor the line out of the loop into a
small pure function taking the entry, `Option<usize>` offered, installed count and marker.

## Implementation Steps

1. `rust/fontaine/src/plugin.rs`: in `marketplace_list`, resolve the checkout with
   `plugins::marketplace_dir`, call `read_catalog`, and build the line through a new
   `marketplace_line` helper that renders both counts and the unreadable case. Report a read error
   on stderr and continue.
2. Unit tests for `marketplace_line` in `plugin.rs`'s test module (or the sibling tests file if one
   exists): offered and installed both shown; zero installed with nonzero offered (the reported
   bug); unreadable catalog; auto-update marker still appended.
3. `rust/fontaine/tests/plugin.rs`: extend the add-then-list test (`:40`) to assert the list line
   contains the catalog size and `0 installed` before install, and `1 installed` after
   `plugin install`. This pins the add/list disagreement that happened in the wild (#280).
4. `docs/plugins.md:117`: change the row to "Name, url, pin, plugins offered by the catalog and
   plugins installed from it."
5. `CHANGELOG.md`: add the entry the `pr` step expects.

## Files to Modify

- `rust/fontaine/src/plugin.rs`
- `rust/fontaine/tests/plugin.rs`
- `docs/plugins.md`
- `CHANGELOG.md`

## Trade-offs

- Offered only, installed only, or both: installed is already in `plugin list`, but the issue
  asks that add and list agree and that a fresh marketplace show what it offers; showing both
  costs one clause and removes the ambiguity.
- Reading the catalog on every list vs caching the count in `plugins.toml`: a cache adds a
  registry field (`deny_unknown_fields`) that can drift from the pin; the file is small and local,
  so read it.

## Decisions

- 2026-11-03: An unreadable catalog degrades to a warning plus `catalog unreadable` rather than
  failing the command, so one broken marketplace does not hide the rest.

## Documentation

`docs/plugins.md` command table row, as in step 4.

## Testing Strategy

- Unit tests on `marketplace_line` cover the formatting branches, including the unreadable case.
- The CLI integration test drives `fontaine plugin marketplace add/list/install` against the
  fixture marketplace and pins the #280 regression (count after add equals count in list, before
  any install). No live network or service is involved.

## Open Questions

None.
