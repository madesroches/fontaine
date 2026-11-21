# Anthropic Native Deferred Tool Loading Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/628

## Overview

Fontaine's tool search (#34) keeps deferred MCP tools out of `tools[]` and advertises a tool once
`tool_search` loads it. On the Anthropic Messages wire a change to `tools[]` invalidates the whole
prompt cache (tools, system, messages), so every load discards the cached conversation. For
`anthropic` accounts, send every deferred MCP tool in `tools[]` once with `defer_loading: true`
and have `tool_search` answer with `tool_reference` blocks, Anthropic's documented client-side
search. The request prefix then never changes when a tool is loaded, and references persist in
history. Registry, risk class and gate are untouched; only how the schema reaches the model changes
(the same split as #439 for Kimi).

## Current State

- `rust/fontaine-sdk/src/core/agent/deferred.rs`: `DeferredAdvertiser` is an `AgentHook` whose
  `on_completion_call` patches `active_tools` to drop deferred tools not yet loaded and
  `on_invalid_tool_call` tells the model to search first. Loaded sets are per agent
  (`LoadedSets` in `tools/mcp/deferral.rs`).
- `rust/fontaine-sdk/src/tools/tool_search.rs`: `ToolSearchTool` returns a `String` — a header line
  plus a pretty-printed JSON array of `{name, description, input_schema}` — and records the names
  in `LoadedSets`.
- `rust/fontaine-sdk/src/core/providers/anthropic.rs`: `Connected::model` builds a rig
  `Model<Messages>`; `PromptCaching::apply` (`providers/mod.rs:386`) adjusts the wire.
  `cache_wire.rs` shows the pattern for patching an encoded JSON body from a wire wrapper.
- rig-core 0.46 (`providers/anthropic/completion.rs`) already passes a tool carrying
  `defer_loading: true` through from `additional_params`/raw tool JSON and, when placing the tools
  cache marker, skips deferred tools (`rfind(... defer_loading != true)`). It does **not** model
  `tool_reference` in a `tool_result`: `ToolResultContent::Json` is stringified
  (`completion.rs:417`). So the `tool_result` rewrite has to happen on the encoded body, as
  `CacheWire` does for system markers.
- `docs/mcp.md` "Tool search" documents the cache restart and the "search again after resume or
  compaction" limitation; the master plan's Decisions line for #34 says deferral is provider-neutral
  and client-side.

## Design

**Capability.** A new `AccountKind`/provider method `native_deferral() -> bool` (default `false`,
`true` for `anthropic`), next to `accepts_context_editing`. Used only when the account talks to a
host that supports it: Anthropic's own endpoint and any `anthropic`-kind account whose model is
not Opus 4.1 or earlier (decided from catalog facts; unknown models are treated as supported).
A user-scope `[mcp] native_tool_search = true|false` (default `true`) turns it off; it only
selects a wire shape, so project scope may set it too (it tightens nothing, loosens nothing).

**Wire wrapper `DeferWire<W>`** (new `core/providers/defer_wire.rs`, modeled on `CacheWire`),
applied in `Connected::model` for the conversation scope only (not the guardian). It holds a clone
of the session's `Deferral`. In `encode`, after the inner wire encodes:

1. For every tool in `tools[]` whose name `Deferral::is_registered`, insert `defer_loading: true`.
   `tool_search` and all non-MCP tools stay non-deferred, which satisfies "at least one
   non-deferred tool", and rig's cache marker already lands on the last non-deferred tool.
2. For each `tool_result` block whose `tool_use_id` belongs to a `tool_use` named `tool_search`,
   rewrite `content` into `tool_reference` blocks (`{"type":"tool_reference","tool_name":…}`),
   one per loaded name. Names are read from the existing JSON array `tool_search` already returns,
   so nothing new is stored in history and a resumed session re-derives the references for free.
   A `tool_search` result with no array (miss, "no tools deferred") is left as text.
3. A referenced name absent from `tools[]` (tool vanished in a `list_changed` refresh) is dropped
   from the blocks, because the API rejects references without a definition; the text fallback is
   used if none remain.

**Advertiser.** On the native route `DeferredAdvertiser::on_completion_call` must not filter
deferred tools out (they have to be in `tools[]`, marked deferred). `Deferral`/the advertiser
gets a `native: AtomicBool`-style flag set by the agent builder from the account capability and
the retry state; when set, `advertised` returns every registered name while deferral is active.
`LoadedSets` still records per-agent loads (used by `/context`, the subagent toolset limit in
`tool_search`, and as the fallback route's state). `on_invalid_tool_call` is unchanged.

**Subagents.** A subagent's `tool_search` only loads names its own toolset holds (unchanged), and
its request carries only its own registered tools, so its references can only name tools in its own
`tools[]`.

**Retry without.** The issue asks for a one-time retry on a host that rejects the fields (#169 is
not shipped, so no generic mechanism exists). Add a narrow one here: when the first request of an
account is rejected with a 400 whose message names `defer_loading` or `tool_reference`, flip the
native flag off for the session, emit `Approver::warn` once, and retry the request on the
client-side route. The flag lives with the account connection, is never persisted, and no gate
decision depends on it.

**Fallback and `claude-subscription`.** Non-Anthropic providers, `native_tool_search = false`, and
the retry state keep today's behavior byte for byte. `claude-subscription` still never defers.

## Implementation Steps

1. `providers/mod.rs`, `providers/anthropic.rs`: add `native_deferral` capability; thread the
   session `Deferral` and native flag into `ConnectContext`/`Connected`.
2. `providers/defer_wire.rs`: `DeferWire` with the three body rewrites above; wrap in
   `Connected::model` for `CacheScope::Conversation`.
3. `core/agent/deferred.rs`, `core/agent/build.rs`: native flag on the advertiser; skip the
   `active_tools` patch when native.
4. `core/config.rs`, `core/config_catalog.rs`, `core/config_scope.rs`: `[mcp] native_tool_search`.
5. Retry-without: detect the 400 text, flip the flag, warn once, resend.
6. `tool_search.rs`: tool description text stays accurate for both routes ("callable from your
   next turn" holds on both); add a helper that parses the loaded names from its output, shared by
   the wire wrapper so the format has one owner.
7. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/{mod,anthropic}.rs`, new `defer_wire.rs`
- `rust/fontaine-sdk/src/core/agent/{deferred,build}.rs`
- `rust/fontaine-sdk/src/tools/tool_search.rs`, `tools/mcp/deferral.rs` (native flag)
- `rust/fontaine-sdk/src/core/{config,config_catalog,config_scope}.rs`
- `docs/mcp.md`, `docs/configuration.md`, `CHANGELOG.md`, master plan `## Decisions`

## Trade-offs

- **Body patch vs. extending rig's types.** rig exposes `defer_loading` but not `tool_reference`
  results; patching the encoded body avoids forking rig (pinned exactly) and matches `CacheWire`.
- **Parse names from `tool_search`'s text vs. a structured side channel.** Parsing keeps history
  format unchanged and makes resume work with no new storage; the cost is a format coupling,
  contained in one shared helper with a test.
- **Hosting gate by capability, not model sniffing alone.** Unknown hosts fall to the retry rather
  than a per-host table (that is #169's scope).

## Decisions

- AFK: default `native_tool_search = true` on `anthropic` accounts (conservative on correctness:
  retry-without falls back safely), user and project scope may both set it.
- AFK: the retry covers only this feature's two field names, not a general capability set (#169).
- AFK: cache savings are not measured in this change; run `cache-bench` (#498) before and after and
  record the numbers in the PR as a measurement, not a merge gate.

## Documentation

- `docs/mcp.md` "Tool search": describe the Anthropic route (definitions sent once, deferred;
  loads no longer restart the cache), keep the cache-restart text for other providers, and revise
  the resume/compaction limitation (references persist; after a compaction that summarises them
  away the model searches again).
- `docs/configuration.md` `[mcp]`: `native_tool_search`.
- Master plan `## Decisions`: dated line under the tool-search subject noting the Anthropic-native
  route alongside the provider-neutral one.

## Testing Strategy

All unit tests, no live API:

- `defer_wire`: tools marked `defer_loading` only for deferred MCP names; `tool_search` and others
  unmarked; the tools cache marker lands on a non-deferred tool; `tool_search` result becomes
  `tool_reference` blocks; a miss stays text; a vanished name is dropped; a result from another
  tool is untouched; guardian scope is untouched.
- `deferred.rs`: with the native flag, every registered tool is advertised; without it, today's
  tests still pass.
- `tool_search`: the shared name-parsing helper round-trips its own output.
- Retry: a wiremock 400 naming `defer_loading` flips the route once, warns once, and the second
  request carries no `defer_loading` and text results.
- Resume: rebuilding history from a session log with a `tool_search` result yields references.
- Config: key parsing, scope placement, catalog test stays green.
- The gate choke-point test stays green: no tool gains or loses a risk class.

## Open Questions

- Whether Bedrock/Vertex-hosted Claude accepts the fields beyond InvokeModel (issue: InvokeModel
  only). Left to the retry-without path; no per-host table here.
