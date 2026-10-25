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
  args, agent, annotations }`, built in `guarded.rs::gated` and in 6 other places (agent.rs, the
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
                         │ inserts marked names                                     │ looks the tool up
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
  original (un-renamed) tool names that any `tools/list` entry on this connection carried the
  marker for. It only grows; a new connection starts with an empty one. It feeds a
  permission decision, so it is read and written with `fontaine_tools::lock::{read, write}`,
  which panic on poison.
- `fn marks(tool: &rmcp::model::Tool) -> bool`: true only when
  `tool.meta` has the key with `Value::Bool(true)`. Any other value is ignored, as in Claude Code.
- `pub(crate) struct InteractionTap<T> { inner: T, marked: Marked }`, implementing
  `rmcp::transport::Transport<RoleClient>`. `send` and `close` delegate. `receive` awaits the
  inner message, and when it is a `ServerJsonRpcMessage::Response` carrying
  `ServerResult::ListToolsResult(page)`, it **inserts** each listed tool for which `marks` is
  true and never removes a name. Then it returns the message unchanged. Inserting per name is
  right under pagination (rig-rmcp's `list_tools` fetches page by page). A name left over from a
  tool that went away is harmless, because no registered tool carries it.
- Why the marker is sticky: rmcp runs each list-changed handler as its own task, rig-rmcp 0.45
  allows two refreshes in flight and drops the older one's commit, and a late response to a
  timed-out `tools/list` still passes through `receive`. Responses can therefore arrive in a
  different order than the refreshes rig-rmcp commits, so a "latest entry wins" upsert could let
  a stale unmarked page clear a marker that the committed listing carries. Insert-only can't
  loosen under any interleaving: `receive` updates `Marked` before rig-rmcp gets the page, and
  rig-rmcp sinks tools only after their last page, so `GuardedSink::wrap` sees every marker any
  response on this connection has carried. The cost is that a server dropping the marker takes
  effect only on a new connection (Fontaine has no reconnect, so the next session), where
  Claude Code follows the latest listing. That deviation only ever asks more.

`servers.rs`: `start` and `connect_transport` both build one `Marked` per server and pass it to
`GuardedSink::new`. `start` wraps the stdio and HTTP transports directly
(`InteractionTap::new(transport, marked.clone())`), since `TokioChildProcess` and
`StreamableHttpClientTransport` already implement `Transport<RoleClient>`. Only `connect_transport`
calls `.into_transport()` (its where-clause pins the generics). The tap's `type Error = T::Error`.
The tap sits on the path every server takes, so a new transport can't silently skip it.

### 2. Carrying it to the gate (`fontaine-tools`)

- `CallView` gains `pub requires_user_interaction: bool`, documented as: "The MCP server marked
  this tool as needing a person's approval on every call. It can only add a prompt; it never
  allows." Built-in tools and every other construction site pass `false`.
- `guarded.rs`: `Meta` gains the same field. `gated` copies it into both `CallView`s (gate and
  result pipeline). `guard_callback` gains a `requires_user_interaction: bool` parameter. The
  built-in `GuardedTool::call` passes `false`.
- `GuardedSink` gains a `marked: Marked` field, which stays private. `GuardedSink::new` takes it and
  becomes `pub(crate)`, because a `pub` signature naming the `pub(crate)` `Marked` would trip
  `private_interfaces` under `-D warnings`; its only callers are in `servers.rs`. `wrap` reads `marked.contains(&original_name)`
  once, before renaming, and passes the result to `guard_callback`. Each refresh re-wraps every
  tool, so a marker a server adds takes effect on the next list-changed refresh; one it removes
  stays in force until a new connection (see §1). This
  matches how the risk class is fixed at wrap time.

### 3. Enforcing it at the floor (`fontaine-core`)

In `PermissionPipeline::evaluate`, extend the forced-ask chain. The order is the user's ask
rule, then the server marker, then a hook `ask`:

```rust
let forced_ask = rules.ask.iter().find(..)
    .map(|r| (Tier::AskRule, r.label(), format!("`{}` asks for approval before this runs", r.label())))
    .or_else(|| call.requires_user_interaction.then(|| (
        Tier::AskRule,
        REQUIRES_USER_INTERACTION.to_string(),
        "the MCP server requires a person's approval for every call of this tool".to_string(),
    )))
    .or_else(|| hook.ask.clone().map(|id| (Tier::Hook, id, /* existing hook reason */)));
