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
Credentials come from a named env var or the stored-credentials file (`fontaine auth login`, user
data dir, 0600); raw keys are never read from config files. Accounts and all account/model/endpoint
keys are user-scope only (a project file is rejected at load); a missing or invalid credential on
the guardian's account makes the guardian unavailable and fails closed. The "any model I like"
requirement is met by dependency rather than by code we maintain.

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
   testable headlessly from day one (the `clap` entry from Phase 0 step 1 wires to the rig model
   layer now and to the agent loop as soon as Phase 2 step 2 lands). Headless semantics (Claude Code
   parity; enforced by the pipeline in Phase 3 for manual/dontAsk and Phase 4 for auto): nothing
   prompts — every would-prompt outcome (ask-rule match, Escalate verdict,
   critical-path/protected-path prompt, loop breaker) becomes a Deny returned to the
   agent as the tool_result and the run continues; headless `manual` = anything not pre-allowed is
   denied; headless `auto` = guardian-only adjudication. Exit codes: 0 run completed (denials do not
   change it), 1 run error, 2 config or usage error. CLI surface:

   | Invocation | Meaning |
   |---|---|
   | `fontaine` | interactive TUI (Phase 3) |
   | `fontaine -p "task"` | headless run; result on stdout |
   | `--permission-mode <manual\|acceptEdits\|plan\|auto\|dontAsk>` | session mode, overriding `[permissions] default_mode`; Fontaine's five modes, no bypass; `auto` is a usage error (exit 2) until the guardian ships (Phase 4) |
   | `--model <account>/<model>` | overrides `[model] model` |
   | `--output-format text\|json` | headless output; `json` is one final object with the result text and the list of denials (tool, reason) |
   | `--in-container` | user assertion that the process runs inside a container; only an input to `[guardian] profile = "minimal"` resolution, never sufficient alone (the runtime probe must also pass — Phase 4 step 5); a CLI flag, not a config key |
   | `fontaine auth login <account>` | store the account's credentials in the stored-credentials file |
3. Docs — accounts and models: grow `docs/configuration.md` with `[accounts]` and `[model]`
   sections (multiple accounts, `<account>/<model>` addressing, credential sources, user-scope-only
   rule, fail-closed guardian on bad credentials) carrying worked
   examples for every supported path (Anthropic, OpenAI, Gemini, Ollama, and OpenAI-compatible
   base URLs: OpenRouter / LM Studio / vLLM / LiteLLM) and env-key handling; document the headless
   `-p` surface (the CLI table, exit codes, denial semantics, its role as the CI/eval driver) in
   `docs/architecture.md`; refresh the README quickstart's links into `docs/`; append a CHANGELOG entry.

## Files created or modified

`rust/Cargo.toml` (rig pins); `rust/fontaine-core/src/models.rs` (plus the accounts/credentials config types and `auth login`); `rust/fontaine/src/main.rs`
(`-p` flag wiring); `docs/configuration.md`, `docs/architecture.md`, `README.md`.

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

## Definition of done

- Spike completes a streamed prompt + tool round-trip against a mock endpoint.
- `fontaine -p "…"` runs non-interactively and exits with the run's result.
- Multiple accounts resolve `<account>/<model>` and route to their own endpoints; project-scope
  `[accounts]` is rejected; bad guardian-account credentials fail closed.
- Provider docs examples compile-check against the config loader's schema tests.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
