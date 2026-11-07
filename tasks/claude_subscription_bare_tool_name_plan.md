# Claude Subscription Bare Tool Name Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/471

## Overview

With the `claude-subscription` provider the CLI occasionally names a tool without the
`mcp__fontaine__` prefix (`shell` for `mcp__fontaine__shell`). `Aliases::restore` maps prefixed names
only, so the call is rejected as "a tool Fontaine did not define", the request fails, and the agent
dies with its context. Recover such a call: a bare name that resolves to a tool in this request's set
is restored and normalized exactly like the prefixed one. Anything else still fails, and the error
text for a name that is not defined stays accurate.

## Current State

- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`:
  - `Aliases` (l.140-181) holds `short` (original → part after the prefix) and `original`
    (prefixed CLI name → original). `restore(full)` looks up only prefixed names.
  - `stream_event`, `content_block_start` (l.1001-1037): `restore(&cli_name)`; on `None` and not
    `TOLERATED_BUILTIN` it returns `undefined_call` (l.930), which says the tool was not defined even
    when the bare name is a defined tool's name.
  - `Call.cli_name` keeps the name as emitted; `permission_denied` (l.861) compares the CLI's
    reported `tool_name` against it, so a bare emission stays consistent.
  - `validate_init` (l.888) checks the CLI's offered tool list with `restore`; init lists prefixed
    names, so it is unaffected.
- Tests in `protocol/tests.rs` (l.412-462) pin the undefined-call text.
- `docs/claude-subscription.md` "Failures" describes protocol failures.

## Design

Option 1 from the issue: restore a bare name. The call still carries the Fontaine name, so it passes
the normal permission gate and guardian; no gate is relaxed, and the set of acceptable names is
unchanged (the request's own tools).

- Add `Aliases::restore_call(&self, cli_name) -> Option<&str>`: the prefixed lookup, else the bare
  lookup `original.get(format!("{TOOL_PREFIX}{cli_name}"))`. Bare matching uses the alias part
  (the name the inventory lists), so an aliased long/odd name is matched by the same string the CLI
  saw. `restore` stays prefixed-only for `validate_init`, where a bare name in the init tool list is
  still an error.
- `content_block_start` calls `restore_call`; the existing code already rewrites the block name to
  the original, so the emitted block is normalized.
- A case-sensitive exact match only: `Bash`, `Task` and other built-ins are not in the set and still
  fail. `TOLERATED_BUILTIN` keeps its existing handling and is checked after the restore, so a
  defined tool never shadows nothing new.
- Error text (option 3, for the remaining failures): `undefined_call` is unchanged for names not in
  the set. No new text is needed because a bare name of a defined tool no longer fails.
- No retry (option 2): recovery makes it unnecessary, and retrying a nondeterministic model would not
  address the cause.

## Implementation Steps

1. `protocol.rs`: add `Aliases::restore_call` with a one-line why-comment (the CLI sometimes drops
   the prefix).
2. `protocol.rs`: use it in `content_block_start` in place of `restore`.
3. `protocol/tests.rs`: add tests (below).
4. `docs/claude-subscription.md`: note the recovery under "Failures".
5. `CHANGELOG.md`: entry under Unreleased.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`
- `rust/fontaine-core/src/providers/claude_subscription/protocol/tests.rs`
- `docs/claude-subscription.md`
- `CHANGELOG.md`

## Trade-offs

- Retry once (option 2) was rejected: it costs a request, still fails on a repeat, and leaves the
  gate-equivalent path unchanged.
- Text-only fix (option 3) was rejected as sole fix: the agent would still die.
- Restoring in `Aliases::restore` itself was rejected: `validate_init` must keep refusing an init
  tool list that offers an unprefixed name.

## Decisions

## Documentation

`docs/claude-subscription.md` (Failures): one sentence that an unprefixed name of a tool the request
defines is accepted as that tool, and any other undefined name still fails the request.

## Testing Strategy

Unit tests in `protocol/tests.rs`, using the existing `reader`, `feed`, `stream` helpers:

- A bare name of a defined tool (`shell`) in `content_block_start` is accepted and the forwarded
  block's name is the original; this pins the bug in #471.
- A bare name of an aliased tool (long or non-plain name) is restored the same way.
- A bare name not in the set (`Bash`, a Fontaine tool outside this request's set) still fails with
  the "did not define" text (existing tests stay).
- `validate_init` still refuses a bare name in the init tool list.
- A bare-named call followed by the CLI's permission-denied report with the same bare `tool_name` is
  accepted (denial path stays consistent).

## Open Questions

None.
