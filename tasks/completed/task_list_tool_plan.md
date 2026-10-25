# Task List Tool with TUI Rendering Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/22

## Overview

Give the model a session task list it can keep current while it works, and render that list in
the TUI as a collapsible panel. Claude Code ships this as TaskCreate/TaskGet/TaskList/TaskUpdate
(formerly TodoWrite). Fontaine registers one tool, `todo_write`, that replaces the whole list; the
list lives in an in-memory store that is written through to the session directory, so a resumed or
forked session gets it back. The tool touches no file the model names, so it takes the same
no-path fast path as `skill`.

## Current State

- Built-in names: `rust/fontaine-tools/src/names.rs` (`BUILTIN_TOOL_NAMES`, 11 entries).
- Risk classes: `rust/fontaine-tools/src/risk.rs`. Tools beyond `builtins::build` (`agent`, `skill`,
  `send_message`) sit in `EXTENSION_RISK`; a tool missing from the table defaults to `Arbitrary`.
- Extension tools are assembled in `fontaine-core/src/tools.rs` (`ExtensionTools`,
  `register_extension`) and constructed in `fontaine-core/src/agent.rs` (~line 1425). `SkillTool`
  (`fontaine-tools/src/skill.rs`) is the pattern to copy: a trait seam (`SkillSource`) implemented
  in core, so `fontaine-tools` never depends on core.
- Fast path: `fontaine-core/src/permissions.rs:647` allows a `ReadOnly` call only if
  `scope.locate` succeeds or `tool == "skill"`. Destructive derivation
  (`fontaine-core/src/destructive.rs:62`) treats every unlisted tool as destructive.
- Sessions: `fontaine-core/src/session.rs` owns a directory per session (ledger, conversation
  store). `Session::open` and `fork` reload/copy it.
- TUI: `SessionEvent::{ToolCall, ToolResult}` (`fontaine-core/src/turn.rs`) feed
  `fontaine-tui/src/transcript.rs`, which shows every tool call as a one-line `Entry::Tool`.
  `fontaine-tui/src/agents_panel.rs` is the existing panel the driver fills with rows.
- `docs/tools.md` lists the built-in tools and risk classes.

## Design

### Tool

`todo_write` in a new `fontaine-tools/src/todo.rs`:

```
args:   { "todos": [ { "content": string, "status": "pending"|"in_progress"|"completed" } ] }
result: "Updated todo list: N items (P pending, I in progress, C completed)"
```

- Replaces the whole list: the list is never re-injected into context, the model sees it only
  through its own earlier calls.
- `#[serde(deny_unknown_fields)]`. Rejected with an error result: more than 50 items, empty or
  over-long `content` (500 chars), more than one `in_progress` item (keeps the panel's "current"
  line unambiguous). An empty array clears the list.
- Seam: `trait TodoStore: Send + Sync { fn replace(&self, agent: &str, items: Vec<TodoItem>) -> Result<(), TodoError>; }`,
  where `TodoError` is a small enum in `fontaine-tools` (`Write { path, source: io::Error }`) that
  keeps the cause as `source()`; `TodoTool` renders it once with `fontaine_tools::error_chain::chain`
  into the tool's error result.
  `TodoItem { content, status: TodoStatus }` and `TodoStatus` live in `fontaine-tools` with serde
  derives. The agent id comes from the `AgentId` context value as in `SkillTool`.

### Store and persistence

