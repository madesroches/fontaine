# Kimi Account Type Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/548

## Overview

Add a first-class `kimi` account kind for Kimi for Coding (the Kimi Code subscription:
flat-rate, `https://api.kimi.com/coding/v1`, subscription quota at `GET /usages`), so a user
writes `kind = "kimi"` instead of `kind = "openai-compatible"` plus `base_url` plus
`quota = "kimi"`. The moonshot.ai pay-as-you-go API is a different product and stays on
`openai-compatible`; the existing `openai-compatible` + `quota = "kimi"` configuration keeps
working unchanged.

## Current State

- The provider registry is one `Provider` impl per module plus one line in `PROVIDERS`
  (`rust/fontaine-core/src/providers/mod.rs:65-76`); `accounts::kinds()`
  (`rust/fontaine-core/src/accounts.rs:370`) and the settings schema derive from it, so a new
  kind is picked up by the provider dialog, `/login`, model resolution and the settings JSON
  schema automatically.
- Kimi for Coding is configured today as `kind = "openai-compatible"` with
  `base_url = "https://api.kimi.com/coding/v1"` and `quota = "kimi"`
  (`docs/configuration.md:204-218`). The quota opt-in is the only `QuotaKind` variant
  (`rust/fontaine-core/src/config.rs:355-360`).
- `openai_compatible.rs` speaks one gateway Chat Completions dialect
  (`GATEWAY = Dialect::gateway("openai-compatible", "", "")`), takes an optional bearer key,
  and — only when `quota = "kimi"` is set — attaches `KimiQuota`
  (`rust/fontaine-core/src/providers/kimi_quota.rs`), which reads `<base_url>/usages` and
  parses the `limit_*` windows. The same module fetches the fuller `/models` entry
  (`ModelsFetcher`, `parse_served`) for `served_model`. Both helpers are private to the module
  and built only when the account sets `base_url`.
- Kinds that refuse an account key (`anthropic`, `openai`, `gemini`, `openrouter`) fail at
  startup; `openai-compatible` and `ollama` send no `Authorization` header when the account
  has no key (`docs/configuration.md:220-229`). `check_account`
  (`rust/fontaine-core/src/config.rs:1555`) already rejects keys a kind does not accept via
  the `Provider` flags, including `quota` on kinds with `accepts_quota() == false`.
- `Billing::Unpriced` (`rust/fontaine-core/src/providers/mod.rs:78-81`) drops any cost a
  response carries; today only `claude-subscription` and `opencode-go` (the flat-rate kinds)
  set it. `openai-compatible` stays `Billing::Catalog`.
- The status bar segment label is the quota source's name (`"kimi"`,
  `rust/fontaine-tui/src/statusbar.rs:565`) and renders identically for any account whose
  client returns a `KimiQuota`.

## Design

**New module `rust/fontaine-core/src/providers/kimi.rs`** implementing `Provider`:

```text
name()                  -> "kimi"
credential()            -> CredentialPolicy::Optional   // key sent when present, as today
billing()               -> Billing::Unpriced            // flat-rate subscription
base_url_policy()       -> BaseUrlPolicy::Fixed         // one endpoint; the dialog does not ask
default_endpoint()      -> Some("https://api.kimi.com/coding/v1")  // the international
                        // service; the .cn endpoint is out of scope
accepts_quota()         -> false                        // built in; a `quota` key is refused
```

`validate` needs no account-shape checks: `base_url` is not required (the default endpoint
applies, as for `anthropic`/`openai`) and every other key is gated generically by
`check_account`. The generic rule that rejects `accounts.<name>.quota` on kinds that do not
accept it already produces the right error for `kind = "kimi"`.

