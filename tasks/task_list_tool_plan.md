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

- Replaces the whole list, matching the shape the DeepSeek harness study measured as sufficient: the
  list is never re-injected into context, the model sees it only through its own earlier calls.
- `#[serde(deny_unknown_fields)]`. Rejected with an error result: more than 50 items, empty or
  over-long `content` (500 chars), more than one `in_progress` item (keeps the panel's "current"
  line unambiguous). An empty array clears the list.
- Seam: `trait TodoStore: Send + Sync { fn replace(&self, agent: &str, items: Vec<TodoItem>) -> Result<(), String>; }`.
  `TodoItem { content, status: TodoStatus }` and `TodoStatus` live in `fontaine-tools` with serde
  derives. The agent id comes from the `AgentId` context value as in `SkillTool`.

### Store and persistence

`fontaine-core` implements `TodoStore` as `SessionTodos`: a `Mutex<HashMap<agent id, Vec<TodoItem>>>`
written through to `<session dir>/todos.json` (owner-only, atomic write via temp file + rename) on
every replace. `Session::open` loads it; a missing or unparseable file yields an empty list and one
`Approver::warn` for the unparseable case. `Session::fork` already copies the directory; a test pins
that `todos.json` comes along. A write failure is returned as the tool's error result and warned,
never swallowed. Lists are keyed by agent so a subagent's calls cannot overwrite the root's; the
TUI shows the root's list. Sharing one list with subagents is left for later, as the issue says.

### Permissions

- `todo_write` joins `EXTENSION_RISK` as `ReadOnly` and `BUILTIN_TOOL_NAMES` (so permission rules
  and agent type toolsets can name it).
- `permissions.rs`: the no-path fast-path exception becomes a small `NO_PATH_TOOLS = ["skill", "todo_write"]`
  check instead of a second `==`. This is the one place a name, not containment, earns the fast
  path; it is justified because the tool's only effect is writing a file Fontaine itself picks
  inside the session directory, with content that is never executed.
- `destructive.rs`: add `todo_write` to the non-destructive arm.
- The deterministic floor still runs first: `deny`/`ask` rules naming `todo_write` apply.

### TUI

- New `fontaine-tui/src/todo_panel.rs`: `TodoPanel { items, expanded }`. Collapsed it is one line
  above the input: `Tasks 3/7 · ▶ <in-progress content>`; expanded it lists every item with
  `[ ]`, `[~]`, `[x]` markers (reuse `theme.rs` styles, and the glyph fallbacks `accessibility.rs`
  already uses). Hidden when the list is empty. Toggled by a key binding registered next to the
  agents panel's.
- Updates: the driver reads the store after a successful `todo_write` `ToolResult` and calls
  `panel.set_items(...)`, the same pull model as the agents panel rows, so no new `SessionEvent`
  variant. On session start/resume the driver seeds the panel from the store.
- The transcript keeps its one-line `Entry::Tool` for the call; its summary for `todo_write` shows
  the counts instead of the raw JSON.
- Headless (`fontaine -p`): no panel; the tool works and the call appears in the event stream like
  any other.

### System prompt

One short paragraph in the system prompt (`fontaine-core/src/prompt.rs`) telling the model to use
`todo_write` for multi-step work, keep one item `in_progress`, and mark items complete as it
finishes them. Snapshot test in `tests/snapshots` updated.

## Implementation Steps

1. `fontaine-tools`: add `todo.rs` (types, `TodoStore`, `TodoTool`, validation); export in `lib.rs`;
   add the name to `names.rs` and the row to `EXTENSION_RISK` in `risk.rs`.
2. `fontaine-core`: add `SessionTodos` (new `todos.rs`), load/save in `session.rs`; hold it on
   `Session`; add `todo: TodoTool` to `ExtensionTools`, register it in `tools.rs`, construct it in
   `agent.rs`.
3. `fontaine-core`: fast-path and destructive changes in `permissions.rs` / `destructive.rs`;
   system prompt paragraph.
4. `fontaine-tui`: `todo_panel.rs`, wire into `app.rs` / `view.rs`, key binding, transcript summary,
   driver seeding and refresh.
5. Update `choke_point.rs` tables and `headless_smoke.rs` tool list; docs; CHANGELOG.

## Files to Modify

- `rust/fontaine-tools/src/{todo.rs (new), lib.rs, names.rs, risk.rs}`
- `rust/fontaine-core/src/{todos.rs (new), lib.rs, session.rs, tools.rs, agent.rs, permissions.rs, destructive.rs, prompt.rs}`
- `rust/fontaine-tui/src/{todo_panel.rs (new), lib.rs, app.rs, view.rs, transcript.rs}`
- `rust/fontaine/src/lib.rs` (driver seeding/refresh)
- `rust/fontaine-core/tests/choke_point.rs`, `rust/fontaine/tests/headless_smoke.rs`, snapshots
- `docs/tools.md`, `docs/tui.md`, `docs/permissions.md`, `CHANGELOG.md`

## Trade-offs

- **One replace-all tool vs. Claude Code's four task tools.** One tool is one schema in every
  request and was measured sufficient by the DeepSeek study; the four-tool shape earns its keep
  only for dependency tracking and multi-agent claiming, which the issue defers.
- **New `RiskClass::Internal` vs. `ReadOnly` plus a named exception.** A new class touches every
  `match` on `RiskClass` and the guardian's inputs for a single tool; `skill` already sets the
  named-exception precedent.
- **New `SessionEvent::Todos` vs. driver pull.** The agents panel already uses the pull model;
  avoiding a new variant keeps every `SessionEvent` consumer (headless, evals) untouched.

## Decisions

- 2026-10-25: Single `todo_write` tool, replace-whole-list semantics (see Trade-offs).
- 2026-10-25: Lists are keyed by agent id and the TUI shows only the root's; shared lists are a follow-up.

## Documentation

- `docs/tools.md`: add `todo_write` to the table (read-only class) with its arguments.
- `docs/tui.md`: the task panel, its collapsed/expanded forms and the toggle key.
- `docs/permissions.md`: note that `todo_write` takes the fast path and can still be denied by rule.
- `CHANGELOG.md`: entry under unreleased.

## Testing Strategy

Unit (no live APIs):
- `todo.rs`: schema parse, unknown field rejected, each validation limit, empty array clears, result text counts.
- `SessionTodos`: replace/read per agent, write-through file contents, load on open, corrupt file →
  empty list + warning, forked session carries the list, write failure surfaces as an error result.
- Permissions: `todo_write` in default mode takes the fast path; a `deny` rule on it still denies
  (floor precedence); `choke_point.rs` expects the new name with `ReadOnly`; destructive derive is false.
- Child toolset: an agent type narrowed to `read` does not get `todo_write`.
- TUI: `TodoPanel` rendering (collapsed, expanded, empty, ASCII fallback) via ratatui `TestBackend`
  snapshots; toggle key; driver refresh after a `todo_write` result using `rig_core::test_utils`
  deterministic models.
- Headless integration: a scripted model calls `todo_write`; the `tool_result` event succeeds and
  `todos.json` appears in the session dir; resuming that session reloads it.

## Open Questions

- Which key toggles the panel? Choose one not already bound in `app.rs` (check the keymap in `docs/tui.md`).
