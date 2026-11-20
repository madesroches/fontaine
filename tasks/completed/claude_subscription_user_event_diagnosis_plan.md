# claude-subscription: diagnose the rejected `user` event Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/714

## Overview
With `system/informational` ignored (PR #715), the #107 item 2 request on `claudemax/sonnet` fails
with `the Claude CLI executed a native tool or permission request (event: user)`. `Reader::denial_echo`
refuses a `user` event for any of several reasons and reports all of them with one message, so the
failure cannot say which shape the CLI sent. The payload has not been captured, and the check is
the adapter's guard against the CLI running tools itself, so this plan does not accept a new shape
blind. It makes the failure self-describing, so one more run of the request yields the capture
the issue needs, and leaves acceptance to a follow-up built from that capture.

## Current State
`denial_echo` (`rust/fontaine-sdk/src/core/providers/claude_subscription/protocol.rs`, ~line 1253)
returns the same `protocol("...executed a native tool or permission request")` when: `message.content`
is missing or an empty array; it is not an array (a string, the usual shape of a CLI-synthesized
user turn); a block is not a `tool_result`; a block is not
`is_error: true`; a block has no `tool_use_id`; the id is not a call announced by this response, or
was already echoed; the same id repeats within the event. A rejected line is written to the debug
file (4 KiB, #448) only when `--debug-file` is given, and the user's TUI run had none.

## Design
1. Give each refusal its own message. The shared prefix stays (so existing assertions and the
   "(event: user)" suffix hold) and a reason follows: `no content`, `content that is not a list of
   blocks`, `a block that is not a tool result`, `a tool result that is not an error`, `a tool result
   without an id`, `an id this response did not announce`, `a call already answered`, `a tool result
   repeated`. Reasons describe the shape only: no payload text, ids or
   tool output enter the message. Acceptance is unchanged: every shape refused today stays refused.
2. Document the capture in `docs/claude-subscription.md` only (the error lives in the SDK, which
   embedders and the claude-compat surface use without `--debug-file`): the runbook (how to rerun the request with
   `--debug-file`, where the file lands, that the line is cut to 4 KiB and may hold tool output).
3. Follow-up, not this PR: after a capture, decide per shape (narrow accept vs keep refusing)
   and add a golden test from the captured line. The issue stays open for it.

## Implementation Steps
1. `protocol.rs`: replace the single `refused` closure in `denial_echo` with a helper taking the reason.
2. `protocol/tests.rs`: extend `an_echo_must_answer_an_unanswered_call_with_an_error` and its
   `started`/`echo`/`failure` helpers to assert each reason's text (including string content, an
   announced-but-echoed id, and an unannounced id) and that the payload marker is absent from the
   message; keep the accepted echo tests as is.
3. `docs/claude-subscription.md`: the capture runbook paragraph next to the debug-file sentence.
4. `CHANGELOG.md`: `### Changed` entry citing #714.

## Files to Modify
`protocol.rs`, `protocol/tests.rs`, `docs/claude-subscription.md`, `CHANGELOG.md`.

## Trade-offs
Accepting "any `user` event" or any non-error tool result was rejected: those shapes are exactly
what a CLI executing a native tool produces, and nothing captured says which one the CLI sent.
Ignoring the event outright was rejected for the same reason. Auto-running the dangerous request
here to capture it was rejected: it spends the subscription's quota and is the user's guardian test.

## Decisions
- 2026-11-19: the PR does not close #714 (`Refs`, not `Fixes`): the issue's capture and acceptance
  items remain open.
- 2026-11-19 AFK: no acceptance of any new `user` shape without a captured payload; the most conservative option
  that still moves the issue.

## Documentation
`docs/claude-subscription.md` as above.

## Testing Strategy
Unit tests over constructed `user` events, one per refusal reason, pinning the #714 failure's
reachable shapes and that no payload leaks into the message.
