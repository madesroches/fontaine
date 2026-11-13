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
- `McpClientHandler` is a public `ClientHandler`; its `on_tool_list_changed` fetches the
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
2. Initial tool list: `NotificationContext` is `#[non_exhaustive]` with no public constructor, and
   `fetch_tools`/`commit_initial` are `pub(crate)`, so the handler's tool refresh cannot be called
   directly. Instead the tap stack injects one synthetic `notifications/tools/list_changed` into
   the inbound stream right after the first successful `server/discover` or `initialize` response;
   rmcp dispatches it to `on_tool_list_changed` with a real context, which fetches and commits
   exactly as a later notification does (through `reconcile_managed_tools(expected = {})`, which
   is equivalent to `add_managed_tools` here: the connection's registry is empty and `muted` is
   false at connect). `start_service` then waits for that first commit (a `Notify` fired from
   `GuardedSink` when it records tools, including an empty list), inside the caller's
   `CONNECT_TIMEOUT`. `on_tool_list_changed` only logs a fetch failure, so a server whose
   `tools/list` fails never signals and connect ends as a Timeout (Transient, retried) instead of
   the underlying error; that is the one limitation of not forking rig-rmcp.
3. `McpConnection::new(service)`.

Everything wrapping the transport (`MetaTap`, `ElicitationTap`, `CallTracker`) is unchanged: the
taps see `server/discover` like any other request, and per-request `_meta` is attached by rmcp when
discover negotiates 2026-07-28.

`Auto` semantics worth knowing: a legacy server that answers `server/discover` with method-not-found
falls back at once; one that never answers it delays connect by the 10 s probe timeout before
falling back. The whole sequence (discover wait, initialize, tool list) shares the 30 s `CONNECT_TIMEOUT` budget
wrapped by `connect()`.

The shim is deliberately small and isolated so it can be replaced by a call to an upstream
`connect_with_lifecycle` if rig-rmcp adds one.

## Implementation Steps

1. `rust/fontaine-tools/src/mcp/servers.rs` (and `meta.rs` for the injection): add
   `start_service` (above) and call it from `serve()` in place of `handler.connect`; map errors through `McpError::from_client` (the
   existing `McpClientError` conversion from `ClientInitializeError` covers `serve_with_lifecycle`;
   convert explicitly with `McpClientError::from`/`.into()` before `McpError::from_client`).
   Also make `classify_init` (`servers.rs:317-324`) unwrap
   `ClientInitializeError::LegacyFallbackFailed` and classify its `fallback` error, so a transient
   fallback failure stays Transient instead of falling to Permanent.
2. Update the doc comment of `client_config()` and `serve()` to say the lifecycle is `Auto`.
3. `rust/fontaine-tools/tests/mcp_mrtr.rs` (or a sibling `mcp_discover.rs`): scripted servers for
   the cases in Testing Strategy. The existing scripted server answers unknown methods (including
   `server/discover`) with `{}`, which under `Auto` would change the legacy path the current MRTR
   test relies on, so give it an explicit `server/discover` arm (method-not-found for legacy cases)
   and an `initialize` rejection arm; parameterize it per scenario or put the scenarios in a sibling
   file. Its loop only writes replies, so give it a writer handle for sending notifications
   (`tools/list_changed`). The rmcp double in `mcp_common` always answers `initialize` with the older
   version, so it cannot serve as the discover-lifecycle server.
4. Docs, changelog, master-plan Decisions line (below).

## Files to Modify

- `rust/fontaine-tools/src/mcp/servers.rs`, `rust/fontaine-tools/src/mcp/meta.rs`, `rust/fontaine-tools/src/mcp/register.rs`
- `rust/fontaine-tools/tests/mcp_mrtr.rs` (or new `mcp_discover.rs`)
- `docs/mcp.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Upstream `connect_with_lifecycle` first**: cleanest, but it blocks on a release and a pin bump;
  the shim covers the need now and is replaceable. Filing/offering the upstream change is the
  maintainer's call and is not part of this plan.
- **Vendor or `[patch]` rig-rmcp**: heavier supply-chain surface than a ~30-line shim.
- **Synthetic `tools/list_changed` injected from the tap** (chosen): hides the initial fetch inside
  the transport, but is the only way to reuse the handler's own refresh state with public API.
  Rejected: building the context directly (`NotificationContext` is non-exhaustive); fetching with
  `list_all_tools` and registering via `add_managed_tools` (the handler's registrations and
  `committed_refresh` would not know those tools, so a later refresh never retires them, and
  `McpTool::new` holds a `McpConnection` clone that forms a cycle through the sink's registry);
  wrapping the handler (still no way to run its initial fetch).
- **`Discover` only**: would drop every legacy server; `Auto` keeps them.

## Decisions

- 2026-11-13: `classify_init` unwraps `LegacyFallbackFailed` and classifies the fallback error, so retry behavior is unchanged under `Auto`.
- AFK: The initial tool list comes from a tap-injected synthetic `tools/list_changed` handled by rig-rmcp's own `on_tool_list_changed` (the only public-API route, since `NotificationContext` is non-exhaustive and `fetch_tools`/`commit_initial` are `pub(crate)`); connect waits for the first commit, and a failing `tools/list` surfaces as a connect Timeout rather than the underlying error.
- AFK: The initial commit going through `reconcile_managed_tools(expected = {})` instead of `add_managed_tools` is accepted: the per-connection registry is empty and `muted` is false at connect, so the two are equivalent.

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
  tools, and serves an MRTR elicitation round (the headline case of #613); the test also asserts the discover
  round trip passes through `MetaTap` and `ElicitationTap`.
- A legacy server that answers `server/discover` with method-not-found falls back to `initialize`
  and registers its tools exactly once (no duplicate registration from the probe).
- A server whose `tools/list` fails does not connect: attempt ends as a Timeout (Transient) with no tools registered.
- A server with an empty tool list connects (the commit signal fires for an empty list).
- A later `tools/list_changed` (sent by the scripted server) still reconciles tools after a
  discover-lifecycle connect.
- Unit test of `classify_init` (beside the existing ones in `servers.rs`):
  `ClientInitializeError::LegacyFallbackFailed { fallback: ConnectionClosed }` is classified Transient.
- The existing `client_config()` and MRTR tests stay green.

## Open Questions

None.
