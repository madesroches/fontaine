# List Models (CLI and Tool) Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/254
**Also closes**: https://github.com/madesroches/fontaine/issues/393

## Overview
Add one report of the models each configured account offers, and surface it twice from the same
code: a `fontaine models [<account>]` subcommand for scripts and CI (#254), and a read-only
`list_models` tool an agent can call instead of grepping the config directory (#393). Both
render a single `ModelReport` built from `Switcher::listings`, so Ollama `name:tag` handling,
per-account timeouts and failure entries behave as in the `/model` picker, and credentials are
excluded structurally rather than filtered.

## Current State
- `Switcher::listings(main, only)` (`rust/fontaine-core/src/models.rs`) returns
  `Vec<AccountListing { account, kind, models: Result<Vec<String>, String> }>`: explicit
  `models` lists and the main account's fetched listing cost no call; the rest are listed
  concurrently under `LISTING_TIMEOUT`; failures keep their entry with the error chain.
- `AgentRuntime::model_listings` (`fontaine-core/src/agent/models.rs`) caches listings for the
  TUI pickers.
- Shipped validated guardian ranking: `validated_models()` (`models.rs`), from
  `guardian_models.toml`.
- Subcommands live in `fontaine/src/cli.rs` (`Command::{AutoMode, Plugin}`), dispatched in
  `fontaine/src/lib.rs` (~line 257: plugin runs before config load; auto-mode after
  `load_config`). `ReportFormat::{Text, Json}` and `FormatArgs` already exist for report
  subcommands (`auto_mode.rs`).
- Tools are `rig` `Tool` impls registered in `extension_tools`
  (`fontaine-core/src/agent/build.rs`) with a source trait for session state (`SkillTool`'s
  `source`, `TodoTool`'s `store`); every tool needs a row in `fontaine-tools/src/descriptor.rs`
  (`TOOLS`, plus its mirrored table test) and is checked by `fontaine-core/tests/choke_point.rs`.

## Design
**Shared report** (new `fontaine-core/src/model_report.rs`, re-exported):
```
ModelReport { accounts: Vec<AccountReport> }
AccountReport { account, kind, models: Result<Vec<ModelRow>, String> }
ModelRow { id, roles: Vec<Role> /* main | guardian */, validated_guardian: bool }
```
- `ModelReport::build(listings: &[AccountListing], main: Option<&ModelRef>, guardian:
  Option<&ModelRef>)` is pure: it marks roles and the `validated_guardian` flag from
  `validated_models()`. No config, endpoint, key or env value enters the type, so none can be
  printed.
- `render_text()` and `to_json()` live next to it; the CLI's `--format=json` emits `to_json()`,
  the tool returns `render_text()`. The same `ModelReport::build` inputs give the same text.
- Listing-failure strings carry no URL credentials: the transport already strips request URLs
  (`fontaine-core/src/transport.rs:41-53`, pinned by `a_real_transport_error_drops_the_request_url`).

**CLI** (`fontaine/src/models_cmd.rs`): `fontaine models [<account>] [--format text|json]`, with
`ModelsArgs { account: Option<String>, #[command(flatten)] format: FormatArgs }` (matching
auto-mode/plugin list).
Dispatched in `lib.rs` after `load_config`, before session setup (no model is resolved). Role
marks: main is the configured main ref, with `--model` overriding it; guardian is the effective
guardian, `Switcher::guardian_default` (`models.rs:795`), falling back to main.
Builds a `Switcher` the same way session setup does (same account resolution and `[network]`),
calls `listings(None, only)`, builds the report, prints. An unknown account is checked against
`config.accounts` before listing and exits 2 through `usage()` (`EXIT_CONFIG`,
`fontaine/src/lib.rs:69`); a listing failure for an account prints its error chain and still
exits 0 unless every requested account failed (exit 1 then, so scripts notice).

**Tool** (`fontaine-tools/src/list_models.rs`): `ListModelsTool { source: Arc<dyn ModelSource> }`
with args `{ account?: string }`. `ModelSource::report(account) -> BoxFuture<'_, Result<String,
String>>` (like `SkillSource`, `fontaine-tools/src/skill.rs:15-18`) is implemented in
`fontaine-core` by an adapter built in `Parts` holding the shared switcher (`p.setup.switcher:
Option<SharedSwitcher>`, cloned and not held across an await, as `switcher()` does at
`agent/models.rs:61`), the live main model and listing (`p.runner.context.main`) and the live
guardian (`p.state.guardian()`). It calls `Switcher::listings` fresh rather than using the
runtime's listing cache, then the same `ModelReport::build`/`render_text`; the tool marks the
session's live roles. An unknown account returns an error naming the known accounts.
Registered in `extension_tools` for the main agent, and added to the built-in `explore` and
`worker` toolsets the way `enable_teams` adds `board_show` (`fontaine-agents/src/registry.rs`),
so subagents receive it (read-only, so the child toolset still takes the read-only spawn fast
path).
Descriptor row: `list_models`, `RiskClass::ReadOnly`, `path_free: true`, `Destructive::Never`,
`Specifier::None`, no Claude name.

## Implementation Steps
1. `fontaine-core/src/model_report.rs`: types, `build`, `render_text`, `to_json`; export from
   `lib.rs`.
2. `fontaine-tools/src/list_models.rs`: `ModelSource` trait + `ListModelsTool`; register module.
3. `fontaine-tools/src/descriptor.rs`: add the `list_models` row (both the static table and the
   mirrored test table).
4. `fontaine-core/src/agent/build.rs`: add `ListModelsTool` to `extension_tools`; implement
   `ModelSource` for the adapter over `Parts` described above.
5. `fontaine-agents/src/registry.rs`: add `list_models` to the built-in `explore` and `worker`
   tool lists, as `enable_teams` does for `board_show`.
6. `fontaine-core/tests/choke_point.rs`: add `list_models` to the extras list.
7. `fontaine/tests/headless_smoke.rs`: add `list_models` to the exact tool list (~:59-79).
8. `fontaine/src/cli.rs`: `Command::Models(ModelsArgs { account, format })`.
9. `fontaine/src/models_cmd.rs` + `lib.rs` dispatch.
10. Append dated lines at the end of `### Tools` under `## Decisions` in
    `tasks/fontaine_harness_plan.md`: `list_models` is a Fontaine-only tool, and its read-only fast
    path (it touches only user-scope accounts' listing endpoints, no agent-chosen host, no
    credentials in output).
11. Docs and changelog (below).

## Files to Modify
- new: `fontaine-core/src/model_report.rs`, `fontaine-tools/src/list_models.rs`,
  `fontaine/src/models_cmd.rs`
- `fontaine-core/src/lib.rs`, `fontaine-core/src/agent/build.rs`,
  `fontaine-core/tests/choke_point.rs`, `fontaine-tools/src/lib.rs`,
  `fontaine-tools/src/descriptor.rs`, `fontaine-agents/src/registry.rs`,
  `fontaine/src/cli.rs`, `fontaine/src/lib.rs`, `fontaine/tests/headless_smoke.rs`
- `tasks/fontaine_harness_plan.md`
- `docs/tools.md` (incl. :110), `docs/configuration.md` (incl. :633), `docs/permissions.md`
  (incl. :245), `CHANGELOG.md`

## Decisions
- 2026-11-02 Claude Code has no model-listing tool (only `/model`); `list_models` is a Fontaine
  addition, scoped to the models usable by configured accounts, never credentials or endpoints.
- 2026-11-02 Ollama `/api/show` `tools` capability is out of this change: it needs a per-model
  request the pickers do not make, so it would be a second listing path. Tracked as follow-up.
- 2026-11-02 The CLI takes `--format text|json` (matching `auto-mode`) instead of issue #254's
  `--json`.

## Documentation
- `docs/tools.md`: the `list_models` tool and its output shape; add it to the tool list (:110).
- `docs/configuration.md#model-discovery`: `fontaine models [<account>] [--format=json]`, exit
  codes 2 and 1; add the tool to the list at :633.
- `docs/permissions.md`: `list_models` among read-only path-free tools (:245).
- `CHANGELOG.md`: one line covering the subcommand and the tool.

## Testing Strategy
- Unit (`model_report`): role marking for main/guardian, validated flag, Ollama `name:tag` ids
  kept intact, failed account keeps an error row, text and JSON golden output, nothing but the
  listed fields can appear in JSON.
- Unit (`list_models`): unknown account errors; args reject unknown fields; the same
  `ModelReport::build` inputs give the same text.
- Headless integration (`fontaine/tests`): the test writes a `user_config` account with no
  `models` list and mounts `GET /models` on the endpoint's server (or a 500 route for the
  all-failed case), since `Sandbox::compatible` sets `models=["scripted"]`
  (`fontaine/tests/common/mod.rs:603-607`); `fontaine models` and `--format=json` against it;
  unknown account exits 2; all-failed exits 1.
- Descriptor table test and choke-point test cover the risk class; a permissions test asserts
  the tool takes the read-only fast path in `auto` and for a read-only subagent toolset.
