# Anthropic Context Editing Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/629

## Overview
Long sessions resend every old tool result on each request. Anthropic's server-side context
editing (`clear_tool_uses_20250919`, beta `context-management-2025-06-27`) clears old tool results
before the model reads the conversation, while the client history stays untouched. This plan adds
an opt-in `context_editing` setting to `anthropic` accounts that sends it on main-agent and
subagent turns. rig 0.46.0 already types and sends the option, so this is config, defaults and
wiring, not a wire change.

## Current State
- Fontaine never sets `context_management` (no match in `rust/`).
- rig-core 0.46.0 `AnthropicOptions::context_management(ContextManagement::new(Vec<Value>))`
  (`providers/anthropic/extension.rs`) serializes `context_management`; it does not add the `anthropic-beta` header (that needs
  `AnthropicConfig::with_beta`, which Fontaine calls nowhere); edits are raw JSON. rig-agent's `AgentBuilder::provider_option` and `AgentRunner::provider_option`
  attach typed provider options.
- `rust/fontaine-core/src/providers/anthropic.rs` `Connected` builds the completion model;
  `completion` serves the main agent and subagents, `guardian_completion` the guardian. Per-account
  knobs live on `Account` (`config.rs`, e.g. `prompt_caching`, `cache_ttl`), are gated by a
  `Provider::accepts_*` method and checked in `check_account` (`config.rs`). User scope is already
  enforced by the single `accounts` rule in `config_scope.rs`, so no code step is needed.
