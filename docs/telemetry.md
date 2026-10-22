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

Fontaine sets `MICROMEGAS_ENABLE_CPU_TRACING=true` in its own environment, only when this backend
is enabled, because span events travel on per-thread streams that micromegas creates only then.

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
`fontaine.interaction` carries `prompt.id` and `prompt.length`. Each tool call also has a `fontaine.permission` child span with `tool_name`, `permission_mode`, and the `tier` and `decision` of the pipeline's decision; while a human is asked, a `fontaine.tool.blocked_on_user` span below it measures the wait. Calls that reach the guardian add a `fontaine.guardian` span below `fontaine.permission` with
`tool_name`, `guardian_model`, `guardian_profile`, the `decision`, whether it was a `cache_hit` and
the `fail_closed_reason` when it failed closed; the guardian's own model call sits under it.
A subagent adds a `fontaine.subagent` span (`agent.id`, `agent.type`) under the span of the `agent`
call that started it, and its own interactions, model calls and tool calls nest below it. A user
hook adds a `fontaine.hook` span (`hook_event`, `success`) under the span of the call it ran for,
and a call to an MCP tool a `fontaine.mcp.call` span (`mcp.server`, `mcp.tool`). Model-call spans carry the
provider, the model, the response id and the token counts. Source file paths and thread names are
not exported.

### Metrics

| Metric | Unit | Attributes |
|---|---|---|
| `fontaine.session.count` | `{session}` | `start_type` (`fresh`, `resume` or `continue`; a fork takes the value of the flag it forks from, and `--continue` with nothing to continue is `fresh`), `headless` (`true` or `false`) |
| `fontaine.token.usage` | `{token}` | `type` (`input`, `output`, `cacheRead`, `cacheCreation`, `reasoning`), `model`, `query_source` |
| `fontaine.cost.usage` | USD | `model`, `query_source` |
| `fontaine.tool_decision.count` | `{decision}` | `tier`, `decision` |
| `fontaine.guardian.latency` | ms | `model`, `profile`, `cache_hit` |
| `fontaine.guardian.fail_closed.count` | `{denial}` | `reason` (`error`, `timeout`, `unparseable`, `low_confidence`, `unavailable`) |

`fontaine.cost.usage` is priced from the model catalog and is absent when the catalog has no price,
which is always the case for an `openai-compatible` endpoint. `query_source` is `main` for the
session model, `guardian` for the model calls made while a guardian adjudication is open, and
`subagent` for the calls made under a subagent's span.
Both guardian metrics are derived from the guardian attributes of the decision event. No metric
carries a prompt or tool-call id.

### Events

| Event | Fields |
|---|---|
| `api_request` | `model`, `provider`, `input_tokens`, `output_tokens`, `cost_usd` (when priced), `duration_ms`, `query_source` |
| `user_prompt` | `prompt_id`, `prompt_length` (the length only) |
| `subagent` | `event` (`spawn`, `complete`, `killed` or `budget_exhausted`), `agent.id`, `agent.type` |
| `mcp_server_connection` | `server`, `transport` (`stdio` or `http`), `status` (`connected` or `failed`) |
| `tool_decision` | `tool_name`, `agent.id`, `agent.type`, `tool_use_id`, `tier`, `decision`, `permission_mode`, `headless`, `latency_ms`; for guardian decisions also `guardian.model`, `guardian.profile`, `guardian.confidence`, `guardian.cache_hit`, `guardian.latency_ms` and, when it failed closed, `fail_closed_reason`. The rule label and explanation are not exported; they are in the audit log. |
| `tool_result` | `tool_name`, `risk_class`, `tool_source`, `success`, `duration_ms`, `args_bytes`, `result_bytes` (sizes only: no arguments and no output). Emitted for every call, including a denied one. |

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
