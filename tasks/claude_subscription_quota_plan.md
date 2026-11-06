# Claude Subscription Quota Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/454

## Overview

A `claude-subscription` account shows its plan usage — the 5-hour and weekly Claude
subscription windows — in the status bar, in headless `result.account_usage` and in the custom
status line input. The data source is the `rate_limit_event` frames the official CLI already
emits mid-stream, which carry per-window utilization and reset times. No config key, no extra
network request: the figures fall out of the model calls the session already makes.

## Current State

- The quota machinery is provider-agnostic (`fontaine-core/src/quota.rs`): `QuotaSource`
  (`name`, `fetch`), `Quota`/`QuotaWindow` (windows ordered longest-period first),
  `QuotaPoller` (fetch at once, then every 60 s; last good value kept 10 minutes; failures
  published as `QuotaSnapshot.error`). Only `KimiQuota`
  (`providers/kimi_quota.rs`) implements the trait; `AccountClient::quota_source()` defaults to
  `None` (`providers/mod.rs:236`) and `ModelHandle::quota` is filled in
  `models.rs:214` (`guardian_from_client`).
- The TUI polls main and guardian accounts (`Ui::sync_usage`,
  `fontaine-tui/src/ui/mod.rs:284`; the pure `usage_targets` dedupes a guardian on the main
  account) and renders `quota_segment` (`statusbar.rs:112`). Headless fetches once per `result`
  event (`fontaine/src/headless.rs:295`). Every surface consumes `AccountUsage`; a quota with no
  windows renders as no segment.
- `claude-subscription` (`providers/claude_subscription/`) drives the installed `claude` CLI.
  Its protocol reader drops the frames that carry the data:
  `protocol.rs:757` maps `"rate_limit_event" | "keep_alive"` to `Event::Nothing`. `Reader` is
  built per request in `transport.rs` (`Reader::new`), and no state is shared between requests:
  `Connected` (`mod.rs`) holds `cli`, `session`, `inventory` and the discovered-model catalog.
- `quota = "kimi"` is an explicit opt-in on `openai-compatible` accounts because it makes extra
  requests carrying the credential (`config.rs:1155`, `Provider::accepts_quota`).
- `MIN_VERSION` for the CLI is 2.1.318 (`cli.rs:28`).
- The `claude_cli_double` example (`examples/claude_cli_double.rs`) is script-driven; its
  `mid_stream` denial mode already emits a bare `{"type": "rate_limit_event"}` line (438), which
  the protocol tests assert is tolerated (`protocol/tests.rs:789`).

## The frame

Step 1 of the issue asks whether a real `rate_limit_event` carries window utilization and reset
times, or only a status. A real payload (captured on CLI 2.1.283, one-turn probe, mirrored by
the community stream-json references):

```json
{"type":"rate_limit_event","rate_limit_info":{
  "status":"allowed","resetsAt":1790578200,"rateLimitType":"five_hour",
  "overageStatus":"rejected","overageDisabledReason":"out_of_credits","isUsingOverage":false,
  "unifiedWindows":{
    "five_hour":{"utilization":0.09,"resetsAt":1790578200},
    "seven_day":{"utilization":0.15,"resetsAt":1791036000}}},
 "uuid":"…","session_id":"…"}
```

Facts that drive the design:

- `rate_limit_info.unifiedWindows` holds the subscription windows — `five_hour`, `seven_day`,
  `seven_day_overage_included` — each `{utilization: 0–1, resetsAt}`. `resetsAt` is **epoch
  seconds** (1790578200 ≈ 2026-07-28), despite some wrapper docs claiming milliseconds.
- `unifiedWindows` is absent for API-key / Bedrock / Vertex sessions. `claude-subscription`
  requires the CLI's own subscription login, and every accepted CLI is ≥ 2.1.318 >
  2.1.283 (the version the frame above was captured on), so subscription sessions reliably
  carry it; defensive parsing still treats its absence as "no data".
- The CLI emits the event whenever an API response moves a window's rounded utilization or a
  reset time — a healthy session sees a steady trickle of `status: "allowed"` events, and the
  first one lands after the first model turn. That matches the issue's expectation that data
  appears only after the first turn.
- The top-level `rateLimitType` / `utilization` / `resetsAt` describe one window (the one the
  status concerns) and serve as a fallback when `unifiedWindows` is absent. A bare
  `{"type": "rate_limit_event"}` (no `rate_limit_info`) also occurs — the double emits one —
  and is tolerated today.

Conclusion: the frame carries utilization and reset times, so the issue's approach 2 applies.
The CLI's OAuth credentials and Anthropic's undocumented usage endpoints are never touched.

## Design

### Parser and shared slot: `claude_subscription/usage.rs` (new)

