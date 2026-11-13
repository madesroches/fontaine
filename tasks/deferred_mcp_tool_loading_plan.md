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
  does.
- When active, MCP tools are *deferred*: registered and guarded exactly as today, but not part of the
  advertised definitions. The system prompt gains a section listing deferred tool names grouped by server
  (names only, sorted, so the prefix is stable).
- A new built-in `tool_search` tool takes `query` (keywords, or `select:name1,name2` for exact names) and
  `max_results`. It matches against deferred tool names and descriptions, loads the matches, and returns their
  full definitions (name, description, JSON schema) as the result. A loaded tool is advertised in `tools[]`
  from the next request on.
- **Append-only loading.** Loaded tools are appended after the always-exposed set in load order, never
  re-sorted, so a load only invalidates the cache from the append point, never the stable prefix. The loaded
  set lives for the session (and is rebuilt on resume from the transcript's `tool_search` results; compaction
  may drop some of those results, so the rebuild is best-effort from whatever remains).
- Tool-list-changed refreshes and reconnects keep working: a deferred tool that disappears is dropped from
  the name list and the loaded set; a new one lands in the name list.
- Servers' `instructions`/resources/prompts are unaffected.

### Gate invariants

- `tool_search` is a registered built-in with a descriptor and risk class `ReadOnly`; it reads nothing outside
  the in-memory tool registry and takes no path, so the fast path rule is unchanged (read-only is not a
  safety claim for MCP tools, and loading a schema grants nothing).
- A deferred tool, loaded or not, keeps `RiskClass::Arbitrary` and the same `guard_callback`; a call to it,
  whatever route the model reached the name by, goes through the identical gate. A call to a deferred tool
  that was never loaded never reaches the wrapped callback: rig-agent narrows dispatch to the advertised set
  and classes the call `InvalidToolCallReason::UnknownTool`, which fails the run by default. A per-agent
  `on_invalid_tool_call` hook (Step 6) turns it into a `Skip` refusal telling the model to search first. This
  refusal never touches the gate or the audit log, because the call never reaches the gate.
- A subagent only sees deferred names its own toolset allows, and its `tool_search` only loads those.
  Loaded sets are per-agent (a child's loads never widen its parent's `tools[]` or the reverse): the
  connection-scoped handle holds no loaded set.

### Mechanism

1. **Registration side** (`fontaine-tools/src/mcp/register.rs`): `GuardedSink` gains a shared,
   connection-scoped `Deferral` handle holding only the registry (names, descriptions, schemas) and the
   active flag. The wrapped tools are unchanged; only advertisement differs.
2. **Advertisement side**: deferred tools are registered normally (always-exposed side). Each agent gets a
   rig `CompletionCall` hook returning `CompletionCallAction::Patch(RequestPatch::active_tools(..))` listing
   the non-deferred tool names followed by that agent's loaded names in load order, so a request carries the
   always-exposed set plus the loaded deferred tools. rig-agent 0.46 has no per-run dynamic-id injection
   (`snapshot_with_dynamic` ids come only from retrieval indexes), so this is the route.
3. **`tool_search` tool** (`fontaine-tools`, new `tool_search.rs`): one instance per agent, holding the
   connection-scoped `Deferral` handle (read-only registry) and that agent's own loaded-set object, which its
   completion hook also reads. Ranks matches (case-insensitive term overlap on name then description;
   `select:` exact), marks them loaded in its own set, renders the result. Matching logic is a pure function.
4. **Decision** (`fontaine-core/src/agent/mcp.rs`, after `connect_mcp` and after each refresh): compute
   deferral from the setting and the MCP definitions' token estimate; recompute when the toolset changes. If
   the decision flips from deferred to not (e.g. servers removed), the loaded set is cleared and everything is
   advertised.
5. **Prompt** (`fontaine-core/src/prompt.rs`): a `deferred_tools_section(names)` builder beside
   `mcp_resource_section`, only when active.
6. **Transport carve-out**: the `claude-subscription` transport already switches tool search off in its CLI;
   Fontaine's own deferral is forced to `off` there (the CLI owns the tool list).

### Out of scope

