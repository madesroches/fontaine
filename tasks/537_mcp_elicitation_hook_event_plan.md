# MCP Elicitation Hook Event Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/537

## Overview

Fontaine serves MCP form-mode and URL-mode elicitation but has no hook event for it: the
terminal UI always shows a dialog, and headless runs, AFK mode, the doctor probe and subagent
calls always answer `cancel`. This plan adds an `Elicitation` hook event, modeled on Claude
Code's, that can accept, decline or cancel a server's request for input before any dialog
appears. It uses the existing hook machinery (same config shape, same trust rules: a hook the
repository defines can tighten but never loosen), and it fires on every surface — TUI,
headless and AFK — so a configured hook can answer where today everything cancels.

## Current State

- `rust/fontaine-tools/src/mcp/elicitation.rs` intercepts `elicitation/create` in
  `ElicitationTap`. `ElicitForm` (:29) and `ElicitUrl` (:38) carry `server`, `message` and
  `schema`/`url`. `ElicitReply` (:63) is `Accept(Map)` / `Decline` / `Cancel`. The `Elicitor`
  trait (:71) has `elicit` and `elicit_url` (default Cancel); `CancelAll` (:85) answers the
  doctor probe.
- The tap gates before asking: a request arriving while a subagent call is in flight is
  answered `Cancel` (`gate`, :781), a non-`http`/`https` URL is declined before asking
  (`start_url`, :820), and `deliver` (:746) schema-validates an `Accept`'s content,
  downgrading a violating answer to `Decline` with a warning.
- `rust/fontaine-core/src/agent/mcp.rs:576` — `ApproverElicitor` adapts the session
  `Approver` to `Elicitor`; `AgentRuntime::mcp_supervisor()` (:437) installs it as
  `options.elicitor` for every session, headless included.
- `rust/fontaine-core/src/permissions.rs` — `Approver::elicit`/`elicit_url` (:137-145)
  default to `Cancel`; `AfkApprover` (:219-269) cancels while AFK is on and is wrapped around
  every session approver in `rust/fontaine-core/src/agent/setup.rs:550`.
- Hook infrastructure: `HookEvent` (`rust/fontaine-core/src/config.rs:686-900`) enumerates 17
  events with per-event arms (`config_key`, `wire_name`, `matcher_kind`, `handlers`,
  `project_scope`, `default_timeout`, `check_matcher`). Deciding gates are traits in
  `permissions.rs` — `PreToolUseHook` (:402) with `can_allow()` (a project hook's `allow`
  never counts) and `PermissionDeniedHook` (:456) with `can_retry()`. `UserHook`
  (`rust/fontaine-core/src/user_hooks.rs`) implements them; `build_layer` (:1812-1878)
  applies the trust rules (project hooks dropped when the workspace is untrusted, project
  non-command handlers dropped, project `Stop`/`SubagentStop`/`TeammateIdle` ignored with a
  warning). `BuiltHooks` (:1711+) is assembled in `hook_stages`
  (`rust/fontaine-core/src/agent/setup.rs:762-850`).
- `docs/hooks.md:359` lists `Elicitation` (and `ElicitationResult`) under "What is not built";
  a plugin naming one loads with a warning (`plugins.rs:1286-1296`).
- Claude Code semantics (observed from the published hooks reference): `Elicitation` fires
  when a server requests input, before the dialog; the matcher filters on the MCP server
  name; input carries `mcp_server_name`, `message`, and optional `mode`, `url`,
  `elicitation_id`, `requested_schema`; output is `hookSpecificOutput` with
  `hookEventName: "Elicitation"`, `action` ∈ `accept|decline|cancel`, and `content` (form
  field values) for accept; top-level `decision: "block"` or exit code 2 declines; the
  `hookSpecificOutput` applies only on exit 0.

## Design

### 1. The `Elicitation` hook event

Add the variant to `HookEvent` in `rust/fontaine-core/src/config.rs`:

- `Elicitation` in `HookEvent::ALL`, `config_key()` → `"elicitation"`, `wire_name()` →
  `"Elicitation"`, `from_wire_name()` arm (so plugin `hooks.json` stops warning).
