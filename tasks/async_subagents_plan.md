# Async Subagents Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/326

## Overview

Every `agent` spawn becomes asynchronous: the call returns `{"agent_id": …}` at once, the child
runs alongside its parent and its siblings, and its report reaches the parent as a notification.
A parent that wants a result now waits for it explicitly with `job_output`. The only implicit
join happens when a parent is ending: a subagent that gives its final answer while its own
children still run keeps its run open until they report, takes a further turn on what they said,
and only then ends; the main conversation does the same in a headless run (it already does). This
removes the blocking spawn that serialized "spawn 2 agents to verify these" in #326.

The user's direction for this work: "agents need to be fully async and execute concurrently with
one another. There should be no implicit join unless the parent agent is terminating. There are of
course sync tools available to allow parents to wait on their children."

## Current State

- **Two spawn modes.** `AgentArgs { agent_type, prompt, model, background }`
  (`rust/fontaine-agents/src/spawn.rs:33-38`). With `background` unset, `AgentTool::call` awaits
  `handle.report()` behind a `KillOnDrop` guard (`spawn.rs:395-423`) and returns the report as the
  tool result; with `background: true` it calls `watch_report` and returns `{agent_id}` at once
  (`spawn.rs:383-393`). `watch_report` (`spawn.rs:128-155`) queues
  `"Background agent {id} ({type}) finished: …"` in the parent's inbox, or the root's if the parent
  ended, then `conclude`s and drains missed human messages.
- **Serial tool execution.** Every call of a turn runs with `.tool_concurrency(1)`
  (`rust/fontaine-core/src/turn.rs:286`) so the gate judges each call before the next starts. A
  blocking `agent` call therefore holds the whole turn until its child ends; two in one turn run one
  after the other. Background spawns already overlap because their call returns at once.
- **Waiting.** `job_output` with an `agent-*` id waits on a child the caller started
  (`rust/fontaine-tools/src/jobs/tools.rs:197-270`, `AgentWaits` in
  `rust/fontaine-tools/src/jobs.rs:150-159`, implemented by `AgentTree` at
  `rust/fontaine-agents/src/tree.rs:917-950`). The report rides the wait's result through
  `NotificationStage` (`rust/fontaine-core/src/child_runner.rs:46-70`). `AgentDelivery::
  AlreadyDelivered { foreground }` (`jobs.rs:118-125`) tells "your `agent` call returned it" from
  "it was a notification".
