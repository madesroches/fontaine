# Per-session Scratch Root Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/238

## Overview

A file-tool call outside the workspace roots is denied with an `/add-dir` hint, so a model that
stages a helper script in `/tmp` is denied on `write` and then writes the same file through
`shell`. The confinement protects nothing there and pushes the model to a worse route. Each
session gets a private scratch directory (owner-only, removed on exit) that is added to the
workspace roots automatically. `write` and `edit` work there without a prompt, the system prompt
names the path, and the out-of-roots denial points at it. It is an ordinary root: the
deterministic floor, the guardian and the shell path are unchanged.

## Current State

- Roots are resolved once per session in `prepare_session` (`rust/fontaine/src/lib.rs:558-585`)
  by `workspace::resolve` (`rust/fontaine-core/src/workspace.rs`) from the launch directory,
  `--add-dir` and `permissions.additional_directories`. The first root is the primary root.
- `Scope` (`rust/fontaine-tools/src/scope.rs`) holds the roots; one `Arc<Scope>` is shared by the
  parent and every subagent, so a root added once applies to all agents. A child's mode is capped
  by its parent's independently (`rust/fontaine-agents/src/mode.rs`).
- The out-of-roots denial lives at `rust/fontaine-core/src/permissions.rs:822-840` and says
  "file tools work inside the workspace roots only (add one with /add-dir)". Shell is exempt: it
  is `Arbitrary` and goes to the pre-classifier, rules, then the mode/guardian.
- The system prompt lists the roots (`rust/fontaine-core/prompts/system_prompt.md:5-9`,
  `rust/fontaine-core/src/prompt.rs` `base`) and says only the user can add a root.
- `/resume` and `/branch` end a run and call `prepare_session` again in the same process
  (`lib.rs:308-375`), so per-session state is created once per loop iteration, not per process.
- `owner_only::create_dir` (`rust/fontaine-core/src/owner_only.rs:30`) creates a 0700 directory on
  Unix and a current-user-only DACL on Windows. `file_lock` provides advisory locks. There is no
  existing runtime/temp-dir code and no session-end hook.
- Dirs: `Dirs { config, data }` (`rust/fontaine-core/src/dirs.rs`), already overridable by
  `FONTAINE_DATA_DIR`.

## Design

### Location and lifetime

`<data>/scratch/<session-id>/`, created with `owner_only::create_dir` (parent created owner-only
the same way). One path rule on every platform; no `XDG_RUNTIME_DIR` branch, no `mkdtemp`
fallback, and Windows needs no special case beyond the existing DACL helper. Keying on the
session id means a resumed session reuses its path; an `/add-dir`-style rebuild needs nothing
because the scratch root is part of the launch set.

New module `rust/fontaine-core/src/scratch.rs`:

```
pub struct Scratch { dir: PathBuf, lock: File }   // Drop: best-effort remove dir + lock file
impl Scratch {
    pub fn create(data: &Path, session_id: &str) -> io::Result<Scratch>;
    pub fn path(&self) -> &Path;                   // canonical
}
pub fn sweep_stale(data: &Path) -> Vec<String>;    // returns warnings
```

- Beside each directory sits `<id>.lock`, held with an exclusive advisory lock for the life of
  the `Scratch`. The lock file is outside the root so the model cannot touch it.
- `sweep_stale` runs at startup (before creating the new scratch): for each `<id>.lock` it tries
  to take the lock; if it succeeds, the owner is dead (crash, `kill -9`), so remove `<id>/` and
  the lock file. A held lock means a live session; it is skipped. This covers crash/kill without
  timestamps or PID checks.
- Cleanup on normal exit is `Drop`. The guard is owned by the `main` loop iteration, not by
  `Session` (which is `Clone`), and dropped after `run_headless`/`run_interactive` returns, so it
  covers `/exit`, `/resume`, `/branch` and headless early returns. Removal failures (Windows
  file locks) retry as `owner_only` does, then surface once through `Approver::warn`-style
  notice rather than being swallowed.

### Wiring

- `prepare_session` creates the `Scratch` after `open_session` (the id is known there) and
  appends its canonical path to `roots.roots` as a non-primary root, so relative paths and the
  primary root are unchanged. It is returned alongside `SessionPrep` so the loop owns the guard.
- Everything downstream (Scope, per-root tool instances, prompt, guardian `workspace_roots`,
  subagents) picks it up from the roots list. **Subagents share the parent's scratch**: they use
  the same `Scope`, and the mode ladder already prevents a child from being looser than its
  parent, so no new rule is needed.
- Failure to create the directory is a warning, and the session runs without a scratch root
  (the confinement is the old behavior, never a looser one).

### Prompt and denial text

- The system prompt gains a sentence naming the scratch path as the place for temporary and
  helper files, and the "only the user can add a root" sentence is reworded to cover it.
  `PromptParts` gets an optional `scratch: Option<PathBuf>`; `base` marks it. Added at session
  start, so no cache-boundary cost (a mid-session change would be one).
