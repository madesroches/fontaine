# Read-only shell fast path: agent-type restrictions and read-scoped hooks Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/675

## Overview

A read-only `shell` command that the pre-classifier accepts (`cat secrets/key`) is checked against
the session's `rules.deny`/`rules.ask` on the `read`/`ls`/`grep` calls it stands for, then allowed
at `Tier05`. Two deterministic restrictions never see those synthesized calls: agent-type
`disallowed`/`only` restrictions on `read(...)` and `pre_tool_use` hooks whose matcher names
`read`, `ls` or `grep`. This plan closes both so the fast path is never looser than the same read
made through the file tool.

## Current State

- `PermissionPipeline::evaluate` (`rust/fontaine-sdk/src/core/permissions.rs`, the `tool == "shell"`
  block after the read-only fast path) calls `Preclassifier::read_paths`, builds the file-tool calls
  with `ruled_read_calls`, tests `rules.deny` then `rules.ask` through `ruled_hit`, then checks
  secret operands via `secret_read_calls`; with none it returns `Verdict::Allow { Tier05 }`.
- `SessionState::child_restriction` (`core/gate.rs`) is evaluated earlier in `evaluate`, but only on
  the real call (`tool == "shell"`); `restriction_on` matches rules by tool name, so a
  `read(secrets/**)` restriction never matches `shell`.
- `run_hooks` (`permissions.rs`) filters hooks by `matches(rule, CallContext { tool: call.tool, .. })`
  on the real call, so a `read(src/private/**)` hook never runs for a shell call.
- Out-of-workspace paths are rejected by the pre-classifier; secret files are handled in the same
  block. Both are unaffected.

## Design

Both fixes live in the `Some(paths) = read_paths(...)` branch of `evaluate`, reusing the `reads`
vector already built.

1. **Agent-type restrictions.** Run `self.state.child_restriction(call.agent, &CallContext { tool, args, scope, home })` for each
   entry of `reads`. Place the check
   immediately after the `rules.deny` check, before `rules.ask`, mirroring the main path where
   `child_restriction` precedes `forced_ask`. A hit returns `Verdict::Deny { tier: Tier::DenyRule,
   rule: Some("agent tools"), why, converted: None }` — the same shape as the existing denial.
   `only` restrictions on `read`/`ls`/`grep` (`restriction_on`'s second arm) apply to the built
   calls through the same function, with no extra code. Extract a small helper
   `restricted_read(&self, agent, &reads, scope, home) -> Option<String>` next to `ruled_hit` to keep
   `evaluate` readable.
2. **Read-scoped hooks.** A hook that cannot be evaluated against built calls must not be bypassed.
   Add `fn hooks_name_file_reads(&self) -> bool` on the pipeline: true when any hook in
   `self.hooks` or `self.plugin_hooks.get().pre` has a parseable matcher whose tool is `read`, `ls`
   or `grep`. When true, the shell branch skips the `Tier05` allow and falls through to the
   normal rule/mode routing, as the issue suggests: hooks do not run on synthesized calls. When
   the hook gate is set and `secrets` is empty, the whole secrets/restoration block is skipped and
   the call falls through to the allow rules and mode routing (never index `secrets[0]` on an
   empty vec). The skip is deliberately coarse: any hook naming `read`/`ls`/`grep` takes all shell
   reads off the `Tier05` fast path, regardless of its specifier. The deny
   and ask checks and the new restriction check above still run first. Hooks with no matcher, an
   unparseable matcher, or a `shell` matcher already run on the real call and do not trigger the skip.

## Implementation Steps

1. `permissions.rs`: add `restricted_read` helper and call it after the `rules.deny` check in the
   shell branch.
2. `permissions.rs`: add `hooks_name_file_reads`; when set, skip the `Tier05` return and the secrets/restoration block if `secrets` is empty.
3. Tests (below).
4. `docs/permissions.md` and `docs/hooks.md` (matcher behavior for shell reads): note that a
   read-only shell command is held to agent-type read restrictions and that a hook matching
   `read`/`ls`/`grep` takes shell reads off the pre-classifier fast path.
5. `CHANGELOG.md` entry.

## Files to Modify

- `rust/fontaine-sdk/src/core/permissions.rs`
- `docs/permissions.md`, `docs/hooks.md`
- `CHANGELOG.md`

## Trade-offs

- Running the hooks on the built calls was rejected: a hook's decision (allow, updated input) has
  no meaning for a shell command, and `updated_input` could not be applied back. Skipping the fast
  path hands the call to the mode (guardian in `auto`), which is strictly tighter.
- Skipping the fast path only when a hook names `read`/`ls`/`grep` keeps every other session on the fast path; within such a session the skip is coarse (all shell reads, whatever the hook's specifier).

## Testing Strategy

Unit tests in `permissions.rs` next to the existing shell pre-classifier tests (pipeline built with
a temp scope; no live APIs):

- A child agent with `disallowed: ["read(secrets/**)"]` that keeps `shell`: `cat secrets/key` is
  denied with rule `agent tools`; `cat src/a.txt` is still allowed at `Tier05`; the same restriction
  on the parent applies to a grandchild.
- An `only` restriction on `read` that does not cover the path denies `cat <path>`.
- A `pre_tool_use` hook with matcher `read(src/private/**)`: `cat src/private/x` no longer returns
  `Tier05` (it reaches the mode, e.g. the guardian in `auto`); with matchers `shell`, none, or
  `edit(**)` it still returns `Tier05`.
- `ls`/`grep` matchers likewise skip the fast path.
- With such a hook present, a non-covered path like `cat src/a.txt` also loses `Tier05`.
- Hook gate set, no secret operand, plain command: no panic, and not `Tier05`.
- Hook gate set with a secret operand: still handled by the secrets block (no panic).
- A deny `ls(...)` or `grep(...)` agent-type restriction denies the matching shell command.

## Open Questions

None.
