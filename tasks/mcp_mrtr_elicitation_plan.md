# MCP MRTR Elicitation Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/542

## Overview

At protocol 2026-07-28 an MCP server asks for input by answering `tools/call`, `prompts/get` or
`resources/read` with an `InputRequiredResult` (`resultType: "input_required"`, `inputRequests`,
`requestState`); the client fulfils the requests and retries the same call with `inputResponses`
and the echoed `requestState` (SEP-2322, "MRTR"). rig-rmcp maps that result to
`UnexpectedResponse`, so Fontaine pins the offered version to 2025-11-25 (`client_config()`), where
the request arrives as `elicitation/create` and `ElicitationTap` serves it. This plan teaches the
tap to drive MRTR rounds itself, reusing the existing dialog path (hooks, subagent cancel, form
validation, URL confirmation, telemetry), and lifts the pin.

## Current State

- `rust/fontaine-tools/src/mcp/servers.rs:545` `client_config()` calls
  `.with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)`; the test at `:1325` asserts it.
- `serve()` (`servers.rs:755`) wraps the transport as `ElicitationTap<MetaTap<transport>>` and hands
  it to `McpClientHandler::connect`, which uses the legacy `initialize` lifecycle only
  (`rig-rmcp-0.46.0/src/handler.rs:213`; its refresh helpers are `pub(crate)`).
- `ElicitationTap` (`mcp/elicitation.rs:675`) sees every message. In `receive()` it takes
  `ServerRequest::ElicitRequest` off the wire, gates it (`gate`: a subagent's call in flight
  answers `cancel`), spawns the `Elicitor` (`start`/`start_url`), validates the answer
  (`deliver`) and writes a client response with `respond()`. `end_calls()` cancels pending dialogs
  once `CallTracker` shows no call in flight; a `notifications/cancelled` for a pending id aborts
  it. Its `send()` is a pass-through.
- rig-rmcp's tool call (`native.rs:210`) sends one `tools/call` and fails with
  `ServerResult::InputRequiredResult` as `UnexpectedResponse`. rmcp's own `call_tool` /
  `get_prompt` / `read_resource` drive MRTR through the handler, whose `create_elicitation` declines
  (the tap never sees those), and `mcp_supervisor.rs:517` uses the `_once` resource read and turns
  `InputRequired` into a tool error.
- rmcp 3.9: `CallToolRequestParams`, `GetPromptRequestParams` and `ReadResourceRequestParams` carry
  `input_responses: Option<BTreeMap<String, Value>>` and `request_state: Option<String>`;
  `InputRequest` is `CreateMessage | Elicitation | ListRoots`; `DEFAULT_MRTR_MAX_ROUNDS = 10`;
  rmcp's driver backs off 50 ms doubling to 250 ms on a round with `requestState` only.
- A server answering `initialize` with 2026-07-28 makes rmcp attach per-request `_meta` itself
  (`RoleClient::configure_direct_peer`); an rmcp server always answers `initialize` with the newest
  handshake version (2025-11-25), so against it the legacy path stays in use.

## Design

The driver lives in `ElicitationTap`, below rig-rmcp and rmcp, so one implementation serves tool
calls, prompt fetches and resource reads on every connection kind, and rmcp never sees an
`InputRequiredResult`.

### Tracking calls

`send()` records a clone of each outgoing `ClientRequest::{CallToolRequest, GetPromptRequest,
ReadResourceRequest}` by its `RequestId` (`eligible`). The entry is dropped when its final
response, or an error, passes through `receive()`.

### A round

When a `ServerJsonRpcMessage::Response` carries `ServerResult::InputRequiredResult` and its id is
in `eligible`, the tap takes it (rmcp never sees it) and keeps a `Driven` record keyed by the
original id: the cloned request, the round count, the answers collected, the keys still awaited
and the id of the retry in flight. A result for an id not in `eligible` passes through unchanged.

For each `(key, InputRequest::Elicitation(request))` the tap runs the existing `start` /
`start_url` with a synthetic `RequestId` (`fontaine-mrtr/<call id>/<round>/<key>`) recorded in a map
to `(call id, key)`. `respond()` checks that map first: an MRTR id stores
`serde_json::to_value(ElicitResult)` under its key instead of sending a client response, and emits
the same `mcp_elicitation` telemetry event. Everything before it (gate, hooks via the `Elicitor`,
`ElicitForm::validate`, URL confirmation, `openable_url`) is reused unchanged, so a subagent's
call, a headless run and the doctor probe still answer `cancel` / decline exactly as on the wire
path.

When no key is awaited, the tap sends the retry through `inner.send` on a spawned task (as
`respond()` does, because `receive` futures are dropped): the cloned request with
`input_responses` (omitted when empty) and `request_state` set, under a fresh string id
`fontaine-mrtr-retry/<n>`. A round with `requestState` only waits rmcp's 50 ms doubling to 250 ms
before retrying. A result with neither field is a protocol error.

The retry's response, matched by id and rewritten to the original id, is either the final answer
(returned from `receive`, so rmcp and `MetaTap` see an ordinary response) or another
`InputRequiredResult`, which starts the next round. The 11th round fails the call.

### Failing a call

A synthesized `ServerJsonRpcMessage::error` to the original id, queued on an `outbox` that
`receive()` drains first, ends a call the tap cannot continue: a `CreateMessage` or `ListRoots`
input request (Fontaine declares neither capability), the round cap, an answer that cannot be
serialised, or a send failure on the retry. The text names the cause; the full chain stays in the
log.

### Cancelling

