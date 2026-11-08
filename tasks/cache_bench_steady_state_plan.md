# Cache Bench Steady-State Mean Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/497

## Overview

`cache-bench` compares Fontaine's per-step cache write against `claude -p` using the mean over
steps 2..N-1. Step 2 is the one cheap step (the previous message is the short original prompt,
already in the base context), so it drags the mean down: the default 6-file run reports 1.80 while
the steady-state ratio is about 1.97. A short run can pass a threshold a longer run would fail.
Average steps 3..N-1 only, and reject runs with too few steps left to average.

## Current State

- `rust/fontaine-evals/src/cache_bench.rs`: `mean_middle_creation` averages `steps[1..len-1]`;
  `summarize` rejects fewer than 3 steps per side and a zero Claude mean ("middle steps" wording in
  both errors); doc comment on `summarize` and the `table()` label `mean write (middle steps)`.
- `rust/fontaine-evals/src/runner.rs` (`run_cache_bench`): usage error when `--files < 3`
  ("the ratio needs a middle step"). A run of F files makes F+1 model calls.
- `Summary` field names `fontaine_mean_creation` / `claude_mean_creation` are serialized into the
  scorecard.
- Docs: `evals/README.md` (Prompt-cache bench: flag table `--files` "at least 3", "middle steps"
  paragraph and exit-code sentence) and `docs/claude-subscription.md` ("Checking cache usage").
- `evals/scorecards/claude-subscription-cache-sonnet.json` named in the issue is not tracked in
  the repository on this branch. Open PR #499 (branch `cache-bench-first-scorecard`) commits a
  scorecard (ratio 1.797) measured with the old mean.

## Design

- Rename `mean_middle_creation` to `mean_steady_creation`, averaging `steps[2..len-1]`.
- A side needs at least 2 steady steps, i.e. at least 5 model calls (first, settling step, two
  steady, last). `summarize` returns an error naming the side, the count, and the 5 required.
  One steady step would make the "mean" a single noisy sample.
- `run_cache_bench` usage error becomes `--files must be at least 4` (F files = F+1 calls, so 4
  files gives 5 calls); update the message to say the ratio needs two steady-state steps. Default
  of 6 files is unchanged (4 steady steps' worth: steps 3..6).
- Scorecard schema: keep the field names to stay compatible with existing JSON files, since the
  stored values are means of "steady" steps either way; update doc comments only. Table label
  becomes `mean write (steady-state steps)`.
- Exit semantics are unchanged: a side with too few calls exits 1 via the existing error path.

## Implementation Steps

1. `cache_bench.rs`: replace `mean_middle_creation` with `mean_steady_creation` (steps 2..len-1);
   raise the minimum in `summarize` to 5 with the new message; reword the zero-mean error and the
   `summarize` doc comment (skips the first step for base context, the second because it writes
   only one message, and the last because it only answers); change the `table()` label.
2. `runner.rs`: `--files` minimum 4 and the new message; reword the `threshold` clap doc comment
   (line ~79, "mean middle-step write") to "mean steady-state write".
3. Update unit tests in `cache_bench.rs` (see Testing Strategy).
4. Update `tests/cache_bench.rs`: `bench()` default becomes `--files 4`; every creation array has at
   least 5 steps (the too-short side in `a_side_with_too_few_steps_fails_the_bench` stays short);
   expected ratios (e.g. 1.5) are recomputed over steps 3..N-1; `bad_flags_are_usage_errors` uses
   "3" as the too-small `--files` value and keeps a threshold-validation case reachable (a valid
   `--files` with `--threshold 0`).
5. Docs: `evals/README.md` (flag table "at least 4", ratio paragraph, exit-code sentence "fewer than
   five model calls") and `docs/claude-subscription.md` ("leaving out the first step, the second
   (it writes only one message) and the last"; "fewer than five model calls").
6. `CHANGELOG.md`: amend the existing unreleased #483 `cache-bench` entry under `## [Unreleased]` /
   `### Added` with a sentence about the steady-state mean (steps 3..N-1) and the minimum of 4
   files, ending with "(#497)"; no new entry.

## Files to Modify

- `rust/fontaine-evals/src/cache_bench.rs`
- `rust/fontaine-evals/src/runner.rs`
- `rust/fontaine-evals/tests/cache_bench.rs`
- `evals/README.md`
- `docs/claude-subscription.md`
- `CHANGELOG.md`

## Trade-offs

- Excluding step 2 vs. a "settled" detector (mean after the write stops changing): the detector
  adds heuristics and nondeterminism; the cheap step is structural (the prompt precedes any tool
  result), so a fixed skip is exact and simple.
- Warn vs. fail on too few steps: a warning still emits a ratio that can pass a threshold wrongly;
  failing matches the existing fewer-than-3 handling.

## Decisions

- AFK: re-running the bench needs a signed-in subscription and live CLI (manual-only per
  AGENTS.md), so open PR #499's scorecard (ratio 1.797, old mean) is left for the maintainer to
  re-record with the new method (its stored per-step `cache_creation` values allow recomputing
  offline); the PR notes it.

## Documentation

`evals/README.md` and `docs/claude-subscription.md` as listed in the steps.

## Testing Strategy

Unit tests in `cache_bench.rs`, no live calls:
- Rewrite `the_ratio_covers_the_middle_steps_only` (renamed for steady-state) to use five-plus steps
  and assert that the first, second and last steps do not move the mean: put huge values in the
  first and last steps and a cheap step 2, with steady steps at ratio 2.0 yielding exactly 2.0, so
  it pins the issue.
- Update `the_threshold_is_exclusive`, `base_context_is_the_first_steps_read_plus_creation`,
  `the_zero_native_write...` and `a_scorecard_round_trips` to at-least-5-step inputs.
- `fewer_than_five_steps_on_either_side_is_an_error` (replacing the 3-step test) checks both sides
  and 4 steps.
