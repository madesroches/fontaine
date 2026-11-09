# MCP Robustness: Elicitation, Reconnect, Output Cap Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/35
**Tracking issue**: https://github.com/madesroches/fontaine/issues/486 (first step of its suggested order)

## Overview

Make Fontaine's MCP client survive real-world servers, as Claude Code does. There are three parts:

1. **Elicitation.** A server's `elicitation/create` request is shown to the human as a form in the
   terminal UI. Headless runs, AFK mode and the doctor probe answer `cancel`.
2. **Reconnect.** A remote (HTTP) server that drops mid-session is reconnected with exponential
   backoff, and its first connection is retried after a transient error. A stdio server that exits
   is marked failed. `/mcp reconnect <name|all>` restarts a server by hand.
3. **Output cap.** A successful MCP result that is too large is saved to a file in the session
   scratch directory, and the model gets the path instead. A long error result keeps its head and
   tail.

`list_changed` refresh has already landed and is out of scope.

## Current State

- `fontaine-tools/src/mcp/servers.rs` (`connect`, `start`, `connect_transport`) builds a
  `rig_rmcp::McpClientHandler` around a `GuardedSink` and serves it over an `InteractionTap`. The
  tap is a transport wrapper that reads `_meta["anthropic/requiresUserInteraction"]` off
  `tools/list` responses, because rig-rmcp drops `_meta`. `client_config()` advertises
  `ClientCapabilities::default()`, so no elicitation capability is declared. If a server sends
  `elicitation/create` anyway, the handler's default `create_elicitation` declines it.
- `rig_rmcp::McpClientHandler` (rig-rmcp 0.46.0, `handler.rs`) implements `ClientHandler` itself.
  It only overrides `on_tool_list_changed`, and its initial fetch (`fetch_tools`, `commit_initial`)
  is `pub(crate)`. Fontaine therefore cannot wrap the handler to override `create_elicitation`
  without reimplementing `connect`.
- `fontaine-tools/src/mcp/register.rs` `GuardedSink::wrap` wraps each tool with
  `guard_callback(.., RiskClass::Arbitrary, ..)` and attaches `with_liveness(probe.is_live())`.
  When the transport closes, the registry retires a dead connection's tools. Nothing starts the
  server again.
- `fontaine-core/src/agent/mcp.rs` `AgentRuntime::connect_mcp` connects each planned server once,
  pushes a `Connected` into `AgentRuntime::mcp: Vec<Connected>` (`agent.rs:205`), reports failures
  through `Approver::warn`, then calls `refresh_rebuilt_toolsets`. `probe_server` (doctor) uses
  `DenyEveryCall` and a `CountingSink`.
- `fontaine-tui/src/ui/mcp.rs` `start_mcp` records a static `Vec<(String, McpState)>` at startup.
  `fontaine-tui/src/reports.rs` `McpState` is `Connected | Failed | LeftOff | NotTrusted`, and
  `mcp_with_origins` renders `/mcp`. The 2026-10-24 line "`/mcp` takes no `reconnect`/`enable`/`disable` arguments until #35" is among
  the master plan's `### TUI` Decisions.
- MCP results are not capped. rig-rmcp's `mcp_result_output` turns a `CallToolResult` into a
  `ToolOutput`, which the guard's `ResultPipeline` (scanner, redaction) then processes. Other tools
  do cap: `shell` cuts at 2,000 lines / 50 KB (`jobs/cut.rs`) and `webfetch` at 100,000 chars.
  rig-tools ships `Spill(PathBuf)` (`spill.rs`), which creates `<tool>-<n>.txt` files, but
  Fontaine does not use it yet.
- The session scratch directory (`SessionState.scratch: Option<PathBuf>`, reached as
  `AgentRuntime.state.scratch`; `fontaine-core/src/scratch.rs`) is a private workspace root, so `read` of a file in it takes the
  in-root read path.
- `Approver` (`fontaine-core/src/permissions.rs:91`) has `approve`, `notice`, `warn`,
  `guardian_unavailable` and `ask_user`, each with a "nobody can answer" default. `DenyAll` serves
  headless runs. `AfkApprover` short-circuits each prompt while AFK is on. The TUI's
  `ChannelApprover` (`fontaine-tui/src/lib.rs:162`) forwards prompts to dialogs such as
  `question.rs`.

### What Claude Code does (code.claude.com/docs/en/mcp, read 2026-11-08)

- **Elicitation.** Form mode shows a dialog with the server's fields. URL mode asks whether to open
  a link. Claude Code declares `elicitation: {form: {}, url: {}}` to configured servers. An
  `Elicitation` hook can auto-respond. In a non-interactive session it asks the server to cancel,
  without running the hooks.
