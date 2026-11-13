# Deferred MCP Tool Loading (Tool Search) Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/34

## Overview

When MCP servers expose many tools, their schemas dominate the request's `tools[]` array. Claude Code
defers them behind a `ToolSearch` tool when they would use too much context. Fontaine does the same:
every MCP tool is still registered (risk registry, guard wrapper, choke-point coverage) at connect time,
but past a threshold only their **names** reach the model until it asks for them with a new `tool_search`
tool. Searching loads the matching tools' full definitions into the advertised set, and the gate for
calling a deferred tool is unchanged.

## Current State

- `fontaine-tools/src/mcp/register.rs`: `GuardedSink::wrap` wraps every MCP tool with `guard_callback(..,
  RiskClass::Arbitrary, ..)`, names it `mcp__<server>__<tool>`, and `wrap_all` sorts by name so the tool
  array is stable for the provider's cached prefix. `add_managed_tools` / `reconcile_managed_tools` pass the
  result to the rig `ToolServerHandle`, which registers each as always-exposed (`register(tool, true)` in
  rig-agent 0.46 `tool/server.rs`).
- Every request advertises every registered tool (`tool_defs`); `/context` measures them
  (`agent.rs` `context_usage`, `context.rs` `tool_tokens` partitions by the `mcp__` prefix).
- rig-agent already distinguishes always-exposed from retrievable registrations
  (`ToolSet::add_retrievable_tools`, `ToolServerHandle::snapshot_with_dynamic(ids)`); the MCP managed-sink
  path never uses the retrievable side.
- Subagent toolsets are computed by Fontaine (`fontaine-agents/src/registry.rs`), MCP entries matched by
  `mcp__server` / `mcp__server__tool` names.
- `docs/mcp.md` has no section on this; `docs/claude-subscription.md` notes tool search is switched off for
  that transport.

## Design

### Behavior

- A new setting `[mcp] tool_search = "auto" | "on" | "off"` (user and project scope; default `auto`), as in
  Claude Code's `ENABLE_TOOL_SEARCH`. `auto` defers when the MCP tool definitions' estimated tokens
  (`HeuristicTokenCounter::count_tool`, as `/context` uses) exceed a fraction of the model's context window
  (10%, the Claude Code default; no window known → 10,000 tokens). `on` always defers MCP tools, `off` never
  does. Deferral hides schemas but changes no gate, so it is not a loosening key and either
  scope may set it.
- When active, MCP tools are *deferred*: registered and guarded exactly as today, but not part of the
  advertised definitions. The system prompt gains a section listing deferred tool names grouped by server
  (names only, sorted, so the prefix is stable).
- A new built-in `tool_search` tool takes `query` (keywords, or `select:name1,name2` for exact names) and
  `max_results`. It matches against deferred tool names and descriptions, loads the matches, and returns their
  full definitions (name, description, JSON schema) as the result. A loaded tool is advertised in `tools[]`
  from the next request on.
