# Atomic Replace and Locked Edit Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/183

## Overview

Fontaine rewrites six persistent files with three different "replace a file" implementations, and
the two that are read-modify-write (`trust.toml`, the user `config.toml`) take no cross-process
lock. A crash truncates `trust.toml`, and two sessions can drop each other's edits, including a
hand-added `[permissions] deny` rule. This plan makes `owner_only::write_atomic` the single
atomic-replace implementation, adds one lock helper generalized from `credentials::lock`, takes it
around every read-modify-write of `trust.toml` and the settings file, and makes an unparseable
`trust.toml` visible at session start.

## Current State

| File | Writer | Atomic | Cross-process lock |
|---|---|---|---|
| `trust.toml` | `trust.rs` `approve_server`, `record`: `owner_only::write` (truncates in place) | no | no |
| user `config.toml` | `user_config.rs` `write_atomically` (`NamedTempFile::persist`, resolves symlinks, keeps the target's permissions) behind `append_allow_rule`, `set_string` (model/guardian/theme), `add_account`, `remove_account` | yes | no |
| `credentials.toml` | `owner_only::write_atomic` (`.<name>.<pid>.tmp`, `rename`) | yes | yes, `credentials::lock` |
| `todos.json` | `owner_only::write_atomic` | yes | in-process `Mutex` (`todos.rs:19`, justified by the per-process temp name) |
| `recent.json` | `owner_only::write_atomic` (`fontaine-tui/src/recent.rs`) | yes | no (best-effort file) |
| `history.jsonl` rewrite | `history.rs` `rewrite`: own `<pid>.tmp`, `owner_only::write`, `rename`, no `sync_all` | partly | no |
| session copy (`copy_tree`) | `session.rs` `owner_only::write` | no | n/a (fresh directory); unchanged |

`read_file` in `trust.rs` returns `None` for an unreadable or unparseable file, so `is_trusted` and
`server_approved` answer "no" with no message. The only callers of `is_trusted` outside tests are
`fontaine/src/lib.rs` (interactive prompt), `fontaine/src/headless.rs` (the `init` event) and
`agent.rs` (`SessionStart`). On a corrupt file the interactive prompt path already fails closed
with a `ConfigError::Parse` naming the file when the user answers "trust"; the "do not trust" path
and every headless run say nothing.

`credentials::lock` opens `<data dir>/credentials.lock` with `owner_only::open_lock` and holds
`File::lock` until drop. The sibling-name convention (`credentials.toml` → `credentials.lock`)
generalizes by swapping the extension.

## Design

### One atomic replace (`fontaine-core/src/owner_only.rs`)

`write_atomic(path, contents)` stays the entry point for owner-only content. It and the settings
file go through one private core:

```
enum Access { OwnerOnly, KeepExisting }

fn replace(path, access, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()>
pub fn write_atomic(path, contents)               // replace(.., OwnerOnly, write_all)
pub fn write_atomic_keeping_mode(path, contents)  // replace(.., KeepExisting, write_all)
```

`replace`:

1. If `path` exists, canonicalize it so a symlink's target is replaced and the link survives.
2. Reserve a unique temp name in the target's directory with `tempfile::Builder`
   (`.<name>.` prefix, `.tmp` suffix; `tempfile` is already a non-dev dependency of
   `fontaine-core`), keeping only the `TempPath` so the file is deleted on any early return. A
   unique name per call is what lets two processes (or two threads) replace the same file without a
   shared mutex.
3. `OwnerOnly`: reopen the temp path with `owner_only::open(.., false)`, which applies mode 0600 or
   the protected current-user DACL before any content is written. `KeepExisting`: open it plainly
   and copy the target's permissions onto it when the target exists (today's settings behavior; a
   newly created settings file is 0600 on Unix through `tempfile`'s own mode and inherits the
   directory ACL on Windows, as now).
4. Run `write`, `flush`, `sync_all`, drop the handle, then `TempPath::persist` onto the target. On
   Windows, a rename onto a file another handle holds open fails with `PermissionDenied`; retry a
   few short sleeps (5 × 20 ms) before surfacing it. Unix does not retry. This answers the issue's
   requirement of a rename that is safe on Windows.

`replace` taking a writer closure is the injection point for the interrupted-write test; it is not
public.

Callers moved onto it: `credentials` (unchanged call), `todos`, `recent` (unchanged),
`trust` (`write_atomic`), `user_config` (`write_atomic_keeping_mode`, `write_atomically` deleted),
`history::rewrite` (`write_atomic`, own temp name deleted). `owner_only::write` stays for its remaining callers (new files and the lock open).
`SessionTodos` keeps its mutex only to order concurrent in-memory updates with their saves; its
comment about the temp name is replaced.

### One lock helper (`fontaine-core/src/file_lock.rs`, new)

```
pub struct FileLock(File);                       // unlocks on drop, like credentials::Lock
pub fn lock(target: &Path) -> io::Result<FileLock>
```

`lock` takes the exclusive `File::lock` on `target.with_extension("lock")`, opened with
`owner_only::open_lock`, creating the parent directory first. It blocks until held. The lock path
is derived from the symlink-resolved target when the target exists, so two paths to one file share
a lock. `credentials::lock` becomes a thin wrapper over it (keeping its `ConfigError` mapping and
the `Lock` type that `store_oauth_locked` takes as proof of holding it); the resulting lock file is
still `credentials.lock`, so a running older build and a new one exclude each other.

Callers hold the guard across read → edit → `write_atomic*`:

