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
  the same client. rig-core 0.46.0 already supports Bearer natively:
  `AnthropicConfig.dialect.quirks.auth` is a public `wire::Auth` (`ApiKey` | `OptionalBearer`), and
  `OptionalBearer` sends only `Authorization: Bearer <key>`, with no `x-api-key`, for completions
  and model listing alike.
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

**Provider.** In `anthropic.rs` `connect`, when `account.auth == Some(Bearer)`, set
`config.dialect.quirks.auth = wire::Auth::OptionalBearer` on the `AnthropicConfig`; the connected
client type and the default path are unchanged. A missing key with `Required` credential policy is
reported before `connect`.

## Implementation Steps

1. `config.rs`: add `AnthropicAuth`, the `Account.auth` field and `Account::new` default; extend
   `check_account` with the `accepts_auth` check.
2. `providers/mod.rs`: add `accepts_auth` to the trait; `anthropic.rs` returns `true`.
3. `anthropic.rs`: set the Bearer auth quirk in `connect` as above.
4. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-core/src/providers/mod.rs`
- `rust/fontaine-core/src/providers/anthropic.rs`
- `rust/fontaine-core/tests/providers.rs`, `rust/fontaine-core/tests/models.rs`
- `docs/configuration.md`, `CHANGELOG.md`

## Trade-offs

- **A boolean `bearer = true`** instead of an enum: the issue proposes `auth = "..."`, and an enum
  leaves room for other schemes without another key.

## Decisions

- Project scope needs no new rule: `accounts` as a whole is already user/CLI scope only, which
  satisfies the issue's "same rule as the other user-scope-only network keys".
- Model-discovery failure messaging, per-host feature gating (#169) and host model-name mapping
  (#167) stay out of scope, as the issue says.

## Documentation

- `docs/configuration.md` `[accounts.<name>]` key table: add an `auth` row (`anthropic` only;
  `"x-api-key"` default or `"bearer"`; user scope only), in the "`<kind>` only: ... User scope
  only." style of its neighbours. Put a Databricks-style example among the worked provider
  examples.
- `CHANGELOG.md`: entry under unreleased.

## Testing Strategy

- `tests/providers.rs`, next to `quota_belongs_to_openai_compatible_only`: `auth = "bearer"` parses
  on `anthropic`; unknown value is an error; `auth` on `openai-compatible` (and any other kind) is
  rejected with the "not used by kind" message; absent means default.
- `tests/models.rs` (wiremock, beside `anthropic_uses_the_messages_wire_and_the_env_key`): an
  `anthropic` account with `auth = "bearer"` sends `Authorization: Bearer <key>` and no `x-api-key`
  on both a completion and the model listing; the default account still sends `x-api-key` and no
  `Authorization`. This guards the silent failure mode (auth header wrong, 401 only on that host).
- The `config_catalog` and `config_scope` schema-coverage tests stay green.

## Open Questions

None.