- **Reconnect.** A remote server that drops is retried up to 5 times with exponential backoff (1 s,
  doubling). While it retries, `/mcp` shows it as pending. After that it is marked failed with
  `MCP server "<name>" disconnected · open /mcp to reconnect`. An HTTP/SSE first connection that
  fails transiently (5xx, connection refused, timeout) is retried up to 3 times, but not after
  authentication or not-found errors. A stdio server that exits mid-session is marked failed in
  interactive sessions; in `-p` it is restarted on the next call. In `-p`, transient failures keep
  retrying in the background for about 30 minutes. `/mcp reconnect all` retries failed servers.
- **Output.** A warning appears at 10,000 tokens. The limit is 25,000 tokens
  (`MAX_MCP_OUTPUT_TOKENS`). A successful result with no image that is over the limit is saved to a
  file under the session's `tool-results`, and the model gets the path. Separately, a successful
  text result over 50,000 chars is saved to a file whatever its token count. A tool can raise the
  char threshold with `_meta["anthropic/maxResultSizeChars"]`, up to a 500,000 ceiling. An
  `isError` result over about 11,000 chars keeps its first 5,000 and last 5,000 chars with a
  marker between them.

## Design

### 1. Output cap: `fontaine-tools/src/mcp/output.rs` (new)

```rust
pub struct OutputLimits {
    /// Token limit (default 25_000; `MAX_MCP_OUTPUT_TOKENS` overrides).
    pub max_tokens: usize,
    /// Where oversized results are saved; `None` falls back to a head/tail cut.
    pub spill: Option<rig_tools::Spill>,
}
pub const DEFAULT_CHAR_LIMIT: usize = 50_000;
pub const MAX_CHAR_LIMIT: usize = 500_000;
pub const ERROR_KEEP: usize = 5_000;        // each end
pub const ERROR_THRESHOLD: usize = 11_000;

/// Applied to a finished call's outcome, before the guard's result pipeline.
pub(crate) fn cap(outcome: ToolOutcome, tool: &str, char_limit: usize, limits: &OutputLimits) -> ToolOutcome;
```

- **Success with no image.** Let `text` be `result_pipeline::text_view(output)`. If
  `text.chars().count() > char_limit` or the token estimate exceeds `max_tokens`, write the full
  text with `Spill::create(<sanitized tool name>)` and replace the output with this message:
  `Output too large (N chars, ~T tokens), saved to <path>. Use read or grep on that file to see it.`
  Estimate tokens as `text.len().div_ceil(4)`, as the guardian does.
- **No spill directory, or the write fails.** Keep the first and last halves of the limit with a
  `[… N chars cut …]` marker: the head comes from `rig_tools::clip`, the tail is cut on a char
  boundary, and the marker counts the removed span in chars. A failed write is reported once
  through the warn callback in `McpOptions`. The fallback is never a silent drop.
- **`isError` with text longer than `ERROR_THRESHOLD`.** rig-rmcp maps `is_error` to
  `Err(ToolExecutionError::other(..).with_model_output(output))`. The cut applies to that `Err`'s
  model output and replaces it via `with_model_output`: keep the first and last `ERROR_KEEP` chars
  (cut the same way) with a marker that counts what was removed. Other errors (transport, timeout)
  are untouched.
- **Results with image content** are passed through unchanged (see Decisions).
- **Per-tool `char_limit`.** It is `DEFAULT_CHAR_LIMIT` unless the tool's `tools/list` entry
  carries a positive integer `_meta["anthropic/maxResultSizeChars"]`, clamped to `MAX_CHAR_LIMIT`.
  The `InteractionTap` already parses `_meta` off `tools/list`, so it now records two facts per
  tool: the marker set it keeps today, and a `HashMap<String, usize>` of char limits. Unlike the
  marker, the latest listing wins, because this value only sizes a buffer and decides nothing.
  Rename `Marked` to `ToolMeta` and move it to `mcp/meta.rs`, with `requires_user_interaction(tool)`
  and `char_limit(tool)`.
- **Where it runs.** Inside `GuardedSink::wrap`'s callback, right after `original.run_with`. The
  scanner and redaction therefore see the replacement message, and the saved file is screened
  when the model reads it, like any `read` result. It is not a pipeline stage because the cap is
  per tool, while the pipeline's stages are tool-agnostic.
