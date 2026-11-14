# Remove Plan Mode Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/638

## Overview

Remove the `plan` permission mode: the `/plan` command, the plan stop in the Shift+Tab cycle,
`plan` as a value of `--permission-mode` and `[permissions] default_mode`, the `plan_block` tier and
the child-agent plan posture. The maintainer does not use it, and `auto` with the guardian and the
deterministic floor covers "look but do not touch" well enough. Every mode is a surface in the
permission test matrix and in what subagents inherit, so removing one shrinks both. This is a
deliberate deviation from Claude Code, which has plan mode.

## Current State

`PermissionMode::Plan` (`rust/fontaine-core/src/config.rs:216`) is threaded through:

- **Permission pipeline** (`fontaine-core/src/permissions.rs`): the edit block at ~1159
  (`mode == Plan || plan_block` and `edit_class` → `Tier::PlanBlock` deny), the `agent` spawn arm at
  ~1491 (`Plan` allows at tier `spawn`), the mode match at ~1518 (`Plan => Verdict::Guardian`), the
  `plan_block` parameter of `evaluate` and `posture_of` (~974, 1106, 1963, 2072).
- **Tier** (`tier.rs`): `Tier::PlanBlock`, `Tier::ALL` is `[Tier; 22]`, `as_str` → `"plan_block"`.
  `afk_report.rs` (~123, ~1022) lists `PlanBlock` among the Medium-severity denials.
- **Agents** (`fontaine-agents/src/mode.rs`, `tree.rs`): `RootMode::Plan`, `Posture::plan_block`,
  `Posture::of_root`/`child`. With plan gone, `RootMode` has one variant (`Ladder`).
  `tree.rs` holds `set_root_mode` and a `RootModeSource` closure; `gate.rs:144` maps
  `PermissionMode` to `RootMode`.
- **Mode lists that simply name it**: `headless.rs` (1037, 1601), `agent/goal.rs:54`,
  `native_search.rs` (26, tests), `claude_compat/cli.rs:503` (`parse_mode`), `cli.rs` (`ModeArg::Plan`).
- **Config**: `config_scope.rs` (project scope already refuses `plan`; test at ~929 and ~524).
- **TUI**: `Command::Plan` and its `/plan` catalog row (`commands.rs`), `ui/mod.rs:1272`
  (the handler), `statusbar.rs` (`mode_label`, `CYCLE`, `mode_style`), `theme.rs` (`mode_plan` in six
  themes), `linear.rs:209`, `reports.rs:524` and `AfkSource::Plan` (`afk_report.rs:32`, set when `/plan`
  leaves AFK).
- **Tests** that exercise it: `fontaine-core/tests/{permissions,permissions_auto,agent_pipeline,
  audit_telemetry,hook_slot,permission_rules,choke_point,config,native_search,runtime_state,
  side_question,trusted_skills,afk}.rs`, `fontaine/tests/{exit_code,headless_smoke,pipeline_headless,
  claude_sdk_compat}.rs` plus the SDK fixtures under `tests/fixtures/claude_sdk/` and
  `tests/sdk_compat/{py,ts}/controls.*`, `fontaine-evals/tests/none_mode.rs`, and
  `fontaine-tui/{src/app.rs,src/ui/tests.rs,tests/{afk,extensions,tui}.rs}`.
- **Docs and schemas**: `docs/{afk,agents,checkpoints,configuration,guardian,headless,index,memory,
  permissions,terminal,tools,tui}.md`, `docs/schemas/headless-v1.schema.json` (`plan_block` tier,
  `plan` in the `permission_mode` enum), `tasks/design/permissions.md` (step 9 and the tier list) and
  the master plan's `## Decisions` lines that describe plan behavior.

`pr-run-mode = "plan"` in `rust/Cargo.toml` is the dist release tool's setting and is unrelated.

## Design

Delete the mode end to end; add nothing. The pieces that need a decision:

- **`PermissionMode`**: drop the `Plan` variant. serde then rejects `default_mode = "plan"` with its
  own "unknown variant `plan`, expected one of …" error, which names the key and the valid values;
  that is the clear failure the issue asks for (config errors already exit 2 with the chain
  printed). No special-case message.
