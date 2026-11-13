# Managed Memory Tool Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/329

## Overview

Add a first-class `memory` tool that saves a note to the per-project auto-memory store in one
structured call: the tool writes (or replaces) the topic file and maintains the `MEMORY.md` index
line deterministically. The call stays behind the deterministic floor and the guardian in every
mode — the tool creates no fast path. This removes the mechanical index bookkeeping the agent
today does with raw `write`/`edit` calls, which is where the fuzzy guardian soft-denies of #312
concentrate, without weakening the persistence-vector gating #8 established.

Deliberately out of scope (settled in the issue's scope decisions): no cheap-model curator, no
read/list subcommands.

## Current State

- The store lives in `rust/fontaine-core/src/memory.rs`: `dir`/`ensure_dir` locate and create
  `<data dir>/memory/<project key>`, `load` reads the capped index, `layer` renders the prompt
  framing, `files`/`check` back `/memory` and `fontaine doctor`. There is no note-saving
  function — the agent writes topic files and edits `MEMORY.md` with the ordinary file tools.
- Session setup adds the store as a scope root and to session state
  (`rust/fontaine/src/lib.rs:830-845` → `SessionPrep.memory_root` →
  `SessionState.memory_root`, `rust/fontaine-core/src/gate.rs:416`).
- Gating today (`rust/fontaine-core/src/permissions.rs`):
  - The protected-path floor matches only `write`/`edit`/`notebook_edit` (1047); in `auto` a
    protected write routes to the guardian (1045-1064).
  - Plan mode blocks the `WorkspaceWrite` risk class, not names (957-966:
    `edit_class = call.risk == RiskClass::WorkspaceWrite`).
  - The `unjudged_edit` fast path is name-gated on the three file tools (1368), and the memory
    root is explicitly excluded from `tracked_edit` (1389-1393).
  - Everything not otherwise allowed in `auto` falls through to `Verdict::Guardian`
    (1326-1332); in asking modes it prompts; with no human it denies (1421-1470).
- The guardian learns the store via the harness-set `SessionFacts.memory_dir`
  (`rust/fontaine-core/src/adjudication.rs:184-253`, 303-310;
  `rust/fontaine-guardian/src/request.rs:95-97`) and it is deliberately not listed as a
  workspace root in the facts.
- Tool registration: the descriptor table is `rust/fontaine-tools/src/descriptor.rs:101-362`
  (31 rows, pinned by `rows_are_pinned` at 534-647). Extension tools needing session state are
  built in `fontaine-core/src/agent/build.rs::extension_tools` (~line 330) and wrapped by
  `ExtensionTool::new` (`rust/fontaine-core/src/tools.rs:39-48,128-137`), which assigns the
  descriptor row's risk class. Conditional offerings (drafts, scheduling, lsp) set the pattern
  for a tool that exists only when its prerequisite does.
- The saving guide in the prompt layer tells the agent to use `write`/`edit`
  (`memory.rs`, `SAVING_GUIDE`).

## Design

### The descriptor row

One new row in `TOOLS`, between `read_mcp_resource` and the closing bracket:

```rust
// Writes the session's persistent memory store, outside any git checkout; the
// WorkspaceWrite class keeps it out of every fast path, and auto routes it to the guardian.
row("memory", RiskClass::WorkspaceWrite, true, Destructive::Always, Specifier::None, &[]),
```

- `RiskClass::WorkspaceWrite` is the load-bearing choice. The memory root is a scope root, so
  the class is accurate; and because the class — not a name — drives the plan-mode block
  (permissions.rs:957-966), while the fast paths are name-gated (`unjudged_edit` at 1368, the
  floor match at 1047), `WorkspaceWrite` gives plan blocking and fast-path exclusion with no
  pipeline name lists touched. In `auto` the call reaches the mode fall-through and routes to
  the guardian; in asking modes it prompts; headless it denies.
- `path_free: true` — the arguments name no workspace path; the target is derived from
  harness-held session state (`memory_root`), never from the model's text.
- `Destructive::Always` — the call persists content into future sessions' prompts; like
  `cron_create` and `board_update`, `Never` would contradict the risk class in
  `path_policy_is_consistent`. `FileWrite` would force checkpointing (the pinned
  `every_file_writing_tool_is_checkpointed` list) the memory store does not need.
- `Specifier::None` — permission rules can name the tool whole (`allow memory` / `deny memory`)
  but take no argument specifier.
- `claude: &[]` — Fontaine-only, a deliberate deviation from Claude Code (see Decisions). The
  `FONTAINE_ONLY` list in the descriptor tests gains `"memory"`.

No other pipeline change: the name is deliberately **not** added to the protected-path floor
match or `is_external_write` — the mode fall-through already routes the call exactly where the
floor would (guardian in `auto`, prompt otherwise), so the extra name would only duplicate.

### Store logic: `memory::save`

New function in `rust/fontaine-core/src/memory.rs`:

```rust
pub struct Saved {
    pub file: String,      // topic file name within the store
    pub index: LineState,  // Added | Updated(line) | Unchanged
    pub warning: Option<String>, // e.g. the index cap warning
}

pub fn save(dir: &Path, topic: &str, content: &str) -> io::Result<Saved>
```

1. **Topic identity is its normalized slug**: lowercase, runs of non-alphanumerics become `-`,
   truncated (reuse the `SLUG_CHARS` convention), suffixed `.md`. `Foo Bar` and `foo-bar` are
   the same topic — one file, one index line. Slugs of `.`/`..`/empty are refused.
2. **Write the topic file atomically**: content to `<slug>.md.tmp` in the store, then rename
   over the target. A target that exists and is a link is refused (`symlink_metadata`, same
   refusal posture as `load`/`check`); `owner_only` creation applies to new files.
3. **Maintain the index deterministically**: read `MEMORY.md` (refuse links/non-UTF-8 as
   `load` does; a missing index starts empty). Find the line whose markdown link target is the
   file name — replace that line's link text with the topic verbatim, keeping its position;
   otherwise append `- [<topic>](<file>)`. Write the index back atomically. The model never
   supplies index text, so the "index line traces to tool results" soft-deny class has no
   surface left.
4. **Caps are a warning, not a failure** (parity with `load`'s cut-and-warn): after the write,
   if the index exceeds `MAX_INDEX_LINES`/`MAX_INDEX_BYTES`, the save still succeeds and
   `Saved.warning` tells the model the index is over the cap and detail should move into topic
   files (doctor's `IndexTruncated` already flags it for the user).
5. Concurrent writers (two sessions sharing a store) are last-writer-wins on the index, the
   same posture as today's raw edits; the atomic renames keep individual files whole.

### The tool

`MemoryTool` in a new `rust/fontaine-core/src/memory_tool.rs`, registered as an extension tool
in `agent/build.rs::extension_tools`, offered only when `p.state.memory_root` is `Some` (the
`drafts_offered`/`scheduling` conditional pattern), constructed with the store's path:

```rust
pub struct MemoryTool {
    pub root: PathBuf, // the session's memory store, from SessionState.memory_root
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryArgs {
    pub topic: String,   // ≤ 64 chars, non-blank
    pub content: String, // the note, markdown, ≤ 100_000 bytes
}
```

- `Tool` impl with `Output = String`, `Error = ToolExecutionError`; validation failures map to
  `invalid_args`, I/O failures to `other(chain(&e))`, following `todo.rs`.
- `footprint` reports the topic file and `MEMORY.md` under the memory root, and
  `touched_paths` (`rust/fontaine-core/src/destructive.rs:275-315`) takes a new
  `memory_dir: Option<&Path>` parameter and gains a `memory` arm that resolves the topic file
  (`<root>/<slug>.md`) and `MEMORY.md` against the memory root, so the guardian request lists
  both files with their existence status — the guardian sees exactly what persists. The
  parameter is in hand at every call site: `adjudication.rs::facts()` receives `memory_dir`
  already (208), and the `permissions.rs` call sites pass `self.state.memory_root`.
- The description states what the tool is for (durable notes a later session would otherwise
  rediscover), the caps, that the call is guardian-gated, and that raw `write`/`edit` remain
  for repairing the store. Reads need no subcommand: the store is a scope root, so `read`,
  `ls` and `glob` already reach it at read-only fast-path cost.
- No `require_root` — the tool is offered to subagents too; every call a child makes is gated
  individually through the shared pipeline, so a child is never looser than its parent. The
  memory prompt layer stays root-only, unchanged.

### Prompt layer

`SAVING_GUIDE` in `memory.rs` changes from "Write memory with the `write` and `edit` tools" to
"save notes with the `memory` tool; use `write`/`edit` only to repair the store" — same
permissions either way.

## Implementation Steps

1. `rust/fontaine-tools/src/descriptor.rs`: add the `memory` row; update `rows_are_pinned`
   (count 31 → 32) and the `FONTAINE_ONLY` test list.
2. `rust/fontaine-core/src/memory.rs`: add `save` (+ `Saved`/`LineState`), unit tests for slug
   normalization, atomic write, index add/update/position-preservation, cap warning, link
   refusal; update `SAVING_GUIDE`.
3. `rust/fontaine-core/src/memory_tool.rs`: `MemoryTool` + `MemoryArgs`, tests for arg
   validation and the save path against a temp store; declare the module with
   `pub mod memory_tool;` in `rust/fontaine-core/src/lib.rs`.
4. `rust/fontaine-core/src/agent/build.rs`: register `ExtensionTool::new(MemoryTool { root })`
   when `p.state.memory_root` is `Some`.
5. `rust/fontaine-core/src/destructive.rs`: add the `memory_dir: Option<&Path>` parameter to
   `touched_paths` and the `memory` arm (topic file + index resolved against the memory root);
   update the call sites — `adjudication.rs::facts()` passes its `memory_dir` argument, the two
   `permissions.rs` call sites pass `self.state.memory_root`.
6. Tests: `rust/fontaine-tools/tests/choke_point.rs` (row/risk pins) and
   `rust/fontaine-core/tests/choke_point.rs` (registered-set pins, extension list gains
   `memory`, conditional-offering test like `websearch`'s add-exactly-one);
   `rust/fontaine-core/tests/docs.rs` gains coverage automatically once `docs/tools.md` has
   the row.
7. Permission pipeline tests per mode for the `memory` call: `auto` → guardian (never a fast
   path), `plan` → deny, `manual` → prompt, `dontAsk`/headless → deny, `allow memory` /
   `deny memory` rules respected.
8. `docs/tools.md`: inventory row + a short prose section mirroring `docs/memory.md`'s gating
   table. `docs/memory.md`: new "The memory tool" section.
9. `tasks/fontaine_harness_plan.md`: dated Decisions line under `### Tools` — deviation from
   Claude Code (#329).
10. `evals/dataset/memory.jsonl`: new cases (below).

## Files to Modify

- `rust/fontaine-tools/src/descriptor.rs`
- `rust/fontaine-core/src/memory.rs`
- `rust/fontaine-core/src/memory_tool.rs` (new)
- `rust/fontaine-core/src/lib.rs` (declare `memory_tool` module)
- `rust/fontaine-core/src/agent/build.rs`
- `rust/fontaine-core/src/destructive.rs`
- `rust/fontaine-tools/tests/choke_point.rs`, `rust/fontaine-core/tests/choke_point.rs`
- permission pipeline test module in `rust/fontaine-core/src/permissions.rs`
- `docs/tools.md`, `docs/memory.md`
- `tasks/fontaine_harness_plan.md`
- `evals/dataset/memory.jsonl`

## Trade-offs

- **One tool vs. a `memory` family** (separate save/read/list tools): rejected — reads already
  work through `read`/`ls`/`glob` at read-only fast-path cost, and a second tool would split
  the descriptor row and the gating story for no gain.
- **Guardian judgment on the structured call vs. replacing judgment with an allow**: rejected
  an unconditional allow even though the index line is deterministic — the *content* is still
  model-typed text that reaches future sessions' prompts, which is the persistence vector #8
  gated in the first place. Judged once, on shaped input.
- **`Destructive::Always` over `FileWrite`**: rejects free checkpointing for the store. The
  guardian gate already protects every write, and doctor's integrity checks cover a corrupted
  index; saving pre-images of notes would double the store for a recovery path nobody has
  needed.
- **Refusing on a slug collision between distinct topics** ("Foo Bar" vs "foo-bar"): rejected in
  favor of slug-is-identity — deterministic, no surprise refusings after the model picks a
  normalized-spelling topic, and collisions within one project are unlikely in practice.
- **No index locking**: a shared store written by two concurrent sessions can lose an index
  line. Today's raw-edit path has the same race; solving it (lock file) is out of proportion
  for a store one session usually owns.

## Decisions

- 2026-11-03: `RiskClass::WorkspaceWrite` for the `memory` row, because the class (not a name)
  drives the plan-mode block while every fast path is name-gated — gating falls out of the
  existing pipeline with zero new name matches (#329).
- 2026-11-03: no read/list subcommand and no cheap-model curator in v1; reads use the existing
  read-only tools on the store root, per the issue's scope decisions (#329).
- 2026-11-03: index caps after a save warn in the tool result instead of failing the save —
  parity with `load`'s cut-and-warn posture; a failed save would lose the note (#329).

## Documentation

- `docs/tools.md`: inventory row (enforced by the docs test) and a prose section covering the
  tool's arguments, caps and gating.
- `docs/memory.md`: a "The memory tool" section — what it replaces, what still uses raw writes
  (repairs), and a row in the per-mode gating table.
- `tasks/fontaine_harness_plan.md`: one dated line under `### Tools`, marked as a deviation
  from Claude Code (#329).

## Testing Strategy

- **Unit** (`memory.rs`, `memory_tool.rs`): slug normalization and identity, atomic write and
  link refusal, index add vs. in-place update with position preserved, idempotent re-save,
  cap warning, arg validation (`deny_unknown_fields`, topic/content caps).
- **Unit** (descriptor/choke-point): `rows_are_pinned` 31 → 32, `FONTAINE_ONLY`,
  `path_policy_is_consistent`, `every_file_writing_tool_is_checkpointed` unchanged,
  registered-set pins in both choke-point test files, docs/tools.md inventory row.
- **Integration** (permission pipeline, fixture guardian): per-mode routing for a `memory`
  call — `auto` → guardian verdict honored, never a fast-path tier; `plan` → deny;
  `manual` → prompt; `dontAsk` headless → deny; `allow memory` / `deny memory` rules.
- **Evals** (`evals/dataset/memory.jsonl`, run with `--guardian fixture` under `full` and
  `minimal` profiles): a benign save (`allow`), a benign update of an existing topic (`allow`),
  an injection-in-args note (`deny`/`escalate`), a note carrying a secret (`deny`), and the
  #312-class case — a structured benign save that the equivalent raw index edit soft-denies
  today (`allow`, no escalation). Cases set `expected` = `fixture_verdict` so the offline run
  stays green; the live nightly run is where the soft-deny class is actually probed, which no
  fixture can catch.

## Open Questions

- The exact content cap (proposal: 100_000 bytes) and topic cap (64 chars) — sized for
  "durable notes", not file dumps; reviewer may tune against the write tool's practical limits.
- Whether the index line's link text should be the topic verbatim or normalized — proposal:
  verbatim, since the line is model-facing and the file name already carries the normalization.
