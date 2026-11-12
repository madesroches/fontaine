# Claude Subscription Guardian Test Flake Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/595

## Overview

Remove the redundant full-headless `a_guardian_that_cannot_be_read_fails_closed` test that flaked
on macOS. The fail-closed algorithm, Claude-specific protocol-failure handling, and headless denial
event/tool-blocking behavior are already covered independently at their owning layers, so retaining
this cross-product test is not worth another speculative timing change. Production code and behavior
stay unchanged.

## Current State

- `rust/fontaine/tests/claude_subscription.rs:297-320` drives one shared CLI-double scenario through
  a main-model tool call, three non-JSON guardian replies, and a final main response. It waits for the
  entire headless process, then indexes `of_type(&lines, "gate_decision")[0]` without checking the
  exit status or including the captured output in the failure.
- The failing macOS attempt returned in 2.927 seconds with no `gate_decision`; the panic discarded
  the child's exit code, stdout, and stderr. The same commit passed on rerun. There is no explicit
  test deadline in `Claude::auto` or `Sandbox::run`: the latter awaits `wait_with_output()` at
  `rust/fontaine/tests/common/mod.rs:778-810`. The observed failure therefore does not establish that
  a test watchdog fired or identify which stage omitted the event.
- The non-JSON lines are Claude protocol failures, not malformed verdict JSON. The protocol reader
  rejects them in `rust/fontaine-core/src/providers/claude_subscription/protocol.rs:861-869`, and
  the guardian maps provider failures to `Decision::Deny` with `FailClosed::Error` in
  `rust/fontaine-guardian/src/guardian.rs:345-363`.
- The required behavior already has the following offline coverage:
  - `rust/fontaine-guardian/tests/guardian.rs:53-66` proves three malformed answers are retried
    twice and then denied as `Unparseable`.
  - `rust/fontaine-guardian/tests/guardian.rs:198-214` proves repeated provider errors become a
    `GuardianUnavailable` fail-closed denial; `:402-410` separately covers timeout fail-closed.
  - `rust/fontaine-core/tests/claude_subscription.rs:1153-1165` drives the actual Claude CLI double
    with a non-JSON protocol reply through `Guardian::adjudicate` and asserts a non-allow,
    fail-closed result.
  - `rust/fontaine/tests/auto_headless.rs:259-276` proves a guardian endpoint error produces one
    headless `gate_decision` with `GuardianUnavailable/error`, blocks the shell command, and returns
    the denial to the agent. `:280-294` does the same for an unparseable verdict.
  - `rust/fontaine/tests/claude_subscription.rs:274-294` retains the complementary Claude-specific
    full-headless happy path, proving main → guardian → main sequencing and execution after a valid
    verdict.

## Design

Take the issue's removal option. Delete only
`a_guardian_that_cannot_be_read_fails_closed` from `rust/fontaine/tests/claude_subscription.rs`.
Keep its imports and helpers because the adjacent guardian happy-path test still uses them.

Do not add a sleep, retry, longer production timeout, new test-only production seam, or speculative
process/watchdog change. Do not tighten the lower-level Claude test to require exactly three calls or
an exact failure reason: its current assertions still accept a legitimate fail-closed timeout under
an overloaded runner, while the deterministic guardian tests already pin exact retry and reason
behavior.

The remaining tests divide responsibility without a timing-sensitive third full-stack repetition:

```text
Claude protocol failure → fontaine-core Claude guardian test
Provider/malformed/timeout fail-closed policy → fontaine-guardian tests
Decision event + denial feedback + no tool execution → fontaine auto_headless tests
Claude guardian happy-path composition → remaining claude_subscription headless test
```

## Implementation Steps

1. Remove `a_guardian_that_cannot_be_read_fails_closed` from
   `rust/fontaine/tests/claude_subscription.rs`; leave every neighboring test unchanged.
2. Run the focused coverage and existing gates in Testing Strategy.
3. Let PR finalization add a reliability entry to `CHANGELOG.md` recording the removal of the
   redundant flaky integration test.

## Files to Modify

- `rust/fontaine/tests/claude_subscription.rs`
- `CHANGELOG.md` (PR finalization)

## Trade-offs

- **Remove the redundant test vs. add diagnostics and a larger deadline.** The failing run completed
  too quickly to support the hypothesis that a short test deadline fired. A deadline change would be
  speculative, and diagnostics alone would not make the test reliable. The issue explicitly permits
  removal when fail-closed behavior is covered; the coverage map above includes the Claude-specific
  protocol path and the headless security outcome.
- **Remove it vs. retain the unique full-stack composition.** The deleted test is the only test that
  combines every layer in one scenario, but each required behavior remains pinned at the layer that
  owns it and the Claude happy path still checks their composition. Retaining a known-flaky duplicate
  risks hiding future regressions behind reruns.
- **Strengthen the remaining lower-level assertions vs. leave them tolerant.** Exact retry counts and
  failure classes belong in the deterministic guardian tests. Requiring them again in the
  subprocess-backed Claude test could recreate the same load sensitivity.

## Decisions

None yet.

## Documentation

No product documentation changes: this removes test code only and changes no behavior or interface.
The changelog entry is sufficient.

## Testing Strategy

The change removes a flaky integration test rather than adding behavior, so verification proves the
coverage on which the removal relies and then runs the ordinary gates. All tests are offline.

From `rust/`:

```sh
cargo test -p fontaine-guardian --test guardian malformed_answers_are_retried_and_then_denied_as_unparseable -- --exact
cargo test -p fontaine-guardian --test guardian an_endpoint_error_is_denied_as_guardian_unavailable -- --exact
cargo test -p fontaine-guardian --test guardian a_guardian_that_does_not_answer_in_time_is_denied_as_a_timeout -- --exact
cargo test -p fontaine-core --test claude_subscription a_guardian_that_gets_a_provider_failure_fails_closed -- --exact
cargo test -p fontaine --test auto_headless a_guardian_endpoint_error_denies_the_call_as_guardian_unavailable -- --exact
cargo test -p fontaine --test auto_headless an_unparseable_verdict_denies_the_call -- --exact
cargo test -p fontaine --test claude_subscription
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The focused guardian tests pin exact retry, denial, and timeout behavior. The core Claude test pins
the non-JSON protocol path. The two headless tests pin `gate_decision`, denial feedback, and physical
non-execution. The remaining Claude subscription suite must retain its valid-verdict guardian test.
The macOS CI run of the ordinary workspace suite is the final cross-platform check that the removed
panic site is gone; no live API or real Claude CLI is involved.

## Open Questions

None.
