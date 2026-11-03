# List Models (CLI and Tool) Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/254
**Also closes**: https://github.com/madesroches/fontaine/issues/393

## Overview
Add one report of the models each configured account offers, and surface it twice from the same
code: a `fontaine models [<account>]` subcommand for scripts and CI (#254), and a read-only
`list_models` tool an agent can call instead of grepping the config directory (#393). Both
render a single `ModelReport` built from `Switcher::listings`, so per-account timeouts and failure entries behave
as in the `/model` picker; Ollama `name:tag` matching comes from each provider's `lists`, and
credentials are excluded structurally rather than filtered.

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
AccountReport { account, kind /* kind.as_str() */, models: Result<Vec<ModelRow>, String> }
ModelRow { id, roles: Vec<Role> /* main | guardian */, validated_guardian: bool }
```
- `ModelReport::build(listings: &[AccountListing], main: Option<&ModelRef>, guardian:
  Option<&ModelRef>, validated: &[ModelEntry])` is pure (precedent `resolve_with`,
  `models.rs:535-543`): it marks roles and the `validated_guardian` flag. The CLI and the
  adapter pass `&validated_models()`; tests pass a fixed list. No config, endpoint, key or env
  value enters the type, so none can be printed.
- Matching: a role mark needs `ref.account == listing.account &&
  listing.kind.provider().lists(&[row_id], &ref.model)`, as the `/model` picker's `is_current`
  (`fontaine-tui/src/model_picker.rs:105-111`); Ollama's bare name matches `:latest`
  (`providers/ollama.rs:37-41`). `validated_guardian` needs a `validated` entry with
  `entry.kind == listing.kind.as_str()` (`models.rs:696`) that `lists` matches.
- `render_text()` and `to_json()` live next to it; the CLI's `--format=json` emits `to_json()`,
  the tool returns `render_text()`. JSON shape: per account `{account, kind, models: [{id,
  roles, validated_guardian}]}` or `{account, kind, error}`, built with a custom serializer or
  `json!` (`AccountKind` has no `Serialize`).
- Listing-failure strings carry no URL credentials: the transport already strips request URLs
  (`fontaine-core/src/transport.rs:41-53`, pinned by `a_real_transport_error_drops_the_request_url`).

**CLI** (`fontaine/src/models_cmd.rs`): `fontaine models [<account>] [--format text|json]`, with
`ModelsArgs { account: Option<String>, #[command(flatten)] format: FormatArgs }` (matching
auto-mode/plugin list). `--model` is a top-level non-global arg (`cli.rs:48-50`), so the form is
`fontaine --model <ref> models`, and `&cli` is passed to `models_cmd::run` as auto-mode does.
Dispatched in `lib.rs` after `load_config`, before session setup. Role marks follow the
auto-mode pattern: call `models::resolve(&ResolveParams{…, model_override: cli.model.as_deref(),
session_id: None})` (`auto_mode.rs:276-282, 489-495`) to get the main reference, main listing
and guardian (pinned `[guardian] model`, validated default, main fallback; `models.rs:556-585`).
When `resolve` fails (no main model, main listing fails, pinned guardian unknown), the CLI still
lists, with no role marks, and prints a warning.
Builds a `Switcher` the same way session setup does (same account resolution and `[network]`),
calls `switcher.listings(Some((&main_ref, &main_listing)), only)`, builds the report, prints. An unknown account is checked against
`config.accounts` before listing and exits 2 through `usage()` (`EXIT_CONFIG`,
`fontaine/src/lib.rs:69`); a listing failure for an account prints its error chain and still
exits 0 unless every requested account failed (exit 1 then, so scripts notice).

**Tool** (`fontaine-tools/src/list_models.rs`): `ListModelsTool { source: Arc<dyn ModelSource> }`
with args `{ account?: string }`. `ModelSource::report(account) -> BoxFuture<'_, Result<String,
String>>` (like `SkillSource`, `fontaine-tools/src/skill.rs:15-18`) is implemented in
`fontaine-core` by an adapter built in `Parts` holding the shared switcher (`p.setup.switcher:
Option<SharedSwitcher>`, cloned and not held across an await, as `switcher()` does at
`agent/models.rs:61`), the live main model and listing (`p.runner.context.main`, read under its lock at call
time) and the guardian ref taken at call time as `ModelRef::parse(p.state.guardian().label())`
(`None` when `"none"`). It calls `Switcher::listings` fresh rather than using the
runtime's listing cache, then the same `ModelReport::build`/`render_text`; the tool marks the
session's live roles. An unknown account returns an error naming the known accounts; when the switcher is `None`,
`report` returns `Err("models cannot be listed in this session")`.
Registered unconditionally in `extension_tools` for the main agent, and added to the static
`explore` and `worker` arrays (`fontaine-agents/src/registry.rs:170-174, 186-199`), which already
hold conditional tools such as `lsp` since `toolset()` intersects with the parent's tools, so
subagents receive it (read-only, so the child toolset still takes the read-only spawn fast
path).
Descriptor row: `list_models`, `RiskClass::ReadOnly`, `path_free: true`, `Destructive::Never`,
`Specifier::None`, no Claude name.

## Implementation Steps
1. `fontaine-core/src/model_report.rs`: types, `build`, `render_text`, `to_json`; export from
   `lib.rs`.
2. `fontaine-tools/src/list_models.rs`: `ModelSource` trait + `ListModelsTool`; register module.
3. `fontaine-tools/src/descriptor.rs`: add the `list_models` row (both the static table and the
   mirrored test table, whose pinned length goes 26 -> 27, `descriptor.rs:405`).
4. `fontaine-core/src/agent/build.rs`: add `ListModelsTool` to `extension_tools`; implement
   `ModelSource` for the adapter over `Parts` described above.
5. `fontaine-agents/src/registry.rs`: add `list_models` to the built-in `explore` and `worker`
   static tool arrays.
6. `fontaine-core/tests/choke_point.rs`: add `list_models` to the extras list, and add
   `ListModelsTool` with a stub `ModelSource` to `extension_asking` (:112-149).
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
- `docs/configuration.md#model-discovery`: `fontaine models [<account>] [--format=json]` (`fontaine --model <ref> models`
  to override the main model), exit codes 2 and 1; add the tool to the list at :633.
- `docs/permissions.md`: `list_models` among read-only path-free tools (:245).
- `CHANGELOG.md`: one line covering the subcommand and the tool.

## Testing Strategy
- Unit (`model_report`): role marking for main/guardian, validated flag, Ollama `name:tag` ids
  kept intact, `ollama/llama3` marks the `llama3:latest` row, failed account keeps an error row, text and JSON golden output, nothing but the
  listed fields can appear in JSON.
- Unit (`list_models`): unknown account errors; args reject unknown fields.
- Headless integration (`fontaine/tests`): the test writes a `user_config` account with no
  `models` list and mounts `GET /models` on the endpoint's server (or a 500 route for the
  all-failed case), since `Sandbox::compatible` sets `models=["scripted"]`
  (`fontaine/tests/common/mod.rs:603-607`); `fontaine models` and `--format=json` against it;
  unknown account exits 2; all-failed exits 1.
- Descriptor table test and choke-point test cover the risk class; a permissions test asserts
  the tool takes the read-only fast path in `auto` and for a read-only subagent toolset.