- `turn.rs` `drive_once` applies per-turn `GenerationOptions` (effort) on the runner.
- rig does not parse the response's `context_management.applied_edits` (no match in rig-core).
- Fallback on a host that rejects the beta is #169 (open); the 5m/1h cache-write split for the
  `anthropic` account is recorded (#631, closed).

## Design
### Config
`[accounts.<name>.context_editing]`, `anthropic` kind only, user scope only (like `cache_ttl`):

| key | meaning | default |
|---|---|---|
| `enabled` | send `context_management` | `false` |
| `trigger_tokens` | `trigger: {type: "input_tokens", value}` | `100000` |
| `keep` | `keep: {type: "tool_uses", value}`, newest tool uses kept | `3` |
| `clear_at_least_tokens` | `clear_at_least: {type: "input_tokens", value}`: skip a clearing that frees less, since each clearing invalidates the cached prefix | `20000` |
| `exclude_tools` | tool names whose results are never cleared | unset: `["board_show", "board_update", "memory", "todo_write"]` (state-carrying tools in `fontaine-tools/src/descriptor.rs`); `[]`: none. MCP entries use the full `mcp__<server>__<tool>` name |

`trigger` and `keep` are the API defaults; `clear_at_least_tokens = 20000` is Fontaine's choice.
`clear_tool_inputs` is left unset (API default: inputs kept). No `clear_thinking_20251015` edit is
ever sent.

Types: a `ContextEditing` struct on `Account` (`Option<ContextEditing>`; `exclude_tools` is `Option<Vec<String>>`, `deny_unknown_fields`,
`JsonSchema`), validated in `check_account`: an `Account` of any other kind that sets it is
rejected like the other `accepts_*` keys (new `Provider::accepts_context_editing`, true only for
`anthropic`); `trigger_tokens >= 1`, `keep >= 1` and `clear_at_least_tokens < trigger_tokens` (the last two are Fontaine guards); every
`exclude_tools` entry is non-empty.

### Wire
A pure function `context_editing::edit_json(&ContextEditing) -> serde_json::Value` builds the single
edit object; it is the unit-tested seam. `AccountClient` gains
`fn provider_options(&self) -> Option<ProviderOptions>` (default `None`); `anthropic::Connected`
returns `AnthropicOptions::default().context_management(ContextManagement::new(vec![edit]))` when
enabled. Because rig does not send the beta header, `connect` calls
`config.with_beta(CONTEXT_MANAGEMENT_BETA)` (rig's constant, rig-core `providers/anthropic/extension.rs`)
when enabled; that client config is shared by every completion from the account, so the
`anthropic-beta` header rides on all of them (main, subagents, guardian, side calls), harmless
without the body field. `finish_builder` (main agent) and `build_child_agent` (subagent) attach it with
`provider_option`/`provider_options`; only the main agent and subagents get `context_management`
in the body; `guardian_completion` and every side call never do.

## Implementation Steps
1. `rust/fontaine-core/src/config.rs`: `ContextEditing` struct, `Account.context_editing`,
   `Account::new`, `check_account` rules (the struct derives `JsonSchema`).
2. `providers/mod.rs`: `Provider::accepts_context_editing` (default false), `AccountClient::provider_options`, and a `ModelHandle` accessor forwarding it (`models.rs` keeps `client` private).
3. `providers/anthropic.rs`: accept the key; `connect` calls `with_beta(CONTEXT_MANAGEMENT_BETA)` (rig's constant, not a repeated literal) when enabled; `Connected` stores the edit and implements `provider_options`.
4. New `providers/context_editing.rs`: `edit_json`, defaults, exclude-tool default resolution.
5. `agent/build.rs` `finish_builder` (main agent) and `child_runner.rs` `build_child_agent` (subagent): attach the model's provider options; a child gets the option only through its own account.
6. Docs (below), `CHANGELOG.md`, and a Decisions line in the master plan.

## Files to Modify
- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-core/tests/account_session.rs`, `tests/providers.rs` (rejection test beside `prompt_caching_keys_belong_to_the_kinds_that_accept_them`), `tests/docs.rs`
- `rust/fontaine-core/src/providers/mod.rs`, `anthropic.rs` (+ new module), `models.rs`
- `rust/fontaine-core/src/agent/build.rs`, `rust/fontaine-core/src/child_runner.rs`
- `docs/configuration.md`, `docs/providers.md`, `docs/architecture.md` (cache section)
- `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions, Providers)

## Decisions
- No `clear_thinking_20251015` edit is ever sent (it invalidates the cache without `keep: "all"`).
- The guardian and side calls never carry `context_management`.
- Response `applied_edits` is not surfaced because rig does not parse it; follow-up once rig exposes it.
- Default off: net cache cost is unmeasured and the #169 fallback is not built.
- Raw JSON edit server-side rather than a client prune: client history stays intact.
- Per-account rather than `[model]`: it exists only on the Anthropic wire.

## Documentation
- `docs/configuration.md` `[accounts.<name>]` table: the `context_editing` keys, defaults, the cache
  invalidation caveat, user scope only.
- `docs/providers.md` (anthropic section): one paragraph and an example (the toml example must validate against the schema:
  `tests/docs.rs` parses every toml block in `docs/*.md`).
- `docs/architecture.md` cache section: a new entry saying clearing invalidates the cached prefix from
  the cleared point on.

## Testing Strategy
- Unit: `edit_json` for defaults, every key set, and `exclude_tools` empty (key omitted); config
  parsing and `deny_unknown_fields`; `check_account` rejections (non-anthropic kind, `keep = 0`,
  `clear_at_least_tokens >= trigger_tokens`, empty tool name); every default exclude name is found by
  `fontaine_tools::descriptor::find` (guards drift; not the live registry, since board tools register
  only in teams and memory only when enabled).
- Wiremock-backed provider test (in `account_session.rs`, using its existing helpers): drive the request
  through the built agent (or a `fontaine -p` run), not `completion.call` directly, since the option
  is attached at the agent builders. With `enabled`, the Messages request body carries
  `context_management.edits[0]` with the expected fields and the `anthropic-beta` header holds
  `context-management-2025-06-27` (the literal appears only here, as the expected value); disabled sends neither. Separately assert that
  `guardian_completion` sends no `context_management` in the body.
- Run `fontaine-evals cache-bench` once manually with it on and off for the measurement the
  issue asks for; record the result in the PR (not a merge gate).
