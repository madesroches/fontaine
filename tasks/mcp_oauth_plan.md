# MCP OAuth for Remote Servers Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/32

## Overview

Authenticate to remote (streamable HTTP) MCP servers with OAuth 2.1, using rmcp 3.9.0's built-in
auth stack, with tokens persisted owner-only beside the account credentials. Ships in the same
pass: static `headers` on HTTP server declarations (already parsed from `.mcp.json` but dropped
today) and `headersHelper` scripts restricted to trusted scopes.

## Current State

- **Declaration** — `McpServer` (`rust/fontaine-core/src/config.rs:988`) has `command`, `args`,
  `env_vars`, `env`, `url`; `#[serde(deny_unknown_fields)]` means a `headers` key in a settings
  TOML is a parse error. `.mcp.json` entries with `headers` are parsed but warned about and
  dropped (`rust/fontaine-core/src/mcp_json.rs:103`). CLI stubs `reject_header` /
  `reject_oauth` (`rust/fontaine/src/cli.rs:216-222`) make `-H/--header` and
  `--client-id/--client-secret/--callback-port` fail with "not supported yet (see issue #32)".
- **Transport** — `fontaine-tools/src/mcp/servers.rs` builds one reqwest client per HTTP server
  (`http_client`, line 314, applying `[network]` proxy/CA/client-cert via `tls::build_client`)
  and hands it to `StreamableHttpClientTransport::with_client` (line 399). No headers, no auth;
  a 401 is a permanent failure (`McpErrorKind::Permanent`, line ~694 tests).
- **rmcp auth stack (feature `auth`, not enabled)** — `rmcp::transport::auth` provides
  `AuthorizationManager` (protected-resource + authorization-server metadata discovery),
  `AuthClient<C: OAuthHttpClient>` which implements `StreamableHttpClient` and drives
  authorization lazily from 401 `WWW-Authenticate` challenges (surfacing
  `StreamableHttpError::AuthRequired` when no credentials exist),
  `CredentialStore` / `StateStore` traits (in-memory impls only ship in rmcp), PKCE +
  loopback-redirect authorization sessions (`AuthorizationRequest`, `AuthorizationSession`),
  and `WWWAuthenticateParams` for challenge parsing.
- **Credentials** — `rust/fontaine-core/src/credentials.rs` writes `credentials.toml` in the
  data dir owner-only (0600, `owner_only::write_atomic`), with a cross-process
  `credentials.lock`; today it holds `accounts`, `telemetry`, `websearch` entries.
- **Surfaces** — `fontaine mcp add|list|get|remove` (`rust/fontaine/src/mcp_cmd.rs`); TUI `/mcp`
  lists servers and reconnects (`rust/fontaine-tui/src/commands.rs:51`), no login concept.
- **Docs** — `docs/mcp.md` states "Nothing is sent to a streamable HTTP server but the
  protocol's own requests; Fontaine adds no credentials of its own."

## Design

### 1. Configuration

`McpServer` gains two optional fields:

```rust
pub headers: Option<BTreeMap<String, String>>,
pub headers_helper: Option<String>, // path to an executable
```

- `headers` on a `command` (stdio) server is a validation error in `check_mcp_server`
  (`config.rs:1695`), matching the existing args/env rule for `url` servers. Values may carry
  `${VAR}` references; expansion happens at connect time against Fontaine's scrubbed
  environment (the same expander used for repo `.mcp.json` definitions), so secrets stay out of
  the config file.
- `.mcp.json` parsing (`mcp_json.rs`) maps `headers` → `headers` and `headersHelper` →
  `headers_helper`; the "not supported yet (#32)" warnings are removed.
- CLI `mcp add` unhides `-H/--header KEY=VALUE` (repeatable) and stores headers in the
  declaration; `--client-id`/`--client-secret`/`--callback-port` are removed rather than
  implemented (see Trade-offs).

### 2. Token storage

`credentials.toml` gains a top-level `[mcp]` table:

```toml
[mcp."github"]
credentials = "<JSON: rmcp StoredCredentials>"
state = "<JSON: rmcp StoredAuthorizationState>"
```

- New `mcp: BTreeMap<String, McpEntry>` field on the private `File` struct in
  `credentials.rs`, where `McpEntry` holds the two opaque JSON strings. rmcp's types are
  versioned by the pinned crate; storing them as JSON strings keeps `credentials.toml` parsing
  decoupled from rmcp and lets a newer rmcp migrate on read.
- A new `rust/fontaine-tools/src/mcp/auth.rs` implements rmcp's `CredentialStore` and
  `StateStore` traits over this table, going through `credentials::lock()` for read-modify-write
  so a concurrent `fontaine mcp login` and an agent refresh never clobber each other (the same
  rotation hazard `TokenSource` handles for provider OAuth). Writes reuse
  `owner_only::write_atomic` (0600).
- Keying is the server **name**, matching `[mcp.servers]` config; tokens therefore follow the
  existing per-scope declaration merge (a project reusing a user-scope server's name would
  pick up its token — acceptable, since the token only goes to that server's URL; noted in
  Decisions).