Provider-specific backends (#439) are separate.

## Implementation Steps

1. Add the failing test that an unloaded deferred tool is absent from the request's tool definitions.
2. `fontaine-tools/src/mcp/deferral.rs` (new): `Deferral` (active flag, index of name/description/schema),
   `search(query, max)` pure function; and a per-agent loaded-set object (loaded names in order, `load`,
   `clear`).
3. `register.rs`: thread the connection-scoped `Deferral` handle through `Shared`/`McpOptions`; keep `record`
   feeding it on each `add`/`reconcile`; names that disappeared are dropped from the registry and from each
   agent's loaded set on its next read.
4. `tool_search.rs` + descriptor entry (`descriptor.rs`) with `RiskClass::ReadOnly`; register in the built-in
   table; add to the default toolsets that include MCP tools (and not to children whose toolset has no MCP).
5. Advertisement: the per-agent `CompletionCall` hook with `RequestPatch::active_tools` (Mechanism 2) so
   request definitions = always-exposed + loaded.
6. Refusal for unloaded deferred calls: a per-agent `on_invalid_tool_call` hook (root builder in
   `agent/build.rs`, child builder in `child_runner.rs`) returning
   `InvalidToolCallAction::Skip { reason: "search first with tool_search" }` for names that are
   registered-but-deferred-and-unloaded; every other invalid call stays on the default fail path.
7. Config: `McpSection.tool_search` in `config.rs`, schema (`docs/schemas`), config catalog/merge (either
   scope, no loosening classification), `/config` display.
8. Decision logic + threshold in `agent/mcp.rs`; prompt section in `prompt.rs`; claude-subscription forced off.
9. `/context`: show deferred names and loaded schemas separately (`context.rs`, TUI context view).
10. Subagent registry: `tool_search` follows the parent's MCP entries; each child gets its own `tool_search` instance, loaded-set
    object and completion hook, starting empty.
11. Telemetry: a `tool_search` span/event with counts only (names hidden unless `log_tool_details`, as for MCP
    calls).
12. Docs (below), CHANGELOG entry, `## Decisions` line in the master plan's MCP subsection.

## Files to Modify

- `rust/fontaine-tools/src/mcp/register.rs`, new `mcp/deferral.rs`, `mcp/servers.rs` (options)
- `rust/fontaine-tools/src/tool_search.rs` (new), `descriptor.rs`, `builtins.rs`, `lib.rs`
- `rust/fontaine-core/src/agent/mcp.rs`, `agent/build.rs`, `child_runner.rs`, `agent.rs`, `prompt.rs`, `context.rs`, `config.rs`,
  `config_catalog.rs`, `providers/claude_subscription/*` (forced off)
- `rust/fontaine-agents/src/registry.rs` (child toolsets)
- `rust/fontaine-tui` context view
- `docs/mcp.md`, `docs/configuration.md`, `docs/tools.md`, `docs/schemas/*`, `CHANGELOG.md`

## Decisions

- 2026-11-13: (#34) Names-only prompt plus loading the real tool definition, not a `tool_call` proxy tool: keeps rules, hooks and audit keyed on the real `mcp__server__tool` name.
- 2026-11-13: (#34) Loading is append-only (loaded tools after the always-exposed set, in load order, never re-sorted): a load only invalidates the provider's cached prefix from the append point.
- 2026-11-13: (#34) Deferral is provider-neutral and client-side, not provider-native tool search: native needs per-provider wire support; #439 tracks the Kimi variant.
- 2026-11-13: (#34) `[mcp] tool_search = "auto"` decides on a heuristic token estimate (10% of the context window, 10,000 tokens if unknown), as Claude Code does; the estimate only trades context for one round trip.
- 2026-11-13: (#34) `[mcp] tool_search` is not a loosening key and either scope may set it: deferral hides schemas but changes no gate.

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
  silent bypass here would be a security regression, hence automated); an unloaded call is refused by the
  `on_invalid_tool_call` hook with the search-first message and no gate decision is recorded; a `list_changed` refresh updates names and loaded set.
- Headless `fontaine -p` scripted-model test: deferred names in the prompt, search, then call.
- `/context` rendering test for deferred/loaded split.

## Open Questions

- Whether resume should rebuild the loaded set from transcript `tool_search` results (planned; best-effort,
  since compaction may drop some) or start empty and let the model search again (cheaper to build, costs one
  round trip).
