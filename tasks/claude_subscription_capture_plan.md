# claude-subscription Opt-in Capture Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/737

## Overview

An opt-in, development-only capture of the `claude-subscription` traffic: the stream-json frames
Fontaine writes to the CLI and the result events the CLI returns, so two consecutive requests of a
conversation can be reassembled and diffed once the cache self-check (#735) reports a replay miss.
It supplements the self-check, which keeps only a hash per frame. A supplementary switch also
points the CLI's own undocumented raw-body capture at a private directory (the issue requires it).

## Current State

- `transport.rs` `prepare` builds the request's frames (`Translated.frames`), takes
  `context::fingerprints` (a `DefaultHasher` hash of each frame's `message`) before
  `context::apply`, applies the cache layout, then writes every history frame and finally the
  query frame to the CLI's stdin (`write_frame`).
- The accepted final result's usage is read by `Reader::cache_usage` (`protocol.rs`) and compared
  to the previous request's record in `context.rs` (`Records`, keyed by session id, bounded).
- `Cli` (`cli.rs`) owns `data_dir`, a `Warn` sink (`providers/warn.rs`, the `Approver::warn`
  route) and builds the child's environment in `Cli::environment`. `command()` does `env_clear`
  and `environment` passes only `PASSTHROUGH_ENV` plus explicit vars.
- `launch` has three callers: `transport.rs` `prepare`, and `cli.rs` discovery and probe (plus a
  test caller in `cli.rs`).
- One `Cli` serves every conversation of a connection (`Connected` holds one `Arc<Cli>`;
  `completion`, `guardian_completion` and `completion_for_agent` share it), each request with its own
  derived `translated.session_id`. `connect` is also reached from `client_for` in `models.rs` for
  login checks.
- `preserve_order` is enabled for `serde_json` in `rust/Cargo.toml`; `sha2` is already a
  `fontaine-sdk` dependency.
- `owner_only::create_new` / `create_dir` (`core/owner_only.rs`) create 0600 / 0700 entries with
  `O_EXCL` semantics; `Scratch` already uses them for per-launch files.
- `Cli::new` is built in `Provider::connect` (`mod.rs`) from `ConnectContext`, which carries the
  env (`ctx.env`), `data_dir`, `warn` and `session_id`; `Cli::new` has other callers, and in
  `connect` it is built before the session id is computed.

## Design

### Switch

A user-level environment variable `FONTAINE_CLAUDE_CAPTURE`: unset/empty/`0` is off, `1` (or `full`)
records the full post-`apply` frame list of every request, `delta` records only the new
message-level frames (pre-`apply`; it does not show cache-marker or reminder placement). It is read from `ctx.env` in `connect` only. There
is no config key at any scope, so project scope cannot enable it (the project-scope loader never
sees the variable; a project `[env]` table does not feed `ctx.env` for this read, which a test
pins). `--debug-file` / `FONTAINE_LOG` never enable it.

### New module `claude_subscription/capture.rs`

- `Capture` (held as `Option<Arc<Capture>>` on `Cli`; `None` when off, so nothing is created).
- On the first request launch (lazily, never in `connect`: `connect` is also reached for login
  checks): `Capture::ensure_created()` runs when `launch`/`launch_with` is called with the request
  flag, so the stem and `.bodies/` dir exist before the CLI spawns. It creates `<data_dir>/claude-capture/` with
  `owner_only::create_dir` tolerating `AlreadyExists` (verifying it is a real directory, not a
  symlink, via `symlink_metadata`), create the file `<fontaine-session-id>-<short random token>.jsonl` with
  `owner_only::create_new` (`O_EXCL`, 0600; no symlink following since it is a fresh create), and
  create the `.bodies/` dir (below), and emit one `Warn` stating the path, that nothing is redacted,
  and how to delete it. It runs once under the Capture's lock (or a `OnceLock`) and returns the same
  paths to every caller: one `Arc<Cli>` serves root, guardian and subagent requests that can reach
  first launch concurrently. A creation failure warns once through `Warn`, disables capture, and the
  launch proceeds without `OTEL_LOG_RAW_API_BODIES` (never propagated as a launch error).
- Concurrent conversations: one `Capture` and one file per `Cli`, shared by every conversation of
  the connection. A `Mutex` serialises appends; the capture keeps the previous-hash baseline and the
  request index per `translated.session_id` (a map inside `Capture`) and puts that id in each
  record. The baseline advances when a request is recorded (in `prepare`, before the replay), even if
  the request is later cut or fails. The switch is read from the host's env and is never forwarded to the CLI child
  environment, so the child CLI never sees it.
- No export: the module never touches telemetry, the ledger or remote events; records go only to
  the file. A write failure is reported once through `Warn` and capture stops for that file
  (never fails the request).

### Record format (JSON Lines, one object per line)

- `request`: `{kind:"request", session (the request's `translated.session_id`), index, at (unix ms),
  mode, hashes:[hex per frame], diverged_at: null | n, frames:[{i, frame}]}`.
  `hashes` are computed by capture.rs itself: a sha256 prefix (hex) of
  `frame.value["message"].to_string()` per pre-`apply` frame, via `Capture::prints(&frames)` called
  where `context::fingerprints` is called (it returns `u64` `DefaultHasher` values, so it is not
  reused). `hashes` and `diverged_at` appear in every record in both modes. They are
  layout-neutral because `apply` moves the cache marker/reminder between requests; divergence and
  delta use them.
  In full mode (default) `frames` holds all post-`apply` frames exactly as sent, so
  `reassemble` returns what was sent. In delta mode `frames` holds the pre-`apply` frames with
  index >= the length of the common prefix with the previous request's hashes, and `reassemble`
  returns the messages (not marker/reminder placement). If the new hash list does not extend the
  previous one, `diverged_at` is the index of the first differing frame (in delta mode frames from
  there on are recorded); if it is a strict prefix of the previous (shorter, no differing frame),
  `diverged_at` is `new.len()`. The first request records all frames.
- `result`: `{kind:"result", session, index, at, result}` (`session` and `index` from the
  handle `prepare` put on `Running`): the CLI's final `result` event
  verbatim (usage is inside it).
- Frames are stored as parsed JSON values via `serde_json` (`preserve_order` gives byte-stable
  replay). An empty thinking text with a
  signature serialises as `""` and survives unchanged.
- The capture hook points: `prepare` calls `Capture::prints(&frames)` pre-`apply` (in delta mode
  the caller also keeps the pre-`apply` frames), then, post-`apply` and before writing,
  `Capture::record_request(session, prints, pre_frames_if_delta, &post_frames_including_query)`
  with the full frame list including the popped query frame; and `stream` on `Event::Complete`, where the result `line`
  is already in hand and is parsed there.

### CLI's own capture (supplement)

When the switch is on, `Cli::environment` adds `OTEL_LOG_RAW_API_BODIES=file:<dir>` where `<dir>`
is `<data_dir>/claude-capture/<file-stem>.bodies/`, created 0700 via
`owner_only::create_dir` by `ensure_created()` (at the same lazy point as the file); the
request-launch path passes the resolved `.bodies` dir into `Cli::environment`. Absence of output is non-fatal (verified manually, see Manual
Verification). The mechanism is a separate `Cli::launch_request(scratch, args)` (calls `launch_with`
internally with a request flag), called by `transport.rs` `prepare` (~line 218) instead of `launch`;
`launch`/`launch_with` keep their signatures, so discovery, probe and reminder launches never get the
variable. `command`/`environment` take the optional bodies dir via an internal options value, read
from the `Cli`'s `Capture`.

### Reassemble and diff helper

A dev example `rust/fontaine-sdk/examples/claude_capture_diff.rs`
plus library functions: `capture::reassemble(path, session, index) -> io::Result<Reassembled { frames: Vec<Frame>,
diverged_at: Option<usize> }>` and `capture::diff(path, session, a_index, b_index)` (reads both and
reports the later record's `diverged_at` first)
(`mod.rs` declares `pub mod capture` with `reassemble` and `diff` `pub`, so the example can use
them). `reassemble` returns the frames of a full record directly and replays deltas for delta
records, honouring divergence markers. `diff`
reports the divergence point first, then the first differing frame index and a JSON-path level diff
of that frame. The example takes `<file> <session> <index>` and prints the diff between request `index-1`
and `index`.

## Implementation Steps

1. `capture.rs`: `Capture`, switch parsing, `ensure_created` (lazy directory/file/`.bodies`), warning, record writing,
   per-session baselines, full/delta/divergence logic, `reassemble`, `diff`.
2. `cli.rs`: hold `Option<Arc<Capture>>`, attached by a builder/setter on `Cli` (not a new
   `Cli::new` argument); add `Cli::launch_request(scratch, args)` (calls `launch_with` with a request
   flag, which calls `Capture::ensure_created()`); `command`/`environment` take the optional
   `.bodies` dir via an internal options value from the `Cli`'s `Capture` (adds the raw-bodies
   variable); `launch`/`launch_with` signatures and the `cli.rs` test caller are unchanged;
   `transport.rs` `prepare` is the one `launch_request` caller.
3. `mod.rs` `connect`: compute `session` first, read the switch from `ctx.env`, build the capture
   and attach it to the `Cli`.
4. `transport.rs`: call `prints` pre-`apply` and record the request in `prepare` post-`apply`
   (launching via `Cli::launch_request`); add the
   capture handle and request index to `Running`; record the result in `stream` at
   `Event::Complete`.
5. Example `claude_capture_diff.rs`.
6. Docs and changelog; master plan Decisions line.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/claude_subscription/{capture.rs (new), cli.rs, mod.rs,
  transport.rs}` (`reminder.rs` not touched)
- `rust/fontaine-sdk/examples/claude_capture_diff.rs` (new)
- `docs/claude-subscription.md`, `docs/configuration.md` (env var table, data-dir file table), `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Decisions

- 2026-11-20: Own capture is primary because the CLI's raw-body capture redacts thinking text, the
  suspect field.
- 2026-11-20: The capture records frames unredacted by design; protection is opt-in, 0600/0700,
  the startup warning and no export path.
- 2026-11-20: AFK: divergence/delta comparison uses the layout-neutral pre-`apply` message hash, as
  the self-check does, because `apply` moves the cache marker between requests.
- 2026-11-20: AFK: the capture records full post-`apply` frames by default (`delta` is opt-in and
  message-level), because `apply` rewrites earlier frames so deltas cannot reassemble what was sent.
- 2026-11-20: AFK: the CLI raw-body supplement is kept (the issue requires it), non-fatal and
  verified manually.

## Documentation

- `docs/claude-subscription.md`: a "Capturing traffic" section with the limits, the warning, the
  file locations and how to delete them.
- `docs/configuration.md`: the environment variable row, and a `claude-capture/` row in the data-dir
  file table (permissions 0700/0600, safe to delete).

## Testing Strategy

Unit tests in `capture/tests.rs` (no live CLI):
- Off by default: no file or directory is created when the variable is unset.
- Owner-only: file is 0600 and directories 0700 (Unix); the warning is raised through a recording
  `Warn`; `O_EXCL` refuses an existing file.
- Project scope cannot enable it: a project `[env]` / config carrying the variable leaves the
  capture off.
- A scripted conversation of three requests: default (full) mode reassembles to exactly the frames
  sent, including a moved cache marker and an inserted reminder; `delta` mode reassembles the
  messages; the hash list matches; the cache marker moving between consecutive requests does not
  report divergence; an edited history produces `diverged_at` with the right index.
- Two conversations (different `translated.session_id`) through one `Capture` keep separate
  baselines and indices; the baseline advances when a request is recorded, even if it is later cut
  or fails.
- Concurrent first launch (several tasks calling `ensure_created()` at once): one file, one dir, one
  warning, and the same paths returned to all callers.
- Creation failure (e.g. unwritable data dir): one warning, capture disabled, the launch proceeds
  without `OTEL_LOG_RAW_API_BODIES` and no launch error.
- No file, directory or warning is created until the first request launch; `ensure_created()` then
  creates the file, `.bodies/` dir and warning before spawn.
- A thinking block with empty text and a signature round-trips byte-for-byte through write and
  reassemble.
- `diff` reports the divergence point first, then the first differing frame.
- The child environment gets `OTEL_LOG_RAW_API_BODIES` only when on and only for request
  launches (not discovery, probe or reminder), and never the Fontaine switch.
- A transport test with the existing fake CLI double shows the request and result records appear.

## Manual Verification

Not automated: it depends on the undocumented behaviour of the external binary.

1. Run `FONTAINE_CLAUDE_CAPTURE=1 cargo run -p fontaine` from `rust/` (the TUI) with a
   `claude-subscription` main model and send two prompts in the same process.
2. Check `<data_dir>/claude-capture/` holds one `.jsonl` for the process with request and result
   records for both turns (a `--resume` in a second process makes a second file with no cross-turn
   diff).
3. Check the `.bodies/` directory next to it is 0700 and whether the CLI wrote raw bodies into it;
   empty is acceptable and does not fail the run.
