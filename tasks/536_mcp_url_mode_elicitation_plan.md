# MCP URL-mode elicitation Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/536

## Overview

Fontaine already serves MCP form-mode elicitation: a server can ask the person to fill in a
form while a tool runs, and the answer goes back to the server. This plan adds URL-mode
elicitation, where the server asks the person to open a link. The link is never opened
automatically; the terminal UI shows the URL and asks for confirmation, and only then hands it
to the system's default browser. Headless runs, AFK mode, the doctor probe and calls made by
subagents keep answering `cancel`, as they do for form mode.

## Current State

- `rust/fontaine-tools/src/mcp/servers.rs` declares elicitation in `client_config()` with
  `ElicitationCapability::new().with_form(FormElicitationCapability::new())` and pins the
  protocol to `ProtocolVersion::LATEST_WITH_INITIALIZE`.
- `rust/fontaine-tools/src/mcp/elicitation.rs` intercepts `elicitation/create` requests in
  `ElicitationTap`. A `UrlElicitationParams` request is answered `Decline` immediately, with a
  warning log.
- `rust/fontaine-core/src/permissions.rs` adds `Approver::elicit`, defaulting to `Cancel`.
  `AfkApprover` cancels while AFK is on and forwards otherwise.
- `rust/fontaine-tui/src/elicitation.rs` draws the form-mode dialog. The `Prompt::Elicit`
  variant carries an `ElicitForm`; `ChannelApprover::elicit` queues it to the UI.
- `docs/mcp.md` documents that URL mode is always declined.

## Design

### 1. Declare URL capability

In `rust/fontaine-tools/src/mcp/servers.rs`, change `client_config()` to also declare URL mode:

```rust
capabilities.elicitation = Some(
    ElicitationCapability::new()
        .with_form(FormElicitationCapability::new())
        .with_url(UrlElicitationCapability::new()),
);
```

Keep the protocol pinned to `ProtocolVersion::LATEST_WITH_INITIALIZE`; URL mode, like form mode,
arrives as a request on that version.

### 2. URL-mode request handling in the tap

Extend `rust/fontaine-tools/src/mcp/elicitation.rs`:

- Add `pub struct ElicitUrl { pub server: String, pub message: String, pub url: String }`.
- Extend the `Elicitor` trait with a second method:
  ```rust
  fn elicit_url<'a>(&'a self, request: ElicitUrl) -> BoxFuture<'a, ElicitReply>;
  ```
  The default implementation returns `Cancel`, so existing elicitors keep compiling and
  headless/doctor/test surfaces stay `Cancel`.
- In `ElicitationTap::take`, when a `UrlElicitationParams` arrives, apply the same gating as form
  mode: answer `Cancel` immediately when a subagent's call is in flight (`flight.sub > 0`).
  Otherwise call `elicitor.elicit_url(ElicitUrl { server, message, url })` and map the reply:
  - `Accept(_)` → `ElicitationAction::Accept` with empty content.
  - `Decline` → `Decline`.
  - `Cancel` → `Cancel`.
- Keep the telemetry event `mcp_elicitation` with the action and the server name (or
  `third-party`), never the URL, message or `elicitation_id`.

### 3. Core wiring

- In `rust/fontaine-core/src/permissions.rs`, give `Approver::elicit_url` the same default
  (`Cancel`) and have `AfkApprover` cancel while AFK is on.
- In `rust/fontaine-core/src/agent/mcp.rs`, the `ApproverElicitor` wrapper already implements
  `Elicitor` by forwarding `elicit`. Add `elicit_url` forwarding to the approver.

### 4. Terminal UI dialog

Add `rust/fontaine-tui/src/url_elicitation.rs` with a `UrlElicitationPanel`:

- Header: `MCP server `<name>` wants to open a link`.
- Body: the server's message (cleaned of control characters, newlines preserved), then the URL
  in bold so it is clearly visible and can be copied.
- Footer: `y opens the link · n or Esc declines · Ctrl-D declines`.
- Keys: `y` opens, `n`/`Esc`/`Ctrl-D` decline. The usual `ANSWER_GRACE` applies.
- Opening the URL: use the `open` crate's `open_detached(url)` (or `open::that(url)` on
  non-Unix). If it fails, show a one-line error in the dialog and decline.
