# Android Agent Combo Box Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/777

## Overview

The Android app lists agents as a wrapped bar of chips under the status line. With several agents it
takes several rows of a small screen. Replace the bar with a combo box (dropdown) next to the prompt
field: the collapsed control shows the agent on screen, and the full list shows only while the
dropdown is open. Selection semantics do not change.

## Current State

- `rust/fontaine-android/src/app.rs`: `App::chips()` builds `Vec<AgentChip>` (main first, then
  running/waiting, failed, rest via `rank`), empty while `layout::agents_bar_visible` is false (only
  main exists). `View.agents`, `View.header` (`AgentHeader`: name, status, model, spend) feed the UI.
  `App::view_agent(Option<&str>)` switches the view; unknown ids are refused.
- `rust/fontaine-android/src/android/ui.rs`: `session()` draws the status line, chips, `agents_bar`
  (one `styled_button` per chip, wrapped by `layout::chip_rows`, tap -> `Tap::ViewAgent`), then
  `agent_header`, the transcript child, the footer (dialog, todos, queued, `command_picker`,
  selection bar, `field_frame`, Send / Stop agent or Interrupt / Leave session). `state.footer_px`
  caches the measured footer height, so a footer that grows shrinks the transcript automatically.
- `layout.rs`: `agents_bar_visible`, `chip_rows` (used only by the bar; its tests cover it).
- `docs/android.md` (agents bar paragraph, ~lines 91-95) and the 746 plan
  (`tasks/completed/android_agent_switching_parity_plan.md`) describe the bar.
- TUI: `fontaine-tui/src/agent_list.rs` lists `main` and worth-looking-at agents below the status
  line; Enter views the selected row. The set of listed agents, their order and the view-on-select
  behaviour are what the app already mirrors; only the control changes.

## Design

Presentation-only change; no wire, host or `App` state-machine change.

- **Control**: ImGui `igBeginCombo` placed in the footer directly above the prompt field (after the
  command picker / selection bar), full width. Preview text = selected chip's label
  (`name status`), prefixed `! ` when any agent other than the selected one has a waiting dialog
  (so a pending approval is not hidden while collapsed). Hidden while `view.agents` is empty
  (unchanged rule).
- **Popup**: one `igSelectable` per chip in the existing order, label `[! ]name status`, the viewed
  one drawn selected. Choosing a different one pushes `Tap::ViewAgent(id)` (same tap as today);
  choosing the viewed one is a no-op. ImGui closes the popup on selection.
- **Label logic** moves into portable `layout.rs` as a pure function
  `agent_combo_preview(chips: &[AgentChip]) -> String` (or equivalent over plain fields) so it is
  unit tested on the host; `ui.rs` only draws.
- **Removed**: `agents_bar`, `layout::chip_rows` and its tests (dead after the change; no other
  caller). `agent_header` stays (model and spend), drawn where it is today.
- Footer height: `footer_px` is measured per frame, so one combo row replaces N wrapped chip rows
  above the transcript with no further layout work. The first frame uses the existing estimate
  (`row * 3.0`); add one row to that fallback when the combo shows.

## Implementation Steps

1. `layout.rs`: add the preview-label function and tests; delete `chip_rows` and its tests.
2. `android/ui.rs`: delete `agents_bar` and its call; add `agent_combo(agents, taps)` and call it
   in the footer before `field_frame`; adjust the `footer` fallback estimate.
3. `docs/android.md`: rewrite the agents-bar bullet as the dropdown above the message field.
4. `CHANGELOG.md`: user-visible entry for #777.
5. Run the gates from `rust/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test -p fontaine-android`; compile-check the Android layer if the target is installed
   (`cargo check -p fontaine-android --target aarch64-linux-android`).

## Files to Modify

- `rust/fontaine-android/src/layout.rs`
- `rust/fontaine-android/src/android/ui.rs`
- `docs/android.md`, `CHANGELOG.md`

## Trade-offs

- **Combo vs. keeping chips but collapsible**: a combo matches the issue and keeps one row
  regardless of agent count; a collapsible bar still needs its own toggle row and a second control.
- **Popup (`igBeginCombo`) vs. inline expanding list** (as `command_picker` does): an inline list
  would push the transcript up while open; a popup floats over it.
- **Keep `AgentChip` and `App::chips()` as is**: ordering, waiting badge and selection already
  tested in `app.rs`; only the drawing changes.

## Decisions

- AFK: the combo sits above the prompt field in the footer, not inside the same row as the field,
  because the field is a custom-drawn multi-line widget (`field_frame`) and the footer already stacks
  full-width controls; "near the prompt box" is met by adjacency.

## Documentation

- `docs/android.md`: describe the dropdown instead of the agents bar.
- `CHANGELOG.md`: entry.

## Testing Strategy

- `layout.rs` unit tests for the preview label: selected only; waiting badge when another agent
  waits and not when only the selected one does; status included; main selected.
- Existing `app.rs` tests (`chips` order, selected, waiting, `view_agent`) stay green unchanged.
- The ImGui drawing cannot be unit tested on the host; compile-checked for Android (see step 5).

## Manual Verification

1. Pair the APK with a session holding two or more subagents. Expected: no chip rows under the
   status line; a combo above the message field showing the viewed agent; tapping opens the list,
   choosing an agent switches the transcript; a pending approval from another agent shows `!` on the
   collapsed control. Not automated: needs a device and the ImGui renderer.

## Open Questions

- None the codebase cannot settle.