### 3. Transport wiring

In `fontaine-tools/src/mcp/servers.rs`:

- New `ServerSpec`/`Transport::Http` payloads: `headers` (expanded map), `headers_helper`,
  `oauth: bool` derived from config presence of the server in `[mcp]` having had a login, or
  simply always-on for HTTP servers (see below).
- An `OAuthHttpClient` adapter over the same reqwest client built by `http_client(network)` so
  token/metadata/authorize HTTP goes through the user's proxy and CA settings.
- For every HTTP server, build `AuthorizationManager::new(url)` with the file-backed stores,
  then `AuthClient::new(adapter, manager)` and pass it to
  `StreamableHttpClientTransport::with_client`. rmcp's design is lazy: requests go out
  unauthenticated when no usable token exists and the server's 401 challenge drives
  authorization, so always wrapping costs nothing for open servers.
- When the server has declared `headers`, they are applied on the underlying reqwest client
  (default headers) so they ride every protocol request — independent of OAuth.
- `headers_helper`: when set, the executable is run at connect time, its stdout parsed as
  `KEY: VALUE` lines, and those headers merged in. It runs **only when the declaration's scope
  is trusted** — local (`mcp-local.toml`) or user (`config.toml`) scope. A project-scope
  declaration (repo `.mcp.json`, `.fontaine/settings.toml`) with `headers_helper` is refused at
  validation time with a message naming the scope: a repo must not be able to make Fontaine
  execute a script just by being opened. (This is the "headersHelper runs only for trusted
  scopes" requirement.)
- Supervisor reconnection (`mcp_supervisor.rs`) treats `AuthRequired` as permanent, with the
  error message naming `fontaine mcp login <name>`.

### 4. The login flow — `fontaine mcp login <name>`

A new `McpAction::Login` / `Logout` in `rust/fontaine/src/cli.rs` / `mcp_cmd.rs`:

1. Resolve the server (must be `url`), build `AuthorizationManager` with the file-backed
   stores, `configure_client(OAuthClientConfig)` with a loopback redirect URI on a fixed
   callback port (Claude Code uses a fixed port; pick one, e.g. 3111, with
   `AuthorizationRequest::new(redirect_uri)`), PKCE on by default in rmcp.
2. Print the authorize URL, open the user's browser (`open` crate equivalent — check
   `fontaine-tui`/`fontaine` for an existing opener; else print the URL and let the user open
   it), run the loopback callback listener until the redirect arrives or a timeout (5 min).
3. Store the resulting `StoredCredentials` + state through the locked store.
4. Headless `-p` sessions never run the flow: an unauthenticated server fails with
   "authentication required; run `fontaine mcp login <name>`". This matches the existing
   user-interaction posture (`mcp_requires_user_interaction_plan.md`): no hidden prompts in
   headless runs.
5. `logout` clears the server's `[mcp]` entry (and would delete a dynamic client registration
   if rmcp stored one — it does not persist registration, so clearing tokens suffices).

The TUI `/mcp` listing shows an `auth` column (ok / required / n/a) driven by whether the
server is HTTP and whether tokens exist; reconnect after a CLI login picks them up.

### 5. Guardian / floor posture

No permission-pipeline changes: MCP tools are already guardian-gated at registration and call
time. The new executable surface (`headers_helper`) is scope-gated as above and runs at connect
time outside the model's reach — the model can trigger a reconnect of a trusted-scope server
but cannot point the helper anywhere new. Documented in `docs/mcp.md`.

## Implementation Steps

1. Enable rmcp's `auth` feature in `rust/Cargo.toml` (add `"auth"` to the pinned feature list;
  verify what it pulls in — `dep:oauth2` and likely a reqwest-based default client we will not
  use).
2. `config.rs`: `headers` / `headers_helper` fields on `McpServer`; `check_mcp_server` rules
   (headers/helper on `url` servers only; helper requires trusted scope — needs the origin
   available at validation; thread it from `mcp_config` layer merge or validate at
   `server_spec()` time in `agent/mcp.rs`, which already knows origins).
3. `mcp_json.rs`: parse `headers` / `headersHelper`; drop the #32 warnings.
4. `credentials.rs`: `[mcp]` table, `McpEntry`, public API `mcp_get`/`mcp_put`/`mcp_remove`
   under the existing lock.
5. `fontaine-tools/src/mcp/auth.rs`: `CredentialStore` + `StateStore` impls; OAuthHttpClient
   reqwest adapter honoring `[network]`.
6. `servers.rs`: thread headers/helper/oauth through `ServerSpec` and `start()`; build
   `AuthClient` for HTTP servers; helper execution + header merge; `AuthRequired` error text.
7. `cli.rs` / `mcp_cmd.rs`: `login` / `logout` actions; unhide `-H/--header`; remove the
   `--client-id/--client-secret/--callback-port` stubs.
8. TUI `ui/mcp.rs`: auth status column.
9. Docs: `docs/mcp.md` (auth section, headers, headersHelper trust rule), `docs/configuration.md`
   (credentials schema + validation rules).

