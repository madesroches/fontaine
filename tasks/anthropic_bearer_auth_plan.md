# Anthropic Bearer Auth Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/566

## Overview

The `anthropic` account kind always sends its credential as `x-api-key`. Many hosts that speak the
Messages API (Databricks AI Gateway, OpenRouter's Anthropic endpoint, LiteLLM and similar gateways)
accept only `Authorization: Bearer <token>`, which is what Claude Code's `ANTHROPIC_AUTH_TOKEN`
sends. This adds an account key `auth = "x-api-key" | "bearer"` on `anthropic` accounts. The
default stays `x-api-key`, so existing accounts are unchanged.

## Current State

- `fontaine-core/src/providers/anthropic.rs` `connect` builds `AnthropicConfig::new(key)`, which
  makes rig put the key in `x-api-key`, and connects it over `caused_client`. `list` goes through
  the same client.
- `fontaine-core/src/transport.rs` already has `Decorated` / `Decoration` / `AuthPlacement`, used
  by `opencode_go.rs`. Its `auth` field takes a `TokenSource` (OAuth tokens with refresh), which
  does not fit a static key. `AuthPlacement::Bearer` inserts `Authorization` but leaves any
  `x-api-key` rig set.
- `Account` (`fontaine-core/src/config.rs:~362`) carries per-kind keys, each refused on other kinds
  in `check_account` (`config.rs:~1570`) through a `Provider::accepts_*` method
  (`providers/mod.rs`, e.g. `accepts_quota`). `Provider::validate` is the per-kind hook.
- `accounts` is already `Rule::user_or_cli("accounts", USER_FILE)` in `config_scope.rs`, so every
  account key, including a new one, is refused in project scope with no further rule.
- Credential resolution (`api_key_env` or the `/login` stored key) reaches the provider as
  `ConnectContext::api_key()`; it is unchanged.

## Design

**Config.** A new enum in `config.rs`:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum AnthropicAuth {
    #[default]
    #[serde(rename = "x-api-key")] XApiKey,
    #[serde(rename = "bearer")] Bearer,
}
```

`Account` gains `auth: Option<AnthropicAuth>` (doc comment: `anthropic` only), `None` in
`Account::new`. `Provider` gains `accepts_auth()` (default `false`, `true` for `anthropic`);
`check_account` rejects `auth` on other kinds with the existing "not used by kind" error shape.

**Transport.** Generalize `Decoration.auth` from `Arc<TokenSource>` to a small enum
`AuthSource { Refreshing(Arc<TokenSource>), Fixed(String) }`; `decorate` resolves the token from
either. `AuthPlacement::Bearer` also removes `x-api-key`, so rig's placeholder key cannot sit next
to the real credential (the doc comment on `Decorated` already promises this). The value is marked
sensitive like today (`token_header`). `opencode_go.rs` changes only to wrap its source in
`AuthSource::Refreshing`.

**Provider.** In `anthropic.rs` `connect`, when `account.auth == Some(Bearer)`: build the config
with an empty key, and wrap `caused_client` in `Decorated` with
`auth: Some((AuthSource::Fixed(key), AuthPlacement::Bearer))`. `Connected.client` is currently
typed `Client` (default `CausedClient`); make the connected client type cover both, either by
boxing behind `Decorated<CausedClient>` with an empty `Decoration` for the default path (the
decorate step is then a no-op) or by making `Connected` generic. Prefer the single
`Decorated<CausedClient>` type: one code path, no generic spread. A missing key with `Required`
credential policy is already reported before `connect`, so `Fixed` never holds an empty string
(an empty value is treated as an unusable-header error by `token_header`, not silently sent).

The `x-api-key` default path sends exactly what it does today (empty decoration changes nothing).

## Implementation Steps

1. `config.rs`: add `AnthropicAuth`, the `Account.auth` field and `Account::new` default; extend
   `check_account` with the `accepts_auth` check.
2. `providers/mod.rs`: add `accepts_auth` to the trait; `anthropic.rs` returns `true`.
3. `transport.rs`: add `AuthSource`, change `Decoration.auth`, make `Bearer` drop `x-api-key`;
   update `opencode_go.rs` and the transport tests' `decorated` helper for the new type.
4. `anthropic.rs`: apply the decoration in `connect` as above.
5. Check other `Account { .. }` literals and `Account` consumers (the provider dialog / account
   JSON schema output) compile and, where a schema file is checked in, regenerate it.
6. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-core/src/providers/mod.rs`
- `rust/fontaine-core/src/providers/anthropic.rs`
- `rust/fontaine-core/src/providers/opencode_go.rs`
- `rust/fontaine-core/src/transport.rs`
- `rust/fontaine-core/tests/providers.rs`, `rust/fontaine-core/tests/config.rs`
- `docs/configuration.md`, `CHANGELOG.md`

## Trade-offs

- **A `TokenSource` with no refresh** for the static key instead of `AuthSource::Fixed`: reuses the
  type but forces a fake `OAuthTokens`, expiry and refresher onto a plain string. Rejected.
- **A generic `Connected<C>`** instead of one `Decorated<CausedClient>`: avoids an empty decoration
  on the default path but spreads a type parameter through the registry. Rejected for one cheap
  no-op.
- **A boolean `bearer = true`** instead of an enum: the issue proposes `auth = "..."`, and an enum
  leaves room for other schemes without another key.

## Decisions

- Project scope needs no new rule: `accounts` as a whole is already user/CLI scope only, which
  satisfies the issue's "same rule as the other user-scope-only network keys".
- Model-discovery failure messaging, per-host feature gating (#169) and host model-name mapping
  (#167) stay out of scope, as the issue says.

## Documentation

- `docs/configuration.md` `[accounts.<name>]` table: add `auth` (`anthropic` only; `"x-api-key"`
  default or `"bearer"`; user scope only), with a Databricks-style example.
- `CHANGELOG.md`: entry under unreleased.

## Testing Strategy

- Unit (`config` tests): `auth = "bearer"` parses on `anthropic`; unknown value is an error;
  `auth` on `openai-compatible` (and any other kind) is rejected with the "not used by kind"
  message; absent means default; a project-scope file setting `auth` is refused (via `accounts`).
- Unit (`transport.rs`): `Bearer` with a fixed source sets `Authorization: Bearer <key>`, removes a
  pre-set `x-api-key`, and marks the header sensitive; an unsendable value (newline) errors
  without echoing the key.
- Integration (`tests/providers.rs`, wiremock): an `anthropic` account with `auth = "bearer"`
  sends `Authorization: Bearer <key>` and no `x-api-key` on both a completion and the model
  listing; the default account still sends `x-api-key` and no `Authorization`. This guards the
  silent failure mode (auth header wrong → 401 only on that host).
- Existing `opencode_go` tests stay green, proving the `AuthSource` change is behavior-neutral.

## Open Questions

None.
