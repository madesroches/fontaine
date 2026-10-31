# Killed Subagent Transcript Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/348

## Overview

A subagent killed mid-run leaves no transcript: `logs/agent-<n>.jsonl` never appears, so the
evidence needed to diagnose why it was stopped is gone. This plan makes a killed run settle
gracefully instead of being dropped, so the existing recovery persistence saves the transcript
up to the kill — always on, with no config gate.

## Current State

- rig-agent saves a run's conversation to its memory store only when the run ends
  (`rig-agent-0.45.0/src/agent/engine.rs:423-427`, the `AgentRunStep::Done` branch:
  `conversation.extend(run.messages()); store.save(...)`). A run that never reaches `Done`
  leaves nothing on disk.
- Fontaine implements the kill by dropping the run's future: `run_child`
  (`rust/fontaine-core/src/child_runner.rs:615-623`) selects
  `kill.wait_for(|killed| *killed) => break None` over the in-flight `drive` future, and the
  `join_wait` after a completed run (`child_runner.rs:627-631`) the same. Dropping the drive
  future drops rig's run state with the run's committed messages inside; the run never
  settles.
- `RecoveryHook` (`rust/fontaine-core/src/turn_recovery.rs:40-62`) persists a *settled*
  error's committed messages (`on_run_settled` with `SettledOutcome::Error`). It is registered
  on every child (`child_runner.rs:463-467`), but a dropped run never settles, so the hook
  never fires on a kill.