## Files to Modify

- `rust/Cargo.toml` (rmcp feature)
- `rust/fontaine-core/src/config.rs`, `mcp_json.rs`, `credentials.rs`, `agent/mcp.rs`
- `rust/fontaine-tools/src/mcp.rs`, `mcp/servers.rs`, new `mcp/auth.rs`
- `rust/fontaine/src/cli.rs`, `mcp_cmd.rs`
- `rust/fontaine-tui/src/ui/mcp.rs`
- `docs/mcp.md`, `docs/configuration.md`

## Trade-offs

- **rmcp's auth stack vs hand-rolled OAuth.** rmcp 3.9.0 ships exactly the pieces needed
  (metadata discovery, PKCE, loopback redirect, challenge-driven lazy auth, pluggable stores)
  and is already the pinned transport dependency. Hand-rolling would duplicate it and drift
  from MCP spec updates. Chosen: rmcp, with Fontaine owning persistence, the network policy,
  and the trust gating.
- **`--client-id/--client-secret/--callback-port` stubs removed rather than implemented.**
  Claude Code's fixed client credentials are Anthropic-operated infrastructure; Fontaine has no
  equivalent, so client registration/metadata discovery (rmcp's default) is the honest path.
  The flags never worked, so removal loses nothing; `-H/--header` is unhidden because headers
  ship here.
- **Tokens in `credentials.toml` vs a separate file.** Beside-the-credentials placement is what
  the issue asks for, reuses the lock and owner-only writer for free, and keeps
  `fontaine doctor`-style tooling with one file to check. A separate file would only isolate a
  parse failure, which `credentials.rs` already treats conservatively (never overwrite).
- **Always wrap HTTP transports in `AuthClient`** rather than a per-server `oauth = true` flag:
  rmcp's lazy design makes the wrapper inert for open servers and automatic for challenged
  ones, matching Claude Code's "401 starts the flow" UX; an opt-in flag would be one more
  config concept that can be wrong.
- **CLI-first login, TUI shows status.** The flow needs a browser and a loopback listener —
  identical machinery in TUI and CLI, but the CLI version composes with headless setups and
  tmux SSH sessions. TUI-driven login can layer on later without storage changes.

## Decisions

(none yet — settled outcomes land here during review)

## Documentation

- `docs/mcp.md` — new "Authentication" section: OAuth flow, `fontaine mcp login/logout`,
  header declaration + `${VAR}` expansion, the headersHelper trusted-scope rule, token storage
  location and permissions. Replace the "adds no credentials of its own" sentence.
- `docs/configuration.md` — credentials file schema gains `[mcp]`; validation rules for
  `headers`/`headers_helper`.
- `docs/tui.md` — `/mcp` listing's auth column.

## Testing Strategy

- **Unit** (`config.rs`, `mcp_json.rs`): header/helper parsing, unknown-key behavior,
  stdio-server-with-headers rejection, project-scope helper rejection.
- **Unit** (`credentials.rs`): `[mcp]` round-trip, lock behavior, malformed-JSON entry
  never clobbers the file.
- **Unit** (`auth.rs`): store trait impls against a temp data dir; adapter maps rmcp's
  `OAuthHttpRequest` onto reqwest including redirect policy and `[network]` proxy env.
- **Integration** (`fontaine-tools/tests/mcp_common` wiremock server): full OAuth dance —
  wiremock serves protected-resource metadata, authorization-server metadata, 401 with
  `WWW-Authenticate`, token endpoint; `CredentialStore` pre-seeded with a token → request
  carries `Authorization: Bearer`; no token → `AuthRequired` surfaces with the login hint;
  static headers and helper-output headers arrive at the server; helper not executed when
  scope is untrusted.
- **Integration** (`fontaine/tests/mcp_cmd.rs`): `login` against the wiremock auth server with
  a scripted browser callback (drive the loopback redirect with a reqwest GET instead of a
  browser); `logout` clears the entry; `mcp add -H` writes headers into the declaration.
- No live-API tier: the wiremock server implements the same metadata documents a real server
  serves, so nothing in the flow needs a real provider; the one thing it cannot catch is a
  provider quirk in metadata discovery, covered by the manual step below.

## Manual Verification

1. `fontaine mcp add github --url https://api.githubcopilot.com/mcp/` then
   `fontaine mcp login github` — browser opens, authorize, tools list in
   `fontaine mcp list`. Not automated: requires a real GitHub OAuth app round-trip and a human
   browser; breakage would be obvious (login fails immediately).
2. `ls -l ~/.local/share/fontaine/credentials.toml` shows 0600 and a `[mcp."github"]` entry.
3. Re-run `fontaine mcp list` after `fontaine mcp logout github` — server shows
   authentication required.

## Open Questions

- Which callback port? Claude Code's exact port is undocumented in the issue; pick an
  unregistered-looking default (3111) — the review pass may settle this from the parity docs.
- Should `fontaine mcp list` attempt a probe connection (current behavior) or trust stored
  tokens and skip the network probe for authenticated servers? Deferred: keep current probing.
