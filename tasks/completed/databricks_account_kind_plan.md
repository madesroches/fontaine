# Databricks Account Kind Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/166

## Overview

Add a `databricks` account kind that talks to Databricks Unity Gateway (`https://<workspace>/ai-gateway/...`,
model ids `system.ai.<model>`) with a Databricks token sent as `Authorization: Bearer`. Like
`opencode-go`, one account serves several wires and picks one per model: Claude on the Anthropic
Messages route (with prompt caching), GPT on the OpenAI Responses route, Gemini on the native Gemini
route, and every other model on the provider-neutral MLflow Chat Completions route. Models are
listed from the Unity Catalog model-services API.

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
| `gemini` kind at `.../ai-gateway/gemini` | 401 "Credential was not sent or was of an unsupported type": the gateway refused `?key=` auth (the stock `gemini` kind's request) and wants Bearer. |
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
                     naming the key and the expected form; also refuse a missing or empty `base_url`
                     (as `openai_compatible.rs` does)
  connect            -> DatabricksClient
```

`connect` trims the trailing `/` from the base URL and builds the four rig clients. **Every** wire
uses Bearer, Gemini included (the live 401). Chat and Responses use
`OpenAIConfig::with_key(&DBX_DIALECT, key)`, which sends Bearer. Messages uses
`AnthropicConfig::with_key(&DBX_MESSAGES, key)`. Only Gemini needs a decorated client placing the
key as Bearer. rig's `GeminiConfig` appends `?key=<api_key>` to every GenerateContent URI and
`TokenSource` has no static-key constructor, so Gemini uses `GeminiConfig::new("")` on a
`Decorated` client whose `Decoration.headers` holds a fixed, sensitive
`Authorization: Bearer <key>` entry (`auth: None`), and whose new `Decoration.strip_key_param: bool`
(default `false`, applied in `transport.rs` `decorate` independent of `auth`, reusing
`without_key_param`) is `true`. The request then carries no `key=` query at all, empty or not;
existing placements are unchanged. The kind-name constants:

- `DBX_DIALECT: Dialect = Dialect::gateway("databricks", "", "")` for Chat and Responses
  (the same gateway dialect `opencode-go` and `openai-compatible` use. Fontaine sets no
  `store`/`background` params and rig sends no strict schemas by default; the live Chat and
  Responses probes passed with it).
- `DBX_MESSAGES: MessagesDialect = compatible("databricks", "", "", None)`, the gateway Messages
  dialect, as on `opencode-go`. It supplies a default `max_tokens` and sends no strict tool schemas.
  It sends no thinking-binding beta. `compatible()` uses `Auth::ApiKey` (always `x-api-key`), so
  `connect` sets `config.dialect.quirks.auth = Auth::OptionalBearer` on the Messages config
  (mutating, since `Quirks` is non-exhaustive), as `anthropic.rs` does for `auth = "bearer"`.

Roots:

| Wire | rig client base |
|---|---|
| Messages | `{origin}/ai-gateway/anthropic` |
| Responses | `{origin}/ai-gateway/openai/v1` (the documented route; `codex/v1` works too) |
| Gemini | `{origin}/ai-gateway/gemini` |
| Chat | `{origin}/ai-gateway/mlflow/v1` |

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
                             scope: CacheScope, tool_input: Option<ToolInputStreaming>)
                             -> DynModel<Completion>;
}
```

`tool_input` is applied in the Messages arm before the model is erased. `GoClient` passes `None`.
`GoClient` keeps its own `wire()`, facts vendor, listing and `thinking_off`, and calls
`WireClients::completion`. Behaviour stays the same, and the existing `opencode-go` tests guard the
move.

### Model facts

For `system.ai.<base>`, the facts come from the base id on the family's catalog vendor:

| Family | Catalog vendor |
|---|---|
| `claude-*` | `anthropic` |
| `gpt-*` | `openai` |
| `gemini-*` | `gcp.gemini` |
| others | none |

Lookup: the exact `Catalog::builtin().get(vendor, base)` entry, else `derive(...)` on the same
vendor, called on the base id (shape matching also bridges `gpt-5-6-luna` and `gpt-5.6-luna`). Make
that a small helper in `derived_facts.rs`:
`facts_as(vendor, base_id, served_id) -> Option<(ModelFacts, String)>`. It relabels the spec to the
served id (`spec.id`, `display_name`) and rebuilds `ModelFacts::new(spec).with_catalog(..)`
(`ollama.rs` is the precedent). The exact-entry path clears `pricing` and `details.cost`
explicitly. It also returns the `from` id, the catalog entry the facts came from. It sets
`spec.provider = ProviderId::declare("databricks")` on the relabelled spec: rig's
`ModelFacts::for_model(vendor, model)` returns a bound spec only when `spec.provider.is_declared()`
or the vendor matches the asked name, and the wires ask under the dialect name `databricks`. That
gives Claude its catalog `max_output_tokens` on the Messages wire and the model's reasoning/effort
facts. The kind alone avoids the live `max_tokens` failure (`turn_max_tokens` sends the 32k fallback
for non-anthropic kinds and `compatible()` has a fixed default); `facts_as` supplies the catalog value.

Models with no facts fall into the existing `turn_max_tokens` fallback: `databricks` is not in the
`ollama | anthropic | claude-subscription` list, so `FALLBACK_MAX_OUTPUT_TOKENS` (32k) is sent. The
GLM output cap that `ug` pins (25k on the Chat route) is lower than that, so a GLM request without a
user cap can be rejected. Add one entry, in a per-family table in `databricks.rs`
(`OUTPUT_CAPS: &[("glm-", 25_000)]`). GLM has no catalog facts, so there is no spec to put the cap
on: for a base id matching a cap and with no facts, build a minimal spec under the declared
`databricks` vendor with `max_output_tokens = cap`, `tools = true` and
`reasoning = ReasoningSupport::Unknown { default: None }` (a bare `ModelSpec::new` defaults to
`tools: false`, no reasoning). More families are added there as they are found. Users can still
lower the cap with `[model] max_output_tokens`.

`catalog_vendor()` returns `None`. Pricing is never looked up for this kind (`Billing::Unpriced`).
Because `catalog_vendor()` is `None`, the default `facts_notice` (`providers/mod.rs:273`) never
fires, so `DatabricksClient` overrides `facts_notice` and returns
`derived_facts::notice(model, base_id, &from, None)` when the facts were derived
(`claude_subscription/mod.rs:411` is the precedent).

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
`ToolInputStreaming::BetaHeader` on the built model's wire. This is the one place the Messages wire differs by model in this kind. The override is passed as
`WireClients::completion`'s `tool_input` argument. Whether the gateway passes
`fine-grained-tool-streaming` through for a non-Claude model forced onto Messages is unverified.

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
do. Custom `catalog.schema.name` services are not listed. A `models` list replaces the UC listing
(non-discovery kinds don't validate it against the listing), so custom services are accepted as-is,
and a user who adds one must also list the `system.ai.*` models they want. There is no `lists()`
override.

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
   all four wires (Gemini through the static header and `strip_key_param`: add that
   `Decoration` field and its `decorate` branch in `transport.rs`, with a unit test there),
   `DatabricksClient` (`completion`, `guardian_completion`,
   `list`, `catalog_vendor`, `facts_notice`), the non-Claude-on-Messages `BetaHeader` override, and the listing
   parser as a pure function (`parse_model_services(body) -> (ids, next_token)`). Register it in
   `PROVIDERS` and `mod.rs`. The provider dialog picks the kind up through the registry. Add a
   `"databricks" => None` arm in `tests/providers.rs` (~line 331), and special-case `databricks`
   with a pathless `base_url` (e.g. `http://localhost:1`) in
   `every_registered_name_round_trips_through_the_kind` and
   `every_kind_but_the_arbitrary_gateway_names_a_catalog_vendor`, as they do for claude-subscription,
   since `validate` refuses a base_url with a path.
4. **Config wiring.** Update the kind enumerations in `config.rs` doc comments. Check the `wires`,
   `prompt_caching`, `cache_ttl` and `api_key_env` validation paths for `databricks`.
5. **Min cacheable tokens.** Include `databricks` in `min_cacheable_tokens`
   (`providers/cache_minimum.rs:27`), stripping `system.ai.` / `databricks-` before the Claude match.
6. **Docs.** See Documentation.
7. **Tests.** See Testing Strategy.
8. **cache-bench wording.** Update the `cache-bench` help text in
   `rust/fontaine-evals/src/runner.rs` (~lines 65-91), the module doc in `cache_bench.rs` and the
   "Prompt-cache bench" section of `evals/README.md` to say any Claude model on a Messages-wire
   account (claude-subscription or databricks) works. `cache_bench.rs` and `run_cache_bench` do not
   gate on account kind, so nothing there blocks a databricks run.

## Files to Modify

- `rust/fontaine-core/src/providers/databricks.rs` (new)
- `rust/fontaine-core/src/providers/multi_wire.rs` (new)
- `rust/fontaine-core/src/providers/mod.rs`
- `rust/fontaine-core/src/providers/opencode_go.rs`
- `rust/fontaine-core/src/providers/derived_facts.rs`
- `rust/fontaine-core/src/providers/cache_minimum.rs`
- `rust/fontaine-core/src/transport.rs` (`Decoration.strip_key_param`)
- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-core/tests/providers.rs`
- `rust/fontaine-core/tests/databricks.rs` (new)
- `rust/fontaine-evals/src/runner.rs`, `rust/fontaine-evals/src/cache_bench.rs` (wording only)
- `evals/README.md`
- `docs/configuration.md`, `docs/guardian.md`, `docs/tui.md`, `docs/telemetry.md`, `docs/tools.md`
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

## Decisions

- Live probes (2026-11-11) overrode two of the issue's assumptions: Claude on `/ai-gateway/anthropic`
  accepts `eager_input_streaming`, and Gemini on the gateway needs Bearer, not `x-goog-api-key`.
- Billing is `Unpriced`: workspace pricing is in DBUs and not in rig's catalog, so a spend cap is
  unavailable on this kind, as on `opencode-go`.
- `databricks-` prefixed legacy ids are routed like `system.ai.` ones but are not listed.
- The validated-guardian entry for this kind is generated post-merge by
  `fontaine-evals --guardian live:<account>/system.ai.claude-haiku-5-5`.
- rig never sends thinking-binding-controls on a non-Anthropic dialect; gateway pass-through is
  tracked in #169.
- #167 is addressed for this kind by `facts_as`; remaining guardian/scanner name handling is tracked
  in #167/#168.
- AFK: Gemini sends the static key as a sensitive `Authorization: Bearer` entry in `Decoration.headers` plus a new `Decoration.strip_key_param` flag in `transport.rs`, so no `key=` (empty or not) ever reaches the URL; chosen over relying on unverified empty-`key=` acceptance and over a new static `TokenSource` plus placement.
- AFK: `thinking_off` keeps the trait default (`None`): the MLflow Chat route's acceptance of a top-level `thinking` field is unverified; revisit when a Chat thinking model needs a forced call.
- AFK: A flagged cache-bench run (steady-state cache-write ratio above 1.25, lower cache-read share or larger base context) blocks the PR unless the gap is traced to the gateway rather than Fontaine, in which case it is recorded in the PR and filed as a follow-up.

## Documentation

- `docs/configuration.md`: add `databricks` to the kinds table (wire: per model; endpoint: none,
  `base_url` = workspace origin, required; credential: required, Bearer). Add it to the
  `wires` / `prompt_caching` / `cache_ttl` key rows and add a routing subsection (the family table,
  the `wires` override, custom services → Chat, `gpt-oss` → Chat). Replace the
  `/serving-endpoints/anthropic` `anthropic`+`bearer` example with a `databricks` account example,
  and keep one sentence saying `auth = "bearer"` on `anthropic` still serves other Bearer gateways.
- `docs/configuration.md:176` (kind row), `:178` (`base_url` required list), `:182`
  (`prompt_caching` default), `:186` (`auth` row: point the Databricks mention to the new kind),
  `:272-276` (derived-facts paragraph): one-line update each.
- `docs/telemetry.md:132-135`: add `databricks` to the unpriced kinds list.
- `docs/guardian.md:675` (thinking-off on Chat), `:700-701` (no-catalog-match endpoints list):
  one-line update each.
- `docs/tools.md:450-452`: Databricks native search mention, one-line update.

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
- `facts_as`: the relabelled spec is declared `databricks`, so `completion("system.ai.claude-...").spec()`
  returns the catalog `max_output_tokens` on Messages (and on Responses via a `wires` override).
- `OUTPUT_CAPS`: GLM with no user cap sends 25k, a lower user cap wins, and a non-GLM open model
  sends 32k.
- `validate`: a base URL with a path is refused, naming the key; a missing or empty `base_url` is
  refused.
- `facts_notice` returns the derived-facts notice for a derived id.
- `Databricks.billing() == Billing::Unpriced`.

Integration tests (`rust/fontaine-core/tests/databricks.rs`, wiremock stand-in for the workspace,
modelled on `tests/opencode_go.rs`):

- Each wire hits its path (`/ai-gateway/anthropic/v1/messages`, `/ai-gateway/openai/v1/responses`,
  `/ai-gateway/gemini/v1beta/models/...`, `/ai-gateway/mlflow/v1/chat/completions`) with
  `Authorization: Bearer <key>`, the Gemini request carries `Authorization: Bearer <key>` and no `key=` query
  parameter at all (pins the live 401), and the Claude request carries no `x-api-key`.
- **Regression for the live `max_tokens` failure**: a `system.ai.claude-haiku-5-5` turn with no
  user cap reaches the mock with `max_tokens` equal to the catalog `max_output_tokens` for
  claude-haiku-5-5.
- The Claude request carries `cache_control` breakpoints by default, the `ttl: "1h"` form with
  `cache_ttl = "1h"`, and none with `prompt_caching = false`. The GPT, Gemini and Chat requests
  carry no `cache_control`.
- Claude tools carry `eager_input_streaming`. A non-Claude model forced onto Messages with
  `wires` carries the `fine-grained-tool-streaming` beta header and no per-tool field.
- Listing pages through two mock pages, sending `parent=schemas/system.ai` and `page_size=100`.
- The guardian handle on the kind marks only the static prefix (StaticPrefix scope).

## Manual Verification

Against a real workspace (needs a Databricks token, so not in CI). Run with an account
`[accounts.dbx] kind = "databricks"`, `base_url = "https://<workspace>"`,
`api_key_env = "DATABRICKS_TOKEN"`:

1. `fontaine models` lists `dbx` with `system.ai.claude-*`, `system.ai.gpt-*` and open models, with
   no `anthropic-aigw-*` entries and no embedding models.
2. `fontaine -p --model dbx/system.ai.claude-haiku-5-5 "Run 'echo hi' then say pong"` succeeds
   with a tool call, and a second run with `-c` in the same session reports non-zero
   `cache_read_input_tokens` in `--output-format json`.
3. The same with `dbx/system.ai.gpt-5-6-luna` (Responses, tools) and `dbx/system.ai.glm-5-3`
   (Chat) succeeds.
4. If the workspace serves a Gemini model, the same with `dbx/system.ai.<gemini model>` succeeds
   (the Bearer-on-Gemini path was only verified to be refused without it).
5. A non-Claude model forced onto Messages with `wires` (e.g. `wires = { "system.ai.glm-5-3" = "messages" }`)
   succeeds with a tool call, confirming `fine-grained-tool-streaming` pass-through.
6. Token and cache efficiency against Claude Code on the same workspace (manual because it needs a
   live workspace token and the real `claude` CLI). With `claude` configured against the same
   workspace's Unity Gateway, run for Sonnet, Opus and Haiku:
   `cargo run -p fontaine-evals -- cache-bench --model dbx/system.ai.claude-sonnet-5-5 --claude-model <same model as claude names it on the gateway> --out ../evals/scorecards/databricks-cache-sonnet.json`
   (and `-opus`, `-haiku`). Pass when, for each model: the run is not flagged (steady-state
   cache-write ratio, Fontaine over Claude Code, at most 1.25); Fontaine's total cache-read share of
   input is not lower than Claude Code's by more than a few points; and Fontaine's base context is
   not larger than Claude Code's. A failing model blocks the PR until the gap is fixed, or traced
   to the gateway rather than to Fontaine's requests (then recorded in the PR and filed as a
   follow-up).

### Manual verification results (2026-11-11, branch build, one live workspace)

- `fontaine models`: 58 `system.ai.*` chat models listed through the UC model-services API; no embedding or `anthropic-aigw-*` entries.
- Tool-calling turns succeeded on every wire: `system.ai.claude-haiku-5-5` (Messages), `system.ai.gpt-5-6-luna` (Responses), `system.ai.glm-5-3` (Chat), `system.ai.gemini-3-5-flash` (Gemini, Bearer with no `key=`), and `system.ai.glm-5-3` forced onto Messages with `wires` (beta header path).
- `-c` continuation on `system.ai.claude-haiku-5-5`: 11,777 of 11,790 input tokens read from cache.
- `cache-bench` against `claude -p` (Claude Code 2.1.322) on the same gateway, scorecards in `evals/scorecards/databricks-cache-{sonnet,opus,haiku}.json`:

  | Model | Steady-state write ratio | Base context (Fontaine / Claude Code) | Total input (Fontaine / Claude Code) |
  |---|---|---|---|
  | Sonnet 5.5 | 0.95 | 11,850 / 17,851 | about 173k / 220k |
  | Opus 5.5 | 0.95 | 11,854 / 17,974 | about 173k / 220k |
  | Haiku 5.5 | 0.95 | 11,847 / 18,631 | about 173k / 222k |

  Every run is under the 1.25 threshold. Fontaine's cache-read share of input is higher than Claude Code's (about 84% vs. 80%).
