# job_output Blocking Wait Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/336

## Overview
A `job_output` wait on a shell job returns as soon as the job prints anything new, so a chatty job
(`gh pr checks --watch`) makes a `wait_secs: 600` call return after seconds. Make the job wait
block until the job ends or the wait expires (Claude Code's `TaskOutput` `block: true`), leaving
line-by-line delivery to `monitor`.

## Current State
- `Jobs::output` (`rust/fontaine-tools/src/jobs.rs`, ~L972) has a non-monitor branch that returns
  when `job.ended.is_some() || fresh || expired || wait.is_zero()`, where `fresh` is
  `job.buffer.next > job.cursor`. `fresh` is the wake being removed.
- The monitor branch (note posted / ended / expired) and the `agent-*` path
  (`wait_on_agent` in `jobs/tools.rs`) are separate and unchanged.
- The tool description and `wait_secs` schema text (`jobs/tools.rs` ~L271–289) and `docs/tools.md`
  (table row L21, `job_output` bullet ~L303–309) say "waits for new output or its end".
- `tests/jobs.rs` ~L141 reads a still-running job with `wait_secs: 10` and expects its output.

## Design
- In the non-monitor branch drop `fresh`: return on `job.ended.is_some() || expired ||
  wait.is_zero()`. The loop otherwise parks on `changes.changed()`; output flushes still notify
  the channel, so the loop wakes, finds nothing to return, and parks again.
- The returned `Read` still carries everything accumulated since the cursor, so a wait that ends
  by expiry or job end returns all output.
- `wait_secs` omitted/0 stays an immediate poll.
- Message and user-interjection early return (`call` in `jobs/tools.rs`) is untouched.
- Description/schema: "waits for the job to end or `wait_secs` to elapse (default 0), then returns
  what it printed". Monitor wording unchanged.

## Implementation Steps
1. `rust/fontaine-tools/src/jobs.rs`: remove `fresh` from the non-monitor condition in `output`;
   adjust any doc comment that names output as a wake. Touch only the local `fresh` in `output`
   (~L1013), not the `fresh` parameter of `read(job, fresh)`.
2. `rust/fontaine-tools/src/jobs/tools.rs`: update the `job_output` description and `wait_secs`
   schema text: the stale "waits for new output or its end" wording is at L274 (description) and
   L289 (schema; change only the job half of "A job or monitor: for new output or its end",
   monitor behavior unchanged).
3. `rust/fontaine-tools/tests/jobs.rs`: adapt the L141 test (a running job with output now holds a
   `wait_secs: 10` call until expiry) to use a short wait or `wait_secs: 0`; add new tests below.
4. `docs/tools.md`: update the table row and the `job_output` bullet (stale wording at L305).
5. Master plan `## Decisions`: add a dated line at the end of `### Agents`; add a `CHANGELOG.md` entry under
   `### Changed` → `#### Agents`, next to the existing `wait_secs` entry.

## Files to Modify
- `rust/fontaine-tools/src/jobs.rs`
- `rust/fontaine-tools/src/jobs/tools.rs`
- `rust/fontaine-tools/tests/jobs.rs`
- `docs/tools.md`
- `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

## Trade-offs
- Keeping a fresh-output wake behind an opt-in argument was rejected: `monitor` already owns
  streaming and a flag adds surface for no caller.

## Decisions
- The recorded `job_output` decision (#301) already lists only a message or user interjection as
  early-return causes; this change aligns the code with it, so no deviation line from Claude Code
  is needed — only a record of the behavior.

## Documentation
`docs/tools.md` (table row and `job_output` bullet); check `docs/agents.md` L36–42 needs no change
(subagent path unchanged).

## Testing Strategy
Unit/integration tests in `rust/fontaine-tools/tests/jobs.rs`, using real short-lived shell jobs
with small waits:
- A job printing repeatedly then ending: a long `wait_secs` returns only at end, with all output
  and the end state (pins the observed `gh pr checks --watch` early-return bug).
- A job that prints and keeps running: a short wait returns at expiry, not before, with the
  accumulated output.
- `wait_secs: 0` on a job with fresh output returns immediately.
- Monitor and `agent-*` wait behavior, and message early return
  (`a_message_ends_any_wait_early_with_the_zero_wait_answer`): existing tests stay green.

## Open Questions
None.