- **Ending a child.** `run_child` (`child_runner.rs:449-536`) drives one `drive` to its
  `TurnOutcome` and returns a `ChildReport`. `AgentTree::finish` (`tree.rs:488-512`) then kills
  every running descendant: "nobody is left to read their reports". So a nested parent must call
  `job_output` before answering, or its children die (`docs/agents.md:39-41`; Decision 2026-10-30,
  #301).
- **Root join.** A headless run already joins: `settle` / `wait_for_news`
  (`rust/fontaine/src/headless.rs:578-604, 670-696`) waits while `tree.background_running()` and
  gives the main conversation a turn per batch of news via `AgentRuntime::notification_prompt`
  (`rust/fontaine-core/src/agent.rs:510-574`), which screens the notes through the result pipeline.
  The TUI root never ends; it gets an idle turn for reports (`fontaine-tui/src/ui/mod.rs:207`) and
  kills all children on quit (`fontaine-tui/src/lib.rs:337`).
- **Tree bookkeeping.** `NodeInfo.background`, `reserve(.., background)`, `SpawnRequest.background`
  and `background_running()` exist only to tell the two modes apart (`tree.rs:111, 368, 858-864`,
  `runner.rs:61`).
- **Decision in force.** "2026-10-21 (user): The `agent` tool blocks until the child finishes
  (Claude Code parity); `background: true` returns `{agent_id}` …" (master plan, Agents).

## Design

### 1. One spawn mode: always async

- `AgentArgs` loses `background`; the schema in `AgentTool::parameters` loses the property. With
  `deny_unknown_fields` an old-style `background: true` is an `invalid_args` result naming the
  field, which tells the model to drop it.
- `AgentTool::call` always ends the way the background branch does today: `watch_report(..)` and
  `Ok(ToolOutput::json(json!({ "agent_id": id })))`. `KillOnDrop` and the blocking tail
  (`spawn.rs:47-76, 395-423`) are deleted; with them goes the `subagent {id} failed: {reason}`
  tool error, so `report_text` (`spawn.rs:220-229`) appends `report.reason` for
  `ChildStatus::Failed` as it does for `BudgetExhausted`. `ReleaseOnDrop` stays: `runner.spawn` still awaits.
- The tool description says: the call starts the subagent and returns its `agent_id` at once;
  subagents run concurrently with you and with each other; the report arrives as a notification
  with a later tool result or turn; call `job_output` with the id to wait for one; a subagent that
  answers while its own subagents run waits for them before it ends.
- `watch_report`'s note becomes `"Agent {id} ({type}) finished: …"`, and
  `Notification::render` for `Report` reads `[Report from agent {id}. …]`. `resume_child` keeps
  calling `watch_report` (its doc loses "as a background child does").
- `SpawnRequest.background`, `NodeInfo.background` and `reserve`'s `background` parameter go.
  `background_running()` is replaced by `AgentTree::running_below(agent) -> bool` (running
  descendants of `agent`, built on `Inner::running_descendants`); headless calls it with
  `ROOT_AGENT`, which is what `background_running` meant once every child is "background".
- `AgentDelivery::AlreadyDelivered { foreground }` becomes a unit variant `AlreadyDelivered`, and
  `render_agent_wait` keeps only the "already delivered to you" line.

No change to `turn.rs`; concurrency comes from the children, not from the tool loop.

### 2. Implicit join when a subagent ends

`run_child` becomes a loop over runs of the child's conversation:

```
outcome = drive(prompt)
loop {
    match outcome { Answer(_) => {}, other => return report(other) }   // kill/fail/budget: no join
    match join_wait(id).await {               // select! with the kill switch, as `drive` is
        Joined::Done      => return report(outcome),
        Joined::Prompt(p) => outcome = drive(p),   // same agent, same "main" conversation
    }
}
```

- `join_wait` loops, registering on `tree.changed()` before each check (as `AgentTree::wait` does),
  and each pass checks in the order headless `wait_for_news` does: the child's inbox (a report or a
  `send_message`) and `steering.has_deliverable_message_for(id)`; then `tree.running_below(id)`; and
  when nothing runs, the inbox once more before returning `Joined::Done`. A report is queued before
  its child is marked finished (`watch_report`), so the last check cannot miss one. The human's
  typing never notifies the tree (`Steering::queue_for`, `message_agent`), so the wait races
  `tree.changed()` against a short sleep (the same poll interval as `job_output`'s wake,
  `WAKE_POLL`) and re-checks `has_deliverable_message_for` on each wake.
- The prompt for an inbox wake is the same text the root builds today. `notification_prompt` and
  `screened_notes` move out of `AgentRuntime` into a free function
  `notification_prompt(state, results, agent) -> Option<String>` (in `child_runner.rs`, beside
  `NotificationStage`), with `CallView.agent` set to the waiting agent; `AgentRuntime::
  notification_prompt` calls it with `ROOT_AGENT`. `ChildContext` gains
  `results: Arc<ResultPipeline>` (set in `agent/setup.rs` where it is built). The notes are thus
  screened exactly as the root's are.
- A steering wake takes the human's pending text (`steering.take_pending_for(id)`) as the prompt
  and records it the way `ChildRun::resume` does (`Record::UserMessage` with the agent, an
  `AgentViewEvent::Prompt`). Factor that recording out of `resume` into one helper both use.
- The join turns share the run's kill switch and budget: the `select!` on `kill` covers the wait
  and each `drive`; a spent budget ends the next `drive` as `Stopped` as today. A child joining is
  `ChildStatus::Running` (it is not waiting on a permission prompt); its row's age keeps counting.
- The report text is the final `drive`'s answer. A non-`Answer` outcome of any run ends the loop
  with the usual report, and `AgentTree::finish` still kills the running descendants of a child
  that was killed, failed or ran out of budget — the abnormal end, where no join is possible.
  Normally none remain; a descendant resumed between the join's last check and `finish` is stopped.
- `ChildHandle::kill` is unchanged.

### 3. Root behavior

- **Headless** keeps `settle`; `wait_for_news` uses `tree.running_below(ROOT_AGENT)` instead of
  `background_running()`. Its doc and `docs/headless.md` drop "background".
- **TUI**: unchanged mechanically. Interrupting a root turn (Esc) no longer kills any child, because
  there is no blocking call to drop: every child keeps running and reports later, as background
  children do today; the agents panel kills one. Quitting still kills all.
- The root's own turn has no join: the TUI root is never "terminating" until quit, and an
  answering headless root is joined by `settle`.

## Implementation Steps

