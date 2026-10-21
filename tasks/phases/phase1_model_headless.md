# Phase 1 — Model layer + headless surface (mostly dependency wiring)

**Status:** not started · **Depends on:** [Phase 0](phase0_scaffold.md) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Model layer: rig-core providers (replaces fontaine-llm)

No custom `Provider` trait. `rig_core` supplies provider-neutral messages, completion, streaming,
and 20+ providers behind one interface (Anthropic, OpenAI, Gemini, Ollama, Bedrock, Vertex, and
custom OpenAI-compatible endpoints for OpenRouter/LM Studio/vLLM/LiteLLM). The **same rig model
abstraction serves both the main agent and the guardian** — the guardian is just another rig agent
pointed at a different model handle (`guardian.model`). Streaming to the TUI uses rig's streaming
events; structured verdicts use rig's `structured_output` synthetic-tool path (see the master
plan's §Ecosystem / dependencies). Provider selection comes from Fontaine config **accounts** (`[accounts.<name>]`: `kind`,
optional `base_url`, credential source) and maps onto rig client constructors — one rig client per
account, so two accounts of the same kind (work + personal Anthropic) coexist with separate
credentials and endpoints. Each account provides models: an explicit `models` list, else discovery
from the provider where rig supports listing. Models are addressed `<account>/<model>` in `[model]`,
`[guardian] model`, and `[agents.types]`, so main agent and guardian may sit on different accounts.
Credentials come from a named env var or the stored-credentials file (`fontaine auth login`,
`<data dir>/credentials.toml`, owner-only); raw keys are never read from config files. Accounts and all account/model/endpoint
keys follow the master scope table (`[accounts]` only from the user file or CLI; no
account/model/endpoint reference from shared project scope — rejected at load); a missing or invalid credential on
the guardian's account makes the guardian unavailable and fails closed. The "any model I like"
requirement is met by dependency rather than by code we maintain.

### Headless I/O contract (versioned)

`fontaine -p` is the surface the eval runner, the CI integration tests and any external consumer
parse, so its output is a versioned contract (Claude Code parity for the flags). `--output-format
text` prints the final result text; `json` prints one final `result` object; `stream-json` prints
JSONL events. Every JSON object carries `schema_version` (starting at 1; additive fields keep it, any
rename or removal bumps it) and `session_id`. Event types:

- `init` — model, guardian model and profile, mode, workspace roots, loaded config sources with
  their scopes, workspace trust state, allow rules suspended by `auto`.
- `assistant` — a complete assistant message; with `--include-partial-messages`, also `assistant_delta`.
- `tool_call` — `tool_use_id`, tool, args.
- `gate_decision` — `tool_use_id`, `tier` (same values as the audit log), `decision`, `rule`, and for
  guardian decisions `explanation`, `confidence`, `guardian_model`.
- `tool_result` — `tool_use_id`, success, the result text as the agent receives it.
- `result` — `subtype` (`success` | `error_max_turns` | `error_max_budget` | `error_run`), result
  text, `structured_output` (with `--json-schema`), usage and cost (main / guardian / subagents),
  and the denial list (tool, tier, rule, reason).

