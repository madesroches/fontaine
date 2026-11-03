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
- Without a sanctioned way to list models, an agent greps `~/.config/fontaine/`, which the
  guardian rightly escalates.

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
- `render_text()` and `to_json()` live next to it, so the CLI and the tool print identical
  content; the CLI's `--format=json` emits `to_json()`, the tool returns `render_text()`.
- Listing-failure strings come from `with_causes` and may carry a URL; the build step passes
  them through `redact_url_credentials` (reuse an existing redactor if present in
  `fontaine-tools`, otherwise a small one beside the report) so userinfo/query tokens never reach
  output.

**CLI** (`fontaine/src/models_cmd.rs`): `fontaine models [<account>] [--format text|json]`.
Dispatched in `lib.rs` after `load_config`, before session setup (no model is resolved, so
`main`/`guardian` role marks come from config's configured main/guardian refs when set).
Builds a `Switcher` the same way session setup does (same account resolution and `[network]`),
calls `listings(None, only)`, builds the report, prints. An unknown account is a usage error;
a listing failure for an account prints its error chain and still exits 0 unless every
requested account failed (exit non-zero then, so scripts notice).

**Tool** (`fontaine-tools/src/list_models.rs`): `ListModelsTool { source: Arc<dyn ModelSource> }`
with args `{ account?: string }`. `ModelSource::report(account) -> Result<String, String>` is
implemented in `fontaine-core` over `AgentRuntime`'s shared switcher and cached listings, calling
the same `ModelReport::build`/`render_text`. Registered in `extension_tools` for main agent and
subagents (read-only, so the child toolset still takes the read-only spawn fast path).
Descriptor row: `list_models`, `RiskClass::ReadOnly`, `path_free: true`, `Destructive::Never`,
`Specifier::None`, no Claude name. It touches only user-scope accounts' listing endpoints
(project scope cannot define accounts), no workspace paths, so it earns the fast path by the
same rule as `skill`/`board_show`; the guardian is not bypassed in any other way.

## Implementation Steps
1. `fontaine-core/src/model_report.rs`: types, `build`, `render_text`, `to_json`, URL redaction;
   export from `lib.rs`.
2. `fontaine-tools/src/list_models.rs`: `ModelSource` trait + `ListModelsTool`; register module.
3. `fontaine-tools/src/descriptor.rs`: add the `list_models` row (both the static table and the
   mirrored test table).
4. `fontaine-core/src/agent/build.rs`: add `ListModelsTool` to `extension_tools`; implement
   `ModelSource` for a small adapter over the runtime's switcher and listings cache.
5. `fontaine-core/tests/choke_point.rs`: add `list_models` to the extras list.
6. `fontaine/src/cli.rs`: `Command::Models(ModelsArgs { account, format })`.
7. `fontaine/src/models_cmd.rs` + `lib.rs` dispatch.
8. Docs and changelog (below).

## Files to Modify
- new: `fontaine-core/src/model_report.rs`, `fontaine-tools/src/list_models.rs`,
  `fontaine/src/models_cmd.rs`
- `fontaine-core/src/lib.rs`, `fontaine-core/src/agent/build.rs`,
  `fontaine-core/tests/choke_point.rs`, `fontaine-tools/src/lib.rs`,
  `fontaine-tools/src/descriptor.rs`, `fontaine/src/cli.rs`, `fontaine/src/lib.rs`
- `docs/tools.md`, `docs/headless.md` (or `docs/configuration.md`), `docs/permissions.md`
  (fast-path list), `CHANGELOG.md`

## Trade-offs
- **Shared `ModelReport` vs. the tool shelling to the CLI:** a shell call is exactly what the
  issue wants gone; sharing the pure report keeps one implementation of rows/roles/redaction.
- **Tool vs. system-prompt listing:** a prompt listing goes stale mid-session (`/login`, account
  edits) and bloats context; a tool is fresh and costs nothing unless called.
- **ReadOnly + path_free vs. Arbitrary:** the tool reads no files and mutates nothing; listing
  contacts only user-configured provider endpoints through the same client the pickers use.

## Decisions
- 2026-11-02 Claude Code has no model-listing tool (only `/model`); `list_models` is a Fontaine
  addition, scoped to the models usable by configured accounts, never credentials or endpoints.
- 2026-11-02 Ollama `/api/show` `tools` capability is out of this change: it needs a per-model
  request the pickers do not make, so it would be a second listing path. Tracked as follow-up.
- 2026-11-02 Subagents receive the tool: it is read-only and takes the same fast path.

## Documentation
- `docs/tools.md`: the `list_models` tool and its output shape.
- `docs/headless.md`: `fontaine models [<account>] [--format=json]`, exit codes.
- `docs/permissions.md`: `list_models` among read-only path-free tools.
- `CHANGELOG.md`: one line covering the subcommand and the tool.

## Testing Strategy
- Unit (`model_report`): role marking for main/guardian, validated flag, Ollama `name:tag` ids
  kept intact, failed account keeps an error row, text and JSON golden output, URL credentials
  in an error string are redacted, nothing but the listed fields can appear in JSON.
- Unit (`list_models`): unknown account errors; args reject unknown fields; output equals the
  CLI's text for the same listings.
- Integration (`fontaine-core/tests/models.rs` style, wiremock providers): the CLI path and the
  tool path render identical content for the same config, including a timing-out account.
- Headless integration (`fontaine/tests`): `fontaine models` and `--format=json` against a
  wiremock account; unknown account exits with the usage code; all-failed exits non-zero.
- Descriptor table test and choke-point test cover the risk class; a permissions test asserts
  the tool takes the read-only fast path in `auto` and for a read-only subagent toolset.

## Open Questions
- Whether the CLI should accept `--format=text|json` (matching `auto-mode`) rather than the
  issue's `--json`: this plan follows the existing `--format` convention.
