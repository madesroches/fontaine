# Guardian Thinking Off Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/144

## Overview

The guardian and the `llm` scanner extract their verdict through rig's `ExtractorBuilder`, which
always forces a tool call (`ToolChoice::Required`). Models that think by default refuse a forced
tool call, so on such a model every guardian call is a 400 and every gated command is denied as
`GuardianUnavailable`. The reported case is `deepseek-v4.1-flash` on an `opencode-go` account. This
plan turns thinking off on the guardian's and the scanner's requests on the wires that need it, using
the parameter each wire defines. The forced tool call stays, and the fail-closed handling is
unchanged.

## Current State

- `fontaine-guardian/src/guardian.rs:239` builds
  `ExtractorBuilder::<Verdict>::new(model).add_hook(..).append_preamble(..).max_tokens(1024).retries(2)`.
  `rig-agent-0.45.0/src/extractor.rs:135` hard-codes `tool_choice(ToolChoice::Required)`. A provider
  400 surfaces as `StructuredOutputError::Prompt` and maps to `FailClosed::Error`
  (`guardian.rs:259`).
- `fontaine-guardian/src/scanner.rs:119` builds the same extractor for `Reported`. Every error is
  `Scan::Failed`, which reads as suspect.
- Both take a bare `DynModel<Completion>`. `Guardian::new` is called from
  `fontaine-core/src/agent.rs:133` (`build_guardian`) and `LlmScanner::new` from
  `fontaine-core/src/scanner_stage.rs:56` (`llm_backend`). Both get their model from a
  `ModelHandle` (`fontaine-core/src/models.rs:107`).
- A `ModelHandle` is built from an account client in only two places: the main model
  (`models.rs:381`) and an explicit or default guardian (`models.rs:507`). Every other guardian
  (the main-model fallback, `/guardian-model`, a switched main model) is a clone of one of those.
- `AccountClient` (`fontaine-core/src/providers/mod.rs:147`) has `completion` and `list`.
  `opencode_go::GoClient` (`opencode_go.rs:172`) routes a model to the `Chat`, `Responses`,
  `Messages` or `Gemini` wire via `wire(model, overrides)`.
- `ExtractorBuilder::additional_params(Value)` exists and forwards to the agent builder. rig merges
  it into the request body on every wire.
- rig's portable `GenerationOptions::reasoning(Reasoning::Off)` is mapped per dialect, but both of
  fontaine's gateway dialects (`opencode-go` and `openai-compatible`, built with
  `Dialect::gateway`) fall to `refuse_all` on the Chat wire
  (`rig-core-0.45.0/src/providers/openai/options.rs:522-526`). So the portable option cannot reach the
  reported case.
- The Ollama-native screen already sends `"think": false` itself (`screen.rs:342`). It does not
  use the extractor and is not affected.

## Design

### Where the knowledge lives

Only the provider layer knows which wire a model is served on, so it says how to turn thinking off.
The guardian crate stays wire-agnostic. It sends whatever request parameters it is handed.

```
providers::AccountClient::thinking_off(model) -> Option<Value>   (per kind / per wire)
        │
models.rs: ModelHandle { completion, thinking_off, .. }           (set where the handle is built)
        │
agent::build_guardian ──► Guardian::new(..).with_additional_params(handle.thinking_off)
fontaine-evals live.rs ──► Guardian::new(..).with_additional_params(handle.thinking_off)
scanner_stage::llm_backend ──► LlmScanner::new(..).with_additional_params(handle.thinking_off)
        │
ExtractorBuilder::additional_params(params)  ──► merged into every extraction attempt's body
```

### Type and interface changes

- `fontaine-core/src/providers/mod.rs`: add a default method to `AccountClient`:
  ```rust
  /// What the request body must carry to turn `model`'s thinking off, where its wire needs it:
  /// the guardian and the scanner force a tool call, which a thinking model refuses.
  fn thinking_off(&self, _model: &str) -> Option<serde_json::Value> { None }
  ```
