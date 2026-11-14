# Built-in `reviewer` Agent Type Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/616

## Overview

Add a fourth built-in agent type, `reviewer`: the `explore` tools plus `shell` limited by
specifiers to a fixed allowlist of review commands (`git diff`, `gh pr view`, `cargo clippy`,
`cargo test`, …), with no edit tools, and a type prompt that says to report findings, not fix them.
Today nothing sits between `explore` (no shell) and `worker` (edit tools plus unrestricted `shell`).

## Current State

- Built-ins are inserted in `AgentTypes::builtin()` (`rust/fontaine-agents/src/registry.rs`):
  `explore`, `worker`, `fork`. `enable_teams` gives `worker` both board tools and `explore`
  `board_show`.
- A `tools` entry may carry a specifier (`shell(git diff *)`). `child_runner::restrictions`
  (`rust/fontaine-core/src/child_runner.rs`) turns specifier entries into `ChildRestrictions.only`,
  and `restriction_on` (`rust/fontaine-core/src/gate.rs`) refuses a call of that tool that matches
  none of them. It parses entries with `parse_rule(..).expect("validated when the definition was
  loaded")`, so a built-in entry that does not parse would panic at spawn.
- A `shell` allow rule matches only one simple command with literal words equal to the rule, or
  starting with it for a trailing ` *`; chains, pipes, redirections, substitutions, globs and
  variables never match (`docs/permissions.md`).
- `AgentType.prompt` is added to the subagent system prompt for any origin
  (`prompt::subagent_layer`, used from `fontaine-core/src/agent/setup.rs`), so a built-in can carry
  one.
- `toolset` lets an entry naming `shell` bare cover `powershell` when the parent has no `shell`; an
  entry with a specifier does not.
- The read-only spawn fast path applies only when every tool in the child's set is read-only;
  `shell` is not, so `reviewer` is gated like `worker`.
- Docs: `docs/agents.md` ("Agent types", currently "Three types are built in").

## Design

New built-in in `AgentTypes::builtin()`:

- `name`: `reviewer`; `description`: "Reads and searches the workspace and runs a fixed set of
  review commands; edits nothing through the file tools."
- `tools`: `ls`, `read`, `glob`, `grep`, `lsp`, `send_message` (the `explore` set, shared through
  one constant so the two cannot drift) plus the allowlist entries below.
- `prompt`: `Some(..)`: review the change you were pointed at, run the allowed commands to check
  it, report findings with file and line, and do not fix anything or try to work around a refused
  command.
- `model: Main`, `mode: Inherit`, `effort: None`, `origin: Builtin`, `inherits_context: false`.

Allowlist (fixed; each command needs a bare entry and a `*` entry because `shell(x *)` does not
match `x` alone):

- `git diff`, `git log`, `git show`, `git status`
- `gh pr view`, `gh pr diff`, `gh pr checks`, `gh issue view`
- `cargo clippy`, `cargo test`, `cargo fmt --check`

`cargo fmt --check` needs only a `cargo fmt --check` / `cargo fmt --check *` pair, so plain
`cargo fmt` (which rewrites files) is refused.

`enable_teams` gives `reviewer` `board_show`, like `explore`.

No gate change: `restriction_on` already enforces the entries per call. No new risk class or tool.

### Limits (documented, not fixed)

This is "no edit tools, shell limited to listed commands", not read-only: `cargo` writes
`target/` and runs build scripts and tests, `git diff --output=<file>` and some `gh` options write.
Because it holds `shell`, `reviewer` has no read-only spawn fast path and is gated like `worker`;
every call it makes still passes the full pipeline.

## Implementation Steps

1. `registry.rs`: extract the `explore` tool list into a shared constant; add the `reviewer`
   built-in with the allowlist entries and prompt; add `reviewer` to `enable_teams`; update the
   doc comment on `builtin()`.
2. Registry unit tests (below).
3. A gate-level test in `fontaine-core` that a `reviewer` child's `shell` call is allowed for
   `git diff HEAD~1` and refused for `git diff && rm x`, `git diff > x`, `cargo fmt` and
   `git push`.
4. `docs/agents.md`: "Four types are built in", table row, the shell-limit and Windows notes.
5. `CHANGELOG.md` Added entry; a dated line in the master plan's `## Decisions` under `### Agents`.

## Files to Modify

- `rust/fontaine-agents/src/registry.rs`
- `rust/fontaine-core/tests/` (a gate test, in the file that already drives child restrictions or
  `permissions.rs`)
- `docs/agents.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- A fixed allowlist vs. a configurable one: users who want another list already write their own
  type (`[agents.types.<name>]` or an agent file, `tools = ["shell(...)"]`), so the built-in stays a
  fixed, reviewable default.
- Specifier entries vs. a dedicated read-only shell tool: the specifier mechanism already exists
  and is enforced at the gate; a new tool would need a new risk class and parser.

## Decisions

- Allowlist is fixed; users define their own type for a different list (resolves the open question
  on extension).
- Windows without Git Bash: the specifier entries do not cover `powershell`, so `reviewer` has no
  command tool there. Accepted: it degrades to an `explore`-like type rather than widening to an
  unrestricted `powershell`. Documented.
- The `claude-subscription` undefined-tool error wording is out of scope; it concerns that
  provider's transport and not this type.
- `cargo test` and `cargo clippy` are in despite writing `target/` and running project code: a
  reviewer without them cannot verify a change. Documented as a limit.

## Documentation

`docs/agents.md` (types table, limits, Windows note); `docs/permissions.md` needs no change.

## Testing Strategy

Unit tests in `registry.rs`:
- `reviewer` is built in, holds no `edit`, `write`, `notebook_edit` or `memory`, and its toolset
  from a full parent is the `explore` tools plus `shell`.
- Every `reviewer` specifier entry parses with `parse_rule` (guards the `expect` in
  `restrictions`) and `restrictions(reviewer).only` contains the expected rules.
- With a parent lacking `shell`, the toolset has no `powershell`.
- `enable_teams` gives it `board_show` and not `board_update`.
- A `reviewer` toolset is not read-only, so no fast path (asserted through the existing read-only
  classification if reachable from the crate; otherwise by containing `shell`).

Gate test (step 3) pins that chained, redirected and non-listed commands are refused. No live
services are needed.

## Open Questions

None.
