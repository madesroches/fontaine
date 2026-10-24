# Telemetry

Fontaine can export spans, metrics and events about a run to a collector. It is off by default and
sends nothing until you turn it on. Everything it exports is described on this page.

## Turning it on

```toml
[telemetry]
enabled = true
backend = "micromegas"
endpoint = "https://telemetry.example.com"
api_key_env = "MICROMEGAS_INGESTION_API_KEY"
```

`enabled = true` needs two things, and either one missing is a configuration error (exit code 2)
before anything runs:

- the ingestion URL, `endpoint`; there is no default;
- the ingestion key: the environment variable that `api_key_env` names, or, when `api_key_env` is
  omitted, the key stored with `fontaine auth login --telemetry`. Keys are never written in
  settings.

Only the user file and `--settings` can enable telemetry or change where it goes. A project file
can set `enabled = false` and nothing else, so a repository cannot start exporting from your
machine. The full key list is in [Configuration](configuration.md#telemetry).

If the exporter cannot start, Fontaine prints one line (`telemetry export is off: ...`) and the run
continues with export off. Telemetry never changes what a run does, and a collector that never
answers cannot delay the exit by more than two seconds: the final flush is given up after that.

## Backends

### micromegas

The default backend sends to a [micromegas](https://micromegas.info/) ingestion service over its
native protocol. Spans arrive as async span events carrying a span id, a parent span id and a
depth, which the analytics side reassembles into a tree. Metrics and events arrive as micromegas
metrics and log entries.

```toml
[telemetry]
enabled = true
backend = "micromegas"
endpoint = "https://telemetry.example.com"
api_key_env = "MICROMEGAS_INGESTION_API_KEY"
resource_attributes = { "team.id" = "platform" }
```

Fontaine builds the sink with its local sink, the process-wide `tracing` capture and the system
monitor all off, and with the endpoint and key given explicitly: the endpoint and key come from
`[telemetry]`, never from the `MICROMEGAS_TELEMETRY_URL` or `MICROMEGAS_INGESTION_API_KEY`
fallbacks. The default host properties micromegas would attach, such as the user name and machine
name, are not sent.

Span events travel on per-thread streams that micromegas creates only while CPU tracing is on.
Fontaine switches it on through the sink builder when this backend is enabled; it does not touch
its environment.

### OTLP

The `otlp` backend speaks OTLP over HTTP with protobuf bodies to any OpenTelemetry collector,
with no gRPC. Spans, metrics and events go to `<endpoint>/v1/traces`, `<endpoint>/v1/metrics` and
`<endpoint>/v1/logs`, each with an `Authorization: Bearer <key>` header.

```toml
[telemetry]
enabled = true
backend = "otlp"
endpoint = "http://localhost:4318"
api_key_env = "OTLP_API_KEY"
```

micromegas also accepts OTLP, which gives you its span trees without the native protocol:

```toml
[telemetry]
enabled = true
backend = "otlp"
endpoint = "https://telemetry.example.com/ingestion/otlp"
api_key_env = "MICROMEGAS_INGESTION_API_KEY"
```

Set `traces = false` to export metrics and events without spans. Both backends are compiled into
the default build.

## What is exported

Only spans from Fontaine's own `fontaine::telemetry` target and from the model layer's completion
and agent targets are exported, and only events from the Fontaine target. Anything else a
dependency logs stays in the local debug file.

### Spans

Both backends export the same tree: `fontaine.session` contains one `fontaine.interaction` per
user message, and below it the model calls (`chat`) and tool executions (`execute_tool`). Each
tool call also has a `fontaine.tool` span, with `tool_name`, `risk_class` (`read_only`,
`workspace_write` or `arbitrary`), `tool_source` (`builtin` or `mcp`) and `success`.
`fontaine.interaction` carries `prompt.id`, `prompt.length` and `model.priced`. Each tool call also has a `fontaine.permission` child span with `tool_name`, `permission_mode`, and the `tier` and `decision` of the pipeline's decision; while a human is asked, a `fontaine.tool.blocked_on_user` span below it measures the wait. With a `[guardian.screen]` stage, each call it scores adds a `fontaine.guardian.screen` span (`risk`, `allow_risk`, `steering`, `outcome`, `latency_ms`) below `fontaine.permission`. Calls that reach the guardian add a `fontaine.guardian` span below `fontaine.permission` with
`tool_name`, `guardian_model`, `guardian_profile`, `model.priced`, the `decision`, whether it was a `cache_hit` and
the `fail_closed_reason` when it failed closed; the guardian's own model call sits under it.
A subagent adds a `fontaine.subagent` span (`agent.id`, `agent.type`, `model.priced`) under the span of the `agent`
call that started it, and its own interactions, model calls and tool calls nest below it. A user
hook adds a `fontaine.hook` span (`hook_event`, `success`) under the span of the call it ran for,
and a call to an MCP tool a `fontaine.mcp.call` span (`mcp.server`, `mcp.tool`). A threshold
compaction adds a `fontaine.compaction` span around the summarizer call, and each scan by the
`llm` tool-result scanner a `fontaine.scanner` span (`scanner`) around the scanner's model call.
Those two also carry `model.priced`. `model.priced` says whether the model it bills by is priced;
the cost of a model call is dropped under the nearest ancestor span that records
`model.priced = false`. Model-call spans carry the provider, the model, the response id and
the token counts. Source file paths and thread names are not exported.

### Metrics

| Metric | Unit | Attributes |
|---|---|---|
| `fontaine.session.count` | `{session}` | `start_type` (`fresh`, `resume` or `continue`; a fork takes the value of the flag it forks from, and `--continue` with nothing to continue is `fresh`), `headless` (`true` or `false`) |
| `fontaine.token.usage` | `{token}` | `type` (`input`, `output`, `cacheRead`, `cacheCreation`, `reasoning`), `model`, `query_source`; `estimated` (`true`) on the guardian spend estimate only |
| `fontaine.cost.usage` | USD | `model`, `query_source`; `estimated` (`true`) on the guardian spend estimate only |
| `fontaine.tool_decision.count` | `{decision}` | `tier`, `decision` |
| `fontaine.guardian.latency` | ms | `model`, `profile`, `cache_hit` |
| `fontaine.guardian.fail_closed.count` | `{denial}` | `reason` (`error`, `timeout`, `unparseable`, `low_confidence`, `unavailable`) |
| `fontaine.screen.count` | `{call}` | `outcome` (`stop`, `allow`, `pass`, `unavailable`; in shadow mode `stop` and `allow` are what the screen would have done) |

`fontaine.cost.usage` is priced from the model catalog and is absent when the catalog has no price,
which is always the case for an `openai-compatible` endpoint or an `opencode-go` account.
`query_source` is `main` for the session model, `guardian` for the model calls made while a
guardian adjudication is open, `scanner` for the calls the `llm` tool-result scanner makes, and
`subagent` for the calls made under a subagent's span.
Both guardian metrics are derived from the guardian attributes of the decision event. No metric
carries a prompt or tool-call id.

When a [screen](guardian.md) in enforcing mode stops or allows a call, the guardian request that
was racing it is dropped. The request has usually been sent and billed by then, but no usage comes
back for it. Fontaine adds an estimate to `fontaine.token.usage` (`type = input`) and, for a
priced guardian model, to `fontaine.cost.usage`, with `query_source = guardian` and `estimated =
true`: a quarter of the prompt's bytes, priced as input only. Real usage never carries
`estimated`, so a query that leaves the estimate out filters on its absence. The session's `/cost`
and budgets do not count the estimate.

### Events

| Event | Fields |
|---|---|
| `api_request` | `model`, `provider`, `input_tokens`, `output_tokens`, `cost_usd` (when priced), `duration_ms`, `query_source` |
| `user_prompt` | `prompt_id`, `prompt_length` (the length only) |
| `subagent` | `event` (`spawn`, `complete`, `killed` or `budget_exhausted`), `agent.id`, `agent.type` |
| `mcp_server_connection` | `server`, `transport` (`stdio` or `http`), `status` (`connected` or `failed`) |
| `api_error` | `model`, `provider`, `query_source` (`main` or `subagent`), `agent.id`, `error_class` (rig's stable error code, such as `provider_response`, `http`, `timeout` or `json`), and `status_code` when the failure carries an HTTP status. The provider's message is not exported: it can echo the request. Retries are not visible, so there is no attempt count. Emitted for a main-agent or subagent turn that failed with a provider error or a rig error report, in practice a failed model call; `error_class` tells the rare rig-internal failures (such as `internal`, `divergence` or `handler_unavailable`) apart. A turn limit or a cancel is not one. |
| `internal_error` | `class` (`audit_write` or `ledger_write`). Emitted for every failed write to the audit log or the session ledger, not only the first one the terminal reports. The I/O error text is not exported: it can contain a path. |
| `permission_mode_changed` | `from`, `to` |
| `tool_decision` | `tool_name`, `agent.id`, `agent.type`, `tool_use_id`, `tier`, `decision`, `permission_mode`, `headless`, `latency_ms`; for guardian decisions also `guardian.model`, `guardian.profile`, `guardian.verdict`, `guardian.confidence`, `guardian.cache_hit`, `guardian.latency_ms` and, when it failed closed, `fail_closed_reason`; when a [screen](guardian.md) scored the call, `screen.outcome`, `screen.risk`, `screen.allow_risk`, `screen.steering`, `screen.model` and `screen.latency_ms`, in shadow mode too; when the screen settled the call and cut a guardian request short, `guardian.dropped_input_tokens`, `guardian.dropped_provider`, `guardian.dropped_model` and `guardian.dropped_priced`. The rule label and explanation are not exported; they are in the audit log. |
| `tool_result` | `tool_name`, `risk_class`, `tool_source`, `success`, `duration_ms`, `args_bytes`, `result_bytes` (sizes only: no arguments and no output); when the [tool-result scanner](guardian.md#scanning-tool-results) scanned the result, also `scanner_verdict` (`clean`, `flagged` or `error`) and `scanner_latency_ms`, absent when no scan ran. Emitted for every call, including a denied one. |

A command you run with `!` in the terminal UI is not a gate decision, but it is reported through the
same event and counter: `tool_name = shell`, `tier = human`, `decision = allow`, and it counts with
the human approvals in `fontaine.tool_decision.count`. Its `user_shell` rule and its agent id (`user`)
are only in the audit log.

## Privacy

Fontaine's own fields carry no content: no prompts, no replies, no file contents, no command
lines, no paths. The model layer can attach content to its spans, and it is dropped before export
unless you opt in, one key per kind of content:

| Key | Exports |
|---|---|
| `log_user_prompts` | The prompt and the system prompt |
| `log_assistant_responses` | The model's reply |
| `log_tool_details` | Tool-call arguments |
| `log_tool_content` | Tool results |

A message you type while a turn runs reaches the agent inside a tool result, so `log_tool_content`
exports it along with that result.

```toml
[telemetry]
enabled = true
endpoint = "https://telemetry.example.com"
api_key_env = "MICROMEGAS_INGESTION_API_KEY"
log_user_prompts = true
content_max_bytes = 16384
```

Opted-in content is redacted first, so anything shaped like an API key, token or private key is
replaced with `[REDACTED]`, and truncated to `content_max_bytes` (61440 by default) second. A field
the model layer adds in a later version is treated as content and stays out until a key covers it.

The names of MCP servers and tools are the exception to "Fontaine's own fields carry no content":
they belong to a third party and can name internal systems. They appear in the `tool_name` of
spans and events, in `mcp.server` and `mcp.tool` and in `server` only with `log_tool_details =
true`; otherwise every one of them reads `third-party`.

`resource_attributes` adds static tags to everything exported; do not put secrets there.

## Telemetry and the audit log

Telemetry is best effort: it is off by default, lossy under backpressure, redacted and bounded, and
it can be switched off at any time. It is not a security record. The permission audit log is a
separate, local, always-on record of every tool decision that does not depend on `[telemetry]` and
is never derived from it (see [Permissions](permissions.md#the-pipeline)); telemetry is not a substitute.

The two share one key. A `tool_decision` event's `tool_use_id` is the `tool_use_id` of the audit
record for the same decision, so someone who holds both can join them locally, for instance to put
the explanation the audit log keeps next to the tier and latency telemetry shows. Telemetry gains
no content from the join.

## Retention

micromegas expires data by stream tag when the deployment sets `retention_by_stream_tag`. Fontaine's
data sits on these streams:

| Stream tag | Holds | Suggested retention |
|---|---|---|
| `log` | the events: decisions, tool results, errors | long, such as `"forever"` or 365 days |
| `metrics` | the metrics | medium, such as 180 days |
| `cpu` | the span trees, with the opted-in content when any `log_*` key is on | short, such as 30 days |
| `trace` | the spans sent through `/ingestion/otlp`, with the same opted-in content | short, as for `cpu` |

```json
{ "log": "forever", "metrics": 180, "cpu": 30, "trace": 30 }
```

This is a deployment-wide setting: it applies to every process's streams, not only Fontaine's. A
per-tag override covers the raw payloads only; the partitions of a materialized view follow
`retention_days`. The tag names are fixed by micromegas, so opted-in content cannot be given a
stream of its own: it is off by default, and it lives on the stream the recipe expires first.

## Analytics

`analytics/notebooks/fontaine-overview.json` is a micromegas notebook that answers the usual
questions about a deployment's Fontaine sessions over the last seven days: cost and tokens by
session, model and `query_source` (the guardian spend estimate apart); guardian latency and
fail-closed denials; the decision tier and mix; screen outcomes and the risk distribution; tool
success rate and latency; subagent lifecycle; and `api_error` and `internal_error` counts. Event
cells read `log_entries`, with the event name in the first word of `msg` and the fields in
`properties`. Metric cells read `measures`.

To load it, install the `micromegas` Python package, then from `analytics/notebooks` inside the git
checkout:

```bash
micromegas-screens init https://telemetry.example.com
micromegas-screens apply
```

`init` writes `micromegas-screens.json` (the server, and the repository as `managed_by`), which is
per deployment and ignored by git. `apply` also writes `managed_by` into the screen file, and it
deletes screens that the repository manages and no longer has a file for; it asks first unless you
pass `--auto-approve`.

Through `/ingestion/otlp` the notebook differs in these ways:

- The event cells work, and agree with the native backend. The event name is the whole of `msg`
  there, where the native protocol appends `key=value` pairs; `split_part(msg, ' ', 1)` reads
  both. The tracing target arrives as `target`, and the fields as `properties`.
- The metric cells are native-only. micromegas drops OTLP histogram points, so the guardian latency
  cell stays empty, and Fontaine's OTLP meter exports cumulative sums, so `SUM(value)` adds up
  running totals.
- Spans land in `otel_spans` instead of `async_events` (`process_spans(..., 'async')`), so the
  notebook has no span cells.