rmcp sends `notifications/cancelled` for the original id when a call times out or is dropped. In
`send()` the tap, for a driven id, aborts that call's pending dialogs and drops its `Driven`
record; if a retry is in flight it forwards the notification with the retry's id, otherwise it
swallows it (the server holds no live request). A late response for a dropped round's retry id is
swallowed. `end_calls()` (no call in flight) discards the pending dialogs of a driven call instead
of answering them.

### Protocol version

`client_config()` stops overriding the version, so `ClientConfig` offers rmcp's `LATEST`
(2026-07-28) in `initialize`. A server that supports both still answers with the version it picks
(rmcp servers answer 2025-11-25) and is served on the existing wire path; a server that answers
2026-07-28 is served by the driver. The capability declaration (form and URL) is unchanged.
rig-rmcp's `connect` cannot start the `server/discover` lifecycle, so a server that rejects
`initialize` altogether is out of reach until rig-rmcp exposes a lifecycle choice (follow-up).

## Implementation Steps

1. `mcp/elicitation.rs`: add `eligible`, `driven`, `mrtr_inputs`, `outbox` and a round counter to
   `ElicitationTap`; record in `send()`, handle the cancel notification in `send()`.
2. `mcp/elicitation.rs`: in `take()` handle the `InputRequiredResult` response and the retry-id
   rewrite; split the answer destination in `respond()`; make `end_calls()` and `receive()`
   (outbox drain) aware of driven calls. Keep `take()` returning "taken or not" for the wire path.
3. `mcp/servers.rs`: drop `with_protocol_version` and the version import, rewrite the doc comment
   of `client_config()`, update the test at `:1325` to assert the default (`LATEST`).
4. Tests (see Testing Strategy).
5. Docs, CHANGELOG, plan Decisions (see below).
6. File follow-up issues: rig-rmcp lifecycle option (`server/discover`) so 2026-07-28-only servers
   connect; the `tasks` result type (`resultType: "task"`) for `tools/call`.

## Files to Modify

- `rust/fontaine-tools/src/mcp/elicitation.rs` — the driver and its unit tests
- `rust/fontaine-tools/src/mcp/servers.rs` — lift the pin, test
- `rust/fontaine-tools/tests/mcp_mrtr.rs` — new end-to-end test
- `docs/mcp.md` (the closing paragraph of the elicitation section), `docs/headless.md` if its
  elicitation text names the version
- `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (`## Decisions`, `### MCP`)

## Trade-offs

- **Driver in the tap vs. replacing rig-rmcp's tool wrapper.** The wrapper's `call` is private and
  guarded by `GuardedSink`; replacing it would duplicate rig-rmcp's argument, meta, timeout and
  result handling. The tap already owns the dialogs and sees the one place every call, prompt and
  read crosses, and covers the `_once` resource read without further code.
- **Driver in the tap vs. letting rmcp drive and implementing `create_elicitation`.** rig-rmcp's
  handler declines, the elicitor can't be reached from `McpClientHandler`'s generic sink, and tool
  calls bypass rmcp's `call_tool` anyway.
- **Synthetic ids** keep the existing `Pending` / dialog machinery keyed by `RequestId` instead of
  a second dialog path.

## Decisions

(none yet)

## Documentation

- `docs/mcp.md`: replace the closing "Fontaine offers the MCP protocol version 2025-11-25" paragraph
  with the current behaviour (requests for input are served at both the request form and the
  2026-07-28 result form; same dialogs, same limits; a call is retried with your answer, up to 10
  rounds).
- `docs/headless.md`, `docs/hooks.md`: check their elicitation text; nothing to change expected.
- `CHANGELOG.md` Added entry (#542); a plan `## Decisions` line at the end of `### MCP`, in the
  "supersedes the 2026-11-08 …" form for the protocol-version pin line.

## Testing Strategy

Unit tests in `elicitation.rs` over the existing scripted `Script` transport (no live server):
- a form round: accepted answer produces one retry whose id is fresh, whose params carry
  `inputResponses` under the server's key and the echoed `requestState`, and whose response reaches
  `receive()` under the original id;
- two keys answered out of order produce exactly one retry; decline and cancel answers appear in
  `inputResponses`; an answer failing form validation becomes decline (as on the wire path);
- URL mode: confirmed, and a non-http(s) link declined;
- a subagent's call in flight answers `cancel` with no dialog; a headless / null elicitor cancels;
- a `requestState`-only round retries after the back-off (`tokio::time::pause`);
- a second `InputRequiredResult` starts round two; the 11th fails with an error response to the
  original id;
- a sampling or roots input request, an empty result, and a retry send failure each produce one
  error response;
- `prompts/get` and `resources/read` rounds work the same; an `InputRequiredResult` for an id the
  tap did not record passes through;
- cancel while a dialog is open aborts it and sends no retry; cancel with a retry in flight
  forwards the notification with the retry id; a late response for the dropped round is swallowed;
  the last call ending discards a driven call's dialogs;
- the existing wire-path tests stay unchanged and green.

Integration, `rust/fontaine-tools/tests/mcp_mrtr.rs`: a hand-written JSON-RPC server on a
`tokio::io::duplex` pipe (rmcp's server cannot be made to answer `initialize` with 2026-07-28)
that answers `initialize` with 2026-07-28, lists one tool, and answers `tools/call` with
`input_required` before a complete result. Connected through `connect_transport` with a recording
`Elicitor`: the model-facing tool result is the complete one, the elicitor saw the form once, and
the server received the retry with the answer. This pins the wiring (`ElicitationTap` under
`MetaTap`, rmcp's response matching with the rewritten id) that unit tests with a scripted
transport cannot, and the failure would be silent (a hang or an unanswered dialog).

## Open Questions

- Does a real 2026-07-28 server answer `initialize` with 2026-07-28, or only support `server/discover`?
  The driver and the pin lift are correct either way, but only the former is reachable until the
  lifecycle follow-up lands.