- `matcher_kind()` — matches on the MCP server name, like Claude Code. Reuse the string-list
  matcher shape `Notification` uses.
- `handlers()` — `COMMAND_HTTP`. No `prompt` handler: a model verdict auto-answering a
  server's request for input is a loosening this plan does not introduce. (Claude Code also
  supports `mcp_tool` handlers, which Fontaine does not build at all.)
- `project_scope()` — `Loads`. A project hook may decline or cancel (tighten) but its
  `accept` never counts (see `can_accept` below).
- `default_timeout()` — 30 seconds, the same default the other deciding events use; an
  elicitation blocks an in-flight tool call, like `PreToolUse`.
- `check_matcher()` — accepts the same string values as other list matchers.

Add the `hooks.elicitation` key to the project-scope allowlist in
`rust/fontaine-core/src/config_scope.rs:207-223` and the sample config at :551-628, and a
catalog row beside `hooks.user_prompt_submit` in `config_catalog.rs`.

### 2. The deciding gate

In `rust/fontaine-core/src/permissions.rs`, next to `PreToolUseHook`:

```rust
pub struct ElicitationEvent<'a> {
    pub server: &'a str,
    /// "form" or "url".
    pub mode: &'a str,
    /// Untrusted text from the server.
    pub message: &'a str,
    pub url: Option<&'a str>,
    pub elicitation_id: &'a str,
    pub requested_schema: Option<&'a Value>,
    pub session_id: &'a str,
    pub cwd: &'a Path,
    pub permission_mode: &'a str,
    pub workspace_roots: &'a [String],
}

pub trait ElicitationHook: Send + Sync {
    fn id(&self) -> String;
    fn matcher(&self) -> Option<String>;
    fn timeout(&self) -> Duration { Duration::from_secs(30) }
    /// Whether this hook's `accept` counts. A hook the repository defines can tighten a
    /// request but never answer for the person.
    fn can_accept(&self) -> bool { true }
    /// As `PreToolUseHook::gate`.
    fn gate<'a>(&'a self) -> BoxFuture<'a, Result<(), HookError>> { ... }
    fn run<'a>(&'a self, event: ElicitationEvent<'a>)
        -> BoxFuture<'a, Result<ElicitReply, HookError>>;
}
```

The decision reuses `ElicitReply` from `fontaine_tools::mcp` (already `Accept(Map)` /
`Decline` / `Cancel`), avoiding a parallel decision type.

Add a runner `run_elicitation_hooks` in `permissions.rs` (or `user_hooks.rs`, beside
`run_hooks`) that takes the frozen hooks, the plugin layer and the event, and applies:

1. Match each hook's matcher against `server`; run matched hooks (user layer first, then
   plugin layer).
2. Collect verdicts. Precedence: `Cancel` > `Decline` > `Accept` — the most conservative
   answer toward the server wins; `Accept` counts only from a hook with `can_accept()`.
3. A hook that errors, times out, exits non-zero (except the exit-2 decline below), or prints
   an unreadable verdict contributes **no verdict** and is reported through
   `Approver::warn`; it never changes the outcome on its own. Falling through to the dialog
   (or the unattended `cancel`) is the fail-closed answer: a flaky hook must be no worse than
   no hook.
4. Exit code 2 and top-level `decision: "block"` map to `Decline` (Claude Code parity: exit 2
   declines and the `hookSpecificOutput` is then ignored).

`impl ElicitationHook for UserHook` in `user_hooks.rs` mirrors `impl PreToolUseHook for
UserHook` (:1044-1117): it builds the wire event (base event fields plus
`mcp_server_name`, `message`, `mode`, `url`, `elicitation_id`, `requested_schema`), requires
JSON (`require_json`), reads `hookSpecificOutput.action` (accept/decline/cancel, else
`HookError::malformed`) and `hookSpecificOutput.content` (object; required for accept, must
be absent or empty for a URL-mode accept), and maps `blocked` (exit 2 / `decision: "block"`)
to `Decline`. `can_accept()` returns `origin.user_trust` exactly like `can_allow()`. An
accept verdict from a `can_accept() == false` hook is ignored with a warning, like a project
hook's `allow` today.

