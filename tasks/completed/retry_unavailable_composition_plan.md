# Retry From the Guardian-Unavailable Dialog Re-enters the Adjudication Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/674

## Overview
Picking **Retry** in the guardian-unavailable dialog calls the guardian directly, so the retried
call skips the usage gate, the guardian composition (screen stop/allow/deny routing), the
usage-limit pause loop, and the `screened` audit refresh. With a user-scope
`[guardian.composition]` the LLM guardian alone can then allow a call the screen would have
escalated. Make Retry run the same adjudication pass as the first attempt.

## Current State
`PermissionGate::guardian_branch` (`rust/fontaine-sdk/src/core/permissions.rs`, ~2090-2400) runs an
inline `loop` that: waits at `usage_gate_for`, runs either `guardian.adjudicate` or
`composition::adjudicate`, routes `Decided::{Escalate,Allow,Deny}` to `screen_stop` /
`screen_allow` / `screen_deny` (early returns), and pauses on a spent usage limit. It yields
`(adjudication, screened)`. If the adjudication is an unavailable fail-closed and a human is
present, the dialog (`approver.guardian_unavailable`) is shown; `UnavailableChoice::Retry` does
`adjudication = self.state.guardian().adjudicate(&request).await`, leaving `screened` from the
first pass (outcome `unavailable`) attached to the retried verdict. Then `settle`.

Existing tests `retrying_an_unavailable_guardian_asks_the_replacement` and
`a_retried_adjudication_counts_as_one_guardian_call` (`rust/fontaine-sdk/tests/permissions_auto.rs`)
cover Retry only without a composition, and rely on a Retry that finds the guardian still
unavailable ending in a denial without a second dialog.

## Design
- Extract the loop body into a private method, e.g.
  `async fn adjudication_pass(&self, cx, request: &Arc<Request>) -> Pass`, where
  `enum Pass { Settled(GateDecision), Adjudicated(Adjudication, Option<Screened>) }`. `Settled`
  carries the early returns from `screen_stop`/`screen_allow`/`screen_deny`; the rest is moved
  verbatim (gate, composition, usage accounting, pause loop).
- `guardian_branch` calls it once; on an unavailable result with a human present it shows the
  dialog. On `Retry` it calls the pass again, replacing both `adjudication` and `screened`, and a
  `Settled` result is returned directly. The dialog is shown at most once per call: a retry that is
  still unavailable falls through to `settle` as a denial (current behavior, which the existing
  tests pin and which avoids an unbounded dialog loop).
- Everything after the dialog (`add_guardian_adjudication`, `settle`) is unchanged.

## Implementation Steps
1. In `permissions.rs`, add `Pass` and `adjudication_pass`, moving the loop out of
   `guardian_branch`.
2. Replace the direct `adjudicate` in the `Retry` arm with a second `adjudication_pass` call.
3. Add tests (below).
4. Add a CHANGELOG entry per repo convention.

## Files to Modify
- `rust/fontaine-sdk/src/core/permissions.rs`
- `rust/fontaine-sdk/tests/permissions_auto.rs`
- `CHANGELOG.md`

## Trade-offs
Recursing into `guardian_branch` was rejected: it would re-run the retry-token, auto-paused check
and request build, and could re-show the dialog without bound. Extracting only the adjudication
pass is the smallest change that gives the gate, composition and pause loop again.

## Decisions
- 2026-11-17: the dialog is offered once per call; a still-unavailable retry denies, as today.

## Documentation
No docs page describes Retry's internals; check `docs/guardian.md` for a statement that Retry
bypasses the screen and fix it if present.

## Testing Strategy
All unit/integration, no live APIs, in `tests/permissions_auto.rs` using the existing rig helpers:
- Regression for this bug: composition configured whose screen stops (escalates), guardian
  unavailable first, then repaired via `set_guardian`, Retry chosen: the call must escalate to the
  human (`screen_stop`), not be allowed by the LLM guardian, and the audit record's `screened`
  must reflect the second pass, not `unavailable`.
- A usage gate/pause-on-retry check if the rig already supports a spent window; otherwise
  covered by the shared-pass construction.
- Existing Retry tests stay green.

## Open Questions
None.
