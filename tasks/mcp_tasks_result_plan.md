# MCP Tasks Result Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/614

## Overview

At protocol 2026-07-28 a server may answer `tools/call` with a `CreateTaskResult`
(`resultType: "task"`, SEP-2663 Tasks extension) and the client then polls `tasks/get` for the
outcome. rig-rmcp maps that result to `UnexpectedResponse`, so the call fails. This plan has
`ElicitationTap` drive the task to completion below rig-rmcp, the way it already drives
`input_required` rounds, so rmcp sees only the call's final `CallToolResult` or error, and
declares the tasks extension so servers know the client can take a task handle.

## Current State

- `ElicitationTap` (`rust/fontaine-tools/src/mcp/elicitation.rs:744`) records each outgoing
  `CallToolRequest` / `GetPromptRequest` / `ReadResourceRequest` in `eligible` (`outgoing`, `:1214`),
  intercepts an `InputRequiredResult` for one of them in `route` (`:1164`), and answers rmcp with the
  final response under the original id. It keeps a `Driven` record per call, retries under
  synthetic ids (`retries`), swallows late responses of cancelled retries (`dropped_retries`), and
  turns a `notifications/cancelled` for the call into one for the retry in flight (`outgoing`).
  Dialogs for input requests run through `start` / `start_url` under synthetic ids
  (`mrtr_inputs`), gated by `CallTracker` (a subagent's call in flight cancels the dialog).
- `route` passes a `ServerResult::CreateTaskResult` through unchanged. rig-rmcp fails the call.
- `client_config()` (`rust/fontaine-tools/src/mcp/servers.rs:549`) declares only elicitation; no
  `extensions` entry, so a server has no reason to return a task.
- rmcp 3.9 has the types: `CreateTaskResult` (`task.task_id`, `poll_interval_ms`, `ttl_ms`),
  `GetTaskRequest` / `UpdateTaskRequest` / `CancelTaskRequest` as `ClientRequest` variants,
  `GetTaskResult` whose `DetailedTask.payload` is `Working | InputRequired{input_requests} |
  Completed{result} | Failed{error} | Cancelled`, `TASKS_EXTENSION_ID`, and
  `ClientCapabilities.extensions` (`ExtensionCapabilities`).
- The call timeout is rig-rmcp's: on expiry it sends `notifications/cancelled` for the call id,
  which the tap already sees in `outgoing`. The tap needs no timeout of its own.

## Design

A task is a second kind of driven call. Only `tools/call` can yield one (prompts and resources
cannot per the extension); a task result for any other eligible request fails that call with a clear
error instead of reaching rmcp.

### State

`Driven` gains a phase: `Retrying` (today's MRTR behaviour) or `Task(TaskState)`, where `TaskState`
holds `task_id`, the `RequestId` of the `tasks/get` / `tasks/update` in flight (if any), the
server's last `poll_interval_ms`, and the set of `input_requests` keys already surfaced. The
`task_poll_ids` map (poll id to call id, like `retries`) routes responses; `dropped_retries`
generalises to cover swallowed poll responses.

### Flow

1. `route` sees `CreateTaskResult` for an eligible `CallToolRequest`: it builds a `Driven` in the
   `Task` phase and schedules the first poll after `poll_interval_ms` (a new `Event::PollDue(call)`,
   same back-off task style as `RetryDue`; the interval is clamped to 50 ms..5 s, default 1 s when
   absent, so a hostile or buggy value cannot spin or stall).
2. `PollDue` sends `tasks/get {taskId}` through `inner.send` under a fresh `fontaine-task/...` id.
3. The response is a `GetTaskResult`; by status:
   - `Working`: re-arm `PollDue` with the (possibly updated) interval.
   - `InputRequired`: for each not-yet-surfaced input key start the same dialogs as an MRTR round
     (reusing the `start` / `start_url` / `deliver` path, sampling and roots fail the call as
     today); when all are answered send one `tasks/update {taskId, inputResponses}` and resume polling. Each `tasks/update` increments
     `Driven.sent` (1 for the `tools/call`), checked `>= MAX_REQUESTS` (10) on each `input_required` as in the MRTR
     path, so the tenth `input_required` fails the call after nine updates. `sent` supplies the dialog `round` in the synthetic
     ids, and `answers` / `awaiting` are cleared per round. `working`
     poll loops need no cap: rig-rmcp's call timeout cancels the call (step 5).
   - `Completed`: deserialise `result` as `CallToolResult` and write it to rmcp as a response under the
     original call id (an `isError: true` result is a normal result). A result that does not parse
     fails the call without `tasks/cancel`.
   - `Failed`: write a JSON-RPC error under the call id. `error` is a `JsonObject`, not `ErrorData`:
     build the error leniently from its `code` / `message` / `data`, with fallbacks (internal-error
     code, generic message) when a field is missing or mistyped.
   - `Cancelled`: write a JSON-RPC error "the server cancelled the task".
   - `TaskStatus` / `TaskPayload` are `non_exhaustive`: a wildcard arm fails the call.
4. A JSON-RPC error answering a poll (unknown or expired task) fails the call with that error. An
   expired `ttl_ms` is the server's business: it surfaces as such an error. A response to a poll id
   that is not a `GetTaskResult` (`ServerResult` is untagged, so it may land as `CustomResult`) fails
   the call; an error answering `tasks/update` fails the call; an error answering `tasks/cancel` is
   swallowed.
5. Cancellation (rig-rmcp's timeout or the user's interrupt): `outgoing` sees
   `notifications/cancelled` for the call; the tap drops the `Driven`, sends `tasks/cancel {taskId}`
   (fire and forget; its ack and any in-flight poll response are swallowed) instead of a
   `cancelled` notification for a retry, and aborts pending dialogs through the existing
   `drop_call`. `outgoing` currently does `let retry = driven.retry?;`, which returns early for a task
   call: branch on the phase first. `end_calls` also sends `tasks/cancel` for task-phase calls. A
   stale `PollDue` for a dropped call is a no-op, and every terminal status removes the `Driven` and
   its poll mapping. `fail` sends a best-effort `tasks/cancel {taskId}` (ack swallowed) for a
   task-phase call, so a call the tap fails itself (sampling/roots input, `MAX_REQUESTS`, a poll error) does not leave the server task live until its TTL; not for
   `Completed` / `Failed` / `Cancelled` terminal statuses and not on transport close.

Every tap-sent `tasks/get`, `tasks/update` and `tasks/cancel` id is registered (in `task_poll_ids` or
`dropped_retries`) at send time so no response leaks to rmcp. The `tasks/update` and `tasks/cancel`
acks are always swallowed (the update ack keeps the call alive). On every drop path (cancel, fail,
`end_calls`, `finish`) the in-flight poll id moves to `dropped_retries`, so a late `tasks/get`
response is swallowed.

The three failure paths funnel through the existing `fail(call, text)` so the user-visible error text
is built in one place; its log text (currently hard-coded as "ending an MCP call that asked for
input") is made generic, or parameterised, since `fail` is reused for tasks; a new `finish(call, ServerResult)` writes a synthetic success the same way.

### Capability

`client_config()` inserts `TASKS_EXTENSION_ID` with an empty object into
`ClientCapabilities.extensions`. This is the only change visible to servers on connect; permission
and guardian paths are untouched because the call was already authorised before it was sent, and the
polls are protocol traffic, not tool calls.

### Why in the tap

The tap is below rig-rmcp and rmcp, so every connection kind (stdio, HTTP, pipe) gets the
behaviour from one implementation and rmcp's `UnexpectedResponse` is never hit. A rig-rmcp change
would be a fork of a pinned dependency.

## Implementation Steps

1. `elicitation.rs`: add the `Task` phase to `Driven`, `Event::PollDue`, `task_poll_ids`, and
   `finish` / `fail` helpers; extend `route` to take `CreateTaskResult` (tools/call only) and poll
   `GetTaskResult` responses; factor the dialog-starting loop out of `input_required` so the task's
   `InputRequired` status reuses it with a different completion action (`tasks/update` instead of a
   retry).
2. `elicitation.rs` `outgoing`: on cancellation of a task-phase call send `tasks/cancel`.
3. `servers.rs`: declare the tasks extension in `client_config()`; update its doc comment and the
   test at `servers.rs:1322` area, which only checks elicitation today: add an assertion that
   `extensions` holds `TASKS_EXTENSION_ID` (`ClientCapabilities::supports_tasks()`).
4. Docs: `docs/mcp.md` (tasks section: what Fontaine does, that it polls transparently and honours
   the call timeout and interrupt, and that the existing "given up after 10 requests" limit applies to
   a task's `tasks/update` rounds: the tenth `input_required` fails the call after nine updates); `CHANGELOG.md` entry; the master plan `## Decisions` MCP
   subsection gets a dated line recording that Fontaine declares and serves the tasks extension for
   `tools/call` only.

## Files to Modify

- `rust/fontaine-tools/src/mcp/elicitation.rs`
- `rust/fontaine-tools/src/mcp/servers.rs`
- `docs/mcp.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions)

## Decisions

- In-task `input_required` is served through the existing dialogs rather than failing, because a
  task that blocks on input would otherwise hang until its TTL; sampling and roots still fail the
  call, as in MRTR.
- Rejected alternatives: declining to support tasks (long-running tools are the extension's purpose);
  supporting without declaring the extension (servers would never send a task, dead code); exposing
  the task to the model as a handle with a poll tool (new model-facing semantics, at odds with the
  transparent MRTR approach and the permission model).

## Documentation

`docs/mcp.md` (new tasks section), `CHANGELOG.md`, master plan Decisions line (see Implementation Step 4).

## Testing Strategy

All unit tests in `elicitation.rs` using the existing `Script` transport double (no live servers):

- A `CreateTaskResult` for a `tools/call` is not forwarded to rmcp; a `tasks/get` goes out after the
  poll interval; `working` then `completed` yields one `CallToolResult` response under the original id.
- `isError: true` completed result passes as a normal result.
- `failed` yields a JSON-RPC error under the original id; `cancelled` likewise.
- An error answering `tasks/get` fails the call.
- Interval clamping (absent, 0, huge) via paused tokio time.
- `input_required` task: dialog answered, `tasks/update` sent with the answers under the server's keys,
  polling resumes; a subagent's call still gets the dialog
  cancelled.
- `notifications/cancelled` for a task call sends `tasks/cancel`, swallows the late poll response,
  and aborts pending dialogs; transport close drops the task.
- A task result for a `prompts/get` or `resources/read` fails that call.
- The tenth `input_required` (after nine `tasks/update` rounds) fails the call.
- Sampling/roots input in a task fails the call and sends `tasks/cancel` from `fail`.
- An error answering `tasks/update` fails the call.
- `servers.rs`: `client_config()` declares the tasks extension (`supports_tasks()`) and still the
  elicitation capability.
- Regression guard: existing MRTR tests stay green (the `Driven` refactor must not change them).

No task-returning server exists in `evals/` or the tests (`rust/fontaine-tools/tests/mcp_mrtr.rs` has
no task support), so the scripted unit tests are the only coverage; a live check is a follow-up issue.