`build_layer` gains the arm: `HookEvent::Elicitation => built.elicitation.push(Arc::new(user))`,
and `BuiltHooks` gains `pub elicitation: Vec<Arc<dyn ElicitationHook>>`. Because
`handlers()` is `COMMAND_HTTP`, the existing rule that drops a project `http`/`prompt` hook
applies unchanged. `discards_system_message` — the elicitation event is deciding, so
`systemMessage` reaches the human via `Approver::warn` (do not add it to the discard list).

### 3. Wiring the gate into the elicitation path

Thread the request id so the hook input can carry `elicitation_id`: add `pub id: String` to
`ElicitForm` and `ElicitUrl` in `rust/fontaine-tools/src/mcp/elicitation.rs`, filled from the
JSON-RPC request id in the tap's `start`/`start_url` (empty in hand-built test fixtures).

In `rust/fontaine-core/src/agent/mcp.rs`, add a `HookElicitor` and install it in
`mcp_supervisor` (it sets `options.elicitor` at :437):

```rust
pub(crate) struct HookElicitor {
    hooks: Vec<Arc<dyn ElicitationHook>>,
    plugin_hooks: LivePlugins,
    inner: ApproverElicitor,
    approver: Arc<dyn Approver>, // for warnings
}
```

- `elicit(form)`: build the `ElicitationEvent` (mode `"form"`, `requested_schema` from
  `form.schema`), run the hooks. A counted verdict short-circuits — no dialog, no approver
  call. No verdict (or an ignored accept) falls through to `inner.elicit(form)`.
- `elicitation_url(request)`: same, mode `"url"`. A hook `Accept` is **never** honored for
  URL mode: it is ignored with a warning and the flow falls through to the confirmation
  dialog. `Decline`/`Cancel` are honored. (Deviation from Claude Code, where a hook accept
  opens the link; recorded in the master plan. Fontaine's outward-action rule — a link opens
  only after the person confirms — outranks parity here.)
- `gate()` on each hook runs before its `run`, outside its timeout, like the other gates.

Order of decisions for one request, end to end:

1. `ElicitationTap::gate` — a subagent call in flight answers `Cancel`, as today; the hook
   does not fire. The subagent invariant is unchanged.
2. Non-`http`/`https` URL declined in the tap, as today.
3. `Elicitation` hooks run (every surface: TUI, headless, AFK).
4. No verdict → the approver chain, as today: `AfkApprover` cancels while AFK is on; headless
   `DenyAll`/trait defaults cancel; the TUI shows the dialog.

Because the hook layer sits inside the tap and ahead of `ApproverElicitor`, a configured hook
can answer elicitations in headless runs and AFK mode — where everything cancels today — and
an unconfigured session behaves exactly as before. The doctor probe keeps `CancelAll` and is
untouched. `deliver`'s schema validation still downgrades a hook-supplied violating `Accept`
to `Decline`, so a hook cannot send the server a malformed answer.

### 4. What is not in this plan

- `ElicitationResult` (the post-response override hook) stays unbuilt, listed in
  `docs/hooks.md` "What is not built".
- No `prompt` handler for `Elicitation` (see Design 1).
- No hook firing for requests the tap gates off (subagent calls, non-HTTP URLs).

## Implementation Steps

1. **Event.** Add the `Elicitation` variant and its arms in `config.rs`; extend
   `config_scope.rs` (allowlist + sample) and `config_catalog.rs`; add the wire-name round
   trip so plugin `hooks.json` names load.
2. **Gate types.** Add `ElicitationEvent` and `ElicitationHook` in `permissions.rs`; add
   `ElicitationReply` reuse of `ElicitReply`; write `run_elicitation_hooks` with the
   precedence, trust and no-verdict rules.
3. **UserHook.** Implement `ElicitationHook for UserHook` in `user_hooks.rs` (wire event
   build, `require_json`, action/content parsing, exit-2 → `Decline`, `can_accept` from
   `user_trust`); extend `BuiltHooks` and `build_layer`; keep `systemMessage` warned.
