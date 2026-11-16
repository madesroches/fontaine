# Auto-update Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/175

## Overview


Fontaine updates itself without a command or a prompt. When the interactive terminal UI starts, a
background pass (at most once a day) looks for a newer build on the install's own channel,
downloads it, verifies its minisign signature against the key embedded in the running binary, and
makes it the binary the next launch runs. The next launch shows one `Fontaine updated to vX.Y.Z`
notice. Only installs made by the Fontaine installers (a receipt that matches the running
executable) take part; every other install stays silent. `fontaine update` stays as the manual
check and repair path and shares the same download, verification and install code.

## Current State

- `fontaine update` (`rust/fontaine/src/update/`): `mod.rs::update()` locates the receipt
  (`receipt::locate`, the receipt must name the running executable), takes the receipt lock
  (`file_lock::lock`), asks the release index for the latest build of the receipt's channel
  (`release_index::latest`), compares builds (`release_index::compare`, never a downgrade),
  downloads the archive and its `.minisig` (`download`), verifies with
  `release_signature::verify_file` plus `check_binding` (signed channel, file and version must equal
  what the index offered), and replaces the binary (`install::replace_binary`: atomic persist on
  Unix; on Windows the running exe is renamed to `fontaine.exe.old[-pid]` and removed by the next
  `fontaine update`), then rewrites the receipt version. All of it is one function today, so a
  background caller cannot reuse the stages separately.
