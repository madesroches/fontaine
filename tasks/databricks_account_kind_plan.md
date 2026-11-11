# Databricks Account Kind Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/166

## Overview

Add a `databricks` account kind that talks to Databricks Unity Gateway (`https://<workspace>/ai-gateway/...`,
model ids `system.ai.<model>`) with a Databricks token sent as `Authorization: Bearer`. Like
`opencode-go`, one account serves several wires and picks one per model: Claude on the Anthropic
Messages route (with prompt caching), GPT on the OpenAI Responses route, Gemini on the native Gemini
route, and every other model on the provider-neutral MLflow Chat Completions route. Models are
listed from the Unity Catalog model-services API. Today a Databricks user has to choose between an
`anthropic` account with `auth = "bearer"` (Claude only, and broken on `system.ai.*` names, see
below) and an OpenAI-style Chat account (no `cache_control`, so no Claude prompt caching, and GPT
models that refuse tools over Chat).

## Current State

- Kinds are `Provider` impls registered in `PROVIDERS`
  (`rust/fontaine-core/src/providers/mod.rs:425`). The trait covers credential policy, base-URL
  policy, billing, `accepts_wires`, `accepts_prompt_caching` / `default_prompt_caching`, `validate`,
  and `connect`, which returns an `AccountClient` (`completion`, `guardian_completion`, `list`,
  `thinking_off`, `catalog_vendor`, ...).
- `opencode-go` (`rust/fontaine-core/src/providers/opencode_go.rs`) is the multi-wire precedent:
  `wire(model, overrides)` (line 165) routes by id prefix with the `wires` table taking priority, and
  `GoClient::completion_in` (line 210) dispatches to one of four pre-built rig clients (Chat and
  Responses through `OpenAIConfig` with a gateway `Dialect`, Messages through `AnthropicConfig` with
  a `compatible(...)` Messages dialect, Gemini through `GeminiConfig`), all sharing one
  `Decorated` HTTP client that places the credential per wire (`AuthPlacement::Bearer`,
  `Header(x-api-key)`, `GoogleKey`). Prompt caching is applied only to the Messages wire through
  `PromptCaching::apply` (`mod.rs:342`).
- `anthropic` (`providers/anthropic.rs`) with `auth = "bearer"` sets
  `config.dialect.quirks.auth = Auth::OptionalBearer`; this is the documented Databricks path today
  (`docs/configuration.md:330`, which still points at the older `/serving-endpoints/anthropic` path
  and a `databricks-claude-*` name).
- `ModelHandle::turn_max_tokens` (`rust/fontaine-core/src/models.rs:327`) sends no `max_tokens`
  for the `anthropic` kind when the model has no catalog limit. rig's Anthropic dialect then refuses
  the request.
- Model facts for ids the catalog does not list come from `derived_facts::unlisted_facts` /
  `derive` (`providers/derived_facts.rs:71`), which matches on the id's version-stripped shape
  within one catalog vendor.
- Config: `Account` has `kind`, `base_url`, `api_key_env`, `models`, `wires` (`Wire` enum
  `chat | responses | messages | gemini`), `prompt_caching`, `cache_ttl`, `auth`
  (`rust/fontaine-core/src/config.rs`). Key availability per kind is gated by the `accepts_*` trait
  methods.
- Listing helpers: `gateway::ModelsFetcher` (`providers/gateway.rs:19`) shows the pattern for a
  plain reqwest GET with bearer auth, timeout and error chaining outside rig.

### Live findings (Unity Gateway workspace, 2026-11-11, through stock Fontaine kinds)