4. **Types.** Add `id: String` to `ElicitForm`/`ElicitUrl`; fill it in the tap; fix all
   construction sites (tests, fixtures, the TUI payload pass-throughs are unaffected).
5. **Wiring.** Add `HookElicitor` in `agent/mcp.rs`; give `AgentRuntime` the frozen
   `elicitation` hook list and `LivePlugins` from `hook_stages` (`setup.rs`); install the
   wrapper in `mcp_supervisor`.
6. **Tests.** See Testing Strategy.
7. **Docs.** Update `docs/hooks.md` (event table, matcher table, decision control, config
   example, remove `Elicitation` from "What is not built", trust notes), `docs/mcp.md`
   ("Requests for input": a hook can answer before the dialog; headless/AFK cancel only when
   no hook answers), `docs/headless.md`, `docs/afk.md`.
8. **Master plan.** Add dated Decisions lines under `### MCP` in
   `tasks/fontaine_harness_plan.md`: the event exists and fires before the dialog on every
   surface; the URL-mode deviation (hooks never auto-accept a link); headless/AFK cancel only
   when no hook answers. Update the two "not built" lines that name the `Elicitation` hook.

## Files to Modify

- `rust/fontaine-core/src/config.rs` — `HookEvent` variant + arms.
- `rust/fontaine-core/src/config_scope.rs` — project allowlist + sample config.
- `rust/fontaine-core/src/config_catalog.rs` — catalog row.
- `rust/fontaine-core/src/permissions.rs` — `ElicitationEvent`, `ElicitationHook`,
  `run_elicitation_hooks`.
- `rust/fontaine-core/src/user_hooks.rs` — `impl ElicitationHook for UserHook`, `BuiltHooks`,
  `build_layer`.
- `rust/fontaine-tools/src/mcp/elicitation.rs` — `id` on `ElicitForm`/`ElicitUrl`, tap fills
  it.
- `rust/fontaine-core/src/agent/mcp.rs` — `HookElicitor`, `mcp_supervisor` install.
- `rust/fontaine-core/src/agent/setup.rs` — carry the elicitation hooks to the runtime.
- `docs/hooks.md`, `docs/mcp.md`, `docs/headless.md`, `docs/afk.md`.
- `tasks/fontaine_harness_plan.md` — Decisions.
- Tests: `rust/fontaine-core/tests/user_hooks.rs`, `rust/fontaine-core/tests/lifecycle_hooks.rs`
  (or a new `elicitation_hooks.rs`), `rust/fontaine-core/tests/afk.rs`,
  `rust/fontaine-tools/src/mcp/elicitation.rs` tests, `rust/fontaine/tests/lifecycle_hooks_headless.rs`.

## Trade-offs

- **Wrap the `Elicitor` vs. wrap the `Approver`.** Wrapping the `Elicitor` in `mcp_supervisor`
  hits exactly the elicitation path on every surface (TUI, headless, AFK) with one insertion
  point, and leaves `ask_user` and every other approver concern alone. Wrapping the
  `Approver` would spread hook knowledge into the permission pipeline and still need the same
  trust plumbing.
- **Reuse `ElicitReply` vs. a new decision type.** Reusing it keeps the gate, the tap and the
  wire mapping in one vocabulary; a parallel type would need a conversion at every boundary.
- **No-verdict fall-through vs. fail-closed cancel.** A hook that errors or times out falls
  through to the dialog (attended) or the unattended `cancel`, with a warning. Auto-cancel on
  failure would make a flaky hook strictly worse than no hook and would deny the person the
  chance to answer in an attended session. The event's conservative outcomes (decline,
  cancel) remain available to a working hook.
- **Precedence cancel > decline > accept.** Any objection beats an acceptance; cancel beats
  decline as the most conservative answer. Accept counts only when no other hook objected and
  the accepting hook is trusted.
- **URL-mode hooks: decline/cancel only.** Honoring form-mode accepts but never URL-mode
  accepts keeps the existing "a link opens only after the person confirms" rule absolute,
  while still letting hooks silence URL requests. This is a deliberate deviation from Claude
  Code, recorded in the master plan.

## Decisions

