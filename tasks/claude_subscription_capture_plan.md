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
- `launch` has three callers: `transport.rs` `prepare`, and `cli.rs` discovery and probe.
- `preserve_order` is enabled for `serde_json` in `rust/Cargo.toml`; `sha2` is already a
  `fontaine-sdk` dependency.
- `owner_only::create_new` / `create_dir` (`core/owner_only.rs`) create 0600 / 0700 entries with
  `O_EXCL` semantics; `Scratch` already uses them for per-launch files.
- `Cli::new` is built in `Provider::connect` (`mod.rs`) from `ConnectContext`, which carries the
  env (`ctx.env`), `data_dir`, `warn` and `session_id`; `Cli::new` has other callers, and in
  `connect` it is built before the session id is computed.

## Design

### Switch

A user-level environment variable `FONTAINE_CLAUDE_CAPTURE`: unset/empty/`0` is off, `1` records
deltas, `full` records full frames per request. It is read from `ctx.env` in `connect` only. There
is no config key at any scope, so project scope cannot enable it (the project-scope loader never
sees the variable; a project `[env]` table does not feed `ctx.env` for this read, which a test
pins). `--debug-file` / `FONTAINE_LOG` never enable it.

### New module `claude_subscription/capture.rs`

- `Capture` (held as `Option<Arc<Capture>>` on `Cli`; `None` when off, so nothing is created).
- On `connect` with the switch on: create `<data_dir>/claude-capture/` with
  `owner_only::create_dir` tolerating `AlreadyExists` (verifying it is a real directory, not a
  symlink, via `symlink_metadata`), create the per-session file `<session-id>.jsonl` with
  `owner_only::create_new` (`O_EXCL`, 0600; no symlink following since it is a fresh create), and
  emit one `Warn` stating the path, that nothing is redacted, and how to delete it.
- Concurrent conversations: one file per `Capture`; a `Capture` is created per `connect` (one per
  session/child account client), and its session id suffix includes a short random token so two
  clients with the same session never collide. A `Mutex` serialises appends inside one capture and
  the request index is a counter on it. Subagents get their own `connect` and therefore their own
  file; the switch is read from the host's env and is never forwarded to the CLI child environment
  (`command()` does `env_clear` and `environment` passes only `PASSTHROUGH_ENV`), so the child
  CLI never sees it.
- No export: the module never touches telemetry, the ledger or remote events; records go only to
  the file. A write failure is reported once through `Warn` and capture stops for that file
  (never fails the request).

### Record format (JSON Lines, one object per line)

- `request`: `{kind:"request", session, index, at (unix ms), hashes:[hex per frame],
  diverged_at: null | n, frames:[{i, frame}]}`. `frames` are the post-`apply` frames as sent.
  `hashes` are the pre-`apply` fingerprints, as `context::fingerprints` takes (context.rs ~362):
  a sha256 prefix (hex) of each frame's `message` JSON. They are layout-neutral because `apply`
  moves the cache marker/reminder between requests; delta and divergence use them. In delta mode `frames` holds only frames with index >=
  the length of the common prefix with the previous request's hashes; in `full` mode all frames.
  If the new hash list does not extend the previous one (the common prefix is shorter than the
  previous list), `diverged_at` is the index of the first differing frame and frames from there on
  are recorded. The first request records all frames.
- `result`: `{kind:"result", session, index, at, usage, result}`: the CLI's final `result` event
  verbatim (usage included).
- Frames are stored as parsed JSON values via `serde_json` (`preserve_order` gives byte-stable
  replay). An empty thinking text with a
  signature serialises as `""` and survives unchanged.
- The capture hook points: `prepare` (after `apply`, before writing, with the full frame list
  including the popped query frame) and `stream` on `Event::Complete`, where the result `line`
  is already in hand and is parsed there.

### CLI's own capture (supplement)

When the switch is on, `Cli::environment` adds `OTEL_LOG_RAW_API_BODIES=file:<dir>` where `<dir>`
is `<data_dir>/claude-capture/<session-file-stem>.bodies/`, created 0700 via
`owner_only::create_dir`. Absence of output is non-fatal (verified manually, see Manual
Verification). The mechanism is a bool on `launch`/`environment` (a request-launch variant used
only by `transport.rs` `prepare`), so discovery, probe and reminder launches never get the
variable.

