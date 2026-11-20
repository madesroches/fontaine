# claude-subscription: Cache Verdict False Positives and Re-measurement Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/718

## Overview

A real session on Claude CLI 2.1.329 printed the "replayed context was not read back from the
prompt cache" warning, and the verdict was persisted (#503), turning the native replay layout off
for that CLI version until a Fontaine update changes the template. The warning carries no numbers,
so it cannot be told whether 2.1.329 really regressed or the self-check judged a cold, evicted or
otherwise incomparable pair of requests. This plan makes the verdict auditable (the usage it was
judged on is recorded and shown), makes it harder to reach on one observation, and makes it
re-measurable instead of permanent.

## Current State

- `context::finished` (`rust/fontaine-sdk/src/core/providers/claude_subscription/context.rs`)
  compares the previous and current `Record` of a conversation through `check`: broken when
  `now.usage.read + READ_SLACK(64) < prev.usage.read + prev.usage.write_1h`. One shortfall calls
  `Verdicts::mark`, which inserts the version and persists it keyed by CLI version and
  `TEMPLATE_REVISION`.
- `check` skips pairs with a merged or unreplayable tail, changed settings, a gap of 55 min or
  more, or more than 18 added blocks. It does not skip a pair whose previous request was itself a
  cache miss (a cold write), whose usage is the sum over several API calls inside one CLI run, or
  that straddles a model change.
- `CacheUsage::from_result` reads the result frame's `usage` (`cache_read_input_tokens`,
  `cache_creation.ephemeral_1h_input_tokens`). Only these two numbers survive; the warning in
  `transport.rs` (~`:485-510`) prints the version only.
- Verdicts never expire: a version stays broken until `TEMPLATE_REVISION` changes. `docs/claude-subscription.md`
  (~`:228`) documents this. `cache-bench` (`rust/fontaine-evals/src/cache_bench.rs`) measures the
  effect offline against a real CLI.

## Design

The root cause is not known from the code alone (CLI regression versus a false positive), and no
evidence of a template change exists, so the plan treats the verdict as a possible false positive:
the hardening below ships, and `TEMPLATE_REVISION` and the template stay unchanged. A live
`cache-bench` comparison of 2.1.329 against the last known good version is a manual follow-up, not
a gate.

1. **Record the evidence.** `Record` keeps its `CacheUsage`; `finished` returns the previous and
   current usage and the gap with the verdict, and the warning prints them
   (`read X of expected Y after Z s`). The persisted entry gains optional `read`, `expected`
   fields for audit; `parse` ignores them.
2. **Harden `check` against false positives**:
   - A previous request that itself read nothing (`prev.usage.read == 0`) is a cold start, so the
     pair gives no verdict: its write is not evidence of a replayable prefix.
   - A verdict needs the shortfall twice in a row for the same version: the first shortfall is held
     in memory as a strike (`Verdicts::strike(version) -> bool`, true on the second); a
     healthy pair clears the strike. Only the second strike calls `mark`. `check` returns a
     three-way outcome (broken / healthy / no verdict): only a healthy pair clears the strike; a
     no-verdict pair (skipped or cold) leaves it untouched. The strike is per version, across
     conversations.
3. **Expire the verdict.** Each persisted entry gets a `"at"` unix timestamp. `load` drops entries
   older than 7 days, and the next `mark` rewrites the file without them, so a version is
   re-measured after a week and a transient condition cannot disable the optimization for good.
   Entries without `"at"` (written before this change) count as expired. `parse` and the in-memory
   broken collection carry the `at` per version (a map, not a set); `persist` keeps existing
   entries' `at` and stamps only the newly marked entry; `mark` takes the evidence for the
   `read`/`expected` fields.

## Implementation Steps

1. `context.rs`: usage and gap carried in `Record`/the return of `finished`; the strike set; `check`
   returns broken/healthy/no verdict and skips cold previous pairs; `"at"` timestamp on write and expiry in `parse`/`load`. Do not bump
   `TEMPLATE_REVISION` or change the template.
2. `transport.rs`: warning text includes the numbers from `finished`.
3. `context/tests.rs`: the tests below.
4. `docs/claude-subscription.md`: replace the "kept until the template changes" sentence with the
   two-strike rule and the 7-day expiry. `CHANGELOG.md`: entry.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/claude_subscription/context.rs`
- `rust/fontaine-sdk/src/core/providers/claude_subscription/context/tests.rs`
- `rust/fontaine-sdk/src/core/providers/claude_subscription/transport.rs`
- `rust/fontaine-sdk/tests/provider_warn_claude_subscription.rs`
- `docs/claude-subscription.md`
- `docs/configuration.md` (the `claude-cache-verdicts.json` row: 7-day expiry)
- `CHANGELOG.md`

## Trade-offs

- Two strikes instead of one costs one more full-history rewrite on a genuinely broken version,
  against disabling the optimization for a week on a single cold or evicted observation.
- A time-based expiry instead of re-measuring on CLI update: a version string does not change when
  the API's cache behaviour does, and the CLI updates in place, so age is the only signal that is
  always available.

## Decisions

- AFK: 2026-11-20 — Whether CLI 2.1.329 is a genuine regression is unanswered (needs a live `cache-bench` run); take the most conservative option satisfying #718: ship the false-positive hardening (cold-previous skip, two strikes, 7-day expiry, numbers in the warning) and do NOT bump `TEMPLATE_REVISION` or change the template, since no evidence of a template change exists; the live `cache-bench` comparison is a manual follow-up, not a gate.

## Documentation

`docs/claude-subscription.md` verdict paragraph (two strikes, 7-day expiry, numbers in the warning);
`docs/configuration.md` `claude-cache-verdicts.json` row (entries expire after 7 days).

## Testing Strategy

Unit tests against a temp data dir and constructed `Record`s, no live CLI:

- A cold previous request (`read == 0`) gives no verdict (the suspected false positive).
- One shortfall strikes without marking or warning; a second consecutive one marks; a healthy pair
  in between clears the strike.
- The warning text carries read, expected and the gap.
- An entry older than 7 days, or without `"at"`, is ignored on load and absent after the next
  `mark`; a fresh entry still forces `Legacy`.
- Existing persistence, merge and template-revision tests, adjusted for the new field.
- `rust/fontaine-sdk/tests/provider_warn_claude_subscription.rs` and the unit test
  `a_broken_verdict_needs_a_previous_native_request_and_survives_a_passing_check` (context/tests.rs)
  are reworked: scripted usage with a warm previous request (`read > 0`), two consecutive
  shortfalls, and the new warning text.

## Manual Verification

1. `cargo run -p fontaine-evals -- cache-bench` against the installed CLI (2.1.329) and the previous
   version: native replay should read back on a healthy version. This needs the real CLI and the
   live API, which no unit test reaches. It is a manual follow-up, not a gate: if 2.1.329 genuinely
   misses, record the byte-level template difference and bump `TEMPLATE_REVISION` in a separate change.

## Open Questions

None.
