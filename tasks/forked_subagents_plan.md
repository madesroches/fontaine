# Forked Subagents That Inherit the Parent Context Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/39

## Overview

Claude Code can fork the current conversation into a subagent that starts with the parent's full
context instead of a prompt-only briefing. Fontaine gets the same through the existing `agent`
tool: a built-in `fork` agent type whose child starts from a copy of the calling agent's
conversation (rig-memory), then receives the `prompt` as its next user turn. Everything else about
a child is unchanged: same pipeline, mode-never-looser rule, budgets, depth and parallel limits,
agents panel. The guardian ledger is **not** copied (see Decisions).

## Current State

- `agent` tool: `rust/fontaine-agents/src/spawn.rs` (`AgentTool::call`) resolves an `AgentType`,
  cuts the toolset to the parent's, reserves a slot in `AgentTree`, and hands a `SpawnRequest`
  (`runner.rs`) to a `ChildRunner`.
- Core runner: `rust/fontaine-core/src/child_runner.rs` `CoreChildRunner::spawn` builds each child
  with `MemoryStore::default()` (empty in-memory store) and `.conversation("main")`, so a child
  sees only its preamble and `prompt`. The tool description says so ("it cannot see this
  conversation").
- The root conversation is stored through `Session::store()` (`fontaine-core/src/session.rs:441`)
  under `CONVERSATION_LOG`; `load_async` / `save_async` / `Conversation::set_entries` are already
  used by `/clear` and `/rewind` (`session.rs:637`). Children's contexts are not persisted.
- Guardian intent: `LedgerWindow::human_messages` (`child_runner.rs`) is the root ledger for every
  agent; spawn prompts and `send_message` bodies reach the guardian only as untrusted blocks
  (`docs/agents.md`, "Every call is gated").
- Built-in types `explore` / `worker`: `fontaine-agents/src/registry.rs`. Documentation:
  `docs/agents.md`.

## Design

### The `fork` agent type

A third built-in type, `fork`: description "Starts from a copy of your conversation, so it needs
only a short task in `prompt`", `tools: None` (every tool the parent has, still cut to the
parent's and with `agent` withheld at the depth limit), `mode: Inherit`, `model: None`.
`AgentType` gains `inherits_context: bool` (true only for `fork`; agent files and
`[agents.types]` entries cannot set it, so it stays a built-in capability). `SpawnRequest` already
carries `agent_type: AgentType`, so the runner reads `request.agent_type.inherits_context`; no new
request field. A user agent file, project file or `[agents.types]` entry named `fork` replaces the
built-in (later of the four places wins) and loses the capability (`inherits_context` false).

Rules enforced in `AgentTool::call`, before reserving a slot:

- Root-only: refuse when `caller != ROOT_AGENT` (`fontaine_tools::gate`), with
  `ToolExecutionError::refused`, before `reserve`.
  Message: "only the main conversation can fork itself; start a typed subagent and put what it
  needs in `prompt`" (a child's context is an unpersisted in-memory store).
- `model` is refused for `fork` (invalid-args error "a fork runs on the session model; leave
  `model` out": the session model produced the context it inherits). `explore`/`worker` behavior
  is unchanged.

### Seeding the child's context (core)

`CoreChildRunner::spawn`, when `request.agent_type.inherits_context`:

1. Load the root conversation: `ChildContext` gains
   `parent_conversation: Arc<dyn Fn() -> BoxFuture<'static, Result<Conversation, SessionError>> + Send + Sync>`,
   an async fallible closure over `load_async(store, CONVERSATION_LOG)` returning the whole
   `Conversation`, so tests can supply a fixed one; the typed error keeps its `source()` and is
   rendered once with `fontaine_tools::error_chain::chain` at the `ChildRunner::spawn` seam as
   `could not start: <chain>`. `child_runner` has no `Session` today, so `agent/setup.rs` passes
   it the session store (`p.session.store()`, a cheap `DirStore` clone, as `rust/fontaine-core/src/agent/build.rs:246` does)
   as a new parameter, and the closure captures that clone.
2. Drop a trailing assistant message whose tool calls have no results yet: at spawn time the
   parent's in-flight `agent` call is exactly that. Whether rig-memory has saved it by then is
   verified first (Implementation Steps, step 1); the trim is a no-op if not.
3. Build a fresh `MemoryStore::default()`, put only the trimmed loaded `Conversation` (no prompt)
   into it with `save_async` under `"main"`, then proceed exactly as for any child. The fork's own
   first user turn is the run prompt passed through `drive` (`run_child` -> `drive`, which appends
   it via `agent.prompt`), prefixed with a one-line harness note naming it as a fork task
   ("You are a forked subagent of the conversation above; do only the task below and report.");
   it is never also stored, or the child would see it twice. `set_entries`
   does not carry compaction asides (`session.rs:650-670` re-adds them with `add_aside`), so the
   loaded `Conversation` is cloned wholesale, keeping a compacted parent compacted in the child.

The child's preamble (the `preamble` closure at `rust/fontaine-core/src/agent/setup.rs:493`,
which chooses the fork layer from `agent_type.inherits_context`) is `subagent_layer("fork", None)` on the standard assembly, with a distinct
`fork` layer text in `prompt.rs` ("you are a forked subagent; the conversation above is the
parent's") in place of the typed-subagent line "You cannot see that agent's conversation; work from
the task you were given"; a fork does not reuse the parent's exact system prompt (see Decisions).

### Security and budget invariants

- **Intent**: unchanged; the root ledger is the sole source. The inherited conversation includes
  tool results and model text, which are already untrusted data to the guardian; nothing in the
  copied context is promoted to intent.
- **Mode**: `fork` is `Inherit`, so the existing tighter-of-own-and-parent rule applies per call;
  the type cannot be configured looser.
- **Fail closed**: if the parent conversation cannot be read, the spawn fails
  (`could not start: <chain>`), never falling back to an empty-context fork that would silently
  lack what the prompt assumes.
- No new tool, so the risk-class choke-point test is unaffected; `agent` stays `Arbitrary`, and a
  rule such as `agent(fork)` in `deny`/`ask` works through the existing specifier matching.

### Surfaces

- `agent` tool `prompt` parameter: "The task, with all the context it needs." becomes per-type
  too (a fork needs only a short task).
- `agent` tool description: replace "it cannot see this conversation" with the per-type rule: typed
  subagents cannot see the conversation; `fork` can and needs only a short prompt. The types list
  already comes from `AgentTypes::describe()`.
- Agents panel and telemetry: `agent.type = "fork"` appears with no further change.

## Implementation Steps

1. Probe (test in `fontaine-core`): drive a turn whose model calls `agent`, and read the stored
   conversation from inside the runner to confirm whether the in-flight assistant tool call is
   already persisted. Record the answer as a Decisions line.
2. `fontaine-agents/src/registry.rs`: add `inherits_context` to `AgentType` (default false in all
   constructors/loaders), fixing the sites the new field breaks: `fontaine-core/src/extensions.rs:327`
   (`load_agent`), `registry.rs:148,162`, `tests/seam.rs:296,315,364,525`,
   `fontaine-core/src/child_runner.rs:455`, `agent/setup.rs:186`; add the built-in `fork` type,
   include it in `describe()`/`names()`.
3. `fontaine-agents/src/spawn.rs`: the
   `model` refusal and the root-only refusal; updated tool description and `prompt` parameter
   description (`spawn.rs` ~156, "The task, with all the context it needs.") saying a fork needs
   only a short task.
4. `fontaine-core/src/child_runner.rs`: `parent_conversation` closure field on `ChildContext`, seeding
   in `spawn` (load, trim dangling call, copy only the trimmed conversation into the child's store,
   and pass the fork-note-prefixed prompt as the run prompt through `drive`, not into the store); wire the
   store-backed closure in `agent/setup.rs::child_runner`; add the distinct `fork` layer text
   in `fontaine-core/src/prompt.rs`.
5. `docs/agents.md`: update "Two types are built in" (line 31) to three; note that a user, project or
   `[agents.types]` entry named `fork` replaces the built-in and loses the capability (four places,
   later replace earlier); reword the `prompt` row of the agent tool table (line 13, "it cannot see the
   conversation") per type, new `fork` row in Agent types, a "Forking the conversation" subsection
   (context copied, same model, main-conversation only, rule `agent(fork)`, a sentence that this is
   not the `/fork` slash command (an alias of `/branch`, `docs/tui.md`), and the note that a
   large inherited context is billed on the first call and can exhaust the child's default budget,
   ending `BudgetExhausted`). `CHANGELOG.md` Added entry (#39).
6. Master plan `## Decisions`: dated lines for the deviations below.

## Files to Modify

- `rust/fontaine-agents/src/registry.rs`, `spawn.rs`
- `rust/fontaine-agents/tests/seam.rs`
- `rust/fontaine-core/src/child_runner.rs`, `rust/fontaine-core/src/agent/setup.rs`,
  `rust/fontaine-core/src/prompt.rs`, `rust/fontaine-core/src/extensions.rs`
- `docs/agents.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Decisions

- 2026-10-27: Deviation from the issue text — the guardian ledger is shared, not cloned, for a fork: the tree's one intent source is the root ledger, and a copy would create a second one.
- 2026-10-27: Only the main conversation can fork; a fork always runs on the session model (no `model` override). Children's contexts are in-memory only, so forking from a child would need a second context-reading path for little value.
- 2026-10-27: A fork uses the child preamble with a distinct `fork` layer, not the parent's exact system prompt; the parent's assembled prompt is not retained by `ChildContext` and carries root-only sections (memory, scheduling, skill index) a child must not get. Prompt-cache sharing is not measured.
- 2026-10-27: Forking is an agent type, not a `fork: true` argument: a type reuses toolset, mode, model, listing, `agent(fork)` rule matching and telemetry unchanged.

## Documentation

`docs/agents.md` (type table, new subsection, the "cannot see the conversation" wording),
`CHANGELOG.md`.

## Testing Strategy

All offline, no live APIs.

- `fontaine-agents` unit tests (fake runner, `tests/seam.rs`): the flag-propagation test (request
  reaches the runner with `agent_type.inherits_context` true, false for `explore`) also asserts `fork`
  resolves with all parent tools and mode Inherit; extend the `seam.rs:362` test to refuse `model`
  on `fork`; a call from a child is refused.
- `fontaine-core` unit tests with `rig_core::test_utils` deterministic models: the child's first
  completion request contains the parent's prior messages followed by the prefixed prompt; a
  dangling in-flight assistant tool call is trimmed; a compacted parent's asides (summary) are
  present in the child's store; an unreadable parent conversation fails the spawn with the cause
  chained.