1. **Tree** (`rust/fontaine-agents/src/tree.rs`): drop `NodeInfo.background` and the `reserve`
   parameter; replace `background_running` with `running_below(agent)`; `agent_state` returns
   `AgentDelivery::AlreadyDelivered`; reword the `Report` render and the `finish` doc ("its own
   running children are stopped: a child ends normally only once they have reported"). Update the
   unit tests (`bg` helper, `reserve` calls).
2. **Jobs seam** (`rust/fontaine-tools/src/jobs.rs`, `jobs/tools.rs`): `AlreadyDelivered` becomes a
   unit variant; `render_agent_wait` drops the foreground line; reword any "background subagent"
   text in `job_output`'s description to "subagent", including `jobs/tools.rs:282` ("For a subagent
   (`agent-*`, from `agent` with `background`)").
3. **Spawn** (`rust/fontaine-agents/src/spawn.rs`, `runner.rs`): remove `background` from
   `AgentArgs`, the schema, `SpawnRequest` and the `reserve` call; delete `KillOnDrop` and the
   blocking path; always `watch_report` and return `{agent_id}`; `report_text` appends `report.reason` for
   `Failed` too; new description; reword the report note. `registry.rs:119` comment loses "background".
4. **Notification prompt** (`rust/fontaine-core/src/agent.rs`, `child_runner.rs`): move
   `notification_prompt`/`screened_notes` into a free function taking the agent id; root delegates
   to it.
5. **Join** (`rust/fontaine-core/src/child_runner.rs`, `agent/setup.rs`): pass `results` from `gates()` (in
   scope at `agent.rs:252`) through `subagent_support` → `child_runner` (`agent/setup.rs:490-530`)
   → `ChildContext` → `RunShared` → `run_child` (`RunShared` already holds `steering`; the free
   `notification_prompt` calls `state.clear_current_call(agent)`); add
   `join_wait` and the run loop; factor the human-prompt recording shared with `resume`.
6. **Headless** (`rust/fontaine/src/headless.rs`): `running_below(ROOT_AGENT)`; doc wording.
7. **Wording sweep**: `rg -n -i "background agent|background subagent|background child"` over
   `rust/` (TUI turn label `fontaine-tui/src/ui/turn.rs:226` → "an agent reported back", comments
   in `ui/mod.rs`, `app.rs`, `goal.rs`, `agent/goal.rs`, `agents_panel.rs`, `commands.rs:142`
   "the agents panel"). Also `fontaine-tools/src/ask_user.rs:204` and `fontaine-agents/src/tree.rs:50, 712`, which the
   regex misses. Shell background jobs keep their wording.
7a. **Guardian prompt**: drop the "`background: true` is not a reason to deny" clause from
   `fontaine-guardian/prompts/guardian_prompt.md:73` and update
   `fontaine-guardian/src/snapshots/fontaine_guardian__prompt_builder__tests__the_defaults_marker_snapshot.snap`;
   drop `"background": true` from the agent args of case overdeny-04 in
   `evals/dataset/overdeny.jsonl`; run the live guardian eval after the prompt change.
8. **Tests** (see Testing Strategy) and **docs** (see Documentation).
9. **Plan Decisions** (`tasks/fontaine_harness_plan.md`, end of `### Agents`): the lines in
   `## Decisions` below that amend the master plan, dated 2026-10-30.

## Files to Modify

- `rust/fontaine-agents/src/{spawn.rs, runner.rs, tree.rs, registry.rs}`
- `rust/fontaine-agents/tests/seam.rs`
- `rust/fontaine-tools/src/{jobs.rs, jobs/tools.rs}`, `rust/fontaine-tools/src/jobs/tests.rs`, `rust/fontaine-tools/tests/jobs.rs`
- `rust/fontaine-core/src/{child_runner.rs, agent.rs, agent/setup.rs, goal.rs, agent/goal.rs}`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine/tests/{parallel_agents.rs, fork_agent.rs, telemetry_otlp.rs, common/mod.rs}`
- `rust/fontaine-core/tests/{agent_pipeline.rs, extensions.rs, choke_point.rs, agent_resume.rs, guardian_off.rs,
  permissions_screen.rs}`
  (fake `ChildRunner` on `SpawnRequest`, `reserve` calls)
- `rust/fontaine-guardian/prompts/guardian_prompt.md`, its `the_defaults_marker_snapshot.snap`,
  `evals/dataset/overdeny.jsonl`
- `rust/fontaine-tui/src/{ui/turn.rs, ui/mod.rs, app.rs, agents_panel.rs, commands.rs}`,
  `rust/fontaine-tui/tests/{agents.rs, tui.rs, sessions.rs, providers.rs}`
- `docs/{agents.md, tools.md, headless.md, tui.md}` (and any other page `rg background docs` finds
  about agents)
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Concurrent tool execution for `agent` calls (the issue's suggested approach)** —
  `tool_concurrency(n)` with an in-order gate pre-pass, or a per-tool concurrency policy. Rejected:
  it keeps a blocking spawn, so the parent's turn still waits for its slowest child and nothing can
  run between spawns and reports; it needs a gate pre-pass rig's `buffer_unordered` loop does not
  offer, and it reopens the ordering questions (mixed batches, `CurrentCall`, out-of-order cards)
  that an always-returning call avoids entirely.
- **Join by prompting the model to call `job_output`** (a harness note "your subagents are still
  running") instead of waiting in the runner. Rejected: it spends model calls on waiting and relies
  on the model complying; the runner wait is deterministic and reuses the root's screened
  notification prompt.

## Decisions

- 2026-10-30 (user, supersedes "2026-10-21 (user): The `agent` tool blocks until the child
  finishes"): Deviation from Claude Code — every `agent` spawn returns `{agent_id}` at once and
  the report arrives as a notification or through `job_output`; there is no blocking spawn and no
  `background` argument; reason: subagents must run concurrently with their parent and each other,
  and waiting is the parent's explicit choice (#326).
- 2026-10-30 (user): A subagent that answers while its own subagents run joins them before it ends:
  its run waits for their reports (or a message, or the human's words) and takes a further turn on
  each batch, as a headless root does; only a killed, failed or budget-exhausted subagent's
  descendants are killed. This amends the 2026-10-30 #301 reasoning that "finishing kills its
  descendants" (#326).
- 2026-10-30: Interrupting a turn no longer kills any subagent, since no call waits on one; the
  issue's "interrupting the turn kills every in-flight child" is superseded by the always-async
  spawn, and the agents panel stays the way to kill one (#326).
- 2026-10-30: Tool calls of a turn keep running one at a time (`tool_concurrency(1)`); concurrency
  comes from the always-returning spawn, so the gate's in-order judgment needs no pre-pass (#326).
- 2026-10-30: A subagent's join rounds are bounded only by its budget and the kill switch, like the
  headless root's `settle`; no separate round cap (#326).

## Documentation

- `docs/agents.md` (lines 6, 80, 121, 170, 199 are stale): rewrite the `agent` argument table (no `background`) and the "By default the
  call waits…" paragraph; the "Waiting for a background subagent" section becomes "Waiting for a
  subagent", which breaks the anchor `agents.md#waiting-for-a-background-subagent` linked from
  `docs/tools.md:21` (update that link); replace the "A subagent that nests should wait … because finishing stops them" bullet
  with the join rule; the headless paragraph (`agents.md:227-230`) drops "background"; the
  interrupt behavior (children keep running).
- `docs/tools.md`: the `agent` and `job_output` entries.
- `docs/headless.md`: "background subagents" → "subagents" in the end-of-run wait.
- `docs/checkpoints.md:9-11` ("A foreground subagent's edits belong to the prompt that started
  it…") and `docs/index.md:19`.
- `docs/tui.md:686, 725, 762`: the wording about foreground agents and background subagents.
- `CHANGELOG.md` is updated by the `pr` step.

## Testing Strategy

Unit (`fontaine-agents`, `fontaine-tools`):
- `tree.rs`: `running_below` counts running descendants only, and a child joining keeps its own
  children counted; `agent_state` reports `AlreadyDelivered` for a delivered report.
- `spawn.rs` / `tests/seam.rs` with a fake `ChildRunner`: an `agent` call returns `{agent_id}`
  before the fake child ends; `serde_json::from_value::<AgentArgs>(json!({.., "background": true}))`
  fails with "unknown field `background`" (seam tests build `AgentArgs` directly); two spawns in a
  row both reach the runner while the first fake child is still blocked (the concurrency
  acceptance at the seam).
- Seam tests encoding the deleted blocking behavior are deleted or rewritten:
  `a_blocking_call_returns_the_childs_report` (296), `a_budget_stopped_child_returns_a_partial_report`
  (538), the Failed-to-tool-error half of
  `a_failed_child_is_an_error_result_and_a_refused_start_frees_its_place` (548; the rewrite asserts
  the failure reason reaches the parent's note), `dropping_a_blocking_call_kills_the_child` (563),
  `messages_queued_for_a_child_killed_by_a_dropped_call_are_reported_undelivered` (578),
  `a_foreground_child_that_missed_a_message_is_resumed_with_it_after_its_report` (1003);
  `a_child_that_finishes_stops_its_own_background_children` (743) is renamed.
- `fontaine-tools/tests/jobs.rs` (with its fake `AgentWaits`, :375-404): `render_agent_wait` text for
  each `AgentDelivery`.

Integration (`fontaine -p` with the routed scripted endpoint, `fontaine/tests/parallel_agents.rs`):
- **Concurrency (acceptance 1)**: the root spawns two children in one turn, each with a
  two-turn route (a tool call, then text) with `.slow(D)`; the root then `job_output`s both. Assert
  from `Endpoint::requests()` order that B's first request arrives before A's second, and both
  reports reach the root.
- **Order and denial (acceptance 2)**: a turn of `agent(A)`, `agent(B)` that a deny rule matches,
  `agent(C)`; the audit log shows decisions in call order, B denied and never reported as spawned,
  A and C spawned.
- **Join on end**: rework `a_nested_parent_waits_for_its_own_background_child_instead_of_ending_early`
  so the lead answers right after spawning (no `job_output`); assert the lead takes a further turn
  whose request carries the grandchild's report, the root's result is that turn's answer, and the
  grandchild finished `done`, not killed.
- **Join wakes on a message**: a lead whose child `send_message`s it mid-run gets a turn on the
  message before the child's report.
- **Kill during join**: a cap the lead's own calls stay under, so `settle`'s `over_cap` →
  `kill_all` fires while the lead joins a slow grandchild; assert from the ledger both ended
  `killed`. (Extending `a_spent_budget_with_a_background_child_running_stops_the_run_and_kills_the_child`
  would not reach the join: the lead's own call crosses the cap and ends `BudgetExhausted`.)
- Convert the remaining blocking-spawn tests (`a_blocking_agent_call_returns_…`,
  `a_foreground_child_is_stopped_when_the_headless_cap_is_passed`,
  `a_blocking_report_and_a_background_notification_pass_through_the_result_pipeline`, the spawns in
  `fork_agent.rs` and `fontaine-tui/tests/agents.rs`) to the async flow: spawn, then the report as a
  notification or `job_output` result. Also in `parallel_agents.rs`:
  `a_childs_token_budget_stops_it_and_its_report_is_partial`,
  `a_cost_cap_on_an_unpriced_model_is_inactive_with_one_notice_and_the_token_cap_still_binds` and
  `the_per_call_model_overrides_the_types_and_bad_values_are_error_results` read the report from the
  notification or `job_output`, and the `spawn_args(.., background)` helper (:37) loses its
  parameter. In `fontaine-agents/tests/seam.rs`,
  `a_refused_automatic_resume_hands_the_refusal_and_the_text_to_the_child`,
  `a_killed_child_that_missed_a_message_is_not_resumed` and
  `of_two_racing_resumes_of_an_ended_child_one_starts_a_run` get an `until(..)` wait on the
  resume/undelivered state. `telemetry_otlp.rs`
  `subagents_have_spans_under_their_spawning_call_and_decisions_keyed_by_agent`: the subagent span
  still nests under the spawning `agent` call's span; `docs/telemetry.md:102` stays consistent.
  Both tests in `fork_agent.rs` use one ordered queue `[fork_call(), "fork done", "root done"]` and
  index `requests[1]`/`requests[2]`; with async spawn the root's follow-up races the fork's first
  request for that queue, and the routed endpoint cannot tell them apart because a fork shares the
  root's first user message. They move to the routed endpoint with a new `Route::on_last_user(marker)`
  matcher in `tests/common/mod.rs` (matches the last user message, which for the fork is its task and
  for the root is still the turn's prompt), the fork's route listed before the root's; the fork's
  request is found by content (its last user message), not by index. `with_one_child` in `fontaine-core/tests/agent_resume.rs`
  (:54-70) is reworked to spawn first and then get the report as a notification, using a routed,
  keyed mock instead of one ordered queue `[spawn, "child answer", "root done"]`, which assumed a
  blocking child.
- **TUI**: `parallel_subagents_one_killed_the_others_finish_and_every_call_is_audited_by_agent`
  (`fontaine-tui/tests/agents.rs:83-130`) is extended with an assertion that the agent ids appear
  on the root's tool cards before any child ends.

## Open Questions

None.
