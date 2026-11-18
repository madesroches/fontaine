# MCP: Headless Stdio Restart and Background Retry Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/538

## Overview

Two follow-ups to the MCP robustness work (#35, #486), both for headless runs only, matching Claude
Code's documented "Automatic reconnection" behavior:

- **A.** After a remote server's five reconnect attempts fail on a transient error, keep retrying in
  the background for about 30 minutes; its tools come back when one connects.
- **B.** A stdio server that exited mid-session is started again the next time the model calls one of
  its tools; the call runs once the server connects.

Interactive sessions keep today's behavior (remote: five attempts then failed; stdio: failed until
`/mcp reconnect`).

## Current State

All in `rust/fontaine-sdk/src/core/agent/mcp_supervisor.rs`:

- `Supervisor::watch` waits for the connection to close (or, for remote, a `transport_failed`
  signal), drops the connection, calls the `refresh` callback, then for a remote slot runs
  `reconnect_with_backoff` and for a stdio slot sets `Failed("the server exited")` and warns with
  `/mcp reconnect`.
- `reconnect_with_backoff` makes `RECONNECT_ATTEMPTS` (5) attempts with `RECONNECT_BASE` (1 s)
  doubling, stopping early on `McpErrorKind::Permanent`; afterwards the slot is `Failed`.
- `Supervisor::reconnect` (manual) calls `slot.supersede()` (bumps `generation`, aborts the watcher),
  closes the old connection, connects once, `install`s. `install` rejects a stale generation.
- `McpServerState` is `Connected | Reconnecting { attempt } | Failed(String)`.
- The module doc and `docs/mcp.md` ("Reconnecting") state a stdio server is never restarted
  automatically, headless included; the master plan's Decisions line dated 2026-11-08 (#35) records
  the gap.
- A connection's tools are registered through `GuardedSink` (`rust/fontaine-sdk/src/tools/mcp/register.rs`)
  and are retired when the `Connected` is dropped; `.with_liveness(probe.is_live())` hides a tool
  whose underlying rmcp tool is no longer live. With the connection gone the model has no tool to
  call, so "restart on next call" needs a hook that survives the connection (Part B).
- "Headless" is `setup.headless` (`rust/fontaine-sdk/src/core/agent.rs:149`, set by
  `rust/fontaine/src/lib.rs` for `-p`, and used for `Teams::headless`, `offers_drafts`). The
  Supervisor is built in `core/agent/mcp.rs` (~line 771) and does not receive it yet.

## Design

### Headless signal

Add `headless: bool` to `Supervisor` (constructor parameter), passed from `self.setup.headless` where
`Supervisor::new` is called in `core/agent/mcp.rs`. This is the single existing headless signal, so
it covers `fontaine -p`, the stream-json / Claude-compat surface (which goes through
`headless::run`) and SDK embedders that set `headless`. Tests construct it directly.

### Part A: background retry for remote servers

Constants next to `RECONNECT_ATTEMPTS`: `BACKGROUND_RETRY_CAP = 60 s` (interval ceiling, doubling
from the last attempt's wait) and `BACKGROUND_RETRY_WINDOW = 30 min`, measured from the moment the
connection was lost.

In `reconnect_with_backoff`, when the fifth attempt has failed, the last error is `Transient`, and
`self.headless`: continue in the same loop with waits `16 s → 32 s → 60 s → 60 s …` until success,
a `Permanent` error, the window elapsing, or the slot being superseded/closed. Extract the
attempt-and-install body into one helper used by both phases (DRY), so success handling (listing
pull, `install`, "reconnected" notice, `refresh`) exists once.

Supersession: the loop already runs inside the watcher task, which `supersede()` aborts; a manual
`/mcp reconnect`, server removal and `retire` therefore end it. The `generation` check in `install`
covers an attempt in flight.

State: add `McpServerState::Retrying` — no payload — shown as "pending (retrying in the background)".
It is not `Failed`, so `/mcp`, the stream-json `mcp_servers` status and `AllFailed` do not report a
still-retried server as dead. When the window ends the slot becomes `Failed(<last error>)` and the
warning naming `/mcp reconnect` is emitted once (as today after the fifth attempt, which in headless
now fires only at the end of the window). `Retrying` is a new enum variant, so every `match` on
`McpServerState` must be updated (compiler-enforced). The stream-json `mcp_servers` status maps it to
`pending`, as `Reconnecting` is today.

Remote only; a stdio slot never enters this path.

### Part B: restart a stdio server on the next call (headless)

Chosen approach: a tombstone (issue option 1), because it is the only one that matches "the call
runs once the server connects" within the same turn.

1. When a headless stdio slot loses its connection, `watch` records the server's tool definitions
   (name, description, schema) in the slot as a `Tombstone` instead of letting them leave the
   registry. The definitions come from `Connected::tool_names()`/the deferral registry record taken
   before the connection is dropped. The lost connection's own tools are removed as today; the
   tombstone registers one stand-in `DynamicTool` per original tool through the same `GuardedSink`
   wrapping path, so it keeps `RiskClass::Arbitrary`, `ToolSource::Mcp` and the choke-point
   registration — the risk class and gate (floor, hooks, guardian) run **before** the stand-in's
   body, exactly as for a live tool.
2. The stand-in's body calls `Supervisor::restart_for_call(slot)`: single-flight per slot (an async
   mutex on the slot; a caller that finds the slot already `Connected` skips the restart), bounded
   by `CONNECT_TIMEOUT`, using `Supervisor::reconnect`'s machinery on the **stored** `ServerSpec`.
   It never re-reads config, re-resolves project scope or reruns a helper. On success `install`
   swaps the tombstone for the live tools (reconcile through the existing sink) and the stand-in
   dispatches the call to the new connection by tool name. On failure it returns the connect error
   chain as the tool error and leaves the tombstone in place.
