# claude-subscription: report a classifier refusal as one Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/714

## Overview
Captured on CLI 2.1.329 (issue #714 comment): when Anthropic's safety classifier withholds a reply,
the CLI sends a synthetic text-only `user` event (`isSynthetic: true`, `refused_message_id`), after a
`system/informational` notice. `denial_echo` refuses it as a native tool request, which is wrong:
nothing ran. The adapter should end the turn with a clear refusal error instead.

## Design
In `Reader` (`rust/fontaine-sdk/src/core/providers/claude_subscription/protocol.rs`), before
`denial_echo` treats a `user` event as an echo, recognise exactly the refusal shape: `isSynthetic`
is true, `message.content` is one `text` block, `refused_message_id` is a string, and no block is a
`tool_result`. It fails the request at once with a distinct, non-retried error whose text says the
Claude safety classifier stopped the response (no payload text, no ids in the message). Use the
error kind the provider layer already has for a refused or filtered response if one exists;
otherwise a protocol-style failure with that text. Every other non-`tool_result` shape stays refused
as today. The `system/informational` arm stays as it is.

## Implementation Steps
1. `protocol.rs`: the recognition and the error.
2. `protocol/tests.rs`: a golden test from the captured line (payload text not echoed into the
   message), and tests that variants (not synthetic, extra block, `tool_result` mixed in, missing
   `refused_message_id`) stay refused with their existing reasons.
3. `docs/claude-subscription.md`: failure-table row/sentence for a classifier refusal, and note the
   guardian cannot be observed on a call the classifier withholds.
4. `CHANGELOG.md`: `### Changed` entry citing #714.

## Trade-offs
Ignoring the event and continuing was rejected: the reply is withheld, so the turn has no usable
response. Matching on the message text was rejected: the structured fields are the contract.

## Decisions
- 2026-11-19 AFK: refusal is matched on structured fields only and fails the turn; no retry, since
  the classifier would refuse the same request again.

## Documentation
`docs/claude-subscription.md`.

## Testing Strategy
Unit tests over constructed `user` events, pinning the #714 capture and the neighbouring refused shapes.