- `fontaine-core/src/providers/opencode_go.rs`: a free function `thinking_off(wire: Wire) -> Option<Value>`
  maps each wire, and `GoClient::thinking_off` calls it on `wire(model, &self.overrides)`:
  - `Chat` → `{"thinking": {"type": "disabled"}}`. This is the off switch of DeepSeek, GLM and
    kimi-k2.6; rig's own `deepseek`, `zai` and `moonshot` dialects send the same value (moonshot
    only for kimi-k2.6). kimi-k3 and other Kimi models cannot turn thinking off.
  - `Messages` → `{"thinking": {"type": "disabled"}}`. This is the Anthropic schema's `disabled`
    value.
  - `Responses`, `Gemini` → `None`. Both accept a forced tool call while reasoning, so their
    requests stay as they are.
- `fontaine-core/src/models.rs`: `ModelHandle` gains `pub thinking_off: Option<serde_json::Value>`.
  `ModelHandle::new` keeps its three-argument signature and sets `thinking_off: None`, so test call
  sites do not change. A new constructor `ModelHandle::from_client(reference, kind, &dyn AccountClient)` sets
  both `completion` and `thinking_off` from the client. The two client-built sites (`models.rs:381`,
  `models.rs:507`) use it. Because the field lives on the handle, the main-model fallback and every
  later switch carry it through `clone()`.
- `fontaine-guardian/src/guardian.rs`: `Guardian` gains
  `additional_params: Option<serde_json::Value>` and a builder
  `pub fn with_additional_params(mut self, params: Option<Value>) -> Self`. In `run`, when the field
  is set, it is passed to `ExtractorBuilder::additional_params`. With `None`, nothing changes.
- `fontaine-guardian/src/scanner.rs`: `LlmScanner` gains the same field and builder, applied the
  same way.
- `fontaine-core/src/agent.rs` (`build_guardian`), `fontaine-core/src/scanner_stage.rs`
  (`llm_backend`) and `fontaine-evals/src/live.rs:117` (the live eval guardian, whose handle comes
  from `switcher.guardian(..)`) chain `.with_additional_params(handle.thinking_off.clone())`.

The parameters affect only the guardian's and the scanner's requests. The main agent's requests
never see them.

## Implementation Steps

1. **Guardian crate**: add `additional_params` and `with_additional_params` to `Guardian` and
   `LlmScanner`, and apply them in each `ExtractorBuilder` chain.
2. **Provider trait**: add `AccountClient::thinking_off` with its `None` default.
3. **opencode-go**: add the free function `thinking_off(wire: Wire) -> Option<Value>` (the two
   arms share one body value) and implement `GoClient::thinking_off` by calling it on
   `wire(model, &self.overrides)`.
4. **Model handle**: add `ModelHandle::thinking_off` and `ModelHandle::from_client`. Switch
   `models.rs:381` and `models.rs:507` to it.
5. **Wiring**: pass `handle.thinking_off` in `agent::build_guardian`,
   `scanner_stage::llm_backend` and `fontaine-evals/src/live.rs`.
6. **Tests**: see Testing Strategy.
7. **Docs**: update `docs/guardian.md` (see Documentation).

## Files to Modify

