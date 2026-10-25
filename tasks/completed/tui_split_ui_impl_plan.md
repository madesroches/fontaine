# TUI Ui Impl Split Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/188

## Overview

`rust/fontaine-tui/src/lib.rs` is 2,027 lines, almost all of it one `impl<R: FnMut(&mut App)> Ui<R>`
block (lines 231–1686, ~75 methods). Nearly every TUI feature edits that block, so parallel
branches conflict there. This plan moves the methods into several `impl Ui<R>` blocks in a new
private `ui` module tree, grouped by concern. It is a mechanical move: no behavior change, no
public API change.

## Current State

- `lib.rs` holds the `pub mod` list (5–29), the `Prompt` enum and `ChannelApprover` (82–153),
  the turn types `TurnEnd` / `Serve` / `EndOfTurn` with `end_of_turn` (155–209),
  `queue_provider_request` (184), the `Ui` struct (211–229), the big impl (231–1686), the public
  `drive` (1686) and `run` (1827) with `TerminalGuard` (1778), and a `#[cfg(test)] mod tests`
  (1898–2027) that drives `Ui` and its private methods directly.
- `commands.rs` already owns the command table; its handlers are the methods dispatched from
  `Ui::submit` (`lib.rs:518`).
- All methods are private (`fn`). Items private to the crate root are visible to its descendant
  modules, so the `Ui` fields and the free helper types stay reachable from a `ui::*` tree.
  Methods are different: a private method in an `impl` inside module `m` is visible only inside
  `m`, so methods called across the new files need widening.

## Design

New private module `mod ui;` declared in `lib.rs`. The `Ui` struct and the other crate-root items
stay in `lib.rs`, as the issue asks. Method bodies move verbatim.

Layout and method assignment (approximate sizes, all under the ~600-line cap):

| File | Methods |
|---|---|
| `ui/mod.rs` — idle loop and input | `draw`, `refresh_panel`, `on_prompt`, `drain_prompts`, `cycle_mode`, `announce_auto`, `run`, `kill_agent`, `submit`, `remember`, `edit_queued`; declares the submodules and `#[cfg(test)] mod tests;` |
| `ui/turn.rs` — turn loop | `notification_turn`, `serve_while`, `expand_serving`, `converse`, `turn`, `show_delivered`, `on_event`, `finish_turn`, `settle_turn`; plus `TurnEnd`, `Serve`, `EndOfTurn`, `end_of_turn` moved from `lib.rs` (only the turn loop uses them) |
| `ui/commands/context.rs` | `clear`, `context`, `compact` |
| `ui/commands/model.rs` | `model`, `open_model_picker`, `guardian_model`, `open_guardian_picker`, `apply_pick`, `remember_model`, `finish_switch`, `retry_denied`, `answer_unavailable` (the unavailable-guardian dialog sits with the guardian model commands) |
| `ui/commands/appearance.rs` | `theme`, `output_style` |
| `ui/commands/workspace.rs` | `add_dir`, `shell` |
| `ui/commands/reports.rs` | `status`, `permissions`, `mcp`, `copy` |
| `ui/commands/mod.rs` | only the `mod` declarations |
| `ui/providers.rs` | `login`, `account_rows`, `queue_provider`, `tell_dialog`, `refresh_rows`, `provider_request`, `failed`, `add_account`, `credential_effects`, `store_key`, `finish_login`, `sign_in`, `sign_in_step`, `log_out`, `remove_account`; plus `queue_provider_request` from `lib.rs` |
| `ui/mcp.rs` | `start_mcp` (including its server-approval prompt handling) |
| `ui/tests.rs` | the existing `lib.rs` test module, unchanged apart from import paths |

`lib.rs` keeps the `pub mod` list, re-exports, `Prompt`, `ChannelApprover`, `ClipboardSink`, the
`Ui` struct, `drive`, `run`, `TerminalGuard` — about 400 lines.

Visibility: any item (method, fn, type) used from a different file (including `ui/tests.rs`, or
from `lib.rs`: `Ui::run` from `drive`) becomes `pub(crate)`; an item used only inside its own file
stays private. Examples: `settle_turn`, `end_of_turn`, `queue_provider_request`, `TurnEnd`,
`EndOfTurn` and `Serve` (used by `ui/tests.rs`; `ui/mod.rs` also uses `TurnEnd`). `ui/tests.rs`
imports `turn::*`, `providers::queue_provider_request` and the crate-root items explicitly, since
its `use super::*` now resolves to `ui/mod.rs`, not the crate root. `Ui` itself stays
private, so no rustdoc surface changes. Each new file carries only the `use` lines it needs
(clippy `-D warnings` rejects unused imports); the `lib.rs` import block shrinks to what remains.

`ui::commands` is a different path from the existing crate-root `commands` module (the command
table); the handlers keep calling the table through `crate::commands`.

## Implementation Steps

1. Create `ui/` and move methods file by file (one commit-sized step each: `turn`, `providers`,
   `mcp`, the `commands/*` files, then the remainder into `ui/mod.rs`), copying bodies and their
   doc comments verbatim and widening visibility (methods, fns, types) only where a cross-file
   caller exists, `ui/tests.rs` included.
2. Move `TurnEnd`, `Serve`, `EndOfTurn`, `end_of_turn` into `ui/turn.rs` and
   `queue_provider_request` into `ui/providers.rs`; move the test module to `ui/tests.rs`, widening the moved items it uses to `pub(crate)` and
   importing them explicitly.
3. Prune `lib.rs` imports; add `mod ui;`.
4. Check sizes: `wc -l rust/fontaine-tui/src/lib.rs rust/fontaine-tui/src/ui/*.rs
   rust/fontaine-tui/src/ui/commands/*.rs` — `lib.rs` under ~500, no new file over ~600.
5. `cargo fmt`, clippy for host and `x86_64-pc-windows-gnu`, `cargo test`.

## Files to Modify

- `rust/fontaine-tui/src/lib.rs` (shrinks)
- New: `rust/fontaine-tui/src/ui/{mod,turn,providers,mcp,tests}.rs`,
  `rust/fontaine-tui/src/ui/commands/{mod,context,model,appearance,workspace,reports}.rs`

## Trade-offs

- **Grouping the guardian dialog handlers with model commands** rather than a seventh file: each
  is small, and they share `finish_switch` / picker state with the model flows.
- **One level of `ui/` instead of promoting each concern to a crate-root module**: keeps the
  crate-root namespace (already ~25 modules) from growing and lets `Ui`'s private fields stay
  reachable as descendant modules.
- **Moving the turn types out of `lib.rs`**: needed to reach the size target, and only the turn
  loop touches them.

## Documentation

None. Internal layout only; no docs page describes `lib.rs`. `CHANGELOG.md` gets the usual
entry at PR time.

## Testing Strategy

No new tests: the change is a pure move. The existing coverage must pass unchanged —
`rust/fontaine-tui/tests/tui.rs`, the linear-mode tests, and the unit tests in the moved
`ui/tests.rs` (only their `use` paths may change). To confirm bodies were not altered, review the
diff with `git diff --color-moved=dimmed-zebra` (moved blocks should show as moved, with changes
only to visibility keywords and imports).

## Open Questions

None.