`connect` mirrors `openai_compatible::connect` with two differences: the endpoint is
`ctx.base_url().or_else(|| self.default_endpoint())` (never `None`, so the quota and
models fetchers always build), and the `KimiQuota` is attached unconditionally — the quota
display is a property of the kind, like `claude-subscription`'s plan usage, not an opt-in.
A keyless account sends no `Authorization` header to either the completions or the usages
endpoint, exactly as `KimiQuota` already supports. `Billing::Unpriced` makes the flat-rate
account's token cost reporting consistent with the other subscription kinds.

**Sharing with `openai-compatible`.** `GATEWAY`, `ModelsFetcher` and `parse_served` move out
of `openai_compatible.rs` into a new `pub(crate)` module `rust/fontaine-core/src/providers/gateway.rs`,
which both kinds use; the quota wiring (`KimiQuota`, `REQUEST_TIMEOUT`) stays in
`kimi_quota.rs` as today. `openai-compatible` keeps its current behavior exactly — optional
key, required `base_url`, `quota = "kimi"` opt-in, `Billing::Catalog` — so existing
configurations, including generic Chat Completions gateways and the moonshot.ai
pay-as-you-go API, are untouched.

**Registry.** One line in `PROVIDERS`, after `openai-compatible`:

```text
&openai_compatible::OpenAiCompatible,
&kimi::Kimi,
```

Nothing else enumerates kinds by hand: the provider dialog offers the kind in registry order
with no base-URL step (`BaseUrlPolicy::Fixed`), the settings schema and the unknown-kind
error list pick it up from `providers::names()`, and the generic registry tests
(`every_registered_name_round_trips_through_the_kind`, the unique-names test) cover it.

`QuotaKind::Kimi` stays as the `openai-compatible` opt-in; no config migration is added.

## Implementation Steps

1. Extract `GATEWAY`, `ModelsFetcher` and `parse_served` from
   `rust/fontaine-core/src/providers/openai_compatible.rs` into
   `rust/fontaine-core/src/providers/gateway.rs` (`pub(crate)`), declaring `pub(crate) mod gateway;`
   in `providers/mod.rs`; `openai_compatible.rs` imports them. No behavior change; its
   existing tests move with the code they cover.
2. Add `rust/fontaine-core/src/providers/kimi.rs` (`pub mod kimi;` in `providers/mod.rs`):
   the `Provider` impl above and a `Connected` whose `completion`/`list`/`served_model`
   mirror `openai-compatible`'s, with `quota_source` always returning the `KimiQuota` built
   on the resolved endpoint.
3. Register `&kimi::Kimi` in `PROVIDERS` (`providers/mod.rs`).
4. Tests:
   - `rust/fontaine-core/tests/providers.rs`: `kind = "kimi"` parses and round-trips
     (also covered generically); a `quota` key on a `kimi` account is rejected naming
     `accounts.<name>.quota`; `prompt_caching`/`cache_ttl`/`wires` remain refused.
   - `kimi.rs` unit tests with wiremock: a connected keyless account lists models and fetches
     quota from `{endpoint}/usages` with no `Authorization` header; the key is sent when
     present; `served_model` parses a wrapped and a bare-array `/models` reply (reuse the
     moved cases at the `gateway` module level).
   - `rust/fontaine-core/tests/providers.rs`: connecting a `kind = "kimi"` account via
     `providers::find("kimi").connect(ConnectContext {…})` (as the catalog-vendor test
     does) yields a client with `quota_source().is_some()` and no `base_url` set.
   - In `every_kind_but_the_arbitrary_gateway_names_a_catalog_vendor`
     (`tests/providers.rs`), add `"kimi" => None` to the `expected` match — `kimi`, like
     `openai-compatible`, has no catalog vendor.
   - Run the generic registry tests in `tests/providers.rs` — they iterate
     `providers::names()` and must stay green.
