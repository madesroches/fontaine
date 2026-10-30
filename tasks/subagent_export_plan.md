# Subagent Transcript Export and Persistence Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/300 (including the retention gap
it notes: "exporting their conversations may also require retaining or persisting those stores")

## Overview

Subagent conversations are today anonymous `MemoryStore`s that die with the session process, so
`/export` can only ever see the root conversation and delegated work is undiagnosable after the
fact. This plan changes that in two folds: **(1)** every subagent's conversation is persisted as a
per-agent log inside the session's owner-only store, and agent ids become stable across `--resume`
(a counter derived from the ledger — no new state file); **(2)** `/export`, when the session has
subagents, writes a folder — `main.md` with the root conversation plus a Subagents index, and one
`<agent-id>.md` per subagent with the same render detail — readable both live and after a resume.
The owner-only and never-overwrite guarantees extend to every file and to the folder itself.

## Current State

- `export_session` (`rust/fontaine-core/src/export.rs:51-59`) loads the root conversation with
  `load_async(&session.store(), CONVERSATION_LOG)` ("main", `session.rs:33`) and renders it via
  `render` (`export.rs:126`). `write_new` (`export.rs:61`) writes through
  `owner_only::create_new` (mode 0600 / user-only DACL, `owner_only.rs`), maps `AlreadyExists`
  to `ExportError::Exists`, and deletes a partially written file on write failure. The `/export`
  handler (`rust/fontaine-tui/src/ui/commands/session.rs:119-133`) is its only caller; the
  default path is `fontaine-<id8>.md` in the launch directory.
- Subagent conversations exist only in per-child `MemoryStore`s: `CoreChildRunner::spawn`
  (`rust/fontaine-core/src/child_runner.rs:347-362`) builds each child with
  `MemoryStore::default()` (or a seeded one for context forks, `fork_context.rs:57-70`) under
  log "main". rig-agent saves a store only when a run ends
  (`rig-agent-0.45.0/src/agent/engine.rs:425`); the in-flight run lives only in the run engine.
  `rig_memory::load_async` maps a missing log to an empty conversation
  (`rig-memory-0.45.0/src/session.rs:663`).
- `Session` (`session.rs:176-198`) owns the session `DirStore` (`<data dir>/sessions/<id>/`,
  `Session::store()` at `session.rs:441`), the append-only ledger, and small per-session files
  (`SessionTodos`, `SessionGoal`, `Scheduler`, all rooted at the session dir). `Session::fork`
  (`session.rs:308-337`) copies the whole session directory (`copy_tree`) and reopens it, so
  anything added to the dir or ledger automatically survives `/branch`.
- `SessionState` (`gate.rs:290`) holds `agents: Arc<AgentTree>` but no session store; the
  `AgentSetup` that builds the runtime carries the `Session` (`agent.rs:92-97`), and
  `agent/setup.rs` reaches it (`setup.rs:254-260`, `:386`), so plumbing the store into the child
  runner is a local change.
- The agent tree (`AgentTree`, `rust/fontaine-agents/src/tree.rs:137`) is one per process.
  `reserve` (`tree.rs:374-409`) mints ids as `agent-<n>` from `next: AtomicU64` seeded at 1 in
  `AgentTree::new` — so after `--resume` ids restart and would collide with any persisted naming.
  Nodes carry `NodeInfo { id, parent, agent_type, depth, ... }` (`tree.rs:86-94`) and a
  `ChildStatus` (`runner.rs:15-37`); `rows() -> Vec<AgentRow>` (`tree.rs:893`) is oldest first.
  Nodes of failed spawns are removed by `release` (`tree.rs:413`).
- The ledger persists the agent lifecycle: `Record::Agent { event, agent, parent, detail }`
  (`ledger.rs:114-121`) with `spawned` (detail = agent type), `finished` (detail = status
  string), `message` and `resumed` events, appended via `LedgerWindow`
  (`child_runner.rs:78-114`). The file is replayed into `LedgerView` at open — which today
  *ignores* `Record::Agent` (`ledger.rs:378`). After `--resume` the tree is empty but the ledger
  file still holds everything needed to reconstruct the agent list.
- Root `rewind`/`clear` rewrite only the "main" log via `replace_conversation`
  (`session.rs:613-650`); separate logs are untouched. The ledger is never rewound.
