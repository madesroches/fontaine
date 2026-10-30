# Async Subagents Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/326

## Overview

Every `agent` spawn becomes asynchronous: the call returns `{"agent_id": …}` at once, the child
runs alongside its parent and its siblings, and its report reaches the parent as a notification.
A parent that wants a result now waits for it explicitly with `job_output`. The only implicit
join happens when a parent is ending: a subagent that gives its final answer while its own
children still run keeps its run open until they report, takes a further turn on what they said,
and only then ends; the main conversation does the same in a headless run (it already does). This
removes the blocking spawn that serialized "spawn 2 agents to verify these" in #326, and does so
without touching the turn's one-call-at-a-time execution, so the permission pipeline's ordering
guarantees are unchanged.

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
  (`spawn.rs:47-76, 395-423`) are deleted. `ReleaseOnDrop` stays: `runner.spawn` still awaits.
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

No change to `turn.rs`: the calls of a turn still run one at a time, so the floor, hooks and
guardian judge each `agent` call before the next, in call order, and a denied call starts nothing.
Concurrency comes from the children, not from the tool loop.

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

- `join_wait` waits on `tree.changed()` (register-then-check, as `AgentTree::wait` does) until
  either the child's inbox holds a note (a report or a `send_message`), the human typed to it
  (`steering.has_deliverable_message_for(id)`), or `tree.running_below(id)` is false and the inbox
  is empty. A report is queued before its child is marked finished (`watch_report`), so the last
  check cannot miss one.
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
  After a normal end there are none to kill.
- `ChildHandle::kill` is unchanged; a kill during the join wait yields a `Killed` report whose text
  is the last answer.

### 3. Root behavior

- **Headless** keeps `settle`; `wait_for_news` uses `tree.running_below(ROOT_AGENT)` instead of
  `background_running()`. Its doc and `docs/headless.md` drop "background".
- **TUI**: unchanged mechanically. Interrupting a root turn (Esc) no longer kills any child, because
  there is no blocking call to drop: every child keeps running and reports later, as background
  children do today; the agents panel kills one. Quitting still kills all.
- The root's own turn has no join: the TUI root is never "terminating" until quit, and an
  answering headless root is joined by `settle`.

### 4. Ordering and shared per-turn state

