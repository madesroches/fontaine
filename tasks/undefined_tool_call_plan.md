# Undefined Tool Call from the Claude CLI Plan

## Overview

On `claudemax/sonnet` a model turn ended with `the Claude CLI called a tool Fontaine did not define:
job_cancel_placeholder` (issue #717). The name is a variant of the defined `job_cancel`. The adapter
refused it, correctly, but the refusal is a non-retryable protocol failure, so an autonomous
session stopped on what looks like one-off model noise. This plan keeps the trust boundary
(an undefined name is never run) and makes the failure recoverable and clearer.

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/717

## Current State

- `Reader::stream_event` (`rust/fontaine-sdk/src/core/providers/claude_subscription/protocol.rs`,
  `content_block_start` / `tool_use` arm) calls `Aliases::restore_call`, which accepts the full
  `mcp__fontaine__<name>` or the bare `<name>`. Anything else except `TOLERATED_BUILTIN`
  (`EndConversation`) goes to `undefined_call`, which returns a `protocol(..)` failure.
- `ClaudeError` kinds (`claude_subscription/mod.rs`) are all documented as non-retryable;
  `ClaudeError::provider()` yields a non-retryable `ProviderError::request`. Only `ReadIdle` is
  retried (as an `Http` error).
- `docs/claude-subscription.md` ("Failures") says an undefined name fails the request.
- The raw capture has not been obtained: the issue has no `--debug-file` output. Nothing in the
  repo produces `_placeholder`; the alias scheme (`alias_part`) only appends `_<8 hex>` to
  normalized names, so the suffix is not a Fontaine alias. It is most probably model noise.

## Design

1. **No new alias.** Without a capture, no deterministic mangling is evidenced, and mapping a
   near-miss name (prefix/suffix matching) to a tool would run a call the model did not name
   exactly. `restore_call` stays unchanged.
2. **Distinguish a model-invented name from a protocol violation.** Add `ErrorKind::UndefinedTool`
   (a call to a tool name the request did not define). `undefined_call` returns it instead of
   `ErrorKind::Protocol`; the message keeps today's text, including the capped list of defined
   tools.
3. **Make that kind retryable.** The model's choice of name is nondeterministic, so a fresh attempt
   is likely to succeed. `ClaudeError::provider()` maps `UndefinedTool` to an error the existing
   `[retry]` loop retries, the same way `ReadIdle` is retried, with no new retry machinery. The
   retry only fires when no frame of the failed response has reached the caller; otherwise the
   failure stays terminal, so a partially streamed turn is never replayed on top of itself.
   (Which signal says "nothing forwarded" is for the implementer to find in `transport.rs`; if the
   transport cannot tell, the kind stays non-retryable and only the clearer error ships.)
4. **Clearer error.** The message additionally says the model named a tool that is not in the
   request, so the user need not diagnose it.
5. **Trust boundary unchanged.** The refused call is never forwarded, never reaches the permission
   pipeline, and is recorded as `request_failed`. The init-time check (`validate_init`) is
   untouched.

## Implementation Steps

1. `claude_subscription/mod.rs`: add `ErrorKind::UndefinedTool`; update the `ErrorKind` doc comment
   (not every kind is non-retryable) and make `provider()` retryable for it.
2. `protocol.rs`: `undefined_call` returns the new kind; adjust the `Failure` construction.
3. `transport.rs`: carry the "nothing forwarded yet" condition to the retry decision if it is not
   already available.
4. Tests in `protocol/tests.rs` (see Testing Strategy).
5. `docs/claude-subscription.md`: update the failure table and paragraph.
6. `CHANGELOG.md`: a Fixed entry for #717.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/claude_subscription/mod.rs`
- `rust/fontaine-sdk/src/core/providers/claude_subscription/protocol.rs`
- `rust/fontaine-sdk/src/core/providers/claude_subscription/transport.rs` (if needed)
- `rust/fontaine-sdk/src/core/providers/claude_subscription/protocol/tests.rs`
- `docs/claude-subscription.md`, `CHANGELOG.md`

## Trade-offs

- **Narrow alias for `job_cancel_placeholder`**: rejected; no evidence of deterministic mangling,
  and fuzzy name matching weakens the trust boundary.
- **Retry vs only a clearer error**: a clearer error alone still stops an AFK session; retry is the
  behavior the issue asks for.
- **Silently dropping the call (like `EndConversation`)**: rejected; it would end a turn with no
  tool result and no explanation.

## Decisions

- AFK: no live capture is available, so the fix is classification plus retry, not an alias.

## Documentation

`docs/claude-subscription.md`: the failure table row for protocol failures and the paragraph after
it (an undefined tool name is retried when nothing of the response was delivered).

## Testing Strategy

All unit tests, no live CLI:

- The captured shape (`content_block_start`, `tool_use`, name `job_cancel_placeholder`, with
  `job_cancel` defined) is refused, names the call and the defined tools, and has kind
  `UndefinedTool`. Pins #717.
- An arbitrary unknown name (`Bash`, a 500-char name) is still refused and never forwarded; the
  existing tests keep passing with the new kind.
- The refused call is not forwarded as a frame.
- Retry classification: `UndefinedTool` is retryable when nothing was forwarded and terminal
  otherwise; every other `ErrorKind` stays non-retryable.

## Open Questions

- Whether the transport can tell that nothing of the failed response was forwarded (decides step 3
  and whether retry ships).