`fontaine-core` implements `TodoStore` as `SessionTodos`: a `Mutex<HashMap<agent id, Vec<TodoItem>>>`.
Only the root agent's list is written through to `<session dir>/todos.json` (owner-only, atomic
write via `owner_only::write_atomic`, temp file + rename) on every root replace; subagent lists stay in memory.
`Session::create` and `open` build the struct literally (`session.rs` ~201, ~234), so both set the new
field; `open` loads it; a missing or unparseable file yields an empty list. `open` takes no
`Approver`, so an unparseable file leaves a pending warning string inside `SessionTodos`, which
`build_agent` (`agent.rs`, where `setup.approver` is in hand) drains with `Approver::warn` on first build. `Session::fork` already copies the directory; a test pins
that `todos.json` comes along. A write failure is returned as the tool's error result and also warned
through `setup.approver` (the `todo_write` tool is built with an approver clone), never swallowed. The file write happens inside the store `Mutex` (`owner_only::write_atomic`'s temp name is
`.<name>.<pid>.tmp`, so concurrent writes would race) and never across an await. Lists are keyed by
agent in memory so a subagent's calls cannot overwrite the root's; the TUI shows the root's list.
`SessionTodos` is an `Arc` field on `Session` (`Session` is `Clone`, and `build_agent` re-runs on
`rebuild()`), so every rebuilt toolset shares the same store.

### Permissions

- `todo_write` joins `EXTENSION_RISK` as `ReadOnly` and `BUILTIN_TOOL_NAMES` (so permission rules
  and agent type toolsets can name it).
- `permissions.rs`: the no-path fast-path exception becomes a small `NO_PATH_TOOLS = ["skill", "todo_write"]`
  check instead of a second `==`. This is the one place a name, not containment, earns the fast
  path.
- `destructive.rs`: add `todo_write` to the non-destructive arm.
- `permission_rules.rs` `validate_specifier` (:157-165): add a "takes no specifier" arm for
  `todo_write`; otherwise it falls to `_ => Ok(())` and a rule with a specifier silently never matches.
- The deterministic floor still runs first: `deny`/`ask` rules naming `todo_write` apply.

### TUI

- New `fontaine-tui/src/todo_panel.rs`: `TodoPanel { items, expanded }`. Collapsed it is one line
  above the input: `Tasks 3/7 · ▶ <in-progress content>`; expanded it lists every item with
  plain `[ ]`, `[~]`, `[x]` markers. Hidden when the list is empty. Toggled by Ctrl-O, a plain
  bool toggle checked beside Ctrl-T in `handle_input` (`app.rs:518`) under the same `!typing_reason`
  guard (the Ctrl-T check follows the `if let Some(panel) = &mut self.agents` early return, so Ctrl-O
  is ignored while the agents panel is open), and by a new `/todos` slash command. Not `/tasks`, which aliases the agents panel.
- `/todos` wiring (`commands.rs`): a `Command::Todos` variant, a `BUILTINS` entry in alphabetical
  position (`the_table_is_sorted_by_name` enforces it), a match arm near `Command::Agents`
  (`lib.rs` ~503), and a `parse_command("/todos")` test.
- Layout (`view.rs`): the draw path uses a fixed vertical stack (:85-92) and `view::height`
  (:47-62) computes the inline live-area height; both get the todo-row constraint (0 when hidden,
  1 collapsed, item count when expanded). The expanded panel is capped at the terminal height minus
  the input rows, status row, and any approval/queued rows, and at least 1 row is kept; overflow
  shows the first items that fit with a final `+N more` line. Row position: directly above the
  input, below the approval and queued rows (stack: transcript, approval, queued, todos, input,
  status); the approval-height clamp counts the todo rows so the approval shrinks first. The `KEYS` help line (`reports.rs:28-29`) gains
  `ctrl+o todos`; optionally the `statusbar.rs:55` hint too.
- Screen-reader/linear mode (`linear.rs`): no panel. `linear.rs` is pull-based, so `Linear` keeps a
  last-printed snapshot of the list; each loop iteration compares the store's root list against it
  and, if it differs, prints the list as plain lines (`[ ]`/`[~]`/`[x]`) before the next prompt.
- Updates: in `on_event` (`fontaine-tui/src/lib.rs`), after a successful `todo_write` `ToolResult`,
  the driver reads the store and calls `panel.set_items(...)`. At start the panel is seeded from `runtime.session` (covers resume); the Session exposes the store
  via a read accessor the driver uses. `/clear` empties the list (and `todos.json`); `rebuild()` keeps it.