- Validation: before opening, parse the URL with `url::Url`. Only `http:` and `https:` schemes
  are opened; anything else is declined with a warning. The validated URL string is what is
  handed to the opener, not the raw server string.
- The reply is `ElicitReply::Accept(Empty)` on success, `Decline` on decline or open failure.

Wire the panel through the same `Prompt::Elicit` path, carrying an extended `ElicitForm` enum
or a parallel prompt variant. The smallest change is to widen `ElicitForm` to an enum:

```rust
pub enum ElicitRequest {
    Form(ElicitForm),
    Url(ElicitUrl),
}
```

and carry that in `Prompt::Elicit`. Update `ChannelApprover::elicit` to send the right variant,
and `fontaine-tui/src/app.rs` and `view.rs` to render `UrlElicitationPanel` when the request is a
URL. `App::answer_for_afk` and `App::prune_closed` close/decline URL-mode elicitations the same
way as form mode.

### 5. Permission-floor interaction

URL-mode elicitation is treated exactly like form mode for the permission floor:

- It is never reachable in headless runs, AFK mode, the doctor probe, or while a subagent's call
  is in flight on the connection. Those cases answer `Cancel`.
- It is not pre-approvable by any rule or hook. There is no "always open" option.
- It does not change any permission decision. The call that caused it was already approved.

### 6. Security

- The URL is untrusted server text. It is shown but never opened automatically.
- Only `http:` and `https:` URLs are accepted; any other scheme is declined.
- The URL is parsed and the parsed string is what is opened, so escapes and control characters
  cannot change the meaning.
- The opened URL is not logged, transcribed, audited or sent to telemetry. Only the action
  (`accept`/`decline`/`cancel`) and the server identifier are logged.
- The dialog warns that the link goes to an external site chosen by the server.

## Implementation Steps

1. **Capability.** Add `UrlElicitationCapability` to `client_config()` in
   `rust/fontaine-tools/src/mcp/servers.rs`.
2. **Core types.** Add `ElicitUrl` and extend `Elicitor` with `elicit_url` in
   `rust/fontaine-tools/src/mcp/elicitation.rs`. Update `Approver`/`AfkApprover` in
   `rust/fontaine-core/src/permissions.rs` and `ApproverElicitor` in
   `rust/fontaine-core/src/agent/mcp.rs`.
3. **Tap handling.** In `ElicitationTap`, route `UrlElicitationParams` through the new elicitor
   method, with the same subagent/call-ended cancellation rules as form mode.
4. **UI panel.** Create `rust/fontaine-tui/src/url_elicitation.rs` with `UrlElicitationPanel`.
   Add `ElicitRequest` enum (or equivalent) and plumb it through `Prompt::Elicit`,
   `ChannelApprover`, `App`, `view.rs` and `linear.rs`.
5. **Tests.** Add unit tests for the tap (URL request is intercepted, subagent call cancels it,
   invalid scheme declines, valid `http`/`https` accepts), the URL parser/validation, and the
   panel's key handling. Add an integration test that a server sending a URL-mode elicitation
   gets the right protocol response through a scripted elicitor.
6. **Docs.** Update `docs/mcp.md` to describe URL-mode elicitation, the confirmation dialog, the
   `http`/`https` restriction and the headless/AFK/subagent behavior. Update `docs/tui.md` with
   the keys.
7. **Master plan.** Add a dated Decisions line to `tasks/fontaine_harness_plan.md` under `### MCP`
   recording that URL-mode elicitation is supported and opens only after explicit confirmation.

## Files to Modify

- `rust/fontaine-tools/src/mcp/servers.rs` — capability declaration.
- `rust/fontaine-tools/src/mcp/elicitation.rs` — `ElicitUrl`, `Elicitor::elicit_url`, tap routing.
- `rust/fontaine-core/src/permissions.rs` — `Approver::elicit_url`, `AfkApprover`.
- `rust/fontaine-core/src/agent/mcp.rs` — `ApproverElicitor` forwarding.
- `rust/fontaine-tui/src/url_elicitation.rs` (new) — URL-mode dialog.
- `rust/fontaine-tui/src/lib.rs` — `Prompt::Elicit` variant or `ElicitRequest` enum.
- `rust/fontaine-tui/src/app.rs`, `rust/fontaine-tui/src/view.rs`,
  `rust/fontaine-tui/src/linear.rs` — render and key routing for URL mode.