```text
pub struct Usage { latest: Mutex<Option<Quota>> }          // interior-mutable slot
impl Usage {
    pub fn new() -> Self;
    pub fn record(&self, quota: Quota);                    // called from the protocol reader
    pub fn snapshot(&self) -> Option<Quota>;               // called by fetch()
}
impl QuotaSource for Usage {
    fn name(&self) -> &str { "claude" }
    fn fetch(&self) -> Ok(snapshot, or Quota { windows: vec![] } when none yet)
}
```

`Usage` is one type serving both roles (slot + source), created once per `Connected` in
`connect()` and shared by every model and namespace of the account, so main, guardian and
subagent turns all feed the same slot. The lock is display-only state, so it uses the
recovering `fontaine_tools::lock` helpers with a one-line reason, not the panicking ones.

A pure function `parse_rate_limit(value: &Value) -> Quota` owns the wire knowledge:

- `rate_limit_info.unifiedWindows`: known keys map to window labels `five_hour` → `5h`,
  `seven_day` → `7d`; `seven_day_overage_included` and unknown keys are skipped. A window needs
  a `utilization` that is a finite number ≥ 0 (mirroring `parse_usages`' dropping rule);
  `resetsAt` epoch seconds become `reset_at`, anything missing or unparseable becomes `None`.
- No usable `unifiedWindows` → fall back to the top-level `rateLimitType` + `utilization` (+
  `resetsAt`) as a single window, with the same type mapping. `overage` is skipped.
- Windows are emitted longest-first (`[7d, 5h]`), matching `Quota`'s ordering contract.
- Nothing in the frame can fail: a bare event, a status-only frame, or an unparseable payload
  yields an empty `Quota`, and the reader treats that as a no-op. A malformed frame never
  fails a turn — the same tolerance the adapter has today.

### Protocol records: `protocol.rs`

`Reader` gains a `usage: Option<Arc<Usage>>` field (`Reader::new` takes it; the tests' `started`
helper passes `None` or a real slot). `dispatch` splits the two ignored events:

- `"keep_alive"` → `Event::Nothing`, unchanged.
- `"rate_limit_event"` → `if let Some(usage) = &self.usage { let quota = parse_rate_limit(value); if !quota.windows.is_empty() { usage.record(quota); } } Ok(Event::Nothing)`.

Only frames with at least one window overwrite the slot, so a sparse fallback frame cannot
erase fuller data. The event flow, trail labels and rejection logging are untouched — quota is
out-of-band advisory state, the same relationship `Reader::native()` has to error reporting.

### Wiring: `mod.rs`, `transport.rs`

- `Connected` gains `usage: Arc<Usage>`, built in `ClaudeSubscription::connect`.
- `impl AccountClient for Connected` overrides `quota_source()` to return
  `Some(self.usage.clone())` — always, with no config key (see Trade-offs).
- `Connected::model` passes `self.usage.clone()` to `ClaudeTransport` (new constructor
  parameter; `Selection` stays purely model identity) and `transport.rs` hands it to
  `Reader::new`.

### Core poller: empty windows mean "no data", not an error — `quota.rs`

Before the first turn the slot is empty; a fetch must then answer "nothing to show" without
becoming `QuotaSnapshot.error`, because the TUI warns once per account on the first failure
(`ui/mod.rs:303`) and "no data yet" is the normal state of every new session, not a failure.

- `QuotaSource::fetch`'s doc comment gains: an `Ok` quota with no windows means the source has
  no data right now — nothing to show, not an error.
- `run()` matches `Ok(quota) if quota.windows.is_empty()` by clearing `last_good` and leaving
  `published.error` at `None`. Kimi never returns empty (`parse_usages` errors instead), so its
  behaviour is unchanged.

The rest of the pipeline needs no changes: `quota_segment` already renders a windowless quota
as nothing, the guardian's same-account dedupe in `usage_targets` covers a guardian on the same
`claude-subscription` account, and headless `account_usage` fetches after the turn's frames have
been read, so a turn that produced usage data reports it in its own `result` event.

### Double fixture: `claude_cli_double.rs`

New script key `"rate_limit"`: when the script object carries it, the double emits, after
`message_stop`, a realistic frame — the captured payload above with values overridable from the
script object (e.g. `"rate_limit": {"five_hour": 0.6, "seven_day": 0.3}`). The `mid_stream`
bare frame and every existing script behaviour are unchanged.

## Implementation Steps

1. `fontaine-core/src/quota.rs`: the empty-windows branch in `run()` + the `fetch` doc comment
   (+ paused-time poller tests).
2. `fontaine-core/src/providers/claude_subscription/usage.rs`: `Usage`, `parse_rate_limit`
   (+ unit tests).
3. `protocol.rs` (+ `protocol/tests.rs`): the `usage` field, the split dispatch, recording
   tests.
4. `mod.rs`: the `usage` field on `Connected`, creation in `connect`, `quota_source`.
5. `transport.rs`: thread the slot from `Connected` into `Reader`.
6. `examples/claude_cli_double.rs`: the `"rate_limit"` script key.
7. Integration test through a double-backed `claude-subscription` account (headless
   `result.account_usage` with and without the script key).
8. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/quota.rs`
- `rust/fontaine-core/src/providers/claude_subscription/{usage.rs (new), protocol.rs, protocol/tests.rs, mod.rs, transport.rs}`
- `rust/fontaine-core/examples/claude_cli_double.rs`
- `docs/{configuration.md, tui.md, claude-subscription.md}`, `CHANGELOG.md`

## Trade-offs

- **Parsing in the `Reader` (side effect into a slot) vs a new `Event` variant:** the event
  stream is rig-facing wire frames consumed by the turn machinery; quota is advisory display
  state, like `Reader::native()`, and a variant would touch every consumer for a signal none of
  them needs.
- **Always-on vs a `quota = "claude"` opt-in key:** the existing opt-in exists because a quota
  probe sends the credential in extra requests to an endpoint the user named. This source makes
  no request at all — the data arrives on the model stream the session already initiated — so a
  config key would add surface for zero privacy or traffic gain. `accepts_quota` stays `false`
  for this kind and `Account.quota` is not settable on it.
- **Empty-windows `Ok` vs a typed "no data" on `fetch`:** a windowless `Quota` already renders
  as no segment in every surface; a `fetch` enum would churn the shared trait for one
  provider's transient state.
- **Skipping `seven_day_overage_included`:** that window meters paid extra usage, which this
  adapter refuses to draw (the billing guard rejects credit-marked models), so the figures that
  matter are the included 5h and 7d windows. Revisit if a user with extra usage wants it shown.
- **`resetsAt` read as epoch seconds:** the captured real payload shows seconds; a wrong-unit
  value would just render a reset time in the past, which the bar omits.

## Decisions

- 2026-11-06 The real `rate_limit_event` (CLI 2.1.283) carries `unifiedWindows` with per-window
  utilization and `resetsAt` epoch seconds, so the issue's utilization-parsing approach ships
  and the coarse-status fallback is not built. The CLI's OAuth credentials and undocumented
  usage endpoints stay untouched.
- 2026-11-06 Quota for `claude-subscription` is always on with no config key: unlike the Kimi
  probe it sends no extra request and no credential anywhere.
- 2026-11-06 A `rate_limit_event` with no usable window data (bare, status-only, or malformed)
  is tolerated and leaves the slot untouched; it never fails a turn.

## Documentation

- `docs/configuration.md`: the `quota` row notes it is `openai-compatible`-only and that
  `claude-subscription` shows usage without any key; the `claude-subscription` account section
  mentions the status bar figures.
- `docs/tui.md` ("Subscription quota"): a `claude-subscription` account adds
  `claude · 5h 9% (15:00) · 7d 15% (Mon)` from the first turn on, no `quota` key needed; no data
  (API-key session, pre-first-turn) means no segment.
- `docs/claude-subscription.md`: the usage figures come from the CLI's own stream events and
  never leave the machine.
- `CHANGELOG.md`: Added entry.

## Testing Strategy

- Unit: `parse_rate_limit` — the captured payload → `[7d 0.15 (reset), 5h 0.09 (reset)]`;
  `seven_day_overage_included` and unknown keys skipped; a window with a bad utilization drops
  while a bad `resetsAt` only loses the reset; fallback single window from the top-level fields;
  bare and status-only frames → empty; ordering longest-first.
- Unit: `Usage` — `fetch` before any record returns an empty quota; after `record` it returns
  the recorded value; `name()` is `claude`.
- Unit (paused time): the poller's empty-windows branch — snapshot has no quota and no error;
  a later non-empty fetch shows; the existing failure paths still publish errors.
- Unit: the `Reader` records a parsed quota mid-response and still returns `Event::Nothing`;
  with a slot attached, a bare frame changes nothing.
- Integration through a double-backed `claude-subscription` account
  (`claude_cli_double` with the `"rate_limit"` script key): headless `result.account_usage`
  carries the `claude` entry with both windows; without the key the entry has no windows. No
  live API in tests — the frame shape is pinned by the parser tests from the captured payload.

## Manual Verification

1. On a machine whose `claude` CLI signs in with a Claude Max subscription, select a
   `claude-subscription` account, send one message in `cargo run -p fontaine`, and confirm the
   bar shows `claude · 5h N% (…) · 7d N% (…)` within a minute; `fontaine -p --output-format
   stream-json` shows the windows in `result.account_usage`. Not automated: needs a live
   subscription login — the development box's CLI authenticates by API key, a session class that
   never emits `unifiedWindows`, so the frame cannot be captured here.

## Open Questions

None blocking.
