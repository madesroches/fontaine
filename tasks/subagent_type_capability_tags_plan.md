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
`describe` appends ` (<tag>)` to each type's line, derived from the tools a type declares, minus its
bare (specifier-free) `disallowed` entries (never from a parent, which the description cannot see).
Specifier `disallowed` entries never count:

- Shell (`shell` or `powershell` entry, bare or with specifier): none → `no shell` (both `shell` and
  `powershell` must be gone, since bare `disallowed: ["shell"]` only removes `powershell` when the
  parent lacks POSIX shell); every shell
  entry has a specifier → `shell limited to listed commands`; any bare entry → `shell`.
- File edits (`edit`, `write`, `notebook_edit`): all gone → `no file edits`; else `edits files`.
- `tools: None` (inherits): `your tools`, `your tools, no shell`, or `your tools, no shell, no file
  edits`, the suffixes applying per the same classification.

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
5. `tasks/fontaine_harness_plan.md`: add a dated one-line Decision at the end of the `### Agents`
   subsection of `## Decisions`: the `agent` tool's type listing carries computed capability flags
   in place of Claude Code's per-type tool list, for token and noise reasons.

## Files to Modify
- `rust/fontaine-sdk/src/agents/registry.rs`
- `docs/agents.md`
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (`### Agents` Decision)

## Trade-offs
- Full tool list per type (Claude Code's form): see the `### Agents` Decision in the master plan.
- Giving `explore` a guardian-gated shell: rejected in the issue (shell cannot be proven read-only
  without argument matching).

## Decisions
- 2026-11-20: Tags describe the declared toolset, not the effective one; a child's toolset is
  never wider than its parent's, so a tag can claim a tool the parent lacks (e.g. `reviewer`'s shell on
  Windows without Git Bash) but never hides one the type has.

## Documentation
`docs/agents.md` (Agent types).

## Testing Strategy
Unit tests in `registry.rs`: `describe()` for the builtins names `no shell` on `explore`,
`shell limited to listed commands` on `reviewer`, `shell, edits files` on `worker`, `your tools` on `fork`; a user type
with `tools: None` and `disallowed: ["shell","powershell"]` shows `your tools, no shell`; with
`disallowed: ["shell"]` alone it does not show `no shell`; `["shell","powershell","edit","write","notebook_edit"]`
shows `your tools, no shell, no file edits`; a type listing `powershell` counts as shell.
