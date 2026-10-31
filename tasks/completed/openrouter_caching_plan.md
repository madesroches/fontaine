# OpenRouter Prompt Caching and Cache Accounting Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/331

## Overview

Prompt caching (#13) is landed for direct Anthropic accounts, but an OpenRouter account today is
`kind = "openai-compatible"` on rig's generic gateway dialect, which sends no `cache_control`, so a Claude model behind OpenRouter is billed full input price every call. This
plan adds a first-class `openrouter` kind on rig's `OPENROUTER` dialect with prompt caching, adds
cache read/write tokens to guardian (and the other non-main) usage so the effect is visible, and
adds a startup notice when a pinned guardian model's minimum cacheable length exceeds the guardian
prefix. Measuring opencode-go's Messages route (issue item 4) needs a live Go account and is left as
a manual step; its default does not change here.

## Current State

- `fontaine-core/src/providers/mod.rs`: the `Provider` trait and `PROVIDERS` registry; `PromptCaching::of`
  (enabled from `prompt_caching` or the kind's `default_prompt_caching()`, TTL from `cache_ttl`) and
  `PromptCaching::apply`, which is typed to `Model<Messages>` only. `CacheScope::{Conversation, StaticPrefix}`
  picks the main-agent vs guardian marker.
- `providers/openai_compatible.rs`: `Dialect::gateway("openai-compatible", "", "")`, optional bearer
  key, required `base_url`; wraps `providers/openai.rs::Connected`, whose `completion` just erases the
  client's model. `guardian_completion` defaults to `completion`.
- `providers/ollama.rs` (`Tuned`) is the precedent for a request-rewriting seam: a `Wire` wrapper that
  edits the `CompletionRequest` (including `request.options`) in `encode` and delegates the rest to
  the inner wire. rig's `ErasedModel` is `pub(crate)`, so the issue's "decorating `ErasedModel`" is a
  `Wire` wrapper in practice.
- rig-core 0.45 `OPENROUTER` dialect (`providers/openai/wire/dialects.rs`): base URL
  `https://openrouter.ai/api/v1`, env `OPENROUTER_API_KEY`. `GenerationOptions::cache(CacheRetention)`
  maps (via `openrouter_cache` in `providers/openai/options.rs`) to a top-level
  `cache_control: {"type":"ephemeral"[,"ttl":"1h"]}` for non-automatic upstreams and nothing for
  automatic ones (`openai/`, `deepseek/`, `x-ai/`, `groq/`); `CacheRetention::Long` on an automatic
  upstream is "unsupported". rig's OPENROUTER Chat mapping leaves `cache_key` unrouted, so no
  `prompt_cache_key` is sent.
- `ConnectContext.session_id` (`providers/mod.rs`) is already plumbed from `models::resolve`.
- `ModelHandle` (`models.rs`) builds a `conversation` and a `guardian` completion per client through
  `AccountClient::completion` / `guardian_completion`.
- Accounting: `GuardianUsage` (`fontaine-core/src/gate.rs:205`) holds input/output/cost only;
  `add_guardian_usage` (`gate.rs:606`), `add_goal_usage` and `add_subagent_usage` receive a full rig
  `Usage` with `cached_input_tokens` / `cache_creation_input_tokens` and drop them. Headless
  `result_event` (`fontaine/src/headless.rs:~310`) emits cache fields for `usage.main` only;
  `docs/schemas/headless-v1.schema.json` defines `usage_bucket` and `main_usage_bucket` (the latter
  adds the cache fields).
- `config.rs:353-357` documents `prompt_caching` / `cache_ttl` as `anthropic` and `opencode-go` only;
  `check_account` (~`config.rs:992`) rejects them for kinds where `accepts_prompt_caching()` is false.
- Guardian model resolution and notices: `agent/setup.rs::build_guardian_telling` builds the guardian
  from a `GuardianModel::Ready(handle)` and already reports start-up notices through
  `Approver::notice` / `warn` (`mode_notifier`). The shipped list is
  `fontaine-guardian/data/guardian_models.toml`.
- `docs/configuration.md:~304` documents OpenRouter via `openai-compatible`.

## Design

### 1. `openrouter` kind

New `providers/openrouter.rs`, `pub struct OpenRouter`, registered in `PROVIDERS`:

- `name() = "openrouter"`, `credential() = Required`, `base_url_policy() = Fixed`,
  `default_endpoint() = Some("https://openrouter.ai/api/v1")`, `accepts_prompt_caching() = true`,
  `default_prompt_caching() = true`. `Billing::Catalog` (rig's OPENROUTER dialect prices by upstream).
- `connect`: `OpenAIConfig::with_key(&OPENROUTER, key)` (API key from the credential, as the other
  keyed kinds do), `connect(caused_client(...))`. A `base_url` on the account is honored if set
  (same as `anthropic`/`openai`, which keep `Fixed` for the dialog but accept an override for
  proxies and wiremock tests).
- `list` is a direct `list_models()` call on OpenRouter's own client (`openai::Connected` is `pub(super) struct Connected(pub(super) Client)`, so it is not wrapped), so `/models` and account validation work.
- The completion model is `Model<CacheWire>` where `CacheWire` is a `Wire` wrapper (below) over the
  client's Chat wire: `Connected` builds the model with `client.chat(model)` (rig's
  `OpenAI::completion()` returns an enum of Chat and Responses wires) and wraps the resulting Chat
  wire in `CacheWire`. Because `openai::Connected` is generic over nothing, OpenRouter gets its own
  `Connected` that holds the client, the resolved `PromptCaching`, and the session id.

### 2. The request-options seam: `CacheWire`

A small `Wire` wrapper in `providers/` (shared module, e.g. `providers/cache_wire.rs`), modeled on
`ollama::Tuned`. It carries a `CacheScope` field (the existing enum in `providers/mod.rs`, reused
as-is); the OpenRouter `Connected` builds the conversation completion with `Conversation` scope and
`guardian_completion` with `StaticPrefix` scope. `encode` sets `request.options.cache` when the
request does not already carry it, then delegates to the inner wire; every other `Wire` method
delegates. It is generic over the inner `Wire<Op = Completion>` so another Chat-wire kind can adopt
it later without a change here; the guardian-scope body rewrite (Design 3) additionally needs
`W: Wire<Payload = Encoded>`, since it parses the encoded `Body::Bytes` JSON and rewrites it.

- `CacheRetention`: `Short` for `cache_ttl = "5m"`; for `"1h"`, `Long`, except `Short` when the model
  id starts with `openai/`, `deepseek/`, `x-ai/` or `groq/` (rig rejects `Long` on those automatic
  upstreams). That prefix list is a local const duplicating rig's private automatic-upstream list and
  must track rig. Never `CacheRetention::None`; `options.cache` is left unset when `prompt_caching`
  is off.

### 3. Guardian scope on OpenRouter

The main-agent scope takes rig's top-level automatic `cache_control`. For the guardian, whose last
message is per-call and never reused, a top-level marker lands on that tail (write premium, no
read), the opposite of the Anthropic `StaticPrefix` design. The first implementation step is a spike
that records what rig's OPENROUTER Chat encoding emits for a system message. rig's openai provider
maps `cache_control` only at the top level, so it cannot express a per-block breakpoint. The design:
`CacheWire::encode` (the wrapper owns the encoded body) parses the encoded `Body::Bytes` JSON and adds
`cache_control` to the system message's content block for the guardian scope, sending no top-level
marker. The spike only settles the system-content shape (string vs. block array) and the fallback:

1. The body rewrite is not workable cheaply: the guardian scope sends no marker and the conversation scope still
   caches; the gap is recorded in Decisions and docs. A top-level marker on the guardian tail is not
   an acceptable fallback.

`AccountClient::guardian_completion` for OpenRouter returns the guardian-scoped model;
`ModelHandle` needs no change.

### 4. Guardian (and other) cache accounting

Rename-free: add `cache_read_input_tokens` and `cache_creation_input_tokens` (`u64`) to
`GuardianUsage`; every `add_*_usage` that sums a `Usage` (`add_guardian_usage`, `add_goal_usage`,
`add_subagent_usage`) adds them through one shared helper on `GuardianUsage` (`fn add(&mut self, &Usage)`)
instead of three copies of the same four lines. The helper covers only the token and cost sums;
`agents.debit` and `add_guardian_usage`'s per-agent second application stay at each call site. Headless `result_event` emits the two fields for
`guardian`, `subagents` and `goal`, and `docs/schemas/headless-v1.schema.json` makes
`usage_bucket` carry them (`main_usage_bucket` collapses into `usage_bucket`, since every bucket now
has the fields; safe because `result_event` is the only emitter and neither def sets
`additionalProperties`). The schema is `v1`: adding required integer fields to buckets is an additive
change for consumers, called out in the CHANGELOG.

### 5. Minimum-cacheable-length notice

A pure function `min_cacheable_tokens(kind, model) -> Option<u32>` (in `fontaine-core`, next to the
provider code) applies to Claude ids on the `anthropic` and `openrouter` kinds only and matches against the
table from the issue: 512 for Claude 5.x, 1024 for Sonnet 4.x, 2048 for Haiku 3.5 / Opus 4.7,
4096 for Opus 4.5/4.6 and Haiku 4.5; `None` for unknown or non-Anthropic ids. Normalization: strip an
OpenRouter `anthropic/` prefix and map '.' to '-' (OpenRouter ids are dotted, e.g.
`anthropic/claude-haiku-4.5`; direct ids are dashed), and match both `haiku-3-5` and `3-5-haiku`
orderings. `build_guardian_telling` (which already receives `profile`) emits one `approver.notice`
when the guardian model is ready, its account has prompt caching enabled, and the minimum exceeds a
per-profile prefix-size constant (about 1500 tokens for `minimal`, about 3000 for the full profile,
matching the sizes of `guardian_minimal.md` and `guardian_prompt.md`), saying the minimum, that cache markers on this guardian are a no-op below it, and
that a lower-minimum model is the fix. The notice is emitted only when an approver is present (none when `approver` is `None`), once per
guardian rebuild; a model switch re-emitting it is acceptable. The table is data in one place, so a
new model is one line.
The caching-enabled check needs the guardian account's `PromptCaching`, which `ModelHandle` does not
carry (it has `reference`, account plus model id, and `kind`): look up
`config.accounts[handle.reference.account]` and call `PromptCaching::of` (`pub(crate)`, usable from
`agent/setup.rs`), for kinds where `handle.kind.accepts_prompt_caching()` is true.

## Implementation Steps

1. Spike (Design 3): wiremock a rig OPENROUTER Chat request with `.cache(Short)`
   and record the exact body; settle the system-content shape, the fallback, and the `Long`-on-automatic behavior.
2. `providers/cache_wire.rs`: `CacheWire<W>` with unit tests on the encoded body (no network).
3. `providers/openrouter.rs` + registry line in `providers/mod.rs`; `config.rs` doc comments for
   `prompt_caching` / `cache_ttl` name `openrouter`.
4. `GuardianUsage` cache fields and the shared `add`; `headless.rs` emission; schema update.
5. `min_cacheable_tokens` and the `build_guardian_telling` notice.
6. Docs, CHANGELOG, plan Decisions.

## Files to Modify

- `rust/fontaine-core/src/providers/openrouter.rs` (new), `providers/cache_wire.rs` (new),
  `providers/mod.rs`
- `rust/fontaine-core/src/config.rs` (doc comments), `gate.rs`, `agent/setup.rs`
- `rust/fontaine/src/headless.rs`, `docs/schemas/headless-v1.schema.json`
- `rust/fontaine-core/tests/account_session.rs`, `tests/providers.rs`
- `docs/configuration.md` (also :161-162), `docs/guardian.md` (also :236-241), `docs/headless.md` (also ~:204), `docs/architecture.md`, `docs/telemetry.md` (:129), `README.md` (provider list, :4, :68)
- `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Decisions

- 2026-10-30: AFK: the opencode-go default is not flipped in this change, since it needs a live Go measurement; item 4 stays a manual step after the cache accounting lands.
- 2026-10-30: AFK: `openai-compatible` accounts pointing at OpenRouter are not migrated or detected; they keep working uncached, and the docs point to `kind = "openrouter"`.
- 2026-10-30: AFK: sticky routing (`prompt_cache_key`) is deferred, since rig's OPENROUTER Chat mapping omits `cache_key` and sending it would need a body rewrite; the issue lists it as optional.
- 2026-10-30: AFK: the minimum-length notice applies to Claude ids on `anthropic`/`openrouter` only and is reachable only for pinned guardians with a minimum above the prefix size; the shipped 512-minimum default never triggers it.
- 2026-10-30: AFK: `CacheWire` maps `Long` to `Short` for `openai/`, `deepseek/`, `x-ai/` and `groq/` model ids (a local copy of rig's automatic-upstream list that must track rig), never sets `CacheRetention::None`, and leaves `options.cache` unset when caching is off.
- 2026-10-30: AFK: `openrouter` is a new kind on rig's OPENROUTER dialect rather than a `prompt_caching` key on `openai-compatible`, since only that dialect has the cache mapping and a flag on the generic gateway would send OpenRouter-specific fields to arbitrary endpoints.

## Documentation

- `docs/configuration.md`: replace the OpenRouter-through-`openai-compatible` example with
  `kind = "openrouter"`, document that `prompt_caching` / `cache_ttl` now apply to `openrouter`, the
  and that non-Anthropic upstreams cache automatically.
- `docs/guardian.md`: OpenRouter guardian caching, the minimum-length notice and the table.
- `docs/architecture.md`: lines ~268-270 say other wires cache automatically and cache counts are "of
  the main model"; both go stale. Also check `docs/tui.md` and `docs/evals.md` for provider-kind lists.
- `docs/headless.md`: `usage.guardian` / `subagents` / `goal` cache fields; schema file updated.
- `README.md`: provider list if it names kinds.
- Stale spots to fix: `docs/configuration.md:161-162` ("anthropic and opencode-go only"),
  `docs/guardian.md:236-241` (says caching is on by default for anthropic), `docs/headless.md:~204`
  (says only main has cache fields), `docs/telemetry.md:129` (kinds list), `README.md:4` and `:68`
  (name OpenRouter as an OpenAI-compatible endpoint).

## Testing Strategy

- Unit (`cache_wire.rs`): encoded body has top-level `cache_control` (and `ttl: "1h"` for `Long`)
  when enabled, none when disabled; an automatic upstream (`openai/gpt-...`) with a
  `Long` TTL is mapped to `Short`; guardian scope sends no top-level marker.
- Wiremock integration (`tests/account_session.rs`, helpers `gateway()` / `last_body()`; only the
  enabled and disabled cases, the encoded-body detail is covered by the `cache_wire` unit tests):
  a `kind = "openrouter"` account with `base_url` pointing at the mock sends the marker through the
  real resolve-and-complete path; the same account with `prompt_caching = false` sends none.
- Config (`tests/providers.rs`): `prompt_caching` / `cache_ttl` accepted for `openrouter`, rejected
  for other kinds as before; default is on; `openrouter` is in `names()`. The existing
  `prompt_caching_keys_belong_to_the_kinds_that_accept_them` and `prompt_caching_defaults_per_kind`
  hard-code kind lists and get `openrouter` added (accepted, default on).
- Accounting: `GuardianUsage::add` and `add_guardian_usage` / `add_goal_usage` / `add_subagent_usage`
  carry cache read/write tokens; a headless test like `cache_tokens_reach_the_result_usage`
  (`headless.rs:964`) makes one `result_event` call with distinct per-bucket values and asserts all
  four buckets; the JSON validates against the updated schema
  (existing schema test, if present, otherwise one assertion on the new required fields).
- Notice: `min_cacheable_tokens` one case per tier, one dotted OpenRouter id
  (`anthropic/claude-haiku-4.5`) and an unknown id; `build_guardian_telling` notices once for a
  4096-minimum model with caching on, not for a 512-minimum model, not with caching off, and not
  without an approver.
- Pinned bug: the live observation (zero cache tokens through OpenRouter) is pinned by the wiremock
  test above, which fails on the old `openai-compatible` path.

## Manual Verification

1. Live OpenRouter account with a Claude guardian (`kind = "openrouter"`): run
   `cargo run -p fontaine -- -p "list the files, then read the README"` twice within five minutes.
   Expect non-zero `usage.guardian.cache_read_input_tokens` on the second run and cache tokens in
   the OpenRouter dashboard. Not automated: needs a live key and depends on upstream minimums.
2. opencode-go (issue item 4): with a Go account and a Claude guardian, the same run with
   `prompt_caching = true`; if `usage.guardian.cache_read_input_tokens` is non-zero, flip
   `OpencodeGo::default_prompt_caching()` in a follow-up. Not automated: needs a live subscription.

## Open Questions

None.