- The out-of-roots denial names the scratch dir alongside `/add-dir` when one exists
  (`permissions.rs:827`); the pipeline learns the path from the same place it gets roots.
- Error text only explains the Deny; it does not change it.

### Security properties (unchanged)

- No floor exception: `deny → ask → critical-path → protected-path` still gates every call.
  Protected-path rules apply inside the scratch root as in any root.
- The fast path is still earned by Fontaine-enforced containment; the scratch root is enforced
  containment (private, 0700, the session owns it), unlike `/tmp`.
- Shell is untouched. Running a script from the scratch dir is a shell command and goes through
  the guardian like any other; root membership buys only the file-tool path. Removing the
  scratch root itself is already a critical-path prompt (`floor.rs` root-removal rule).
- Roots grant file access, not configuration: no instructions, MCP servers or hooks are loaded
  from the scratch dir.

## Implementation Steps

1. `rust/fontaine-core/src/scratch.rs` (+ `lib.rs` export): `Scratch`, `sweep_stale`, unit tests.
2. `rust/fontaine/src/lib.rs`: call `sweep_stale` at startup, create `Scratch` in
   `prepare_session`, append the root, return the guard to the loop and drop it per iteration.
3. `rust/fontaine-core/src/prompt.rs` + `prompts/system_prompt.md`: scratch sentence, reworded
   root-confinement sentence.
4. `rust/fontaine-core/src/permissions.rs`: denial text names the scratch dir.
5. Docs: `docs/tools.md` (Workspace roots; "Only the user adds roots"), `docs/permissions.md`
   step 3, `docs/configuration.md` Workspace roots, `docs/headless.md` if session files are listed.
6. `tasks/fontaine_harness_plan.md` `## Decisions`: dated line (below). `CHANGELOG.md`
   `[Unreleased]` / `### Added` bullet with `(#238)`.

## Files to Modify

- `rust/fontaine-core/src/scratch.rs` (new), `rust/fontaine-core/src/lib.rs`
- `rust/fontaine/src/lib.rs`
- `rust/fontaine-core/src/prompt.rs`, `rust/fontaine-core/prompts/system_prompt.md`
- `rust/fontaine-core/src/permissions.rs`, `rust/fontaine-core/tests/permissions.rs`
- `docs/tools.md`, `docs/permissions.md`, `docs/configuration.md`
- `tasks/fontaine_harness_plan.md`, `tasks/design/permissions.md`, `CHANGELOG.md`

## Trade-offs

- **`$XDG_RUNTIME_DIR` with `mkdtemp` fallback** (the issue's example) gives tmpfs and OS-managed
  cleanup, but needs two code paths plus a Windows answer. A directory under the data dir uses
  one path everywhere and the existing owner-only helper; crash cleanup is handled by the lock
  sweep either way.
- **Allow `/tmp` or a `[permissions]` allowlist**: rejected in the issue (shared, world-writable,
  would let writes skip the guardian).
- **Scratch special-cased in the permission pipeline** instead of being a root: rejected; being
  an ordinary root means the floor needs no exception.
- **Per-subagent scratch dirs**: more isolation, but every child shares the parent's `Scope`
  today, so separate dirs would need per-child scopes for no security gain.

## Decisions

- 2026-10-26: Each session gets a private scratch directory, `<data>/scratch/<session-id>/`,
  added to the workspace roots automatically and removed on exit, so file-tool writes of helper
  files work without a prompt (Claude Code prompts for out-of-root writes and Fontaine denies
  them, 2026-10-22; this is a further deviation); reason: the denial protected nothing once the
  model wrote the file through `shell`, and a private owner-only root keeps the floor intact.
- 2026-10-26: Subagents share the parent's scratch directory; reason: one shared `Scope`, and a
  child's mode is already capped by its parent's.

## Documentation

Update `docs/tools.md`, `docs/permissions.md` and `docs/configuration.md` as listed above:
what the scratch root is, where it lives, that it is removed on exit and swept after a crash,
that shell commands in it still go through the guardian, and the deviation from Claude Code.

## Testing Strategy

Unit tests (`rust/fontaine-core/src/scratch.rs`, temp data dir):
- `create` makes an owner-only (0700 on Unix) directory and returns a canonical path.
- Drop removes the directory and lock file.
- `sweep_stale` removes a dir whose lock is free and skips one whose lock is held.
- `create` for an id whose directory already exists (resume after crash) succeeds.

Integration (`rust/fontaine-core/tests/permissions.rs`, `tests/workspace.rs`):
- `write`/`edit` to a path in the scratch root is allowed; a path in `/tmp` is denied and the
  message names the scratch dir and `/add-dir`.
- A protected file name (e.g. `.bashrc`) in the scratch root still hits the protected-path floor.
- The prompt names the scratch path and no longer claims only the user can add a root.
- Headless `fontaine -p` run leaves no directory under `<data>/scratch/` afterwards.

## Open Questions

None the code cannot settle; the Windows path follows from using the data dir and the existing
DACL helper.