- **Config surface.** `GuardedSink` gets `limits: Arc<OutputLimits>`, threaded through `connect` /
  `connect_transport` in a new `McpOptions` struct. `McpOptions` bundles `guard`, `reveal_names`,
  `limits`, `elicitor` (section 3) and a warn callback, so the argument list stops growing. Core
  builds `OutputLimits` from `MAX_MCP_OUTPUT_TOKENS` (read through `setup.env`; an unparsable value
  is warned about once at startup through `Approver::warn` and the default is used) and
  `Spill(scratch.join("mcp-results"))`.

### 2. Reconnect

**fontaine-tools (`servers.rs`).**
- Give `McpError` a `kind: McpErrorKind { Transient, Permanent }` and a boxed `source` field, so
  the cause is no longer flattened into `message`: `message` becomes a short context prefix and the
  rmcp error moves to `source`. This is the "typed error enum where a caller
  dispatches on the kind" case in AGENTS.md. The kind is set where the error is made, from the
  typed rmcp error. `handler.connect` returns `McpClientError`; a `ClientInitializeError` arrives
  inside `McpClientError::Connection(..)`, and a stdio/duplex transport error is `std::io::Error`
  (Transient, though stdio is never retried). A `TransportError { error: DynamicTransportError }`'s
  boxed error downcasts to `StreamableHttpError<reqwest::Error>`. Every variant gets a kind, and the
  `non_exhaustive` enums get a wildcard arm that defaults to `Permanent`:
  - `Transient`: `StreamableHttpError::Client(e)` for every request error (a reset after TCP connect
    is neither `is_connect()` nor `is_timeout()`), `Io`, `Sse`, `UnexpectedEndOfStream`,
    `SessionRecoveryTimeout`; `ClientInitializeError::ConnectionClosed` and `TransportChannelClosed`;
    `McpClientError::Timeout`; `McpClientError::Request(ServiceError)` recursed by inner error
    (`TransportSend` is classified by its inner `DynamicTransportError`; `Timeout` and
    `TransportClosed` are Transient; `McpError` and `UnexpectedResponse` are Permanent); and expiry
    of the connect timeout below.
  - `Permanent`: `AuthRequired` / `InsufficientScope`, `SessionExpired`, `ReinitializationFailed`,
    `ClientInitializeError::JsonRpcError` (a non-2xx answer with a JSON-RPC error body), other
    protocol and deserialize errors, a spawn failure, and `UnexpectedServerResponse(String)`. rmcp
    turns every other non-2xx response (5xx, 429 included) into that variant with no status, so they
    cannot be told apart. A 404 at initialize is also `UnexpectedServerResponse`, since
    `SessionExpired` needs a session id, and so is a 401/403 without `WWW-Authenticate`.
- Every connect attempt (startup retries and reconnects) is wrapped in `tokio::time::timeout`
  (30 s), because `build_client` sets none.

**fontaine-core (`agent/mcp.rs`, new `agent/mcp_supervisor.rs`).**
- Replace `AgentRuntime::mcp: Vec<Connected>` with `mcp: McpServers`:

  ```rust
  pub struct McpServers { slots: BTreeMap<String, Arc<ServerSlot>> }
  pub struct ServerSlot {
      spec: ServerSpec,                    // lets a reconnect run without re-deriving it
      state: RwLock<McpServerState>,       // lock_recovering: display/advisory only
      connection: Mutex<Option<Connected>>, // None while Failed
      watcher: Mutex<Option<JoinHandle<()>>>,
      closing: AtomicBool,                 // set before Fontaine closes a connection itself
  }
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub enum McpServerState {
      Connected,
      Reconnecting { attempt: u32 },   // 1..=5
      Failed(String),                  // rendered error chain
  }
  ```

  `Drop for McpServers` aborts every watcher. Teardown sets `closing` and calls `close()` on each
  connection.
- **Startup (`connect_mcp`).** Every server it attempts gets a slot, so a server that fails at
  startup has a slot in state `Failed(..)` with no connection. For an HTTP server whose first
  attempt fails with `Transient`, retry up to 3 more times after 1 s, 2 s and 4 s. Stdio servers
  and `Permanent` errors are not retried. The planned servers connect concurrently
  (`futures::future::join_all` over the plans, outcomes pushed in plan order), so one slow server
  does not delay the others. `McpOutcome` is unchanged. `probe_server` (doctor) does
  not retry.