5. `docs/configuration.md`: add a `kimi` row to the provider table (Chat Completions,
   `https://api.kimi.com/coding/v1`, optional key); replace the `[accounts.kimi]` example with
   `kind = "kimi"` + `api_key_env = "KIMI_API_KEY"` and no `base_url`/`quota`; reword the
   flat-rate paragraph so the quota display is built in for `kimi` accounts and remains a
   `quota = "kimi"` opt-in for `openai-compatible`; state that the moonshot.ai
   pay-as-you-go API stays `openai-compatible`. Add `kimi` to the "Where a credential comes
   from" sentence that names the kinds sending no `Authorization` header without a key.
   Check `docs/tui.md` status-bar prose for the same rewording need.
6. `CHANGELOG.md`: entry under `[Unreleased]` → `Added`.

## Files to Modify

- `rust/fontaine-core/src/providers/kimi.rs` (new), `providers/gateway.rs` (new),
  `providers/mod.rs`, `providers/openai_compatible.rs`
- `rust/fontaine-core/tests/providers.rs`
- `docs/configuration.md`, possibly `docs/tui.md`, `CHANGELOG.md`

## Trade-offs

- **Fixed endpoint vs. asking for a base URL.** `BaseUrlPolicy::Fixed` treats the kind as the
  Kimi for Coding subscription specifically; users pointing at other Kimi-family endpoints
  (moonshot.ai pay-as-you-go) keep using `openai-compatible`. `base_url` remains settable in
  settings for Fixed kinds (as with `anthropic`), so a staging endpoint is still reachable.
- **No implicit `api_key_env` default.** The kind does not assume `KIMI_API_KEY` when no
  credential is configured; every other kind requires an explicit `api_key_env` or a
  `/login`-stored key, and silent env lookups make "why is this account authenticated"
  hard to answer. The docs example names `KIMI_API_KEY`.
- **Always-on quota vs. an opt-in key.** The kind's only purpose is the subscription, so the
  quota display is built in and the `quota` key is refused (the generic "not used by kind"
  error). This matches `claude-subscription`, the other subscription kind.
- **`Billing::Unpriced`.** The subscription has no per-token price; dropping cost estimates
  for this kind matches `claude-subscription`/`opencode-go`. The old
  `openai-compatible` + `quota = "kimi"` path keeps `Billing::Catalog` and its (absent)
  catalog pricing, unchanged.

## Documentation

- `docs/configuration.md`: provider table, the `[accounts.kimi]` example, the flat-rate/quota
  paragraph, and a note that moonshot.ai pay-as-you-go remains `openai-compatible`.
- `docs/tui.md`: status-bar subscription-quota prose if it describes the `quota = "kimi"`
  opt-in as the only way to get the segment.
- `CHANGELOG.md` under `[Unreleased]` → `Added`.

## Testing Strategy

- Unit tests (wiremock) for the moved `/models` parsing and the new kind's connect: endpoint
  resolution with and without an account `base_url`, bearer behavior with and without a key,
  quota always attached. These cover logic, branches and error paths — the tier a unit test
  owns.
- Integration tests through `parse_settings`/`accounts::build` for the config shape rules
  (kind round-trip, refused keys), reusing the existing generic per-kind tests that iterate
  the registry.
- Live end-to-end (a real `kind = "kimi"` account answering a `fontaine -p` turn, quota
  segment visible) needs a Kimi for Coding key; it is a manual check, not a CI gate, and
  catches nothing the wiremock tests do not — the endpoint's wire shape is already pinned by
  them and by the recorded capture in `research/kimi-harness/captures/`.

## Manual Verification

None required beyond the automated tiers above. If a Kimi for Coding key is available, a
`fontaine -p "say hi"` run with a `kind = "kimi"` account and no `base_url`/`quota` keys
shows the model answering and the `kimi` quota segment in the result — optional confirmation,
not a gate.

## Open Questions

- None blocking. The issue's open questions settle as: `base_url` defaults to
  `https://api.kimi.com/coding/v1` (Fixed, overridable in settings); no implicit
  `api_key_env` (docs recommend `KIMI_API_KEY`); the quota display is built in, no `quota`
  key; model discovery is unchanged (the `/models` listing, asked once when the account has
  no `models` list).