- Prior art: `owner_only::create_dir` creates a 0700 / inheriting-DACL directory with
  `AlreadyExists` on an existing entry (`owner_only.rs`). Master plan decisions at
  `tasks/fontaine_harness_plan.md:668` (export guarantees, #10); the id-instability note this
  plan supersedes is `tasks/completed/task_list_tool_plan.md:169` (agent ids are not stable
  across resume — this plan changes that, see Decisions).
  The subagent-view plan decided `/export` stays view-independent
  (`tasks/completed/subagent_view_plan.md` §5), which this feature keeps.
- Integration coverage: `export_writes_the_conversation_once_and_never_overwrites`
  (`rust/fontaine-tui/tests/sessions.rs:444-473`) drives the TUI with a scripted
  `MockCompletionModel` (`tests/common/mod.rs`); subagent sessions are scripted with wiremock
  routes per agent (`rust/fontaine-tui/tests/common/routed.rs`, `rust/fontaine-tui/tests/agents.rs`);
  core-level runtime tests live in `rust/fontaine-core/tests/agent_pipeline.rs` with a shared
  harness at `tests/common/pipeline.rs`.
- Docs: `/export` at `docs/tui.md:321-326` (§Sessions) and the command-table row at
  `docs/tui.md:344`; the session on-disk layout at `docs/architecture.md:176-212` and
  "Subagents in the ledger" at `docs/architecture.md:233-245`; subagent resume at
  `docs/agents.md:205-217`.

## Design

### Part 1 — persist subagent conversations

**One log per agent, in the session store.** `ChildContext`
(`child_runner.rs:132-144`) gains a `store: DirStore`. The retained `Setup` that
`child_runner()` receives carries no `Session`, so the store is cloned from
`setup.session.store()` at the `subagent_support` call site in `agent.rs:263` (where the full
`AgentSetup` is still available) and threaded through `subagent_support` → `child_runner` as a
new parameter. `CoreChildRunner::spawn` stops creating
`MemoryStore`s: the child's log name is its agent id (`agent-<n>` — a valid rig log name, so the
file lands at `<session>/logs/agent-<n>.jsonl` beside `main.jsonl`), and the builder gets
`.memory(store.clone()).conversation(request.id)` plus `RecoveryHook::new(store, request.id, …)`
(the hook's log field widens from `&'static str` to `String`). rig-agent then saves the child's
conversation to disk when each run ends, reloads it at the start of a resumed run
(`engine.rs:219-228`), and compaction (if a child compacts) rewrites the log through the same
`save_async` the root uses. `ParentContext::seed` (`fork_context.rs:57-70`) changes to save the
seeded inherited context into the child's log instead of a fresh `MemoryStore`.

**Stable ids from the ledger, no state file.** `LedgerView` (`ledger.rs:214-240`) starts folding
`Record::Agent` instead of ignoring it, keeping:

- `pub next_agent: u64` — one past the largest `agent-<n>` ordinal seen in `spawned` records
  (non-conforming ids ignored), so a resumed session continues where the previous process left
  off. Crash-safe by construction: an id reserved but never recorded (crash before the `spawned`
  append) had no child, so reusing its ordinal loses nothing.
- `pub agents: Vec<AgentRecord>` — one entry per agent, in spawn order:
  `{ id, parent: Option<String>, agent_type: String, ended: Option<String> }`, where `ended` is
  the `finished` record's status, or `None` if no `finished` record exists (crashed session).
  A `resumed` record for an id resets its entry's `ended` to `None` — a finished agent resumed
  for another run is unfinished again until a new `finished` record lands (so a session that
  crashes mid-second-run exports the agent as unfinished, not "done").

`AgentTree::new` takes the seed: `AgentTree::new(limits, next_agent: u64)`, and `AgentRuntime::new`
(`agent.rs:241-243`) passes the value from the opened session's ledger view. `/branch` needs no
counter handling at all: `Session::fork` copies the ledger, and the fork's counter derives from
the copy.

**What this deliberately does not change.** The live tree stays per-process and in-memory;
`--resume` does not rebuild it (live status, handles and resumability remain session-scoped).
The TUI's streamed agent views are unchanged — re-opening persisted logs so views survive
`--resume` is a later candidate; only the export reads them. The only new durable artefacts are
the per-agent logs, inside the already owner-only session directory. No store field in
`RunShared`.

### Part 2 — the export

`export_session(session: &Session, tree: &AgentTree, path: &Path)`:

**Agent list.** Merge two sources, deduplicated by id, live rows first: (a) the live tree's
`rows()` (for agents of this process: live status, parent, type); (b) `session.ledger.view().agents`
(for agents of earlier processes of this session: parent and type from the `spawned` record,
status from `ended`, or "ended" when there is no `finished` record). Both sources are already
ordered oldest-first; the merge preserves that.

**Reading transcripts.** Each agent's transcript is `load_async(&session.store(), &id)` — a
missing log yields an empty conversation, never an error. A running agent's log holds its
completed runs only, because rig saves at run end; the in-flight run is not in it (the same
"persisted log" view the root export has always given). A child killed before its first save, or
spawned but never run to completion, exports as an empty transcript with its status.

**Layout.** No subagents (neither source has any) → today's behavior, byte for byte: one file,
default `fontaine-<id8>.md`. Otherwise the target is a folder:

```
fontaine-<id8>/
├── main.md        # root conversation + "## Subagents" index
├── agent-1.md     # one per subagent, flat, named by agent id
└── agent-2.md
```

- `main.md` is today's render with an index section after the `Session `<id>`` header: one bullet
  per subagent, oldest first, flat — `- `agent-2` — explore, child of `agent-1`: done —
  [transcript](agent-2.md)`. A running row is annotated: "running at export; its file holds
  completed runs only". A row with no readable transcript (defensive; cannot happen for a
  ledger-recorded agent with a log present — a missing log reads as empty) is listed without a
  link, marked "transcript unavailable".
- Each `<agent-id>.md` opens with an identifying header — `# Subagent `agent-2` (explore)` /
  `Session `<id>`` / `Parent: `agent-1`` / `Status: done` — followed by the same message render
  as the root export (tool calls one line, tool results as lengths only, compaction summaries
  labelled). An empty conversation renders one italic line: "_No completed runs at export
  time._"

**Guarantees, extended to the folder.** The folder is created with `owner_only::create_dir`
(0700 / user-only inheriting DACL); `AlreadyExists` — a file or folder already at the target —
is `ExportError::Exists`, so an export never merges into an existing folder, matching the
single-file semantics. Every file inside goes through `write_new` (create_new, 0600,
delete-on-partial). All loads (root plus every agent) happen before the folder is created, so a
failed load cannot leave a half-written folder; a write failure after creation triggers a
best-effort `owner_only::remove_dir_all_retrying` of the folder (it can hold only files this
export just created), then the error.

**Path semantics.** Default with subagents: `fontaine-<id8>/` in the launch directory; an
explicit path names the folder (relative paths stay relative to the launch directory); exporting
again to the same target is an error, exactly as today.

**Rewind and `/clear`.** A root conversation rewind or clear rewrites only "main"; agent logs
are not touched, and the ledger (which records the spawns) is append-only. The export therefore
lists every agent recorded in the session ledger, including agents whose spawn turn was later
rewound away; their transcripts stand as per-agent evidence, the same way the ledger keeps the
fact of their work. This is documented, not worked around.

## Implementation Steps

1. **`rust/fontaine-core/src/ledger.rs`** — fold `Record::Agent` in `LedgerView::apply`: add
   `AgentRecord` and the `agents` / `next_agent` fields; parse the `agent-<n>` ordinal from
   `spawned` records (largest wins).
2. **`rust/fontaine-agents/src/tree.rs`** — `AgentTree::new(limits, next_agent: u64)`;
   `reserve` unchanged. Update the call sites: `agent.rs:243` (seed from the ledger view) and
   the mechanical rest (`fontaine-evals/src/none.rs:123`,
   `fontaine-core/tests/choke_point.rs:112`, `tests/common/pipeline.rs:236`,
   `src/gate.rs:943`, `src/steering.rs:268`, `src/questions.rs:89`, `child_runner.rs:567`,
   `tests/agent_pipeline.rs:371`, tree's own tests, `tests/seam.rs:218`) passing 1.
3. **`rust/fontaine-core/src/child_runner.rs`** — add `store: DirStore` to `ChildContext`;
   in `spawn`, drop the `MemoryStore`, use log name `request.id` for the builder memory and the
   `RecoveryHook` (widen its log field to `String` in `turn_recovery.rs`, so the root call site
   in `agent/build.rs:270` passes `CONVERSATION_LOG.to_owned()`); pass the store through from
   the `subagent_support` call site in `agent.rs` (`setup.session.store()` is available there —
   thread it through `subagent_support` → `child_runner`, `setup.rs:522-536`).
4. **`rust/fontaine-core/src/fork_context.rs`** — `ParentContext::seed` saves the seeded
   conversation into the child's log of the session store (caller passes store + log name).
5. **`rust/fontaine-core/src/export.rs`** — split `render` into the existing wrapper plus a
   reusable body; add `index_section` and `render_agent_file` as pure functions; change
   `export_session` to take the tree, merge the two agent sources, and implement the folder
   flow (load all → `create_dir` → `write_new` per file → best-effort cleanup on failure).
6. **`rust/fontaine-tui/src/ui/commands/session.rs`** — pass `runtime.state.agents()`; pick the
   default path by layout; notice text ("exported the conversation to X" unchanged, or
   "exported the session and N subagent transcripts to X").
7. **`rust/fontaine-tui/src/commands.rs`** — update the `/export` help line.
8. **Tests** (see Testing Strategy).
9. **Docs** — `CHANGELOG.md` (`Added` entry under `[Unreleased]` for persisted subagent
   transcripts and the folder export), `docs/tui.md` (§Sessions paragraph + command table),
   `docs/architecture.md` (session layout and "Subagents in the ledger": per-agent logs now
   exist and persist across resume), `docs/agents.md` (a subagent's conversation now survives
   on disk for the session's life).
10. **`tasks/fontaine_harness_plan.md`** — dated Decisions lines at the end of `### Sessions`:
    per-agent persisted logs and ledger-derived stable ids (noting it supersedes the id-
    instability note at `tasks/completed/task_list_tool_plan.md:169`), the folder export
    layout, completed-runs-only
    semantics for running subagents, and rewind/clear leaving agent logs untouched.

## Files to Modify

- `rust/fontaine-core/src/ledger.rs`
- `rust/fontaine-agents/src/tree.rs` (+ its tests)
- `rust/fontaine-core/src/agent.rs`, `src/agent/setup.rs`, `src/agent/build.rs`
- `rust/fontaine-core/src/child_runner.rs`, `src/turn_recovery.rs`, `src/fork_context.rs`
- `rust/fontaine-core/src/export.rs`
- `rust/fontaine-tui/src/ui/commands/session.rs`, `src/commands.rs`
- `rust/fontaine-evals/src/none.rs` and test harnesses (mechanical `AgentTree::new` call sites)
- `rust/fontaine-tui/tests/agents.rs` (or `sessions.rs`) — integration coverage
- `CHANGELOG.md`
- `docs/tui.md`, `docs/architecture.md`, `docs/agents.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Durable vs. ephemeral subagent traces** — persistence fixes the issue's actual motivation
  (diagnosing a failed subagent after the fact; a crash or a forgotten `/export` no longer loses
  the evidence) and makes the ledger's lifecycle records point at something real. The cost:
  subagent work now leaves a durable trace in the session directory (owner-only, like the root
  transcript, and fork seeds duplicate context the root transcript already holds), and sessions
  with many agents accumulate one log file each. Accepted as the intended behaviour change.
- **Ledger-derived counter vs. a counter file vs. UUIDs** — compared at authoring time: a
  persisted `agents.json` adds a state file and a durability ordering constraint; UUIDs need no
  state at all but degrade every place an id is read or typed (agents panel, notifications,
  `send_message`/`job_output` addressing, export headings), and the tree still wants an ordinal
  for display. Deriving the next ordinal from the ledger's `spawned` records has no new state,
  no crash window (an unrecorded reserved id never had a child), keeps `agent-<n>` ergonomics,
  and makes `/branch` correct for free (the fork inherits the ledger). Chosen; UUIDs remain the
  fallback if the id format ever stops being ordinal.
- **One log per agent vs. one combined "agents" log** — rig's store model is one conversation
  per log with per-log length-checked writes, so separate logs give each agent independent
  save/compaction semantics and isolate writers; a combined log would need a muxing layer that
  does not exist.
- **Fold vs. ship export-time reads first** — an export that reads in-memory stores would meet
  #300's acceptance criteria but ship an interim behaviour (transcripts vanish when the process
  ends) that persistence then changes again, along with docs churn. Folding costs one extra
  design question (rewind semantics) and lands one durable feature; the cross-crate seam the
  interim version needed disappears entirely.
- **Flat index vs. indented tree** — indentation by depth reads nicely, but the flat "child of"
  list already encodes the tree unambiguously and is what the ledger stores.

## Decisions

- 2026-11-21 (authoring, user call): persistence is folded into this plan — subagent
  conversations persist as per-agent logs in the session store — instead of shipping export-time
  reads first and persisting later.
- Agent ids stay `agent-<n>` and become stable across `--resume` via a counter derived from the
  ledger's `spawned` records; no `agents.json`, no UUIDs (user proposed UUIDs; the counter was
  chosen for display/addressing ergonomics with the same guarantee).
- The per-agent log's name is the agent id; logs live in the session store's `logs/` beside
  `main.jsonl`.
- Root rewind and `/clear` do not touch agent logs; the export lists every agent recorded in
  the ledger, and a transcript stands as per-agent evidence of work the root conversation no
  longer shows.
- A running subagent is exported rather than refused; its file holds completed runs only (rig
  saves at run end), and the index row says so.
- The export is a folder (`fontaine-<id8>/` with `main.md` and one `<agent-id>.md` per
  subagent) whenever any subagent is recorded; a single file exactly as today otherwise. Zip and
  single-file layouts were offered and rejected.
- `/export` remains view-independent (running it from a subagent view exports the whole
  session), continuing the subagent-view plan's routing decision.
- The index lists id, parent, agent type and status per subagent; `background` and `depth` are
  omitted.
- Agent logs accumulate for the session's life and are pruned only when the session is deleted;
  extending `/clear` epoch semantics to them is a follow-up if session dirs grow uncomfortably.
  Accepted for v1.

## Documentation

- `docs/tui.md` §Sessions (`:321-326`): rewrite the `/export` paragraph — folder layout when
  subagents exist (live or from before a resume), the index and per-agent files, the
  completed-runs-only note for running subagents, and the unchanged owner-only /
  never-overwrite guarantees. Update the command-table row at `:344`.
- `docs/architecture.md`: the session layout (`:176-212`) gains the per-agent `logs/agent-<n>.jsonl`
  files, and "Subagents in the ledger" (`:233-245`) notes the ledger now both records the
  lifecycle and anchors id continuity across resume.
- `docs/agents.md` (`:205-217`): a subagent's conversation is persisted for the session's life
  and is what `/export` renders; resume still starts a fresh run on it.
- `tasks/fontaine_harness_plan.md` Decisions, `### Sessions`: lines per Implementation Steps.

## Testing Strategy

Unit and core integration (all deterministic, no live APIs):

- `ledger.rs`: `LedgerView` folds `spawned`/`finished` into `agents` (order, parent, type, ended
  status, missing-finished case) and derives `next_agent` (largest ordinal; non-conforming ids
  ignored; empty ledger → 1); a `finished` → `resumed` → crash sequence leaves `ended` as
  `None`, not the stale finished status.
- `tree.rs`: two trees seeded from the same ledger-derived value mint disjoint ids (restart
  simulation); a tree seeded after N spawns starts at `agent-<N+1>`.
- `fork_context.rs` / `child_runner.rs`: a forked child's seeded conversation lands in the
  child's log of the session store; `RecoveryHook` appends an interrupted turn there.
- `export.rs`: the pure renderers — `index_section` (ids, parents, types, statuses, links,
  running annotation) and `render_agent_file` (header fields, empty-conversation note) — and
  `render` byte-identical for the root conversation.
- `export.rs` against a temp data dir: happy-path folder (files, modes 0700/0600 on Unix);
  pre-existing folder or file at the target is `Exists` with nothing written; write failure
  removes the created folder; a session whose ledger records agents but whose tree is empty
  exports those agents with their recorded statuses (the post-`--resume` shape, built with
  `Session::create` + hand-appended ledger records + `save_async` into the store — no provider
  involved); no-subagent sessions keep the single-file path, pinning the existing tests green.
- `agent_pipeline.rs` (existing harness, scripted models): after a child completes, its log in
  the session store holds its conversation; ids do not collide across a simulated restart that
  reopens the session and seeds a new tree from its ledger.

Integration (TUI harness, wiremock-routed scripted provider — no live API):

- `rust/fontaine-tui/tests/agents.rs` (routed harness): a root turn spawns a worker that
  completes; after its report lands, `/export` produces the folder — `main.md` carries the root
  conversation and a Subagents index naming `agent-1`, its parent and `done` status with a link;
  `agent-1.md` carries the identifying header and the child's scripted messages; a second
  `/export` to the same default path errors with "already exists"; Unix modes are 0700/0600.
  This is the wiring no unit test reaches: spawn → persisted log → tree/ledger → export across
  the real runtime path.
- `sessions.rs:444` (existing single-file test) keeps passing unchanged.

## Manual Verification

None. The rendered Markdown shape is fully covered by the unit tests above and the integration
test reads the files back; nothing here needs a human eyeball that a test cannot assert.

