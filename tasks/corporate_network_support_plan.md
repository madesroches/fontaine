# Corporate Network Support Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/6

## Overview
Claude Code works behind a corporate network: it honors `HTTPS_PROXY`/`NO_PROXY`, trusts extra CA
certificates, and presents a client certificate (mTLS). Fontaine builds its HTTP clients in four
places with no way to configure any of this beyond what reqwest reads from the environment. Add a
`[network]` section (plus per-account overrides) that feeds proxy, extra CA roots and a client
identity into the clients Fontaine builds for: model accounts, the guardian screen, webfetch, the
scanner-model download, the opencode-go sign-in and HTTP MCP servers. An invalid setting fails
closed: the client is not built and nothing falls back to direct or default-trust traffic.

## Current State
- `reqwest` is pinned with `rustls` and `system-proxy` (`rust/Cargo.toml:70`), so
  `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY` (and the OS proxy settings on macOS/Windows)
  are already honored by every reqwest client. Nothing documents it, nothing tests it, and there
  is no config key.
- `fontaine_tools::tls::build_client(make)` (`fontaine-tools/src/tls.rs`) is the one place that
  retries with bundled Mozilla roots when the system store fails to load. Callers: `Webfetch::new`
  (`webfetch.rs:55`), `transport::reqwest_client` (`fontaine-core/src/transport.rs:129`),
  `scanner_model::ensure` (`scanner_model.rs:92`).
- `transport::caused_client()` is called once per account by each provider's `connect`
  (`providers/{anthropic,openai,gemini,ollama,openai_compatible,opencode_go}.rs`), which receives a
  `ConnectContext` holding the `Account`. `reqwest_client()` is also used for the guardian screen
  (`agent.rs:197`) and the opencode-go device flow (`device_flow.rs:136`, `opencode_go.rs:327`).
- HTTP MCP servers use `StreamableHttpClientTransport::from_uri` (`fontaine-mcp/src/servers.rs:226`),
  which builds rmcp's own client (`pool_max_idle_per_host(0)` and a private same-origin-307/308
  redirect policy) and can panic when the build fails. It gets the env proxy but no extra CA or identity.
- `SSL_CERT_FILE`/`SSL_CERT_DIR` already replace the system roots through the platform verifier
  (`docs/tools.md:125`); there is no way to *add* roots.
- `Account` (`config.rs:312`) is `deny_unknown_fields`; accounts are user/CLI scope only. Loosening
  keys are user-scope only (`docs/configuration.md` "Trust asymmetry").

## Design

### Settings
```toml
[network]                      # user scope and --settings only
https_proxy = "http://proxy.corp:8080"   # also: http_proxy
no_proxy    = "localhost,.corp.internal"
ca_bundle   = "/etc/corp/ca.pem"         # PEM, one or more certs; ADDED to the system roots
client_cert = "/etc/corp/client.pem"     # PEM certificate chain
client_key  = "/etc/corp/client.key"     # unencrypted PEM key

[accounts.work]                # each overrides the [network] value for that account only
https_proxy = "..."            # also http_proxy, no_proxy, ca_bundle, client_cert, client_key
```
- With no proxy key set, reqwest's environment/system handling is untouched, so existing
  `HTTPS_PROXY` users see no change. Any configured proxy key (`https_proxy`, `http_proxy`,
  `no_proxy`) turns off environment and system proxy handling for that client (reqwest's
  `ClientBuilder::proxy()` disables it: `HTTP_PROXY`, `ALL_PROXY`, env `NO_PROXY`, macOS/Windows
  system proxy); `no_proxy` is an exception list in `HTTPS_PROXY`'s own syntax. Proxy URLs may carry
  credentials (`http://user:pass@host`), as in Claude Code.
