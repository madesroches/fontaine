# Phase 1 — Model layer + headless surface (mostly dependency wiring)

**Status:** not started · **Depends on:** [Phase 0](phase0_scaffold.md) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Model layer: rig-core providers

No custom `Provider` trait. `rig_core` supplies provider-neutral messages, completion, streaming,
and 20+ providers behind one interface (Anthropic, OpenAI, Gemini, Ollama, Bedrock, Vertex, and
custom OpenAI-compatible endpoints for OpenRouter/LM Studio/vLLM/LiteLLM). The **same rig model
abstraction serves both the main agent and the guardian** — the guardian is just another rig agent
pointed at a different model handle (`guardian.model`). Streaming to the TUI uses rig's streaming
events; structured verdicts use rig's `structured_output` synthetic-tool path (see
`tasks/design/research-findings.md` §Ecosystem / dependencies). Provider selection comes from Fontaine config **accounts** (already parsed and scope-validated by Phase 0's `config.rs`; this phase wires behavior to them) (`[accounts.<name>]`: `kind`,
optional `base_url`, credential source) and maps onto rig client constructors — one rig client per
account, so two accounts of the same kind (work + personal Anthropic) coexist with separate
credentials and endpoints. Each account provides models: an explicit `models` list, else discovery
from the provider where rig supports listing. Models are addressed `<account>/<model>` in `[model]`,
`[guardian] model`, and `[agents.types]`, so main agent and guardian may sit on different accounts.
Credentials come from a named env var or the stored-credentials file (`fontaine auth login`,
`<data dir>/credentials.toml` with `[accounts.<name>]` / `[telemetry]` tables, owner-only); raw keys are never read from config files. A missing or invalid credential on
the guardian's account makes the guardian unavailable and fails closed.

### Headless I/O contract (versioned)

`fontaine -p` is the surface the eval runner, the CI integration tests and any external consumer
parse, so its output is a versioned contract (Claude Code parity for the flags). `--output-format
text` prints the final result text; `json` prints one final `result` object; `stream-json` prints
JSONL events. Every JSON object carries `schema_version` (starting at 1; additive fields keep it, any
rename or removal bumps it) and `session_id`. The contract is a checked-in JSON Schema per version,
`docs/schemas/headless-v1.schema.json`, published with the docs. Event types:

- `init` — model, guardian model and profile, mode, workspace roots, loaded config sources with
  their scopes, workspace trust state, allow rules suspended by `auto`.
