# Corporate Network Support Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/6

## Overview
Claude Code works behind a corporate network: it honors `HTTPS_PROXY`/`NO_PROXY`, trusts extra CA
certificates, and presents a client certificate (mTLS). Fontaine builds its HTTP clients in four
places with no way to configure any of this beyond what reqwest reads from the environment. Add a
`[network]` section (plus per-account overrides) that feeds proxy, extra CA roots and a client
identity into every client Fontaine builds: model accounts, the guardian screen, webfetch, the
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
client_key  = "/etc/corp/client.key"     # PKCS#8 PEM, unencrypted

[accounts.work]                # each overrides the [network] value for that account only
https_proxy = "..."            # also http_proxy, no_proxy, ca_bundle, client_cert, client_key
```
- Unset keys leave reqwest's environment/system handling untouched, so existing `HTTPS_PROXY`
  users see no change. A set `https_proxy`/`http_proxy` replaces the environment value for that
  scheme; `no_proxy` is an exception list in `HTTPS_PROXY`'s own syntax. Proxy URLs may carry
  credentials (`http://user:pass@host`), as in Claude Code.
- Claude Code's environment variables map to: `NODE_EXTRA_CA_CERTS` → `ca_bundle`,
  `CLAUDE_CODE_CLIENT_CERT`/`_KEY` → `client_cert`/`client_key`. Fontaine reads no new environment
  variables; the config file is the surface (recorded in Decisions).
- `client_cert` and `client_key` must be set together. Encrypted private keys are not supported
  (reqwest's rustls identity takes unencrypted PKCS#8); the error says so.
- Account values override `[network]` per key. `ollama`/local accounts on `localhost` keep working
  through a proxy because reqwest's env handling and `no_proxy` apply to them like any host.

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
`apply` adds the proxies, `tls_certs_merge(roots)` and `identity`. `build_client` gains a
`&LoadedNetwork` parameter and applies it inside `make`'s builder on both the first attempt and the
bundled-roots retry (retry uses `tls_certs_only(bundled)` then merges the custom roots too).
`NetworkError` is a typed enum (`Read{path,source}`, `NoCertificates{path}`, `Pem{path,source}`,
`Proxy{source}`, `HalfIdentity`) because callers surface it at config load with exit code 2.

### Wiring
- `Config` gains `network: NetworkSettings` (merged like other user-only sections) and `Account`
  gains the same six optional keys, flattened into a `NetworkSettings` field.
- Validation at config load: `LoadedNetwork` is built for `[network]` and for each account that
  overrides it; any `NetworkError` is a config error (exit 2) naming the key and file. Relative
  paths resolve against the config file's directory, like other path-valued keys (check
  `scanner_model_path` handling and follow it); `~` is not expanded.
