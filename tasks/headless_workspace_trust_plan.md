# Headless Workspace Trust Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/228

## Overview

Today a headless run (`fontaine -p`) loads project executables — project hooks, repository-defined
MCP servers, and `!cmd` commands in project skills — without asking, matching Claude Code's `-p`
(recorded in the master plan's Decisions, 2026-10-21). Only the TUI asks. This plan keeps that
default but closes the gap for users who want it closed: an opt-in, user-scope-only
`[headless] require_trust = true` setting makes a headless run skip project executables unless the
workspace was trusted in the TUI. It also records the actual loaded state (`project_executables`)
in the headless `init` event and the ledger's `session_start` record so "how many headless runs in
untrusted workspaces loaded project executables" is countable from data already collected — the
measure-first step the issue asks for.

## Current State

- The trust decision is centralized in `rust/fontaine/src/lib.rs:392-417`: `project_executables`
  starts `true`; only the TUI (`!cli.print`) asks the trust prompt
  (`fontaine_tui::trust_prompt::ask`) when `trust::is_trusted` is false, and "Don't trust" sets
  `project_executables = false`. Headless never asks.
- The single flag covers everything downstream: project hooks are skipped in
  `user_hooks::build_layer` (`rust/fontaine-core/src/user_hooks.rs:1810-1819`), repository MCP
  servers are filtered in `mcp_plan` (`rust/fontaine-core/src/agent/mcp.rs:259-291`), and project
  skill commands return "[command not run: this project's executables are not trusted]"
  (`rust/fontaine-core/src/skills.rs:193-194`). `SkillRuntime` and `Extensions` both take the flag
  (`rust/fontaine-core/src/agent/setup.rs:788-796`).
- The trust record is `<data dir>/trust.toml` (`rust/fontaine-core/src/trust.rs`).
- Measurement signals that already exist: the ledger `SessionStart` record carries
  `workspace_trust` (`rust/fontaine-core/src/ledger.rs:358`, written at
  `rust/fontaine-core/src/agent.rs:838-851`), and the headless `init` stream-json event carries
  `workspace_trust` (`rust/fontaine/src/headless.rs:485-535`). Neither records whether the run was
  headless nor whether project executables actually loaded — so the exposure the issue asks to
  measure is not countable today.
- Scope enforcement: a config key is refused from every non-user scope unless a row exists in
  `config_scope.rs` `RULES` (`Rule::user_only`, `Rule::user_or_cli`, …); `[headless]
  max_usage_wait_secs` is the closest existing pattern (`Rule::user_or_cli`, user file or CLI
  `--settings`).

## Design

1. **New config key `[headless] require_trust: bool`, default `false`.** Lives on the headless
   settings struct in `rust/fontaine-core/src/config.rs` alongside `max_usage_wait_secs`.
   Scope rule: `Rule::user_or_cli` in `config_scope.rs` — a repository must not decide what an
   unattended run loads (the whole point of the setting is the *user's* judgment about unattended
   runs; CI can opt in or out with `--settings`). A row in `config_catalog.rs` with
   `Applies::NextLaunch` keeps the catalog test green; the key is read at process start.

2. **Decision in `lib.rs`.** Where `project_executables` is set today
   (`rust/fontaine/src/lib.rs:392-417`), add the headless branch before the existing TUI branch:
   when `cli.print && config.headless.require_trust && !trust::is_trusted(&dirs.data,
   &config.workspace)`, set `project_executables = false` and print one stderr line, e.g.
   `fontaine: workspace is not trusted; project hooks, MCP servers and project skill commands are
   skipped ([headless] require_trust = true)` — headless warnings go to stderr, matching
   `print_warnings` at `lib.rs:456-458`. No downstream changes: the flag already gates hooks, MCP
   servers, skills, and extensions exactly as the TUI's "Don't trust" choice does.

3. **Measure the exposure.** Add the actual outcome to the two records that already carry
   `workspace_trust`, both additive:
   - Headless `init` event (`rust/fontaine/src/headless.rs:485-535`): add
     `project_executables: bool` from `runtime.project_executables()`.
   - Ledger `Record::SessionStart` (`rust/fontaine-core/src/ledger.rs:38-48`): add `headless: bool`
     and `project_executables: bool`, both `#[serde(default, skip_serializing_if)]`-style optional
     (following the existing `env_keys` pattern) so old ledgers parse unchanged. The writer at
     `rust/fontaine-core/src/agent.rs:838-851` passes `self.setup.headless` and
     `self.setup.project_executables`.
   With one release of data, `headless=true ∧ workspace_trust=untrusted ∧ project_executables=true`
   counts the runs the setting would change. `docs/schemas/headless-v1.schema.json` gains the new
   `init` field (schema stays v1; the addition is backward-compatible).

4. **Default unchanged.** With the setting off (the default), behavior is byte-for-byte today's:
   headless loads project executables without asking, as Claude Code's `-p` does. The setting is
   the deviation, and only for users who opt in.

## Implementation Steps

1. `rust/fontaine-core/src/config.rs`: add `require_trust: bool` (default `false`) to the headless
   settings struct; include it in defaults.
2. `rust/fontaine-core/src/config_scope.rs`: add the `Rule::user_or_cli` row + TOML snippet to the
   `RULES` table and its tests.
3. `rust/fontaine-core/src/config_catalog.rs`: add the `KeyRow` (`Applies::NextLaunch`).
4. `rust/fontaine/src/lib.rs`: the headless branch from Design step 2, with the stderr line.
5. `rust/fontaine/src/headless.rs`: add `project_executables` to the `init` event;
   `rust/fontaine/tests/headless_contract.rs` and the schema json.
6. `rust/fontaine-core/src/ledger.rs` + `rust/fontaine-core/src/agent.rs`: the two new
   `SessionStart` fields and their writer.
7. Tests (below).
8. Docs + master plan Decisions amendment (below).
9. `CHANGELOG.md`: add an entry.

## Files to Modify

- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-core/src/config_scope.rs`
- `rust/fontaine-core/src/config_catalog.rs`
- `rust/fontaine/src/lib.rs`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine-core/src/ledger.rs`
- `rust/fontaine-core/src/agent.rs`
- `docs/schemas/headless-v1.schema.json`
- `docs/headless.md`, `docs/hooks.md`, `docs/mcp.md`, `docs/configuration.md`
- `tasks/fontaine_harness_plan.md` (Decisions)
- `CHANGELOG.md`

## Trade-offs

- **Setting vs. changing the default.** Closing the gap by default would deviate from Claude Code
  (`-p` loads project executables unprompted) and would break CI setups that rely on project MCP
  servers in fresh containers with no trust record. The issue explicitly proposes opt-in; the
  default stays.
- **`user_or_cli` vs. also allowing project scope.** Project scope could only *tighten* (skipping
  its own hooks hurts no one else), but a repo opting itself out is self-defeating and the key is
  meaningless if a repo can flip it — so it is user-scope and CLI only, like
  `[headless] max_usage_wait_secs`.
- **Why the flag, not new gating logic.** Reusing the existing `project_executables` flag makes the
  headless path identical to the TUI's "Don't trust" path — one code path to test and audit,
  instead of a parallel headless-only gate.
- **Measurement in the ledger vs. new telemetry counters.** Telemetry is opt-in
  (`[telemetry] enabled` + endpoint), so a telemetry-only measure would undercount. The ledger and
  `init` event are written for every session, so the count is available from data that already
  exists locally.

## Decisions

- The default stays Claude Code parity: headless loads project executables without a prompt. Only
  the opt-in setting changes behavior, and the setting itself is the recorded deviation (master
  plan Decisions amendment, dated, amending the 2026-10-21 workspace-trust line).

## Documentation

- `docs/headless.md`: in the workspace-trust paragraph, document `[headless] require_trust = true`
  (headless skips project executables in an untrusted workspace) and the new `init` field.
- `docs/hooks.md` (~lines 319-334) and `docs/mcp.md` (~lines 225-234): the "A headless run loads
  them without asking" sentences gain "unless `[headless] require_trust = true` and the workspace
  is not trusted".
- `docs/configuration.md`: the `[headless]` key/scope table row and, where workspace trust is
  described, the new setting.
- `tasks/fontaine_harness_plan.md` `### Config`: one dated line amending the 2026-10-21
  workspace-trust entry — headless still loads project executables by default (Claude Code
  parity), and an opt-in user-scope `[headless] require_trust = true` makes headless skip them in
  an untrusted workspace (#228).

## Testing Strategy

All automated; nothing here needs a live service.

- Config unit tests (`config_scope.rs` test module): the new row satisfies
  `every_settings_leaf_has_a_row` / `every_row_matches_a_settings_node` / `each_column_follows_its_cells`,
  and the snippet parses; a project-scope `[headless] require_trust` is a scope error; default is
  `false` when unset.
- Integration, in `rust/fontaine/tests/` (extend `project_executables.rs`, which already builds
  headless runs with project servers and hooks):
  - with the setting on and an untrusted workspace, a project MCP server's tools are absent and a
    project hook does not run — mirroring the TUI "Don't trust" tests
    (`rust/fontaine-tui/tests/agents.rs:206-430`) headlessly;
  - with the setting on and a trusted workspace (trust recorded first), project executables load;
  - with the setting off (default) and an untrusted workspace, project executables load (the
    existing tests already pin this — keep them green);
  - the stderr skip line appears when the setting fires.
- Headless contract (`rust/fontaine/tests/headless_contract.rs`): the `init` event carries
  `project_executables` and it is `false` in the skipped case; schema json test green.
- Ledger (`rust/fontaine-core/tests/ledger.rs`): `SessionStart` round-trips the new fields; a
  synthetic pre-change `SessionStart` JSON without them still parses (serde defaults), pinning
  resume compatibility.

## Open Questions

- None blocking. The measurement answers "is the exposure worth a default change" with data after
  this ships; that follow-up decision is deliberately left to a later issue.
