# Ollama Context Window Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/96

## Overview

Fontaine never tells Ollama how large a context to load a model with, so the daemon uses its
default (4096 tokens unless the Modelfile or `OLLAMA_CONTEXT_LENGTH` says otherwise). The
guardian's rendered default rules alone are about 4,000 tokens, and Ollama silently truncates an
over-long prompt from the front, so a local guardian judges calls without most of its rules — and
a local main model loses its system prompt the same way. This plan makes every request to an
`ollama` account carry `options.num_ctx` (a new account key, `context_tokens`, default 16384),
makes the screen stage send the same value so the two never force a model reload, adds an
optional per-account `think` switch, warns when a reply shows the prompt reached the window, and
re-scores the local models.

## Current State

- `rust/fontaine-core/src/providers/ollama.rs` connects an `OllamaConfig` and serves
  `native_completion(model).erase()` — rig's native `/api/chat` wire (`rig_core::providers::ollama::Chat`),
  the only Ollama route that reads model `options`. No request carries any provider option:
  `rg provider_option rust/` finds nothing, and `num_ctx` appears nowhere.
- rig 0.45 already models the option: `rig_core::providers::ollama::extension::OllamaOptions::num_ctx(u32)`,
  attached per request with `CompletionRequest::provider_option(..)`. rig maps
  `Reasoning::Off` on a request to Ollama's top-level `think: false` (`providers/ollama/chat.rs:405`).
- Requests are built in several places, all from the `DynModel<Completion>` that
  `AccountClient::completion` returns: the main agent (`fontaine-core/src/agent.rs:1215`), child
  agents (`child_runner.rs:273`), compaction (`session.rs:210-295`), the guardian
  (`fontaine-guardian/src/guardian.rs`), and the eval runner (`fontaine-evals/src/live.rs`, via
  the same `Switcher`). The provider's `completion` is therefore the one choke point.
- `DynModel` is a sealed erasure (`ErasedModel` is `pub(crate)` in rig), so a `DynModel` cannot be
  wrapped; but `Model::new(wire, transport)` is public, `Model.wire`/`Model.transport` are public
  fields, and rig's HTTP transport serves any `W: Wire<Payload = Encoded, Frame = WireFrame>`
  (`rig-core/src/driver/http_transport.rs:17`). A wrapper wire around `ollama::Chat` is therefore
  possible without touching rig.
- The screen stage builds its own `/api/chat` body (`fontaine-guardian/src/screen.rs:333-346`)
  with `think: false` and `options: { num_predict: 1, temperature: 0 }` — no `num_ctx`.
  `build_screen` (`fontaine-core/src/agent.rs:155`) reads only the account's `base_url`. Ollama
  reloads a model whenever a request's `num_ctx` differs from the loaded one, so a screen and a
  guardian on the same model with different values would reload it on every call.
- `config::Account` (`fontaine-core/src/config.rs:312`) has `kind`, `api_key_env`, `base_url`,
  `models`, `wires`. Kind-restricted keys follow the `wires` pattern (`config.rs:729`,
  `Provider::accepts_wires`). Accounts are user-scope / `--settings` only.
- Usage reaches Fontaine at two sites: the main/child agent loop
  (`fontaine-core/src/turn.rs:236`, `MultiTurnStreamItem::CompletionCall`) and the guardian's
  adjudication (`fontaine-core/src/permissions.rs:1159`, `add_guardian_usage`). Ollama reports
  `prompt_eval_count` as `usage.input_tokens` (`rig-core/src/providers/ollama/streaming.rs:149`).
- `ModelHandle` (`fontaine-core/src/models.rs:106`) carries `reference`, `kind`, `completion`.

## Design

### Account keys

```rust
pub struct Account {
    // existing fields …
    /// `ollama` only: the context window requested with every call (`options.num_ctx`).
    pub context_tokens: Option<u32>,
    /// `ollama` only: `false` asks a thinking model to answer without reasoning.
    pub think: Option<bool>,
}
```

- `context_tokens`: unset means `DEFAULT_OLLAMA_CONTEXT_TOKENS = 16384`. It is always sent; there
  is no "leave it to the daemon" value, because the daemon's default is exactly the bug.
  Validation: refused (`not used by kind = "…"`) on any other kind; must be ≥ 2048 (smaller
  cannot hold the guardian prompt, so it is a mistake, not a choice).
