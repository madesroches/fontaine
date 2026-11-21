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
A `[mcp] native_tool_search = true|false` (default `true`) turns it off; it only selects a wire
shape, so project scope may set it too. A non-Anthropic host on an `anthropic` account relies on
the retry; `databricks` and `opencode-go` stay client-side.

**Wire wrapper `DeferWire<W>`** (new `core/providers/defer_wire.rs`, modeled on `CacheWire`),
applied at agent build time (`core/agent/build.rs`, `core/agent/child_runner.rs`) where the
session `Deferral` is available, for the conversation scope only (not the guardian). It is not
applied in `Connected::model`/`ConnectContext`, which are built at startup before MCP and
`Deferral` exist. The Anthropic provider exposes only the `native_deferral` capability and a way
to wrap its `Model<Messages>`. `DeferWire` holds a clone of the session's `Deferral` and the
session retry-off flag (below). In `encode`, after the inner wire encodes, and only when the
encoded `tools[]` contains `tool_search` (otherwise the body is left untouched):

1. For every tool in `tools[]` whose name `Deferral::is_registered`, insert `defer_loading: true`.
   `tool_search` and all non-MCP tools stay non-deferred, which satisfies "at least one
   non-deferred tool". rig applies its tools cache marker inside its own encode, before this
   step, so the marker can sit on a tool `DeferWire` then defers, which Anthropic rejects with a
   400. `DeferWire` therefore moves `cache_control` off every tool it defers onto the last
   remaining non-deferred tool (if none carried one and a tools cache marker was present, it
   keeps one marker on the last non-deferred tool).
2. For each `tool_result` block whose `tool_use_id` belongs to a `tool_use` named `tool_search`,
   rewrite `content` into `tool_reference` blocks (`{"type":"tool_reference","tool_name":…}`),
   one per loaded name. Names are read from the JSON array that directly follows the
   `Loaded N tool(s)` header in the existing `tool_search` output, so nothing new is stored in
   history and a resumed session re-derives the references for free. When any text trails the
   array (the result pipeline appends notification, scanner and hook notes as `\n\n{note}`), or
   there is no array (miss, "no tools deferred"), the result stays plain text.
3. A referenced name absent from `tools[]` (tool vanished in a `list_changed` refresh) is dropped
   from the blocks, because the API rejects references without a definition; the text fallback is
   used if none remain.

**Advertiser.** The native decision is per advertiser, computed from that agent's own
model/account (a subagent may run on another account), in both `build.rs` and
`child_runner.rs` (which constructs the child `DeferredAdvertiser`), combined with the
session-level retry-off state. When native, `DeferredAdvertiser::on_completion_call` must not
filter deferred tools out (they have to be in `tools[]`, marked deferred) and `advertised` returns
every registered name while deferral is active. `LoadedSets` still records per-agent loads (used
by `/context`, the subagent toolset limit in `tool_search`, and as the fallback route's state).
`on_invalid_tool_call` is unchanged.

**Subagents.** A subagent's `tool_search` only loads names its own toolset holds (unchanged), and
its request carries only its own registered tools, so its references can only name tools in its own
`tools[]`.

**Retry without.** The issue asks for a retry on a host that rejects the fields (#169 is not
shipped, so no generic mechanism exists). Add a narrow one here: one session-owned retry-off
flag, read live by `DeferWire` and the advertisers on each request (not set once at build). The
retry point is `turn::drive` in `core/turn.rs`: on a 400 naming `defer_loading` or
`tool_reference`, on any request (not just the first), flip the flag, emit `Approver::warn` once,
and re-drive (a 400 is not otherwise retryable). The flag is never persisted and no gate decision
depends on it.

**Fallback and `claude-subscription`.** Non-Anthropic providers, `native_tool_search = false`, and
the retry state keep today's behavior byte for byte. `claude-subscription` still never defers.

## Implementation Steps

1. `providers/mod.rs`, `providers/anthropic.rs`: add the `native_deferral` capability and a way
   to wrap the provider's `Model<Messages>`.
2. `providers/defer_wire.rs`: `DeferWire` with the body rewrites above (including the cache-marker
   move and the `tool_search`-present guard).
3. `core/agent/deferred.rs`, `core/agent/build.rs`, `core/agent/child_runner.rs`: per-advertiser
   native decision; apply `DeferWire` at build; skip the `active_tools` patch when native.
4. `core/config.rs`, `core/config_catalog.rs`, `core/config_scope.rs`: `[mcp] native_tool_search`,
   with a `Rule::any("mcp.native_tool_search")` (precedent `mcp.tool_search`).
5. `core/turn.rs`: retry-without in `turn::drive`; the session flag.
6. `tool_search.rs`: tool description text stays accurate for both routes ("callable from your
   next turn" holds on both); add a helper, shared by the wire wrapper so the format has one
   owner, that parses the loaded names from the array directly following the `Loaded N tool(s)`
   header and reports trailing text so the caller keeps the result as text.
7. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/{mod,anthropic}.rs`, new `defer_wire.rs`
- `rust/fontaine-sdk/src/core/agent/{deferred,build,child_runner}.rs`, `core/turn.rs`
- `rust/fontaine-sdk/src/tools/tool_search.rs`
- `rust/fontaine-sdk/src/core/{config,config_catalog,config_scope}.rs`
- `docs/mcp.md`, `docs/configuration.md`, `CHANGELOG.md`, master plan `## Decisions`

## Trade-offs

- **Body patch vs. extending rig's types.** rig exposes `defer_loading` but not `tool_reference`
  results; patching the encoded body avoids forking rig (pinned exactly) and matches `CacheWire`.
- **Parse names from `tool_search`'s text vs. a structured side channel.** Parsing only the array
  directly after the header keeps history format unchanged and makes resume work with no new
  storage; a result with trailing notes stays text. The cost is a format coupling, contained in
  one shared helper with a test.

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
- `docs/configuration.md` `[mcp]`: `native_tool_search`, with a scope-table row beside
  `mcp.tool_search`.
- Master plan `## Decisions`: a dated line at the end of `### MCP` that supersedes the 2026-11-13
  (#34) provider-neutral line for `anthropic` accounts and amends the resume line; reason: Claude
  Code uses native deferral there.

## Testing Strategy

All unit tests, no live API:

- `defer_wire`: tools marked `defer_loading` only for deferred MCP names; `tool_search` and others
  unmarked; a cache marker rig placed on a tool that gets deferred moves to the last non-deferred
  tool, and no tool carries both; a request without `tool_search` in `tools[]` is untouched;
  `tool_search` result becomes `tool_reference` blocks; a result with an appended note stays text;
  a miss stays text; a vanished name is dropped; a result from another tool is untouched;
  guardian scope is untouched.
- `deferred.rs`: with the native flag, every registered tool is advertised; without it, today's
  tests still pass.
- `tool_search`: the shared name-parsing helper round-trips its own output.
- Retry: a wiremock 400 naming `defer_loading` flips the route once, warns once, and the second
  request carries no `defer_loading` and text results; the same on a later (not first) request.
- `child_runner`: a child on a non-native account gets a non-native advertiser while the parent
  stays native.
- Config: key parsing, scope placement, catalog test stays green.
- The gate choke-point test stays green: no tool gains or loses a risk class.
