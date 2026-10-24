# Full Observability Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/139

## Overview

Fontaine's telemetry exports a span tree, metrics and events through two backends, but it still
works around missing pieces of micromegas, some events in the design are never emitted, and nothing
exists on the analytics side. micromegas `main` now has span capture in its stock layer, a builder
switch for CPU tracing, per-stream-tag retention, and listing of built-in view sets. This plan moves
Fontaine onto that micromegas commit and removes the CPU-tracing environment workaround. It checks
the upstream span capture against the bridge and records what still blocks swapping one for the
other. It emits the designed-but-missing events (`api_error`, `internal_error`) and an estimate of
the guardian spend dropped when the screen settles a call. It documents what is emitted but
missing from the docs, documents a retention recipe and the audit-log join, and ships a micromegas
notebook under a new root `analytics/` folder.

## Current State

- **Dependencies.** `rust/Cargo.toml:51-53` pins `micromegas-tracing`, `micromegas-telemetry-sink`
  and `micromegas-transit` to `=0.32.0`. `rust/deny.toml:39-42` denies unknown git sources. The
  latest micromegas release on crates.io is 0.32.0. Commit `7a08f6541` on micromegas `main`
  (workspace version `0.33.0`, unreleased) also moves the sink to reqwest 0.13, which removes the
  second reqwest that 0.32 brings in.
- **Backend wiring.** In `rust/fontaine/src/telemetry/mod.rs:185-203` the micromegas arm calls
  `micromegas::enable_cpu_tracing()`, which does an `unsafe` `set_var` of
  `MICROMEGAS_ENABLE_CPU_TRACING` (`telemetry/micromegas.rs:23-37`). It then builds the guard and
  composes two layers: `SpanBridge<MicromegasExport>`, filtered by `is_bridged`, and the stock
  `TracingCaptureLayer { max_level: Info }` for events only, filtered by `is_exported_event`. The
  child environment already drops every `MICROMEGAS_*` variable by prefix
  (`fontaine-core/src/tools.rs:23-32`), so the variable has no entry of its own to remove.
- **Span bridge.** `telemetry/span_bridge.rs` collects each allowlisted span's fields
  (`on_new_span` / `on_record`). On close it derives metrics and the `api_request` / `user_prompt`
  events, filters the fields through `ContentPolicy::export_fields`, and hands begin and end to the
  backend's `Export`. `MicromegasExport` (`micromegas.rs:63-171`) maps those to
  `on_begin_async_named_scope_dyn` / `on_end_async_named_scope_tagged`. It leaks one
  `SpanLocation` per callsite, with `file: ""` and `line: 0`. OTLP's export uses the trait's
  default `span_begin` / `span_end` (no-ops), because `tracing-opentelemetry` exports the spans.
  The parent is `span.parent()` from the layer's filtered context, which under a per-layer filter
  is the nearest *enabled* ancestor.
- **Content policy.** `telemetry/content.rs` classifies each field. Open fields pass through.
  Prompt, response and tool fields pass only behind their `log_*` key, redacted and then
  truncated. Unknown `gen_ai.*` / `rig.*` fields are dropped. A `gen_ai.tool.name` starting with
  `mcp__` is *rewritten* to `third-party` unless `log_tool_details` is set.
- **Upstream span capture at `7a08f6541`** (`rust/telemetry-sink/src/tracing_interop.rs`).
  `TracingCaptureLayer` has a new `capture_spans: bool` field. The struct literal in
  `micromegas::capture_layer()` must name it. It parents a span to its nearest captured ancestor
  and records only on threads that already have a stream. Two properties rule it out as a
  replacement today:
  1. It exports every recorded field verbatim. It has no hook to drop, redact, truncate or
     rewrite a field, so rig's `gen_ai.input.messages` / `gen_ai.tool.call.arguments` and MCP
     tool names would leave the process unfiltered.
  2. Its `SpanLocation` carries `metadata.file()` and `metadata.line()`. For a dependency callsite
     that is an absolute path under the user's home (`~/.cargo/registry/...`). The master plan's
     2026-10-21 Phase 1 decision says source locations are not exported.
  A `filter_fn` on metadata can only keep or drop whole spans, and `tracing` exposes no way to
  rebuild `Attributes` with different values, so neither gap can be closed from Fontaine's side.