- `think`: unset sends nothing (the model's default). `false` sets `Reasoning::Off` on the
  request; `true` sets nothing beyond the default either — it is only meaningful as `false`, but
  accepting `true` keeps the key a plain boolean. Refused on other kinds.
- Both keys are validated through one new `Provider::accepts_model_options()` (default `false`,
  `true` for `Ollama`), mirroring `accepts_wires`.

### The wrapper wire (`fontaine-core/src/providers/ollama.rs`)

```rust
#[derive(Clone)]
struct Tuned {
    chat: rig_core::providers::ollama::Chat,
    context_tokens: u32,
    think: Option<bool>,
}

impl Wire for Tuned {
    type Op = Completion;
    type Payload = <Chat as Wire>::Payload;   // all associated types delegate
    …
    fn describe(&self) -> Descriptor<'_> { self.chat.describe() }
    fn encode(&self, request: CompletionRequest, mode: Mode) -> Result<Encoded, EncodeError> {
        self.chat.encode(self.tune(request), mode)
    }
    fn decoder<'id>(&self) -> Self::Decoder<'id> { self.chat.decoder() }
    fn reassembler(&self) -> Self::Reassembler { self.chat.reassembler() }
}
```

`tune` sets `OllamaOptions::default().num_ctx(context_tokens)` and sets `Reasoning::Off` when `think ==
Some(false)` and the request names no reasoning. `Connected::completion` builds
`Model::new(Tuned { chat: native.wire, .. }, native.transport).erase()`. Every request path above
inherits it with no call-site change. `connect` reads the two keys from `ctx.account`.

### The screen

`ScreenSettings` gains `context_tokens: u32`; `build_screen` fills it from the screen account
(`context_tokens` or the default — one shared `Account::ollama_context_tokens()` helper so both
paths resolve the same number), and `Screen::ask` adds `"num_ctx"` to its `options`. The screen
already sends `think: false`; unchanged.

### Window warning

`ModelHandle` gains `context_tokens: Option<u32>` (set for `ollama` accounts by
`models.rs`, `None` otherwise; `ModelHandle::new` keeps its signature and a
`with_context_tokens` builder sets it). The window travels with the model that produced the usage:
`Guardian` gets a plain `Option<u32>` field and accessor (keeping `fontaine-guardian`
dependency-light), filled in `build_guardian` from the handle, so a `/guardian-model` swap
(`set_guardian`) replaces it with the guardian. `TurnParams`, `Drive` and the child runner's
`ChildModel` each carry the window of the model they drive. A shared helper:

```rust
/// Whether a reply's prompt filled the window: Ollama never reports more than it kept.
pub fn filled_window(input_tokens: u64, context_tokens: u32) -> bool {
    input_tokens * 10 >= u64::from(context_tokens) * 9
}
```

At both usage sites, when it returns true, the session warns once per model through
`Approver::warn`: "`local/qwen3:4b`'s prompt used N of its `context_tokens = M`; Ollama drops the
start of a longer prompt. Raise `context_tokens` on account `local`." The once-per-model memo lives
in `SessionState` (a `Mutex<HashSet<String>>`). The guardian site reads the window from `state.guardian()`; the turn site reads it from `Drive`.
Compaction (`session.rs`) sends the whole history, so its usage is checked against the main
model's window too. A warning never changes a verdict.

The exact 90% threshold is checked against live Ollama behavior (Testing Strategy, live step 3) and
adjusted there if the daemon reports a truncated prompt below it.

## Implementation Steps

1. **Config.** Add `context_tokens` and `think` to `Account`, the
   `accepts_model_options` provider hook, validation in `config.rs` next to the `wires` check,
   `DEFAULT_OLLAMA_CONTEXT_TOKENS`, and `Account::ollama_context_tokens()`.
2. **Wrapper wire.** `Tuned` in `providers/ollama.rs`; `connect` stores the account's values in
   `Connected`; `completion` returns the wrapped model.
3. **Screen.** `ScreenSettings::context_tokens`, filled in `build_screen` and in
   `ask` sends it. `fontaine-evals/src/live.rs` needs no change: `add_screen` goes through
   `build_screen`.
4. **Warning.** `ModelHandle::context_tokens` + builder, set in `models.rs` (main and guardian
   resolution), `filled_window`, the window on `Guardian`, `TurnParams`, `Drive` and
   `ChildModel`, the memo on `SessionState`, and the call sites (`turn.rs`, `permissions.rs`,
   compaction in `session.rs`).
5. **Docs** (see Documentation).
6. **Live verification and re-score** (Testing Strategy, live steps), recording numbers in the
   PR and updating `evals/scorecards/qwen3-4b.json`.

## Files to Modify

- `rust/fontaine-core/src/config.rs` — account keys, validation, default, helper.
- `rust/fontaine-core/src/providers/mod.rs` — `accepts_model_options`.
- `rust/fontaine-core/src/providers/ollama.rs` — `Tuned` wire, `connect`/`completion`.
- `rust/fontaine-guardian/src/screen.rs` — `ScreenSettings::context_tokens`, request body, test
  helper (`:461`).
- `rust/fontaine-guardian/src/guardian.rs` — window field and accessor on `Guardian`.
- `rust/fontaine-guardian/tests/cascade.rs`, `rust/fontaine-core/tests/permissions_screen.rs` —
  `ScreenSettings` literals.
- `rust/fontaine-core/src/agent.rs` — `build_screen`, `build_guardian` fills the guardian window.
- `rust/fontaine-core/src/models.rs` — `ModelHandle::context_tokens`.
- `rust/fontaine-core/src/gate.rs` — warning memo on `SessionState`.
- `rust/fontaine-core/src/turn.rs` — `TurnParams`/`Drive` window, warning site.
- `rust/fontaine-core/src/child_runner.rs` — `ChildModel` window, `Drive` construction.
- `rust/fontaine-core/src/permissions.rs`, `rust/fontaine-core/src/session.rs` — warning call
  sites (guardian, compaction).
- `TurnParams` constructors: `rust/fontaine-tui/src/lib.rs`, `rust/fontaine/src/headless.rs`,
  `rust/fontaine-core/tests/billing.rs`, `audit_telemetry.rs`, `output_styles.rs` (`Drive`
  literals in `billing.rs` and `audit_telemetry.rs` too).
- `docs/configuration.md`, `docs/guardian.md`, `docs/evals.md`.
- `CHANGELOG.md` (via the PR step).
- `evals/scorecards/qwen3-4b.json` (re-scored).

## Trade-offs

- **Wrapper wire vs. per-call-site option.** Five request-building sites exist and more will come;
  a missed one silently reintroduces the bug. The provider is the one place every Ollama request
  passes through.
- **Always send vs. send only when configured.** Sending a default overrides a user's
  `OLLAMA_CONTEXT_LENGTH` or Modelfile `num_ctx` (a user who raised it to 32768 gets 16384 until
  they set `context_tokens`). Not sending leaves every default install broken. The override is
  documented; the warning tells the user when their window is too small.
- **16384 default.** Holds the ~4k-token rules plus the rest of the guardian prompt and a sizeable
  request; larger costs GPU memory (KV cache grows linearly) and can push a model to partial CPU
  offload on small GPUs. Measured live on a 6 GB card (Testing Strategy).
- **Query `/api/show` for the model's own context length** was rejected: an extra call per model,
  and the trained maximum (often 32k–128k) is not what fits in memory.
- **`think` as an account key, not a guardian key.** It is a property of how a model on that
  daemon is called, and the main agent may want it too; the screen already hard-codes it off.

## Decisions

- 2026-10-24: The window warning stays in scope although issue #96 does not ask for it — the user asked for every change proposed in the issue triage, the warning included.

## Documentation

- `docs/configuration.md`: `context_tokens` and `think` rows in the `[accounts.<name>]` table;
  the Ollama example gains `context_tokens`; a short paragraph on the GPU-memory trade-off, the
  override of the daemon's own setting, and the window warning.
- `docs/guardian.md`: in the local-guardian / screen guidance, that the guardian prompt needs
  more than Ollama's default window and the screen uses the account's `context_tokens`.
- `docs/configuration.md`: the `[guardian.screen]` `model` row (~line 368) also says the account's
  `context_tokens` is used.
- `docs/evals.md`: scoring a local model relies on the account's window; note the re-score, and
  rewrite the stale truncation sentence for `local/qwen3:4b` (`:130-132`).
- `CHANGELOG.md`: via the PR step.

## Testing Strategy

Unit / integration (CI):

- `config.rs`: `context_tokens`/`think` accepted on `ollama`, refused on another kind with the
  `accounts.<name>.<key>` path; `context_tokens < 2048` refused; default resolves to 16384.
- `providers/ollama.rs` with `wiremock` standing in for the daemon's `/api/chat`: a request
  carries `options.num_ctx` = default when unset and = the configured value when set; `think:
  false` is present only when `think = false`. This is the regression test pinning the issue's bug.
- `screen.rs` (existing wiremock test at `:584`): the body carries `options.num_ctx`.
- `filled_window` boundaries; a session-level test (existing `rig_core::test_utils` /
  `agent_pipeline.rs` style) that a usage report at the window warns exactly once per model and
  below it never warns.

Live, against the local Ollama daemon (manual; the user approved testing every proposed change
against it). These catch what a fake cannot: the real daemon's truncation, reload and memory
behavior.

1. `ollama ps` during `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian
   live:local/qwen3:4b` shows `CONTEXT 16384` (and the configured value after setting
   `context_tokens`).
2. With a cascade (`--screen live:local/qwen3:4b-instruct-2507-q4_K_M`, guardian on the same
   daemon) and with screen and guardian on the *same* model: no reload between calls (`ollama ps`
   UNTIL/load stays stable; per-call latency has no reload spikes).
3. Truncation: send a prompt longer than a small `context_tokens` (e.g. 2048) and record the
   reported `prompt_eval_count`; confirm `filled_window` fires and the warning appears once. Adjust
   the threshold if the daemon reports less.
4. Memory: `ollama ps` SIZE/PROCESSOR for `qwen3:4b` and `qwen3:8b` at 16384 on the 6 GB GTX
   1060; record whether either spills to CPU, for the docs' trade-off paragraph.
5. `think = false` on `qwen3:4b`: full scorecard with and without it; record accuracy, fail-closed
   rate and timeout rate. Measurement only — the default stays unset regardless.
6. Re-score `qwen3:4b` at the default window (full live bench, default timeout) and replace
   `evals/scorecards/qwen3-4b.json`; record before/after in the PR.

## Open Questions
