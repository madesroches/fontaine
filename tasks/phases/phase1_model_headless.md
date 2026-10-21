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
plan's §Ecosystem / dependencies). Provider selection comes from Fontaine config and maps onto
rig client constructors (`OpenAI::from_env()`, `Anthropic::…`, custom base URLs). The "any model
I like" requirement is met by dependency rather than by code we maintain.

## Steps

1. Add rig crates (pinned exact versions): `rig-core`, `rig-agent`, `rig-memory`, `rig-tools`,
   `rig-rmcp`, `rig-cassette` (dev). `fontaine-core/src/models.rs`: map Fontaine config → rig
   provider clients (Anthropic, OpenAI, Gemini, Ollama, custom OpenAI-compatible base URLs) and
   expose main-model + guardian-model handles. Spike: streaming one prompt to stdout via rig, with
   a tool call round-trip, to validate rig's ergonomics before building on it.
2. Headless surface `fontaine -p "task"` on the same core — a non-interactive run driven by the CI
   runner, the Phase-0 golden-dataset eval harness, and the integration tests, so the core is
   testable headlessly from day one (the `clap` entry from Phase 0 step 1 wires to the rig model
   layer now and to the agent loop as soon as Phase 2 step 2 lands). Headless semantics (Claude Code
   parity; enforced by the pipeline in Phase 3 for manual/dontAsk and Phase 4 for auto): nothing
   prompts — every would-prompt outcome (ask-rule match, Escalate verdict, low-confidence
   escalation, critical-path/protected-path prompt, loop breaker) becomes a Deny returned to the
   agent as the tool_result and the run continues; headless `manual` = anything not pre-allowed is
   denied; headless `auto` = guardian-only adjudication. Exit codes: 0 run completed (denials do not
   change it), 1 run error, 2 config or usage error.
3. Docs — provider setup: grow `docs/configuration.md` with a `[model]` section carrying worked
   examples for every supported path (Anthropic, OpenAI, Gemini, Ollama, and OpenAI-compatible
   base URLs: OpenRouter / LM Studio / vLLM / LiteLLM) and env-key handling; document the headless
   `-p` surface (flags, exit codes, denial semantics, its role as the CI/eval driver) in
   `docs/architecture.md`; refresh the README quickstart's links into `docs/`; append a CHANGELOG entry.

## Files created or modified

`rust/Cargo.toml` (rig pins); `rust/fontaine-core/src/models.rs`; `rust/fontaine/src/main.rs`
(`-p` flag wiring); `docs/configuration.md`, `docs/architecture.md`, `README.md`.

## Tests owned by this phase

- **model wiring** (master Testing Strategy): config → rig provider-client mapping (provider
  choice, base URLs, env keys, guardian-model override), with one smoke test per provider against
  a `wiremock` endpoint speaking that provider's wire format (Anthropic, OpenAI, Gemini); the
  OpenAI-compatible mock serves only openai-compatible/Ollama.
- Integration surface: the headless `-p` run is the driver for every later integration test
  (cassette replay lands with Phase 2/3 tests).

## Definition of done

- Spike completes a streamed prompt + tool round-trip against a mock endpoint.
- `fontaine -p "…"` runs non-interactively and exits with the run's result.
- Provider docs examples compile-check against the config loader's schema tests.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