- Completed runs are saved, so a killed agent that finished at least one run keeps its
  transcript; one killed during its first run (the issue's `agent-3`, ledger seq 129) has
  none at all. `/export` then lists it as "transcript unavailable"
  (`export.rs` `index_section`), and a later resume has nothing to continue.
- rig-agent is built for graceful termination: a hook's stop action ends the run through
  `run.cancel_error(reason)` (`engine.rs:284-286` for `CompletionCallAction::Stop`,
  `engine.rs:1601-1607` for a `Cancelled` tool-dispatch denial), producing
  `PromptError::Cancelled { chat_history, reason }` (`run/response.rs:276-284`) with the
  history closed by `pairing::repair`. Every such exit goes through the `fail!` macro
  (`engine.rs:161-189`), which settles with `SettledOutcome::Error` and
  `messages: Some(run.messages())` — exactly what `RecoveryHook` already persists.
- Kill plumbing today: `AgentTree::kill` (`rust/fontaine-agents/src/tree.rs:590-617`) marks
  the node `Killed` immediately and calls `ChildHandle::kill`; the core handle
  (`child_runner.rs:349-351`) sets a `tokio::sync::watch` flag, and `run_child` selects on
  its receiver. `attach` (`tree.rs:434-449`) replays a kill that landed before the handle
  existed. `tree.finish` (`tree.rs:503-526`) keeps a node already marked `Killed`.
- Every tool call passes the single choke point `gated` (`rust/fontaine-tools/src/guarded.rs`),
  which extracts the calling agent (`agent_of`, `guarded.rs:167-172`) and then awaits
  `guard.gate.check` (the floor, rules, guardian and the human's prompt — the wait carries a
  drop-safe `WaitGuard`, `rust/fontaine-core/src/gate.rs:853-864`) and, on allow, the tool
  body. Pre-edit images are captured around the body (`guarded.rs:148-176`).
- The root agent has no kill switch; a TUI interrupt of the root turn drops the drive future
  the same way (`turn.rs:151`) and is out of scope here.

## Design

### Part 1 — stop a killed run gracefully, and save what it committed

**KillHook.** A new `AgentHook` holding a clone of the child's kill watch receiver. It stops
the run at the two hook boundaries that bracket a model turn:

- `on_completion_call` → when the flag is set, `CompletionCallAction::Stop(KILL_REASON)`.
  This fires before every model request, so a kill that arrived mid-stream takes effect
  before the next request is sent.
- `on_dispatch` → when the flag is set, `DispatchAction::stop(KILL_REASON)`, refusing the
  tool call with a `Cancelled` report. This fires before each tool call is dispatched, so a
  kill between tool calls in a batch stops the batch before the next call runs.

Both actions end the run through `cancel_error` → the `fail!` settle, which fires
`on_run_settled(Error, messages)`; the already-registered `RecoveryHook` then appends the
committed messages to `logs/agent-<n>.jsonl` — the same append a failed run gets. No new
persistence code, and the transcript lands in the log export and resume already read.

`KILL_REASON` is the cancellation reason recorded in the run's error ("the run was
cancelled: it was killed"); it never reaches the model, since the run ends there.

KillHook is registered in `CoreChildRunner::spawn` alongside `RecoveryHook`. The kill channel
moves above the builder so the hook can subscribe to it.

**Grace, then the hard drop stays as the backstop.** `run_child` keeps its kill select, but
the drive branch now gets a bounded grace window: when the flag fires, await the in-flight
run for up to `KILL_GRACE` (a named constant; proposed 30 s). The KillHook settles the run at
the next hook boundary, normally well inside the window, and the transcript is saved. If the
run has not settled when the grace expires — a hung provider stream, a wedged hook — the
future is dropped exactly as today: the kill is guaranteed, the transcript may be lost. The
select in `join_wait` is unchanged (the previous run already saved at its `Done`; nothing is
lost there).

**Outcome mapping.** A run that settles after the kill fired arrives as
`TurnOutcome::Failed` (rig's `PromptError::Cancelled` is not retryable, `turn.rs` test
"a cancel is not retried"). When the kill flag is set, `run_child` reports
`ChildStatus::Killed` with the usual reason ("it was killed") and the partial-text report —
matching `tree.kill`, which already marked the node `Killed` the moment the user clicked.
The kill-before-spawn case (`attach`'s replay) now saves a transcript holding just the
prompt, instead of none.

### Part 2 — kill in-flight work at the tool choke point

The KillHook covers the model stream and the gaps between tool calls, but a kill landing
*inside* a long tool call or a permission prompt would otherwise wait out the grace and then
be dropped without a transcript — the runaway-command case is exactly the one the issue
wants to diagnose. So the choke point becomes kill-aware:

**KillSwitches.** A small registry in `fontaine-tools` (`kills.rs`):
`KillSwitches { inner: Mutex<HashMap<String, watch::Receiver<bool>>> }` with `register(agent,
receiver)` and a non-awaiting `killed(agent) -> bool`. Core creates one per session and
hands it to both the child runner (which registers each child's receiver at spawn) and the
tool registry. The root registers nothing and is unaffected. The session
instance also reaches the
separate `Guard` that wraps MCP tools — `connect_mcp` builds it directly rather than
through the registry, so it takes the same `Arc` by a different path (step 6).

**The select in `gated`.** `Guard` gains `kills: Option<Arc<KillSwitches>>`. In `gated`
(`guarded.rs`), once the agent is known:

- If the agent is already killed, the call is refused immediately (nothing new starts for a
  dead agent — this also covers a call that begins after the kill but before the run's next
  hook boundary).
- Otherwise the permission wait (`guard.gate.check`) and the allowed tool body
  (`run(...)`) each run under a biased `tokio::select!` against the agent's kill wait. On
  kill: the inner future is dropped (the pending permission prompt and in-flight tool are
  dropped exactly as today's whole-drive drop drops them; the `WaitGuard` and pre-image
  capture bookkeeping unwind the same way) and the call returns a refusal
  ("the agent was killed") that skips the result pipeline — no guardian screen of a
  synthetic result.

The refusal becomes the tool's result; the run advances, and the KillHook stops it at the
next model request with the cancelled call in the committed history — so the transcript
shows the call that was cut short.

### Behavior

| Kill lands during | Today | After |
|---|---|---|
| Model stream | Drop; no transcript of the run | Run settles at the next request; transcript saved |
| Tool call | Drop; no transcript of the run | Call cut short at the choke point; transcript saved with the cancelled call |
| Permission wait | Drop cancels the prompt; no transcript | Prompt dropped at the choke point; transcript saved |
| Between runs (`join_wait`) | Drop; transcript of completed runs kept | Unchanged |
| Hung stream/tool | Drop | Grace, then drop as today |

What deliberately does not change: the tree flips the node to `Killed` immediately (the UI
shows it at once; only the report and the ledger's `finished` record wait for the run to
settle, up to the grace); a killed child still cannot be resumed; `kill_all` and session
exit are unchanged (`tree.settled` already waits for the ledger records); the root agent's
interrupt is untouched.

## Implementation Steps

1. **`rust/fontaine-tools/src/kills.rs`** (new) — `KillSwitches` registry; register, `killed`,
   and an `async fn wait(&self, agent) -> KillWait` (a `wait_for`-style future, or
   `pending()` when the agent has no switch) for the choke-point select.
2. **`rust/fontaine-tools/src/guarded.rs`** — add `kills` to `Guard`; in `gated`, the
   pre-check refusal and the two kill-selects around `gate.check` and `run`; skip the result
   pipeline for a kill refusal; keep the `tool_result` telemetry record.
3. **`rust/fontaine-core/src/child_runner.rs`** — `KillHook` (new private type) with
   `on_completion_call` and `on_dispatch`; move the kill channel above the agent build in
   `spawn`, register KillHook (and register the receiver with the session's `KillSwitches`);
   add `kills` to `ChildContext`, threaded from the `subagent_support` call site.
4. **`rust/fontaine-core/src/child_runner.rs` `run_child`** — on kill, grace the in-flight
   run (`tokio::time::timeout(KILL_GRACE, &mut run)`); on expiry drop as today; when the
   kill flag is set, report `ChildStatus::Killed` for any settle outcome.
5. **`rust/fontaine-core/src/tools.rs`** — accept the session `Arc<KillSwitches>` in
   `RegistryParams` and pass it into each `Guard`.
6. **`rust/fontaine-core/src/agent/mcp.rs`** — `connect_mcp` builds its own `Guard`
   (`mcp.rs:141-146`, feeding `GuardedSink` → `guard_callback`), so step 5 alone leaves
   every MCP call without a kill switch; adding `kills` to `Guard` also breaks this
   struct literal, which must be touched to compile regardless. Store the session
   `Arc<KillSwitches>` on `AgentRuntime` (created in `AgentRuntime::new` alongside
   `pipeline`/`results`, so the same instance also feeds `subagent_support` and
   `registry()`) and set `kills: self.kills.clone()` in the literal. The instance reaches
   the runtime through `AgentSetup` — built in `fontaine/src/lib.rs`, driven by the TUI —
   and `connect_mcp` runs from `fontaine-tui/src/ui/mcp.rs` and `fontaine/src/lib.rs`,
   a different path from the `registry()` call site; check it too.
7. **Tests** (see Testing Strategy).
8. **Docs** — `CHANGELOG.md` (`Fixed` under `[Unreleased]`); `docs/agents.md` (what kill
   keeps and when); `docs/architecture.md` (session layout: a killed agent's log holds its
   committed messages); the plan's Decisions below.
9. **`tasks/fontaine_harness_plan.md`** — a dated Decisions line under the `Agents`
   subject: killed subagents persist their partial transcript; graceful settle + grace +
   choke-point kill, no config gate.

## Files to Modify

- `rust/fontaine-tools/src/kills.rs` (new), `src/guarded.rs`, `src/lib.rs`
- `rust/fontaine-core/src/child_runner.rs`, `src/tools.rs`, `src/agent/mcp.rs`, `src/agent.rs` /
  `src/agent/setup.rs` (threading `KillSwitches`)
- `rust/fontaine-core/tests/agent_pipeline.rs` (+ shared harness as needed)
- `rust/fontaine-tools/tests/guarded.rs` (new or existing test file)
- `CHANGELOG.md`, `docs/agents.md`, `docs/architecture.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Graceful settle vs. today's instantaneous drop.** A kill no longer interrupts a model
  stream mid-token: it lands at the next hook boundary (before the next request, before the
  next tool call) or immediately at the choke point. The cost is bounded latency in one
  window; the gain is the transcript, which is the issue's whole purpose. The grace-plus-drop
  backstop keeps kill total: a wedged run still dies after `KILL_GRACE`, as today.
- **Choke-point kill vs. waiting out tool calls.** Selecting the tool body against the kill
  flag drops an in-flight tool future from inside the harness rather than from the top. That
  is the same drop today's kill performs (the drive future holds the tool future), so
  tool-level cleanup semantics — e.g. a spawned process's fate on drop — are unchanged; the
  difference is that the run around the tool survives to settle and be saved.
- **Hook stop vs. reconstructing messages from events.** The hooks rig provides carry no
  committed-message view except `RunSettled`; rebuilding the transcript from
  `ModelTurnFinished` content plus `OutcomeEvent` results would duplicate rig's
  canonicalization (batching, retries, feedback) and diverge from what a resumed run reads.
  The hook-stop path reuses rig's own `cancel_error` history and the existing
  `RecoveryHook` append — one writer, one format.
- **No kill for the root.** The root has no kill switch and its interrupt is a separate
  surface; folding it in would touch the TUI's turn lifecycle for no issue requirement.
  Follow-up if wanted: the same KillHook pattern applies to a root interrupt channel.
- **No config gate.** The behavior is unconditional, per the user's call: during
  development every agent's transcript is worth keeping, and an opt-out knob adds a test
  matrix for a failure mode (lost evidence) that has no upside to preserve.

## Decisions

- 2026-10-31 (user call, mid-design): a killed agent keeps the transcript it produced up to
  the kill, for all agents, enabled by default — no config gate, no opt-out.
- 2026-10-31: The kill mechanism is graceful settle (KillHook at rig's hook boundaries) plus
  a bounded grace with the existing hard drop as backstop, and a kill-select at the
  `gated` choke point for in-flight tool calls and permission waits. The instantaneous
  whole-drive drop is kept only as the grace-expiry fallback.
- 2026-10-31: Persistence reuses the existing `RecoveryHook` append on the rig
  `RunSettled(Error)` path; no new transcript-writing code and no message reconstruction.

## Documentation

- `docs/agents.md` — the kill entry (killing a subagent): a killed subagent's transcript up
  to the kill is kept in its per-agent log and appears in `/export`; kill lands at the next
  boundary of the in-flight model call or tool, with a bounded backstop.
- `docs/architecture.md` — session layout: `logs/agent-<n>.jsonl` now exists for any agent
  that committed at least a prompt, including killed ones (previously: completed runs only).
- `CHANGELOG.md` — `Fixed`: killed subagents keep their partial transcript.
- `tasks/fontaine_harness_plan.md` — Decisions line per Implementation Steps.

## Testing Strategy

All deterministic, no live APIs; the wild-observed bug (#348's `agent-3`) is pinned by the
first integration test.

- `fontaine-tools` `kills.rs`: register/lookup, unknown agent is never killed, `wait`
  pending for an unregistered agent.
- `fontaine-tools` choke point: a registered kill during a blocking tool call returns the
  refusal and the pre-image capture still completes; a kill during a never-answered
  permission wait drops the wait (the `WaitGuard` clears `is_waiting`); an unregistered
  agent (root) is untouched; a call starting after the kill is refused pre-flight.
- Core integration (`rust/fontaine-core/tests/agent_pipeline.rs` harness, scripted completion
  models): spawn a child, kill it after its first committed turn (the harness observes the
  agent's events, then kills) — assert `logs/agent-<n>.jsonl` exists in the session store
  and holds the committed messages up to the kill, the report status is `killed`, and the
  ledger's `finished` record says `killed`. This is the regression test for the issue.
- Same harness: kill before the first model call (the `attach` replay shape) leaves a log
  holding the prompt; kill between runs (after an answer, during the join wait) keeps the
  completed run's log; a kill-refused tool call appears in the transcript as the cancelled
  result.
- Unit: `KillHook` returns `Continue`/`Proceed` with the flag clear and the stop actions
  with it set; `run_child`'s mapping reports `Killed` for a run that settles after the flag
  fired, including a concurrently-completed `Answer`.

## Manual Verification

1. `cargo run -p fontaine` in a scratch repo; ask for a long-running subagent (e.g. one that
   runs a slow build), select it in the agents panel and kill it mid-run.
2. Check `<data dir>/sessions/<id>/logs/`: `agent-<n>.jsonl` exists and ends at the kill.
3. `/export` and open the folder: the killed agent's file renders its conversation up to the
   kill.

Not automated above the integration tier because the TUI agents-panel keystrokes and the
visual read of the export add no assertion the core integration test does not already make;
the steps exist to eyeball the real product path once.