- **Watcher.** After each successful connect, spawn a task that waits on
  `connection.closed().await` and then:
  - stdio: sets `Failed("the server exited")` and calls
    `approver.warn("MCP server `<name>` disconnected · run /mcp reconnect <name>")`.
  - HTTP: for attempt 1..=5, sets `Reconnecting{attempt}`, sleeps `1 s << (attempt-1)`, then
    tries `connect_server` (under the connect timeout) with the same `McpOptions` and tool server. On success it stores the new
    `Connected`, sets `Connected`, sends `approver.notice("MCP server `<name>` reconnected")`, calls
    `state.agents().refresh_rebuilt_toolsets(&agent_types)` as `connect_mcp` does, and starts a
    new watcher. On a `Permanent` error, or after 5 failures, it sets `Failed` and warns as above.
    Each attempt goes through `connect_server`, so it emits the existing `mcp_server_connection`
    event with `connected` or `failed`.

  The watcher holds `Arc`s (approver, tool server handle, agent state, options, `network:
  Arc<LoadedNetwork>`, `env: EnvLookup`), never
  `&mut AgentRuntime`. A connection Fontaine closed itself (a manual reconnect, or teardown) must
  not trigger a reconnect: the slot carries a `closing: AtomicBool` that is set before `close()`.
- **Manual reconnect.** `AgentRuntime::reconnect_mcp(&self, target: ReconnectTarget) -> Vec<McpOutcome>`,
  where `ReconnectTarget` is `One(name)` or `AllFailed`. It aborts the watcher, closes the old
  connection (with `closing` set), connects again with no backoff, and installs a new watcher. Only
  servers that have a slot can be reconnected, that is, servers attempted this session, including
  ones that failed at startup. A left-off or untrusted server is never attempted, has no slot and
  gets an error naming why, so `/mcp reconnect` can never bypass workspace trust or server
  approval.
- **Status for frontends.** `AgentRuntime::mcp_states() -> Vec<(String, McpServerState)>`.
- **Choke point.** Every tool registered again after a reconnect goes through the same
  `connect_server`, which wraps it in a fresh `GuardedSink`. Add a case to
  `fontaine-core/tests/choke_point.rs`: drop the `McpConnection`, call `connect_transport` again
  over a new duplex pipe, and assert that every re-registered tool still has a risk class and
  reaches the gate. `reconnect_mcp` itself is covered by the HTTP end-to-end test.

**fontaine-tui.**
- `McpState` adds `Pending { attempt: u32 }`, rendered as `pending (reconnecting, attempt 2 of 5)`.
  `/mcp` merges the TUI's own `LeftOff`/`NotTrusted` entries with `runtime.mcp_states()` at render
  time instead of the startup snapshot. `start_mcp` stops pushing `Connected`/`Failed` into
  `mcp_status` and keeps only `LeftOff`/`NotTrusted` there, still printing its connect notices;
  `/mcp` renders those entries plus `mcp_states()`, sorted by name.
- `/mcp reconnect <name>` and `/mcp reconnect all` call `reconnect_mcp` and print each outcome as a
  notice or error, as `start_mcp` does. Any other `/mcp` argument gets the usage line. Neither
  `enable` nor `disable` is added.
- `runs_mid_turn` keeps only a bare `/mcp` (split on an empty argument, as `/skills` is), because
  those commands run in the synchronous `run_immediate(&AgentRuntime)`, which must not block.
  `/mcp reconnect` is dispatched through `dispatch_inner(&mut AgentRuntime)` and awaited there, with
  a "reconnecting `<name>`" notice drawn before the await.
- `/status` counts connected servers from `runtime.mcp_states()` instead of the static `mcp_status`
  snapshot. `McpState` gets no `Connecting` mapping because `McpServerState` has none.

**Headless.** The same retry and backoff apply. A stdio server that exits stays failed for the rest
of the run (see Decisions).

### 3. Elicitation

**fontaine-tools (`mcp/elicitation.rs`, new).**

```rust
pub struct ElicitForm { pub server: String, pub message: String, pub schema: rmcp::model::ElicitationSchema }
pub enum ElicitReply { Accept(serde_json::Map<String, Value>), Decline, Cancel }
pub trait Elicitor: Send + Sync {
    fn elicit<'a>(&'a self, form: ElicitForm) -> BoxFuture<'a, ElicitReply>;
}
pub struct CancelAll; // Elicitor that always answers Cancel (probe, tests)
```

- **Capability.** `client_config()` declares
  `elicitation: ElicitationCapability::new().with_form(FormElicitationCapability ..)`, which is form
  mode only.