- **CPU tracing knob.** `TelemetryGuardBuilder::with_cpu_tracing_enabled(bool)` exists at
  `7a08f6541`. The explicit value wins over the environment variable.
- **Events.** `api_error` and `internal_error` are in `tasks/design/observability.md` but are not
  emitted anywhere in `rust/`. `permission_mode_changed` is emitted (`fontaine-core/src/gate.rs:
  324-343`, fields `from` and `to`) but is missing from `docs/telemetry.md`. The `tool_decision`
  event (`fontaine-core/src/permissions.rs:1490-1521`) already carries `guardian.verdict`,
  `screen.outcome`, `screen.risk`, `screen.allow_risk`, `screen.steering`, `screen.model` and
  `screen.latency_ms`, shadow mode included, but `docs/telemetry.md` lists none of them. That
  covers the telemetry half of #124 ("per-question probabilities next to the guardian verdict").
- **Turn failures.** `fontaine-core/src/turn.rs:196-201` keeps a stream error as `failure:
  Option<PromptError>`. A failed main-agent or subagent model call never reaches telemetry. Guardian
  failures already reach it as `fail_closed_reason`, and scanner failures as `scanner_verdict =
  error`.
- **Sink failures.** `SessionState::sink_failed` (`gate.rs:638-644`) reports audit and ledger write
  failures through `tracing::warn!` with no target, so they reach the debug file only, and through
  `Approver::warn`.
- **Dropped guardian spend.** `fontaine_guardian::cascade::adjudicate` (`cascade.rs:32-80`) races
  the screen against the LLM stage. When an enforcing screen stops or allows the call, the LLM
  future is dropped. Its request has usually been sent and billed by then, but the `chat` span
  closes without usage, so `fontaine.token.usage` and `fontaine.cost.usage` count nothing for it.
- **Analytics.** There are no queries, notebooks or view sets. The micromegas native backend lands
  events in `log_entries`, with fields as `properties`, read with `property_get`. Metrics land in
  `measures`, and spans in `async_events` / `process_spans(…, 'async')`. Through `/ingestion/otlp`,
  logs and metrics land in the same `log_entries` and `measures`, but spans land in `otel_spans`.
  Notebooks are JSON screens that `micromegas-screens apply` loads ("screens as code"). View sets
  are `CREATE MATERIALIZED VIEW` DDL, which only an admin can apply.

## Design

### 1. micromegas at `7a08f6541`, CPU tracing by builder

- In the workspace, point all three crates at
  `{ git = "https://github.com/madesroches/micromegas", rev = "7a08f6541…" }` (the full SHA).
  `rust/deny.toml` gains `allow-git = ["https://github.com/madesroches/micromegas"]` under
  `[sources]`, with a comment saying it goes away once 0.33 is on crates.io.
- `build_guard` adds `.with_cpu_tracing_enabled(true)`. Delete `CPU_TRACING_VAR`,
  `enable_cpu_tracing()` and its call in `mod.rs`. The `unsafe` block goes with them.
  `TracingRuntimeExt::with_tracing_callbacks()` stays.
- `capture_layer()` sets `capture_spans: false`. The bridge still owns the spans (see §2), and two
  layers recording the same span would export it twice.
- Fix whatever else 0.33 renames. Its changelog flags the `ListViewSetDefinitionsTableFunction`
  and retention signatures, which Fontaine does not use. The test helpers in
  `tests/telemetry_micromegas.rs` (`init_in_memory_tracing`) are rechecked against the new API. The
  in-memory tests need no change, because `init_in_memory_tracing` hard-codes CPU tracing on. The
  comment at `tests/telemetry_micromegas.rs:345-346` that describes the env-var mechanism is
  updated.

### 2. Upstream span capture: verify now, swap when the upstream hook lands

The bridge's span half stays: deleting it would export content and source paths (Current State).
This step does three things:

- **Parity test** (`tests/telemetry_micromegas.rs`). Run one span program twice, through
  `SpanBridge<MicromegasExport>` and through `TracingCaptureLayer { capture_spans: true }` behind
  the same `filtered(…, is_exported_span)`. Compare the trees as `(name, parent name, depth)`
  multisets. The program covers `fontaine.session` > `fontaine.interaction` > `chat` and
  `fontaine.tool` > `fontaine.permission` > `fontaine.guardian` > `chat`, plus the gotcha: a `rig`
  span whose direct parent is a non-allowlisted span (e.g. `target: "hyper"`) must land under the
  enclosing Fontaine span in both layers. A field-free comparison is deliberate, because fields
  are where the two differ.
