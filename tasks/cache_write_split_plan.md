# Cache-Write TTL Split Accounting Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/631

## Overview

Anthropic bills prompt-cache writes at two rates by lifetime: 5-minute writes at the write rate,
1-hour writes at twice the input rate. The reply carries the split in `usage.cache_creation`
(`ephemeral_5m_input_tokens`, `ephemeral_1h_input_tokens`). Fontaine records only the sum
(`cache_creation_input_tokens`), so a session with `cache_ttl = "1h"` cannot be tuned from `/cost`,
and the 5m writes Anthropic adds on its own (an automatic breakpoint when a server tool runs with
caching on) look like ones Fontaine asked for. This carries the two counts through the usage
record into the ledger, `/cost` (a column only when a split is present) and telemetry. Accounts
whose replies carry no split are unchanged.

## Current State

- rig's `Usage` (`rig-core 0.46.0`, `completion/request.rs`) is `#[non_exhaustive]` and `Copy`
  and has no field for the split. rig prices the writes by the split inside its Anthropic stream
  decoder (`providers/anthropic/streaming.rs`, `Counts::write_cost`: 5m at `pricing.cache_write`,
  1h at `2 × pricing.input`, and unpriced when the split is absent or does not sum to the
  writes), so `Usage.cost` on the `anthropic` account is already split-priced. Only
  `Quirks::anthropic()` sets `cache_ttl_pricing`; gateway dialects use `Quirks::gateway()` (false)
  and price every write at the single rate.
- The reply document rides on each call: `CompletionCall.raw` (rig-agent) holds the unary or
  reassembled streamed reply document (turns are streamed), with `usage.cache_creation` in it. `fontaine-core/src/providers/claude_subscription/
  context.rs:295` (`CacheUsage::from_result`) already reads the split from a raw usage `Value`.
- `fontaine-core/src/turn.rs:~853` handles `MultiTurnStreamItem::CompletionCall(call)` and calls
  `state.record_usage(source, agent, &call.usage, true, &served)` (`gate.rs:1289`), which feeds
  `UsageLedger::record` (`usage_ledger.rs:142`) and `Bucket::add`, which sums
  `cache_creation_input_tokens` only.
- `/cost` (`fontaine-tui/src/reports.rs:766`, `cost()`) shows `cache read` and `cache write`
  columns when any cache tokens exist, per source and per model.
- Telemetry: `fontaine/src/telemetry/span_bridge.rs:150` builds token metrics
  (`fontaine.token.usage`, `type` = input/output/cacheRead/cacheCreation/reasoning) from rig's
  `gen_ai.usage.*` span fields. rig's span has no split fields, and `cost_usd` there prices from
  a rebuilt `Usage` through `Pricing::cost`, which charges every write at the single
  `cache_write` rate, so a 1h-heavy session's metric cost is understated.
- Other recorders (guardian, goal, compaction, advisor, side question, prompt suggestion) call
  `record_usage` with a bare `Usage` and no reply document.

## Design

### Type

In `fontaine-core/src/usage_ledger.rs`:

```
pub struct CacheWrites { pub five_minute: u64, pub one_hour: u64 }
impl CacheWrites {
    /// From a reply document's `usage.cache_creation`. None when absent or malformed.
    pub fn from_reply(raw: &Value) -> Option<Self>
}
```

`from_reply` reads `usage.cache_creation.{ephemeral_5m,ephemeral_1h}_input_tokens`, both required
as u64, so a reply without the split (other providers, `null`, a partial object) is `None`,
matching rig's own all-or-nothing rule. It reads Anthropic's wire shape, so the claude_subscription
reader keeps its own 1h-only reading (it needs only the 1h count, and a result may carry no 5m).

### Ledger

- `Bucket` gains `cache_write_5m_tokens` and `cache_write_1h_tokens`: the sums over calls that had a
  split. They are a subset of `cache_creation_input_tokens`; a bucket mixing split and unsplit
  calls (a role that changed account) simply shows less in the split than in the total.
- `Bucket::add` and `UsageLedger::record` take `Option<CacheWrites>`; `merge` sums the new fields.
- `UsageSource`-level `record_usage` keeps its signature and delegates to a new
  `record_usage_split(.., writes: Option<CacheWrites>)` on `SessionState` (`gate.rs`), so the six
  other recorders stay untouched (open/closed). `record_usage_answered` gains the
  `Option<CacheWrites>` parameter and threads it to `self.usage.record`; its three direct callers
  (guardian adjudication, guardian-answered, goal; `gate.rs:1347,1366,1378`) pass `None`, and
  `record_usage_split` passes `answered = sent_request`.

### Turn wiring

`turn.rs` computes `CacheWrites::from_reply(&call.raw)` at `CompletionCall` and calls
`record_usage_split`. This covers main and subagent calls, the only calls that carry a reply
document, including `claude_subscription` ones: their split is read from the reassembled stream
`raw`, not the CLI result frame, and cost is unchanged (`Billing::Unpriced`, cost dropped at
`turn.rs:855`).

### `/cost`

`reports.rs::cost()` adds `write 5m` and `write 1h` columns right after `cache write`, shown only
when the total's `cache_write_5m_tokens + cache_write_1h_tokens > 0`. The column rule is computed
once on the total and applies to the per-source and per-model tables alike, as `cache` does today.

### Pricing