- **Interception.** Add an `ElicitationTap<T>` transport wrapper, composed around
  `InteractionTap` (or merged into one `ServerTap` if that reads cleaner). It intercepts in
  `receive()`:
  - A `ServerJsonRpcMessage::Request` carrying `ServerRequest::ElicitRequest` is not passed to
    rmcp. A request with `ElicitRequestParams::FormElicitationParams` (a request with no `mode`
    means form) spawns a task that calls `elicitor.elicit(form)`. One with
    `ElicitRequestParams::UrlElicitationParams` is answered `Decline` at once, with a
    `tracing::warn`.
  - A `notifications/cancelled` for a pending elicitation id aborts that task; nothing is sent
    back. The notification is still forwarded to rmcp.
  - Everything else passes through unchanged.

  Pending elicit tasks are held in a `JoinSet` (or as `AbortHandle`s) that is aborted when the tap
  drops, so a dead connection leaves no dialog task behind. Answers (`ElicitResult` with an
  `ElicitationAction`) arrive on an `mpsc`. `receive()` `select!`s between `inner.receive()` and
  that channel (both cancel-safe). For each answer it builds the response with
  `ClientJsonRpcMessage::response(ClientResult::ElicitResult(..), id)` and `tokio::spawn`s the
  `'static` future returned by `self.inner.send(..)`, as rmcp does, because rmcp drops the
  `receive` future often (it is one `select!` branch) and an awaited send could be dropped or
  repeated.
- **Validation before sending `Accept`.** The answer must match the requested schema: required
  fields present, string length bounds, string `format` (email, uri, date, date-time), number ranges, enum
  membership, multi-select `min_items` / `max_items`, and boolean type. The
  dialog validates as the human types, and the tap validates again as a backstop. If the backstop
  fails, the tap answers `Decline` and logs a warning.
- The doctor probe uses `CancelAll`.

**fontaine-core.**
- `Approver::elicit` (`fn elicit<'a>(&'a self, form: ElicitForm) -> BoxFuture<'a, ElicitReply>`) defaults to `Cancel`, so
  `DenyAll` (headless) cancels. `AfkApprover` answers `Cancel` while AFK is on and forwards
  otherwise.
- `ApproverElicitor(Arc<dyn Approver>)` implements `Elicitor` and is the one `connect_mcp` passes
  in `McpOptions`.
- Each elicitation writes an audit record and a telemetry event `mcp_elicitation` that carries
  `action` (accept/decline/cancel) and the server (`third-party` unless `log_tool_details`). It
  never carries the message or the answer. Answers never enter the transcript or the model's
  context.

**fontaine-tui (`elicitation.rs`, new dialog, modelled on `question.rs`).**
- The header reads `MCP server `<name>` asks for input`, followed by the server's message, cleaned
  of control characters with the same `clean` helper as `question.rs` (made `pub(crate)`;
  newlines in the message are kept, other control characters stripped). Then comes one row per
  schema property, in `property_order` (by key when it is `None`): its `title` (or the key) with `description`, and a required
  marker.
  - A string, number or integer row is an `InputBuffer` field, pre-filled from the property's
    `default`.
  - A boolean row is a checkbox.
  - An enum row is a single-select list; a multi-select enum gets checkboxes. Titled enums and the
    legacy `enumNames` show their titles.
- Keys: Tab/Shift-Tab and Up/Down move between fields, Enter submits (blocked with an inline error
  while validation fails), Esc cancels (`Cancel`), and Ctrl-D declines (`Decline`).
  `ANSWER_GRACE` applies as it does in the other dialogs.
- A footer line reads "This goes to the server, not to the model. Don't enter secrets you wouldn't
  give this server."
- `ChannelApprover::elicit` queues the dialog the same way `ask_user` does through a new `Prompt`
  variant. `App::answer_for_afk` (`app.rs`), which closes dialogs already open when AFK turns on and
  drains approvals, `unavailable` and `questions` by name, also answers an open elicitation dialog
  with `Cancel`, and `App::prune_closed` drops closed-reply ones. The dialog is drawn in `view.rs`
  beside the question panel. `linear.rs` renders the dialog in screen-reader mode.
- `fontaine_tools::mcp` re-exports the rmcp elicitation schema types the dialog matches on
  (`ElicitationSchema`, `PrimitiveSchema` and the enum schema types), since fontaine-tui has no
  rmcp dependency.

## Implementation Steps

1. **Meta and output cap (tools).** Move `Marked` into `mcp/meta.rs` as `ToolMeta`, adding the
   char-limit map. Add `mcp/output.rs` with `cap`. Add `McpOptions`. Wire the cap into
   `GuardedSink::wrap`. Write unit tests.
2. **Error kind (tools).** Add `McpErrorKind` and the boxed `source` to `McpError`, classify at the
   error sites in `servers.rs`, and wrap each connect attempt in the 30 s timeout. Write unit tests.
