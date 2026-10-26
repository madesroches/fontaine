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
  `FONTAINE_DATA_DIR`. `agent/setup.rs:211-214` sets `protected_dirs = [config_dir, data_dir]`,
  and `floor::protected_path` (`floor.rs:71-94`) returns "Fontaine's own config or data
  directory" for any write/edit under them, ahead of the root-relative checks. A scratch dir under
  the data dir would therefore always hit the forced protected-path prompt.
- `owner_only::create_dir` requires the parent to exist (`CreateDirectoryW` / `DirBuilder` are not
  recursive); `open_lock` does not create the parent either.

## Design

### Location and lifetime

The scratch dir is `<scratch-parent>/<session-id>/`, deliberately outside the config and data
directories so the protected-path floor never sees it as Fontaine's own state. The parent is
resolved once, per platform:

- **Unix, `XDG_RUNTIME_DIR` set, absolute and an existing directory**:
  `$XDG_RUNTIME_DIR/fontaine/`.
- **Unix otherwise (macOS, no runtime dir)**: `<temp_dir>/fontaine-<uid>/`, the temp dir being
  `std::env::temp_dir()` (honours `TMPDIR`).
- **Windows**: `<temp_dir>\fontaine-<sid>\` (`%TEMP%` is already per-user), with `<sid>` from
  `owner_only::current_user_sid`.

The parent is created with `owner_only::create_dir` (0700 / current-user-only DACL; its own
parent exists in all three cases), treating `AlreadyExists` as success only after verifying the
existing entry: `symlink_metadata` says a real directory (not a symlink), on Unix owned by the
current user (uid taken from the owner of a file the process just created, to avoid a new
dependency) with no group/other mode bits. A parent that fails the check (a squatted `/tmp` name)
is an error, so the session runs without a scratch root. On Windows the per-user `%TEMP%` plus
the SID in the name stands in for that check, since the DACL is not inspected.

The session dir and `<id>.lock` are siblings in this parent. Keying on the session id means a
resumed session reuses its path. `/add-dir` extends the existing scope (`agent.rs:552-579`), so
the scratch root persists across it.

**Canonical and outside the protected dirs.** `create` canonicalizes the parent after creating it
(so macOS `/var` -> `/private/var` and a symlinked `TMPDIR` resolve) and returns the canonical
child path. It then refuses, as a creation error, any scratch path that is under either the raw
or the canonical form of the config or data dir (`FONTAINE_DATA_DIR` pointing into the temp dir
or the runtime dir), so the floor never classifies the scratch root as owned. `create` therefore
takes the protected dirs as an argument. Tests and sandboxes redirect the parent through
`XDG_RUNTIME_DIR` (the resolver takes an env closure, as `dirs::resolve` does); there is no new
config key or env var.

New module `rust/fontaine-core/src/scratch.rs`:

```
pub struct Scratch { dir: PathBuf, lock: File }   // Drop: best-effort remove dir + lock file
impl Scratch {
    pub fn create(env: &dyn Fn(&str) -> Option<OsString>, protected: &[PathBuf],
                  session_id: &str) -> io::Result<Scratch>;
    pub fn path(&self) -> &Path;                   // canonical
}
pub fn sweep_stale(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<String>; // warnings
```

- Beside each directory sits `<id>.lock`, held with an exclusive advisory lock for the life of
  the `Scratch`. The lock file is outside the root so the model cannot touch it.
- `file_lock.rs` has only a blocking `lock()` and `read_shared`, so a `try_lock` helper is added
  (`owner_only::open_lock` plus `File::try_lock`); `open_lock` does not create the parent, so
  `create` makes the verified owner-only scratch parent first.
- `create` takes the lock first, then creates the directory, then re-checks that the lock path
  still refers to the locked file (same file identity) and retries if a sweep unlinked it in
  between. `sweep_stale` tolerates a lock with no directory.
- `sweep_stale` runs at startup (before creating the new scratch) over the scratch parent (it
  does nothing if the parent does not exist): for each `<id>.lock` it tries
  to take the lock; if it succeeds, the owner is dead (crash, `kill -9`), so remove `<id>/` (if
  present) and the lock file. A held lock means a live session; it is skipped. This covers
  crash/kill without timestamps or PID checks.
- Cleanup on normal exit is `Drop`. The guard is a loop-iteration local in `fontaine/src/lib.rs` (not owned by
  `Session`, which is `Clone`), dropped after `run_headless`/`run_interactive` returns and before
  any early return, so it covers `/exit`, `/resume`, `/branch` and headless early returns.
  `Drop` closes the lock handle before deleting the lock file. `owner_only.rs:109-124` retries
  only a rename, so a new bounded retry is added for `remove_dir_all` (Windows file locks); a
  final failure is best-effort (no `Approver` is available at `Drop`).

### Wiring

- `prepare_session` creates the `Scratch` after `open_session` (the id is known there) and
  appends its canonical path to `roots.roots` as the last, non-primary root, so relative paths
  and the primary root are unchanged. It is returned alongside `SessionPrep` so the loop owns the guard.
- Everything downstream (Scope, per-root tool instances, prompt, guardian `workspace_roots`,
  subagents) picks it up from the roots list. **Subagents share the parent's scratch**: they use
  the same `Scope`, and the mode ladder already prevents a child from being looser than its
  parent, so no new rule is needed.
- Failure to create the directory, including a lock already held (e.g. `/resume` of a session
  another live process holds), is pushed to `prepare_session`'s `warnings: Vec<Notice>` and the
  session runs without a scratch root (the confinement is the old behavior, never a looser one).

### Prompt and denial text

- The system prompt gains a sentence naming the scratch path as the place for temporary and
  helper files, and the "only the user can add a root" sentence is reworded to cover it.
  `PromptParts` gets an optional `scratch: Option<PathBuf>`; `base` marks it. It is built at
  `agent/build.rs:126` and `agent/setup.rs:454`, and the 7 literals in
  `fontaine-core/tests/system_prompt.rs` gain the field. Added at session start, so no
  cache-boundary cost (a mid-session change would be one).
- The out-of-roots denial names the scratch dir alongside `/add-dir` when one exists
  (`permissions.rs:827`). `Scope` stays a plain root list; the path travels as a
  `scratch: Option<PathBuf>` field on `StateParams` (`gate.rs:98`) and `SessionState`
  (`gate.rs:264`), set from `AgentSetup` in
  `agent/setup.rs` (~:211), the same place the gate receives its scope, and `SessionPrep`
  carries it to `AgentSetup`.
- Error text only explains the Deny; it does not change it.

### Security properties (unchanged)

- No floor exception: `deny → ask → critical-path → protected-path` still gates every call, and
  `protected_dirs` is untouched. The scratch root is not under it by construction, so a
  `write`/`edit` there is not a protected-path hit; protected file names (`.bashrc`, ...) inside
  the root still are, as in any root.
- The fast path is still earned by Fontaine-enforced containment; the scratch root is enforced
  containment (private, 0700, the session owns it), unlike `/tmp`.
- Shell policy is unchanged, but the scratch path is an ordinary root there too: read-only
  commands whose operands are inside it take the pre-classifier fast path like any root
  (`preclassify.rs:552-559`, `:728-760`), and other commands go through the guardian. The path
  also appears in the guardian's `workspace_roots`, the hooks payload, the ledger
  `SessionStart.workspace_roots` and the headless `init` event. Removing the scratch root itself
  is already a critical-path prompt (`floor.rs` root-removal rule).
- Roots grant file access, not configuration discovery: no instruction files, MCP servers or
  hooks are discovered from the scratch dir. It is a permitted `@`-import location, as the
  primary root is (`instructions/mod.rs:100-113` confines imports to `scope.roots()`).

## Implementation Steps

1. `rust/fontaine-core/src/scratch.rs` (+ `pub mod scratch;` in `lib.rs`), the `try_lock` helper
   in `file_lock.rs`, the `remove_dir_all` retry in `owner_only.rs`: parent resolution and
   verification, `Scratch`, `sweep_stale`, unit tests.
2. `rust/fontaine/src/lib.rs`: call `sweep_stale` at startup, create `Scratch` in
   `prepare_session` (passing the protected dirs), append the root last, return the guard to the loop and drop it per
   iteration. Update tests that assert the root list: `fontaine/tests/headless_contract.rs`
   (`:484-487` `workspace_roots == [work]`, `:101-105` indexes `roots[1]`) and
   `fontaine/tests/headless_tools.rs:160` (`roots.len() == 2`).
3. `rust/fontaine-core/src/prompt.rs` + `prompts/system_prompt.md`: scratch sentence, reworded
   root-confinement sentence; `PromptParts` call sites in `agent/build.rs`, `agent/setup.rs` and
   `tests/system_prompt.rs`.
4. `rust/fontaine-core/src/gate.rs` + `agent/setup.rs` + `permissions.rs`: `StateParams` and
   `SessionState` carry the scratch path; denial text names the scratch dir. The other
   `StateParams` construction sites (`fontaine-evals/src/none.rs:117`,
   `fontaine-core/src/questions.rs:99`, `steering.rs:131`, `gate.rs:817`,
   `fontaine-core/tests/common/pipeline.rs:206`) are compile-driven updates.
5. Docs: `docs/tools.md` (Workspace roots; "Only the user adds roots"), `docs/permissions.md`
   step 3, `docs/configuration.md` Workspace roots, `docs/headless.md` (root list in the `init`
   event; `:83`, `:162`), `docs/hooks.md:59`, `docs/schemas/headless-v1.schema.json:76-90`
   (if they describe the root list), `docs/architecture.md`, `docs/tui.md` (`/add-dir` row, denial hint), and
   `/status` in `fontaine-tui/src/reports.rs:87-91`, which prints every non-primary root under
   "added roots:": filter the scratch root out of that list and show it labelled "scratch", and
   `tasks/design/permissions.md` and `tasks/design/config.md:12` ("Only the user adds roots at
   runtime"). `docs/index.md` only lists `/add-dir` and needs no change. Check
   `fontaine/tests/docs.rs:172+` still passes.
6. `tasks/fontaine_harness_plan.md` `## Decisions`: dated line (below). `CHANGELOG.md`
   `[Unreleased]` / `### Added` bullet with `(#238)`.

## Files to Modify

- `rust/fontaine-core/src/scratch.rs` (new), `rust/fontaine-core/src/lib.rs` (`pub mod scratch;`),
  `rust/fontaine-core/src/file_lock.rs`, `rust/fontaine-core/src/owner_only.rs`
- `rust/fontaine/src/lib.rs`, `rust/fontaine/tests/headless_contract.rs`,
  `rust/fontaine/tests/headless_tools.rs`
- `rust/fontaine-core/src/prompt.rs`, `rust/fontaine-core/prompts/system_prompt.md`,
  `rust/fontaine-core/src/agent/build.rs`, `rust/fontaine-core/src/agent/setup.rs`,
  `rust/fontaine-core/tests/system_prompt.rs`
- `rust/fontaine-core/src/gate.rs`, `rust/fontaine-core/src/permissions.rs`,
  `rust/fontaine-core/tests/permissions.rs`, `rust/fontaine-core/tests/workspace.rs`,
  `rust/fontaine-evals/src/none.rs`, `rust/fontaine-core/src/questions.rs`,
  `rust/fontaine-core/src/steering.rs`, `rust/fontaine-core/tests/common/pipeline.rs`
  (`StateParams` construction sites), `rust/fontaine-tui/src/reports.rs`
- `docs/tools.md`, `docs/permissions.md`, `docs/configuration.md`, `docs/headless.md`,
  `docs/architecture.md`, `docs/tui.md`, `docs/hooks.md`, `docs/schemas/headless-v1.schema.json`
- `tasks/fontaine_harness_plan.md`, `tasks/design/permissions.md`, `tasks/design/config.md`,
  `CHANGELOG.md`

## Trade-offs

- **`$XDG_RUNTIME_DIR` with an owner-only temp-dir fallback** (the issue's example; chosen): tmpfs
  and OS-managed cleanup where available, and a per-user owner-only directory under the OS temp
  dir elsewhere, at the cost of a per-platform parent rule and a squat check. It is chosen over
  `<data>/scratch/<id>` because `protected_dirs` covers the data dir, so a scratch root there
  would force the protected-path prompt on every write, and carving it out would be a floor
  exception exposing the sibling sessions, ledger and credentials. Crash cleanup is handled by
  the lock sweep either way.
- **Carve the scratch path out of `protected_dirs`**: rejected; a floor exception.
- **Allow `/tmp` or a `[permissions]` allowlist**: rejected in the issue (shared, world-writable,
  would let writes skip the guardian).
- **Scratch special-cased in the permission pipeline** instead of being a root: rejected; being
  an ordinary root means the floor needs no exception.
- **Per-subagent scratch dirs**: more isolation, but every child shares the parent's `Scope`
  today, so separate dirs would need per-child scopes for no security gain.

## Decisions

- 2026-10-26: Each session gets a private scratch directory,
  `$XDG_RUNTIME_DIR/fontaine/<session-id>/` (else `<temp>/fontaine-<uid>/<session-id>/`; on
  Windows `<temp>\fontaine-<sid>\<session-id>\`), added to the workspace roots automatically and removed on exit, so file-tool writes of helper
  files work without a prompt (Claude Code prompts for out-of-root writes and Fontaine denies
  them, 2026-10-22; this is a further deviation); reason: the denial protected nothing once the
  model wrote the file through `shell`, and a private owner-only root keeps the floor intact.
- AFK: The scratch dir lives outside the config and data dirs (`XDG_RUNTIME_DIR`, else an
  owner-only per-user dir under the OS temp dir, verified against squatting, canonicalized, and
  refused if under a protected dir) rather than under `<data>`, and the protected-path floor gets
  no exception; reason: `protected_dirs` covers the data dir, so `<data>/scratch` would defeat
  the feature or require a floor exception.
- 2026-10-26: Subagents share the parent's scratch directory; reason: one shared `Scope`, and a
  child's mode is already capped by its parent's.

## Documentation

Update `docs/tools.md`, `docs/permissions.md` and `docs/configuration.md` as listed above:
what the scratch root is, where it lives per platform, that it is removed on exit and swept after a crash,
that shell commands in it still go through the guardian, and the deviation from Claude Code.

## Testing Strategy

Unit tests (`rust/fontaine-core/src/scratch.rs`, temp data dir):
- `create` makes an owner-only (0700 on Unix) directory and returns a canonical path; with the
  parent reached through a symlinked `TMPDIR`, the returned path is the resolved one.
- Parent resolution: `XDG_RUNTIME_DIR` is used when set and valid, else the temp-dir fallback; an
  existing parent that is a symlink or has group/other bits is rejected.
- The scratch path is not under the config or data dirs, including when `FONTAINE_DATA_DIR`
  points into the temp dir or the runtime dir (then `create` errors rather than returning it).
- Drop removes the directory and lock file.
- `sweep_stale` removes a dir whose lock is free and skips one whose lock is held; a lock with no
  dir is removed without error.
- `create` racing a concurrent `sweep_stale` never ends with a live dir swept.
- `create` for an id whose directory already exists (resume after crash) succeeds.

Integration (`rust/fontaine-core/tests/permissions.rs`, `tests/workspace.rs`):
- Pipeline test (`tests/common/pipeline.rs`, with `protected_dirs` set as `agent/setup.rs` does):
  `write`/`edit` to a path in the scratch root is allowed (Tier is not `ProtectedPath`); a path in
  `/tmp` is denied and the message names the scratch dir and `/add-dir`.
- A protected file name (e.g. `.bashrc`) in the scratch root still hits the protected-path floor.
- The prompt names the scratch path and no longer claims only the user can add a root.
- Headless `fontaine -p` run leaves no directory under the scratch parent afterwards.
- Headless test (`Sandbox` in `fontaine/tests/common/mod.rs`, which sets `XDG_RUNTIME_DIR` to a
  sandbox dir) seeds a stale `<id>.lock` and `<id>/` under `$XDG_RUNTIME_DIR/fontaine/` and
  asserts the startup sweep removed them.
- Updated headless tests expect the scratch root appended last in `workspace_roots`.

## Open Questions

None.