`--input-format stream-json` reads user messages as JSONL on stdin for a multi-turn headless run. An
external approval delegate (Claude Code's `--permission-prompt-tool`) is not in v1: every
would-prompt outcome stays a Deny.

### Telemetry backends

The export slot from Phase 0 gets both backends from the master plan's §Observability: the
micromegas sink guard (local sink, global capture and system monitor off; explicit endpoint +
`ApiKeyRequestDecorator` from the configured key) with `TracingCaptureLayer` composed into
Fontaine's registry and the `SpanBridge` mapping allowlisted spans to micromegas async span events
(span id + parent span id + depth) so the tree survives, and the OTLP/HTTP backend
(`tracing-opentelemetry`, `opentelemetry-appender-tracing`, metrics via an OTel `Meter`). The bin sets
`MICROMEGAS_ENABLE_CPU_TRACING=true` before building the micromegas guard while still single-threaded
(async span events ride thread streams that exist only with CPU tracing on) and builds the tokio
runtime with `TracingRuntimeExt::with_tracing_callbacks()`; both workarounds go away with the
upstream builder knob.

## Steps

1. Add rig crates (pinned exact versions): `rig-core`, `rig-agent`, `rig-memory`, `rig-tools`,
   `rig-rmcp`, `rig-cassette` (dev). `fontaine-core/src/models.rs`: map Fontaine config
   accounts → rig provider clients (one per account; Anthropic, OpenAI, Gemini, Ollama, custom
   OpenAI-compatible base URLs), resolve `<account>/<model>` references (unknown account or model
   is a config error, exit 2), resolve credentials (env var / stored-credentials file, plus the
   `fontaine auth login` subcommand), and expose main-model + guardian-model handles. Spike: streaming one prompt to stdout via rig, with
   a tool call round-trip, to validate rig's ergonomics before building on it.
2. Headless surface `fontaine -p "task"` on the same core — a non-interactive run driven by the CI
   runner, the Phase-0 golden-dataset eval harness, and the integration tests, so the core is
   testable headlessly from day one (the `clap` entry from Phase 0 step 1 wires to a minimal rig-agent
   `AgentBuilder` loop with the spike tool — enough for `--max-turns`, multi-turn stream-json input
   and the tool round-trip; Phase 2 step 2 replaces it with full session orchestration). Headless semantics (Claude Code
   parity; enforced by the pipeline in Phase 3 for manual/dontAsk and Phase 4 for auto): nothing
   prompts — every would-prompt outcome (ask-rule match, Escalate verdict,
   critical-path/protected-path prompt, loop breaker) becomes a Deny returned to the
   agent as the tool_result and the run continues; headless `manual` = anything not pre-allowed is
   denied; headless `auto` = guardian-only adjudication. Exit codes: 0 run completed (denials do not
   change it), 1 run error, 2 config or usage error. Output follows the headless I/O contract above
   (`rust/fontaine/src/headless.rs`). CLI surface (one table; each flag lands in the phase shown, the
   contract and schema in this one):

   | Invocation | Meaning | Lands |
   |---|---|---|
   | `fontaine` | interactive TUI | 3 |
   | `fontaine -p "task"` | headless run; result on stdout | 1 |
   | `--permission-mode <manual\|acceptEdits\|plan\|auto\|dontAsk>` | session mode, overriding `[permissions] default_mode`; Fontaine's five modes, no bypass; `auto` is a usage error (exit 2) until the guardian ships | 1 (`auto` 4) |
   | `--model <account>/<model>` | overrides `[model] model` | 1 |
   | `--output-format text\|json\|stream-json` | output per the I/O contract | 1 |
   | `--input-format text\|stream-json` | `stream-json`: user messages as JSONL on stdin | 1 |
   | `--include-partial-messages` | `stream-json` also emits `assistant_delta` | 1 |
   | `--max-turns <n>` / `--max-budget-usd <x>` | stop the run with `result.subtype` `error_max_turns` / `error_max_budget`, exit 1; a budget with no known price for a configured model is a usage error (exit 2) | 1 |
   | `--json-schema <file>` | structured final output via rig `structured_output`, in `result.structured_output` | 1 |
   | `--system-prompt <text>` / `--append-system-prompt <text>` | replace / extend the system prompt | 1 |
   | `--settings <file>` | CLI-scope settings overlay (master §Config scope stack) | 1 |
   | `--debug-file <path>` | local debug log (owner-only), `FONTAINE_LOG` filter | 1 |
   | `--add-dir <path>` | extra workspace root | 2 |
   | `--allowed-tools <rule>…` / `--disallowed-tools <rule>…` | CLI-scope `[permissions] allow` / `deny` rules in the `Tool(specifier)` grammar | 3 |
   | `--session-id <uuid>` | use this session id (ledger, audit log, telemetry) | 2 |
   | `--continue` / `--resume <id>` / `--fork-session` | resume the latest / a given session; fork it under a new id | 5 |
   | `--in-container` | user assertion that the process runs inside a container; only an input to `[guardian] profile = "minimal"` resolution, never sufficient alone (the runtime probe must also pass — Phase 4 step 5); a CLI flag, not a config key | 1 (read from 4) |
   | `fontaine auth login <account>` | store the account's credentials in the stored-credentials file | 1 |
   | `fontaine auth login --telemetry` | store the telemetry ingestion key in the stored-credentials file | 1 |
3. Telemetry backends (design above): `rust/fontaine/src/telemetry/{micromegas,otlp,span_bridge}.rs`;
   the `SpanBridge` maps rig `chat` / `invoke_agent` spans to `api_request`, `token.usage` and
   `cost.usage` and exports the span tree; a `fontaine.interaction` span with `prompt.id`; content
   opt-ins wired to rig's `record_content_telemetry`; capped flush on exit (`main` drops the guard,
   then returns an `ExitCode`); exporter init failure ⇒ one-line notice, run continues with export
   off. Env scrubbing lands with the shell tool in Phase 2.
