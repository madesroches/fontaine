# Checkpoints and `/rewind` Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/12

## Overview

Claude Code snapshots a file before each edit-tool change, keyed by user prompt, and `/rewind` (or
Esc Esc) restores the code, the conversation, or both to the start of any earlier prompt. Edits made
through the shell are not tracked. Fontaine does the same: the `write` and `edit` tools save the
file's pre-image after the gate allows the call, a conversation rewind replays rig-memory's log up to
the prompt (`as_of`), and the session ledger is never truncated, so denial and approval history
survive a rewind. Parity target is the documented behavior; deviations are listed under Decisions.

## Current State

- `GuardedTool` (`fontaine-tools/src/guarded.rs`) is the single choke point. `gated` asks the gate,
  and only on `Allow` calls `run`, which locates the per-root tool instance (`Scope::locate`) and
  calls the tool. Nothing there knows about files before they change.
- `write` and `edit` are rig-tools types (`builtins.rs` `write_for` / `edit_for`) sharing a `Seen`
  (read-before-write staleness tracking). Their risk row lives in `descriptor.rs` `TOOLS`.
- `destructive::touched_paths` (`fontaine-core/src/destructive.rs:275`) already resolves the absolute
  paths of a call; the gate's `action` (`permissions.rs:~985`) records them in `Record::Action`,
  with `created` for a `write` to an absent path.
- Two stores per session (`tasks/design/sessions.md`): rig-memory conversation log
  (`CONVERSATION_LOG`) and the append-only ledger. `Session::clear` (`session.rs:618`) is the
  precedent for rewriting the live conversation: `set_entries(.., Author::plugin("clear"))`, drop the
  compaction asides, save, `context_was_reset()`. The log keeps every earlier state as logged changes.
- `rig_memory::history(store, log)` returns every record with its `Stamp { n, seq, t }`;
  `rig_memory::as_of(store, log, n)` returns the conversation as the records before `n` left it.
- `LedgerView` (`ledger.rs`) derives the guardian's intent from `UserMessage` records since the last
  `ContextCleared`. `Record::Resumed`, `SessionNamed`, `Scheduled` are examples of records the view
  ignores.
- Fork (`session::copy_tree`) copies the whole session directory; resume reopens it.
- `/clear` (`fontaine-tui/src/ui/commands/context.rs`) refuses to run while a subagent runs.
  `restore_to_input` puts text back into the prompt box. Esc at an idle prompt cancels a pending
  self-paced loop wakeup (`app.rs:647`); otherwise it does nothing.
- `rig-tools` ships a `checkpoint` module (git-tree or scan snapshots, tracks shell changes too).
  It spawns `git` and snapshots whole trees; Fontaine never spawns `git` and Claude Code does not
  track shell changes, so it is not used (Trade-offs).
- `core_slash_commands_plan.md` reserved `/rewind` for this issue; `research-claude-code-parity.md:300`
  lists checkpointing as `missing`.

## Design

### What is a checkpoint

A **turn checkpoint** is created when the human submits a turn: a typed prompt
(`fontaine-tui/src/ui/turn.rs:257`), an expanded skill command (`ui/mod.rs:364`), `/init`
(`ui/commands/memory.rs:183`) or a goal-set turn (`ui/goal.rs:44`). Mid-turn steering
(`steering.rs:80`), scheduled prompts (`ui/schedule.rs:178`) and subagent questions
(`questions.rs:36`) do not make one. It holds:

- `turn`: a 1-based ordinal within the current clear epoch
- `conv_n`: one past the last stamp in `history(store, CONVERSATION_LOG)` (0 if empty), read at
  submit time after the previous turn's save. The typed prompt reaches the ledger first and the
  conversation log only inside `run_turn` (`agent/build.rs:239`), so `as_of(conv_n)` is the
  conversation before this prompt
- `prompt`: the text, for the picker (the ledger already holds it verbatim; this copy is for display)
- `user_messages_before`: how many `UserMessage` records the ledger view held, to size the
  intent trim (below)

`/clear` starts a new epoch; the picker lists only the current epoch's turns, so a rewind never
resurrects context the ledger's intent view has already dropped.

