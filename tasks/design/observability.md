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
  streams, which exist only while CPU tracing is enabled: until micromegas grows a builder knob, the bin
  sets `MICROMEGAS_ENABLE_CPU_TRACING=true` in its own environment before building the guard (while
  still single-threaded; child environments scrub it), the tokio runtime is built with
  `TracingRuntimeExt::with_tracing_callbacks()`, and the bridge calls `init_thread_stream()` before
  emitting from any other thread. The same layer derives the metrics (`imetric!`/`fmetric!`) on close.
  Once micromegas captures `tracing` spans with parentage upstream, the bridge shrinks to the stock
  layer plus Fontaine's metric derivation.
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
- Metrics: `fontaine.session.count` (`start_type`), `fontaine.token.usage` (`type` =
  input/output/cacheRead/cacheCreation/reasoning, `model`, `query_source` =
  main/guardian/subagent/scanner), `fontaine.cost.usage` (USD; absent when rig has no price),
  `fontaine.tool_decision.count` (`tier`, `decision`), `fontaine.guardian.latency` (ms),
  `fontaine.guardian.fail_closed.count` (`reason`). No `prompt.id`/`tool_use_id` on metrics.
- Events: `api_request`/`api_error` (model, tokens, cost, duration, query_source),
  `user_prompt` (length only), `tool_result` (tool_name, risk_class, success, duration, sizes,
  tool_source builtin/mcp), `tool_decision` (CC attributes + `tier` = the `permissions.md` §Core data flow tier enum, `permission_mode`, `headless`, `guardian.{model,profile,verdict,
  confidence,cache_hit,latency_ms}`, `fail_closed_reason` = error | timeout | unparseable |
  low_confidence | unavailable, with the audit label: provider error, stream drop or connection reset → `error` / `GuardianUnavailable`; missing credential or unavailable pinned guardian → `unavailable` / `GuardianUnavailable`; timeout → `timeout` / `Timeout`; unparseable → `unparseable` / `Unparseable`; low confidence → `low_confidence` / `LowConfidence`), `permission_mode_changed`, `mcp_server_connection`, `subagent`
  (spawn | complete | killed | budget_exhausted), `internal_error` (class only).
- Standard attributes: `session.id`, `agent.id`/`agent.type`, `app.version`; events add
  `prompt.id` and `event.sequence`.

**Audit log vs telemetry.** The pipeline builds one typed `GateDecision` per tool call; it is
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
