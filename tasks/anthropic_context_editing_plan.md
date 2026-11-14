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
  (`providers/anthropic/extension.rs`) serializes `context_management` and adds the beta header;
  edits are raw JSON. rig-agent's `AgentBuilder::provider_option` and `AgentRunner::provider_option`
  attach typed provider options.
- `rust/fontaine-core/src/providers/anthropic.rs` `Connected` builds the completion model;
  `completion` serves the main agent and subagents, `guardian_completion` the guardian. Per-account
  knobs live on `Account` (`config.rs`, e.g. `prompt_caching`, `cache_ttl`), are gated by a
  `Provider::accepts_*` method and checked in `check_account` (`config.rs`), and are user scope only.
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
| `exclude_tools` | tool names whose results are never cleared | Fontaine's state-carrying tools (task list, memory), taken from the registry names |

The defaults are the API documentation's own example values. `clear_tool_inputs` is left unset (API
default: inputs kept). No `clear_thinking_20251015` edit is ever sent: without `keep: "all"` it
keeps one turn on every model and invalidates the cache, and server-side tool-result clearing never
invalidates thinking blocks on preserved-thinking models.

Types: a `ContextEditing` struct on `Account` (`Option<ContextEditing>`, `deny_unknown_fields`,
`JsonSchema`), validated in `check_account`: an `Account` of any other kind that sets it is
rejected like the other `accepts_*` keys (new `Provider::accepts_context_editing`, true only for
`anthropic`); `trigger_tokens`, `keep >= 1`, `clear_at_least_tokens < trigger_tokens`; every
`exclude_tools` entry is non-empty.

### Wire
A pure function `context_editing::edit_json(&ContextEditing) -> serde_json::Value` builds the single
edit object; it is the unit-tested seam. `AccountClient` gains
`fn provider_options(&self) -> Option<ProviderOptions>` (default `None`); `anthropic::Connected`
returns `AnthropicOptions::default().context_management(ContextManagement::new(vec![edit]))` when
enabled. The agent build for the main agent and subagents attaches it with
`provider_option`/`provider_options`; `guardian_completion` and every side call (side questions,
compaction summaries, scanner, goal checks) never get it: a guardian prompt is small and a cleared
result would change what it judges.

### Subagents and models
The option is read from the account of the model in use, so a mid-session `/model` switch to a
non-anthropic account sends nothing, and a child inherits it only through its own account. Nothing
about the guardian, floor or modes changes: the edit is on the model request, not on tool gating.

## Implementation Steps
1. `rust/fontaine-core/src/config.rs`: `ContextEditing` struct, `Account.context_editing`,
   `Account::new`, catalog/schema/known-keys lists, `check_account` rules.
2. `providers/mod.rs`: `Provider::accepts_context_editing` (default false), `AccountClient::provider_options`.
3. `providers/anthropic.rs`: accept the key; `Connected` stores the edit and implements `provider_options`.
4. New `providers/context_editing.rs` (or beside `anthropic.rs`): `edit_json`, defaults, exclude-tool default resolution.
5. `agent/build.rs` (main and subagent builders): attach the account's provider options.
6. `fontaine-core/src/config_catalog.rs` and the settings dialog/schema if keys are listed there.
7. Docs (below), `CHANGELOG.md`, and a Decisions line in the master plan.

## Files to Modify
- `rust/fontaine-core/src/config.rs`, `config_catalog.rs`
- `rust/fontaine-core/src/providers/mod.rs`, `anthropic.rs` (+ new module)
- `rust/fontaine-core/src/agent/build.rs`
- `docs/configuration.md`, `docs/providers.md`, `docs/architecture.md` (cache section)
- `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions, Providers)

## Trade-offs
- **Default off.** Net cache cost against saved input is unmeasured (the issue's "Not measured"),
  every clearing is a cache miss, and the retry-without fallback (#169) is not built, so a gateway
  that rejects the beta would fail turns. Same stance as `cache_ttl = "1h"`.
- **Raw JSON edit rather than a client-side prune.** The server keeps the client history intact, so
  `/rewind`, checkpoints and exports are unaffected; a client prune would mutate history (#161).
- **Per-account, not `[model]`.** The feature exists only on the Anthropic wire, like `cache_ttl`.

## Decisions
- No `clear_thinking_20251015` edit is ever sent (it invalidates the cache without `keep: "all"`).
- The guardian and side calls never carry `context_management`.

## Documentation
- `docs/configuration.md` `[accounts.<name>]` table: the `context_editing` keys, defaults, the cache
  invalidation caveat, user scope only.
- `docs/providers.md` (anthropic section): one paragraph and an example.
- `docs/architecture.md` cache section: clearing invalidates the cached prefix from the cleared point,
  so it is an accepted lost cache read at the trigger, alongside the boundaries already listed.

## Testing Strategy
- Unit: `edit_json` for defaults, every key set, and `exclude_tools` empty (key omitted); config
  parsing and `deny_unknown_fields`; `check_account` rejections (non-anthropic kind, `keep = 0`,
  `clear_at_least_tokens >= trigger_tokens`, empty tool name); every default exclude name is a
  registered tool (guards drift).
- Wiremock-backed provider test: with `enabled`, the Messages request body carries
  `context_management.edits[0]` with the expected fields and the `anthropic-beta` header holds
  `context-management-2025-06-27`; disabled sends neither. A guardian request on the same account
  carries neither (silent-failure risk: a guardian judging cleared context).
- Run `fontaine-evals cache-bench` once manually with it on and off for the measurement the
  issue asks for; record the result in the PR (not a merge gate).

## Open Questions
- `applied_edits` in the response is not parsed by rig, so what was cleared is not visible; surface
  it (a follow-up issue) once rig exposes it.
- Pairing with a pre-summary prune (#161) and `count_tokens` previews is out of scope here.
