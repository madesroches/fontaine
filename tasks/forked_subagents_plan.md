# Forked Subagents That Inherit the Parent Context Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/39

## Overview

Claude Code can fork the current conversation into a subagent that starts with the parent's full
context instead of a prompt-only briefing. Fontaine gets the same through the existing `agent`
tool: a built-in `fork` agent type whose child starts from a copy of the calling agent's
conversation (rig-memory), then receives the `prompt` as its next user turn. Everything else about
a child is unchanged: same pipeline, mode-never-looser rule, budgets, depth and parallel limits,
agents panel. The guardian ledger is **not** copied: every agent in a tree already reads one
intent source, the root session's ledger, so a fork inherits the same picture of what the user
asked by construction.

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
`[agents.types]` entries cannot set it, so it stays a built-in capability). `SpawnRequest` carries
the same flag so the `ChildRunner` seam stays free of registry lookups.

Rules enforced in `AgentTool::call`, before reserving a slot:

- `model` is refused for `fork` (invalid-args error: a fork runs on the session model, the one that
  produced the context it inherits). `explore`/`worker` behavior is unchanged.
- Only the main conversation can fork: the root's context is the stored conversation; a child's is
  an unpersisted in-memory store. A call from a child to `fork` returns a refused error ("only the
  main conversation can fork itself; start a typed subagent and put what it needs in `prompt`").

### Seeding the child's context (core)

`CoreChildRunner::spawn`, when `request.inherits_context`:

1. `load_async(session.store(), CONVERSATION_LOG)` the root conversation (needs a `Session` handle
   or a `Fn() -> DirStore` in `ChildContext`; `ChildContext` gains
   `parent_conversation: Arc<dyn ParentConversation>`, a small trait in `child_runner.rs`
   implemented over `Session`, so tests can supply a fixed history).
2. Drop a trailing assistant message whose tool calls have no results yet: at spawn time the
   parent's in-flight `agent` call is exactly that. Whether rig-memory has saved it by then is
   verified first (step 1 below); the trim is a no-op if not. The fork's own first user turn is
   the `prompt`, prefixed with a one-line harness note naming it as a fork task
   ("You are a forked subagent of the conversation above; do only the task below and report.").
3. Build a fresh `MemoryStore::default()` and write the entries into it with `set_entries` +
   `save_async` under `"main"`, then proceed exactly as for any child. Compaction asides are
   copied with the entries so a compacted parent stays compacted in the child.

The child's preamble is the existing `subagent_layer("fork", None)` on the standard assembly; a
fork does not reuse the parent's exact system prompt (see Trade-offs).

### Security and budget invariants

- **Intent**: unchanged; the root ledger is the sole source. The inherited conversation includes
  tool results and model text, which are already untrusted data to the guardian; nothing in the
  copied context is promoted to intent.
- **Mode**: `fork` is `Inherit`, so the existing tighter-of-own-and-parent rule applies per call;
  the type cannot be configured looser.
- **Budget**: the child's first model call is billed at the full inherited context size. That
  spend counts against the child's `subagent_token_budget` / cost budget like any other call, so
  a large parent context can exhaust a fork's default 200k-token budget on its first turn. The
  report is then `BudgetExhausted` with a clear reason; no special-casing.
- **Fail closed**: if the parent conversation cannot be read, the spawn fails
  (`could not start: <chain>`), never falling back to an empty-context fork that would silently
  lack what the prompt assumes.
- No new tool, so the risk-class choke-point test is unaffected; `agent` stays `Arbitrary`, and a
  rule such as `agent(fork)` in `deny`/`ask` works through the existing specifier matching
  (verified by a test).

### Surfaces

- `agent` tool description: replace "it cannot see this conversation" with the per-type rule: typed
  subagents cannot see the conversation; `fork` can and needs only a short prompt. The types list
  already comes from `AgentTypes::describe()`.
- Agents panel and telemetry: `agent.type = "fork"` appears with no further change.

## Implementation Steps

1. Probe (test in `fontaine-core`): drive a turn whose model calls `agent`, and read the stored
   conversation from inside the runner to confirm whether the in-flight assistant tool call is
   already persisted. Record the answer as a Decisions line.
2. `fontaine-agents/src/registry.rs`: add `inherits_context` to `AgentType` (default false in all
   constructors/loaders), add the built-in `fork` type, include it in `describe()`/`names()`.
3. `fontaine-agents/src/runner.rs` + `spawn.rs`: add `inherits_context` to `SpawnRequest`; the
   `model` refusal and the root-only refusal; updated tool description.
4. `fontaine-core/src/child_runner.rs`: `ParentConversation` trait, `ChildContext` field, seeding
   in `spawn` (load, trim dangling call, copy into the child's store, prefix the prompt); wire the
   `Session` implementation in `agent/setup.rs::child_runner`.
5. `docs/agents.md`: new `fork` row in Agent types, a "Forking the conversation" subsection
   (context copied, same model, budget note, main-conversation only, rule `agent(fork)`);
   `docs/tools.md` if it mirrors the tool description. `CHANGELOG.md` Added entry (#39).
6. Master plan `## Decisions`: dated lines for the deviations below.

## Files to Modify

- `rust/fontaine-agents/src/registry.rs`, `runner.rs`, `spawn.rs`
- `rust/fontaine-agents/tests/seam.rs` (FakeRunner gains the new field)
- `rust/fontaine-core/src/child_runner.rs`, `rust/fontaine-core/src/agent/setup.rs`
- `rust/fontaine-core/tests/choke_point.rs` (NoRunner, if the trait changed)
- `docs/agents.md`, `docs/tools.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **`fork` type vs a `fork: true` argument**: a type reuses toolset, mode, model, listing, rule
  matching (`agent(fork)`) and telemetry unchanged, and keeps `agent_type` required. An argument
  would need its own interaction rules with every type.
- **Same system prompt as the parent (Claude Code, for prompt-cache sharing) vs the child preamble**:
  the parent's assembled prompt is not retained by `ChildContext` and carries root-only sections
  (memory, scheduling, skill index) a child must not get. Using the child preamble loses some
  cache reuse on the first call; correctness and the existing child-prompt boundary win.
- **Copy the guardian ledger (issue text) vs share the root ledger**: copying would create a second
  intent source and break the single-source invariant; sharing gives the same effect.
- **Fork from children too**: children's contexts are in-memory only; supporting it means a second
  context-reading path for little value. Root-only now.

## Decisions

- 2026-10-27: Deviation from the issue text — the guardian ledger is shared, not cloned, for a fork: the tree's one intent source is the root ledger.
- 2026-10-27: Only the main conversation can fork; a fork always runs on the session model (no `model` override).

## Documentation

`docs/agents.md` (type table, new subsection, the "cannot see the conversation" wording),
`docs/tools.md` if it repeats the `agent` description, `CHANGELOG.md`.

## Testing Strategy

All offline, no live APIs.

- `fontaine-agents` unit tests (fake runner, `tests/seam.rs`): `fork` resolves with all parent tools
  and mode inherit; `model` on `fork` is refused; a call from a child is refused; the request
  reaches the runner with `inherits_context` true and false for `explore`; the `agent(fork)` deny
  rule blocks it.
- `fontaine-core` unit tests with `rig_core::test_utils` deterministic models: the child's first
  completion request contains the parent's prior messages followed by the prefixed prompt; a
  dangling in-flight assistant tool call is trimmed; a compacted parent's summary carries over; the
  parent's stored conversation is unchanged after the child runs (the child writes only its own
  store); an unreadable parent conversation fails the spawn with the cause chained.
- A mode test: a fork spawned under `acceptEdits` then a parent cycle to `manual` asks on the
  fork's next call (reuses the existing mode-follow test harness).
- Budget test: a fork whose inherited context exceeds the token budget ends `BudgetExhausted`.

## Open Questions

- Should the fork keep the parent's system prompt exactly to share the provider prompt cache (as
  Claude Code does)? Resolving it requires measuring cache hit rates on a real provider, a human
  step; the plan ships the child preamble.