- **CLI**: drop `ModeArg::Plan`; clap rejects `--permission-mode plan` and lists the accepted values.
- **Claude-compat** (`claude_compat/cli.rs::parse_mode`): `"plan"` falls to the `other` arm,
  `` `plan` is not a permission mode ``, for both `--permission-mode` and the SDK's
  `set_permission_mode`. Claude's SDK hosts may still send `plan`; they get an error, as they already
  do for `bypassPermissions`. Update the SDK fixtures and `controls.{py,mjs}` so `set_mode_plan`
  expects an error, or drop that step if the fixture is a recorded Claude baseline that must not be
  edited (check how the fixture is produced before touching it).
- **Pipeline**: delete the edit block, the `plan_block` argument (so `posture_of` returns only the
  mode) and the `Plan` arms; `Spawn` allows `Auto | Afk`, and the mode match loses `Plan`.
- **Agents**: with no root posture other than the ladder, `RootMode` and `Posture::plan_block`
  have no remaining meaning. Remove `RootMode`, `Posture::of_root`'s plan arm and `plan_block`; if
  `Posture` is then a one-field wrapper around `Ladder`, collapse it to `Ladder` and have
  `set_root_mode` take `Ladder`. Keep this refactor as small as the compiler allows: delete first,
  simplify only what is left empty.
- **`Tier::PlanBlock`**: remove it, drop it from `Tier::ALL` (now 21), `as_str`, the AFK severity
  list and the headless schema's tier enum. The audit log, ledger and `gate_decision` events of
  sessions written before this change may contain `"plan_block"`; before removing the variant,
  grep for every place a `Tier` is *read back* (it derives `Deserialize`). If a persisted record
  can be deserialized on resume or by a report command, keep a read-side shim
  (`#[serde(alias)]` onto the nearest variant is not right for a tier, so instead make the reader
  tolerant of an unknown tier) rather than breaking old sessions. If nothing reads tiers back, no shim.
- **`AfkSource::Plan`**: serialized as `"plan"` in the ledger. Remove the variant and add
  `#[serde(alias = "plan")]` to `Command`, so an old ledger still resumes. Check that the AFK
  events are read back before deciding the alias is needed; if they are not, no alias.
- **Resume**: grep how the session mode persists. If a resumed session can restore `plan`, it must
  fall back (to `manual`, the conservative choice) with a startup warning through `Approver::warn`.
- **TUI**: remove `Command::Plan`, its catalog row and handler, so `/plan` becomes an unknown
  command like any other. Remove the `⏸ plan mode on` label, the plan color from the theme struct and
  its six values, the linear-mode label and the `/plan` entry in the AFK report source labels. The
  cycle becomes manual, accept edits, auto, AFK.
- **Docs and Decisions**: rewrite each doc sentence that describes the current behavior without it;
  do not leave "plan mode was removed" notes in `docs/` (describe current behavior only). The history
  goes to CHANGELOG and the plan's Decisions.

## Implementation Steps

1. **Core types.** Remove `PermissionMode::Plan` (`config.rs`) and fix every exhaustive match the
   compiler reports. Remove `Tier::PlanBlock` (`tier.rs`, `afk_report.rs`) and `AfkSource::Plan`
   (with the `serde(alias)` if needed per above).
2. **Pipeline.** `permissions.rs`: delete the plan edit block, the `plan_block` plumbing and the
   `Plan` arms. `gate.rs`, `goal.rs`, `native_search.rs`: drop `Plan` from the mode lists.
3. **Agents.** `fontaine-agents/src/mode.rs` and `tree.rs`: remove the plan posture as designed
   above; update `tests/seam.rs` for the changed `RootMode`/`set_root_mode` signature.
4. **CLI and compat.** `cli.rs` (`ModeArg`), `claude_compat/cli.rs` (`parse_mode` and its test),
   `headless.rs` (the two mode lists).
5. **TUI.** `commands.rs`, `ui/mod.rs`, `statusbar.rs` (labels, `CYCLE`, style, tests), `theme.rs`,
   `linear.rs`, `reports.rs`, and the `/plan` tests in `app.rs`, `ui/tests.rs`.
