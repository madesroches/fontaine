# Shared-Lock Reads of Persisted Files Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/200

## Overview

On Windows, a reader that opens a file while another thread or process renames a temp file over it
can get `NotFound` or `PermissionDenied` for a few milliseconds instead of the old or new content.
`owner_only::write_atomic` promises "a reader sees the old or the new content", and CI's
`concurrent_atomic_writes_to_one_path_leave_one_complete_file` fails on `windows-latest` because of
it. The bug matters in production: `config::load` maps `NotFound` on the user `config.toml` to "no
settings", so a session starting while another session writes an "always allow" rule silently runs
without the user's deny rules. No rename flavor closes the window. This plan makes readers of the
lock-guarded files take a **shared** lock on the sibling `.lock` file that every writer already holds
exclusively across read, edit and replace, so a reader can never observe a replace in progress.

## Current State

- `fontaine-core/src/file_lock.rs`: `lock(target)` takes the exclusive `File::lock` on
  `target.with_extension("lock")` (opened with `owner_only::open_lock`, parent created first).
- Writers that hold it across read → edit → `write_atomic*`: `trust::record`, `trust::approve_server`
  (`trust.rs:148`, `:190`), the four `user_config` mutators (`user_config.rs:180` `locked`),
  `credentials::write_entry`/`remove`/`store_oauth*` and `TokenSource::refresh_serialized`
  (`providers/token_source.rs:226`).
- Readers that take no lock and race those writers:
  - `config::load` → `read_optional(&user_path)` (`config.rs:1657`, `:1940`). The project and local
    settings files are never written by Fontaine; the `--settings` file (`:1687`) neither.
  - `trust::load` (`trust.rs:64`) behind `read_file` → `is_trusted`, `server_approved`, `unreadable`.
  - `user_config::model_key_naming` (`user_config.rs:146`) via `read_document` (which maps
    `NotFound` to an empty document, `:184`), called from `accounts::remove` (`accounts.rs:314`)
    before `remove_account` takes the lock (`:319`): a replace during that read lets the "account in
    use by a model" guard pass wrongly.
  - `Credentials::load` (`credentials.rs:100`), called from `config.rs:1910`, `accounts.rs:140`,
    `models.rs:33`, `fontaine/src/telemetry/mod.rs:145`, and `token_source.rs:235`.
- Two readers run **while their caller holds the exclusive lock** in the same process:
  `approve_server` calls `server_approved` after `lock` (`trust.rs:148-151`), and
  `refresh_serialized` calls `Credentials::load` while holding `credentials::lock`. A shared lock
  taken through a second handle would deadlock there (`flock` and `LockFileEx` conflict across
  handles even within one process).
- `owner_only::replace` (`owner_only.rs:57`) renames with `std::fs::rename`, retrying 5 × 20 ms on
  `PermissionDenied` on Windows. That handles the target being held open by outside programs
  (editor, scanner) and stays as is.
- The failing test (`fontaine-core/tests/owner_only.rs:104`) runs 8 unlocked writers × 20 against an
  unlocked reader looping `read_to_string`. Local reproduction on NTFS: the reader misses the file
  for 1–28 ms in ~0.3–2% of runs, mostly `NotFound`; a hand-rolled
  `SetFileInformationByHandle(FileRenameInfoEx, POSIX_SEMANTICS | REPLACE_IF_EXISTS)` rename did not
  remove the gaps.

## Design

### `file_lock::read_shared` (`fontaine-core/src/file_lock.rs`)

```rust
/// Reads `target` while holding the shared lock beside it, so a session replacing the file under
/// the exclusive lock is waited for instead of observed mid-rename.
pub fn read_shared(target: &Path) -> io::Result<String>
```

1. Lock path is `target.with_extension("lock")`, the same derivation as `lock` (one private helper
   for both).
2. If the lock's parent directory does not exist, the target cannot exist: return the plain
   `std::fs::read_to_string(target)` result (`NotFound`) without creating anything. A read never
   creates directories.
3. Open the lock with `owner_only::open_lock` (creates the empty `.lock` if absent, as the first
   edit already does). If the open fails with `PermissionDenied` or `ReadOnlyFilesystem`, read
   without the lock: a directory this process cannot create a file in is also one its writers
   cannot replace a file in, and a read-only mounted config must keep loading.
4. `file.lock_shared()`; if it fails with `Unsupported` or an uncategorized error (ENOLCK on NFS),
   read without the lock as in step 3, since today's reads never touch the lock. Otherwise
   `std::fs::read_to_string(target)`, then release on drop (reuse
   `FileLock` so unlock-on-drop lives in one place).

Errors propagate unchanged, so callers keep mapping `NotFound` to "absent".

### Rule: an exclusive holder never calls a shared reader

Each guarded module gets an unlocked inner read for use under its own exclusive lock, and the public
reader wraps it with `read_shared`:

- **Trust** (`trust.rs`): `load` reads through `file_lock::read_shared`. `approve_server` drops its
  `server_approved` early-out under the lock and instead checks the `TrustFile` it already parses
  from the text it reads under the lock (the same shape as `record`'s `parsed.trusted` check).
- **Credentials** (`credentials.rs`): `Credentials::load` reads through `read_shared`. Add
  `Credentials::load_locked(data_dir, &Lock)`, the proof-of-holding variant matching
  `store_oauth_locked`, which reads plainly; `refresh_serialized` uses it. A shared private
  `from_text(path, io::Result<String>)` keeps the `NotFound` → empty and parse mapping in one place.
- **Settings** (`config.rs`): the user layer reads through `read_shared`; project, local and
  `--settings` files keep the plain read. `read_optional` takes the read function
  (`fn(&Path) -> io::Result<String>`) so the `NotFound` → `None` mapping is not duplicated.
  `user_config`'s mutators already read plainly under the exclusive lock and are unchanged;
  `model_key_naming`, which runs outside any lock, reads through `read_shared`.

### `write_atomic`'s contract (`owner_only.rs`)

The doc comment is narrowed to what holds: a reader holding the file's shared lock
(`file_lock::read_shared`) sees the old or the new content; an unlocked reader on Windows can
briefly find the file missing or access denied. No rename change.

## Implementation Steps

1. `file_lock.rs`: private lock-path helper; `read_shared` as above.
2. `trust.rs`: `load` via `read_shared`; `approve_server` early-out from the locked parse.
3. `credentials.rs`: `from_text`, `Credentials::load` via `read_shared`, `load_locked`;
   `token_source.rs:235` calls `load_locked(&self.data_dir, &file_lock)`.
4. `config.rs`: `read_optional` takes the reader; user layer passes `file_lock::read_shared`.
5. `user_config.rs`: `model_key_naming` reads via `file_lock::read_shared`.
6. `owner_only.rs`: doc comment on `write_atomic`.
7. Tests (below), `docs/configuration.md`.

## Files to Modify

- `rust/fontaine-core/src/file_lock.rs`, `trust.rs`, `credentials.rs`, `config.rs`,
  `user_config.rs`, `owner_only.rs`, `providers/token_source.rs`
- `rust/fontaine-core/tests/owner_only.rs` (and a `#[cfg(test)] mod tests` in `file_lock.rs`)
- `docs/configuration.md`

## Trade-offs

- **Shared-lock readers vs. a reader-side retry (Go `robustio` style).** A retry must also retry
  `NotFound`, which makes every read of a genuinely missing file (common at startup) wait out the
  budget, and the budget is a guess (28 ms gaps were observed locally). The lock closes the window
  by construction and reuses the lock writers already take.
- **vs. a stronger rename (POSIX-semantics `FileRenameInfoEx`).** Tried locally; readers still saw
  `NotFound` gaps. Rejected.
- **vs. in-place write under the lock (Go `lockedfile`).** Loses crash safety: a crash or full disk
  mid-write leaves a torn `trust.toml`, which the atomic replace was introduced to prevent.
- **What other harnesses do.** Claude Code and Gemini CLI read without a lock (Claude Code retries
  non-`ENOENT` errors and treats `ENOENT` as absent; its Windows issue tracker has reports of a
  session overwriting `~/.claude.json` after seeing it missing). Codex's `history.jsonl` readers take
  a shared lock. Internal persistence only; no user-visible Claude Code semantics change.

## Decisions

- 2026-10-25: the TUI's `recent.json` and `history.jsonl` keep unlocked reads: best-effort files
  with no cross-process writer lock, where a missed read only empties a list for one session.
- 2026-10-25: a read creates the sibling `.lock` file when its directory exists; the files are
  already documented and created on the first edit.

## Documentation

`docs/configuration.md`: the lock-file row (line 32) and the trust/credentials paragraphs (~780,
~830) say sessions also take the lock briefly when reading these files, so a read waits for an edit
in progress instead of seeing it half-done.

## Testing Strategy

The bug is a real concurrency failure seen in CI, so it is pinned at the tier that reproduced it:
real threads on the real filesystem, run on all three OSes.

- `tests/owner_only.rs`: `concurrent_atomic_writes_to_one_path_leave_one_complete_file` keeps its
  original purpose (unique temp names; one complete file, no `.tmp` leftovers) with unlocked writers,
  and keeps a light unlocked reader that tolerates `NotFound`/`PermissionDenied` (asserting content
  only on success) while every `write_atomic` is still `unwrap()`ed: the regression test that a
  replace succeeds while an unlocked reader holds the target open. A new `a_shared_lock_reader_never_sees_a_replace_in_progress` runs 4 writers × 10,
  each under `file_lock::lock` + `write_atomic`, against a reader looping `read_shared` that asserts
  every read succeeds with complete content. With `read_shared` reading unlocked this test fails on
  Windows as the old one did.
- `file_lock.rs` unit tests:
  - a reader blocks while another handle holds the exclusive lock (pattern of
    `tests/credentials_oauth.rs:97-118`): the main thread holds `lock`, a spawned thread calls
    `read_shared`, sleep, `assert!(!handle.is_finished())`, then `write_atomic` new content, drop the
    lock, join, and assert the reader returned the new content;
  - `lock_shared()` failure falls back to an unlocked read (unit test if a failing lock can be
    constructed portably; it cannot be on most filesystems, so this branch is left to review);
  - missing parent directory → `NotFound`, and the directory is not created;
  - existing directory, missing file → `NotFound`;
  - `#[cfg(unix)]`: a read-only directory holding the file (mode 0555, no `.lock` present) still
    reads it.
- No-deadlock coverage: the existing `approve_server` tests and `token_source` refresh tests would
  hang if an exclusive holder took the shared lock; they run unchanged. The existing approve-twice test
  (`tests/project_origins.rs:135-139`, "Approving twice writes it once") covers the moved early-out.

## Open Questions

None.