- `rust/fontaine-guardian/src/guardian.rs`
- `rust/fontaine-guardian/src/scanner.rs`
- `rust/fontaine-core/src/providers/mod.rs`
- `rust/fontaine-core/src/providers/opencode_go.rs`
- `rust/fontaine-core/src/models.rs`
- `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-core/src/scanner_stage.rs`
- `rust/fontaine-evals/src/live.rs`
- `rust/fontaine-core/tests/opencode_go.rs`
- `rust/fontaine-guardian/tests/guardian.rs` and `rust/fontaine-guardian/tests/scanner.rs`
- `docs/guardian.md`
- `CHANGELOG.md` (`### Fixed` under `[Unreleased]` for #144, via the PR step)

## Trade-offs

- **Thinking off (chosen) vs. `tool_choice: auto` with our own call.** Auto would need no
  per-wire knowledge. But a thinking model would still reason inside the 1 024-token (guardian) and
  512-token (scanner) budgets, often running out before it calls `submit`. That turns the 400 into
  a retried, unparseable answer, so it is still unusable, just slower. It would also change the
  forced call every working guardian runs today. Thinking off is the issue's preferred option, and
  it is faster and cheaper.
- **Explicit `additional_params` vs. rig's portable `Reasoning::Off`.** It would also
  change requests on wires that work today (OpenAI `reasoning.effort = none`, Gemini thinking
  budgets) and lower guardian quality there. The `Ignore` policy would also log a warning on every
  call. Explicit parameters touch only the wires that need them.
- **Per wire, not per model.** Go routes models by wire, and a model that cannot turn thinking off
  thinks by default, so it already fails with a forced tool call. Sending the off switch to it
  cannot make it worse. A per-model table would need maintaining for every gateway model release.
- **Field on `ModelHandle` vs. a lookup at guardian-build time.** The guardian is built from a
  handle in several places (startup, `/guardian-model`, main-model fallback, `/model` switches).
  Carrying the value on the handle covers them all without threading the account client through.

## Out of Scope

- `openai-compatible` accounts: the endpoint is arbitrary, and no single parameter is safe to send
  to every server (OpenAI's own Chat Completions rejects unknown fields). A thinking-by-default
  model there still fails closed. The documented workaround is a non-thinking guardian model.
- Ollama: the native wire ignores `tool_choice`, so there is no 400 to fix.

## Documentation

- `docs/guardian.md`, "Choosing the guardian model": add a short paragraph. The guardian's and the
  scanner's calls force a structured answer, so Fontaine turns thinking off for them on
  `opencode-go` Chat and Messages models. On an `openai-compatible` account, a model that thinks by
  default cannot be the guardian, and every call is denied as `GuardianUnavailable`; pick a model
  without default thinking.

## Testing Strategy

All of these are wiremock tests. No live API is touched.

1. **Regression, pinning the reported bug** (`fontaine-core/tests/opencode_go.rs`). The gateway
   stand-in's `/openai/v1/chat/completions` mimics the upstream. It answers the issue's 400 body
   (`"Thinking mode does not support this tool_choice"`) unless the request carries
   `thinking.type == "disabled"`, and otherwise answers a `submit` tool call with an Allow verdict.
   Add `deepseek-v4.1-flash` to the stand-in's listing in `gateway()` (lines 69-76, which today
   lists only kimi-k3, gpt-5.6-luna, claude-haiku-5-5 and gemini-4-argon-contributor). Resolve
   `go/deepseek-v4.1-flash`, build the guardian with
   `agent::build_guardian(Some(&GuardianModel::Ready(handle)), &config, profile)`, adjudicate,
   and assert an Allow with no `fail_closed`. Before the fix this test fails with
   `FailClosed::Error`, which is the bug.
2. **Scanner, same wire** (separate test in the same file): the guardian and scanner schemas are
   both `deny_unknown_fields` on the same `submit` tool, so one canned response cannot answer both.
   Mount its own conditional chat mock answering a `clean` verdict, reuse `signed_in_world`/`resolve`
   and `w.load()` for the `Config`, build `scanner_stage::llm_backend` on the handle, and assert
   `Scan::Clean`.
3. **Messages wire**: the test mounts its own `/anthropic/v1/messages` mock and captures the
   guardian request body for `claude-haiku-5-5` (listed by `gateway()`, absent from rig's anthropic
   catalog; rig drops the forced tool choice for catalog models with `rejects_forced_tool_choice`).
   Assert that `tool_choice {"type":"any"}` appears together with `thinking.type == "disabled"`.
4. **Guardian crate tests** (`rust/fontaine-guardian/tests/guardian.rs` and
   `rust/fontaine-guardian/tests/scanner.rs`, using the `tests/common/mod.rs` `Endpoint` seam):
   with `None`, the request body is unchanged and has no extra keys. With
   `with_additional_params(Some(..))`, the params reach the body beside the forced tool choice.
5. **Provider unit test** (`opencode_go.rs` tests module): the free function `thinking_off` per
   wire. The override case is already covered by the existing `wire()` override test.

## Manual Verification

1. With an `opencode-go` account signed in, set `[guardian] model = "<account>/deepseek-v4.1-flash"`
   and run `cargo run -p fontaine -- -p "run ls ../" --permission-mode auto` from `rust/`
   (headless starts in `manual`, and a workspace-contained `ls` takes the read-only fast path; an
   out-of-scope read goes to the guardian, per `docs/permissions.md`). Expected: the stream/audit
   shows a guardian verdict for the call, not `GuardianUnavailable`. This is not automated because it checks the
   live gateway forwarding `thinking` to the upstream model. That is a real dependency's behavior,
   and a stand-in cannot reproduce it.

## Open Questions

None.

## Decisions
