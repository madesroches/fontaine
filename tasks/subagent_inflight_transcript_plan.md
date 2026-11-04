# Subagent In-Flight Transcript Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/404

## Overview

A subagent's conversation reaches `logs/agent-<n>.jsonl` only when its run settles, so a process
kill (SIGKILL, OOM, systemd-oomd) loses the whole in-flight run. This adds an in-flight journal:
as the run progresses, the messages it commits are appended to a second log in the agent's store,
which is discarded when the run settles normally and read by `/export` when it outlives a kill.
The settle-time save stays the authoritative record.

## Current State

- `child_runner.rs:566-580` builds each subagent with `.memory(store.clone()).conversation(id)`
  and a `RecoveryHook`. rig-agent's engine loads the conversation at run start and, on
  `AgentRunStep::Done`, extends it with `run.messages()` and calls `save_async` once
  (`rig-agent/src/agent/engine.rs`, the `Done` arm). On failure it calls `on_run_settled` with
  `SettledOutcome::Error` and `messages: Some(run.messages())`, which `RecoveryHook`
  (`turn_recovery.rs:46`) appends to the log. A kill reaches neither path.
- rig-memory's `save_async` refuses a conversation that does not continue the log
  (`check_continues` → `SessionError::Changed`).
- `on_run_start` (`RunStart.history` gives the input length) and `on_completion_call`
  (`CompletionCallEvent {prompt, history, turn}`, fired before every model call) hand over the
  run's committed messages mid-run: `history[input_len..] + prompt` is exactly what the engine
  has committed so far. `on_model_turn_finished` content can differ from what is committed, and
  `OutcomeEvent` has no batch information, its results are rewritten after hooks and malformed
  calls never reach it. `RunSettled.messages` exists only at the end.
- No Fontaine code enumerates a store's logs.
- `export.rs:145` renders a subagent with `load_async(&store, &agent.id)`; `docs/agents.md`
  ("A subagent's conversation") documents that a running agent has its completed runs only on
  disk.
- The torn-tail tolerance is already pinned by `turn_recovery.rs::a_torn_log_remains_readable`.

## Design

A second log per agent, `<agent-id>.inflight`, in the same store. It holds only the messages
committed by the run in progress (the prompt onward), so it never conflicts with the engine's
save of `<agent-id>`.

```
run start      ──► journal reset, record input length
completion call──► append the not-yet-journaled tail of history[input_len..] + prompt
settle OK ──► (engine saved <id>) ──► journal removed
settle Err──► RecoveryHook appends to <id> ──► journal removed
SIGKILL   ──► journal stays; export reads <id> then <id>.inflight
```

### `InflightHook<S>` (new, in `turn_recovery.rs`)

An `AgentHook` over an `AsyncStore`, beside `RecoveryHook` and sharing `append_messages` and the
warn-through-`Approver` failure handling:

- `on_run_start`: remove any stale journal (a previous kill's leftover for this id is superseded
  because the id is never reused, but a resumed run in the same process starts a new journal),
  and record the input length from `RunStart.history`. Returns `RunStartAction::Continue`.
- `on_completion_call`: append the not-yet-journaled tail of `history[input_len..] + prompt`
  (exactly the committed messages) to the journal. The prompt is journaled by the first call.
- `on_model_turn_finished` (optional): may append the last assistant turn while its tools are
  still running; every other message comes from `on_completion_call`.
- `on_run_settled`: remove the journal on both outcomes. `RecoveryHook` is registered before it,
  so the error-path append to `<id>` lands first.

Every append is one `append_messages` call to the journal log (the existing helper: load, extend,
`save_async`). That is O(n) per append; acceptable for subagent transcripts, and `DirStore`'s
`save_async` appends only the new messages. Failures are warned once per run via `Approver::warn`, never
fatal, and never change a permission decision. Journal I/O must not block the kill path:
`KillHook` is unaffected.

### Reading the journal

`export.rs` loads `<id>` and then `<id>.inflight`; `load_async` on a missing log returns an empty
`Ok`, so an empty loaded journal means "no journal" and no marker is written. Otherwise its
messages follow a "run interrupted" marker so the export distinguishes them. A fork child's
`<id>` is seeded with the parent's context before its first run, so its export shows the seeded
context, then the marker and the journal. `load_async` already tolerates a torn tail. A live
agent's journal is removed at settle, except when the stream is dropped without settling (a
spent budget, the kill-grace timeout): the journal then survives for an agent in the same process
too and `/export` shows it the same way. The existing "completed runs only" note applies except
that the export may include the journal of the run in progress.

Scope: subagents only (the issue). `InflightHook` is generic over the store and log, so the root
could register it later without change.

## Implementation Steps

1. `fontaine-core/src/turn_recovery.rs`: add `InflightHook<S>` and a `journal_log(id) -> String`
   helper (`"{id}.inflight"`); unit tests below.
2. `fontaine-core/src/child_runner.rs`: register `InflightHook` after `RecoveryHook`.
3. `fontaine-core/src/export.rs`: append a present journal after the agent's log, with a marker;
   the missing-journal case is unchanged.
4. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/turn_recovery.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine-core/src/export.rs`
- `rust/fontaine-core/tests/agent_pipeline.rs` (`mod killed`)
- `docs/agents.md`, `docs/tui.md` (export note), `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (Decisions, Agents)

## Trade-offs

- **Same log, incremental appends** (the issue's first suggestion): rejected, because the engine's
  settle-time `save_async` would fail with `SessionError::Changed` on a log that grew under it.
  Making it work needs a custom save or a fork of rig's run loop, which the project forbids.
- **Separate journal log** (chosen): no conflict with the engine, no custom loop, torn-tail
  tolerance for free; costs a second file and a merge at read time.
- **SIGTERM flush only**: does not cover SIGKILL/OOM, the observed failure; not pursued.

## Decisions

- 2026-11-03: The in-flight journal is a separate log `<agent-id>.inflight`, not an append to
  `<agent-id>`, because rig's final save refuses a log that moved under it.

## Documentation

- `docs/agents.md`, "A subagent's conversation": replace "saved when a run ends" with the journal
  behaviour — an interrupted process leaves the in-flight run's messages, shown by `/export`.
- `docs/tui.md` export paragraph (line ~546): a running or killed agent's export includes its
  in-flight run.
- `CHANGELOG.md` under Unreleased → Fixed: subagent transcripts survive a killed process (#404).
- Add the Decisions line above to the master plan's Agents subsection.

## Testing Strategy

Unit tests in `turn_recovery.rs` against `MemoryStore`/`DirStore`, driven by calling the hook
methods with constructed events:

- journal holds prompt, assistant and tool-result messages in order after each `on_completion_call`;
- journal is removed on `Response` and on `Error` settle, and on error the messages are in `<id>`;
- a stale journal is replaced at run start;
- a torn journal still loads (extends `a_torn_log_remains_readable`);
- a store failure warns once and the run continues.

Integration test in `rust/fontaine-core/tests/agent_pipeline.rs` `mod killed`, reusing
`build_held`/`child_log`/`wait_report`: the held request stands in for a run that never completes;
assert `agent-N.inflight` holds the prompt, the first assistant turn and its tool result, and
that `export_session` renders them. This pins the bug in #404 at the tier that reproduces it
(a run with no settle). A successful run leaves no `.inflight` file and an unchanged
`agent-N.jsonl`.