4. Docs — accounts and models: grow `docs/configuration.md` with `[accounts]` and `[model]`
   sections (multiple accounts, `<account>/<model>` addressing, credential sources, user-scope-only
   rule, fail-closed guardian on bad credentials) carrying worked
   examples for every supported path (Anthropic, OpenAI, Gemini, Ollama, and OpenAI-compatible
   base URLs: OpenRouter / LM Studio / vLLM / LiteLLM) and env-key handling; create
   `docs/headless.md` (the CLI table with flags that have landed, exit codes, denial semantics, the
   versioned `json`/`stream-json` schema, its role as the CI/eval driver) and link it from
   `docs/architecture.md`; create `docs/telemetry.md` (enabling telemetry — `enabled = true` requires
   the ingestion URL and the ingestion key, by env-var name or `fontaine auth login --telemetry`,
   never inline, and either missing is a config error; both backends with recipes for micromegas
   native span trees and micromegas over OTLP; the metric/event/span table; privacy defaults and
   opt-in keys; the scope rule; telemetry vs the audit log); link both from `docs/index.md`; refresh
   the README quickstart's links into `docs/`; append a CHANGELOG entry.

## Files created or modified

`rust/Cargo.toml` (rig + telemetry pins); `rust/fontaine-core/src/models.rs` (plus the accounts/credentials config types and `auth login`); `rust/fontaine/src/{main,headless}.rs`
(`-p` flag wiring, I/O contract); `rust/fontaine/src/telemetry/{micromegas,otlp,span_bridge}.rs`;
`rust/fontaine/tests/{headless_smoke,headless_contract,telemetry_micromegas,telemetry_otlp}.rs`;
`docs/{configuration,architecture,headless,telemetry,index}.md`, `README.md`.

## Tests owned by this phase

- **model wiring** (master Testing Strategy): config → rig provider-client mapping (provider
  choice, base URLs, env keys, guardian-model override), with one smoke test per provider against
  a `wiremock` endpoint speaking that provider's wire format (Anthropic, OpenAI, Gemini); the
  OpenAI-compatible mock serves only openai-compatible/Ollama.
- **accounts**: config parsing of multiple accounts (incl. two of the same kind); `<account>/<model>`
  resolution with unknown-account and unknown-model errors; two same-kind accounts routed to their
  own `wiremock` endpoints; project-scope `[accounts]` (and `[model]`/`[guardian] model`) rejected;
  credentials resolved from env (and a temp stored-credentials file) with no live API; inline raw
  key rejected; missing guardian-account credential ⇒ guardian unavailable.
- Integration surface: the headless `-p` run is the driver for every later integration test. The
  offline driver is an `openai-compatible` account whose `base_url` is a scripted `wiremock`
  endpoint; `rust/fontaine/tests/headless_smoke.rs` runs `fontaine -p` against it (streamed prompt,
  tool-call round-trip, exit codes 0/1/2) and covers CLI parsing (`--permission-mode auto` rejected
  before Phase 4; no bypass value). Library-level tests (`rust/fontaine-core/tests/{models,accounts}.rs`)
  use `rig_core::test_utils` models.
- **headless contract** (`rust/fontaine/tests/headless_contract.rs`): `json` and `stream-json` output
  validate against the schema snapshot for `schema_version` 1; `init` and `result` present;
  `--max-turns` / `--max-budget-usd` stop with the matching subtype and exit 1; `--input-format
  stream-json` drives a two-turn run; `--json-schema` fills `result.structured_output`.
- **telemetry**: `-p` against the scripted `wiremock` `openai-compatible` endpoint returning `usage` yields `api_request` with tokens and cost; **span tree
  round-trip** (`telemetry_micromegas.rs`, its own test binary, `serial_test`): a nested span tree,
  inner spans closed on a different tokio worker than they opened on, passes through the `SpanBridge`
  into micromegas `InMemorySink`, and decoding the thread blocks gives one begin/end async span event
  pair per span with `parent_span_id` equal to the parent's `span_id` (0 at the root), `depth` + 1 per
  level, and the span fields as end-event properties; a wiremock OTLP endpoint receives protobuf on
  `/v1/traces` (parentage intact) and `/v1/metrics`; the canary test (prompt and response legs: no
  content without the opt-in keys); a black-hole exporter: `-p` exits within the flush cap with the
  same result as telemetry off.

## Definition of done

- Offline gates (no keys, no human): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green from `rust/`, including `rust/fontaine-core/tests/{models,accounts}.rs`,
  `rust/fontaine/tests/{headless_smoke,headless_contract,telemetry_micromegas,telemetry_otlp}.rs`.
- Spike completes a streamed prompt + tool round-trip against a mock endpoint.
- `fontaine -p "…"` runs non-interactively and exits with the run's result.
- Multiple accounts resolve `<account>/<model>` and route to their own endpoints; project-scope
  `[accounts]` is rejected; bad guardian-account credentials fail closed.
- Provider docs examples compile-check against the config loader's schema tests.
- `json` / `stream-json` output matches the versioned schema; `docs/headless.md` documents it.
- With telemetry enabled, the micromegas backend exports a span tree with correct parentage
  (`InMemorySink` test) and the OTLP backend exports to a mock collector; `docs/telemetry.md` exists.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
