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
  `Posture::of_root`/`child`. `tree.rs` holds `set_root_mode` and a `RootModeSource` closure
  (~598, 815, 821, and a test at ~2314-2327); `gate.rs:144` maps `PermissionMode` to `RootMode`;
  `tests/seam.rs:264` uses the signature.
- **Stale doc comment**: `fontaine-core/src/agent.rs:749` ("The permission mode now, which Shift+Tab,
  `/plan` and the guardian prompts can change"). Three more the compiler will not flag: the `mode.rs:1-3` module doc ("`plan` is off the
  ladder, a session-root posture that child agents inherit only as a ban on edits"), the `posture_of`
  doc in `permissions.rs` ~961-962 ("and whether edits are blocked for it") and the `Posture::child` doc
  ("under a `plan` ancestor never allowed to edit").
- **Mode lists that simply name it**: `headless.rs` (1037, 1601), `agent/goal.rs:54`,
  `native_search.rs` (26, tests), `claude_compat/cli.rs:503` (`parse_mode`), `cli.rs` (`ModeArg::Plan`).
- **Config**: `config_scope.rs` (project scope already refuses `plan`; test at ~929 and ~524, both on a raw `toml::Value` so they still pass but would test a value that is no longer a mode).
- **TUI**: `Command::Plan` and its `/plan` catalog row (`commands.rs`), `ui/mod.rs:1272`
  (the handler), `statusbar.rs` (`mode_label`, `CYCLE`, `mode_style`), `theme.rs` (`mode_plan` in six
  themes), `linear.rs:209`, `reports.rs:524` and `AfkSource::Plan` (`afk_report.rs:32`, set when `/plan`
  leaves AFK).
- **Tests** that exercise it: `fontaine-core/tests/{permissions,permissions_auto,agent_pipeline,
  audit_telemetry,hook_slot,permission_rules,choke_point,config,native_search,runtime_state,
  side_question,trusted_skills,afk}.rs`, `fontaine/tests/{exit_code,headless_smoke,pipeline_headless,
  claude_sdk_compat}.rs`, `fontaine-evals/tests/none_mode.rs`, and
  `fontaine-tui/{src/app.rs,src/ui/tests.rs,tests/{afk,extensions,tui}.rs}`.
- **Docs and schemas**: `docs/{afk,agents,checkpoints,configuration,guardian,headless,index,memory,
  permissions,terminal,tools,tui}.md`, `README.md:60` ("six modes (`manual`, `acceptEdits`, `plan`, ...)";
  the count becomes five), `docs/hooks.md:102` (the `permission_mode` values), `docs/evals.md:265`
  ("is plan-blocked in `plan`"), `evals/README.md:40` (mode list) and `evals/README.md:63`
  (`plan_block` tier), `tasks/design/config.md:119` (the "under a `plan` parent child edits are
  blocked" note), `AGENTS.md:66` (`auto`/`plan` allow the `agent` spawn),
  `docs/schemas/headless-v1.schema.json` (`plan_block` tier,
  `plan` in the `permission_mode` enum), `docs/architecture.md` (156 floor diagram `[plan: block edits]`,
  166 `auto / plan allow (spawn)`, 171 `manual / acceptEdits / plan ask`), `docs/tui.md:1327` (`/plan` among
  the queued commands), `tasks/design/testing.md` (10 "plan-mode branch 9", 82 the `--guardian none`
  mode list), `tasks/design/permissions.md` (every plan mention: 38, 41, 52, 54, 90, 114-118, 127, 163,
  177-179, 225), `research/research-claude-code-parity.md` (the `plan` entry in the permission-modes
  row ~258 and the EnterPlanMode/ExitPlanMode "partial / backlog" row ~327, which points at the closed
  #24) and the master plan's `## Decisions` lines that describe plan behavior.

`pr-run-mode = "plan"` in `rust/Cargo.toml` is the dist release tool's setting and is unrelated.

## Design

Delete the mode end to end; add nothing. The pieces that need a decision:

- **`PermissionMode`**: drop the `Plan` variant. serde then rejects `default_mode = "plan"` with its
  own "unknown variant `plan`, expected one of …" error, which the loader reports as a `ConfigError::Parse`
  naming the file, line:column and the valid modes (the key and source line are deliberately left out
  of the message); that is the clear failure the issue asks for (config errors already exit 2 with the chain
  printed). No special-case message.
- **CLI**: drop `ModeArg::Plan`; clap rejects `--permission-mode plan` and lists the accepted values.
- **Claude-compat** (`claude_compat/cli.rs::parse_mode`): `"plan"` falls to the `other` arm,
  `` `plan` is not a permission mode ``, for both `--permission-mode` and the SDK's
  `set_permission_mode`. Claude's SDK hosts may still send `plan`; they get an error, as they already
  do for `bypassPermissions`. `tests/fixtures/claude_sdk/controls.*` is a recorded Claude Code baseline
  and `tests/sdk_compat/{py,ts}/controls.*` only record attempts: leave both unchanged and flip the
  `set_mode_plan` assertion in `claude_sdk_compat.rs`
  (`controls_change_mode_and_model_and_refuse_the_rest`, `report["set_mode_plan"].get("ok")`) to expect `error`.
- **Pipeline**: delete the edit block, the `plan_block` argument (so `posture_of` returns only the
  mode) and the `Plan` arms; `Spawn` allows `Auto | Afk`, and the mode match loses `Plan`.
- **Agents**: with no root posture other than the ladder, `RootMode` and `Posture::plan_block`
  have no remaining meaning. Remove `RootMode`, `Posture::of_root`'s plan arm and `plan_block`; keep
  `Posture { mode }`, so `set_root_mode` takes `Fn() -> Ladder`, `gate::root_mode` returns `Ladder`
  and the root posture is `Posture { mode }`. Keep this refactor as small as the compiler allows: delete first,
  simplify only what is left empty.
- **`Tier::PlanBlock`**: remove it, drop it from `Tier::ALL` (now 21), `as_str`, the AFK severity
  list and the headless schema's tier enum. The audit log, ledger and `gate_decision` events of
  sessions written before this change may contain `"plan_block"`, and both are read back: `Ledger::open` parses every line as `Entry` (`Action { tier: Option<Tier> }`)
  and fails on an unparseable line, and `AuditLog::read` deserializes the non-optional
  `DecisionRecord.tier` (used by `/bug` on a resumed session). Both readers need a shim for an old
  `"plan_block"`; put `#[serde(alias = "plan_block")]` on `Tier::DenyRule`, which covers both readers
  (`Option<Tier>` and `Tier` are the same enum); the alias only affects reading, so new records write
  `"deny_rule"`. The plan-block deny was a `DenyRule`-shaped deny and `severity_weight` already puts
  both tiers in the Medium arm, so old records read as Medium, as before.
- **`AfkSource::Plan`**: serialized as `"plan"` in the ledger, which is read back on resume. Remove
  the variant and add `#[serde(alias = "plan")]` to `Command`.
- **Resume**: a resume does not restore the mode, and `Record::ModeChange` stores plain strings, so
  no fallback or warning is needed.
- **TUI**: remove `Command::Plan`, its catalog row and handler, so `/plan` becomes an unknown
  command like any other. Remove the `⏸ plan mode on` label, the plan color from the theme struct and
  its six values, the linear-mode label and the `/plan` entry in the AFK report source labels. The
  cycle becomes manual, accept edits, auto, AFK.

## Implementation Steps

1. **Core types.** Remove `PermissionMode::Plan` (`config.rs`) and fix every exhaustive match the
   compiler reports. Remove `Tier::PlanBlock` (`tier.rs`, `afk_report.rs`; add
   `#[serde(alias = "plan_block")]` to `Tier::DenyRule`) and `AfkSource::Plan`
   (add `#[serde(alias = "plan")]` to `AfkSource::Command`).
2. **Pipeline.** `permissions.rs`: delete the plan edit block, the `plan_block` plumbing and the
   `Plan` arms. `gate.rs`, `goal.rs`, `native_search.rs`: drop `Plan` from the mode lists.
   `agent.rs:749`: drop `/plan` from the permission-mode doc comment; fix the `posture_of` doc in
   `permissions.rs` the same way.
3. **Agents.** `fontaine-agents/src/mode.rs` and `tree.rs`: remove the plan posture as designed
   above (including the `mode.rs` module doc and the `Posture::child` doc); update `tests/seam.rs`
   and the `tree.rs` test at ~2314-2327 for the new `set_root_mode` signature.
4. **CLI and compat.** `cli.rs` (`ModeArg`), `claude_compat/cli.rs` (`parse_mode` and its test),
   `headless.rs` (the two mode lists).
5. **TUI.** `commands.rs`, `ui/mod.rs`, `statusbar.rs` (labels, `CYCLE`, style, tests), `theme.rs`,
   `linear.rs`, `reports.rs`, and the `/plan` tests in `app.rs`, `ui/tests.rs`.
6. **Tests.** Remove or rewrite every plan test listed under Current State. The mode matrices lose `Plan`: the `MODES` arrays in
   `permissions.rs` and `audit_telemetry.rs` (now `[PermissionMode; 3]`) and the inline mode loops in
   `permissions_auto.rs`; the `(Plan, …)` rows go.
   `tui/tests/extensions.rs` `a_plan_command_queued_mid_turn_enters_plan_mode_and_sends_its_description`
   (~536) is deleted: it depends on `/plan` returning `Submission::Message`, which no other built-in does,
   and queued built-ins stay covered by `a_builtin_queued_mid_turn_runs_after_the_turn_in_typed_order`.
   `exit_code.rs` `a_valid_settings_overlay_is_accepted` switches its example mode to `manual`. In
   `fontaine-core/tests/config.rs` the `default_mode = "plan"` case (~959-979) in
   `cli_scope_accepts_everything_but_the_user_file_only_keys` switches to `acceptEdits`, and the CLI file's `plan` in
   `scalar_precedence_is_cli_then_project_local_then_project_then_user` (~95-102) switches to `auto`
   (assert `Some(PermissionMode::Auto)`; the user, project and local files keep `acceptEdits`, `dontAsk`
   and `manual`, so all four values stay distinct); a new small test there asserts `default_mode = "plan"` yields a `ConfigError::Parse` whose message
   contains `plan`, the line:col and the remaining mode names (as
   `tui_notify_accepts_its_four_values_and_names_them_on_error` in `config_scope.rs` does). In `config_scope.rs` swap the `:524` example to `default_mode = "acceptEdits"` and drop the
   `"plan"` assertion at `:929`. The eval `none_mode.rs` `plan-write` case and `pipeline_headless.rs` plan row go.
7. **Schemas and fixtures.** Update `docs/schemas/headless-v1.schema.json` (tier and
   `permission_mode` enums); the headless schema test must stay green. Flip the `set_mode_plan` assertion in
   `claude_sdk_compat.rs:547` as designed under Claude-compat.
8. **Docs.** Update the pages listed under Current State, in each case by rewriting the sentence or
   row; `tui.md` loses the `/plan` row and the cycle text, `agents.md` loses the "`plan` is not on the
   ladder" paragraph, `memory.md` and `tools.md` lose the `plan` row of the mode table, `guardian.md`
   and `permissions.md` stop naming plan as a guardian mode. Also rewrite the `README.md:60` mode list
   (six becomes five), `docs/hooks.md:102`, `docs/evals.md:265`, `evals/README.md:40` and `:63`, and
   `tasks/design/config.md:119`. Edit `AGENTS.md:66` to `auto`/`afk` (the spawn-allow wording in the
   fast-path invariant). Update `docs/architecture.md` (156, 166, 171), `docs/tui.md:1327` (drop `/plan`
   from the queued commands), `research/research-claude-code-parity.md` (drop `plan` from the modes row
   and mark the EnterPlanMode/ExitPlanMode row as removed: a recorded deviation, #638), `tasks/design/testing.md` (10, 82) and `tasks/design/permissions.md`
   (every plan mention, including step 9 and the tier list) to current behavior.
9. **Decisions and changelog.** One dated line in the master plan's `## Decisions` (see below) at the
   end of the permissions subsection, and a CHANGELOG entry under unreleased: plan mode, `/plan`,
   `--permission-mode plan` and `default_mode = "plan"` are removed, and `plan_block` leaves the
   audit tiers.

## Files to Modify

See Current State: every file listed there, plus `tasks/fontaine_harness_plan.md` and `CHANGELOG.md`.

## Trade-offs

- **Keep it (status quo).** Parity with Claude Code and a structural "never edits" guarantee, at the
  cost of a mode in every matrix. Rejected by the maintainer, who does not use it.
- **Hide it (keep the code, drop it from the cycle and the docs).** Leaves a mode nobody can reach
  and nobody tests honestly. Rejected: dead permission code is worse than no code.

## Decisions

- Removal is a deviation from Claude Code, recorded in the master plan as a dated line that
  supersedes the plan-mode lines in its subsections: `### Permissions` (plan following Claude Code's
  default, the spawn `auto`/`plan` allow, `/plan` leaving AFK, the carousel order and the `set_mode`
  line), `### Agents` (the `plan` child posture), `### Config` (the `default_mode` project-scope
  note), `### Guardian` (the `auto`/`plan` source-inspection line), `### Tools` (the "plan block"
  mention) and `### TUI` (the screen-reader announcement of `/plan`). The older lines stay as history.
- A leftover `plan` in config, `--permission-mode` or an SDK `set_permission_mode` is an error, never
  a silent fallback.
- The headless v1 schema loses the `plan` and `plan_block` enum values without a version bump: no
  event can carry them any more, so a consumer written against the old enum sees a subset.

## Documentation

See Implementation Steps 8.

## Testing Strategy

Everything here is unit or integration level; no live API and no manual step is needed.

- `cargo test` across the workspace after the deletions; the choke-point and tier-completeness tests
  (`Tier::ALL`) stay green with 21 tiers.
- `a_valid_settings_overlay_is_accepted` on `manual`; the two `tests/config.rs` tests that used `plan`
  move to `acceptEdits` and `auto`, and a new small test asserts `default_mode = "plan"` yields a `ConfigError::Parse` naming `plan` and the valid modes; new tests that `--permission-mode plan`
  and the compat `parse_mode("plan")` fail with an error naming `plan`.
- `statusbar.rs` test that the cycle is manual → accept edits → auto → AFK → manual.
- Shim tests: a ledger with `"plan"` (`AfkSource`) and a ledger line with tier `"plan_block"` still
  open (the tier reading back as `DenyRule`), and an audit log record with tier `"plan_block"` still
  reads.
- The permission matrices keep their remaining modes, so the deterministic-floor and edit-class
  coverage of `manual`, `acceptEdits`, `dontAsk`, `auto` and `afk` is unchanged.
- Gates from `rust/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings`, `cargo test`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`, and the two `fixture` eval runs from
  AGENTS.md.