- `trust::record` and `trust::approve_server`: take the lock before the first read; lock errors map
  to `ConfigError::Write`, as in `credentials::lock`. The
  `server_approved` early-out in `approve_server` moves inside the lock so the decision uses the
  latest content.
- `user_config`: `append_allow_rule`, `set_string`, `add_account`, `remove_account` each take the
  lock before reading. A private `locked(path)` wrapper in `user_config.rs` ensures the parent
  directory exists and acquires the guard, so the four functions do not repeat it. The read-only
  `model_key_naming` takes none.

### Visible corrupt trust store

`trust::unreadable(data_dir) -> Option<String>`: `None` for a missing or valid file; otherwise a
one-line message naming `trust.toml`, the cause chain (`error_chain::chain`), and the effect
("no workspace is trusted and no MCP server approved until it is fixed or removed").
`agent.rs` calls it in `record_session_start`, next to the `trust::is_trusted` call, and passes the
message to `Approver::warn` once per session. `unreadable` and `read_file` share a new
Result-returning read helper (today `read_file` discards the parse error).

## Implementation Steps

1. `owner_only.rs`: add `Access`, private `replace`, `write_atomic_keeping_mode`; reimplement
   `write_atomic` on `replace`; Windows rename retry.
2. `file_lock.rs`: new module, export from `lib.rs`; make `credentials::lock` delegate.
3. `trust.rs`: lock + `write_atomic` in `record` and `approve_server`; add `unreadable`.
4. `user_config.rs`: `locked` wrapper in the four mutating functions; replace `write_atomically`.
5. `todos.rs` comment; `fontaine-tui/src/history.rs` `rewrite` (and its test assertion at
   ~247-251: scan the directory and require that only `history.jsonl` remains, replacing the
   check for the old `jsonl.<pid>.tmp` name).
6. `agent.rs`: warn once at session start, in `record_session_start`.
7. Tests (below), docs, changelog.

## Files to Modify

- `rust/fontaine-core/src/owner_only.rs`, `file_lock.rs` (new), `lib.rs`, `credentials.rs`,
  `trust.rs`, `user_config.rs`, `todos.rs`, `agent.rs` (the interrupted-write test lives in a new
  `#[cfg(test)] mod tests` in `owner_only.rs`)
- `rust/fontaine-tui/src/history.rs`
- `rust/fontaine-core/tests/owner_only.rs`, `tests/trust.rs`, `tests/ledger.rs`
- `docs/configuration.md`, `CHANGELOG.md`

## Trade-offs

- **Lock inside a closure-style `update(path, f)` helper vs. a guard.** The guard mirrors
  `credentials::lock`/`Lock` and lets `trust` keep its `ConfigError::{Read,Parse,Write}` mapping
  without a generic error wrapper.
- **One `write_atomic` with an `Access` switch vs. two implementations.** The two differ only in
  permission policy (tighten to owner-only, or keep the user's file mode); everything else (unique
  temp name, fsync, symlink, rename) is shared, which is the point of the issue.
- **Helper in `fontaine-core` vs. `fontaine-tools`.** The TUI already depends on `fontaine-core`
  and imports `owner_only`, so nothing needs the helper lower down.

## Decisions

- 2026-10-25: the settings file keeps its existing mode on replace (`KeepExisting`) rather than
  being forced owner-only: it is the user's own document and may be deliberately group-readable.
- 2026-10-25: the corrupt-trust warning is raised at session start only. The pre-prompt path
  already fails with a `ConfigError::Parse` naming the file if the user answers "trust", and the
  TUI has no `Approver` before the session exists.
- 2026-10-25: `session::copy_tree` stays on `owner_only::write` because it writes only into a
  freshly created directory.

## Documentation

`docs/configuration.md`: in Workspace trust, say that an unparseable `trust.toml` is reported once
at session start; list the new lock files in the "Files in these directories" table; note the sibling `.lock` files (`trust.lock`, next to the settings file
`config.lock`, `credentials.lock`) that serialize edits between sessions. `CHANGELOG.md`
Unreleased: atomic, locked trust and settings writes; corrupt-trust warning.

## Testing Strategy

Unit and integration tests, no live APIs:

- `src/owner_only.rs` (`#[cfg(test)] mod tests`, since `replace` is private): an injected writer
  that writes half and errors leaves the previous content intact and no `.tmp` leftover (the
  interrupted-write criterion).
- `tests/owner_only.rs` (public API): unique temp names under two concurrent `write_atomic` calls
  to one path; a symlinked target stays a symlink and `write_atomic_keeping_mode` preserves a 0644
  mode while `write_atomic` yields 0600 (Unix); existing atomic test still passes on Windows DACL
  check.
- `trust`: two threads calling `record` for different roots each through
  its own lock handle both land; same for `approve_server`; `unreadable` returns `None` for
  missing and valid files and a message containing `trust.toml` for a corrupt one.
- `user_config` (unit `mod tests` in `src/user_config.rs`): two threads calling `append_allow_rule` with different rules both land; existing
  symlink and comment-preservation tests keep passing.
- `tests/ledger.rs`: `record_session_start` over a corrupt `trust.toml` calls `Approver::warn` exactly once
  (recording approver), naming the file.
- `history`: existing rewrite test; the residue assertion scans the directory and requires that
  only `history.jsonl` remains.

The concurrency tests use threads with separate lock handles as the stand-in for processes, since
`flock`/`LockFileEx` conflict across open file descriptions even within one process. CI runs them on
Linux, macOS and Windows.

## Open Questions

None.