3. Budget: per call one restart attempt; per server per run `MAX_STDIO_RESTARTS = 3` restarts. Past
   the budget the tombstone is removed, the slot becomes `Failed("exited more than N times")`, and
   the tools leave the registry. Claude Code documents no cap; this is a deviation recorded in
   Decisions.
4. Only a server that exited **mid-session** gets a tombstone: the slot must have been
   `Connected` before the loss. A first-connection failure stays `Failed` with no tools (as Claude
   Code). `/mcp reconnect`, `remove` and `retire` clear the tombstone and its stand-ins.
5. Deferral and mentions: tombstone tools are recorded in the deferral registry like live ones so
   `ToolSearch` listings stay stable across the restart; resources/prompts of the lost server remain
   dropped from the mention snapshot (listings come with the new connection).
6. Reaping: the restarted `Connected` is a normal `connect` result, so its process group is
   recorded and `close_all_final` signals it.

Interactive stays as today: `headless == false` never builds a tombstone.

### Splitting

Part A and Part B are independent. Implement A first as its own commit(s); B follows on the same
branch. They ship in one PR unless review finds B too large, in which case B moves to a follow-up
issue and the Decisions line is split accordingly.

## Implementation Steps

1. `mcp_supervisor.rs`: add `headless` to `Supervisor`; update `Supervisor::new` callers
   (`core/agent/mcp.rs`, tests in `mcp_supervisor.rs`).
2. Part A: constants, `McpServerState::Retrying`, extended `reconnect_with_backoff` with shared
   attempt helper; update all `McpServerState` matches (TUI `/mcp` rendering, stream-json
   `mcp_servers`, `ReconnectTarget::AllFailed` stays `Failed`-only).
3. Part A tests (below).
4. Part B: `Tombstone` on `ServerSlot`; recording at loss in `watch`; stand-in tool registration
   (`tools/mcp/register.rs`); `restart_for_call` with single-flight and budget; clearing paths.
5. Part B tests (below).
6. Docs and Decisions.

## Files to Modify

- `rust/fontaine-sdk/src/core/agent/mcp_supervisor.rs` — Supervisor, slot, state, retry loop, restart.
- `rust/fontaine-sdk/src/core/agent/mcp.rs` — pass `headless`; mcp status rendering.
- `rust/fontaine-sdk/src/tools/mcp/register.rs`, `servers.rs` — stand-in tools / tool definition capture.
- TUI `/mcp` view and stream-json status mapping (find with `grep Reconnecting`).
- `docs/mcp.md`, `docs/headless.md`, `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`.
- Integration test under `rust/fontaine/tests/` with a stdio test server.

## Trade-offs

- Tombstone vs restart at next turn: the turn-start option is simpler but a call in the same turn
  still fails, which departs from Claude Code's documented behavior. The tombstone costs changes in
  tool registration and the choke-point test but keeps parity.
- `Retrying` variant vs staying `Failed` plus a background task: a distinct state keeps `/mcp`,
  `AllFailed` and the SDK status honest about a server still being retried.

## Decisions

- AFK: Headless means `setup.headless` (covers `-p`, stream-json and SDK embedders that set it).
- AFK: Retry state is a new `Retrying` variant rather than `Failed`, so a retried server is not reported dead.
- AFK: Stdio restart budget is 3 per server per run and 1 per call; Claude Code documents no cap.
- AFK: A and B ship in one PR, with B splittable if review finds it too large.

## Documentation

- `docs/mcp.md` "Reconnecting": describe the headless background retry (30 minutes, transient errors
  only, `Retrying` state) and the stdio restart on next call, its budget, and that interactive is
  unchanged.
- `docs/headless.md` (line ~227): replace the "stays failed" sentence.
- Replace the master plan Decisions line dated 2026-11-08 (#35) with a dated line recording what
  shipped and the deviations (state naming, restart cap, headless definition).
- `CHANGELOG.md` entry; update the #35 and #486 checklists on GitHub after merge.

## Testing Strategy

Unit tests in `mcp_supervisor.rs` using `Script`, the injectable `McpConnector` and
`tokio::time::pause`:

- headless: five transient failures continue retrying (state `Retrying`, gaps 16, 32, 60, 60 …) and a
  later success installs and notifies "reconnected".
- headless: a permanent error during the window ends it as `Failed`.
- headless: the 30-minute window ends it as `Failed` with one warning.
- headless: a manual `reconnect` mid-window supersedes it (no further scripted attempts consumed).
- interactive (`headless = false`): five attempts then `Failed`, no further attempts.
- stdio exit in interactive mode does not retry or restart (existing test kept).
- headless stdio: after a mid-session exit a call to the stand-in restarts and dispatches; concurrent
  calls produce one connect; a failed restart returns the error and keeps the tombstone; the fourth
  loss removes the tombstone and fails the slot; a first-connection failure creates no tombstone.
- choke-point test stays green for stand-in tools; a denied call does not restart the server.

Headless integration test through `fontaine -p` (no live APIs; scripted completion model, a stdio test
server binary that exits after one call): the next call restarts it and succeeds; exits past the cap
fail the call with a clear error. This is automated because the wiring across registry, gate and
process spawn is where a silent regression would hide.

## Open Questions

- Observe `claude -p` against a stdio server that exits mid-run (same-turn restart, restart count,
  how a failed restart surfaces) and align the budget and error text; AGENTS.md prefers observation
  to guessing. Human step, requires the CLI.
