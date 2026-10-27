# Guardian Picker Live Listing Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/151

## Overview

`/guardian-model` with no argument opens no dialog when no account lists `models = [...]` in
settings, because the picker is built from the synchronous `guardian_candidates()`. Build it the
way `/model` is built instead: from a live `model_listings(None)`, so every account appears with
its provider's models, and an account that cannot be listed shows why.

## Current State

- `open_guardian_picker` (`rust/fontaine-tui/src/ui/commands/model.rs:101`) groups
  `runtime.guardian_candidates()` by account. With no candidates it prints the header as a notice
  and returns (`model.rs:120`), so no overlay opens.
- `guardian_candidates()` (`rust/fontaine-core/src/agent/models.rs:319`) →
  `Switcher::candidates` (`rust/fontaine-core/src/models.rs:823`) is synchronous: the
  benchmark-validated table (shipped empty) plus explicit `models` lists. It is also used by the
  unavailable-guardian prompt (`ui/mod.rs:108`), which cannot await.
- `open_model_picker` (`model.rs:37`) shows `listing models…`, awaits `runtime.model_listings`,
  and builds a `Group` per account, failures included (`Group::from(&AccountListing)`).
- `open_guardian_picker` is called from `guardian_model` (sync, `model.rs:71`) and from
  `open_config_picker` (async, `ui/commands/config.rs:39`). `apply_pick` is already async.
- `set_guardian_model` → `build_guardian` rejects a model the account's explicit or main-account
  listing does not offer, and returns an unavailable guardian for a missing credential.
- `docs/tui.md` (~line 319 and ~403) describes the guardian picker as "only the models the
  guardian could use".

## Design

Make the guardian picker async and list live, reusing the `/model` listing path:

- `open_guardian_picker(&mut self, runtime: &mut AgentRuntime)` becomes `async` (its parameter
  changes from `&AgentRuntime` to `&mut AgentRuntime`, since `model_listings` takes `&mut self`;
  `open_config_picker` already holds `&mut`). It shows
  `listing models…`, calls `runtime.model_listings(None)`, and maps each `AccountListing` to a
  `Group` with `Group::from`. An error from `model_listings` is reported as `/guardian-model: …`
  with `error_chain::chain`, as `/model` does.
- Every listed model is shown, not only guardian-capable ones; `set_guardian_model` already
  rejects what cannot be a guardian and the error reaches the transcript. A failed listing keeps
  its heading with the reason, so an account is never silently dropped.
- The header keeps the current text (`guardian <label>` plus `(unavailable: …)`), and `current`
  stays parsed from `guardian.label()`.
- `guardian_model` and its caller in `ui/mod.rs:301` become async/awaited; `open_config_picker`
  awaits the new function. `apply_pick` (`model.rs:145`) also calls `guardian_model` for
  `PickerPurpose::Guardian`; it is already async and awaits it.
- Shared body: factor the picker-opening tail common to both pickers only if it is a clean
  extraction; otherwise leave the two functions separate (they differ in purpose, header, current).
- `guardian_candidates()` is unchanged and stays for the unavailable-guardian prompt.

## Implementation Steps

1. `fontaine-tui/src/ui/commands/model.rs`: make `guardian_model` and `open_guardian_picker`
   async, rewrite the latter over `model_listings(None)` as above; drop the now-unused
   `BTreeMap` grouping. Await `guardian_model` in `apply_pick` (`model.rs:145`).
2. `fontaine-tui/src/ui/mod.rs`: await `self.guardian_model(...)`.
3. `fontaine-tui/src/ui/commands/config.rs`: await `open_guardian_picker`.
4. Tests (below).
5. Docs and changelog: the `CHANGELOG.md` entry goes under `## [Unreleased]` → `### Fixed`,
   suffixed `(#151)`.

## Files to Modify

- `rust/fontaine-tui/src/ui/commands/model.rs`
- `rust/fontaine-tui/src/ui/mod.rs`
- `rust/fontaine-tui/src/ui/commands/config.rs`
- `rust/fontaine-tui/tests/tui.rs`
- `docs/tui.md`
- `CHANGELOG.md`

## Trade-offs

- List first and filter to guardian-capable models: rejected. There is no synchronous or cheap
  test for "can host a guardian" beyond what `set_guardian_model` already does, and a filter would
  recreate the silent-drop bug the issue reports.
- Merge `guardian_candidates()` into the listing as a "recommended" group: rejected as extra UI
  for a table that ships empty.

## Decisions

- 2026-10-27 Show every listed model and let `set_guardian_model` reject unsuitable picks, matching
  the issue's first fix option.

## Documentation

`docs/tui.md`: the `/guardian-model` row and the paragraph under "Choosing a model" describe the
picker as limited to guardian-eligible models; say instead that it opens the same overlay as
`/model` over a live listing, and that a pick the account cannot serve is rejected with the reason.

## Testing Strategy

Automated, in `fontaine-tui/tests/tui.rs` (the bug that happened in the wild, so a regression
test through the real TUI driver):

- Add `guardian_model_lists_accounts_without_a_models_setting`: a world whose account has no
  `models = [...]` and whose listing comes from a `wiremock` models endpoint; `/guardian-model`
  must show `type to filter` and the listed model, and Enter must save `[guardian] model` to the
  user config. Pins the issue's bug. Include a second, broken account in the world; the `could not
  list` row itself is already covered for `/model` by
  `a_failed_account_keeps_its_heading_and_only_failures_are_listed_again` via the shared
  `Group::from`.
- Existing `guardian_model_pins_the_guardian_and_saves_the_choice` and
  `picking_the_current_guardian_saves_it` keep passing (explicit `models` still listed, `(current)`
  marker preserved).

## Open Questions

None.
