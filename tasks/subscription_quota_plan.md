# Subscription Quota in the Status Bar Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/353

## Overview

For flat-rate accounts the useful budget signal is the provider's own quota, not an estimated
token price. This adds a provider-agnostic "quota source" on an account: a small probe that
returns labelled `used_ratio` / `reset_at` windows, polled slowly and shown as a compact
status-bar segment (`kimi 16% · 29d left`, with the 5h window shown only when hot). Kimi for
Coding's `GET <base>/usages` is the first implementation. Unpriced-token counting is unchanged;
this sits alongside it (complements #350, supersedes the subscription half of #351).

## Current State

- Accounts are `[accounts.<name>]` (`Account`, `fontaine-core/src/config.rs:343`). A Kimi account
  is `kind = "openai-compatible"` with `base_url = "https://api.kimi.com/coding/v1"` and an
  `api_key_env` or stored key (`docs/configuration.md`, `[accounts.*]`).
- Each kind is a `Provider` (`providers/mod.rs`); `Provider::connect` builds an
  `Arc<dyn AccountClient>` (completion, `list`, …) from a `ConnectContext` that holds the
  resolved credential, `base_url()` and the `LoadedNetwork`. `ModelHandle::guardian_from_client`
  (`models.rs:176`) copies what it needs out of the client into the handle; the TUI reads
  `runtime.models.main` every draw (`fontaine-tui/src/ui/mod.rs:57`).
- Unpriced tokens are tallied via `spend_of` (`gate.rs:901`) and shown by `/cost`, `/usage` and
  the agent list. Nothing fetches provider-side quota.
- The status bar (`fontaine-tui/src/statusbar.rs`, `render`) is one dim line:
  `<mode> · <dir> (<branch>) · <model> · effort · agents · scheduled · goal · <hint>`, fed from
  `App` fields set in `Ui::draw`. The event loop already wakes on timers (`soonest_tick`,
  `ui/mod.rs:44`).

## Design

### Core: quota model and source

New `fontaine-core/src/quota.rs`, dependency-light:

```text
pub struct QuotaWindow { pub label: String, pub used_ratio: f64 /* 0.0..=1.0+ */, pub reset_at: Option<SystemTime> }
pub struct Quota { pub windows: Vec<QuotaWindow> }          // ordered: longest-period "headline" first
pub trait QuotaSource: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;                                  // short label for the bar, e.g. "kimi"
    fn fetch(&self) -> BoxFuture<'_, Result<Quota, RuntimeError>>;
}
```

Failures use the existing `RuntimeError` (nothing dispatches on kind: failure always means
"keep the last good value, or hide").

`AccountClient` gains `fn quota_source(&self) -> Option<Arc<dyn QuotaSource>> { None }`
(open/closed: only kinds that opt in override it). `ModelHandle` gets a `quota: Option<Arc<dyn QuotaSource>>`
filled in `guardian_from_client` next to `thinking_off`/`output_probe`; `ModelHandle::new` leaves it `None`.

### Opt-in: explicit config key

`Account` gains `quota: Option<QuotaKind>` (enum, serde `kebab-case`, deriving `JsonSchema` alongside its serde derives as `Account` does; currently only `kimi`),
`openai-compatible` only (`Provider::accepts_quota()`, validated like `accepts_prompt_caching`;
other kinds reject it with the same error shape). Example:

```toml
[accounts.kimi]
kind = "openai-compatible"
base_url = "https://api.kimi.com/coding/v1"
api_key_env = "KIMI_API_KEY"
quota = "kimi"
```

Explicit opt-in rather than probe-and-remember: no request goes to an endpoint the user did not
name. The probe URL is the account's own `base_url` plus `/usages`, so it sends the account's key
to the account's own endpoint — the same trust as a model call. `quota` inherits the `accounts`
table's user/CLI-only scope.

### Kimi source

`fontaine-core/src/providers/kimi_quota.rs`: `KimiQuota { client: reqwest::Client, url, key }`
using `reqwest_client(network)` (shares proxy/CA settings), short timeout (10 s), bearer auth.
Parser is a pure function `parse_usages(&str) -> Result<Quota, _>`:

- `usages.limit_5h` → label `5h`; `limit_7d` → `7d`; `limit_month_total` → `month` (headline);
  `limit_month_code` → `code`; unknown `limit_*` keys ignored.
- `used_ratio` accepts number or numeric string; non-finite or negative → entry dropped.
- `reset_time` parsed as RFC 3339 into `SystemTime`; unparseable → `reset_at = None`.
- All entries optional; `boosterWallet` ignored (out of scope). Empty result → `RuntimeError`
  (so the segment hides rather than shows nothing useful).

### Poller

A small `QuotaPoller` in `fontaine-core` (so headless/other hosts could reuse it; TUI consumes it):
holds the source, `last_good: Option<(Quota, Instant)>`, and a `watch`-style shared slot. Rules:

- The poller task itself loops: fetch on start, then sleep 60 s and refetch, so idle sessions
  refresh; it also refetches when re-pointed. The TUI adds the poller's watch receiver
  `changed()` to the idle `select!` (`fontaine-tui/src/ui/mod.rs`) so a finished fetch wakes it.
- Re-pointed when `runtime.models.main.quota` changes by `Arc::ptr_eq` (or None-ness), not on
  `ModelRef`, so credential rebuilds that keep the same model (`main_rebuilt`) are covered.
- A fetch runs on the poller's spawned task; never awaited by the turn loop or the draw path.
- Failure keeps `last_good` until it is older than 10 minutes, then the slot clears (segment
  hidden). The poller publishes its first failure per account per session as an error in its
  slot; the TUI reports it once through `runtime.approver().warn(...)` with
  `error_chain::chain`.
- Source `None` (account has no quota, or model switched to one that doesn't) clears the slot.

### Status bar

`App` gains `quota: Option<QuotaView>` set in `Ui::draw` from the poller slot (a snapshot read,
no I/O). `statusbar.rs` gets a pure `quota_segment(&QuotaView, now) -> String`:

- Headline window (first) as `<name> <pct>% · <Nd|Nh|Nm> left` — percent rounded to nearest,
  `left` from `reset_at - now` (omitted if `reset_at` unknown or past).
- Any non-headline window whose ratio ≥ 0.70 is appended as `<label> <pct>%` (so
  `kimi 16% · 29d left · 5h 82%`); the threshold is a constant, not config.
- Ratios ≥ 1.0 render `100%`; the segment is styled with the theme `warning` colour at ≥ 0.90
  on any shown window.
- Placed after the model (`· <model> · <quota> · effort …`), dropped when `None`. The existing
  right-truncation cuts the hint, goal, scheduled and agents segments before the quota; no new
  layout machinery.

## Implementation Steps

1. `fontaine-core/src/quota.rs`: `Quota`, `QuotaWindow`, `QuotaSource`,
   `QuotaPoller` (+ unit tests with a fake source and `tokio::time::pause`). Export from `lib.rs`.
2. `config.rs`: `QuotaKind`, `Account.quota`, validation via `Provider::accepts_quota`; `quota`
   inherits the `accounts` table's user/CLI-only scope.
3. `providers/mod.rs`: `Provider::accepts_quota` (default false); `AccountClient::quota_source`
   (default `None`). `openai_compatible.rs`: accept `quota`; its client returns `KimiQuota` when
   `quota = "kimi"` (`ConnectContext` needs no change: `openai_compatible.rs` gets its own private
   `Connected { client, quota: Option<Arc<KimiQuota>> }` built from `ctx.account.quota`,
   `ctx.api_key()`, `ctx.base_url()`, `ctx.network`, following `openrouter.rs`).
4. `providers/kimi_quota.rs`: parser + fetcher.
5. `models.rs`: `ModelHandle.quota` populated in `guardian_from_client`.
6. TUI: `App.quota`, `Ui` owns the poller (re-pointed when `runtime.models.main.quota` changes by `Arc::ptr_eq`),
   adds its watch receiver `changed()` to the idle `select!`; `statusbar.rs` segment + warning
   style; the TUI calls `runtime.approver().warn(...)` with `error_chain::chain` for the poller's
   first failure.
7. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/{quota.rs (new), lib.rs, config.rs, models.rs}`
- `rust/fontaine-core/src/providers/{mod.rs, openai_compatible.rs, kimi_quota.rs (new)}`
- `rust/fontaine-tui/src/{app.rs, statusbar.rs, ui/mod.rs}`
- `docs/{configuration.md, tui.md}`, `CHANGELOG.md`

## Trade-offs

- **Trait on `AccountClient` vs a separate registry keyed by kind:** the client already holds the
  credential, endpoint and network settings, and `list()` sets the precedent for an optional
  provider-side query.
- **`quota = "kimi"` vs a generic `usage_url`:** a generic URL needs a response-shape DSL; a
  named parser enum is closed per provider but trivial to extend (one variant + one parser).
- **Fixed thresholds vs config:** 70% hot threshold and 60 s interval are advisory display
  details; no keys until someone needs them.

## Decisions

- Advisory display only: no guardian, floor or permission involvement; errors hide the segment.
- Quota is opt-in, user/CLI-scope config: it causes extra network requests carrying the credential, so none go to an endpoint the user did not name.

## Documentation

- `docs/configuration.md`: `quota` key in the `[accounts.<name>]` table, the Kimi example, and
  what it sends where.
- `docs/tui.md`: the status-bar segment, when the 5h window appears, when it hides.
- `CHANGELOG.md`: Added entry.

## Testing Strategy

- Unit: `parse_usages` (full response, numeric-string ratios, missing windows, unknown keys,
  bad timestamps, empty → error, `boosterWallet` present); `quota_segment` formatting (rounding,
  `Nd/Nh/Nm left`, hot-window threshold edge at 0.70, ≥1.0, unknown reset); config validation
  (kind rejection).
- Poller with a fake source and paused time: first fetch, 60 s throttle, failure keeps last good,
  10-minute expiry, model switch to a no-quota account clears, warn once.
- `wiremock`: `KimiQuota` sends `Authorization: Bearer`, hits `<base>/usages`, honours timeout,
  surfaces non-2xx as an error without the key in the message.
- TUI: `TestBackend` status-bar render with and without a quota.
  No live API in tests.

## Manual Verification

1. Configure the Kimi account above with a real key, run `cargo run -p fontaine`, confirm the bar
   shows `kimi <pct>% · <n>d left` matching the console's monthly figure. Not automated: needs a
   live subscription key (the shape itself is pinned by the parser tests).

## Open Questions

None blocking; `boosterWallet` display is deferred.