| Probe | Result |
|---|---|
| `anthropic` kind, bearer, `.../ai-gateway/anthropic`, `system.ai.claude-haiku-5-5` | Fails before sending: ``RequestError: `max_tokens` must be set for Anthropic`` (no catalog facts for the `system.ai.` id). With `[model] max_output_tokens` set, the turn succeeds. |
| Same, with tools and rig's default `ToolInputStreaming::Eager` (`eager_input_streaming: true` on every tool) | **Accepted**. The model emitted a tool call. The rejection that `ug`'s notes report does not reproduce for Claude models on this route. |
| Same, prompt caching (Fontaine's default breakpoints) | Works: `cache_read_input_tokens` / `cache_creation_input_tokens` reported (about 11k read on the second model call). |
| Same, `cache_ttl = "1h"` | Accepted (no 400), cache reads reported. |
| `/ai-gateway/anthropic/v1/models` | Lists the `system.ai.claude-*` models, plus `anthropic-aigw-<id>-system.ai.<model>` entries for non-Claude models that **404 when called**. Not a usable listing source. |
| Chat kind at `.../ai-gateway/mlflow/v1`, `system.ai.glm-5-3` with tools | Works. Chat's `/models` returns 400 ("unmanaged api request"). |
| Same Chat route, `system.ai.claude-haiku-5-5` | Works, with no cache reads (no markers on Chat). |
| Same Chat route, `system.ai.gpt-5-6-luna` with tools | **400**: "Function tools with reasoning_effort are not supported ... use /v1/responses". Confirms that GPT must go to Responses. |
| `openai` kind (Responses) at `.../ai-gateway/openai/v1` and `.../ai-gateway/codex/v1`, `system.ai.gpt-5-6-luna` | Both work and both report cache creation tokens. `/openai/v1/models` returns 400; `/codex/v1/models` returns an empty list. |
| `gemini` kind at `.../ai-gateway/gemini` | 401 "Credential was not sent or was of an unsupported type": the gateway does not take `x-goog-api-key`, it wants Bearer. |
| Unity Catalog model-services listing | Not probed live (direct HTTP with the session token was out of scope for the probe harness). Shape taken from the `databricks/unity-gateway` source: `{"model_services": [{"name": "model-services/system.ai.<id>", "supported_api_types": [...]}], "next_page_token": ...}`, with `parent=schemas/system.ai` and `page_size` ≤ 100 required. |

## Design

### New provider: `providers/databricks.rs`

```
Databricks (Provider)
  name               "databricks"
  credential         Required            (api_key_env or /login-stored key; OAuth later)
  base_url_policy    Required            (the workspace origin, e.g. https://<workspace>)
  default_endpoint   None
  billing            Unpriced            (billed in DBUs, not catalog token prices)
  accepts_wires      true
  accepts_prompt_caching  true, default_prompt_caching true
  accepts_auth       false               (always Bearer)
  validate           refuse a base_url with a path (".../ai-gateway", "/serving-endpoints"),
                     naming the key and the expected form
  connect            -> DatabricksClient
```

`connect` trims the trailing `/` from the base URL and builds one `Decorated` client
(`Decoration { user_agent, headers: [], auth: Some((static key source, AuthPlacement::Bearer)) }`)
shared by all four rig clients. **Every** wire uses Bearer, Gemini included (the live 401). The
kind-name constants:

- `DBX_DIALECT: Dialect = Dialect::gateway("databricks", "", "")` for Chat and Responses
  (the same gateway dialect `opencode-go` and `openai-compatible` use, which sends no `store`,
  `previous_response_id`, `background` or strict tool schemas; the live Chat and Responses probes
  passed with it).
- `DBX_MESSAGES: MessagesDialect = compatible("databricks", "", "", None)`, the gateway Messages
  dialect, as on `opencode-go`. It supplies a default `max_tokens` and sends no strict tool schemas.
  It does not send Anthropic-only extras such as the `thinking-binding-controls` beta, whose
  pass-through is unverified (#169).

Roots:

| Wire | rig client base |
|---|---|
| Messages | `{origin}/ai-gateway/anthropic` |
| Responses | `{origin}/ai-gateway/openai/v1` (the documented route; `codex/v1` works too) |
| Gemini | `{origin}/ai-gateway/gemini` |
| Chat | `{origin}/ai-gateway/mlflow/v1` |

The static key goes through `TokenSource` if it accepts a fixed credential. Otherwise use a minimal
fixed-key auth source next to it in `token_source.rs`, so the `Decorated` placement path stays the
one used everywhere.

### Routing: `databricks::wire(model, overrides) -> Wire`

1. `overrides` (`wires`) win.
2. Strip a leading `system.ai.` (and, for compatibility with existing configs, a leading
   `databricks-`) to get the base id.
3. Base id `claude-*` → Messages. `gpt-*` → Responses. `gemini-*` → Gemini.
4. Anything else → Chat. That covers open models (GLM, Kimi, DeepSeek, Llama, Qwen, Gemma, GPT-OSS,
   which is `gpt-oss-*`: see below) and custom `catalog.schema.name` model services. The gateway
   translates the unified route to the model's native format, and custom services can split traffic
   across providers, so their name says nothing about the wire.

`gpt-oss-*` is an open model served on Chat, so the GPT rule matches `gpt-` **and not** `gpt-oss-`.
Routing is name-based only, matching `ug`. `supported_api_types` is advisory, so it never chooses a
route.

The `opencode-go` routing and dispatch are not merged with this one: the two tables differ in every
row, and the dispatch is a four-arm `match`. What both kinds share is factored out (see
"Shared multi-wire client" below).

### Shared multi-wire client

`GoClient` and the new `DatabricksClient` have the same shape: four rig clients, a `wires` table, a
`PromptCaching`, and a `completion_in(model, scope)` that binds facts and applies caching to the
Messages wire only. Extract that into `providers/multi_wire.rs`:

```rust
pub(crate) struct WireClients { pub chat: OpenAI, pub responses: OpenAI, pub messages: Anthropic,
                                pub gemini: Gemini, pub caching: PromptCaching }
impl WireClients {
    /// `model` on `wire`, with `facts` bound and the account's cache breakpoints on Messages.
    pub(crate) fn completion(&self, wire: Wire, model: &str, facts: Option<ModelFacts>,
                             scope: CacheScope) -> DynModel<Completion>;
}
```

`GoClient` keeps its own `wire()`, facts vendor, listing and `thinking_off`, and calls
`WireClients::completion`. Behaviour stays the same, and the existing `opencode-go` tests guard the
move.

### Model facts

For `system.ai.<base>`, the facts come from the base id on the family's catalog vendor:

| Family | Catalog vendor |
|---|---|
| `claude-*` | `anthropic` |
| `gpt-*` | `openai` |
| `gemini-*` | `google` (whatever `vendor()` name rig's catalog uses for Gemini) |
| others | none |

Lookup: the exact `Catalog::builtin().get(vendor, base)` entry, else `derive(...)` on the same
vendor (shape matching also bridges `gpt-5-6-luna` and `gpt-5.6-luna`). The resulting spec is
re-labelled to the full requested id with pricing stripped, as `derive` already does. Make that a
small helper in `derived_facts.rs`: `facts_as(vendor, catalog_id, served_id) -> Option<ModelFacts>`,
which tries the exact entry and then `derive`. That gives Claude a real `max_output_tokens` (fixing
the live `max_tokens` failure) and the model's reasoning/effort facts.

Models with no facts fall into the existing `turn_max_tokens` fallback: `databricks` is not in the
`ollama | anthropic | claude-subscription` list, so `FALLBACK_MAX_OUTPUT_TOKENS` (32k) is sent. The
GLM output cap that `ug` pins (25k on the Chat route) is lower than that, so a GLM request without a
user cap can be rejected. Add one entry, in a per-family table in `databricks.rs`
(`OUTPUT_CAPS: &[("glm-", 25_000)]`), applied as the model's `max_output_tokens` when its facts carry
none. More families are added there as they are found. Users can still lower the cap with
`[model] max_output_tokens`.

`catalog_vendor()` returns `None`. Pricing is never looked up for this kind (`Billing::Unpriced`).

### Prompt caching

`accepts_prompt_caching = true`, `default_prompt_caching = true`, applied by
`WireClients::completion` to the Messages wire only. `prompt_caching` / `cache_ttl` therefore affect
only Claude models, and GPT / Gemini / open models cache implicitly. The guardian handle uses
`CacheScope::StaticPrefix` (`guardian_completion`) exactly like `anthropic`. Both the 5-minute and
the 1-hour TTL were accepted live.

### Tool input streaming

Keep rig's default `Eager` for Claude ids on the Messages wire: it was accepted live. For a
**non-Claude** model a user forces onto Messages with `wires` (the gateway's Anthropic translator
for other providers, where `ug` reports the rejection), set
`ToolInputStreaming::BetaHeader` on the built model's wire. This is the one place the Messages wire
differs by model in this kind.

### `thinking_off`

`thinking_off` is used where a forced tool call needs thinking off. Return `None` for Messages
Claude ids, Responses and Gemini (as `opencode-go` does). On Chat, return `None` too: the MLflow
unified route's acceptance of a top-level `thinking` field is unverified, and an unknown field risks
a 400 on every forced call. The guardian's validated Databricks model is Claude on Messages, so it
never needs it. Revisit once a Chat thinking model needs a forced call.

### Listing

`DatabricksClient::list` GETs
`{origin}/api/2.1/unity-catalog/model-services?parent=schemas/system.ai&page_size=100`, following
`next_page_token` (stop on a missing or repeated token, cap at 20 pages). It uses the shared
reqwest client, Bearer auth and `REQUEST_TIMEOUT`, as `ModelsFetcher` does. From each entry:

- keep `name`s that start with `model-services/system.ai.`, stripped of `model-services/`;
- drop an entry whose `supported_api_types` is a non-empty list containing **no** chat type. The
  chat types are `anthropic/v1/messages`, `openai/v1/responses`, `codex/v1/responses`,
  `gemini/v1/generateContent`, `gemini/v1/streamGenerateContent`, `mlflow/v1/chat/completions`,
  `mlflow/v1/responses` and `openai/v1/chat/completions`. A list that is missing or empty keeps the
  entry, since the field is advisory;
- de-duplicate and sort.

Errors map to `ProviderError` so `fontaine models` and the picker report them the way other kinds
do. Custom `catalog.schema.name` services are not listed. A user adds them through the account's
`models` list, which is validated against the listing as on other kinds. If `models` entries
outside the listing are refused by the shared validation, `lists()` is overridden to accept any id
that is not `system.ai.*` (custom services) without a listing match.

### Config and docs surface

- `AccountKind` gains `databricks` through the registry (`names()`), so config parsing, the
  provider dialog, `/login`, and `fontaine doctor` pick it up. The provider dialog asks for a base
  URL (`BaseUrlPolicy::Required`).
- Update the `config.rs` doc comments that enumerate kinds for `prompt_caching`, `cache_ttl` and
  `wires`.

## Implementation Steps

1. **Shared client.** Create `providers/multi_wire.rs` with `WireClients`, and move
   `GoClient::completion_in`'s dispatch onto it. Run the `opencode-go` tests unchanged.
2. **Facts helper.** Add `facts_as(vendor, catalog_id, served_id)` to `derived_facts.rs` (exact
   entry re-labelled and unpriced, else `derive`), with unit tests.
3. **Provider.** Create `providers/databricks.rs`: the `Databricks` `Provider` (with `validate`),
   `wire()`, the family → vendor map, `OUTPUT_CAPS`, `connect` building `WireClients` with Bearer on
   all four wires, `DatabricksClient` (`completion`, `guardian_completion`, `thinking_off`,
   `list`, `catalog_vendor`), the non-Claude-on-Messages `BetaHeader` override, and the listing
   parser as a pure function (`parse_model_services(body) -> (ids, next_token)`). Register it in
   `PROVIDERS` and `mod.rs`.
4. **Config wiring.** Update the kind enumerations in `config.rs` doc comments. Check the `wires`,
   `prompt_caching`, `cache_ttl` and `api_key_env` validation paths for `databricks`.
5. **TUI.** Make sure `provider_dialog.rs` lists the kind and asks for the base URL (driven by
   `BaseUrlPolicy`). Add a test row if the dialog has a per-kind table.
6. **Guardian list.** Add a `databricks` entry for `system.ai.claude-haiku-5-5` to
   `rust/fontaine-guardian/data/guardian_models.toml`, alongside the `opencode-go` Claude Haiku
   entry, if that file's format scopes entries per kind.
7. **Docs.** See Documentation.
8. **Tests.** See Testing Strategy.

## Files to Modify

- `rust/fontaine-core/src/providers/databricks.rs` (new)
- `rust/fontaine-core/src/providers/multi_wire.rs` (new)
- `rust/fontaine-core/src/providers/mod.rs`
- `rust/fontaine-core/src/providers/opencode_go.rs`
- `rust/fontaine-core/src/providers/derived_facts.rs`
- `rust/fontaine-core/src/providers/token_source.rs` (only if a fixed-key source is needed)
- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-tui/src/provider_dialog.rs` (only if a per-kind table needs a row)
- `rust/fontaine-guardian/data/guardian_models.toml`
- `rust/fontaine-core/tests/databricks.rs` (new)
- `docs/configuration.md`, `docs/guardian.md`, `docs/tui.md`
- `CHANGELOG.md` (by the `pr` step)

## Trade-offs

- **New kind vs. more knobs on `anthropic`/`openai-compatible`.** Extra knobs would still leave
  one wire per account. A Databricks user wants Claude, GPT and open models from one workspace and
  one token, which is exactly the `opencode-go` multi-wire shape.
- **Name routing vs. `supported_api_types` routing.** The listing field is advisory (the gateway
  serves models on routes they don't list), and a configured model may never be listed. Name
  routing works offline and is what Databricks' own launcher does.
- **`openai/v1` vs. `codex/v1` for Responses.** Both worked live. `openai/v1` is the documented one.
  A workspace that enables only `codex/v1` can still be served later with a `responses_route` key,
  which is not added now.
- **UC model-services listing vs. `/ai-gateway/anthropic/v1/models`.** The Anthropic listing works
  but mixes in non-callable translator entries and lists no GPT/Gemini/open models. The UC listing is
  the provider-neutral source.
- **Keeping `Eager` for Claude vs. switching everything to `BetaHeader`.** The live probe accepted
  `Eager`, and the beta header is the legacy mechanism. `BetaHeader` stays as the fallback for the
  translator path.

## Decisions

- Live probes (2026-11-11) overrode two of the issue's assumptions: Claude on `/ai-gateway/anthropic`
  accepts `eager_input_streaming`, and Gemini on the gateway needs Bearer, not `x-goog-api-key`.
- Billing is `Unpriced`: workspace pricing is in DBUs and not in rig's catalog, so a spend cap is
  unavailable on this kind, as on `opencode-go`.
- `databricks-` prefixed legacy ids are routed like `system.ai.` ones but are not listed.

## Documentation

- `docs/configuration.md`: add `databricks` to the kinds table (wire: per model; endpoint: none,
  `base_url` = workspace origin, required; credential: required, Bearer). Add it to the
  `wires` / `prompt_caching` / `cache_ttl` key rows and add a routing subsection (the family table,
  the `wires` override, custom services → Chat, `gpt-oss` → Chat). Replace the
  `/serving-endpoints/anthropic` `anthropic`+`bearer` example with a `databricks` account example,
  and keep one sentence saying `auth = "bearer"` on `anthropic` still serves other Bearer gateways.
- `docs/guardian.md`: mention the `databricks` Claude Haiku entry next to the `opencode-go` one,
  if step 6 adds it.
- `docs/tui.md`: the provider dialog's base-URL sentence lists the kinds that ask for one; add
  `databricks`.

## Testing Strategy

Unit tests (in `databricks.rs` / `derived_facts.rs` / `multi_wire.rs`):

- `wire()`: `system.ai.claude-sonnet-5-5` → Messages, `system.ai.gpt-5-6-luna` → Responses,
  `system.ai.gpt-oss-120b` → Chat, `system.ai.gemini-3-pro` → Gemini, `system.ai.glm-5-3` → Chat,
  `main.team.router` → Chat, `databricks-claude-haiku-5-5` → Messages, and a `wires` override
  beating the table.
- `parse_model_services`: prefix stripping, non-`system.ai` dropped, non-chat-only
  (`openai/v1/embeddings`) dropped, missing or empty `supported_api_types` kept, `next_page_token`
  returned, de-dup.
- `facts_as`: an exact Claude id gets a `max_output_tokens` and no pricing under the served id. A
  dashed GPT id derives from the dotted catalog entry. An unknown id gives `None`.
- `OUTPUT_CAPS` is applied to a GLM id with no facts, and not to one that has facts.
- `validate`: a base URL with a path is refused, and naming the key.

Integration tests (`rust/fontaine-core/tests/databricks.rs`, wiremock stand-in for the workspace,
modelled on `tests/opencode_go.rs`):

- Each wire hits its path (`/ai-gateway/anthropic/v1/messages`, `/ai-gateway/openai/v1/responses`,
  `/ai-gateway/gemini/v1beta/models/...`, `/ai-gateway/mlflow/v1/chat/completions`) with
  `Authorization: Bearer <key>`, and the Gemini request carries no `x-goog-api-key` (pins the live
  401).
- **Regression for the live `max_tokens` failure**: a `system.ai.claude-haiku-5-5` turn with no
  user cap reaches the mock with a numeric `max_tokens`.
- The Claude request carries `cache_control` breakpoints by default, the `ttl: "1h"` form with
  `cache_ttl = "1h"`, and none with `prompt_caching = false`. The GPT, Gemini and Chat requests
  carry no `cache_control`.
- Responses body has no `store`, `previous_response_id` or `background`. Chat body has no `store`
  and no `tools[].function.strict` (the gateway rejects them, per the issue).
- Claude tools carry `eager_input_streaming`. A non-Claude model forced onto Messages with
  `wires` carries the `fine-grained-tool-streaming` beta header and no per-tool field.
- Listing pages through two mock pages, sending `parent=schemas/system.ai` and `page_size=100`.
- The guardian handle on the kind marks only the static prefix (StaticPrefix scope).
- Billing: a response's cost is dropped (unpriced).

These run against mocks because the request shape is what Fontaine controls. The gateway's own
acceptance is the manual check below.

## Manual Verification

Against a real workspace (needs a Databricks token, so not in CI). Run with an account
`[accounts.dbx] kind = "databricks"`, `base_url = "https://<workspace>"`,
`api_key_env = "DATABRICKS_TOKEN"`:

1. `fontaine models` lists `dbx` with `system.ai.claude-*`, `system.ai.gpt-*` and open models, with
   no `anthropic-aigw-*` entries and no embedding models.
2. `fontaine -p --model dbx/system.ai.claude-haiku-5-5 "Run 'echo hi' then say pong"` succeeds
   with a tool call, and a second run in the same session reports non-zero
   `cache_read_input_tokens` in `--output-format json`.
3. The same with `dbx/system.ai.gpt-5-6-luna` (Responses, tools) and `dbx/system.ai.glm-5-3`
   (Chat) succeeds.
4. If the workspace serves a Gemini model, the same with `dbx/system.ai.<gemini model>` succeeds
   (the Bearer-on-Gemini path was only verified to be refused without it).

## Open Questions

- Which `anthropic-beta` values pass through on the Claude route (notably
  `thinking-binding-controls-2026-08-01`, #169). The gateway Messages dialect does not send it, so
  this does not block the kind.
- Guardian and scanner behaviour on `system.ai.*` names (#167, #168): follow-up issues.
