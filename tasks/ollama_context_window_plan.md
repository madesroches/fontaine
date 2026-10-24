# Ollama Context Window Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/96

## Overview

Fontaine never tells Ollama how large a context to load a model with, so the daemon uses its
default (4096 tokens unless the Modelfile or `OLLAMA_CONTEXT_LENGTH` says otherwise). The
guardian's rendered default rules alone are about 4,000 tokens, and Ollama silently truncates an
over-long prompt from the front, so a local guardian judges calls without most of its rules — and
a local main model loses its system prompt the same way. This plan makes every request to an
`ollama` account carry `options.num_ctx` (a new account key, `context_tokens`, default 16384),
makes the screen stage send the same value so a screen and guardian on the same account never force a model reload, adds an
optional per-account `think` switch, sends `truncate: false` so the daemon refuses an over-long
prompt instead of silently cutting it (the guardian then fails closed; the main agent shows the
error), tells compaction the real window for models rig's catalog lists, and re-scores the local
models.

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
- The screen stage builds its own `/api/chat` body (`fontaine-guardian/src/screen.rs:334-346`)
  with `think: false` and `options: { num_predict: 1, temperature: 0 }` — no `num_ctx`.
  `build_screen` (`fontaine-core/src/agent.rs:156`) reads only the account's `base_url`. Ollama
  reloads a model whenever a request's `num_ctx` differs from the loaded one, so a screen and a
  guardian on the same model with different values would reload it on every call.
- `config::Account` (`fontaine-core/src/config.rs:312`) has `kind`, `api_key_env`, `base_url`,
  `models`, `wires`. Kind-restricted keys follow the `wires` pattern (`config.rs:729`,
  `Provider::accepts_wires`). Accounts are user-scope / `--settings` only.
- Measured live (Ollama 0.40.8, `llama3.2:1b`, `num_ctx` 2048, a ~6000-token prompt): by default
  the daemon answers HTTP 200 having kept about half the window (`prompt_eval_count` 1026; at
  `num_ctx` 4096 a 7707-token prompt kept 2050, daemon log `truncating input prompt limit=2050`).
  The same request with top-level `"truncate": false` answers HTTP 400 `{"error":"request (6028
  tokens) exceeds the available context size (2048 tokens), try increasing it"}`. rig's native
  wire sends the `additional_params` keys `truncate` and `shift` at the top level of the body
  (`rig-core/src/providers/ollama/chat.rs:38-48`, `TOP_LEVEL`).
- A guardian provider error is already a fail-closed Deny whose explanation carries the error
  (`fontaine-guardian/src/guardian.rs:259-265`, `FailClosed::Error`); a screen error is
  `ScreenOutcome::Unavailable`, whose reason today names only the HTTP status (`screen.rs:366`).
- Threshold compaction reads the window from `model.spec()?.context_window` and does nothing when
  it is `None` (`fontaine-core/src/session.rs:240-242`); it is the only reader of
  `context_window` in Fontaine, rig and rig-memory. The Ollama wire's spec comes from its
  `facts` (`Chat::with_facts`, `chat.rs:72`; resolved in `rig-core/src/wire.rs:525`), which
  default to rig's catalog — never `num_ctx`. A model the catalog lists (e.g. `gpt-oss:20b`,
  131072) therefore compacts far above a 16384 window; one it does not list never compacts.

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
  Validation: refused (`not used by kind = "…"`) on any other kind; must be ≥ 2048 (a floor
  against typos and nonsense values, not a size that fits the guardian prompt).
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

`tune` sets `num_ctx` by overlaying `OllamaOptions::default().num_ctx(context_tokens)` onto any existing
Ollama options entry (`ProviderOptions::overlay`, `provider_options.rs:326`) so no other field is
dropped, sets `"truncate": false` in the request's `additional_params` (merged into any existing
object; rig sends it at the top level), and sets `Reasoning::Off` when `think ==
Some(false)` and the request names no reasoning. `Connected::completion` builds
`Model::new(Tuned { chat: native.wire, .. }, native.transport).erase()`. Every request path above
inherits it with no call-site change. `connect` reads the two keys from `ctx.account`.

When rig's catalog lists the model (`Catalog::builtin().get(..)`), `completion` binds the wire to
that entry with `context_window = context_tokens` and `id` set to the model as written
(`Chat::with_facts(ModelFacts::new(spec))`), so threshold compaction fires at the window the daemon
actually loads; every other field is the catalog's own, so nothing else changes. A model the
catalog does not list stays unbound: a bare `ModelSpec::new` would also claim no tools and
text-only input, overriding rig's per-vendor naming rules, so such a model keeps today's behavior
(no threshold compaction).

### The screen

`ScreenSettings` gains `context_tokens: u32`; `build_screen` fills it from the screen account
(`context_tokens` or the default — one shared `Account::ollama_context_tokens()` helper so both
paths resolve the same number), and `Screen::ask` adds `"num_ctx"` to its `options` and a
top-level `"truncate": false`. The screen already sends `think: false`; unchanged. A non-success
answer's `Unavailable` reason appends the daemon's `error` text when the body has one, so an
over-long screen prompt names the fix in the existing one-shot screen warning.

