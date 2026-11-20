# claude-subscription: accept `system/informational` Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/714

## Overview
CLI 2.1.329 emits a `system` event with subtype `informational` in some turns (seen on a Max
account at 7d 98% usage). `Reader::system` rejects every subtype it does not list, so the turn
fails with `RequestError`. The event is a human-facing notice, not a tool call, history frame or
model output; the adapter treats it as ignorable, like `status`.

## Current State
`Reader::system` (`rust/fontaine-sdk/src/core/providers/claude_subscription/protocol.rs`, ~line 950)
accepts `init`, `status`, `thinking_tokens`, `permission_denied`; `api_retry` and everything else
are protocol errors. The payload of `informational` has not been captured.

## Design
Add `Some("informational") => Ok(Event::Nothing)` next to `status`. The payload is never read,
so it cannot steer anything: the adapter's trust boundary is unchanged (no tool, text or usage
flows from it). Unlike `thinking_tokens` it is not tied to `Phase`/`init_seen`: a notice such as a
usage warning may precede or follow init, and ignoring it is harmless either way.

## Implementation Steps
1. `protocol.rs`: add the `informational` arm with a one-line why-comment (payload never read).
2. `protocol/tests.rs`: add a test that `system/informational` (with a free-form `message`/`level`
   payload) yields `Event::Nothing` both before and after init, and keep
   `other_system_events_are_refused_by_name` on `model_fallback`/`notification`.
3. `docs/claude-subscription.md`: record the qualification decision where the accepted system
   events are described (CLI 2.1.329).
4. `CHANGELOG.md`: entry under Unreleased.

## Files to Modify
`protocol.rs`, `protocol/tests.rs`, `docs/claude-subscription.md`, `CHANGELOG.md`.

## Trade-offs
Failing closed on `informational` was rejected: it carries no capability (the unaccepted-event
rule guards events that could change what runs), and failing makes the account unusable whenever
the CLI shows a notice.

## Decisions
- 2026-11-19: ignore the event without reading the payload, since it was not captured; if a later
  capture shows it carries a tool or permission signal, revisit.

## Documentation
`docs/claude-subscription.md` as above.

## Testing Strategy
Unit test in `protocol/tests.rs` (constructed JSON through `feed`); pins the reported #714 failure.
