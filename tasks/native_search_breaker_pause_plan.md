# Withhold Provider Search While Auto Is Paused Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/676

## Overview
After repeated guardian denials the loop breaker pauses `auto`, and every call that would reach the
guardian then needs a human (or is denied in AFK). The provider's own server-side search runs inside
the model's response, so nothing judges it, yet `native_search::offered` ignores the pause and keeps
offering it. The breaker trips exactly when the agent may be misbehaving, and search queries are an
outbound channel neither the guardian nor the scanner sees. Withhold the entry in `auto` and `afk`
while `auto_paused()` is true.

## Current State
- `native_search::permitted(mode, rules)` (`rust/fontaine-sdk/src/core/native_search.rs`) checks only the
  mode and the `deny`/`ask`/`allow` rules; `Auto | Afk` always permit.
- `native_search::offered(config, models, state)` has the `SessionState` and calls `permitted`.
- `AgentRuntime::refresh_before_turn` (`rust/fontaine-sdk/src/core/agent.rs`) compares
  `offered().is_some()` with the built agent's `native_search` before each turn and rebuilds on a change,
  so a pause toggling `offered` is picked up on the next turn with no new plumbing. A failed rebuild that
  would keep a now-withheld search is already an error.
- `SessionState::auto_paused()` (`gate.rs`) reads the breaker; `resume_auto()` clears it when the human
  approves a call.
- Mid-turn changes already take effect only on the next turn (documented in `docs/permissions.md`).

## Design
Add a `paused: bool` input to `permitted` and have `offered` pass `state.auto_paused()`. In `Auto | Afk`
the arm returns `!paused`. The other modes are unaffected: `auto_paused` only has meaning for the
guardian-judged modes, and they are already withheld without an `allow` rule.

Keeping `permitted` a pure function of its inputs preserves its unit-test shape. No change to the
breaker, `refresh_before_turn` or provider code.

Resume behaviour falls out for free: when the human approves a call, `resume_auto` clears the pause and
the next `refresh_before_turn` re-offers the search (one prompt-cache miss per toggle, as for any gate
change).

## Implementation Steps
1. `native_search.rs`: add the `paused` parameter to `permitted`, update its doc comment, and pass
   `state.auto_paused()` from `offered`. Update the other callers of `permitted` (grep first).
2. Unit tests in `native_search.rs`: `auto_and_afk_withhold_it_while_paused` and the existing
   mode tests updated for the new signature; a paused `manual` with an `allow` rule still offers it.
3. Integration test in `rust/fontaine-sdk/tests/native_search.rs`: build in `Auto` and in `Afk`, trip the
   breaker through `state.guardian_denied()` until `auto_paused()`, call `refresh_before_turn`, assert
   `native_search_active()` is false and the next request carries no search tool; then
   `state.resume_auto()`, refresh, and assert it is offered again.
4. Docs (below).

## Files to Modify
- `rust/fontaine-sdk/src/core/native_search.rs`
- `rust/fontaine-sdk/tests/native_search.rs`
- `docs/permissions.md`, `docs/tools.md`, `docs/guardian.md`
- `CHANGELOG.md` (at PR time)

## Trade-offs
- Alternative: have the breaker itself notify the agent to rebuild. Rejected: `refresh_before_turn`
  already polls `offered` every turn, so a state read is enough.
- Alternative: withhold on any guardian denial rather than the pause. Rejected: the issue targets the
  point at which Fontaine has already handed the session back to a human.

## Documentation
- `docs/permissions.md` (the `websearch` bullet): in `auto` and `afk` it is offered unless auto is paused
  by the loop breaker; it returns on the next turn after the human approves a call.
- `docs/tools.md` (Web search, provider's own search): same condition added to the list of withholding
  conditions.
- `docs/guardian.md` (provider-native search limit): mention the pause as a withholding condition.

## Testing Strategy
- Unit: `permitted` truth table including the paused flag for every mode.
- Integration (mock model, no live API): the breaker-pause regression pins the issue's bug in both `auto`
  and `afk`, and the resume path.

## Open Questions
None.