- `rust/fontaine-tui/src/notify.rs`, `rust/fontaine-tui/src/spinner.rs` — prompt enumeration if
  `Prompt::Elicit` shape changes.
- `rust/fontaine-tools/Cargo.toml` — add `open` dependency (already pulled transitively? verify).
- `docs/mcp.md`, `docs/tui.md`.
- `tasks/fontaine_harness_plan.md` — Decisions.

## Trade-offs

- **Open automatically vs. ask first.** Asking first is slower but matches the outward-action
  concern in the issue: a server must not drive the browser without the person's consent.
- **Use `open` crate vs. hand-roll xdg-open/open.** The `open` crate is already common in the
  Rust ecosystem and handles Windows, macOS and Linux. It is worth adding only if not already
  present in the workspace; otherwise use the existing dependency.
- **Separate panel vs. reuse `ConfirmPrompt`.** A separate panel keeps the MCP-specific wording
  and URL display behavior in one place; `ConfirmPrompt` could be reused but would need several
  one-off additions for URL validation and the external-site warning.
- **Accept with empty content vs. a result object.** The MCP spec defines URL mode as the client
  directing the user to the URL; the server detects completion on its own. An empty `Accept`
  content is sufficient and avoids inventing a response shape.

## Decisions

- URL-mode elicitation is declared and served, but the link opens only after the person
  confirms it in the terminal UI.
- Only `http:` and `https:` URLs are opened; any other scheme is declined.
- There is no "always allow" for URL-mode elicitation; every request is confirmed.
- Headless runs, AFK mode, the doctor probe and subagent calls answer `cancel`, unchanged from
  form mode.
- The telemetry event for URL mode uses the same `mcp_elicitation` event with action and server
  only, never the URL or message.

## Documentation

- `docs/mcp.md`: replace the paragraph that says URL mode is declined with a description of the
  confirmation dialog, the scheme restriction and the unchanged headless/AFK/subagent behavior.
- `docs/tui.md`: add the URL-mode dialog keys.

## Testing Strategy

Unit tests (`cargo test`):
- `elicitation.rs` tap tests:
  - A `UrlElicitationParams` request is intercepted and not forwarded to rmcp.
  - A valid `http` URL answered `Accept` sends `ElicitationAction::Accept` with empty content.
  - A valid `https` URL answered `Decline` sends `Decline`.
  - A URL with a non-HTTP scheme is declined without reaching the elicitor.
  - A URL request while a subagent call is in flight is answered `Cancel`.
  - A URL request emits `mcp_elicitation` with the right action.
- `url_elicitation.rs` panel tests:
  - `y` opens a valid URL and returns `Accept`.
  - `n`, `Esc` and `Ctrl-D` return `Decline`.
  - An invalid scheme is rejected and returns `Decline`.
  - The grace period blocks `y`/`n` initially.
- `permissions.rs`: `DenyAll`/default `elicit_url` cancels, and `AfkApprover` cancels while AFK.

Integration:
- `fontaine-tools/tests/mcp_registration.rs`: an in-process rmcp server whose tool sends a
  URL-mode elicitation gets the expected protocol response through a scripted elicitor.

## Manual Verification

1. `cargo run -p fontaine` with a server that sends a URL-mode elicitation (extend
   `rust/fontaine-tools/tests/mcp_common/server.rs` with such a tool). Ask the model to call the
   tool. The URL-mode dialog appears; pressing `y` opens the link in the default browser and the
   run continues.
2. Reproduce the dialog layout in a real tmux at 80×24 and in screen-reader mode, as
   `tasks/design/testing.md` requires for layout bugs.

## Open Questions

- Is the `open` crate already a transitive dependency of the workspace? If not, is adding it
  acceptable or should we shell out to `xdg-open`/`open`/`start` directly?
