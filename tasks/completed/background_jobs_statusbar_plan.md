# Background Jobs in the Status Bar Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/680

## Overview

After a turn ends with a background shell or monitor still running, the TUI looks idle; the only
tell is the "Background jobs" section of `/agents`. Add an always-on status-bar clause counting
live background jobs (shells and monitors separately), shown while any exists, with the panel key
beside it so the kill path is one keystroke away.

## Current State

- `Jobs::running_background()` (`rust/fontaine-sdk/src/tools/jobs.rs`, `Job::counts`) returns
  running background shells (explicit or promoted) and monitors; `JobInfo.monitor` tells them apart.
- The status bar (`rust/fontaine-tui/src/statusbar.rs`, `render()`) builds `agents`, `scheduled`,
  `goal` clauses from `App` fields (`running_agents`, `scheduled`, …) that
  `ui/mod.rs` refreshes each tick (~line 178–212). No job count.
- The panel's job rows come from `runtime.jobs().snapshot()` (`ui/mod.rs` ~518).
- Subagents cannot own background jobs: a background shell from a non-root agent is refused with
  `JobError::NotRoot`, and its foreground commands run in a throwaway local `Jobs::new()`
  (`tools/builtins.rs` ~764, ~951). So the main registry is the only one that can hold live
  background work.
- The remote `Status` (`remote/event.rs`) mirrors the status-bar counts to attached devices.

## Design

- `App` gains `background_shells: usize` and `background_monitors: usize` (default 0, next to
  `running_agents`).
- `ui/mod.rs` refresh: `running_background()` partitioned by `monitor`; this is the same definition
  the goal wait uses, so no second definition is computed. (#579 will want the same; if it lands
  first, reuse its helper.) The partition is a small pure helper `fn split(&[JobInfo]) -> (usize, usize)`
  (shells, monitors).
- `statusbar.rs`: a pure `jobs_clause(shells, monitors, panel_keys: Option<&str>) -> String`:
  - none → empty
  - grammar: `· {n} shell(s)[, {m} monitor(s)] running`, singular/plural per count, e.g.
    `· 1 shell running`, `· 2 shells running`, `· 3 monitors running`,
    `· 1 shell, 2 monitors running`.
  - followed by ` (<keys> to manage)` using the `AgentsPanel` binding when it is bound; omitted
    when unbound, and always omitted when rendered from a remote `Status` (`panel_keys = None`).
  Placed right after the `agents` clause; included in both trailer branches (custom statusline and
  hint). It shows during a turn as well as idle; the spinner row is untouched.
- Scope: the main agent's registry only; see the Decisions entry. Pinned by the existing test
  `a_subagent_gets_no_jobs_and_keeps_kill_at_timeout` in `rust/fontaine-sdk/tests/jobs.rs`; documented
  in `docs/tui.md`.
- Remote: add `background_shells`/`background_monitors` to `Status` with `#[serde(default)]` so older
  peers still parse; `client_ui.rs` copies them into the device's `App`, so an attached device shows
  the same clause.
- No config.

## Implementation Steps

1. `app.rs`: add the two fields.
2. `ui/mod.rs`: fill them from `runtime.jobs().running_background()` each refresh.
3. `statusbar.rs`: `jobs_clause`, wire into both trailers.
4. `remote/event.rs`, `remote/client_ui.rs`: carry the counts (update the test literal at ~927).
5. Tests (below). Docs. Changelog entry is added by `pr`.

## Files to Modify

- `rust/fontaine-tui/src/app.rs`, `ui/mod.rs`, `statusbar.rs`, `remote/event.rs`,
  `remote/client_ui.rs`
- `docs/tui.md`
- `tasks/fontaine_harness_plan.md` (`### TUI` Decisions: scope line)

## Trade-offs

- Spinner-row line for jobs: rejected; the issue asks the spinner to stop with the turn and the
  status bar to carry the state.

## Decisions

- 2026-11-16: The count covers the main agent's registry only, because subagents cannot start
  background jobs (#680).
- 2026-11-16: AFK: the "(<keys> to manage)" hint is omitted on an attached device, because the
  panel/kill path is host-only.
- 2026-11-16: AFK: the clause keeps its own key hint even though the idle hint already ends with
  `<keys> agents`; the idle hint is unchanged.

## Documentation

`docs/tui.md` (status-bar section): describe the clause, the panel key hint, and scope. Check
`docs/tools.md#background-jobs` for a pointer.

## Testing Strategy

- Unit tests on `jobs_clause`: none, one shell, many shells, one monitor, mixed, key hint present
  and absent.
- Unit test on `split` with constructed `JobInfo` values (shells, monitors, mixed, empty).
- `TestBackend` render tests of `statusbar::render` (wiring only): idle with a job, during a turn,
  with a custom statusline, zero jobs → no clause.
- Remote: `Status` round-trips and parses without the new fields.
- A tmux capture is not needed: only the status bar row changes, not the live area.

## Open Questions

None.
