# Subagent In-Flight Transcript Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/404

## Overview

A subagent's conversation reaches `logs/agent-<n>.jsonl` only when its run settles, so a process
kill (SIGKILL, OOM, systemd-oomd) loses the whole in-flight run. This adds an in-flight journal:
as the run progresses, the messages it commits are appended to a second log in the agent's store,
which is emptied when the run settles normally and read by `/export` when it outlives a kill.
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
  has committed so far.  `RunSettled.messages` exists only at the end.
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
settle OK ──► (engine saved <id>) ──► journal truncated to empty
settle Err──► RecoveryHook appends to <id> ──► journal truncated to empty if <id> holds those messages
SIGKILL   ──► journal stays; export reads <id> then <id>.inflight
```

### `InflightHook<S>` (new, in `turn_recovery.rs`)

An `AgentHook` over an `AsyncStore`, beside `RecoveryHook` and sharing `append_messages` and the
warn-through-`Approver` failure handling:

- Per-run state (input length, journaled count, warned-once flag) lives in the run's `HookContext`
  scratchpad (rig's per-run `Scratchpad`, created fresh each run), set at `on_run_start`: hook
  methods take `&self` and one child agent serves many runs (prompts, idle-agent messages,
  retries), so a reused agent starts each run clean.
- `on_run_start`: truncate any stale journal to empty via `replace_log_async(<id>.inflight, len, b"")`,
  skipped when already empty (rig-memory's `AsyncStore` cannot remove a log; a resumed run in the
  same process starts a new journal, and `load_async` of an empty log is an empty `Ok` so
  `append_messages` keeps working), and record the input length from `RunStart.history`. Returns `RunStartAction::Continue`.
- `on_completion_call`: append the not-yet-journaled tail of `history[input_len..] + prompt`
  (exactly the committed messages) to the journal. The prompt is journaled by the first call.
- `on_run_settled`: on `Response`, truncate the journal to empty as above. On `Error`, load `<id>`
  and truncate only if it holds input length + `messages.len()` messages (rig-memory changes some
  messages on load, e.g. image encodings, so equality is unreliable); otherwise keep the journal
  and say so in the warning. When no input length was recorded (a memory-load failure settles
  `Error` before `on_run_start`) the hook leaves the journal alone. `RecoveryHook` is registered
  before it, so its append to `<id>` lands first; it only warns on failure and shares no state.

Gap: a kill loses the last assistant turn whose tools are still running, together with that
batch's results; they reach the journal only at the next `on_completion_call`.

Every append is one `append_messages` call to the journal log (the existing helper: load, extend,
`save_async`). That is O(n) per append; acceptable for subagent transcripts, and `DirStore`'s
`save_async` appends only the new messages. Failures are warned once per run via `Approver::warn`, never
fatal, and never change a permission decision. Journal I/O must not block the kill path:
`KillHook` is unaffected.

### Reading the journal

`export.rs` loads `<id>.inflight` before `<id>`, in the same pre-folder loop (all loads happen
before `export_folder` creates the folder), so a run settling mid-export may appear twice but is
never lost. A failed journal load warns, as the `<id>` failure does, and renders the agent without
the journal rather than failing the export. It passes the journal into `render_agent_file`;
an empty journal means "none" and no marker is written. Otherwise its messages follow a marker
chosen from `AgentSummary.running`: "run in progress at export" when running, "run interrupted"
otherwise. "_No completed runs at export time._" is printed only when the journal is empty too. A fork child's
`<id>` is seeded with the parent's context before its first run, so its export shows the seeded
context, then the marker and the journal. `load_async` already tolerates a torn tail. A live
agent's journal is emptied at settle, except when the stream is dropped without settling (a
spent budget, the kill-grace timeout): the journal then survives for an agent in the same process
too and `/export` shows it the same way. The existing "completed runs only" note applies except
that the export may include the journal of the run in progress. The index note "running at export;
its file holds completed runs only" (`export.rs` ~378-380) and its test (~602) are updated to match.

Scope: subagents only (the issue).

## Implementation Steps

1. `fontaine-core/src/turn_recovery.rs`: add `InflightHook<S>` and a `journal_log(id) -> String`
   helper (`"{id}.inflight"`); unit tests below.
2. `fontaine-core/src/child_runner.rs`: register `InflightHook` after `RecoveryHook`.
3. `fontaine-core/src/export.rs`: pass the journal into `render_agent_file` and append it after the
   agent's log, with the marker picked from `AgentSummary.running`; the empty-journal case is
   unchanged; update the index note for a running agent and its test.
   Also update the stale doc comments in `export.rs`: the `running` field (~line 69, "holds
   completed runs only"), the index doc (~362-364), and "A log with no completed runs says so"
   (~393-394), which now also needs an empty journal.
4. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/turn_recovery.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine-core/src/export.rs`
- `rust/fontaine-core/tests/agent_pipeline.rs` (`mod killed`)
- `docs/agents.md`, `docs/tui.md` (export note), `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (Decisions, Agents)

## Decisions

- 2026-11-03: The in-flight journal is a separate log `<agent-id>.inflight`, not an append to
  `<agent-id>`, because rig's final save refuses a log that moved under it.
- 2026-11-03: SIGTERM-only flush declined: does not cover SIGKILL/OOM.
- 2026-11-03: journal from `on_completion_call`, not `on_model_turn_finished`/`on_outcome`: neither
  sees exactly the committed messages.

## Documentation

- `docs/agents.md`, "A subagent's conversation": replace "saved when a run ends" with the journal
  behaviour — an interrupted process leaves the in-flight run's messages, shown by `/export`; a kill
  loses the last assistant turn whose tools are still running, together with that batch's results.
- `docs/agents.md`, "Watching and stopping subagents" (~lines 353-358): a run dropped past the kill
  grace never settles, so its in-flight messages live in the journal, which `/export` shows.
- `docs/tui.md` export paragraph (line ~546): a running or killed agent's export includes its
  in-flight run.
- `CHANGELOG.md` under Unreleased → Fixed: subagent transcripts survive a killed process (#404).
- Add the Decisions line above to the master plan's Agents subsection.

## Testing Strategy

Unit tests in `turn_recovery.rs` against `MemoryStore`/`DirStore`, driven by calling the hook
methods with constructed events:

- journal holds prompt, assistant and tool-result messages in order after each `on_completion_call`;
- journal is truncated to empty on `Response` and on `Error` settle, and on error the messages are
  in `<id>`;
- on `Error` settle where the reloaded `<id>` does not hold input length + `messages.len()`
  messages, the journal is kept and a warning is raised;
- a stale journal is truncated at run start;
- a torn journal still loads (extends `a_torn_log_remains_readable`);
- a store failure warns once and the run continues.

Integration test in `rust/fontaine-core/tests/agent_pipeline.rs` `mod killed`, using
`HeldTransport`/`build_held`: the script is a shell tool turn then a text turn. Wait on `seen`
twice and release once, holding the second request. While the run is pending, assert
`load_async(store, "agent-1.inflight")` holds the prompt, the assistant tool-call turn and its tool
result (no kill, no `wait_report`), then call
`export_session(&built.runtime.session, built.runtime.state.agents(), path)` and assert those
appear after the in-progress marker. This pins the bug in #404 at the tier that reproduces it (a
run with no settle). Success case: release both requests, `wait_report`, assert the journal loads
empty and `agent-1.jsonl` holds the full run.