- Channel comes from the receipt version (`receipt::channel_of`), not config; update never
  switches channel (recorded 2026-11-08, #509).
- `release_signature.rs`: `TrustedKeys::embedded()` holds the `release` and `dev` public keys.
- Plugin auto-update is the pattern to copy: `plugin_auto_update.rs` (`spawn` on a plain thread,
  `stamp_due`/`write_stamp` with `plugin-update.stamp`, `append_log` to `plugin-updates.jsonl`,
  `Notice` feed from `fontaine_tui::notice_feed()`), started in `lib.rs` only when
  `headless.is_none() && config.plugins.auto_update`. The opt-out key is `[plugins] auto_update`,
  user scope only (`config_scope.rs` `Rule::user_only`, `config_catalog.rs`, `config.rs`).
- Network: `Config::loaded_network` and `update::build_client` (proxy, CA bundle, mTLS).
- Docs: `docs/installing.md` (update section, channel rules, exit codes), `docs/configuration.md`
  (`[plugins]` table and the user-only matrix).

## Design

### Flow

```
TUI start (not -p, not send, receipt install, [updates] auto_update)
  └─ std::thread: background pass
       stamp due? (<data>/update.stamp)   no → stop
       try_lock <data>/update.lock   (held elsewhere → stop); write stamp
       receipt::locate        (fails → silent stop: package manager, moved binary, no receipt)
       release_index::latest  → newer than installed? no → stop
       stage: download archive+sig → verify_file + check_binding   (reuses update())
       Unix:    take the receipt lock, re-compare, replace_binary (atomic persist), write receipt, write marker
       Windows: leave staged dir, start the helper (below)
next launch: marker present and receipt version (receipt::locate) == marker.to → one Notice::info, delete marker
```

### Code shape (DRY)

Split `update::update()` into stages that both callers use, in `update/mod.rs`:

- `prepare(client, params) -> Result<Prepare, UpdateError>`, where `Prepare` is `UpToDate` or
  `Ready(Prepared)`: locate, compare, download,
  verify, bind, holding no receipt lock. `Prepared` holds `InstallInfo`, the verified archive path
  (in a work dir created beside the binary) and `Verified`.
- `install(prepared) -> Outcome`: takes the receipt lock, re-reads the receipt and re-compares
  versions, then `install::replace_binary` and the receipt rewrite.
- `update()` = `prepare` + `install` (behavior unchanged, still returns `Outcome::UpToDate` for
  `Prepare::UpToDate`; its tests keep passing).

New `update/auto.rs` owns only the background policy: the cadence, the lock, the marker, the log,
the notices, the Windows hand-off. Move `stamp_due`/`write_stamp` out of `plugin_auto_update.rs`
into a shared `stamp.rs` that takes the file name, and have both passes call it.

### Config

`[updates] auto_update` (bool, default `true`), user scope only: `Rule::user_only("updates.auto_update", …)`,
a catalog row, the `config.rs` section and overlay, rows in the `docs/configuration.md` matrix and a
`[updates]` section, a paragraph in `docs/installing.md`. Project scope and `--settings` are rejected
like `plugins.auto_update`. Off disables the check and the install; `fontaine update` still works.

### Gating

The pass starts only when `headless.is_none()` (`-p`, `fontaine send` and the other non-interactive
surfaces never reach it), `config.updates.auto_update`, and `receipt::locate` succeeds for
`std::env::current_exe()`. A `cargo install`, package-manager, container, moved or receipt-less
binary stops silently. No guardian shortcut is involved and no permission is loosened: the pass
runs no tool call and gets no new capability.

### Concurrency and atomicity

- One pass per machine: `file_lock::try_lock(<data>/update.lock)`; a loser stops silently. The
  stamp is written under that lock before any network call, so an offline machine retries the next
  day and two simultaneous launches stage once.
- The receipt lock (as `fontaine update` uses) is taken only around compare + install + receipt
  write, with a re-read of the receipt and a re-compare under it, so a manual update running at
  the same time cannot be undone or doubled. `prepare` (download/verify) holds no receipt lock, so
  a manual update never waits for a background download.
- Download goes to a `.fontaine-update-*` temp dir beside the binary and `replace_binary` persists
  with an atomic rename; a half-written file is never at the target path. A leftover temp dir from
  a crashed pass is removed at the start of the next pass.

### Linux and macOS

The verified binary replaces the file with an atomic rename, so running sessions keep the old
inode and are not interrupted; the next launch executes the new binary. The signature is checked
once, in the same process that installs (nothing persists unverified across launches). The previous
binary is kept as `<exe>.prev` (hard link taken before the rename) for rollback.

### Windows

The running executable is locked, so the session never replaces itself. After verification the pass
keeps the verified archive, its `.minisig` and a manifest (`channel`, `version`, `sha256`) in
`<data>/update-staged/<version>/` (copied from the work dir, not renamed: it may be on another
filesystem; the manifest is written to a temp name and renamed), then starts a detached
helper and returns. The hand-off holds no receipt lock.

- **Helper** = a per-spawn copy of the currently running, already trusted `fontaine.exe`
  (`<data>/update-helper/fontaine-helper-<pid>.exe`) started with a hidden internal subcommand. The
  helper is therefore never the swap target; the next pass deletes stale copies (best effort, a
  running one is skipped).
- **One helper:** `try_lock(<data>/update-helper.lock)`; a second helper exits at once.
- **Wait:** the helper polls (every 2 s) whether the target can be opened for exclusive write, i.e.
  no Fontaine process runs from it. It gives up after 24 h and leaves the stage for the next
  launch's pass to retry. It never relaunches Fontaine and shows no console window.
- **Swap:** when free, under the receipt lock it extracts the staged archive into a fresh
  `.fontaine-update-*` work dir beside the binary, re-verifies the staged archive with
  `verify_file` + `check_binding` against the embedded key (the stage persisted across launches),
  re-compares versions (a newer build installed meanwhile wins), calls `replace_binary`, rewrites the
  receipt, writes the marker, removes the stage. Per-user install dir, so no UAC.
- **Interruption:** the stage stays until applied; a killed helper is replaced by the next launch's
  pass (stamp permitting, plus a check at every launch for an existing stage with no live helper,
  which needs no network).
- `install::replace_binary` on Windows always renames the target aside to `<exe>.old[-pid]`, for
  the helper swap too. `install::remove_stale` is called at the start of the helper swap (Unix:
  n/a), so auto-updates clean up `.old*` leftovers; `update()` keeps calling it.

### Rollback

No automatic rollback in this release (a binary that fails to start cannot report itself). The
one-generation `<exe>.prev` is kept and `fontaine update --rollback` restores it under the receipt
lock, rewrites the receipt version, and records the rolled-back build in `<data>/update-skip` so
auto-update does not reinstall that same build; a still newer build clears the skip. `fontaine
update` (manual) ignores the skip.

### Failure reporting

Outcomes go to `<data>/updates.jsonl`, one line per check or update (`timestamp`, `channel`,
`installed`, `candidate`, `outcome`, `reason`), mirroring `plugin-updates.jsonl`. Classification
reuses `UpdateError`: `Network` is logged and quiet; `Verify` (including a missing or wrong
signature, wrong channel or version), `Install` and a failed swap are logged and shown once as a
`Notice::warning` through the existing notice feed. "Once" is enforced by remembering the
`(candidate, reason-kind)` last warned about in `<data>/update-state.json`. The installed binary
and receipt are untouched on every failure.

### Notice

The marker `<data>/update-applied.json` (`from`, `to`, `channel`) is written after a successful
swap. An interactive start whose receipt version equals `to` shows one
`Fontaine updated to vX.Y.Z` (`dev.N (…)` text reuses `Outcome::report` formatting for dev) and
deletes the marker. The comparison uses the version in the receipt for the running executable
(`receipt::locate`), not `VERSION` (on dev `<pkg>-dev+<sha12>`, never equal to the signed
`<base>-dev.<N>+<sha7>`); a marker whose `to` does not match is deleted silently.

## Implementation Steps

1. Refactor `update/mod.rs` into `prepare`/`install`; keep `update()` and its tests green.
2. Extract `stamp.rs` from `plugin_auto_update.rs`; switch the plugin pass to it.
3. Add `[updates] auto_update` (config.rs, config_scope.rs, config_catalog.rs) with tests.
4. Add `update/auto.rs`: pass, lock, state, log, marker, notices; wire `spawn` in `lib.rs` next to
   the plugin pass and the marker read before the TUI starts.
5. Unix: keep `<exe>.prev` in `install::replace_binary`; add `fontaine update --rollback` and the
   skip file (cli.rs, update/mod.rs).
6. Windows: staged dir, hidden helper subcommand (cli.rs, `update/helper.rs`), lock wait, re-verify
   and swap.
7. Tests (below), docs, CHANGELOG, `## Decisions` lines in the master plan.
8. CI: a manually triggered / nightly workflow `update-e2e.yml` on ubuntu, macos and windows that
   installs the previous dev build with the real installer, publishes nothing, points the updater at
   the current dev feed, launches the TUI under a pty (Unix) or via the hidden pass entry point
   (Windows), and asserts the version changed and the receipt is intact. Coordinate with #176.

## Files to Modify

- `rust/fontaine/src/update/mod.rs`, `install.rs`, new `auto.rs`, `helper.rs`
- `rust/fontaine/src/stamp.rs` (new), `plugin_auto_update.rs`, `lib.rs`, `cli.rs`
- `rust/fontaine-core/src/config.rs`, `config_scope.rs`, `config_catalog.rs`
- `rust/fontaine/tests/update_cmd.rs`, new `update_auto.rs`; core `tests/config.rs`
- `docs/installing.md`, `docs/configuration.md` (incl. the data-file table), `docs/tui.md`, `docs/headless.md`
- `.github/workflows/update-e2e.yml` (new), `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Decisions

- 2026-11-15 (#175): Background auto-update reuses the minimal updater (no axoupdater); channel stays receipt-derived with no config key, so an update is never a channel switch. Reason: one verification path and the 2026-11-08 decision.
- 2026-11-15 (#175): No automatic rollback; one `<exe>.prev` generation plus `fontaine update --rollback` and a skip record. Reason: a binary that cannot start cannot report failure, and a boot-health protocol is out of scope for the first release.
- 2026-11-15 (#175): Deviation from Claude Code, which applies updates by its own policy: Fontaine's update is on by default and silent apart from one post-update notice and a once-shown warning on a failed update; `[updates] auto_update` (user scope only) opts out. Reason: the maintainer's scope decision of 2026-11-15.
- 2026-11-15 (#175): Supersedes the Windows rename-aside note of 2026-11-08 for the background path only; the helper swaps when no process runs the target.
- 2026-11-15 (#175): the Windows helper's 24 h give-up is a constant, not configurable; `fontaine update --rollback` ships in the first release.

## Documentation

- `docs/installing.md`: automatic updates, the opt-out, silent cases, rollback, Windows behavior,
  log and marker files; the uninstall sections name the new files (`.prev`, `update-staged`,
  `update-helper`, `update.lock`, `update.stamp`, `update-helper.lock`, `updates.jsonl`, `update-applied.json`,
  `update-state.json`, `update-skip`).
- `docs/headless.md`: `fontaine update`, including `--rollback`.
- `docs/configuration.md`: `[updates]` section, the user-only matrix row and the new data files
  in the data-file table (validated by `cargo test -p fontaine-core --test docs`).
- `docs/tui.md`: the post-update notice.
- `CHANGELOG.md`.

## Testing Strategy

Unit and integration tests with wiremock and the existing release-index and signature doubles in
`tests/update_cmd.rs`; no live network.

- Background pass: newer signed build is installed and the marker written; unsigned, wrongly signed,
  wrong-channel and wrong-version archives are rejected with the binary and receipt byte-identical
  and one logged, once-shown warning; transient network failure is logged and quiet; up-to-date and
  older feed do nothing (no downgrade).
- Gating: no activity under `-p` and `fontaine send` (no request reaches the mock); receipt for
  another binary, no receipt: silent; `auto_update = false`: no request.
- Cadence: stamp due/not due, clock moved back.
- Concurrency: two passes against one data dir stage once (second `try_lock` loses); a pass racing
  `update()` installs once.
- Config: `updates.auto_update` rejected from project scope and `--settings` (`config_scope` tests),
  accepted in user scope; docs test.
- Notice: marker matching the receipt version shows once and is deleted; mismatched marker is
  deleted silently; a dev-shaped `VERSION` (`<pkg>-dev+<sha12>`) with a receipt at
  `<base>-dev.<N>+<sha7>` still shows the notice.
- Rollback: `--rollback` restores `.prev`, rewrites the receipt, and the skip file stops reinstall of
  that build.
- Windows helper logic: the wait/give-up/one-helper/re-verify/newer-build-wins decisions are tested
  against an injected "is the target locked" function so they run on every OS; the real lock probe
  is compiled and tested on the Windows CI leg.
- End to end N → N+1 on Linux, macOS and Windows runs in `update-e2e.yml` (not a PR gate): it
  catches installer/receipt/updater disagreements and the real Windows file locking that fakes
  cannot reproduce.

## Manual Verification

1. On Windows, install the previous dev build, start `fontaine`, leave it running, wait for the
   background pass (or delete `update.stamp`), confirm `fontaine.exe` is unchanged and
   `update-staged\` exists, exit Fontaine, and within ~5 s `fontaine --version` shows the new build
   with no UAC prompt and no window. Not automated because it needs an interactive session holding
   the lock; CI covers the non-interactive variant.
