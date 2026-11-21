# Subagent Type Capability Tags Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/219

## Overview
The `agent` tool's description lists each type as `name: description`, so the parent model cannot
see that `explore` has no shell and dispatches shell-needing work to it (issue #219). Append a
computed capability tag to each type's line, say in the built-in `explore` description that
commands belong on `worker`, and document it. `explore` itself stays read-only.

## Current State
- `AgentTypes::describe` (`rust/fontaine-sdk/src/agents/registry.rs`) renders
  `- \`name\`: description`; it feeds the `agent` tool description and the "no agent type" error
  (`rust/fontaine-sdk/src/agents/spawn.rs`).
- `AgentType` carries `tools: Option<Vec<String>>` (`None` = inherit the parent's) and
  `disallowed`. Entries may carry a specifier (`shell(git diff *)`); `entry_tool`,
  `entry_has_specifier` and `entry_covers` already parse them.
- `explore`'s description is "Reads and searches the workspace; cannot change anything."
- `reviewer` has a limited, specifier-gated `shell`; `worker` has full `shell`, `edit`, `write`.
- `docs/agents.md` ("Agent types") tabulates the built-ins.

## Design
`describe` appends ` (<tag>)` to each type's line, derived from the type's declared `tools` and
`disallowed` (never from a parent, which the description cannot see):

- Shell (`shell` or `powershell` entry, bare or with specifier): none → `no shell`; every shell
  entry has a specifier → `shell limited to listed commands`; any bare entry → `shell`.
- File edits (`edit`, `write`, `notebook_edit`): none → `no file edits`; else `edits files`.
- `tools: None` (inherits): a tag `your tools` with `disallowed` shell/edit/write entries named as
  `no shell` / `no file edits` when they apply (via the same classification).

Tag form: `no shell, no file edits`, `shell limited to listed commands, no file edits`,
`shell, edits files`. The classification is one private helper on `AgentType` reused by
`describe`; no new config. User-defined types get the tag automatically.

Built-in `explore` description becomes: "Reads and searches the workspace; cannot change anything
or run commands (use `worker` for those)." Tags state facts; the description carries the routing
advice, so the two do not duplicate.

## Implementation Steps
1. `registry.rs`: add `AgentType::capabilities(&self) -> String` and use it in `describe`.
2. `registry.rs`: update the `explore` description.
3. `docs/agents.md`: note the tag in "Agent types" and show the routing hint; mention user-defined
   types are tagged too.
4. Tests (below). `CHANGELOG.md` entry.

## Files to Modify
- `rust/fontaine-sdk/src/agents/registry.rs`
- `docs/agents.md`
- `CHANGELOG.md`

## Trade-offs
- Full tool list per type: more tokens, repeated every turn, and `lsp`/`send_message` noise the
  model doesn't route on. Flags carry the decision.
- Giving `explore` a guardian-gated shell: rejected in the issue (shell cannot be proven read-only
  without argument matching).

## Decisions
- 2026-11-20: Tags describe the declared toolset, not the effective one; a child's toolset is
  never wider than its parent's, so a tag can overstate but never understate a restriction.

## Documentation
`docs/agents.md` (Agent types).

## Testing Strategy
Unit tests in `registry.rs`: `describe()` for the builtins names `no shell` on `explore`,
`shell limited to listed commands` on `reviewer`, `shell, edits files` on `worker`; a user type
with `tools: None` and `disallowed: ["shell"]` shows `no shell`; a type listing `powershell`
counts as shell.