- **Append-only loading.** Loaded tools are appended after the always-exposed set in load order, never
  re-sorted, so a load only invalidates the cache from the append point, never the stable prefix. The loaded
  set lives for the session (and is rebuilt on resume from the transcript's `tool_search` results).
- Tool-list-changed refreshes and reconnects keep working: a deferred tool that disappears is dropped from
  the name list and the loaded set; a new one lands in the name list.
- Servers' `instructions`/resources/prompts are unaffected.

### Gate invariants

- `tool_search` is a registered built-in with a descriptor and risk class `ReadOnly`; it reads nothing outside
  the in-memory tool registry and takes no path, so the fast path rule is unchanged (read-only is not a
  safety claim for MCP tools, and loading a schema grants nothing).
- A deferred tool, loaded or not, keeps `RiskClass::Arbitrary` and the same `guard_callback`; a call to it,
  whatever route the model reached the name by, goes through the identical gate. A call to a deferred tool
  that was never loaded is refused with a message telling the model to search first (the model has no schema;
  it would be guessing arguments). This refusal is a tool error, not a gate decision, and never reaches
  past the floor.
- A subagent only sees deferred names its own toolset allows, and its `tool_search` only loads those.
  Loaded sets are per-agent (a child's loads never widen its parent's `tools[]` or the reverse).
- Deferral never changes what `/context` reports as MCP tools' cost beyond showing the deferred names' tokens
  and the loaded schemas separately.

### Mechanism

1. **Registration side** (`fontaine-tools/src/mcp/register.rs`): `GuardedSink` gains a shared
   `Deferral` handle (decision cell: active or not, plus the loaded-name set). The wrapped tools are
   unchanged; only advertisement differs.
2. **Advertisement side**: the set of definitions a request carries is the always-exposed set plus the loaded
   deferred tools. Preferred route: register deferred tools as retrievable and pin loaded ids with
   rig-agent's `snapshot_with_dynamic`. If the engine's request path does not accept per-run dynamic ids
   (verified in step 1), fall back to a thin `ToolDefinition` filter at the single place Fontaine builds
   each agent's request, keyed on the `Deferral` handle.
3. **`tool_search` tool** (`fontaine-tools`, new `tool_search.rs`): holds the `Deferral` handle (read-only
   index of names + descriptions + schemas), ranks matches (case-insensitive term overlap on name then
   description; `select:` exact), marks them loaded, renders the result. Matching logic is a pure function.
4. **Decision** (`fontaine-core/src/agent/mcp.rs`, after `connect_mcp` and after each refresh): compute
   deferral from the setting and the MCP definitions' token estimate; recompute when the toolset changes. If
   the decision flips from deferred to not (e.g. servers removed), the loaded set is cleared and everything is
   advertised.
5. **Prompt** (`fontaine-core/src/prompt.rs`): a `deferred_tools_section(names)` builder beside
   `mcp_resource_section`, only when active.
6. **Transport carve-out**: the `claude-subscription` transport already switches tool search off in its CLI;
   Fontaine's own deferral is forced to `off` there (the CLI owns the tool list).

### Out of scope

Server-side provider tool-search (Anthropic `tool_search_tool_*`) and the Kimi mid-history `tools` message
(#439) are separate, provider-specific backends for the same feature; this plan is provider-neutral.

## Implementation Steps

1. **Spike the advertisement route**: in `fontaine-agents`/`fontaine-core`, check whether the agent run path
   can pass dynamic ids to `snapshot_with_dynamic` or must use a definition filter; record the finding in
   Decisions. Add a failing unit test asserting an unloaded deferred tool is absent from `tool_defs`.
2. `fontaine-tools/src/mcp/deferral.rs` (new): `Deferral` (active flag, loaded names in order, index of
   name/description/schema), `search(query, max)` pure function, `load`, `clear`.
3. `register.rs`: thread the `Deferral` handle through `Shared`/`McpOptions`; keep `record` feeding it on each
   `add`/`reconcile`; drop names that disappeared.
4. `tool_search.rs` + descriptor entry (`descriptor.rs`) with `RiskClass::ReadOnly`; register in the built-in
   table; add to the default toolsets that include MCP tools (and not to children whose toolset has no MCP).
5. Advertisement: implement the route chosen in step 1 so request definitions = always-exposed + loaded.
6. Refusal for unloaded deferred calls (in the wrapped callback, before the gate runs? No: after the gate, so
   the gate decision is recorded for the attempted call; the refusal is the tool result).
7. Config: `McpSection.tool_search` in `config.rs`, schema (`docs/schemas`), config catalog/merge (either
   scope, no loosening classification), `/config` display.
8. Decision logic + threshold in `agent/mcp.rs`; prompt section in `prompt.rs`; claude-subscription forced off.
9. `/context`: show deferred names and loaded schemas separately (`context.rs`, TUI context view).
10. Subagent registry: `tool_search` follows the parent's MCP entries; a child's loaded set is its own.
11. Telemetry: a `tool_search` span/event with counts only (names hidden unless `log_tool_details`, as for MCP
    calls).
12. Docs (below), CHANGELOG entry, `## Decisions` line in the master plan's MCP subsection.

## Files to Modify

- `rust/fontaine-tools/src/mcp/register.rs`, new `mcp/deferral.rs`, `mcp/servers.rs` (options)
- `rust/fontaine-tools/src/tool_search.rs` (new), `descriptor.rs`, `builtins.rs`, `lib.rs`
- `rust/fontaine-core/src/agent/mcp.rs`, `agent/build.rs`, `agent.rs`, `prompt.rs`, `context.rs`, `config.rs`,
  `config_catalog.rs`, `providers/claude_subscription/*` (forced off)
- `rust/fontaine-agents/src/registry.rs` (child toolsets)
- `rust/fontaine-tui` context view
- `docs/mcp.md`, `docs/configuration.md`, `docs/tools.md`, `docs/schemas/*`, `CHANGELOG.md`

## Trade-offs

- **Names-only prompt + search tool (chosen)** vs. a `tool_call` proxy meta-tool (Hermes style): a proxy
  would hide the real tool name from the gate's rule matching and from `mcp__server__tool` permission rules;
  loading the real definition keeps every rule, hook and audit record keyed on the real name.
- **Append-only loading** vs. re-sorting loaded tools into the stable set: re-sorting would break the cached
  prefix on every search.
- **Provider-neutral client-side deferral** vs. provider-native tool search: native needs per-provider wire
  support and is unavailable on most providers; #439 tracks the Kimi variant.
- **Auto threshold on estimated tokens**: matches Claude Code's behavior; the estimate is heuristic, which is
  acceptable because the setting only trades context for one extra round trip.

## Decisions

(none yet)

## Documentation

- `docs/mcp.md`: new "Tool search" section (when it activates, `tool_search` setting, how loading works, that
  permissions and hooks are unchanged).
- `docs/configuration.md` and the generated schema: `[mcp] tool_search`.
- `docs/tools.md`: the `tool_search` tool.
- `docs/claude-subscription.md`: note that Fontaine's deferral is off there.

## Testing Strategy

- Unit: `search` ranking, `select:` exact, no-match, `max_results`; `Deferral` append order, reconcile drops
  vanished names, clear on flip; threshold decision (auto/on/off, window known/unknown); prompt section is
  sorted and stable; config parse/merge (either scope).
- Unit: an unloaded deferred tool is absent from the advertised definitions and present after `tool_search`;
  the loaded tool is appended after the always-exposed set without reordering it.
- Integration (existing MCP test server in `fontaine-tools/tests/mcp_common`): a call to a deferred tool,
  loaded or not, passes through the same gate (extend `choke_point.rs` so deferred MCP tools stay covered; a
  silent bypass here would be a security regression, hence automated); an unloaded call is refused with the
  search-first message; a `list_changed` refresh updates names and loaded set.
- Headless `fontaine -p` scripted-model test: deferred names in the prompt, search, then call.
- `/context` rendering test for deferred/loaded split.

## Open Questions

- Whether rig-agent 0.46's run path accepts per-run dynamic tool ids (step 1 settles it; the definition
  filter is the fallback).
- Whether resume should rebuild the loaded set from transcript `tool_search` results (planned) or start empty
  and let the model search again (cheaper to build, costs one round trip).
