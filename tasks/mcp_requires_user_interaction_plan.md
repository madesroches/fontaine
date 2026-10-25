# MCP `requiresUserInteraction` Marker Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/2

## Overview

Claude Code lets an MCP server mark a tool as needing a person's approval on every call by setting
`_meta["anthropic/requiresUserInteraction"]` to the JSON Boolean `true` in that tool's `tools/list`
entry. Claude Code then prompts on every call of that tool, even in `acceptEdits`, `auto` and
`bypassPermissions`. Allow rules don't skip the prompt, it never offers "don't ask again", and
`dontAsk` (or a prompt tool that can't reach a person) denies the call instead
(https://code.claude.com/docs/en/mcp#require-approval-for-a-specific-tool). Fontaine treats the
marker as an implicit `[permissions] ask` rule on that one tool, at the deterministic floor. It
therefore holds in every mode, nothing can approve past it, and it only ever tightens.

## Current State

- **MCP registration** — `rust/fontaine-mcp/src/register.rs`: `GuardedSink::wrap` renames each
  tool to `mcp__<server>__<tool>` and registers it through
  `fontaine_tools::guarded::guard_callback` as `RiskClass::Arbitrary`. Declared hints reach the
  guardian only as text (`CallView.annotations`).
- **`_meta` never gets to Fontaine.** rig-rmcp 0.45.0 builds each `DynamicTool` from
  `rmcp::model::Tool` and keeps only name, description, schema and the five annotation hints
  (`rig-rmcp-0.45.0/src/native.rs:484-556`). `rmcp::model::Tool.meta` (`_meta`) is dropped before
  the `ManagedToolSink` ever sees the tool, and nothing on the sink can recover it.
- **Transports** — `rust/fontaine-mcp/src/servers.rs`: `start` builds a `TokioChildProcess`
  (stdio) or a `StreamableHttpClientTransport` (HTTP) and hands it to
  `McpClientHandler::connect`. `connect_transport` does the same for an already-built transport
  (in-process tests). rmcp's `Transport<RoleClient>::receive` yields every server→client
  message, including the `ListToolsResult` responses of the initial fetch and of each
  list-changed refresh, before rig-rmcp turns them into tools.
- **The gate's view of a call** — `rust/fontaine-tools/src/gate.rs:25` `CallView { tool, risk,
  args, agent, annotations }`, built in `guarded.rs::gated` and in 7 other places (agent.rs, the
  none-guardian eval, core test helpers).
- **The floor** — `rust/fontaine-core/src/permissions.rs:613` `evaluate`. The order is: hook deny
  → deny rules → child restriction → plan block → **forced ask** (`rules.ask` match, else a hook
  `ask`; `permissions.rs:670-685`) → critical path → protected path. A forced ask goes through
  `would_prompt`: a `Prompt` in manual/acceptEdits/plan/auto, a Deny in `dontAsk` and in headless
  runs. Allow rules (step 5a) and hook allows only act when the floor forced nothing.
  `ask_human` offers no session/always grant for a `forced` prompt (`permissions.rs:1497-1511`).

So everything Claude Code's semantics need already exists at the floor. What's missing is getting
the marker from the wire to the gate.

## Design

```
 server ──tools/list──▶ InteractionTap (Transport wrapper) ──▶ rig-rmcp handler ──▶ GuardedSink::wrap
                         │ upserts marked names                                     │ looks the tool up
                         ▼                                                          ▼
                    Marked (Arc<RwLock<HashSet<String>>>) ◀───── read ───── guard_callback(.., requires_user_interaction)
                                                                                     │
                                                                CallView.requires_user_interaction
                                                                                     ▼
                                                    PermissionPipeline::evaluate — floor forced ask
```

### 1. Reading the marker: a transport tap (`fontaine-mcp`)

New module `rust/fontaine-mcp/src/interaction.rs`:

- `pub const REQUIRES_USER_INTERACTION: &str = "anthropic/requiresUserInteraction";`
- `pub(crate) struct Marked(Arc<RwLock<HashSet<String>>>)`, cloneable. It holds the server's
  original (un-renamed) tool names whose last `tools/list` entry carried the marker. It feeds a
  permission decision, so it is read and written with `fontaine_tools::lock::{read, write}`,
  which panic on poison.
- `fn marks(tool: &rmcp::model::Tool) -> bool`: true only when
  `tool.meta` has the key with `Value::Bool(true)`. Any other value is ignored, as in Claude Code.
- `pub(crate) struct InteractionTap<T> { inner: T, marked: Marked }`, implementing
  `rmcp::transport::Transport<RoleClient>`. `send` and `close` delegate. `receive` awaits the
  inner message, and when it is a `ServerJsonRpcMessage::Response` carrying
  `ServerResult::ListToolsResult(page)`, it **upserts** each listed tool: insert if `marks`, else
  remove. Then it returns the message unchanged. Upserting per name is right under pagination
  (rig-rmcp's `list_tools` fetches page by page) and on refresh: a tool's marker is always the
  one in the latest entry that lists it. A name left over from a tool that went away is harmless,
  because no registered tool carries it, and the next listing of that name overwrites it.
- Ordering guarantee: `receive` updates `Marked` before rig-rmcp gets the page. rig-rmcp builds
  and sinks the tools only after the last page, so `GuardedSink::wrap` always reads the current
  marker.

`servers.rs`: `start` and `connect_transport` both build one `Marked` per server. They wrap the
transport (`InteractionTap::new(transport.into_transport(), marked.clone())`, for stdio, HTTP and
in-process alike) and pass `marked` to `GuardedSink::new`. The tap sits on the path every
server takes, so a new transport can't silently skip it.

### 2. Carrying it to the gate (`fontaine-tools`)

- `CallView` gains `pub requires_user_interaction: bool`, documented as: "The MCP server marked
  this tool as needing a person's approval on every call. It can only add a prompt; it never
  allows." Built-in tools and every other construction site pass `false`.
- `guarded.rs`: `Meta` gains the same field. `gated` copies it into both `CallView`s (gate and
  result pipeline). `guard_callback` gains a `requires_user_interaction: bool` parameter. The
  built-in `GuardedTool::call` passes `false`.
- `GuardedSink` gains a `marked: Marked` field. `wrap` reads `marked.contains(&original_name)`
  once, before renaming, and passes the result to `guard_callback`. Each refresh re-wraps every
  tool, so a marker a server adds or removes takes effect on the next list-changed refresh. This
  matches how the risk class is fixed at wrap time.

### 3. Enforcing it at the floor (`fontaine-core`)

In `PermissionPipeline::evaluate`, extend the forced-ask chain. The order is the user's ask
rule, then the server marker, then a hook `ask`:

```rust
let forced_ask = rules.ask.iter().find(..).map(|r| (Tier::AskRule, r.label()))
    .or_else(|| call.requires_user_interaction
        .then(|| (Tier::AskRule, REQUIRES_USER_INTERACTION_RULE.to_string())))
    .or_else(|| hook.ask.clone().map(|id| (Tier::Hook, id)));
```

- The marker has the same tier and the same path as an ask rule: `Tier::AskRule`, `forced:
  true`, through `would_prompt`. No new `Tier` variant is needed, so the audit log, telemetry,
  the headless `gate_decision` event and eval `expected_tier` all stay unchanged. The audit
  `rule` is the fixed text `anthropic/requiresUserInteraction` (named once as a constant in
  `permissions.rs`). It never contains the server or tool name, so `hide_mcp_names` telemetry
  stays clean.
- Prompt reason: "the MCP server requires a person's approval for every call of this tool". The
  existing ask-rule wording ("`{rule}` asks for approval before this runs") would read oddly
  with a `_meta` key as the rule, so the marker gets its own wording.
- What follows from the floor (no new code, only tests):
  - Deny rules and a hook deny still win, since they come earlier in the floor.
  - Manual, acceptEdits, plan and auto all prompt. In `auto` neither the guardian nor the screen
    is consulted.
  - `dontAsk` denies, and so does a headless `-p` run. That matches Claude Code's `dontAsk` and
    its "the prompt has to reach a person" rule. Fontaine has no `--permission-prompt-tool`.
  - Allow rules, session grants, skill grants and a PreToolUse hook `allow` never skip it.
  - The prompt offers only allow-once or deny (`forced`), which is Claude Code's "no don't ask
    again".
  - A subagent's calls go through the same pipeline and get the same treatment.

### Security invariants

- The marker can only add a prompt or a Deny. It never sets `Verdict::Allow`, never changes a
  risk class, and never reaches past the floor. A server omitting it gets exactly today's
  behavior.
- No regex: the check is an exact key lookup on a typed `_meta` map plus a `bool` on `CallView`.
- `readOnlyHint` and the other hints stay text-only. Only this marker, and only in the tightening
  direction, becomes structured.

## Implementation Steps

1. **fontaine-tools** — add `requires_user_interaction` to `CallView` (`gate.rs`) and to `Meta`
   (`guarded.rs`). Thread it through `gated`, add the `guard_callback` parameter, and pass `false`
   from `GuardedTool::call`. Update the other `CallView` sites to `false`:
   `fontaine-core/src/agent.rs`, `fontaine-evals/src/none.rs`,
   `fontaine-core/tests/common/pipeline.rs` (let the helper take the flag, or add a sibling
   helper for the marked call), `fontaine-core/tests/{scanner,scanner_model,user_hooks}.rs`.
2. **fontaine-mcp** — add `interaction.rs` (`Marked`, `marks`, `InteractionTap`, the constant)
   with unit tests. Wire the tap and `Marked` into `start` and `connect_transport` in
   `servers.rs`, and add `marked` to `GuardedSink` in `register.rs` (its `new` takes it). Export
   the constant from `lib.rs` if core uses it. Otherwise core defines its own audit-rule constant,
   so it doesn't take a dependency on `fontaine-mcp`.
3. **fontaine-core** — extend the forced-ask chain in `permissions.rs::evaluate` as above, with
   the audit-rule constant and the prompt reason.
4. **Tests** — see Testing Strategy.
5. **Docs** — see Documentation.

## Files to Modify

- `rust/fontaine-tools/src/gate.rs`, `rust/fontaine-tools/src/guarded.rs`
- `rust/fontaine-mcp/src/interaction.rs` (new), `rust/fontaine-mcp/src/servers.rs`,
  `rust/fontaine-mcp/src/register.rs`, `rust/fontaine-mcp/src/lib.rs`
- `rust/fontaine-core/src/permissions.rs`, `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-evals/src/none.rs`
- `rust/fontaine-mcp/tests/common/{mod.rs,server.rs}`, `rust/fontaine-mcp/tests/mcp_registration.rs`
- `rust/fontaine-core/tests/common/pipeline.rs`, `rust/fontaine-core/tests/permission_rules.rs`
  (or `permissions_auto.rs` for the auto case), `rust/fontaine-core/tests/{scanner,scanner_model,user_hooks}.rs`
- `docs/mcp.md`, `docs/permissions.md`, `tasks/design/permissions.md`,
  `research/research-claude-code-parity.md`

## Trade-offs

- **Transport tap vs. an upstream rig-rmcp change.** The clean fix is for rig-rmcp to carry
  `_meta` (or the marker) onto the `DynamicTool`. rig is a third-party crate pinned to an exact
  version, so that means waiting on an upstream release. The tap is roughly 60 lines inside
  `fontaine-mcp`, observes the same typed `rmcp::model::Tool` rig-rmcp parses, and changes no
  message. If upstream later carries `_meta`, the tap collapses into a lookup in `wrap`.
- **Tap vs. a second `tools/list` call from Fontaine.** A separate listing could race a
  list-changed refresh and disagree with what rig-rmcp registered. The tap reads the exact
  responses rig-rmcp builds from.
- **Fixed at wrap time vs. looked up per call.** Wrap time matches how the risk class is fixed,
  and refresh re-wraps anyway. A per-call lookup would add a lock read to every MCP call for no
  behavioral gain.
- **Reuse `Tier::AskRule` vs. a new tier.** The issue asks for an implicit ask rule, and the tier
  enum is a shared contract (audit, telemetry, evals). The `rule` field already says which rule
  fired.
- **A structured `bool` on `CallView` vs. parsing `annotations` text in core.** `annotations` is
  documented as never being a gate. A typed field keeps the "server text never decides" property
  intact.

## Decisions

## Documentation

- `docs/mcp.md` § The trust model: a new bullet. A tool whose `tools/list` entry sets
  `_meta["anthropic/requiresUserInteraction"]` to `true` asks on every call in every mode. It
  offers no session/always grant, is denied in `dontAsk` and headless runs, and isn't skipped by
  allow rules or hook allows. It's audited as `tier = ask_rule`,
  `rule = anthropic/requiresUserInteraction`. Include a `tools/list` JSON example. Also note that
  it's the only declared value that changes a decision, and only toward asking.
- `docs/permissions.md` § The pipeline: name the marker alongside ask rules in the floor's ask
  step.
- `tasks/design/permissions.md` § Core data flow step 2: add the marker to the `ask` floor check.
- `research/research-claude-code-parity.md`: flip the `requiresUserInteraction` row to `done`
  with a pointer to `docs/mcp.md`.
- `CHANGELOG.md` is updated by the `pr` step.

## Testing Strategy

All automated, none live:

- **Unit — `fontaine-mcp/src/interaction.rs`** (feed `InteractionTap` a scripted inner
  transport):
  - a `ListToolsResult` marks exactly the tools whose `_meta` key is JSON `true`;
  - `"true"` (string), `1`, `false` and a missing `_meta` don't mark;
  - a later page listing a marked tool without the key unmarks it, while tools not listed keep
    their state;
  - non-list messages (notifications, other results) pass through unchanged and touch nothing;
  - every message is returned byte-for-byte the same.
- **Integration — `fontaine-mcp/tests/mcp_registration.rs`.** Extend the test `Gate` double's
  `Seen` with the flag, and add a `grant_access_tool()` with the marker to `tests/common/server.rs`.
  - Over the in-process transport, `grant_access` reaches the gate with
    `requires_user_interaction = true` and `echo` with `false`.
  - After `replace_tools` drops the marker from `grant_access` and adds it to `echo`, the
    refreshed registrations flip both.
  - The streamable-HTTP test's scripted `tools/list` includes a marked tool, and the gate sees it
    marked. This pins the claim that `start`'s HTTP branch is tapped too, not just
    `connect_transport`.
- **Pipeline — `fontaine-core/tests/permission_rules.rs`** (plus `permissions_auto.rs` for auto),
  for a marked `mcp__srv__grant` call:
  - manual and acceptEdits prompt with `tier = ask_rule`, the marker rule, and `Offers` with
    neither session nor always;
  - auto prompts, and the scripted guardian and screen are never called (no guardian request
    recorded);
  - an `allow` rule `mcp__srv__grant` and a PreToolUse hook `allow` still prompt;
  - `dontAsk` and headless deny with `tier = ask_rule`;
  - a `deny` rule on the tool still wins with `tier = deny_rule`;
  - the same call unmarked keeps today's behavior (an allow rule allows it).

  These pin the security property: the marker can only tighten, and no allow path skips it.
- The existing choke-point test stays green. No new tool or risk class is added.

## Open Questions

None.