### Over-long prompts

With `truncate: false`, a prompt longer than `context_tokens` is a hard error, not a silent cut:

- **Guardian**: the provider error becomes the existing fail-closed Deny (`FailClosed::Error`),
  whose explanation carries the daemon's message ("exceeds the available context size …, try
  increasing it"). No new code; the error text explains the Deny and never changes it.
- **Screen**: `Unavailable`, so the call goes on to the guardian as for any screen failure.
- **Main agent / child / compaction call**: rig's provider error reaches the user as any other
  provider error does, naming the daemon's limit.

This replaces a usage-based warning: Ollama cuts an over-long prompt to about half the window
(Current State), so a threshold on reported `input_tokens` cannot tell a truncated prompt from a
short one, and the refusal is exact where the warning would guess.

## Implementation Steps

1. **Config.** Add `context_tokens` and `think` to `Account`, the
   `accepts_model_options` provider hook, validation in `config.rs` next to the `wires` check,
   `DEFAULT_OLLAMA_CONTEXT_TOKENS`, and `Account::ollama_context_tokens()`.
2. **Wrapper wire.** `Tuned` in `providers/ollama.rs` (`num_ctx`, `truncate: false`, `think`);
   `connect` stores the account's values in `Connected`; `completion` binds the catalog entry's
   facts with `context_tokens` as the window when the catalog lists the model, and returns the
   wrapped model.
3. **Screen.** `ScreenSettings::context_tokens`, filled in `build_screen`; `ask` sends it and
   `truncate: false`, and puts the daemon's `error` text in a non-success reason. For the screen,
   `fontaine-evals/src/live.rs` needs no change: `add_screen` goes through `build_screen`.
4. **Docs** (see Documentation).
5. **Live verification and re-score** (Testing Strategy, live steps), recording numbers in the
   PR and updating `evals/scorecards/qwen3-4b.json`.

## Files to Modify

- `rust/fontaine-core/src/config.rs` — account keys, validation, default, helper.
- `rust/fontaine-core/src/providers/mod.rs` — `accepts_model_options`.
- `rust/fontaine-core/src/providers/ollama.rs` — `Tuned` wire, `connect`/`completion`, catalog
  facts with the configured window.
- `rust/fontaine-guardian/src/screen.rs` — `ScreenSettings::context_tokens`, request body
  (`num_ctx`, `truncate`), daemon error text in the non-success reason, test helper (`:461`).
- `rust/fontaine-guardian/tests/cascade.rs`, `rust/fontaine-core/tests/permissions_screen.rs` —
  `ScreenSettings` literals.
- `rust/fontaine-core/src/agent.rs` — `build_screen`.
- `rust/fontaine-core/tests/models.rs` — `num_ctx` / `truncate` / `think` regression assertions,
  spec window.
- `docs/configuration.md`, `docs/guardian.md`, `docs/evals.md`, `README.md`.
- `CHANGELOG.md` (via the PR step).
- `evals/scorecards/qwen3-4b.json` (re-scored).

## Trade-offs

- **Wrapper wire vs. per-call-site option.** Five request-building sites exist and more will come;
  a missed one silently reintroduces the bug. The provider is the one place every Ollama request
  passes through.
- **Always send vs. send only when configured.** Sending a default overrides a user's
  `OLLAMA_CONTEXT_LENGTH` or Modelfile `num_ctx` (a user who raised it to 32768 gets 16384 until
  they set `context_tokens`). Not sending leaves every default install broken. The override is
  documented; a prompt that no longer fits fails with the daemon's message naming the fix.
- **`truncate: false` vs. a usage-based warning.** The refusal is exact and needs no plumbing; a
  warning on reported `input_tokens` cannot see a truncated prompt, because the daemon keeps only
  about half the window. The cost: a long local session, or one on a model the catalog does not
  list (no threshold compaction), stops with an error at the window instead of carrying on with
  its start cut off — the user raises `context_tokens` or starts over. A guardian or main model
  silently missing its rules or system prompt is the worse failure.
- **16384 default.** Holds the ~4k-token rules plus the rest of the guardian prompt and a sizeable
  request; larger costs GPU memory (KV cache grows linearly) and can push a model to partial CPU
  offload on small GPUs. Measured live on a 6 GB card (Testing Strategy).
- **Query `/api/show` for the model's own context length** was rejected: an extra call per model,
  and the trained maximum (often 32k–128k) is not what fits in memory.
- **`think` as an account key, not a guardian key.** It is a property of how a model on that
  daemon is called, and the main agent may want it too; the screen already hard-codes it off.

## Decisions