- **Upstream issue.** File one issue on `madesroches/micromegas` asking `TracingCaptureLayer` for:
  (a) a field hook, `Fn(&'static Metadata, &str /*name*/, &str /*value*/) -> Option<Cow<str>>`,
  applied in `on_new_span` / `on_record` before a value is stored, so a host can drop, redact,
  truncate or rewrite; and (b) a switch that leaves `file` / `line` out of the span location. Link
  it from the PR body and from the master plan's Decisions.
- **Design doc.** `tasks/design/observability.md` replaces "Once micromegas captures `tracing` spans
  with parentage upstream, the bridge shrinks…" with the actual remaining trigger: the field hook
  and the location switch. Once both land, `MicromegasExport::span_begin` / `span_end` and the
  location cache are deleted and `capture_spans` flips to `true`. Field collection stays in the
  bridge for metric derivation on both backends.

### 3. Events and metrics that close the gaps

All on target `fontaine::telemetry`, content-free.

- **`api_error`**, emitted from `turn.rs` when the stream yields `Err(PromptError::Provider(_))` or
  `Err(PromptError::Report(_))`, before the failure is returned. `MaxTurns`, `Cancelled`, `Memory`
  and `UnknownToolCall` are not failed API requests and do not emit it (`MaxTurns` already maps to
  `TurnOutcome::MaxTurns`). Fields: `model`, `provider`, `query_source` (`main` for the root agent,
  `subagent` otherwise, the same values as on `api_request`), `agent.id`, `error_class`, and
  `status_code` when the error carries an HTTP status structurally. `error_class` is rig's stable
  `ErrorKind::code()` (`ProviderError::kind().code()` for `Provider`, `ErrorReport.kind.code()` for
  `Report`), and `status_code` comes from `PromptError::provider_response_status()`. A provider's
  message can echo the request, so the message text is not exported. `model` and `provider` are
  added to `TurnParams` and `Drive` (neither has them today), filled from `handle.completion.id()`
  / `.name()` at the call sites (`headless.rs`, `fontaine-tui/src/lib.rs`, `child_runner.rs`).
  Claude Code's `api_error`
  carries `error` (the message) and `attempt`. Fontaine sends the class instead of the message,
  and no `attempt`, because rig's retries are not visible at this layer. That is a deviation (see
  Decisions).
- **`internal_error`**, emitted from `SessionState::sink_failed` on every failure, not just the
  first one that `Approver::warn` shows. Field: `class`, which is `audit_write` or `ledger_write`
  (a `Sink::class()` next to `Sink::what()`). The io error text is not exported. It can contain a
  path.