- `client_cert` and `client_key` must be set together. Encrypted private keys are not supported
  (reqwest's rustls identity takes an unencrypted PEM key); a pre-check for an `ENCRYPTED PRIVATE KEY`
  PEM label gives a clear error, since reqwest itself only reports "No valid certificate".
- Account values override `[network]` per key. `ollama`/local accounts on `localhost` bypass
  the proxy only if `no_proxy` lists them.

### Types
In `fontaine-tools/src/tls.rs` (lowest crate that builds clients, so `fontaine-mcp` and
`fontaine-core` share it):
```rust
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkSettings {
    pub https_proxy: Option<String>, pub http_proxy: Option<String>, pub no_proxy: Option<String>,
    pub ca_bundle: Option<PathBuf>, pub client_cert: Option<PathBuf>, pub client_key: Option<PathBuf>,
}
impl NetworkSettings {
    pub fn overlaid_by(&self, over: &NetworkSettings) -> NetworkSettings // per-key override
    pub fn load(&self) -> Result<LoadedNetwork, NetworkError>  // reads files, parses PEM, validates once
}
pub struct LoadedNetwork { proxies: Vec<reqwest::Proxy>, roots: Vec<Certificate>, identity: Option<Identity> }
impl LoadedNetwork { pub fn apply(&self, builder: ClientBuilder) -> ClientBuilder }
```
`apply` adds the proxies, `tls_certs_merge(roots)` and `identity`. `load` builds the identity by
reading `client_cert` and `client_key`, joining them with a newline and calling
`Identity::from_pem` (the rustls-backend API; `from_pkcs8_pem` is native-tls only). `build_client` gains a
`&LoadedNetwork` parameter and applies it inside `make`'s builder on both the first attempt and the
bundled-roots retry (retry uses `tls_certs_only(bundled)` then merges the custom roots too); the bundled-roots fallback still applies only to a missing system store.
`NetworkError` is a typed enum (`Read{path,source}`, `NoCertificates{path}`, `Pem{path,source}`,
`Proxy{source}`, `HalfIdentity`) because callers surface it at config load with exit code 2.

### Wiring
- `NetworkSettings` has no serde and `Account` is `deny_unknown_fields` (incompatible with
  `flatten`), so `Account` declares the six optional keys directly and `fontaine-core` adds a
  `NetworkSection` (`Deserialize`, `deny_unknown_fields`) for `[network]`; both convert to
  `NetworkSettings` via a method. `Config` gains the `[network]` section, merged per key via
  `overlaid_by` (user scope, with `--settings` overriding user scope).
- Validation at config load: `LoadedNetwork` is built for `[network]` and for each account that
  overrides it; any `NetworkError` is a config error (exit 2) naming the key and file. Relative
  paths resolve against the process cwd, like `scanner_model_path`; `~` is not expanded.
- `Config` holds only the plain `NetworkSettings` for `[network]` (it derives `Clone, Debug,
  PartialEq`, `config.rs:1195`; `LoadedNetwork` wraps reqwest types that are none of these, so it is
  never a `Config` field). Accounts are added and removed at runtime (`agent.rs:1090-1105`,
  `setup.rs`), so an account's network is resolved by lookup when needed: the global settings
  overlaid by the account's own keys (global alone if the account is absent), then `load()`ed on
  demand. `Models` (which holds `self.config`) does this at connect time and passes the
  `&LoadedNetwork` as a new parameter of `client_for` (`models.rs:264`, whose `Sources` holds no
  `Config`); `client_for` stores it in `ConnectContext` (borrow-only, built at `models.rs:289`) as
  `network: &LoadedNetwork`. The opencode-go sign-in (`opencode_go.rs:327`, which has no
  `ConnectContext`) takes the account's `&LoadedNetwork` as an explicit parameter. The global
  `LoadedNetwork` is loaded once at startup in `agent.rs` and held as an `Arc<LoadedNetwork>` by
  the consumers that outlive a call (`BuiltinEnv`, the MCP connect path).
- `transport::caused_client(network: &LoadedNetwork)` and `reqwest_client(network)` take the
  effective settings. Each provider `connect` passes `ctx.network`. The guardian screen passes the
  settings of the `ollama` account it targets (if that account is absent from config, the global
  `[network]`); the device flow and opencode-go use the account's.
- `Webfetch::new` and `scanner_model::ensure` use the global `[network]` settings (no account).
- HTTP MCP: build the client with `tls::build_client` (global `[network]`), keeping
  `pool_max_idle_per_host(0)` and a copy of rmcp's same-origin 307/308 redirect policy, and pass it
  via `StreamableHttpClientTransport::with_client(client, StreamableHttpClientTransportConfig::with_uri(url))`.
  A test pins the redirect behavior to rmcp's documented one.
  `servers.rs` HTTP connect currently gets only `spec`, so the global `&LoadedNetwork` is passed in
  from its caller in `agent.rs`; `fontaine-mcp` gets `reqwest.workspace = true`.
- Trust: `[network]` and the account keys are user-scope/`--settings` only. `[network]` is rejected
  beside `accounts` in the scope check, before the ProjectLocal early return, so both project and
  project-local files error, naming the scope (proxy and trust roots decide who can read model
  traffic; same class as `base_url`). Add the row to the per-key scope table, with the same values
  as `[accounts.*]` (no/no/yes), for both `[network]` and the account network keys.

## Implementation Steps
1. `fontaine-tools/src/tls.rs`: add `NetworkSettings`, `LoadedNetwork`, `NetworkError`; change
   `build_client` to take `&LoadedNetwork`. `Certificate::from_pem_bundle` covers the CA bundle; the
   identity needs our own step: join cert and key PEM and call `Identity::from_pem`, with the
   `ENCRYPTED PRIVATE KEY` pre-check.
2. Update `Webfetch::new` (`webfetch.rs`; called at `builtins.rs:391`) and `scanner_model::ensure` to pass
   the loaded settings. The network reaches webfetch as a new `BuiltinEnv` field
   (`fontaine-tools/src/builtins.rs:303`), set along `agent.rs:1375` `tools::registry(RegistryParams{..})`
   -> `fontaine-core/src/tools.rs:96-106` -> `BuiltinEnv`; `RegistryParams` gains the field.
3. `fontaine-core/src/config.rs`: `[network]` section, `Account` keys, merge, user-scope-only
   enforcement, load-time validation, per-key scope table entry (no/no/yes, same as `[accounts.*]`).
4. `transport.rs`, `providers/*` (add `network` to `ConnectContext`, built at `models.rs:289`),
   `opencode_go.rs` sign-in, `agent.rs` (screen), `device_flow.rs`: pass the effective
   `LoadedNetwork`. Call sites carrying the account's `&LoadedNetwork`: `GoRefresher`
   (`opencode_go.rs:236-246`) -> `DeviceFlow::new` -> `reqwest_client()` (`device_flow.rs:136`),
   `SignIn::sign_in` (`providers/mod.rs:120`), `accounts::sign_in` (`accounts.rs:261`) and its
   callers in `fontaine-tui/src/lib.rs:1215` and `setup.rs:574`; `GoRefresher` (a unit struct)
   gains a network field,
   `scanner_model::ensure`'s caller (`scanner_stage.rs:234`), plus the tests
   `tests/{token_source,device_flow,account_ops}.rs`,
   `fontaine-tools/tests/{webfetch,result_pipeline,common/mod}.rs`,
   `fontaine-core/tests/scanner_model.rs`, the `RegistryParams` literals in
   `fontaine-core/tests/{extensions,choke_point}.rs` (`extensions.rs:315`, `choke_point.rs:82`), `fontaine-mcp/tests/mcp_registration.rs` and the
   `tls.rs` unit tests. Fix the `Account` struct literals at `accounts.rs:217`, `user_config.rs:334` and
   `tests/account_session.rs:193`.
5. `fontaine-mcp/src/servers.rs` and `fontaine-mcp/Cargo.toml` (`reqwest.workspace = true`): build
   the HTTP client through `tls::build_client`; plumb settings into `connect`'s caller in
   `fontaine-core/src/agent.rs`.
6. Docs, changelog, tests (below).

## Files to Modify
- `rust/fontaine-tools/src/{tls.rs,webfetch.rs,builtins.rs}`
- `rust/fontaine-core/src/{config.rs,transport.rs,agent.rs,scanner_model.rs,tools.rs,models.rs,accounts.rs,user_config.rs}`
- `rust/fontaine-core/src/providers/{mod.rs,anthropic.rs,openai.rs,gemini.rs,ollama.rs,openai_compatible.rs,opencode_go.rs,device_flow.rs}`
- `rust/fontaine-core/src/scanner_stage.rs`
- `rust/fontaine-tui/src/{lib.rs,setup.rs}`
- `rust/fontaine-core/tests/{account_session,token_source,device_flow,account_ops,scanner_model,extensions,choke_point}.rs`
- `rust/fontaine-tools/tests/{webfetch.rs,result_pipeline.rs,common/mod.rs}`, `rust/fontaine-mcp/tests/mcp_registration.rs`
- `rust/fontaine-mcp/src/servers.rs`, `rust/fontaine-mcp/Cargo.toml`
- `docs/configuration.md`, `docs/tools.md`, `docs/mcp.md`, `docs/guardian.md` (one line), `CHANGELOG.md`, `research/research-claude-code-parity.md`

## Decisions
- 2026-10-25 Encrypted client keys are out of scope (reqwest's rustls identity needs an unencrypted
  PEM key); a passphrase key is a clear load error.
- 2026-10-25 `ca_bundle` is a single PEM file; directories are unsupported (`SSL_CERT_DIR` still works).
- 2026-10-25 No new environment variables; config keys only. `HTTPS_PROXY` & co. keep working through reqwest.
- 2026-10-25 The eval harness (`fontaine-evals/src/screen.rs:93`, manual, not a CI gate) and telemetry
  OTLP exporters are not covered by `[network]`.

## Documentation
- `docs/configuration.md`: `[network]` reference, account keys, scope-table row, worked example
  (corporate proxy + internal CA + mTLS). With only `https_proxy` set, plain-http local accounts
  (e.g. ollama on localhost) go direct; with `http_proxy` set they go through the proxy unless
  `no_proxy` lists them.
- `docs/configuration.md` merge rules (lines 61-67 say named table entries are "never merged field
  by field"): amend for the `[network]` per-key overlay.
- `docs/tools.md`: webfetch honors `[network]`; amend the `SSL_CERT_FILE` sentence (replace vs add).
- `docs/mcp.md`: HTTP servers honor `[network]`.
- `docs/guardian.md`: the guardian endpoint inherits its account's network settings; an invalid
  `[network]` stops startup with exit 2.
- `CHANGELOG.md`: `### Added` entry under `[Unreleased]`.
- `research/research-claude-code-parity.md:282`: update the stale backlog row for proxy / custom CA / mTLS.

## Testing Strategy
Unit tests (no live network):
- `NetworkSettings::overlaid_by` per-key override; `load` errors: missing file, non-PEM file,
  empty bundle, cert without key and key without cert, encrypted key, unparseable proxy URL.
- Config: `[network]` accepted in user and `--settings` scope, rejected in project scope; account
  keys parsed; load errors exit 2 and name the key.
- `build_client` with a custom root retried through the bundled-roots path keeps the custom root and
  identity: assert `apply` was invoked on both attempts (the existing test forces the failure via a
  bad user agent, so it cannot observe the roots themselves).
- Proxy routing with `wiremock`: a client built from `https_proxy`/`http_proxy` sends a plain-HTTP
  request to the mock "proxy" in absolute-form; `no_proxy` bypass reaches the origin directly.
  A small `tokio::net::TcpListener` CONNECT fake in front of the rcgen TLS server asserts the
  `Proxy-Authorization` header derived from credentials in the proxy URL and a tunnelled https request.
- CA and mTLS against a local rustls test server (generated with `rcgen` as a dev-dependency):
  a client without the CA fails the handshake; with `ca_bundle` it succeeds; a server requiring a
  client cert rejects a client without identity and accepts one with it. This catches what a fake
  cannot: the handshake actually using the configured roots and identity. New dev-dependencies are
  exact pins (workspace convention) in `[workspace.dependencies]` and dev-dependencies of
  `fontaine-tools`: `rcgen =0.14.10`, `tokio-rustls =0.26.6`, `rustls =0.23.46`.
- MCP redirect policy: same-origin 307 followed; 302 and cross-origin redirect responses are
  returned unfollowed (rmcp's policy calls `attempt.stop()`).
- Fail-closed: a bad `[network]` never yields a client.

## Manual Verification
1. Smoke check against a real corporate proxy: run `fontaine -p "say hi"` with `https_proxy` set and
   check the proxy log shows the model request. Not automated: needs a corporate proxy.