6. **Tests.** Remove or rewrite every plan test listed under Current State. The mode matrices in
   `permissions.rs`/`permissions_auto.rs` (`MODES` arrays) lose `Plan`; the `(Plan, …)` rows go.
   `tui/tests/extensions.rs` uses `/plan do x` only as an example of a queued built-in command: move it
   to another built-in that takes a description. `exit_code.rs` `a_valid_settings_overlay_is_accepted`
   switches its example mode to `manual`; add a test next to it that `default_mode = "plan"` exits 2
   naming the key. The eval `none_mode.rs` `plan-write` case and `pipeline_headless.rs` plan row go.
7. **Schemas and fixtures.** Update `docs/schemas/headless-v1.schema.json` (tier and
   `permission_mode` enums) and the SDK `controls` fixtures/scripts as above; the headless schema
   test must stay green.
8. **Docs.** Update the pages listed under Current State, in each case by rewriting the sentence or
   row; `tui.md` loses the `/plan` row and the cycle text, `agents.md` loses the "`plan` is not on the
   ladder" paragraph, `memory.md` and `tools.md` lose the `plan` row of the mode table, `guardian.md`
   and `permissions.md` stop naming plan as a guardian mode. Update `tasks/design/permissions.md`
   (step 9 and the tier list) to current behavior.
9. **Decisions and changelog.** One dated line in the master plan's `## Decisions` (see below) at the
   end of the permissions subsection, and a CHANGELOG entry under unreleased: plan mode, `/plan`,
   `--permission-mode plan` and `default_mode = "plan"` are removed, and `plan_block` leaves the
   audit tiers.

## Files to Modify

See Current State: every file listed there, plus `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`
and `tasks/design/permissions.md`. Close the follow-on `AGENTS.md` mention check: `AGENTS.md` names
`plan` only as the tracking plan, not the mode, so it needs no edit.

## Trade-offs

- **Keep it (status quo).** Parity with Claude Code and a structural "never edits" guarantee, at the
  cost of a mode in every matrix. Rejected by the maintainer, who does not use it.
- **Hide it (keep the code, drop it from the cycle and the docs).** Leaves a mode nobody can reach
  and nobody tests honestly. Rejected: dead permission code is worse than no code.
- **Keep `plan` accepted but map it to `manual` for compatibility.** Silently changes meaning for a
  user or SDK host that asked for the edit block. Rejected: an explicit error is the safe failure.

## Decisions

- Removal is a deviation from Claude Code, recorded in the master plan as a dated line that
  supersedes the plan-mode lines in its permissions, AFK and TUI subsections (the `plan` child
  posture, the `default_mode` project-scope note, the carousel order, `/plan` leaving AFK and the
  screen-reader announcement of `/plan`). The older lines stay as history.
- A leftover `plan` in config, `--permission-mode` or an SDK `set_permission_mode` is an error, never
  a silent fallback.
- The headless v1 schema loses the `plan` and `plan_block` enum values without a version bump: no
  event can carry them any more, so a consumer written against the old enum sees a subset.

## Documentation

Every page listed under Current State and `tasks/design/permissions.md`; docs describe current
behavior only (no "removed in" notes), per the repo's docs rule.

## Testing Strategy

Everything here is unit or integration level; no live API and no manual step is needed.

- `cargo test` across the workspace after the deletions; the choke-point and tier-completeness tests
  (`Tier::ALL`) stay green with 21 tiers.
- `a_valid_settings_overlay_is_accepted` on `manual`; new tests that `default_mode = "plan"`, `--permission-mode plan`
  and the compat `parse_mode("plan")` fail with an error naming `plan`.
- `statusbar.rs` test that the cycle is manual → accept edits → auto → AFK → manual.
- If the ledger or audit readers needed a shim, a test that a record with `"plan"` / `"plan_block"`
  still loads.
- The permission matrices keep their remaining modes, so the deterministic-floor and edit-class
  coverage of `manual`, `acceptEdits`, `dontAsk`, `auto` and `afk` is unchanged.
- Gates from `rust/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings`, `cargo test`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`, and the two `fixture` eval runs from
  AGENTS.md.

## Open Questions

- Whether `Tier` records and `AfkSource` are read back from disk (decides the two read-side shims).
  The implementer settles this by grep before removing the variants; both answers are covered above.
- Whether `tests/fixtures/claude_sdk/controls.*` is a recorded Claude Code baseline (leave it and adjust
  the assertion) or a Fontaine-generated report (regenerate it). Settle by reading how the fixture is
  produced.