- 2026-10-24: Making truncation visible stays in scope although issue #96 does not ask for it — the user asked for every change proposed in the issue triage; it is delivered by `truncate: false` rather than the proposed usage-based warning.
- AFK: 2026-10-24: Send `truncate: false` on every Ollama request and in the screen body, and drop the usage-based window warning with all its plumbing (handle/Guardian window, ModelRef on TurnParams/Drive/ChildModel, SessionState memo, `filled_window`, the `live.rs:117` setter) — live, the daemon cuts an over-long prompt to ~half the window, so no usage threshold can see it, while `truncate: false` turns it into an HTTP 400 that the guardian already fails closed on and the main agent shows as a provider error.
- AFK: 2026-10-24: Item A (90% `filled_window` threshold never fires) — declined as obsolete: the warning and its threshold are removed in favor of `truncate: false`.
- AFK: 2026-10-24: Item B (guardian usage summed across extractor retries) — declined as obsolete: no warning reads guardian usage any more; `UsageTally` stays as it is for billing.
- AFK: 2026-10-24: Item C (compaction ignores `context_tokens`) — the Ollama wire binds rig's catalog entry with `context_window = context_tokens` when the catalog lists the model, so threshold compaction fires at the loaded window rather than hard-erroring under `truncate: false`; unlisted models stay unbound (no threshold compaction, as today), because a bare `ModelSpec::new` would also override tools and image facts.

## Documentation

- `docs/configuration.md`: `context_tokens` and `think` rows in the `[accounts.<name>]` table;
  the Ollama example gains `context_tokens`; a short paragraph on the GPU-memory trade-off, the
  override of the daemon's own setting, that a prompt longer than the window is refused rather than
  truncated (the guardian denies, the main agent shows the daemon's error; raise
  `context_tokens`), and that compaction uses the window for models rig's catalog lists; the `[guardian.screen]` `model`
  row (~line 368) also says the account's `context_tokens` is used, and that equal values are
  needed to avoid reloads.
- `docs/guardian.md`: in "## The screen stage" (~:136-210) and the local-model bullet of "## Choosing the guardian model" (~:236-238), that the guardian prompt needs
  more than Ollama's default window and the screen uses the account's `context_tokens`; a screen
  and guardian avoid reloads only on the same account or equal `context_tokens`.
- `README.md`: the "Quickstart: a local agent with Ollama" example (`:43-55`) mentions
  `context_tokens` and links to the configuration row.
- `docs/evals.md`: scoring a local model relies on the account's window; note the re-score, and
  rewrite the stale truncation sentence for `local/qwen3:4b` (`:130-132`), and update the Results
  intro (`:123-127`), the score rows (`:144`, `:162`) and the prose at `:153` from the re-scored numbers.
- `CHANGELOG.md`: via the PR step.

## Testing Strategy

Unit / integration (CI):

- `config.rs`: `context_tokens`/`think` accepted on `ollama`, refused on another kind with the
  `accounts.<name>.<key>` path; `context_tokens < 2048` refused; default resolves to 16384.
- `rust/fontaine-core/tests/models.rs`, next to `ollama_uses_the_native_chat_route_without_a_key`
  (`:137`, which already mocks `/api/chat` and reads `server.received_requests()`): a request
  carries `options.num_ctx` = default when unset and = the configured value when set, and
  top-level `truncate: false` always; `think: false` is present only when `think = false`. This
  is the regression test pinning the issue's bug. Also: an Ollama model the catalog lists reports
  `spec().context_window == Some(context_tokens)`; one it does not list reports `spec() == None`.
- `screen.rs` (existing wiremock test at `:584`): the body carries `options.num_ctx` and
  `truncate: false`; a 400 with an `error` body gives `Unavailable` whose reason contains that text.

Live, against the local Ollama daemon (manual; the user approved testing every proposed change
against it). These catch what a fake cannot: the real daemon's truncation, reload and memory
behavior.

1. `ollama ps` during `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian
   live:local/qwen3:4b` shows `CONTEXT 16384` (and the configured value after setting
   `context_tokens`).
2. With a cascade (`cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian live:local/qwen3:4b --screen live:local/qwen3:4b-instruct-2507-q4_K_M --stop-threshold <t>`, guardian on the same
   daemon) and with screen and guardian on the *same* model: no reload between calls (`ollama ps`
   UNTIL/load stays stable; per-call latency has no reload spikes).
3. Over-long prompt, through a real session (`cargo run -p fontaine -- -p …`), not the eval
   runner: with the guardian on an `ollama` account at `context_tokens = 2048`, a guarded call is
   denied fail-closed and the explanation carries the daemon's "exceeds the available context
   size" message; with the main model on such an account, the run stops with that provider error
   shown. Confirm no request is answered 200 with a truncated prompt (daemon log has no
   `truncating input prompt`).
4. Memory: `ollama ps` SIZE/PROCESSOR for `qwen3:4b` and `qwen3:8b` at 16384 on the 6 GB GTX
   1060; record whether either spills to CPU, for the docs' trade-off paragraph.
5. `think = false` on `qwen3:4b`: full scorecard with and without it; record accuracy, fail-closed
   rate and timeout rate. Measurement only — the default stays unset regardless.
6. Re-score `qwen3:4b` at the default window (full live bench, default timeout) and replace
   `evals/scorecards/qwen3-4b.json`; record before/after in the PR.

## Open Questions

None.