- **Dropped guardian spend.**
  - `fontaine-guardian`: `Guardian::dropped_estimate(&AdjudicationRequest) -> u64`, returning
    `(system.len() + user.len()).div_ceil(4)` over `render(profile, &profile.shape(request))`,
    sharing the shape/render prelude with `run`. `cascade::Composed` gains
    `dropped_input_tokens: Option<u64>`, set only on the two branches that drop `guardian_call`.
    The guardian also carries `spec.provider` / `spec.id` from `handle.completion.spec()` and
    `handle.catalog_billed()` as the priced flag, set in `agent::build_guardian` next to
    `GuardianSettings::priced`. (`guardian.model` is `handle.reference`, `{account}/{model}`, which
    is not a catalog id.) The crate stays subscriber-free.
  - `fontaine-core`: `DecisionRecord` gains `guardian_dropped_input_tokens: Option<u64>`,
    `guardian_dropped_provider: Option<String>`, `guardian_dropped_model: Option<String>` and
    `guardian_dropped_priced: Option<bool>`, written to the audit record and projected onto
    `tool_decision` as `guardian.dropped_input_tokens`, `guardian.dropped_provider`,
    `guardian.dropped_model` and `guardian.dropped_priced`. (`model.priced` is only on the
    `fontaine.guardian` span, not on `tool_decision`.)
  - `span_bridge`: on a `tool_decision` with `guardian.dropped_input_tokens`, emit
    `fontaine.token.usage {type = input, model, query_source = guardian, estimated = true}`, with
    `model` = `guardian.dropped_model`. When `guardian.dropped_priced` is true, also emit
    `fontaine.cost.usage {model, query_source = guardian, estimated = true}`, with `cost_usd` over
    (`guardian.dropped_provider`, `guardian.dropped_model`) and an input-only `Usage`. Real usage
    carries no `estimated` attribute. `estimated` is a Fontaine extension (see Decisions).
  - The session's `/cost` and budget debit are not changed here. They belong to the
    composable-guardian work (#137), which may make `seq` the default for priced guardians and
    leave nothing to estimate.
- **No new metric for the screen.** `screen.*` fields are already on `tool_decision`, so the work
  for #124 here is documentation.

### 4. Retention and correlation (docs only)

- A `retention_by_stream_tag` recipe in `docs/telemetry.md`. Keep `log` long (e.g. `"forever"` or
  365 days), since it holds decision, tool and error events. Keep `metrics` medium (e.g. 180).
  Keep `cpu` short (e.g. 30), since it holds the span trees, which carry the opt-in content when
  any `log_*` key is on. The recipe says this is a deployment-wide setting that applies to every
  process's streams, not just Fontaine's, and that the backend only sorts data into these streams
  with the native micromegas protocol.
- Custom stream tags for opt-in content: tags are hard-coded in `micromegas_tracing::dispatch`
  (`log`, `metrics`, `cpu`). Not filed upstream now. Opt-in content is off by default and already
  lives on `cpu`, the stream the recipe expires first, and the field hook in §2 comes first (see
  Decisions).
- Correlation: `tool_decision.tool_use_id` matches the audit record's `tool_use_id`, so an operator
  with both can join them locally. Telemetry gains no content.

### 5. Analytics: a notebook under `analytics/`

- `analytics/notebooks/fontaine-overview.json`, in the `micromegas-screens` file format
  (`name`, `screen_type: "notebook"`, `config.cells`, a default time range of `now-7d`). It has
  one cell group per question in the issue. Each cell's SQL reads only `log_entries`
  (`target = 'fontaine::telemetry'`, event name via `split_part(msg, ' ', 1)`, fields via
  `property_get(properties, …)`) and `measures` (`name = 'fontaine.…'`, attributes via
  `property_get`). The native backend writes `msg` as `"<message> k1=v1 …"` and OTLP writes it
  bare, so `split_part` works on both. `property_get` returns strings, so numeric properties are
  `CAST`:
  - cost and tokens by session (`process_id`), `model` and `query_source`, with estimated spend
    apart;
  - guardian latency (p50/p95) by `model`, and fail-closed count by `reason`;
  - decision tier and decision mix (`fontaine.tool_decision.count`);
  - screen outcomes (`fontaine.screen.count`), plus the `screen.risk` distribution from
    `tool_decision`;
  - tool success rate and latency by `tool_name` (`tool_result`);
  - subagent lifecycle counts by `event` (`subagent`);
  - `api_error` / `internal_error` counts by class.
  Span-tree cells are left out, because the span tables differ between the two backends (below).
- No materialized view set. Applying one needs a micromegas admin, and these queries run over the
  base views at notebook scale (see Decisions). A view set can follow if real volumes need one.
- OTLP: the same notebook works through `/ingestion/otlp`, because logs and metrics land in
  `log_entries` / `measures`. The docs say spans land in `otel_spans` instead of `async_events`,
  that the notebook has no span cells for that reason, and that `msg` is bare on OTLP. Event
  attributes arrive as `properties` and the tracing target arrives as `target` on both backends:
  `opentelemetry-appender-tracing` sets the log record target to the tracing target, which
  `opentelemetry-proto` carries as the scope name, which micromegas maps to `target`.
- A new root directory means updating the `AGENTS.md` root-directory list and the master plan's
  workspace layout.

### 6. Process health

Not added. micromegas's system monitor reports host-level CPU and memory and its own host
properties, and Fontaine builds the sink with it off. A process-only collector would be new code
with no consumer among the questions this telemetry answers (see Decisions). Revisit it if an
operator asks.

## Implementation Steps

1. **Dependency move.** `rust/Cargo.toml` (git rev for the three micromegas crates),
   `rust/deny.toml` (`allow-git`), and `cargo update -p` for the three crates. Fix compile errors
   from the 0.33 API. Run `cargo deny check`.
2. **CPU tracing by builder.** `telemetry/micromegas.rs` (`with_cpu_tracing_enabled(true)` in
   `build_guard`, delete `CPU_TRACING_VAR` / `enable_cpu_tracing`, `capture_spans: false`),
   `telemetry/mod.rs` (drop the call). Update the module doc comment.
3. **Parity test.** `tests/telemetry_micromegas.rs`: the two-layer tree comparison from §2,
   reusing the file's `spans()` helper and `serial()` guard.
4. **`api_error`.** `fontaine-core/src/turn.rs` (emit, plus a small `error_class(&PromptError) ->
   &'static str`), and `TurnParams` / `Drive` (model and provider), plus the call sites in `headless.rs`,
   `fontaine-tui/src/lib.rs` and `child_runner.rs`.
5. **`internal_error`.** `fontaine-core/src/gate.rs` (`Sink::class`, emit in `sink_failed`).
6. **Dropped spend.** `fontaine-guardian/src/guardian.rs` (`dropped_estimate`, provider and model
   ids next to `priced`), `cascade.rs` (`dropped_input_tokens`), `fontaine-core/src/agent.rs`
   (ids from the handle), `audit.rs` (`DecisionRecord` fields), `permissions.rs` (set it from
   `composed`, project it in `record_decision`), `fontaine/src/telemetry/span_bridge.rs`
   (derive the estimated token and cost metrics).
7. **Analytics.** Create `analytics/notebooks/fontaine-overview.json`. Update the `AGENTS.md`
   root-directory list and the master plan's workspace layout.
8. **Upstream issue.** File the `TracingCaptureLayer` field-hook and location-switch issue on
   `madesroches/micromegas`, and record its number in Decisions and the PR body.
9. **Docs and design.** See `## Documentation`. Add the Decisions lines to the master plan.

## Files to Modify

- `rust/Cargo.toml`, `rust/Cargo.lock`, `rust/deny.toml`
- `rust/fontaine/src/telemetry/{mod.rs,micromegas.rs,span_bridge.rs}`
- `rust/fontaine/tests/telemetry_micromegas.rs`
- `rust/fontaine-core/src/{turn.rs,gate.rs,audit.rs,permissions.rs,agent.rs}`
- `rust/fontaine/src/headless.rs`, `rust/fontaine-tui/src/lib.rs`, `child_runner.rs` (model and
  provider into `TurnParams`)
- `rust/fontaine-core/tests/audit_telemetry.rs`
- `rust/fontaine-guardian/src/{guardian.rs,cascade.rs}`
- `analytics/notebooks/fontaine-overview.json` (new)
- `docs/telemetry.md`, `tasks/design/observability.md`, `tasks/fontaine_harness_plan.md`,
  `AGENTS.md`

## Trade-offs

- **`api_error` from the turn loop vs. from the completion span.** The span closes the same way on
  error and on cancellation (a dropped guardian call). The turn loop holds the typed error.

## Decisions

Step 9 copies these lines into the master plan's Decisions.

- 2026-10-24: `api_error` exports `error_class` and `status_code` instead of Claude Code's `error`
  message and `attempt`, because provider messages can echo the request and rig's retries are not
  visible at this layer.
- 2026-10-24: `fontaine.token.usage` / `fontaine.cost.usage` gain an `estimated` attribute, a
  Fontaine extension, to separate the dropped-guardian-spend estimate from real usage.
- 2026-10-24: No custom stream tags for opt-in content; tags are hard-coded upstream, and opt-in
  content already lives on `cpu`, which the retention recipe expires first.
- 2026-10-24: No materialized view set; applying one needs a micromegas admin and the notebook
  queries run over base views.
- 2026-10-24: No process-health collector; micromegas's system monitor is host-level and no
  question the telemetry answers needs it.
- 2026-10-24: The `SpanBridge` span half is kept despite the issue's done-when, because the stock
  layer exports field content and absolute source paths; the swap waits on the upstream field hook
  and location switch issue (number recorded in Step 8).
- 2026-10-24: micromegas is on a git pin with `allow-git` in `deny.toml` until 0.33 is on
  crates.io; the git pin is never shipped in a release.

## Documentation

- `docs/telemetry.md`:
  - micromegas section: CPU tracing is switched on through the sink builder, and the environment
    is not touched.
  - Event table: add `api_error`, `internal_error` and `permission_mode_changed` (`from`, `to`).
    Add `guardian.verdict`, the `screen.*` fields and `guardian.dropped_input_tokens`, `guardian.dropped_provider`, `guardian.dropped_model` and
    `guardian.dropped_priced` to `tool_decision`.
  - Metrics: the `estimated` attribute.
  - New sections: "Retention", "Joining with the audit log", and "Analytics" (how to load the
    notebook with `micromegas-screens apply` from `analytics/notebooks`, and where the OTLP path
    differs).
- `tasks/design/research-findings.md` (~line 205, "settable only through
  `MICROMEGAS_ENABLE_CPU_TRACING`"): updated for the builder knob.
- `tasks/design/observability.md`: micromegas paragraph (builder knob, the real swap trigger),
  event list (`api_error` / `internal_error` fields), the dropped-spend estimate, and `analytics/`.
- `tasks/fontaine_harness_plan.md`: workspace layout gains `analytics/`, plus the Decisions lines.
- `AGENTS.md`: root-directory list gains `analytics/`.

## Testing Strategy

Unit and integration tests, no live services:

- `span_bridge.rs` unit tests: a `tool_decision` with `guardian.dropped_input_tokens` yields an
  `estimated = true` input-token metric with `query_source = guardian` and `model` from
  `guardian.dropped_model`, and a cost metric only when `guardian.dropped_priced`. A decision without the field yields neither. Existing token metrics carry no
  `estimated`.
- `fontaine-guardian` unit tests: `dropped_estimate` is `len.div_ceil(4)` over the rendered prompt.
  `cascade::adjudicate` with a scripted
  screen that stops or allows, and a guardian model that never answers, sets
  `dropped_input_tokens`. With the guardian answering first, or a shadow screen, it stays `None`.
- `turn.rs` / `fontaine-core` test with `rig_core::test_utils` (`MockTurn::provider_response_error(
  status, …)`): one `api_request`-free `api_error` event with the expected `error_class`,
  `status_code`, `query_source` and model, and no message text. `MaxTurns` emits none. A subagent failure reports `query_source = subagent`.
- `gate.rs` test: call `state.sink_failed(Sink::Audit, ..)` twice under `TraceCapture` and assert
  two `internal_error { class = audit_write }` events with no error text, as
  `tests/permissions_auto.rs:821-840` does.
- `tests/telemetry_micromegas.rs`: the parity test from §2. The existing shipped-binary round trip
  against a mock ingestion endpoint keeps passing with nothing setting
  `MICROMEGAS_ENABLE_CPU_TRACING` in the parent environment. It pins that the builder knob really
  turns on the thread streams. Without them, span events vanish silently.
- A test that parses `analytics/notebooks/fontaine-overview.json`. It checks that every
  `fontaine.*` metric name and every event name quoted in its SQL appears in `docs/telemetry.md`,
  so the notebook and the docs cannot drift silently. It lives in `rust/fontaine/tests/` and reads
  the file relative to `CARGO_MANIFEST_DIR`.
- `cargo deny check` with the git source allowed.

## Manual Verification

Each step needs a running micromegas deployment, which unit tests cannot reach:

1. Start a local micromegas (its `local_test_env` services), run `cargo run -p fontaine -- -p
   "list the files here"` with `[telemetry]` pointed at it, then `SELECT name, depth FROM
   process_spans('<process_id>', 'async')`. Expected: the same tree as before the change.
2. `micromegas-screens apply` from `analytics/notebooks`, then open the notebook. Expected: every
   cell runs without a SQL error and shows the run from step 1.
3. Repeat step 1 with `backend = "otlp"` and `endpoint = …/ingestion/otlp`. Expected: the
   two backends agree on event names (`split_part(msg, ' ', 1)`) and values. Record any
   difference in the docs.

## Open Questions

- When does micromegas 0.33 ship to crates.io? The issue leaves it to the maintainer whether the
  switch back to an exact `=0.33.x` pin happens before this merges or before the next Fontaine
  release. This PR ships on the git pin with `allow-git` in place.