- `assistant` — a complete assistant message; with `--include-partial-messages`, also `assistant_delta`.
- `tool_call` — `tool_use_id`, tool, args.
- `gate_decision` — `tool_use_id`, `tier` (the tier enum in `tasks/design/permissions.md` §Core data flow, equal to the audit record's), `decision`, `rule`, and for
  guardian decisions `explanation`, `confidence`, `guardian_model`.
- `tool_result` — `tool_use_id`, success, the result text as the agent receives it.
- `result` — `subtype` (`success` | `error_max_turns` | `error_max_budget` | `error_run`), result
  text, `structured_output` (with `--json-schema`), usage and cost (main / guardian / subagents),
  and the denial list (tool, tier, rule, explanation).

`--input-format stream-json` reads user messages as JSONL on stdin for a multi-turn headless run, one
`{"type":"user","message":{"role":"user","content":…}}` line per turn (Claude Code's shape; the input line is
part of `docs/schemas/headless-v1.schema.json`). An
external approval delegate (Claude Code's `--permission-prompt-tool`) is not in v1: every
would-prompt outcome stays a Deny.

### Telemetry backends

The `Option` export layer from Phase 0 is built from loaded config at install time with both backends from `tasks/design/observability.md` §Observability: the
micromegas sink guard (local sink, global capture and system monitor off; explicit endpoint +
`ApiKeyRequestDecorator` from the configured key) with `TracingCaptureLayer` composed into
Fontaine's registry and the `SpanBridge` mapping allowlisted spans (targets `fontaine::telemetry`, `rig::completions`,
`rig::agent_chat` and `rig_agent::agent::telemetry`, the target of rig's `invoke_agent` / `execute_tool` spans) to micromegas async span events
(span id + parent span id + depth) so the tree survives, and the OTLP/HTTP backend
(`tracing-opentelemetry`, `opentelemetry-appender-tracing`, metrics via an OTel `Meter`). `run` loads config, then sets
`MICROMEGAS_ENABLE_CPU_TRACING=true` only when the micromegas backend is enabled (single-threaded, before any
thread exists; async span events ride thread streams that exist only with CPU tracing on), then builds the
guard, then builds the tokio runtime with `TracingRuntimeExt::with_tracing_callbacks()`; both workarounds go away with the
upstream builder knob.

## Steps

1. Add rig crates (pinned exact versions): `rig-core` (feature `reqwest`, plus defaults), `rig-agent`; dev-dependencies `rig-core` and `rig-agent` with `test-utils`. Extend `rust/deny.toml`'s license allowlist with what the final Phase 1 tree needs (at least ISC, BSD-3-Clause, CDLA-Permissive-2.0, BSL-1.0, MIT-0), plus `[[licenses.clarify]]` for any crate lacking an SPDX expression, so `cargo deny check licenses` stays green. `fontaine-core/src/models.rs`: map Fontaine config
   accounts → rig provider clients (one per account; Anthropic, OpenAI, Gemini, Ollama, custom
   OpenAI-compatible base URLs; `kind = "ollama"` uses rig's native Ollama client (`native_completion()`, not `completion()`, which targets the `/v1` OpenAI-compatible API) against Ollama's native API, `base_url` like `http://localhost:11434`; `openai-compatible` is the kind for OpenAI-style endpoints, built on rig's gateway dialect (`Dialect::gateway(<account>, base_url, …)`) so it speaks Chat Completions and reports a non-catalog vendor name, not the OpenAI Responses dialect), resolve `<account>/<model>` references (unknown account or model
   is a config error, exit 2; an account without a `models` list resolves it through rig's `list_models` before the unknown-model check; `[guardian] model`, when set, gets the same unknown-account/model check, exit 2, while a missing or invalid guardian credential does not fail startup — guardian availability is Phase 4's concern), resolve credentials (env var / the Phase 0 `credentials.rs` lookup, plus the
   `fontaine auth login` subcommand, the writer), and expose the main-model handle and, when `[guardian] model` is explicitly set, the guardian-model handle (`init` reports the guardian model as null when unset). Spike: streaming one prompt to stdout via rig, with
   a tool call round-trip through a side-effect-free, test-only `echo` tool (no gate exists until Phase 2; Phase 2 removes it when it registers the wrapped tool set), to validate rig's ergonomics before building on it.
2. Headless surface `fontaine -p "task"` on the same core — a non-interactive run driven by the CI
   runner, the Phase-0 golden-dataset eval harness, and the integration tests, so the core is
   testable headlessly from day one (the `clap` entry from Phase 0 step 1 wires to a minimal rig-agent
   `AgentBuilder` loop with the `echo` spike tool — enough for `--max-turns`, multi-turn stream-json input
   and the tool round-trip; Phase 2 step 2 replaces it with full session orchestration). Headless semantics and exit codes:
   `tasks/design/permissions.md` §Headless (enforced by the pipeline in Phase 3 for manual/dontAsk and Phase 4 for auto). Output follows the headless I/O contract above
   (`rust/fontaine/src/headless.rs`). CLI surface (one table; each flag lands in the phase shown, the
   contract and schema in this one):

   | Invocation | Meaning | Lands |
   |---|---|---|
   | `fontaine` | interactive TUI | 3 |
   | `fontaine -p "task"` | headless run; result on stdout. Claude Code's shape: `-p/--print` is a boolean and the prompt is positional (read from stdin when absent in text mode); a prompt combined with `--input-format stream-json` is a usage error (exit 2); `auth` is a subcommand, never parsed as a prompt | 1 |
   | `--permission-mode <manual\|acceptEdits\|plan\|auto\|dontAsk>` | session mode, overriding `[permissions] default_mode`; Fontaine's five modes, no bypass; `auto` is a usage error (exit 2) until the guardian ships, as is `[permissions] default_mode = "auto"` from any scope (config error, exit 2) | 1 (`auto` 4) |
   | `--model <account>/<model>` | overrides `[model] model` | 1 |
   | `--output-format text\|json\|stream-json` | output per the I/O contract | 1 |
   | `--input-format text\|stream-json` | `stream-json`: user messages as JSONL on stdin | 1 |
   | `--include-partial-messages` | `stream-json` also emits `assistant_delta` | 1 |
   | `--max-turns <n>` / `--max-budget-usd <x>` | stop the run with `result.subtype` `error_max_turns` / `error_max_budget`, exit 1; a budget with no known price for a configured model is a usage error (exit 2); the pre-check queries the builtin catalog for the resolved vendor/model | 1 |
   | `--json-schema <schema>` | inline JSON Schema string (Claude Code parity), parsed to JSON and converted with `Schema::try_from`, then passed to rig-agent `AgentBuilder::output_schema_raw` (an invalid schema is a usage error, exit 2); structured final output in `result.structured_output` | 1 |
   | `--system-prompt <text>` / `--append-system-prompt <text>` | replace the base system prompt / append after all other layers (layering in Phase 2 step 2) | 1 |
   | `--settings <file>` | CLI-scope settings overlay (`tasks/design/config.md` §Config scope stack) | 0 |
   | `--debug-file <path>` | local debug log (owner-only), `FONTAINE_LOG` filter | 0 |
   | `--add-dir <path>` | extra workspace root | 2 |
   | `--allowed-tools <rule>…` / `--disallowed-tools <rule>…` | CLI-scope `[permissions] allow` / `deny` rules in the `Tool(specifier)` grammar | 3 |
   | `--session-id <uuid>` | use this session id (ledger, audit log, telemetry) | 2 |
   | `--continue` / `--resume <id>` / `--fork-session` | resume the latest / a given session; fork it under a new id | 5 |
   | `--in-container` | user assertion that the process runs inside a container; only an input to `[guardian] profile = "minimal"` resolution, never sufficient alone (the runtime probe must also pass — Phase 4 step 6); a CLI flag, not a config key | 4 |
   | `fontaine auth login <account>` | store the account's credentials in the stored-credentials file; the secret comes from a hidden TTY prompt, or from stdin when stdin is not a TTY | 1 |
   | `fontaine auth login --telemetry` | store the telemetry ingestion key in the stored-credentials file; same secret input | 1 |
3. Telemetry backends (design above): `rust/fontaine/src/telemetry/{micromegas,otlp,span_bridge}.rs`;
   the `SpanBridge` maps rig `chat` / `invoke_agent` spans to `api_request`, `token.usage` and
   `cost.usage` and exports the span tree; a `fontaine.interaction` span with `prompt.id`; content
   opt-ins wired to rig's `record_content_telemetry`, with the SpanBridge/OTLP export path applying `rig_core::redact` and truncating to `content_max_bytes` on recorded content before export; capped flush on exit (`main` drops the guard,
   then returns an `ExitCode`); exporter init failure ⇒ one-line notice, run continues with export
   off. The `SpanBridge` derives `cost.usage` from a catalog lookup on `gen_ai.provider.name` + `gen_ai.request.model`, read in `on_record` too (rig::agent_chat records them late), not from span fields (rig spans carry `gen_ai.usage.*` token fields only). The bin crate gets `[features] default = ["telemetry-micromegas", "telemetry-otlp"]`, backend deps optional and cfg-gated. Env scrubbing lands with the shell tool in Phase 2.
4. Docs — accounts and models: grow `docs/configuration.md` with `[accounts]` and `[model]`
   sections (multiple accounts, `<account>/<model>` addressing, credential sources, user-file-or-CLI-only
   rule, fail-closed guardian on bad credentials) carrying worked
   examples for every supported path (Anthropic, OpenAI, Gemini, Ollama, and OpenAI-compatible
   base URLs: OpenRouter / LM Studio / vLLM / LiteLLM) and env-key handling; create
   `docs/headless.md` (the CLI table with flags that have landed, exit codes, denial semantics, the
   versioned `json`/`stream-json` schema with a link to `docs/schemas/headless-v1.schema.json` — the docs.rs link check covers it — and its role as the CI/eval driver) and link it from
   `docs/architecture.md`; create `docs/telemetry.md` (enabling telemetry — `enabled = true` requires
   the ingestion URL and the ingestion key, by env-var name or `fontaine auth login --telemetry`,
   never inline, and either missing is a config error; both backends with recipes for micromegas
   native span trees and micromegas over OTLP; the metric/event/span table; privacy defaults and
   opt-in keys; the scope rule; telemetry vs the audit log); link both from `docs/index.md`; add
   the README quickstart (a local Ollama agent; the guardian leg lands in Phase 4's docs step) linking into `docs/`; append a CHANGELOG entry. Remove stale text: the "Accepted and validated; not yet active" markers on `[accounts.<name>]` and `[model] model` in `docs/configuration.md`, replace its "No exporter is built yet" `[telemetry]` paragraph with a link to `telemetry.md`, add exit code 1 to its exit-code table, and update `docs/architecture.md`'s "No exporter is built" line.

## Files created or modified

`rust/Cargo.toml` (pins: `rig-core =0.45.0` with `reqwest`, `rig-agent =0.45.0`, dev `rig-core`/`rig-agent` with `test-utils`, `micromegas-tracing =0.32.0`, `micromegas-telemetry-sink =0.32.0`, `opentelemetry`/`opentelemetry_sdk`/`opentelemetry-otlp`/`opentelemetry-appender-tracing =0.33.1`, `tracing-opentelemetry =0.34.0`; `jsonschema =0.59.0` dev-dependency; new crates: `tokio 1.54.1` (caret), `wiremock =0.6.5` (dev), `rpassword 7.5.4` (hidden `auth login` prompt), `uuid 1.28.0` with the `v4` feature (session ids)); `rust/deny.toml`; `rust/fontaine/Cargo.toml` (telemetry features); `rust/fontaine-core/Cargo.toml`; `rust/fontaine-core/src/models.rs`; `rust/fontaine-core/src/credentials.rs` (adds the writer for `auth login`); `rust/fontaine/src/{main,lib,headless}.rs`
(`main.rs` thin; `lib.rs` holds the logic the integration tests import; `-p` flag wiring, I/O contract); `rust/fontaine/src/telemetry/{mod,micromegas,otlp,span_bridge}.rs`;
`rust/fontaine/tests/{headless_smoke,headless_contract,telemetry_micromegas,telemetry_otlp}.rs`;
`docs/schemas/headless-v1.schema.json`; `rust/fontaine-core/tests/{models,accounts,credentials}.rs`; `rust/fontaine/tests/common/mod.rs` (shared scripted-wiremock helper);
`docs/{configuration,architecture,headless,telemetry,index}.md`, `README.md`.

## Tests owned by this phase

- **model wiring** (`tasks/design/testing.md` Testing Strategy): config → rig provider-client mapping (provider
  choice, base URLs, env keys, guardian-model override), with one smoke test per provider against
  a `wiremock` endpoint speaking that provider's wire format (Anthropic, OpenAI, Gemini, and Ollama's native `/api/chat`); the
  OpenAI-compatible mock serves only `openai-compatible`.
- **accounts**: `<account>/<model>`
  resolution with unknown-account and unknown-model errors (including an account without `models`, via a wiremock `/models` route; Ollama: `/api/tags`); two same-kind accounts routed to their
  own `wiremock` endpoints; credentials resolved from env (and a temp stored-credentials file) with no live API.
- **credentials writer** (`rust/fontaine-core/tests/credentials.rs`, temp `FONTAINE_DATA_DIR`): `auth login` entries (account and telemetry) round-trip through the lookup; the file is owner-only (0600 on Unix, current-user-only ACL on Windows).
- Integration surface: the headless `-p` run is the driver for every later integration test. The
  offline driver is an `openai-compatible` account whose `base_url` is a scripted `wiremock`
  endpoint, built by the shared helper `rust/fontaine/tests/common/mod.rs`: a scripted
  openai-compatible (and catalog-priced `kind = "openai"`, see the telemetry bullet; its mock speaks the Responses wire format at `/responses`, including streaming events) wiremock builder
  with a turn script, tool-call responses, `usage` fields, and temp
  `FONTAINE_CONFIG_DIR` / `FONTAINE_DATA_DIR` setup. Headless tests written before Phase 4 pass `--permission-mode manual` explicitly, so the Phase 4 default flip does not change them. Later phases reuse it; `rust/fontaine/tests/headless_smoke.rs` runs `fontaine -p` against it (streamed prompt,
  tool-call round-trip, exit codes 0/1/2) and covers CLI parsing (`--permission-mode auto` rejected
  before Phase 4; no bypass value). Library-level tests (`rust/fontaine-core/tests/{models,accounts}.rs`)
  use `rig_core::test_utils` models.
- **headless contract** (`rust/fontaine/tests/headless_contract.rs`): `json` and `stream-json` output
  validate against `docs/schemas/headless-v1.schema.json` (`jsonschema =0.59.0`, a dev-dependency); `init` (roots, config sources, trust state) and `result` present;
  `--max-turns` / `--max-budget-usd` stop with the matching subtype and exit 1 (the budget test uses a catalog-priced account, see the telemetry bullet; `--max-budget-usd` against an unpriced `openai-compatible` model is a usage error, exit 2); `--input-format
  stream-json` drives a two-turn run; `--json-schema '<inline schema>'` fills `result.structured_output`; `--input-format stream-json` input lines validate against the schema too.
- **telemetry**: `-p` against the scripted `wiremock` endpoint returning `usage` yields `api_request` with tokens and cost — cost and budget tests use an account of `kind = "openai"` (or `anthropic`) whose `base_url` points at the wiremock server and whose model id is in rig's price catalog, because rig 0.45's `Catalog` prices by provider vendor and model id and a custom `openai-compatible` endpoint has no price; **span tree
  round-trip** (`telemetry_micromegas.rs`, its own single-test binary; it sets `MICROMEGAS_ENABLE_CPU_TRACING` once at the start before any thread spawns, and further tests in the file serialize via a `static Mutex<()>` guard): a nested span tree,
  inner spans closed on a different tokio worker than they opened on, passes through the `SpanBridge`
  into micromegas `InMemorySink`, and decoding the thread blocks gives one begin/end async span event
  pair per span with `parent_span_id` equal to the parent's `span_id` (0 at the root), `depth` + 1 per
  level, and the span fields as end-event properties; a wiremock OTLP endpoint receives protobuf on
  `/v1/traces` (parentage intact) and `/v1/metrics`; in `telemetry_otlp.rs`, the canary test (prompt and response legs: no
  content without the opt-in keys, and credential-shaped strings redacted even with them) and a black-hole exporter: `-p` exits within the flush cap with the
  same result as telemetry off; in `telemetry_micromegas.rs`, with `enabled = false` (injected env) nothing is built even when `MICROMEGAS_TELEMETRY_URL` / `OTEL_EXPORTER_OTLP_ENDPOINT` are set.

## Definition of done

- Offline gates (no keys, no human): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green from `rust/`, including `rust/fontaine-core/tests/{models,accounts,credentials}.rs`,
  `rust/fontaine/tests/{headless_smoke,headless_contract,telemetry_micromegas,telemetry_otlp}.rs`.
- `fontaine -p "…"` runs non-interactively and exits with the run's result.
- Multiple accounts resolve `<account>/<model>` and route to their own endpoints.
- Provider docs examples are covered by `rust/fontaine-core/tests/docs.rs` (toml blocks validate via `config.rs`'s validation function, links resolve, no `TODO`/`TBD`).
- `json` / `stream-json` output validates against `docs/schemas/headless-v1.schema.json`; `docs/headless.md` documents it.
- With telemetry enabled, the micromegas backend exports a span tree with correct parentage
  (`InMemorySink` test) and the OTLP backend exports to a mock collector; `docs/telemetry.md` exists.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log; set Phase 1 to done in the master Phase Tracker and on this plan's Status line, and refresh the master `## Current State`.

## Open Questions

- When does provider model listing run (at reference resolution vs. startup for every account), and does a failed listing call exit 1 or 2?
