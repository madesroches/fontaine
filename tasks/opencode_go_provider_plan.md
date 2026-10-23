# OpenCode Go Provider Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/93

## Overview

[OpenCode Go](https://opencode.ai/docs/go/) is a subscription that serves about 30 coding models
behind one gateway, each model on one of four wire formats (Chat Completions, Responses, Anthropic
Messages, Gemini `generateContent`). This plan adds an `opencode-go` account kind so one account
reaches every Go model:

- **Interactive sign-in, not an API key.** `fontaine auth login <account>` runs the same OAuth 2.0
  device-authorization flow the opencode CLI runs (client id `opencode-cli` against
  `https://opencode.ai/console`). It stores an access/refresh token pair, and Fontaine refreshes it
  silently.
- **One account, many wires.** Each request for `go/<model>` goes to the right wire, from a
  built-in routing table that the account can override per model.
- **Client identification.** Every Go request carries a `fontaine/<version>` User-Agent and the
  session id in `x-opencode-session`, as the Go terms ask.
- **Unpriced.** Go usage is billed against subscription allowances, so its models are unpriced and
  `--max-budget-usd` does not apply to them.

To get there, the per-kind `match` arms spread across `config.rs` and `models.rs` are first
replaced by a **provider trait and registry** (open/closed). Each kind becomes one module.
Adding a provider then means adding a module and one registry line, with no edits to resolution,
login or listing code.

## Current State

- `AccountKind` (`rust/fontaine-core/src/config.rs:214-239`) is a closed serde enum: `anthropic`,
  `openai`, `gemini`, `openai-compatible`, `ollama`. `Account` (`config.rs:265-271`) has `kind`,
  `api_key_env`, `base_url`, `models`, and uses `deny_unknown_fields`.
- Per-kind behavior lives in `match`es in `rust/fontaine-core/src/models.rs`:
  - `Client` enum + `Client::build` / `completion` / `list` (`models.rs:165-245`): one arm per
    kind. `openai-compatible` uses the `GATEWAY` dialect (`models.rs:22-24`), which leaves it
    unpriced.
  - `needs_credential` (`models.rs:278-283`), `lists` (Ollama's `:latest` rule, `models.rs:360-364`).
  - `ModelHandle::priced` (`models.rs:110-114`) reads rig's catalog spec.
- The HTTP client for every account is `transport::caused_client()`
  (`rust/fontaine-core/src/transport.rs`), a `CausedClient<ReqwestClient>` that implements rig's
  `HttpClientExt` and rewrites transport errors. It is the one place every provider request goes
  through. rig 0.45 has no default-headers hook on any provider config, so request decoration
  belongs here.
- rig auth per wire: OpenAI-family wires send `Authorization: Bearer`. Anthropic sends `x-api-key`
  (`rig-core-0.45.0/src/providers/anthropic/wire.rs:362`). Gemini `generateContent` puts the key in
  the query string as `?key=` (`gemini/mod.rs:96-101`). `OpenAIConfig::with_route(Route::Chat |
  Route::Responses)` picks the OpenAI-family wire independently of the dialect
  (`openai/wire.rs:739-750`).
- Credentials: `credentials.toml` (`rust/fontaine-core/src/credentials.rs`) holds
  `[accounts.<name>] api_key = "..."` only (`Entry`, `deny_unknown_fields`). `fontaine auth login`
  (`rust/fontaine/src/auth.rs`) reads a secret from a hidden prompt or stdin and stores it.
- Session id: `open_session` (`rust/fontaine/src/lib.rs:345-391`) runs inside `prepare_session`
  (line 226), **before** `resolve_models` (lines 586/631). The id is therefore known when models
  are resolved, but `ResolveParams` / `Switcher::new` do not carry it. `fontaine-evals`'s live
  driver (`rust/fontaine-evals/src/live.rs:85`) builds a `Switcher` with no session.
- Cost: rig attaches a catalog price per response. It reaches spend through
  `headless.rs:213` (`cost.total`) and the span bridge (`telemetry/span_bridge.rs:250-255`, looked
  up by `gen_ai.provider.name`). An Anthropic-wire model behind a gateway would be priced as
  `anthropic` today.
- Docs: `docs/configuration.md` `[accounts.<name>]` (kind list, wire table, "Where a credential
  comes from", `openai-compatible` examples) and the Credentials section (`credentials.toml`
  shape, `fontaine auth login`).
- opencode's own client (`anomalyco/opencode`, `packages/core/src/plugin/provider/opencode.ts`) is
  the reference for the flow:
  - `POST {server}/auth/device/code` `{client_id: "opencode-cli", supports_org_scope: true}` →
    `{device_code, user_code, verification_uri_complete, expires_in, interval}`.
  - Poll `POST {server}/auth/device/token` `{grant_type:
    "urn:ietf:params:oauth:grant-type:device_code", device_code, client_id}`. It answers
    `authorization_pending` (wait), `slow_down` (wait +5 s), another `error` (fail), or
    `{access_token, refresh_token, expires_in, org_id?}`.
  - Refresh with `POST {server}/auth/device/token` `{grant_type: "refresh_token", refresh_token,
    client_id}`. A 400 `{error: "invalid_grant"}` means signed out. Refresh tokens rotate.
  - `GET {server}/api/user` (Bearer) → `{id, email}`.
  - Gateway auth uses the access token: Bearer on OpenAI-family routes, `x-api-key` on Messages,
    `x-goog-api-key` on Google (per the `grikomsn/pi-provider-opencode-console` parity notes,
    which ship this against Go).

## Design

### 1. Provider trait and registry (`fontaine-core/src/providers/`)

```rust
// providers/mod.rs
pub trait Provider: Send + Sync + 'static {
    /// The `kind` value in settings.
    fn name(&self) -> &'static str;
    fn credential(&self) -> CredentialPolicy;          // Required | Optional | SignIn
    fn billing(&self) -> Billing { Billing::Catalog }  // Catalog | Unpriced
    /// Account-shape checks beyond serde: required `base_url`, keys this kind refuses.
    fn validate(&self, name: &str, account: &Account) -> Result<(), String> { Ok(()) }
    /// Whether a listed id satisfies a requested model (Ollama's `:latest`).
    fn lists(&self, available: &[String], model: &str) -> bool { available.iter().any(|id| id == model) }
    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String>;
    /// The sign-in `fontaine auth login` runs instead of prompting for a key.
    fn sign_in(&self) -> Option<&dyn SignIn> { None }
}

pub trait AccountClient: Send + Sync {
    fn completion(&self, model: &str) -> DynModel<Completion>;
    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>>;
}

pub struct ConnectContext<'a> {
    pub account_name: &'a str,
    pub account: &'a Account,
    pub credential: Option<StoredCredential>,   // ApiKey(String) | OAuth(OAuthTokens)
    pub data_dir: &'a Path,
    pub session_id: Option<&'a str>,
}

const PROVIDERS: &[&dyn Provider] = &[
    &anthropic::Anthropic, &openai::OpenAi, &gemini::Gemini,
    &openai_compatible::OpenAiCompatible, &ollama::Ollama, &opencode_go::OpencodeGo,
];
pub fn find(name: &str) -> Option<&'static dyn Provider>;
pub fn names() -> impl Iterator<Item = &'static str>;
```

- `AccountKind` becomes a `Copy` newtype over `&'static dyn Provider`. It deserializes by
  registry lookup, and an unknown kind is a config error listing `names()`. It keeps `as_str()`
  and gains `provider()`, and `PartialEq` compares names. Tests that wrote `AccountKind::Ollama`
  use `AccountKind::named("ollama")`, which panics on an unknown name (test-only convenience).
- `models.rs` keeps resolution, error mapping and guardian choice. It loses the `Client` enum,
  `needs_credential` and the kind arm of `lists`. `client_for` becomes:
  1. look up the stored credential;
  2. apply `credential()`. Required + missing is the existing error. SignIn + missing is
     "account `go` is not signed in; run `fontaine auth login go`";
  3. call `provider.connect(ctx)`.
- `ModelHandle` gains `billing: Billing`. `priced()` returns `false` when it is `Unpriced`.
- `Config::validate` runs `provider.validate` for each account. The existing "`base_url` is
  required for `openai-compatible`" check moves into that provider.
- The five existing kinds move one-to-one into `providers/{anthropic,openai,gemini,ollama,
  openai_compatible}.rs` with unchanged behavior. `GATEWAY` moves to `openai_compatible.rs`.

Adding a provider is then one module plus one `PROVIDERS` line, plus its docs row.

### 2. Reusable building blocks (not Go-specific)

- **`providers/device_flow.rs`**: an RFC 8628 client parameterized by `DeviceFlowEndpoints {
  code_url, token_url, client_id, code_params: &[(&str, Value)] }`.
  - `start()` → `DeviceCode`.
  - `poll(&DeviceCode)` → `OAuthTokens`. It handles `authorization_pending`, `slow_down` (+5 s),
    and `expires_in` (gives up with "the sign-in code expired"). Any other `error` fails, naming
    the error code.
  - `refresh(&refresh_token)` → `OAuthTokens`, or `RefreshError::SignedOut` on 400
    `invalid_grant`.

  The verification URL must be `http(s)`; anything else is an error.
- **`OAuthTokens`** `{access, refresh, expires_at (unix seconds), extra: BTreeMap<String,
  String>}`. `extra` holds provider metadata such as `server`, `org_id` and `email`.
- **`providers/token_source.rs`**: a `TokenSource` per account, shared process-wide through a
  `OnceLock<Mutex<HashMap<(data_dir, account), Arc<TokenSource>>>>`, so the `Switcher` rebuilding
  clients and the guardian share one refresh. `token().await`:
  - returns the cached access token while more than 5 minutes remain;
  - otherwise refreshes single-flight under a `tokio::sync::Mutex`:
    1. take an exclusive `std::fs::File::lock` on `<data_dir>/credentials.lock`;
    2. re-read `credentials.toml`; if another process already rotated the entry, adopt it;
    3. otherwise call the provider's refresh and write the rotated pair back through
       `credentials::store_oauth`.

  Refresh tokens rotate, so an unlocked concurrent refresh would invalidate one process's
  session. A `SignedOut` refresh surfaces as an error naming `fontaine auth login <account>`.
- **`transport::decorated_client(Decoration)`**: wraps `CausedClient` in `Decorated<C>`, which
  implements `HttpClientExt`. Before delegating it applies, to every request:
  - `user_agent: Option<&'static str>` → sets `User-Agent`.
  - `headers: Vec<(HeaderName, HeaderValue)>` → sets static headers (`x-opencode-session`).
  - `auth: Option<(Arc<TokenSource>, AuthPlacement)>`. It awaits `token()` and, by placement:
    - `Bearer` replaces `Authorization`;
    - `Header(name)` removes `Authorization` and sets `name`;
    - `GoogleKey` strips the `key` query parameter rig put on the URL and sets `x-goog-api-key`.

  A token error becomes an `Error::Instance` carrying the sign-in hint, so it reaches the user
  through the existing cause chain. For the guardian, it is a provider failure, so the guardian
  fails closed.

  `FONTAINE_USER_AGENT` (`concat!("fontaine/", env!("CARGO_PKG_VERSION"), "
  (+https://github.com/madesroches/fontaine)")`) moves from `fontaine-tools/src/webfetch.rs` to a
  shared constant in `fontaine-tools`, which `fontaine-core` already depends on.

### 3. Credentials store

`Entry` becomes `{ api_key: Option<String>, oauth: Option<OAuthEntry> }`, with exactly one set
(`deny_unknown_fields` kept):

```toml
[accounts.go.oauth]
access = "..."
refresh = "..."
expires_at = 1798000000
server = "https://opencode.ai/console"
org_id = "org_..."
email = "me@example.com"
```

- `Credentials::account(name)` returns `Option<StoredCredential>`.
- `store_oauth(data_dir, name, &OAuthTokens)` uses the same owner-only read-modify-write as
  `store`.
- `validate_credentials_toml` accepts the new shape.
- Old files (`api_key` only) parse unchanged.

### 4. `opencode-go` provider (`providers/opencode_go.rs`)

- `name` `opencode-go`, `credential` `SignIn`, `billing` `Unpriced`.
- `validate`: rejects `api_key_env` with "`opencode-go` accounts sign in with `fontaine auth login
  <name>`; `api_key_env` is not used". `wires` is accepted only here; every other provider's
  `validate` rejects it.
- `base_url` defaults to `https://opencode.ai/inference/go`. A trailing `/` is trimmed.
- `Account` gains `wires: Option<BTreeMap<String, Wire>>` with `Wire = chat | responses |
  messages | gemini`. It is the per-model override for Go's routing.
- **Routing.** `fn wire(model, overrides) -> Wire` takes the override first, then the built-in
  table:

  | Model id | Wire |
  |---|---|
  | starts with `gpt-` or `grok-` | `responses` |
  | starts with `claude-` or `qwen`, or is exactly `minimax-m3` | `messages` |
  | starts with `gemini-` | `gemini` |
  | anything else | `chat` |

  The table is the issue's published mapping. `wires` covers a model the table gets wrong
  without a release.
- **Clients.** `connect` builds one `Decorated` HTTP client per auth placement, sharing one
  `TokenSource` and the decoration (User-Agent, `x-opencode-session` when `session_id` is set):
  - `chat`: `OpenAIConfig::with_key(&GO_DIALECT, "")` + `with_route(Route::Chat)`, base
    `{base}/openai/v1`, `Bearer`.
  - `responses`: same dialect + `with_route(Route::Responses)`, base `{base}/openai/v1`, `Bearer`.
  - `messages`: `AnthropicConfig::new("")`, base `{base}/anthropic`, `Header("x-api-key")`.
  - `gemini`: `GeminiConfig::new("")`, base `{base}/google`, `GoogleKey`.
  - listing: an OpenAI client at `{base}/v1` (`GET {base}/v1/models`).

  `GO_DIALECT = Dialect::gateway("opencode-go", "", "")`, unpriced like `GATEWAY`. The placeholder
  keys are overwritten by the decoration on every request.
- **Sign-in** (`SignIn` impl), with server `https://opencode.ai/console`:
  1. Start the device flow with `code_params` `supports_org_scope: true`.
  2. Print the verification URL and user code to stderr. The browser is not opened
     automatically.
  3. Poll for the tokens.
  4. `GET {server}/api/user` for the email.
  5. Store the tokens via `store_oauth` with `extra` = server, org id, email.
  6. Print "signed in to OpenCode Go as <email>".

  Stdin is not read, so it also works from a non-terminal.
- **Pricing.** `billing() == Unpriced` makes `priced()` false. The handle must also report no cost
  for usage, including the `messages` wire, where rig attaches Anthropic catalog prices. Cost is
  dropped where rig's per-response cost is summed: headless `Totals` (`headless.rs:213`), the
  guardian/subagent usage accumulators and the span bridge. Each of those already knows, or is
  given, the handle's `Billing`. The implementer threads `Billing` to the accumulation points
  found by `grep -n "cost.total\|cost_usd(" rust/`.

### 5. Session id plumbing

- `ResolveParams` and `Switcher::new` gain `session_id: Option<String>`.
- `fontaine/src/lib.rs` passes `prep.session.id`. A resumed session keeps its id, so the header
  stays stable per conversation. Subagents and the guardian reuse the root session's id.
- `fontaine-evals` live mode passes a fresh UUID per run.

### 6. `fontaine auth login`

`auth::login` looks up the account's provider:

- with `sign_in()`, it runs that sign-in (async, on the existing runtime) and skips the secret
  prompt; `--telemetry` is unchanged;
- otherwise it stores an API key as today.

The `api_key_env` warning stays for key-based kinds.

## Implementation Steps

1. **Registry refactor (no behavior change).**
   1. Create `fontaine-core/src/providers/{mod,anthropic,openai,gemini,ollama,openai_compatible}.rs`.
   2. Turn `AccountKind` into the registry newtype.
   3. Move the per-kind logic out of `models.rs`.
   4. Add `Billing` to `ModelHandle`.
   5. Move the `openai-compatible` `base_url` check into `validate`.
   6. Update tests to `AccountKind::named`.

   `cargo test` must pass unchanged apart from those constructors.
2. **Credentials.** Add the `oauth` entry shape, `StoredCredential`, `store_oauth`, and the
   `credentials.lock` helper, with unit tests.
3. **Building blocks.**
   1. Add `device_flow.rs` and `token_source.rs`.
   2. Add `transport::Decorated` + `decorated_client`.
   3. Move the User-Agent constant to `fontaine-tools`.
4. **Session plumbing.** Thread `session_id` through `ResolveParams`, `Switcher`, `lib.rs` and
   `fontaine-evals`.
5. **`opencode-go` provider.** Add the module with routing, clients and the `SignIn` impl, plus
   the `wires` key on `Account` and its validation.
6. **Billing.** Drop cost for `Unpriced` handles at every accumulation point.
7. **`auth login`.** Dispatch to `sign_in()`.
8. **Docs** (see Documentation).

## Files to Modify

- `rust/fontaine-core/src/providers/` (new: `mod.rs`, `anthropic.rs`, `openai.rs`, `gemini.rs`,
  `ollama.rs`, `openai_compatible.rs`, `opencode_go.rs`, `device_flow.rs`, `token_source.rs`)
- `rust/fontaine-core/src/lib.rs` (module)
- `rust/fontaine-core/src/config.rs` (`AccountKind`, `Account::wires`, validation hook)
- `rust/fontaine-core/src/models.rs` (use the registry; `Billing`; `session_id`)
- `rust/fontaine-core/src/transport.rs` (`Decorated`)
- `rust/fontaine-core/src/credentials.rs` (oauth entry, lock, `store_oauth`)
- `rust/fontaine-tools/src/webfetch.rs` + `rust/fontaine-tools/src/lib.rs` (shared User-Agent)
- `rust/fontaine/src/auth.rs`, `rust/fontaine/src/lib.rs`, `rust/fontaine/src/headless.rs`,
  `rust/fontaine/src/telemetry/span_bridge.rs`
- `rust/fontaine-evals/src/live.rs`
- Tests: `rust/fontaine-core/tests/{models,config,accounts,credentials}.rs`, test helpers that
  name `AccountKind::*`, a new `rust/fontaine-core/tests/opencode_go.rs`, `rust/fontaine/tests/`
  (headless)
- `docs/configuration.md`, `CHANGELOG.md`

## Trade-offs

- **Dedicated `opencode-go` kind vs. a generic per-model wire map on any account.** A generic map
  would still need Go's sign-in, headers and billing, which no other kind has. With the registry,
  a second multi-wire gateway is one more module that reuses `Wire`, `Decorated` and the device
  flow. The `wires` key exists, but only on `opencode-go`.
- **Registry of trait objects vs. keeping the enum.** The enum forces every new kind through
  every `match` in config, models and auth (closed for extension). The trait keeps the kind name
  as the stable settings key, at the cost of one dynamic dispatch per client build.
- **Static routing table vs. discovering wires from the Console's `/api/v2/config`.** opencode
  reads per-model packages from that endpoint, but its schema is undocumented and versioned
  server-side. The table plus `wires` override is testable offline and fails visibly (wrong wire
  → provider error) rather than silently.
- **Request-layer auth vs. a key baked in at client build.** Access tokens expire mid-session.
  Baking the key in would mean rebuilding rig clients on refresh. The decoration reads a fresh
  token per request and touches no rig internals.
- **Device flow only vs. also accepting a Go API key.** Per the user's direction, Go accounts sign
  in interactively. An `sk-` service key would be a second credential path to test and document.
  Adding one later is a `validate` + `connect` change in this module only.

## Decisions

- 2026-10-23: Go accounts authenticate only through the device sign-in; `api_key_env` is rejected
  on `opencode-go` (user direction: "use the interactive login like the opencode CLI, not an API
  key").
- 2026-10-23: Providers sit behind a `Provider` trait and registry so adding one is a new module
  (user direction: open/closed).
- 2026-10-23: `fontaine auth login` prints the verification URL and code but does not launch a
  browser. It adds no dependency, and it works over SSH and in containers.

## Security

- Access and refresh tokens live only in `credentials.toml`, written owner-only. They are never
  logged, never placed in a URL (Gemini's `?key=` is stripped and moved to a header), and
  `Debug` is not derived on any type holding them.
- Token failure (refresh error, signed out, lock I/O) is a provider error. A main-model turn fails
  with the sign-in hint. The guardian's adjudication fails closed, which the guardian invariant
  already requires; no new path turns a missing token into an allow.
- Accounts stay user-scope only (`[accounts.*]` is refused in project files). A repository cannot
  point `base_url` or `wires` anywhere.
- The device verification URL is checked to be `http(s)` before it is printed.

## Documentation

- `docs/configuration.md`:
  - `[accounts.<name>]`: add `opencode-go` to `kind`. Add the `wires` key row (opencode-go only).
    Add an `opencode-go` row to the wire table (all four wires, default endpoint, credential
    "sign-in").
  - "Where a credential comes from": add the sign-in case.
  - Add a provider example:

    ```toml
    [accounts.go]
    kind = "opencode-go"
    ```

    followed by `fontaine auth login go` and `--model go/kimi-k3`. Then a `wires` override
    example, and the note that Go models are unpriced, so `--max-budget-usd` does not apply.
  - Credentials section: the `oauth` entry shape, and that `fontaine auth login` on an
    `opencode-go` account opens a device sign-in instead of a prompt.
- `CHANGELOG.md`: the new kind and the provider registry.

## Testing Strategy

All automated tests run offline with `wiremock`. No test touches opencode.ai.

- **Registry** (unit):
  - names are unique;
  - every name round-trips through `AccountKind` deserialization;
  - an unknown kind's error lists every registered name;
  - existing `models.rs` tests pass unchanged except for constructors (the refactor is
    behavior-neutral).
- **Validation** (unit):
  - `api_key_env` on `opencode-go` is refused with the sign-in hint;
  - `wires` on any other kind is refused;
  - `openai-compatible` without `base_url` keeps its existing error.
- **Routing** (unit): every row of the table, and an override that beats the table.
- **Device flow** (wiremock):
  - pending → slow_down → success; the poll interval grows by 5 s after `slow_down` (use
    `tokio::time::pause`);
  - `access_denied` and `expired_token` fail, naming the code;
  - the code expires locally;
  - a non-http verification URL is refused;
  - the request bodies carry `client_id=opencode-cli` and `supports_org_scope`.
- **Token source** (wiremock + temp data dir):
  - a fresh token is served without a request;
  - a near-expiry token refreshes once under 10 concurrent callers (single flight);
  - the rotated pair is persisted;
  - an entry rotated on disk by "another process" (the test rewrites the file before the refresh)
    is adopted without calling the server;
  - `invalid_grant` yields the sign-in hint.
- **Decoration** (unit against a capturing fake `HttpClientExt`):
  - `User-Agent` and `x-opencode-session` are set;
  - `Bearer` replaces `Authorization`;
  - `Header("x-api-key")` removes `Authorization`;
  - `GoogleKey` strips `?key=` and sets `x-goog-api-key`, and the URL in any error has no key;
  - a token error comes back as a transport error carrying the hint.
- **Credentials** (unit):
  - an oauth entry round-trips;
  - an old api_key-only file still parses;
  - an entry with both or neither of `api_key` and `oauth` is malformed (and the message never
    echoes values);
  - `validate_credentials_toml` accepts the docs example.
- **Billing** (unit): an `opencode-go` handle on each wire is unpriced and contributes no cost,
  including a `messages`-wire response that carries a rig catalog cost.
- **End to end** (`fontaine -p` against wiremock standing in for the Go gateway, with a stored
  oauth credential and `base_url` pointed at the mock):
  - a `kimi-k3` turn hits `/openai/v1/chat/completions`, and a `claude-haiku-5-5` turn hits
    `/anthropic/v1/messages`;
  - both carry the session id from `--session-id` in `x-opencode-session`, the Fontaine
    User-Agent and the right auth header.

  This catches plumbing (session id through `lib.rs`, provider dispatch) that unit tests against
  a fake client cannot.
- **`auth login`** (integration): against a wiremock Console, `fontaine auth login go` with stdin
  closed prints the URL and code, stores the oauth entry, and prints the email.

## Manual Verification

These need a real Go subscription, which CI cannot hold.

1. `fontaine auth login go` with `[accounts.go] kind = "opencode-go"`. Expect a URL and code;
   approving in the browser prints "signed in to OpenCode Go as <email>".
2. `fontaine -p --model go/kimi-k3 "say hi"`, then the same with `go/gpt-5.6-luna`,
   `go/claude-haiku-5-5` and `go/gemini-4-argon-contributor`. Each answers. This confirms the
   gateway accepts the Console access token on each wire, which is inferred from third-party
   clients, not documented.
3. Edit `expires_at` in `credentials.toml` to the past and rerun step 2's first command. It
   answers, and the stored `refresh` value has changed (rotation persisted).
4. Guardian scorecard candidates (`glm-5.3-flash`, `deepseek-v4.1-flash`, `mimo-v2.6-flash`):
   `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian live:go/<model>`. The
   evals harness is manual/nightly by design.

## Open Questions

- Does the Go gateway accept the Console OAuth access token on all four wires, as third-party
  clients assume? If it rejects one, that wire needs a token exchange, which is not designed here.
  Manual verification step 2 settles it.
- `x-opencode-org-id` / `x-org-id`: opencode sends `x-org-id` only to Console APIs. A
  multi-organization account might need it on gateway traffic. The org-scoped token
  (`supports_org_scope`) is expected to make it unnecessary.