3. **Elicitation tap (tools).** Add `mcp/elicitation.rs`: the types, `Elicitor`, `CancelAll` and
   `ElicitationTap`. Declare the form capability in `client_config()`. Add the rmcp `elicitation`
   feature to the dev-deps. Add an eliciting tool to `tests/mcp_common/server.rs`. Write unit tests
   with the `Script` transport pattern from `interaction.rs`.
4. **Core wiring.** Add `Approver::elicit` (default `Cancel`), the `AfkApprover` override,
   `ApproverElicitor`, and `OutputLimits` from `MAX_MCP_OUTPUT_TOKENS` + scratch.
   `connect_mcp` builds and passes `McpOptions` internally; its signature is unchanged.
   `probe_server` builds its `McpOptions` internally too (with
   `CancelAll` and no spill), so `doctor_cmd.rs` is unchanged. Add audit and telemetry for
   elicitation.
5. **Supervisor (core).** Add `agent/mcp_supervisor.rs` with `McpServers`, `ServerSlot` (one per
   attempted server, holding its `spec`), `McpServerState`, the startup retry, the watcher with
   backoff, `reconnect_mcp` and `mcp_states`. Replace `AgentRuntime::mcp`. Add the choke-point
   case (drop the connection, `connect_transport` again over a new duplex pipe).
6. **TUI.** Add `McpState::Pending`, live `/mcp` and `/status` rendering from `mcp_states()`,
   `/mcp reconnect <name|all>` (command split and parse in `commands.rs`, dispatched through
   `dispatch_inner`), the `elicitation.rs` dialog (rendered in `view.rs`), the `Prompt` variant,
   `ChannelApprover::elicit`, and the `App::answer_for_afk` / `App::prune_closed` handling.
   Plumb the new prompt through every place that enumerates prompts: `notify.rs` `PromptKind` and
   `App::pending_prompt` (bell and desktop notification); `spinner.rs` pause; in `app.rs`, key
   routing in `handle_key`, the open-prompt checks, and `deny_pending` (Esc interrupt sends
   `Cancel`); in `ui/mod.rs`, the exhaustive `Prompt` matches in `on_prompt`, `drain_prompts` and
   `answer_unattended` (answers `Cancel`).
7. **Integration tests** (see Testing Strategy).
8. **Master plan.** Add the Decisions lines below to the end of the `### MCP` subsection of
   `tasks/fontaine_harness_plan.md`, except the `/mcp reconnect` supersede line, which goes at the
   end of `### TUI` in the form "- DATE (supersedes the 2026-10-24 "…" line): …".
9. **Docs** (see Documentation).
10. **Follow-up issues.** File issues labelled `area:mcp`, linked from #486, for: URL-mode
    elicitation; the `Elicitation` hook event; restarting stdio servers on the next call in
    headless runs, plus the background retry for transient failures; the 10,000-token warning;
    an upstream rig-rmcp `create_elicitation` hook, which would let the tap be retired; and an
    upstream rmcp request for a typed HTTP status on connect errors, so 5xx/429 can be retried.

## Files to Modify

- `rust/fontaine-tools/src/mcp.rs` — module list, re-exports (including the rmcp elicitation schema
  types)
- `rust/fontaine-tools/src/mcp/interaction.rs` → `rust/fontaine-tools/src/mcp/meta.rs`
- `rust/fontaine-tools/src/mcp/output.rs` (new)
- `rust/fontaine-tools/src/mcp/elicitation.rs` (new)
- `rust/fontaine-tools/src/mcp/register.rs` — cap in the callback, `ToolMeta`, `McpOptions`
- `rust/fontaine-tools/src/mcp/servers.rs` — `McpOptions`, `McpErrorKind`, `McpError.source`,
  connect timeout, capability, tap composition
- `rust/fontaine-tools/tests/mcp_registration.rs` — updated signatures, new cases
- `rust/fontaine-core/src/permissions.rs` — `Approver::elicit`, `AfkApprover`
- `rust/fontaine-core/src/agent/mcp.rs`, `rust/fontaine-core/src/agent/mcp_supervisor.rs` (new),
  `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-core/src/audit.rs` — elicitation record
