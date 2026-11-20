# Undefined Tool Call from the Claude CLI Plan

## Overview

On `claudemax/sonnet` a model turn ended with `the Claude CLI called a tool Fontaine did not define:
job_cancel_placeholder` (issue #717). The name is a variant of the defined `job_cancel`. The adapter
refused it, correctly. This plan keeps the trust boundary (an undefined name is never run) and
ships the issue's "clearer error" option: the message says plainly that the model named a tool that
is not in the request.

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/717

## Current State

- `Reader::stream_event` (`rust/fontaine-sdk/src/core/providers/claude_subscription/protocol.rs`,
  `content_block_start` / `tool_use` arm) calls `Aliases::restore_call`, which accepts the full
  `mcp__fontaine__<name>` or the bare `<name>`. Anything else except `TOLERATED_BUILTIN`
  (`EndConversation`) goes to `undefined_call`, which returns a `protocol(..)` failure
  (`ErrorKind::Protocol`), the only kind that gets the `(event: ...)` suffix and the `--debug-file`
  rejected-line trail.
- `docs/claude-subscription.md` ("Failures") says an undefined name fails the request.
- The raw capture has not been obtained. Nothing in the repo produces `_placeholder`; the alias
  scheme (`alias_part`) only appends `_<8 hex>` to normalized names. It is most probably model noise.

## Design

1. **No new alias.** `restore_call` stays unchanged.
2. **No new `ErrorKind`, no retry.** The failure stays `ErrorKind::Protocol`.
3. **Clearer message.** `undefined_call` produces exactly: ``the Claude CLI called a tool Fontaine did not define: `<name>`; the model named a tool that is not in this request, which defines <capped list>``
   (`<name>` is the called name, `<capped list>` the existing capped list of defined tools; with no
   tools defined it reads ``...the model named a tool that is not in this request, which defines no tools``). The
   prefix up to the name is unchanged, so substring assertions on ``did not define: `X`;`` still
   hold. No "did you mean" hint. The init-time message in `validate_init` keeps its old wording.
4. **Trust boundary unchanged.** The refused call is never forwarded, never reaches the permission
   pipeline, and is recorded as `request_failed`. `validate_init` is untouched.

## Implementation Steps

1. `protocol.rs`: reword the message in `undefined_call`; keep the `Protocol` kind.
2. Tests in `protocol/tests.rs` (see Testing Strategy): update the two existing tests asserting the
   old `undefined_call` wording (`a_tool_the_request_did_not_define_is_refused`,
   `an_undefined_call_lists_a_capped_set_of_the_defined_tools`) to the new wording (the latter
   asserts ``Task`; this request defines no tools``, the empty-tools case); the
   `validate_init` message and its tests stay unchanged.
3. `CHANGELOG.md`: a Fixed entry for #717.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/claude_subscription/protocol.rs`
- `rust/fontaine-sdk/src/core/providers/claude_subscription/protocol/tests.rs`
- `CHANGELOG.md`

## Decisions

- AFK: no live capture is available, so the fix is a clearer message, not an alias.
- AFK: no new `ErrorKind`; a new pub enum variant is semver-breaking.
- AFK: no retry; it needs an `Http` wrapper that loses `claude_error` typing. A separate follow-up issue.
- AFK: the new message keeps the existing prefix up to the called name and appends the explanation,
  the most conservative wording, so existing substring assertions partly survive. Docs never quote
  the message text, so no docs change.

## Testing Strategy

All unit tests, no live CLI:

- The #717 shape (`content_block_start`, `tool_use`, name `job_cancel_placeholder`, with
  `job_cancel` defined) is refused, is never forwarded as a frame, the message says the model named
  a tool not in the request and names the call, and the error is still `ErrorKind::Protocol` with
  the `(event: ...)` suffix.
- The two existing tests (`a_tool_the_request_did_not_define_is_refused`,
  `an_undefined_call_lists_a_capped_set_of_the_defined_tools`) are updated to the new wording; they
  pin the arbitrary unknown name and capped-list cases, so no separate test for those.