File pre-images attach to the checkpoint current when the edit happens, whichever agent made it: a
subagent's edits belong to the user turn that spawned it, and a background subagent's late edits
belong to whatever turn is current.

### Storage

New module `fontaine-core/src/checkpoint.rs`, files under `<session dir>/checkpoints/` (owner-only,
a top-level directory beside `ledger/`, outside rig-memory's `logs/` and `files/`):

```
checkpoints/manifest.jsonl   append-only records
checkpoints/blobs/<hash>     pre-image bytes, content-addressed (deduplicated)
```

```rust
enum Entry {
    Epoch {},                                   // written by /clear
    Turn { turn, conv_n, prompt, user_messages_before },
    PreImage { turn, agent, path, before: Before, after: Option<ContentHash> },
}
enum Before { Absent, Blob { hash, unix_mode: Option<u32> }, Skipped { why: SkipReason } }
enum SkipReason { TooLarge, NotRegular, Unreadable, Budget }
```

- The first `PreImage` for a `(turn, path)` is the one that counts; later edits in the same turn add
  nothing but their `after` hash. `after` (hash of what the tool left) is appended when the call
  finishes, so a restore can tell whether the file changed since Fontaine last wrote it.
- Caps under `[checkpoints]` (user scope only, `Rule::user_only`): `enabled` (default `true`),
  `max_file_bytes` (default 10 MiB), `max_session_bytes` (default 256 MiB). A file over a cap is
  recorded `Skipped`, the edit still proceeds, and the user gets one `Approver::warn` per session per
  reason. Capture never blocks or denies an edit: it is a convenience, and failing closed would let a
  full disk stop all editing.
- The store is `Mutex`-guarded; a poisoned lock panics (`fontaine_tools::lock::lock`) because its
  contents feed a restore.
- Manifest and blob writes use `owner_only`. A torn last manifest line is skipped on read, as
  `session::list` tolerates torn ledgers.

### Capture seam

`fontaine-tools` cannot depend on `fontaine-core`, so the seam is a trait beside `ToolGate`:

```rust
// fontaine-tools/src/checkpoint.rs
pub trait PreImages: Send + Sync {
    /// Called after the gate allowed the call, before the tool runs.
    fn before(&self, agent: &str, tool: &str, path: &Path) -> Option<Capture>;
}
pub trait Capture: Send { fn finish(self: Box<Self>, succeeded: bool); }  // records `after`
```

- `Guard` gains `pre_images: Option<Arc<dyn PreImages>>`.
- `ToolDescriptor` gains `checkpointed: bool`, true for `write` and `edit` only. A tool absent from
  the table is not checkpointed (unlike risk, where absence is strictest) — the choke-point test
  gets an assertion that `destructive == FileWrite` implies `checkpointed`, so a new editing tool
  cannot be forgotten silently.
- In `GuardedTool::run`, after `Scope::locate` and before `tool.call`, a checkpointed tool resolves
  its `path` argument with `Scope::find` (`scope.rs:109`; not core's `touched_paths`, and not the
  located args, since `locate` rewrites `path` only for non-primary roots) and calls `before`.
  `run` receives only `(context, args)`, so the agent name is derived (`agent_of`) or passed in.
  `run` is restructured so `finish(ok)` is called with the tool's raw success or failure before
  the `?` propagates, not the result pipeline's outcome. It runs only on the `Allow` path, so a
  denied call captures nothing. MCP tools and the shell are not checkpointed.
- `fontaine-core` implements `PreImages` over the checkpoint store. The guard is built in
  `tools.rs:114` (registry via `RegistryParams`, called from `agent/build.rs:162`) and
  `agent/mcp.rs:112` (`pre_images: None`); `fontaine/src/lib.rs` assembles the session and store.
  The current turn is read from the store.

### Rewind

`fontaine-core::checkpoint::plan(turn) -> RewindPlan` and `Rewinder::apply(plan, what)`, where
`what` is `Both | Conversation | Code`.

**Code.** For every path with a `PreImage` in turns `>= turn`, the earliest one wins and gives the
state at the start of `turn`:

- `Absent`: delete the file if it is a regular file now.
- `Blob`: write the bytes back atomically with `rig_tools::fs::write_atomic` (it follows symlinks
  and preserves an existing file's mode), so the symlink refusal below is a separate pre-check and
  `unix_mode` is applied explicitly. (`owner_only::write_atomic_keeping_mode` also exists.)
- `Skipped`: reported as not restorable.

Safety on restore, because the manifest is trusted data from disk: each path is re-checked to be
inside the current workspace roots (`Scope::find`), a symlink at the target or in its parent chain
is refused rather than followed, and anything failing is reported, not restored. The plan also flags
files whose current hash differs from the last recorded `after` ("changed since Fontaine last wrote
it"); the picker's confirmation lists them and restoring them requires the user's confirmation
because it discards the user's own edits. Nothing outside tracked `write`/`edit` changes is touched,
so shell-made changes, user `!` commands and Skipped files stay as they are (stated in the preview).
Restoring is a harness action at the user's request, like a `!` command: it does not pass through
the permission pipeline, and it is not a model-callable tool, so no risk class is involved.

The rig-tools `Seen` set is left alone: a restored file's content no longer matches what was read,
so the next `edit` or `write` of it already demands a fresh `read`, which is the right behavior.

**Conversation.**
1. `as_of(store, CONVERSATION_LOG, turn.conv_n)` gives the conversation before the prompt.
2. Like `Session::clear`: load the live conversation, drop the compaction asides, `set_entries` with
   `Author::plugin("rewind")`, save, `context_was_reset()`. The log keeps the rewound states, so
   nothing is lost and a later rewind to any earlier turn still folds correctly. Rewinding across a
   compaction restores the pre-compaction messages.
3. An active goal is cleared (it described the conversation that is gone), as `/clear` does; the
   task list, schedule and permissions carry on.
4. The prompt text goes back into the input box for editing, as Claude Code does.

**Ledger.** One new record, `Rewound { turn, what, dropped_user_messages }`, appended after the
operation (even when some files failed, with the counts in the TUI message). Nothing is removed or
edited. `LedgerView` handles it by trimming its `user_messages` by `dropped_user_messages`:
a retracted prompt must not keep authorizing actions in the guardian's request, while approvals,
grants and denials stay untouched, so `/denied` and the guardian's history are complete. For
`what = Code` the count is zero. `session::list` ignores it.

Conversation rewinds and `/rewind` are refused with the same runtime guard as `clear_conversation`
(`agent.rs:750-760`: `any_running() || has_notifications(ROOT_AGENT)`) plus the TUI's running-turn
check (`ui/commands/context.rs`).

### UI

- `/rewind` (alias `/checkpoint`) and Esc Esc at an idle prompt with empty input open the picker.
  The existing single-Esc behaviors (interrupt a running turn, cancel a pending loop wakeup, close
  the palette) win first; the second Esc within the double-press window opens the picker only when
  nothing else claimed Esc.
- Picker: the current epoch's prompts, newest at the bottom, each with the number of files its turn
  and later turns changed (derived from the manifest, no disk reads). Enter selects.
- Action menu: *Restore code and conversation*, *Restore conversation*, *Restore code*, *Never
  mind*. Code actions show the plan first: files to restore, files to delete, files flagged as
  changed since, files not restorable, and the shell caveat.
- Headless `-p` has no rewind; sessions resumed with `--resume` can rewind earlier turns.
- A new `fontaine-tui/src/rewind.rs` holds the picker and menu state as pure data, tested without a
  terminal; `ui/commands/rewind.rs` wires it to the runtime like the other commands.

### Fork, resume, compaction

- Fork copies `checkpoints/` with the rest of the tree; stamps in the copied log are unchanged, so
  the copy's `conv_n` values stay valid. Resume reopens the store.
- `conv_n` stays valid across compaction because compaction appends logged changes and never
  renumbers records.

## Implementation Steps

1. **Store.** `fontaine-core/src/checkpoint.rs`: entry types, manifest + blob IO, caps, skip
   reasons, first-wins resolution, torn-line tolerance. `conv_n` is read from `history`, which is
   O(log); prefer a cheaper last-stamp read if one exists, else accept it.
2. **Capture seam.** `fontaine-tools/src/checkpoint.rs` trait; `Guard.pre_images`;
   `ToolDescriptor.checkpointed` (the `row()` const fn and the `rows_are_pinned` `[Pinned; 18]`
   table); call in `GuardedTool::run`; core implementation and wiring in `tools.rs`,
   `agent/build.rs`, `agent/mcp.rs`, `fontaine/src/lib.rs`; the new `Guard` field in the literals
   in `fontaine-tools/tests/guarded.rs`, `telemetry.rs`, `result_pipeline.rs`. Turn begin hooked at
   the submit sites listed under "What is a checkpoint" (`ui/turn.rs`, `ui/mod.rs`,
   `ui/commands/memory.rs`, `ui/goal.rs`).
3. **Config.** `[checkpoints]` keys in `config.rs`, `config_catalog.rs`, and rows in
   `config_scope.rs` (`Rule::user_only`), plus `fontaine-core/tests/config.rs` and the per-section
   row in the `config_scope.rs` test.
4. **Ledger.** `Record::Rewound`, `LedgerView::apply` arm (it matches `Record` exhaustively) and trim, `Session::rewind_conversation` beside `clear`
   (shared helper for the asides/`set_entries`/save/reset sequence), `/clear` writes the epoch entry.
5. **Rewinder.** Plan and apply for code with the scope, symlink and divergence checks; runtime
   methods on `AgentRuntime` mirroring `clear_conversation`.
6. **TUI.** `rewind.rs` state, Esc Esc handling in `app.rs`, `Command::Rewind` in `commands.rs`
   (menu entry, alias), `ui/commands/rewind.rs`, picker rendering following `resume_picker.rs`.
7. **Docs and bookkeeping** (see Documentation).

## Files to Modify

- New: `fontaine-core/src/checkpoint.rs`, `fontaine-tools/src/checkpoint.rs`,
  `fontaine-tui/src/rewind.rs`, `fontaine-tui/src/ui/commands/rewind.rs`, `docs/checkpoints.md`
- `fontaine-tools/src/guarded.rs`, `descriptor.rs`, `lib.rs`; Guard literals in
  `fontaine-tools/tests/guarded.rs`, `telemetry.rs`, `result_pipeline.rs`
- `fontaine-core/src/ledger.rs`, `session.rs`, `agent.rs`, `tools.rs`, `agent/build.rs`,
  `agent/mcp.rs`, `config.rs`, `config_catalog.rs`, `config_scope.rs`, `lib.rs`,
  `fontaine-core/tests/config.rs`; `fontaine/src/lib.rs`
- `fontaine-tui/src/app.rs`, `commands.rs`, `ui/turn.rs`, `ui/mod.rs`, `ui/goal.rs`,
  `ui/commands/mod.rs`, `ui/commands/memory.rs`, `ui/commands/context.rs`, `lib.rs`
- `docs/index.md`, `docs/tui.md`, `docs/configuration.md` (including the scope-table row),
  `docs/tools.md`, `docs/permissions.md`, `docs/guardian.md`
- `tasks/design/sessions.md`, `research/research-claude-code-parity.md`, `CHANGELOG.md`

## Trade-offs

- **Pre-image capture in `GuardedTool` vs. rig-tools `checkpoint` snapshots.** rig-tools snapshots
  whole trees (spawning `git` or scanning) around a call. That would also catch shell changes, which
  Claude Code deliberately does not, costs a tree walk per turn, and breaks the rule that Fontaine
  never spawns `git`. Per-file pre-images are cheap, exactly scoped to the two editing tools and
  gated by the same Allow.
- **Fail-open capture vs. fail-closed.** The guardian invariants are about permission decisions;
  a checkpoint is a recovery aid. Refusing an allowed edit because a snapshot failed would turn a
  disk-full or oversize file into a denial of service for the user's task. The skip is recorded and
  surfaced instead.
- **Stored `conv_n` vs. locating the prompt in the log at rewind time.** Searching `history` for the
  k-th user message breaks once compaction rewrites entries; a stamp taken at turn start is exact.
- **Trimming the intent view vs. leaving it.** Keeping a retracted prompt in `task_intent` would let
  words the user took back keep authorizing actions. Trimming only the derived view keeps the ledger
  append-only.

## Decisions

- Summarize-from-here (Claude Code's fourth rewind option) is out of scope; the picker has the three
  restore options. Tracked as a follow-up if wanted.
- Esc Esc opens the picker only at an idle, empty prompt; every existing single-Esc meaning wins.
- A conversation rewind clears an active goal, as `/clear` does; todos, the schedule, grants and
  mode carry on.
- Rewind points are limited to the current `/clear` epoch.
- The hash of what a tool left is stored so the restore can warn about the user's own later edits;
  Claude Code does not warn, Fontaine does because a restore overwrites without a backup.

## Documentation

- `docs/checkpoints.md` (new): what is tracked and what is not (shell, `!`, MCP, files over the cap),
  `/rewind` and Esc Esc, the three actions, how the ledger is kept, storage location and caps,
  `[checkpoints]` keys, fork/resume behavior. Linked from `docs/index.md`.
- `docs/tui.md`: `/rewind`, Esc Esc. `docs/configuration.md`: the `[checkpoints]` keys.
- `docs/tools.md`: `write` and `edit` save pre-images. `docs/permissions.md`: a rewind is a user
  action outside the pipeline and cannot be requested by the model.
- `tasks/design/sessions.md`: replace the one-line rewind mention with the `Rewound` record and the
  intent trim. `docs/guardian.md:53` and `docs/tui.md:287/298` (the `/clear` intent reset) mention
  the trim; `docs/tui.md` Esc table (98-99) and command table (~306-316) get the new rows.
  `research/research-claude-code-parity.md`: both rewind mentions (lines 200 and 300) updated, the
  parity row to `done`.

## Testing Strategy

Unit (no live APIs; `rig_core::test_utils` models, tempdirs):

- Store: first pre-image per `(turn, path)` wins, later edits only add `after`; absent file →
  `Absent`; blob dedup; per-file and per-session caps produce `Skipped` and still let the call run;
  torn manifest tail is skipped.
- Restore planning: earliest pre-image across several turns; created files are deleted; a path
  outside the roots, a symlinked target and a symlinked parent are refused; files edited since
  (`after` mismatch) are flagged; `Skipped` files are reported, not touched; unix mode restored.
- Ledger: `Rewound` trims only `user_messages`; `Approval`, `Grant` and `Denial` records and views
  are unchanged; code-only rewind trims nothing; the record is ignored by `session::list`.
- `GuardedTool` wrapper (with a fake `PreImages`): a denied call captures nothing; an allowed `write`
  and `edit` capture before the tool runs; read-only tools, shell and MCP tools never capture;
  per-root routing captures the right root's path. The descriptor test asserts the `checkpointed`
  rows. These pin the silent-failure cases: a denied edit leaving a stale snapshot, and an edit
  tool added without capture.
- TUI: picker selection, action menu, epoch filtering, Esc Esc timing against the other Esc
  meanings, command registration and alias.

Integration (core tests through the headless surface, deterministic model): edit a file over two
turns, rewind to turn 1 with Both, assert file bytes, conversation, input text and ledger contents;
rewind across a compaction restores the original messages (`as_of` replay catches what a fake store
could not); `/clear` then `/rewind` lists only post-clear turns; one combined fork test (fork a session with a
checkpoint, assert `checkpoints/` is present and a code restore works in the copy); resume is
covered by a store unit test (reopen reads manifest and blobs); the guardian request built after a conversation rewind omits the retracted
prompt but still shows prior denials.

## Manual Verification

Terminal layout of the picker is a tmux concern (`tasks/design/testing.md`), not reachable from
`TestBackend`:

1. In tmux, `cargo run -p fontaine`; make two prompts that edit a file; press Esc Esc.
   Expect the picker above the prompt without corrupting scrollback.
2. Pick the first prompt, *Restore code*; expect the file back to its original content and the
   confirmation listing it, and `git status` showing the working tree as it was before the session.