Because each `agent` call returns once its child is attached, the tool loop never has two calls
in flight, so `CurrentCall`, the decision feed and the ledger see the same one-call-at-a-time
sequence as today. Concurrent children each run their own `drive` with their own `AgentId`, which
is the background-child path that already works (#301, #321).

## Implementation Steps

1. **Tree** (`rust/fontaine-agents/src/tree.rs`): drop `NodeInfo.background` and the `reserve`
   parameter; replace `background_running` with `running_below(agent)`; `agent_state` returns
   `AgentDelivery::AlreadyDelivered`; reword the `Report` render and the `finish` doc ("its own
   running children are stopped: a child ends normally only once they have reported"). Update the
   unit tests (`bg` helper, `reserve` calls).
2. **Jobs seam** (`rust/fontaine-tools/src/jobs.rs`, `jobs/tools.rs`): `AlreadyDelivered` becomes a
   unit variant; `render_agent_wait` drops the foreground line; reword any "background subagent"
   text in `job_output`'s description to "subagent".
3. **Spawn** (`rust/fontaine-agents/src/spawn.rs`, `runner.rs`): remove `background` from
   `AgentArgs`, the schema, `SpawnRequest` and the `reserve` call; delete `KillOnDrop` and the
   blocking path; always `watch_report` and return `{agent_id}`; new description; reword the report
   note. `registry.rs:119` comment loses "background".
4. **Notification prompt** (`rust/fontaine-core/src/agent.rs`, `child_runner.rs`): move
   `notification_prompt`/`screened_notes` into a free function taking the agent id; root delegates
   to it.
5. **Join** (`rust/fontaine-core/src/child_runner.rs`, `agent/setup.rs`): add `results` to
   `ChildContext`; give `run_child` the steering handle and the pipeline (through `RunShared`); add
   `join_wait` and the run loop; factor the human-prompt recording shared with `resume`.
6. **Headless** (`rust/fontaine/src/headless.rs`): `running_below(ROOT_AGENT)`; doc wording.
7. **Wording sweep**: `rg -n -i "background agent|background subagent|background child"` over
   `rust/` (TUI turn label `fontaine-tui/src/ui/turn.rs:226` → "an agent reported back", comments
   in `ui/mod.rs`, `app.rs`, `goal.rs`, `agent/goal.rs`, `agents_panel.rs`, `commands.rs:142`
   "the agents panel"). Shell background jobs keep their wording.
8. **Tests** (see Testing Strategy) and **docs** (see Documentation).
9. **Plan Decisions** (`tasks/fontaine_harness_plan.md`, end of `### Agents`): the lines in
   `## Decisions` below that amend the master plan, dated 2026-10-30.

## Files to Modify

- `rust/fontaine-agents/src/{spawn.rs, runner.rs, tree.rs, registry.rs}`
- `rust/fontaine-agents/tests/seam.rs`
- `rust/fontaine-tools/src/{jobs.rs, jobs/tools.rs}`, `rust/fontaine-tools/src/jobs/tests.rs`
- `rust/fontaine-core/src/{child_runner.rs, agent.rs, agent/setup.rs, goal.rs, agent/goal.rs}`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine/tests/{parallel_agents.rs, fork_agent.rs}` and any other test passing `background`
- `rust/fontaine-tui/src/{ui/turn.rs, ui/mod.rs, app.rs, agents_panel.rs, commands.rs}`,
  `rust/fontaine-tui/tests/agents.rs`
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
- **Keep `background` as an ignored field** for models that still send it. Rejected: the schema
  no longer offers it, and an `invalid_args` result that names the field is self-correcting; a
  silently ignored flag would hide that the semantics changed.
- **Join by prompting the model to call `job_output`** (a harness note "your subagents are still
  running") instead of waiting in the runner. Rejected: it spends model calls on waiting and relies
  on the model complying; the runner wait is deterministic and reuses the root's screened
  notification prompt.
- **Keep killing a finishing parent's children** (today's `finish`). Rejected by the user's
  direction: ending is the one point where an implicit join belongs, and killing turns a model's
  early answer into lost work.

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

- `docs/agents.md`: rewrite the `agent` argument table (no `background`) and the "By default the
  call waits…" paragraph; the "Waiting for a background subagent" section becomes "Waiting for a
  subagent"; replace the "A subagent that nests should wait … because finishing stops them" bullet
  with the join rule; the headless paragraph (`agents.md:227-230`) drops "background"; the
  interrupt behavior (children keep running).
- `docs/tools.md`: the `agent` and `job_output` entries.
- `docs/headless.md`: "background subagents" → "subagents" in the end-of-run wait.
- `docs/tui.md`: any wording about foreground agents or interrupts killing them; the panel title.
- `CHANGELOG.md` is updated by the `pr` step.

## Testing Strategy

Unit (`fontaine-agents`, `fontaine-tools`):
- `tree.rs`: `running_below` counts running descendants only, and a child joining keeps its own
  children counted; `agent_state` reports `AlreadyDelivered` for a delivered report.
- `spawn.rs` / `tests/seam.rs` with a fake `ChildRunner`: an `agent` call returns `{agent_id}`
  before the fake child ends; a `background` argument is an `invalid_args` error; two spawns in a
  row both reach the runner while the first fake child is still blocked (the concurrency
  acceptance at the seam).
- `jobs/tools.rs`: `render_agent_wait` text for each `AgentDelivery`.

Integration (`fontaine -p` with the routed scripted endpoint, `fontaine/tests/parallel_agents.rs`):
- **Concurrency (acceptance 1)**: the root spawns two children in one turn, each route `.slow(…)`
  for D; the root then `job_output`s both. Assert both children's requests overlap (each child's
  first request reaches the endpoint before the other's response; or total wall-clock < 2·D with
  margin) and both reports reach the root.
- **Order and denial (acceptance 2)**: a turn of `agent(A)`, `agent(B)` that a deny rule matches,
  `agent(C)`; the audit log shows decisions in call order, B denied and never reported as spawned,
  A and C spawned.
- **Join on end**: rework `a_nested_parent_waits_for_its_own_background_child_instead_of_ending_early`
  so the lead answers right after spawning (no `job_output`); assert the lead takes a further turn
  whose request carries the grandchild's report, the root's result is that turn's answer, and the
  grandchild finished `done`, not killed.
- **Join wakes on a message**: a lead whose child `send_message`s it mid-run gets a turn on the
  message before the child's report.
- **Kill during join**: a joining lead killed by the headless budget cap ends `killed` and its child
  is killed (extend `a_spent_budget_with_a_background_child_running_stops_the_run_and_kills_the_child`).
- Convert the remaining blocking-spawn tests (`a_blocking_agent_call_returns_…`,
  `a_foreground_child_is_stopped_when_the_headless_cap_is_passed`,
  `a_blocking_report_and_a_background_notification_pass_through_the_result_pipeline`, the spawns in
  `fork_agent.rs` and `fontaine-tui/tests/agents.rs`) to the async flow: spawn, then the report as a
  notification or `job_output` result.

These are concurrency and lifecycle behaviors whose failure is silent (a lost report, a child
killed early, serialized runs), so they are automated end to end through the scripted endpoint
rather than checked by hand.

## Manual Verification

1. `cargo run -p fontaine` with a real model, ask "spawn two explore agents to each list a
   different crate's files, then summarize both". Expected: the agents panel shows both running at
   once; the root's tool cards settle immediately with the ids. Why manual: the overlapping rows in
   a live TUI are a visual check, and the timing is already covered by the integration test.

## Open Questions

None.
