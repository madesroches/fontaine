# MCP server/discover Lifecycle Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/613

## Overview

rig-rmcp's `McpClientHandler::connect` always starts the legacy `initialize` lifecycle, so a server
that supports only protocol 2026-07-28 and rejects `initialize` cannot connect. rmcp 3.9 offers
`ClientLifecycleMode::Auto` (probe `server/discover`, fall back to `initialize`). This plan starts
the service from Fontaine with `Auto`, so 2026-07-28-only servers connect and are served by the MRTR
driver in `ElicitationTap`, without waiting for a rig-rmcp release (0.46.0 is the latest; its
`begin_refresh` / `fetch_tools` / `commit_initial` are `pub(crate)`).

## Current State

- `serve()` (`rust/fontaine-tools/src/mcp/servers.rs:753`) builds
  `McpClientHandler::new(client_config(), GuardedSink)`, wraps the transport as
  `ElicitationTap<MetaTap<transport>>` and calls `handler.connect(tapped)`, which does
  `ServiceExt::serve` (legacy), then `begin_refresh` → `fetch_tools` → `commit_initial`, then
  `McpConnection::new(service)` (`rig-rmcp-0.46.0/src/handler.rs:213`).
- `McpClientHandler` is a public `ClientHandler`; its `on_tool_list_changed` (public trait method,
  taking a `NotificationContext` whose fields `meta`, `extensions`, `peer` are public) fetches the
  tool list and commits it with `commit_refresh`, which calls
  `ManagedToolSink::reconcile_managed_tools(expected = {}, tools)` on the first run.
  `GuardedSink::reconcile_managed_tools` (`mcp/register.rs:221`) wraps, records and forwards it; it
  returns `expected` untouched while `muted`.
- `McpConnection::new(RunningService)` is public; `McpClientError` / `classify_client_error`
  already handle failures of `serve` (`ClientInitializeError`) via `McpError::from_client`.
- `CONNECT_TIMEOUT` is 30 s per attempt (`servers.rs:49`); rmcp's `Auto` waits at most 10 s for
  `server/discover` before falling back.
- `tests/mcp_mrtr.rs` has a scripted in-process server that answers `initialize` with 2026-07-28.
- Known gap recorded in the master plan's Decisions (2026-11-13, #542): servers that reject
  `initialize` stay unreachable until the discover lifecycle is available.

## Design

`serve()` replaces `handler.connect(tapped)` with a Fontaine-side `start_service` that does the
same three things with public API:

1. `handler.serve_with_lifecycle(tapped, ClientLifecycleMode::Auto { preferred_versions:
   vec![ProtocolVersion::LATEST], legacy_version: None })` (`ClientServiceExt`). `legacy_version:
   None` keeps the `initialize` offer at `client_config()`'s version (rmcp's newest), as today.
2. Initial tool list: `service.service().on_tool_list_changed(NotificationContext { meta:
   default, extensions: default, peer: service.peer().clone() })`, which fetches and commits
   exactly as a later `tools/list_changed` does. It only logs a failure, so first probe the list
   with `peer.list_all_tools()` (bounded by `CONNECT_TIMEOUT` through the caller's timeout) and
   turn its error into `McpError::from_client` as `connect` did: a server whose tool list fails
   still fails to connect rather than connecting with no tools. The probe's result is discarded;
   one extra `tools/list` at connect is the price of not forking rig-rmcp.
3. `McpConnection::new(service)`.

Everything wrapping the transport (`MetaTap`, `ElicitationTap`, `CallTracker`) is unchanged: the
taps see `server/discover` like any other request, and per-request `_meta` is attached by rmcp when
discover negotiates 2026-07-28.

`Auto` semantics worth knowing: a legacy server that answers `server/discover` with method-not-found
falls back at once; one that never answers it delays connect by the 10 s probe timeout before
falling back. That fits inside `CONNECT_TIMEOUT` (30 s).

The shim is deliberately small and isolated so it can be replaced by a call to an upstream
`connect_with_lifecycle` if rig-rmcp adds one.

## Implementation Steps

1. `rust/fontaine-tools/src/mcp/servers.rs`: add `start_service` (above) and call it from `serve()`
   in place of `handler.connect`; map errors through `McpError::from_client` (extend
   `McpClientError` conversion for the probe's `ServiceError` if needed).
2. Update the doc comment of `client_config()` and `serve()` to say the lifecycle is `Auto`.
3. `rust/fontaine-tools/tests/mcp_mrtr.rs` (or a sibling `mcp_discover.rs`): scripted servers for
   the cases in Testing Strategy.
4. Docs, changelog, master-plan Decisions line (below).

## Files to Modify

- `rust/fontaine-tools/src/mcp/servers.rs`
- `rust/fontaine-tools/tests/mcp_mrtr.rs` (or new `mcp_discover.rs`)
- `docs/mcp.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Upstream `connect_with_lifecycle` first**: cleanest, but it blocks on a release and a pin bump;
  the shim covers the need now and is replaceable. Filing/offering the upstream change is the
  maintainer's call and is not part of this plan.
- **Vendor or `[patch]` rig-rmcp**: heavier supply-chain surface than a ~30-line shim.
- **Synthetic `tools/list_changed` injected from the tap**: hides the initial fetch inside the
  transport; calling the public `on_tool_list_changed` directly is explicit.
- **`Discover` only**: would drop every legacy server; `Auto` keeps them.

## Decisions

## Documentation

- `docs/mcp.md`: state that Fontaine probes `server/discover` first and falls back to `initialize`,
  that a legacy server that ignores the probe adds up to 10 s to connect, and that
  2026-07-28-only servers connect.
- `CHANGELOG.md`: entry under unreleased.
- `tasks/fontaine_harness_plan.md` Decisions: dated line at the end of the MCP subsection,
  superseding the "stay unreachable" clause of the 2026-11-13 (#542) line, and recording that the
  lifecycle is started from Fontaine because rig-rmcp 0.46.0 exposes no lifecycle choice.

## Testing Strategy

Automated, driven through an in-process scripted server (as `mcp_mrtr.rs`):

- A server that answers `server/discover` (2026-07-28) and rejects `initialize` connects, lists its
  tools, and serves an MRTR elicitation round (the headline case of #613).
- A legacy server that answers `server/discover` with method-not-found falls back to `initialize`
  and registers its tools exactly once (no duplicate registration from the probe).
- A server whose `tools/list` fails fails to connect (the probe's guard), classified as before.
- A server that never answers `server/discover` falls back after the probe timeout (use paused
  tokio time).
- A later `tools/list_changed` still reconciles tools after a discover-lifecycle connect.
- The existing `client_config()` and MRTR tests stay green.

## Open Questions

None.