- The `Elicitation` hook fires before any dialog on every surface (TUI, headless, AFK); with
  no hook configured, behavior is byte-for-byte today's.
- Matcher filters on the MCP server name; handlers are `command` and `http` only.
- A project (untrusted-origin) hook and any plugin hook may decline or cancel, but its
  `accept` never counts; a user-trusted hook may accept, decline or cancel.
- Precedence among verdicts is cancel > decline > accept; a hook error, timeout or unreadable
  verdict contributes nothing and falls through, with a warning.
- Exit code 2 and top-level `decision: "block"` decline the request (Claude Code parity).
- A hook can never auto-accept URL-mode elicitation: the link opens only after the person
  confirms in the terminal UI. Deviation from Claude Code, recorded in the master plan.
- Elicituations arriving during a subagent call or with a non-HTTP(S) URL are cancelled or
  declined by the tap as today, without firing the hook.
- `ElicitationResult` stays unbuilt.

## Documentation

- `docs/hooks.md`: add `Elicitation` to the event table, the matcher table (MCP server name)
  and the decision-control section with a config example; remove it from "What is not built"
  (keeping `ElicitationResult`); document the trust rules (project/plugin accepts ignored)
  and the URL-mode restriction.
- `docs/mcp.md` ("Requests for input"): an `Elicitation` hook can answer before the dialog;
  headless runs, AFK mode and subagent calls still answer `cancel` when no hook answers; the
  URL-mode confirmation rule is absolute.
- `docs/headless.md`, `docs/afk.md`: the `cancel` answer applies only when no hook answers.
- `tasks/fontaine_harness_plan.md`: dated Decisions lines under `### MCP`.

## Testing Strategy

Unit (`cargo test`):

- `config.rs` / `config_scope.rs`: `[[hooks.elicitation]]` parses; wire-name round trip
  (`Elicitation` ↔ `elicitation`); matcher check accepts server names; the sample config
  round-trips.
- `user_hooks.rs`: a user command hook is kept; a project command hook is kept with
  `can_accept() == false`; a project `http` hook lands in `ignored_project_handlers`; a plugin
  hook loads with `can_accept() == false`.
- The gate runner: precedence cancel > decline > accept across several hooks; an untrusted
  accept is ignored with a warning; a hook error, timeout or malformed verdict yields no
  verdict; exit 2 and `decision: "block"` map to `Decline`; an accept's `content` passes
  through unchanged; the wire JSON carries `mcp_server_name`, `message`, `mode`, `url`,
  `elicitation_id` and `requested_schema`.
- `HookElicitor`: a form accept/decline/cancel short-circuits the inner elicitor; no verdict
  forwards; a URL-mode accept is ignored and falls through, while URL-mode decline/cancel are
  honored.
- `permissions.rs` defaults unchanged: `DenyAll` and `AfkApprover` still cancel when no hook
  answers.
- The tap: the request id reaches `ElicitForm`/`ElicitUrl`.

Integration:

- `rust/fontaine-tools/tests/mcp_registration.rs` style: through a real rmcp peer, an
  elicitation answered by a hook never reaches the scripted elicitor, and the wire response
  carries the hook's action; a hookless request still reaches the elicitor.
- `rust/fontaine-core/tests/afk.rs` style: while AFK is on, a trusted hook's accept wins; with
  no hook the answer stays `cancel`.
- Headless (`rust/fontaine/tests/lifecycle_hooks_headless.rs` style, or the
  `project_executables.rs` elicit flow with a hook configured): a `fontaine -p` run answers
  an elicitation from a hook config instead of cancelling.

## Manual Verification

1. Run `cargo run -p fontaine` with a user-scope
   `[[hooks.elicitation]]` command hook that prints
   `{"hookSpecificOutput":{"hookEventName":"Elicitation","action":"accept","content":{...}}}`
   for the `mcp_double` server's form-eliciting tool: the tool completes with no dialog. With
   the hook removed, the dialog appears. Not automated: the TUI dialog path is exercised, and
   a human confirms the hook preempted it.
2. Same server in a headless run (`fontaine -p`): with the hook, the elicitation is answered;
   without it, the tool sees `cancel`, as documented.

## Open Questions

None.