- `rust/fontaine-core/tests/choke_point.rs` — reconnect case
- `rust/fontaine-tui/src/elicitation.rs` (new), `rust/fontaine-tui/src/lib.rs`,
  `rust/fontaine-tui/src/app.rs` (`answer_for_afk`, `prune_closed`), `rust/fontaine-tui/src/view.rs`,
  `rust/fontaine-tui/src/reports.rs`, `rust/fontaine-tui/src/notify.rs`,
  `rust/fontaine-tui/src/spinner.rs`,
  `rust/fontaine-tui/src/ui/mcp.rs`, `rust/fontaine-tui/src/ui/mod.rs` (`dispatch_inner`), `rust/fontaine-tui/src/ui/commands/*` (`/mcp reconnect`, `/status`),
  `rust/fontaine-tui/src/question.rs` (`clean`), `rust/fontaine-tui/src/linear.rs`
- `rust/fontaine-tui/src/commands.rs` — `runs_mid_turn`, hint, split, parse tests
- `rust/fontaine-tools/Cargo.toml`, `rust/fontaine/Cargo.toml` — `elicitation` feature on the
  `rmcp` dev-dependency
- `rust/fontaine-tools/tests/mcp_common/server.rs` — an eliciting tool (used by
  `rust/fontaine/examples/repo_mcp_server.rs`)
- `rust/fontaine-core/tests/` — HTTP reconnect test with a wiremock JSON-RPC responder
- `docs/mcp.md`, `docs/tui.md`, `docs/headless.md`, `docs/afk.md`, `docs/telemetry.md`,
  `docs/doctor.md`, `docs/configuration.md`
- `tasks/fontaine_harness_plan.md` — Decisions

## Decisions

Lines to add to the master plan's `### MCP` subsection (dated on merge), except the `/mcp reconnect`
line, which goes at the end of `### TUI`:

- Elicitation declares and serves form mode only. URL mode is declined, because opening a browser
  on a server's say-so is an outward action that needs its own design (follow-up issue).
- There is no `Elicitation` hook event yet. Headless and AFK answer `cancel`, as Claude Code's
  non-interactive sessions do (follow-up issue).
- Elicitation is served from a transport tap, because rig-rmcp's `McpClientHandler` does not expose
  `create_elicitation`. Wrapping the handler would mean reimplementing `connect`, whose initial
  fetch is `pub(crate)`, and a fork breaks the exact-version pin.
- In headless runs a stdio server that exits stays failed and is not restarted on the next call,
  and there is no 30-minute background retry after the five reconnect attempts (follow-up issue).
- Oversized MCP results are saved to `<scratch>/mcp-results/`, not to a `tool-results` directory
  under the data dir. The scratch directory is the session's private root, so reading the file
  needs no out-of-root gate, and the file is cleaned up with the session.
- There is no 10,000-token warning. The cap already keeps oversized output out of context
  (follow-up issue).
- Results that carry image content are not capped or spilled. Fontaine has no image token
  estimate for them yet.
- DATE (supersedes the 2026-10-24 "`/mcp` takes no `reconnect`/`enable`/`disable` arguments until #35."
  line): `/mcp reconnect <name|all>` is offered. `enable` and `disable` are still not offered.
- The 2026-10-24 line "`/help`, `/status`, `/permissions`, `/mcp`, `/hooks` and `/skills` are
  read-only transcript notices ..." no longer covers `/mcp reconnect`, which acts on servers.
- `/mcp reconnect` only restarts servers attempted this session, including ones that failed at
  startup. It never starts a left-off or untrusted one.
- The disconnect text reads "run /mcp reconnect <name>" where Claude Code says "open /mcp to
  reconnect", because Fontaine's `/mcp` is a read-only notice, not a menu, so the command has to be
  named.
- The reconnect watcher stops early on a `Permanent` error rather than spending all 5 attempts,
  because retrying an auth or protocol failure cannot succeed.
- 5xx and 429 responses at connect are not retried, unlike Claude Code, because rmcp keeps no status
  on `UnexpectedServerResponse` and they are counted `Permanent` (follow-up issue asks rmcp for a
  typed status).

## Security

- Elicitation content is untrusted server text shown only to the human, cleaned of control
  characters. The answer goes only to that server, never to the model, transcript, audit or
  telemetry.
- Elicitation never changes a permission decision. No dialog is reachable in headless runs, in AFK,
  or in the doctor probe.

## Documentation

- `docs/mcp.md`: new sections "Requests for input" (elicitation: form mode, what headless/AFK do,
  that answers go only to the server), "Reconnecting" (backoff schedule, stdio behavior,
  `/mcp reconnect`), and "Large outputs" (limits, `MAX_MCP_OUTPUT_TOKENS`, `maxResultSizeChars`,
  where files go), and a `## Telemetry` section for the `mcp_elicitation` event. Add to the line
  at `docs/mcp.md:61` (keep its existing text) for elicitation, and update `docs/mcp.md:63-64` for
  the startup retry.