- The transcript keeps its one-line `Entry::Tool` for the call. `summarize(args)` in `transcript.rs`
  is not per-tool (first of `SUMMARY_KEYS`, else raw JSON; called with args only, `transcript.rs:56-62`), so
  `summarize` (or its call site) gains the tool name and a `todo_write`
  special case is added, computing the counts from `args.todos`.
- Headless (`fontaine -p`): no panel; the tool works and the call appears in the event stream like
  any other.

### System prompt

One short paragraph appended to the base prompt `fontaine-core/prompts/system_prompt.md` (included
at `prompt.rs:7`), telling the model to use `todo_write` for multi-step work, keep one item
`in_progress`, and mark items complete as it finishes them. The base prompt also goes to subagents
(`agent.rs:570-590`), so it is worded conditionally ("if you have `todo_write`"). Snapshot
`fontaine-core/tests/snapshots/system_prompt__the_assembled_prompt_is_stable.snap` (driven by
`tests/system_prompt.rs`) is updated.

## Implementation Steps

1. `fontaine-tools`: add `todo.rs` (types, `TodoStore`, `TodoTool`, validation); export in `lib.rs`;
   add the name to `names.rs` (`BUILTIN_TOOL_NAMES` goes `[&str; 11]` to `[&str; 12]`) and the row
   to `EXTENSION_RISK` in `risk.rs`.
2. `fontaine-core`: add `SessionTodos` (new `todos.rs`), load/save in `session.rs`; hold it on
   `Session`; add `todo: TodoTool` to `ExtensionTools`, register it in `tools.rs`, construct it in
   `agent.rs`.
3. `fontaine-core`: fast-path and destructive changes in `permissions.rs` / `destructive.rs`;
   system prompt paragraph in `prompts/system_prompt.md`.
4. `fontaine-tui`: `todo_panel.rs`, wire into `app.rs` / `view.rs`, Ctrl-O binding and `/todos`
   command, transcript `summarize` special case, linear-mode rendering, and seeding/refresh in
   `fontaine-tui/src/lib.rs` (`refresh_panel` ~227, `set_rows` ~270, `on_event` ~1593).
5. Update `choke_point.rs` (it asserts `EXTENSION_RISK` is exactly the listed entries; the array
   type in `risk.rs:62` goes `[_; 3]` to `[_; 4]`) and `headless_smoke.rs` tool list; the "three beyond the built-ins" comment at `choke_point.rs:112` goes
   stale; docs
   (`tests/docs.rs` requires a `` | `todo_write` | `` row in `docs/tools.md` for every
   `EXTENSION_RISK` entry); CHANGELOG.

## Files to Modify

- `rust/fontaine-tools/src/{todo.rs (new), lib.rs, names.rs, risk.rs}`
- `rust/fontaine-core/src/{todos.rs (new), lib.rs, session.rs, tools.rs, agent.rs, permissions.rs, permission_rules.rs, destructive.rs}`
- `rust/fontaine-core/prompts/system_prompt.md`
- `rust/fontaine-tui/src/{todo_panel.rs (new), lib.rs (seeding/refresh, `/todos` arm), app.rs, commands.rs, view.rs, reports.rs, linear.rs, transcript.rs}`
- `rust/fontaine-core/tests/choke_point.rs`, `rust/fontaine-core/tests/system_prompt.rs`,
  `rust/fontaine-core/tests/snapshots/system_prompt__the_assembled_prompt_is_stable.snap`
- Tests: in-file `#[cfg(test)]` modules in `todos.rs` and `session.rs` (`SessionTodos`, fork) and
  `permission_rules.rs`/`permissions.rs` (permission cases); `rust/fontaine/tests/headless_smoke.rs`
  (tool list) and the combined headless test (see Testing Strategy).
- `docs/tools.md`, `docs/tui.md` (keys table, Commands table :196), `docs/permissions.md`, `docs/architecture.md`, `docs/headless.md`, `CHANGELOG.md`

## Trade-offs

