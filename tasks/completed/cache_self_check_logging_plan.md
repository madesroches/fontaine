# claude-subscription: Cache Self-Check Logging Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/735

## Overview

A session on Claude CLI 2.1.330 produced the "replayed context was not read back from the prompt
cache" verdict after one prompt, and nothing recorded which request pair fell short or why. This
plan makes the self-check observable: one debug event per finished request carrying the usage, the
outcome and, for a skipped pair, the reason (with the prefix divergence when the prefix did not extend); and extra
fields in the persisted verdict, so a verdict can be judged after the fact even when no debug file
was open.

## Current State

- `context::finished` (`rust/fontaine-sdk/src/core/providers/claude_subscription/context.rs`,
  ~`:738`) calls `check` and returns a `Finding` only for a second consecutive shortfall. A
  healthy, skipped or first-strike pair leaves no trace.
- `check` (~`:430`) returns `Outcome::{Broken(Evidence), Healthy, NoVerdict}`. `NoVerdict` covers
  six different exits: the prefix does not extend, a merged tail, an unreplayable tail, changed
  settings, a cold previous request (`prev.usage.read == 0`), a gap of 55 min or more, and too
  many added blocks. `finished` also gives no verdict for a first request (no previous record).
- `CacheUsage` (~`:304`) keeps only `cache_read_input_tokens` and the 1 h cache write. A write
  that landed in the 5-minute bucket looks like a missing read. `Record` keeps `prints` (one hash
  per frame), `blocks`, the tail flags, `settings` and the instant.
- The persisted entry (`Verdicts::persist`) stores `cli`, `wording`, `template`, `at`, and the
  optional `read` and `expected`.
- The only logging nearby is `transport::rejected` (`transport.rs`, ~`:446`), a module-target
  `tracing::debug!` that reaches only the owner-only file opened with `--debug-file`
  (`docs/configuration.md`); it is off unless the user passes that flag. The debug filter defaults
  to `debug`.

## Design

**Outcome with a reason.** Replace `Outcome::NoVerdict` by `Outcome::Skipped(Skip)` with
`enum Skip { FirstRequest, NotExtended { first_diff, prev_len, now_len }, MergedTail,
UnreplayableTail, SettingsChanged, ColdPrevious, GapTooLong, TooManyAdded }`, `Display`ed as a stable snake_case word (the `NotExtended` data is not part of the word; it is logged as separate fields). `check` returns the
first reason that applies, in the order it tests today. `finished`'s control flow does not
change: `Skipped` behaves exactly as `NoVerdict` did, including leaving a strike untouched.

**Richer usage.** `CacheUsage` gains `input`, `write_total` (`cache_creation_input_tokens`) and
`write_5m` (`cache_creation.ephemeral_5m_input_tokens`), all defaulting to 0 when absent. The
check keeps comparing only `read` and `write_1h`; the extra fields are for the log only, so the
decision logic and `READ_SLACK` are untouched.

**Per-request event.** `finished` computes its outcome once and funnels every exit (healthy after `clear_strike`, skipped, non-striking broken, `Mark::Known`, the strike and the verdict) through one emit point, so the strike/verdict state known only after `strike`/`mark` is available to it. That emit is one `tracing::debug!` (module target,
so no export backend sees it) with: session id, CLI version, the wording's first 8 hex digits,
layout, frame count, added blocks, the request's usage, the previous request's usage when there
was one, the gap in seconds, whether the conversation holds a thinking block, and the outcome
(`healthy`, `broken`, `skipped:<reason>`, plus `strike` or `verdict` for the strike state). Counts, hashes and
tokens only; no message content. A request with no native layout, version, wording or usage logs
one event with `outcome = no_record` and the missing piece instead of returning silently.

**Prefix divergence.** `Skip::NotExtended` carries `{ first_diff, prev_len, now_len }`, computed in `check`
where both `prints` vectors are in scope, and the event adds them: the first index where the two
vectors differ and both lengths. This shows how the CLI rewrote the prefix. `Broken`
needs no such field: it is returned only after `extends` is true, so the prefix always matched.
The data rides on the `Skip` variant because `check` returns only the `Outcome`.