### Reassemble and diff helper

A dev example `rust/fontaine-sdk/examples/claude_capture_diff.rs`
plus library functions: `capture::reassemble(path, index) -> Vec<Frame>` and `capture::diff(a, b)`
(also pub(crate) for tests). `reassemble` replays deltas, honouring divergence markers. `diff`
reports the divergence point first, then the first differing frame index and a JSON-path level diff
of that frame. The example takes `<file> <index>` and prints the diff between request `index-1`
and `index`.

## Implementation Steps

1. `capture.rs`: `Capture`, switch parsing, directory/file creation, warning, record writing,
   delta/divergence logic, `reassemble`, `diff`.
2. `cli.rs`: hold `Option<Arc<Capture>>`, attached by a builder/setter on `Cli` (not a new
   `Cli::new` argument); `launch`/`environment` take a request-launch bool that adds the
   raw-bodies variable.
3. `mod.rs` `connect`: compute `session` first, read the switch from `ctx.env`, build the capture
   and attach it to the `Cli`.
4. `transport.rs`: record the request in `prepare` (using the request-launch variant), the result
   in `stream` at `Event::Complete`.
5. Example `claude_capture_diff.rs`.
6. Docs and changelog; master plan Decisions line.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/claude_subscription/{capture.rs (new), cli.rs, mod.rs,
  transport.rs}`
- `rust/fontaine-sdk/examples/claude_capture_diff.rs` (new)
- `docs/claude-subscription.md`, `docs/configuration.md` (env var table), `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- Own capture primary because the CLI's capture redacts thinking text, the suspect field.
- Delta plus pre-`apply` hashes keeps size linear while keeping requests reassemblable; full mode is a flag
  for cases where the append-only assumption is itself suspect.
- Env var rather than config key: a config key invites project-scope enabling and persistence.

## Decisions

- 2026-11-20: The capture records frames unredacted by design; protection is opt-in, 0600/0700,
  the startup warning and no export path.
- 2026-11-20: AFK: divergence/delta comparison uses the layout-neutral pre-`apply` message hash, as
  the self-check does, because `apply` moves the cache marker between requests.
- 2026-11-20: AFK: the CLI raw-body supplement is kept (the issue requires it), non-fatal and
  verified manually.

## Documentation

- `docs/claude-subscription.md`: a "Capturing traffic" section with the limits, the warning, the
  file locations and how to delete them.
- `docs/configuration.md`: the environment variable row.

## Testing Strategy

Unit tests in `capture/tests.rs` (no live CLI):
- Off by default: no file or directory is created when the variable is unset.
- Owner-only: file is 0600 and directories 0700 (Unix); the warning is raised through a recording
  `Warn`; `O_EXCL` refuses an existing file.
- Project scope cannot enable it: a project `[env]` / config carrying the variable leaves the
  capture off.
- A scripted conversation of three requests: deltas reassemble to exactly the frames sent; the
  hash list matches; the cache marker moving between consecutive requests does not report
  divergence; an edited history produces `diverged_at` with the right index; `full` mode
  records all frames.
- A thinking block with empty text and a signature round-trips byte-for-byte through write and
  reassemble.
- `diff` reports the divergence point first, then the first differing frame.
- The child environment gets `OTEL_LOG_RAW_API_BODIES` only when on and only for request
  launches (not discovery, probe or reminder), and never the Fontaine switch.
- A transport test with the existing fake CLI double shows the request and result records appear.

## Manual Verification

Not automated: it depends on the undocumented behaviour of the external binary.

1. Run `FONTAINE_CLAUDE_CAPTURE=1 cargo run -p fontaine -- -p "say hi"` from `rust/` with a
   `claude-subscription` main model, then resume the same session with a second prompt.
2. Check `<data_dir>/claude-capture/` holds the session `.jsonl` with request and result records
   for both turns.
3. Check the `.bodies/` directory next to it is 0700 and whether the CLI wrote raw bodies into it;
   empty is acceptable and does not fail the run.
