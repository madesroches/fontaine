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

1. **No new alias.** No deterministic mangling is evidenced, and fuzzy matching would run a call the
   model did not name exactly. `restore_call` stays unchanged.
2. **No new `ErrorKind`, no retry.** The failure stays `ErrorKind::Protocol`, so the `(event: ...)`
   suffix and debug trail stay (see Trade-offs).
3. **Clearer message.** `undefined_call` says the model named a tool that is not in the request,
   keeping the called name and the capped list of defined tools. No "did you mean" hint.
4. **Trust boundary unchanged.** The refused call is never forwarded, never reaches the permission
   pipeline, and is recorded as `request_failed`. `validate_init` is untouched.

## Implementation Steps

1. `protocol.rs`: reword the message in `undefined_call`; keep the `Protocol` kind.
2. Tests in `protocol/tests.rs` (see Testing Strategy).
3. `docs/claude-subscription.md`: update the wording.
4. `CHANGELOG.md`: a Fixed entry for #717.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/claude_subscription/protocol.rs`
- `rust/fontaine-sdk/src/core/providers/claude_subscription/protocol/tests.rs`
- `docs/claude-subscription.md`, `CHANGELOG.md`

## Trade-offs

- **Narrow alias for `job_cancel_placeholder`**: rejected; no evidence of deterministic mangling,
  and fuzzy name matching weakens the trust boundary.
- **Silently dropping the call (like `EndConversation`)**: rejected; it would end a turn with no
  tool result and no explanation.
- **New `ErrorKind`**: rejected; `ErrorKind` is a pub, non-`#[non_exhaustive]` enum in the
  semver-stable SDK, so a variant is breaking, and it would lose the `(event: ...)` suffix and
  debug trail that apply only to `Protocol`.
- **Retry**: rejected; retry lives in `drive()` (`core/turn.rs`), `provider()` yields a
  non-retryable `ProviderError::request`, and a retryable `Http` wrapper would lose the typed kind
  `claude_error()` recovers (it downcasts only `Request`). The "nothing forwarded" gate is also
  never true, since `message_start` is already forwarded.

## Decisions

- AFK: no live capture is available, so the fix is a clearer message, not an alias.
- AFK: no retry and no new `ErrorKind`; retry needs an `Http` wrapper that loses `claude_error`
  typing, and a new pub enum variant is semver-breaking. A retry can be a separate follow-up issue.

## Documentation

`docs/claude-subscription.md`: the "Failures" wording for an undefined tool name (the model named a
tool not in the request; the request fails).

## Testing Strategy

All unit tests, no live CLI:

- The #717 shape (`content_block_start`, `tool_use`, name `job_cancel_placeholder`, with
  `job_cancel` defined) is refused, is never forwarded as a frame, the message says the model named
  a tool not in the request and names the call, and the error is still `ErrorKind::Protocol` with
  the `(event: ...)` suffix.
- An arbitrary unknown name (`Bash`, a 500-char name) is still refused.