**Record.** `Record` also carries `thinking`, copied from `Sent` in `finished`, because
`check(prev, now)` builds `Evidence` from `Record`s; `Evidence.thinking` is taken from `now`.

**Thinking blocks.** `Sent` gains a `thinking: bool`, set in `Sent::new` when any frame's content
holds a `thinking` block (the same frame walk that fills `blocks`). It is logged only; no
decision uses it. It records the one untested suspect from the issue without acting on it.

**Persisted verdict.** `Entry` (Copy) gains the three optional fields `prev_read`, `prev_write_1h` and
`thinking`, written by `mark` from the `Evidence`, which gains those fields. `parse` reads them as
optional and `persist` writes them when present (it re-parses and rewrites every entry from
`Entry`, so unread fields would be dropped on the next write). Older files load unchanged and
`TEMPLATE_REVISION` stays. This is what makes a verdict diagnosable without a debug file.

## Implementation Steps

1. `context.rs`: `Skip` and `Outcome::Skipped`; widen `CacheUsage` and `CacheUsage::from_result`;
   `Sent.thinking`; extend `Evidence` with the previous usage and the thinking flag (from `now`); add
   `Record.thinking`, copied from `Sent` in `finished`; log the first differing index and both
   lengths on `Skip::NotExtended`; emit the per-request event from `finished`; add the three
   optional fields to `Entry`, read them in `parse` and write them in `persist`.
2. `context/tests.rs`: the tests below; update the existing `record(...)` and `check` tests for
   `Outcome::Skipped`; update the `CacheUsage { read, write_1h }` literals (about 10 sites) and
   `Evidence { .. }` literals (about 3), e.g. by deriving `Default` on `CacheUsage` and using
   `..Default::default()`.
3. `docs/claude-subscription.md`: document the debug event fields next to the existing
   `--debug-file` paragraph (~`:197`) and the new fields of `claude-cache-verdicts.json`.
4. `CHANGELOG.md`: an `Added` entry under `[Unreleased]`.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/claude_subscription/context.rs`
- `rust/fontaine-sdk/src/core/providers/claude_subscription/context/tests.rs`
- `docs/claude-subscription.md`
- `CHANGELOG.md`

## Trade-offs

- Debug level, not info: the file is opt-in (`--debug-file`), and an `info` event would reach
  telemetry export backends. The persisted verdict fields are the always-on record, so a
  verdict is diagnosable even when no debug file was open, without widening what is exported.
- A `Skip` enum instead of a free-text reason keeps the log greppable and the tests exact.
- Logging every request, not only broken ones, is what the issue lacks: the pairs judged healthy
  or skipped are what show which pair changed.

## Decisions

- 2026-11-20: signed thinking blocks are logged, not acted on. The suspect is unconfirmed (it
  did not reproduce headless with `--effort high`), and the data from this change is what decides
  whether `check` should skip such pairs.
- AFK: prefix divergence data moved from `Broken` to `Skip::NotExtended` because `Broken` implies the prefix matched.

## Documentation

`docs/claude-subscription.md` (debug event, verdict fields).

## Testing Strategy

Unit tests in `context/tests.rs`, no live CLI:

- Each `check` exit returns its own `Skip` reason, and `Healthy` and `Broken` are unchanged
  (the existing tests, adapted).
- `finished` returns the same findings as before for the existing scenarios: a skip still leaves
  a strike untouched and a healthy pair still clears it.
- `CacheUsage::from_result` reads `input`, `write_total` and `write_5m`, and each defaults to 0.
- `Sent::new` sets `thinking` for a conversation holding a `thinking` block and not otherwise.
- The per-request event: capture `tracing` output with a test subscriber and assert the outcome
  word, the usage fields and, for `Skip::NotExtended`, the first differing index and both lengths;
  assert no message text appears in it.
- A persisted verdict round-trips `prev_read`, `prev_write_1h` and `thinking`, including a
  merge of two entries (persisting one entry must not drop the other's fields), and a file
  without them still loads.

## Open Questions

None.