`Usage.cost` is already split-priced by rig for the `anthropic` account only, so the ledger cost
needs no change. The telemetry path's `cost_usd` rebuilds a `Usage` and so loses it; it gets the split
and prices writes as rig does (5m at `pricing.cache_write`, 1h at `2 × pricing.input`), only when
the split sums to the writes and `cache_write` is listed or the 5m count is 0; otherwise it falls back to
`Pricing::cost` as today. The 2× multiple lives in one small helper
(`cache_write_cost(pricing, writes)`) beside `cost_usd` in `span_bridge.rs`, its only consumer, with
a comment pointing at rig's matching rule, since rig's own is private.

### Telemetry

The split does not reach `span_bridge` through rig's span, so `turn.rs` emits it itself: a
`cache_write_split` event on target `fontaine::telemetry` (each crate redeclares
`const TARGET: &str = "fontaine::telemetry"`, as `gate.rs:41`), emitted from the `CompletionCall`
arm modeled on `api_error` (`turn.rs:915-924`): `query_source` ("main"/"subagent" from
`params.agent_id == ROOT_AGENT`), `model = params.model`, `provider = params.provider` (bare wire
ids matching the bridge's `gen_ai.request.model`; `params.served.model` is `<account>/<model>` and
would not join), plus `cache_write_5m` and `cache_write_1h`. `span_bridge.rs::on_event` returns
early unless the message is `tool_decision` (:290), so it becomes a dispatch by message name. The
bridge turns the event into `fontaine.token.usage` metrics of `type` `cacheWrite5m` and
`cacheWrite1h`, leaving `cacheCreation` as the sum. Accounts with no split emit no event. Events on
the telemetry target are exported as OTLP log records (`is_exported_event`, `span_bridge.rs:32-34`, used as the filter in `otlp.rs:252`).

## Implementation Steps

1. `usage_ledger.rs`: `CacheWrites`, `from_reply`, the two `Bucket` fields, `add`/`merge`/`record`
   plumbing, unit tests.
2. `gate.rs`: `record_usage_split`; `record_usage` delegates with `None`.
3. `turn.rs`: read the split from `call.raw` and record it.
4. `reports.rs`: the `/cost` columns.
5. Telemetry: the event, the bridge metrics, and the split-aware `cost_usd`.
6. Docs and `CHANGELOG.md`.

## Files to Modify

- `rust/fontaine-core/src/usage_ledger.rs`, `rust/fontaine-core/src/gate.rs`,
  `rust/fontaine-core/src/turn.rs`
- `rust/fontaine-tui/src/reports.rs`
- `rust/fontaine/src/telemetry/span_bridge.rs` (the event dispatch, metrics, `cache_write_cost`)
- `rust/fontaine/src/headless.rs` (the `cache_tokens_reach_the_result_usage` test calls
  `UsageLedger::record` directly; pass `None`)
- `rust/fontaine-core/tests/billing.rs`, `rust/fontaine-core/tests/audit_telemetry.rs`
- `CHANGELOG.md`, `docs/tui.md`, `docs/telemetry.md`

## Trade-offs

- **Add fields to the ledger vs. wrap `Usage`.** `Usage` is a non-exhaustive Copy type owned by
  rig, so it cannot carry the split; a Fontaine-side `Option<CacheWrites>` beside it keeps the
  rig types untouched.
- **Read `raw` vs. a rig hook.** `CompletionCall.raw` already holds the reply, and
  `AnthropicExtras::cache_creation` could be used through `extras::<AnthropicExt>()` on a
  `CompletionResponse`, but the agent loop hands Fontaine only the `CompletionCall`; reading the
  JSON mirrors the existing `claude_subscription` reader.
- **Per-call record vs. bucket-only split.** A subset-of-total split keeps old records and
  unsplit accounts valid with no migration.

## Decisions

- 2026-11-14: headless `result.usage` and `model_call` events are not extended; the issue names
  the ledger, `/cost` and telemetry, and the headless contract is a versioned surface.
- 2026-11-14: guardian, goal, compaction, advisor, side-question and suggestion calls record no
  split; none has a reply document at the recording point, and they stay as they are.
- 2026-11-14: gateway accounts (`Quirks::gateway()`, `cache_ttl_pricing` false) that return a split
  keep single-rate ledger cost; only the `anthropic` account is split-priced.

## Documentation

`docs/tui.md` ("Usage and cost": the new columns and the cache-column rule, ~:853) and
`docs/telemetry.md` (the metric `type` list, ~:125, gaining `cacheWrite5m` / `cacheWrite1h`, and a
`cache_write_split` row in the Events table). State that `cacheWrite5m` + `cacheWrite1h` are a
subset of `cacheCreation`, so the types must not all be summed. A line in `CHANGELOG.md`.

## Testing Strategy

- Unit: `CacheWrites::from_reply` (full split, absent, `null`, partial, non-numeric);
  `Bucket::add`/`merge` sum the split and leave a bucket with only unsplit calls at zero;
  `cache_write_cost` for 5m only, 1h only, mixed, and missing rate.
- `/cost` in `reports.rs` tests: columns absent without a split, present with one, and a mixed
  session (a split and an unsplit call) renders both.
- Integration, extending the existing `fontaine-core/tests/billing.rs`: keep its `MockStreamEvent`
  setup and wrap it in a mock transport (pattern: `tests/common/runtime.rs:84-110`,
  `Transport<MockScript>`) that supplies the raw document through
  `Opened::with_document(json!({"usage":{"cache_creation":{...}}}))` (rig-core `driver.rs:354-359`).
  A reply with `usage.cache_creation` lands in the ledger's split buckets and one without it leaves
  them zero, which catches the `raw` wiring a unit test of the parser cannot.
- Telemetry: the bridge test emits the event and asserts the two metric types; an
  `audit_telemetry.rs` case asserts `turn.rs` emits `cache_write_split` (precedent: `api_error`,
  `:408`).