- `transport::caused_client(network: &LoadedNetwork)` and `reqwest_client(network)` take the
  effective settings. Each provider `connect` passes `ctx.network()` (account overlaid on global,
  loaded once per account and cached on `ConnectContext`'s owner). The guardian screen passes the
  settings of the `ollama` account it targets; the device flow and opencode-go use the account's.
- `Webfetch::new` and `scanner_model::ensure` use the global `[network]` settings (no account).
- HTTP MCP: build the client with `tls::build_client` (global `[network]`), keeping
  `pool_max_idle_per_host(0)` and a copy of rmcp's same-origin 307/308 redirect policy, and pass it
  via `StreamableHttpClientTransport::with_client(client, StreamableHttpClientTransportConfig::with_uri(url))`.
  This also removes the build-time panic path. A test pins the redirect behavior to rmcp's documented one.
- Trust: `[network]` and the account keys are user-scope/`--settings` only. A project file setting
  `[network]` is a load error naming the scope (proxy and trust roots decide who can read model
  traffic; same class as `base_url`). Add the row to the per-key scope table.

### Fail-closed
- Any `NetworkError` at load stops the run before a client exists. At client build, a failure after
  settings loaded surfaces as the existing "could not build the HTTP client" error; there is no
  retry without the proxy, CA or identity.
- The guardian's endpoint is an account, so it inherits the account's network; an unusable network
  setup makes the screen stage and guardian model unavailable, which already denies (fail-closed),
  not skip. The existing bundled-roots fallback still applies only to a missing *system* store.

## Implementation Steps
1. `fontaine-tools/src/tls.rs`: add `NetworkSettings`, `LoadedNetwork`, `NetworkError`; change
   `build_client` to take `&LoadedNetwork`. Add `url`/`pem` helpers only if reqwest's
   `Certificate::from_pem_bundle` and `Identity::from_pkcs8_pem` do not suffice (they should).
2. Update `Webfetch::new` (`webfetch.rs`) and `scanner_model::ensure` to pass the loaded settings;
   thread `Config::network` into `builtins::BuiltinEnv`.
3. `fontaine-core/src/config.rs`: `[network]` section, `Account` keys, merge, user-scope-only
   enforcement, load-time validation, per-key scope table entry; update the config JSON schema in
   `docs/schemas/` if one covers config.
4. `transport.rs`, `providers/*`, `agent.rs` (screen), `device_flow.rs`: pass the effective
   `LoadedNetwork`.
5. `fontaine-mcp/src/servers.rs`: build the HTTP client through `tls::build_client`; plumb settings
   into `connect`'s caller in `fontaine-core/src/agent.rs`.
6. Docs, changelog, tests (below).

## Files to Modify
- `rust/fontaine-tools/src/{tls.rs,webfetch.rs,builtins.rs}`
- `rust/fontaine-core/src/{config.rs,transport.rs,agent.rs,scanner_model.rs,tools.rs}`
- `rust/fontaine-core/src/providers/{mod.rs,anthropic.rs,openai.rs,gemini.rs,ollama.rs,openai_compatible.rs,opencode_go.rs,device_flow.rs}`
- `rust/fontaine-mcp/src/servers.rs`
- `docs/configuration.md`, `docs/tools.md`, `docs/mcp.md`, `docs/guardian.md` (one line), `CHANGELOG.md`

## Trade-offs
- Environment variables (Claude Code's `NODE_EXTRA_CA_CERTS`, `CLAUDE_CODE_CLIENT_*`) vs config
  keys: config keys chosen because they get per-account overrides, scope enforcement and
  load-time validation; env proxies keep working via reqwest as today.
- A process-wide default client vs per-account clients: keep per-account (existing design).
- Copying rmcp's redirect policy vs using `from_uri`: copying is the only way to inject our CA and
  identity; the copy is ~15 lines and test-pinned.

## Decisions
- 2026-10-25 Encrypted client keys are out of scope (reqwest's rustls identity needs unencrypted
  PKCS#8); a passphrase key is a clear load error.
- 2026-10-25 No new environment variables; config keys only. `HTTPS_PROXY` & co. keep working through reqwest.

## Documentation
- `docs/configuration.md`: `[network]` reference, account keys, scope-table row, worked example
  (corporate proxy + internal CA + mTLS).
- `docs/tools.md`: webfetch honors `[network]`; amend the `SSL_CERT_FILE` sentence (replace vs add).
- `docs/mcp.md`: HTTP servers honor `[network]`.
- `docs/guardian.md`: the guardian endpoint inherits its account's network settings and fails closed.
- `CHANGELOG.md`: `### Added` entry under `[Unreleased]`.

## Testing Strategy
Unit tests (no live network):
- `NetworkSettings::overlaid_by` per-key override; `load` errors: missing file, non-PEM file,
  empty bundle, cert without key and key without cert, encrypted key, unparseable proxy URL.
- Config: `[network]` accepted in user and `--settings` scope, rejected in project scope; account
  keys parsed; load errors exit 2 and name the key.
- `build_client` with a custom root retried through the bundled-roots path keeps the custom root and
  identity (extend the existing retry test).
- Proxy routing with `wiremock`: a client built from `https_proxy`/`http_proxy` sends a plain-HTTP
  request to the mock "proxy" in absolute-form; `no_proxy` bypass reaches the origin directly.
- CA and mTLS against a local rustls test server (generated with `rcgen` as a dev-dependency):
  a client without the CA fails the handshake; with `ca_bundle` it succeeds; a server requiring a
  client cert rejects a client without identity and accepts one with it. This catches what a fake
  cannot: the handshake actually using the configured roots and identity.
- MCP redirect policy: same-origin 307 followed, 302 and cross-origin not (pins parity with rmcp's
  default).
- Fail-closed: a bad `[network]` never yields a client; the screen stage with an unusable network
  denies rather than skips.

## Manual Verification
1. Behind a real authenticating proxy, run `fontaine -p "say hi"` with `https_proxy` set and check
   the proxy log shows the model request. Not automated: needs a corporate proxy.

## Open Questions
- Should `ca_bundle` also accept a directory of certs (like `SSL_CERT_DIR`)? Default: file only.