- `docs/tui.md`: the `/mcp reconnect` argument, the pending state, and the elicitation dialog keys.
- `docs/headless.md` and `docs/afk.md`: elicitation is answered `cancel`.
- `docs/telemetry.md`: an `mcp_elicitation` event row, and a note that reconnect attempts emit
  `mcp_server_connection`.
- `docs/doctor.md`: the probe does not retry and cancels elicitation.
- `docs/configuration.md`: `MAX_MCP_OUTPUT_TOKENS` in its environment-variable list.

## Testing Strategy

Unit tests (`cargo test`):
- `output.rs`:
  - The char threshold triggers a spill.
  - The token threshold triggers a spill.
  - `maxResultSizeChars` raises the limit, is clamped at 500,000, and is ignored when it is not a
    positive integer.
  - An image result is untouched.
  - With no spill dir, and with a failing write, the result is a head/tail cut with a marker.
  - Error results above and below 11,000 chars are handled correctly.
  - A spilled file holds the full text.
- `meta.rs`: the marker semantics carried over, and the latest listing wins for char limits.
- `elicitation.rs` (scripted transport):
  - A form request is intercepted, not forwarded.
  - The answer is sent with the request's id.
  - Cancel and decline map to the right protocol actions.
  - URL mode is declined.
  - `notifications/cancelled` aborts the pending task and sends no response.
  - Dropping the tap aborts a pending elicitation.
  - Other messages pass through untouched.
  - The schema backstop declines an invalid `Accept`.
- `servers.rs`: `McpErrorKind` classification for refused connections, a connection reset after TCP
  connect (a raw `tokio::net::TcpListener` that accepts and drops the socket, since wiremock cannot
  drop a connection), an end of stream during initialize, a `tools/list` timeout, a `ServiceError`
  `McpError` (Permanent) and `TransportSend` wrapping `AuthRequired` (Permanent), and expiry of the connect timeout (`Transient`);
  401 with and without `WWW-Authenticate`, 5xx, 404 at initialize, a non-2xx JSON-RPC error body,
  and spawn failures (wiremock where an HTTP server is needed; `Permanent`).
- Supervisor (`tokio::time::pause`):
  - The backoff schedule is 1, 2, 4, 8, 16 s.
  - The server is marked failed after 5 attempts and after a `Permanent` error.
  - A stdio exit marks the server failed with no retry.
  - A `closing` connection does not trigger a reconnect.
  - `reconnect_mcp` refuses unknown, left-off and untrusted names.
  - Startup retries happen 3 times on `Transient` only.
- `permissions.rs`: `DenyAll`/default `elicit` cancels, and `AfkApprover` cancels while AFK is on.
- TUI (`TestBackend`):
  - Elicitation dialog navigation, validation blocking submit, and Esc/Ctrl-D mapping.
  - `/mcp` renders `pending (…)`.
  - `/mcp reconnect` argument parsing.

Integration:
- `fontaine-tools/tests/mcp_registration.rs`:
  - An in-process rmcp server whose tool calls `elicit` gets `cancel` through `CancelAll`, and an
    `Accept` payload through a scripted elicitor.
  - A tool returning 60,000 chars yields the spill message, and the file exists.
- `fontaine-core/tests/choke_point.rs`: drop the `McpConnection`, call `connect_transport` again
  over a new duplex pipe, and assert every re-registered tool still has a risk class and reaches
  the gate. A silent break here would let an unwrapped tool through, so this is automated.
- HTTP reconnect end to end (`fontaine-core/tests`): a wiremock streamable-HTTP JSON-RPC responder
  (copied from `fontaine-tools/tests/mcp_registration.rs`, or moved somewhere both crates can use) is
  stopped and restarted on the same port. Assert that the state goes
  `Reconnecting → Connected` and that the tool works again. This is a concurrency path a scripted
  transport cannot reproduce, because it exercises rmcp's real HTTP session teardown and
  re-initialize.
- Headless (`fontaine -p` with a fake stdio server, following existing headless MCP tests): a tool
  that triggers elicitation gets `cancel` and the run completes.

## Manual Verification

1. `cargo run -p fontaine` with `rust/fontaine/examples/repo_mcp_server.rs` (built on the
   eliciting tool in `mcp_common/server.rs`), configured under `[mcp.servers]`. Ask the model
   to call the tool. The form dialog appears, and submitting returns the values to the server.
   This is checked by hand because how the dialog looks has to be judged by eye.
2. Reproduce the dialog layout in a real tmux at 80×24 and in screen-reader mode, as
   `tasks/design/testing.md` requires for layout.