```

- The marker has the same tier and the same path as an ask rule: `Tier::AskRule`, `forced:
  true`, through `would_prompt`. No new `Tier` variant is needed, so the audit log, telemetry,
  the headless `gate_decision` event and eval `expected_tier` all stay unchanged. The audit
  `rule` is the fixed text `anthropic/requiresUserInteraction` (the `fontaine_mcp::REQUIRES_USER_INTERACTION`
  constant). It never contains the server or tool name, so `hide_mcp_names` telemetry
  stays clean.
- Prompt reason: the tuple carries its own reason, so the marker gets its own wording. The
  existing ask-rule wording ("`{rule}` asks for approval before this runs") would read oddly with a `_meta` key as the rule.

## Implementation Steps

1. **fontaine-tools** — add `requires_user_interaction` to `CallView` (`gate.rs`) and to `Meta`
   (`guarded.rs`). Thread it through `gated`, add the `guard_callback` parameter, and pass `false`
   from `GuardedTool::call`. Update the other `CallView` sites to `false`:
   `fontaine-core/src/agent.rs`, `fontaine-evals/src/none.rs`,
   `fontaine-core/tests/common/pipeline.rs` (let the helper take the flag, or add a sibling
   helper for the marked call), `fontaine-core/tests/{scanner,scanner_model,user_hooks}.rs`, `fontaine-tools/tests/guarded.rs:235` (pass `false` to `guard_callback`).
2. **fontaine-mcp** — add `interaction.rs` (`Marked`, `marks`, `InteractionTap`, the constant)
   with unit tests. Wire the tap and `Marked` into `start` and `connect_transport` in
   `servers.rs`, and add `marked` to `GuardedSink` in `register.rs` (its `new` takes it and becomes `pub(crate)`). Re-export
   `REQUIRES_USER_INTERACTION` from `lib.rs`; core uses it as the audit rule. The tap only
   inserts into `Marked` (sticky for the connection, see Design §1). Update the stale
   comments to say the marker is the one declared value that can force a prompt: the module doc
   at `lib.rs:1-3` ("what a server declares ... reaches the guardian as untrusted text") and the
   comment at `register.rs:89-90` ("Declared hints travel with the definition as text ...
   whatever the server says").
3. **fontaine-core** — extend the forced-ask chain in `permissions.rs::evaluate` as above, with
   `fontaine_mcp::REQUIRES_USER_INTERACTION` as the audit rule and the prompt reason.
4. **Tests** — see Testing Strategy.
5. **Docs** — see Documentation.
6. **Master plan** — add a dated line to `tasks/fontaine_harness_plan.md` `## Decisions` recording
   the deviation from Claude Code: a server's `requiresUserInteraction` marker stays in force for
   the life of the connection once any `tools/list` response carried it (Claude Code follows the
   latest listing), because overlapping refreshes and late responses could otherwise let a stale
   page loosen it.

## Files to Modify

- `rust/fontaine-tools/src/gate.rs`, `rust/fontaine-tools/src/guarded.rs`,
  `rust/fontaine-tools/tests/guarded.rs`
- `rust/fontaine-mcp/src/interaction.rs` (new), `rust/fontaine-mcp/src/servers.rs`,
  `rust/fontaine-mcp/src/register.rs`, `rust/fontaine-mcp/src/lib.rs`
- `rust/fontaine-core/src/permissions.rs`, `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-evals/src/none.rs`
- `rust/fontaine-mcp/tests/common/{mod.rs,server.rs}`, `rust/fontaine-mcp/tests/mcp_registration.rs`
- `rust/fontaine-core/tests/common/pipeline.rs`, `rust/fontaine-core/tests/permission_rules.rs`,
  `rust/fontaine-core/tests/permissions_auto.rs`, `rust/fontaine-core/tests/permissions_screen.rs`, `rust/fontaine-core/tests/{scanner,scanner_model,user_hooks}.rs`
- `docs/mcp.md`, `docs/permissions.md`, `docs/configuration.md`, `tasks/design/permissions.md`,
  `research/research-claude-code-parity.md`
- `AGENTS.md`, `tasks/fontaine_harness_plan.md` (the workspace-trust Decision wording and the new
  sticky-marker deviation line)

## Decisions

