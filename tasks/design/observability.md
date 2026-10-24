# Observability Design

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Observability

**Facade: `tracing` everywhere.** Every crate instruments with `tracing` spans/events only — no
crate but the `fontaine` bin depends on a subscriber or exporter (rig already emits GenAI-semconv
`tracing` spans, so this is the one facade that sees them). for telemetry, `fontaine-guardian` uses only
`tracing` (no subscriber or exporter) and stays embeddable: with no subscriber installed its spans are no-ops. The bin
owns the global subscriber (`rust/fontaine/src/telemetry/`): a `tracing_subscriber::registry()`
with (a) the local debug-file layer (`--debug-file <path>`, `FONTAINE_LOG` env filter; owner-only; Claude
Code's `--debug-file` analog), and (b) when `[telemetry] enabled`, one export backend:

- `micromegas` (default backend): `micromegas-telemetry-sink` guard built with local sink off,
  global tracing capture off (its `TracingCaptureLayer` is composed into Fontaine's registry for
  events), system monitor off, explicit URL + `ApiKeyRequestDecorator` (never the implicit env
  fallback). micromegas does not capture `tracing` spans, so a Fontaine **`SpanBridge`** layer maps
  each allowlisted span onto micromegas **async span events**, which carry `span_id`,
  `parent_span_id` and `depth` and are reassembled into trees by the analytics side (`async_events`
  view, `process_spans(…, 'async')`): `on_new_span` →
  `dispatch::on_begin_async_named_scope_dyn(location, name, parent_mm_id, depth)` with the parent
  taken from the `tracing` parent span's stored micromegas id (0 at a root); the returned id and depth
  live in the span's extensions; `on_close` → `dispatch::on_end_async_named_scope_tagged(…)`
  carrying the span's recorded fields as properties. One `SpanLocation` per `tracing` callsite is
  leaked once and cached (bounded by the callsite count). Async span events ride micromegas thread
  streams, which exist only while CPU tracing is enabled: the sink builder switches it on
  (`with_cpu_tracing_enabled(true)`; the bin no longer touches its environment), the tokio runtime is built with
  `TracingRuntimeExt::with_tracing_callbacks()`, which gives every worker a thread stream and unregisters it as the worker stops; the bridge never creates a stream itself (micromegas keeps a raw pointer to each one, so a thread that exited without unregistering would leave it dangling). The same layer derives the metrics (`imetric!`/`fmetric!`) on close.
  The stock `TracingCaptureLayer` can capture spans too (`capture_spans`), and the stock layer runs with
  it off. It exports every recorded field verbatim, with no hook to drop, redact, truncate or rewrite one,
  and its span location carries the callsite's absolute file path; the bridge exists to keep both
  out of the export. A parity test pins that the two build the same tree. The bridge's span half is deleted
  when upstream has a field hook and a switch that leaves `file` / `line` out of the location
  (madesroches/micromegas#1725). Then `MicromegasExport::span_begin` / `span_end` and the location cache go,
  `capture_spans` becomes `t.traces`, and the capture layer's filter widens to `is_exported_span ||
  is_exported_event`. Field collection stays in the bridge for metric derivation on both backends. The swap
  also fixes orphaned parents: on a thread with no stream the bridge allocates an id whose begin event is
  dropped, where upstream re-parents to the nearest captured ancestor.
- `otlp` (fallback; any OTel collector, including micromegas's `/ingestion/otlp`): OTLP/HTTP
  protobuf via `opentelemetry-otlp` (no gRPC); spans via `tracing-opentelemetry`, events via
  `opentelemetry-appender-tracing`, metrics from the same `SpanBridge` metric derivation through an
  OTel `Meter`.

Both backends sit behind a per-layer **target allowlist** (`fontaine::telemetry`, `rig::completions`,
`rig::agent_chat`, `rig_agent::agent::telemetry`): nothing else rig or a dependency logs is exported (it reaches only the
opt-in local debug file). Fontaine's own telemetry fields are content-free by construction;
content fields exist only behind the opt-in keys below, and rig's content recording follows
them via `record_content_telemetry`.

**What is emitted** (names mirror Claude Code with a `fontaine.` prefix):
- Spans: `fontaine.session` > `fontaine.interaction` (one user turn; `prompt.id`) > rig
  `invoke_agent`/`chat` and `fontaine.tool` > `fontaine.permission` (> `fontaine.hook`,
  `fontaine.guardian`, `fontaine.tool.blocked_on_user`) + rig `execute_tool`; `fontaine.subagent`
  parented to the spawning tool call; `fontaine.mcp.call`. Both backends export this as a tree.
- Metrics: `fontaine.session.count` (`start_type` = fresh/resume/continue, as Claude Code — a fork
  takes the value of the flag it forks from; `headless` = true/false), `fontaine.token.usage` (`type` =
  input/output/cacheRead/cacheCreation/reasoning, `model`, `query_source` =
  main/guardian/subagent/scanner), `fontaine.cost.usage` (USD; absent when rig has no price),
  `fontaine.tool_decision.count` (`tier`, `decision`), `fontaine.guardian.latency` (ms),
  `fontaine.guardian.fail_closed.count` (`reason`). No `prompt.id`/`tool_use_id` on metrics.
- Events: `api_request` (model, tokens, cost, duration, query_source), `api_error` (`model`, `provider`,
  `query_source`, `agent.id`, `error_class` = rig's `ErrorKind::code()`, `status_code` when the failure
  carries one; never the message, which can echo the request, and no attempt count: rig's retries are not
  visible here; emitted from the turn loop, where the typed error is, not from the completion span, which
  closes the same way on error and on a cancelled call),
  `user_prompt` (length only), `tool_result` (tool_name, risk_class, success, duration, sizes,
  tool_source builtin/mcp), `tool_decision` (CC attributes + `tier` = the `permissions.md` §Core data flow tier enum, `permission_mode`, `headless`, `guardian.{model,profile,verdict,
  confidence,cache_hit,latency_ms}`, `fail_closed_reason` = error | timeout | unparseable |
  low_confidence | unavailable, with the audit label: provider error, stream drop or connection reset → `error` / `GuardianUnavailable`; missing credential or unavailable pinned guardian → `unavailable` / `GuardianUnavailable`; timeout → `timeout` / `Timeout`; unparseable → `unparseable` / `Unparseable`; low confidence → `low_confidence` / `LowConfidence`), `permission_mode_changed`, `mcp_server_connection`, `subagent`
  (spawn | complete | killed | budget_exhausted), `internal_error` (`class` = `audit_write` | `ledger_write`, on every failed write; never the io error).
- **Dropped guardian spend.** When an enforcing screen settles a call, the guardian request racing it is
  dropped; it has usually been sent and billed, but its span closes without usage. The cascade estimates
  its input as a quarter of the rendered prompt's bytes (`Guardian::dropped_estimate`); the audit record
  carries it with the guardian's own provider, model id and `priced`, `tool_decision` projects it as
  `guardian.dropped_*`, and the bridge adds `fontaine.token.usage` / `fontaine.cost.usage` with
  `query_source = guardian` and `estimated = true`. `/cost` and budgets do not count it.
- **Analytics.** `analytics/notebooks/fontaine-overview.json` is a micromegas notebook (loaded with
  `micromegas-screens`) over the base `log_entries` and `measures` views. Event cells work on both
  backends; metric cells are native-only (OTLP histograms are dropped, OTLP sums are cumulative); it has no
  span cells because the span tables differ (`async_events` vs `otel_spans`). `docs/telemetry.md` holds the
  retention recipe and how to load it.
- Standard attributes: `session.id`, `agent.id`/`agent.type`, `app.version`; events add
  `prompt.id` and `event.sequence`.

**Audit log vs telemetry.** The pipeline builds one typed `DecisionRecord` per gate decision; it is
written to the audit log **first** (append + flush, always on, unaffected by `[telemetry]`), then
projected to the content-free `tool_decision` event. The audit log is the security record —
complete, local, synchronous, keyed by agent id, feeding `/denied` and the agents panel;
telemetry is best-effort, lossy under backpressure (bounded queues drop), redacted, and may be
off. Neither derives from the other; consistency comes from the shared struct.

**Failure isolation.** Telemetry can never block or change a decision: no pipeline/guardian
code reads telemetry state; exporters run off-thread with bounded queues that drop on overflow;
exporter init failure logs to the debug file, shows a one-line notice, and the run continues
with export off (a malformed `[telemetry]` value is still a config error, exit 2); shutdown
flush is capped (2 s) on both TUI exit and `-p` exit (`main` drops the guard, then returns an
`ExitCode` — `std::process::exit` would skip the flush), after which data is dropped.

**Secrets.** Credentials are env-var names only (`api_key_env`), never inline. The variables
removed from the shell tool's and hooks' child environment are listed in the Phase 2 plan, step 1. Opt-in content still passes through `rig_core::redact` and is
truncated at `content_max_bytes`.