- **New `RiskClass::Internal` vs. `ReadOnly` plus a named exception.** A new class touches every
  `match` on `RiskClass` and the guardian's inputs for a single tool; `skill` already sets the
  named-exception precedent.
- **New `SessionEvent::Todos` vs. driver pull.** The agents panel already uses the pull model;
  avoiding a new variant keeps every `SessionEvent` consumer (headless, evals) untouched.

## Decisions

- 2026-10-25: Single `todo_write` tool, replace-whole-list semantics (see Trade-offs).
- 2026-10-25: Lists are keyed by agent id and the TUI shows only the root's; shared lists are a follow-up.
- 2026-10-25: Only the root's list is persisted to `todos.json`; subagent lists are in-memory (agent ids are not stable across resume).
- 2026-10-25: Panel toggle is Ctrl-O (unbound; Ctrl-T is the agents panel) plus a `/todos` slash command (`/tasks` aliases the agents panel).
- 2026-10-25: Screen-reader/linear mode renders the list as plain lines on change; no glyph fallbacks or per-item theme styles.
- AFK: 2026-10-25: Load warning is stashed by `Session::open` and drained in `build_agent` via `setup.approver`; write-failure warning goes through the same approver (no approver param on `open`).
- AFK: 2026-10-25: Linear mode detects changes by comparing the store's root list to a last-printed snapshot in `Linear`; no new event or transcript notice.
- AFK: 2026-10-25: Expanded todo panel sits directly above the input (below approval/queued), is capped at available height minus input/status/approval/queued rows with a `+N more` line, and the approval shrinks before it.

## Documentation

- `docs/tools.md`: add the `todo_write` row (read-only class) with its arguments; fix line ~56
  ("skill is the one read-only tool that names no path"), which becomes false.
- `docs/tui.md`: the task panel, its collapsed/expanded forms, a Ctrl-O row in the keys table (51-64)
  and the `/todos` command.
- `docs/permissions.md`: note that `todo_write` takes the fast path and can still be denied by rule;
  check line ~114 for a per-tool row that needs a `todo_write` entry.
- `docs/architecture.md` (152-180): sessions keep two stores and resume/fork copy "both stores"; now
  a third file, `todos.json`.
- `docs/headless.md:56`: add `todos.json` to the session dir contents.
- `CHANGELOG.md`: entry under unreleased.

## Testing Strategy

Unit (no live APIs):
- `todo.rs`: schema parse, unknown field rejected, each validation limit, empty array clears, result text counts.
- `SessionTodos`: replace/read per agent in memory, write-through file contents (root only; a subagent replace leaves `todos.json` untouched), load on open, corrupt file →
  empty list + warning, forked session carries the list, write failure surfaces as an error result.
- Permissions: `todo_write` in default mode takes the fast path; a `deny` rule on it still denies
  (floor precedence); a rule with a specifier (`todo_write(x)`) is rejected by `validate_specifier`; `choke_point.rs` expects the new name with `ReadOnly`; destructive derive is false.
- `choke_point.rs`: `EXTENSION_RISK` has exactly four entries including `todo_write`; `tests/docs.rs` finds the `docs/tools.md` row.
- Child toolset: an agent type narrowed to `read` does not get `todo_write`.
- TUI (`fontaine-tui` has no insta): `TodoPanel` rendering (collapsed, expanded, empty) asserted
  with the `ui.send` / `ui.screen().contains` harness (`tests/common/mod.rs`); Ctrl-O and `/todos` toggle; linear-mode plain-line rendering; driver refresh after a `todo_write` result using `rig_core::test_utils`
  deterministic models.
- Headless integration: a single combined test (call, file, resume) in
  `rust/fontaine/tests/headless_smoke.rs`, reusing the `Endpoint`/`Turn::tool` harness from
  `headless_smoke.rs` and `resume.rs`: a scripted model calls `todo_write`; the `tool_result`
  event succeeds, `todos.json` appears in the session dir, and resuming that session reloads it.