- 2026-10-25: Read the marker with a transport tap rather than waiting for an upstream rig-rmcp change, because rig-rmcp 0.45 drops `_meta` and is pinned to an exact version.
- 2026-10-25: Use the tap rather than a second `tools/list` call, because a separate listing is a different response from the ones rig-rmcp registers from and can disagree with them; the tap sees exactly those responses (their ordering is handled by the sticky marker, not by the tap).
- 2026-10-25: AFK: Make `Marked` sticky for the life of the connection (insert on `true`, never remove; a new connection starts empty), recorded as a Claude Code deviation in the master plan — overlapping refreshes and late `tools/list` responses can reach the tap out of commit order, and insert-only is the one option that can never loosen the marker.
- 2026-10-25: AFK: Reword the governing "a project-declared executable can never change a permission decision" statements (`docs/configuration.md`, `AGENTS.md` security invariants, the master plan's workspace-trust Decision) to "can tighten but never loosen a permission decision" — the marker makes the old text literally false, project hook deny/ask already tighten today, and the qualified wording keeps the invariant's intent without weakening it.

## Documentation

- `docs/mcp.md` § The trust model: a new bullet. A tool whose `tools/list` entry sets
  `_meta["anthropic/requiresUserInteraction"]` to `true` asks on every call in every mode. It
  offers no session/always grant, is denied in `dontAsk` and headless runs, and isn't skipped by
  allow rules or hook allows. It's audited as `tier = ask_rule`,
  `rule = anthropic/requiresUserInteraction`. Include a `tools/list` JSON example. Also note that
  it's the only declared value that changes a decision, and only toward asking. Reword the
  statements the marker makes false: auto routes to the guardian and dontAsk/headless deny
  unless an allow rule covers it (lines 61-64), "what a server declares is only text ... never
  as a fact or a gate" (65-67), "rules are the way to pre-approve" (74-76), and project servers'
  tools being "ordinary `arbitrary` tools" (96-98) must each carve out the marker. State that
  once a server marks a tool, the marker holds until the server's connection ends (the next
  session), even if a later listing drops it.
- `docs/configuration.md` § Trust asymmetry (lines 120-121): "a project-declared executable can
  never change a permission decision" becomes "can tighten a permission decision (a hook's deny or
  ask, an MCP tool's `requiresUserInteraction` marker) but never loosen one".
- `AGENTS.md` § Security invariants, the "Project-scope config can only tighten" bullet: qualify
  "can never change a permission decision" to "can tighten but never loosen a permission
  decision". Wording only; the invariant keeps its intent.
- `tasks/fontaine_harness_plan.md`, the 2026-10-21 workspace-trust Decision: qualify "they never
  change a permission decision" the same way.
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
  - a later page listing a marked tool without the key (or with `false`) leaves it marked;
  - a stale unmarked response arriving after a marked one for the same tool leaves it marked;
  - non-list messages (notifications, other results) pass through unchanged and touch nothing;
  - every message is returned unchanged (equal to the input).
- **Integration — `fontaine-mcp/tests/mcp_registration.rs`.** Extend the test `Gate` double's
  `Seen` with the flag, and add a `grant_access_tool()` with the marker to `tests/common/server.rs`.
  - Over the in-process transport, `grant_access` reaches the gate with
    `requires_user_interaction = true` and `echo` with `false`.
  - After `replace_tools` drops the marker from `grant_access` and adds it to `echo`, the
    refreshed registrations show both marked (adding takes effect; removing waits for a new
    connection).
  - The streamable-HTTP test's scripted `tools/list` includes a marked tool, and the gate sees it
    marked. This pins the claim that `start`'s HTTP branch is tapped too, not just
    `connect_transport`.
- **Pipeline — `fontaine-core/tests/permission_rules.rs`** (with the auto cases in
  `permissions_auto.rs` and the screen case in `permissions_screen.rs`), for a marked
  `mcp__srv__grant` call:
  - manual and acceptEdits prompt with `tier = ask_rule`, the marker rule, and `Offers` with
    neither session nor always;
  - auto prompts, and the scripted guardian is never called (`permissions_auto.rs`); in
    `permissions_screen.rs` the screen is never called either;
  - an `allow` rule `mcp__srv__grant` still prompts (`permission_rules.rs`), and so does a
    PreToolUse hook `allow` (`permissions_auto.rs`, which has the allow-capable hook double);
  - `dontAsk` and headless deny with `tier = ask_rule`;
  - a `deny` rule on the tool still wins with `tier = deny_rule`;
  - the same call unmarked keeps today's behavior (an allow rule allows it).

  These pin the security property: the marker can only tighten, and no allow path skips it.
- The existing choke-point test stays green. No new tool or risk class is added.

## Open Questions

None.
