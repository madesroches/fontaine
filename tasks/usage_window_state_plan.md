# Usage-Window State in the Status Line Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/116

## Overview

On a subscription account nothing tells the user how much of the usage window is left or that an
account is out until requests fail. The main agent's Kimi quota already reaches the status bar
(`subscription_quota_plan`). This extends it to the guardian's account (which can be a different
subscription), exposes the same per-account state to headless `-p` output, defines the shared
serialisation the custom status line (#48) will reuse, and leaves a typed slot for the paused
state that #115 will fill. Estimates are never shown as exact figures: with no provider data only
known facts (paused, reset time) appear.

## Current State

- `fontaine-core/src/quota.rs`: `QuotaSource`, `Quota`/`QuotaWindow`, `QuotaPoller` (60 s poll, last
  good value kept 10 min, `QuotaSnapshot { name, quota, error }`).
- `ModelHandle.quota` (`models.rs:123`) is filled by `guardian_from_client` from
  `AccountClient::quota_source()`; only Kimi implements it (`providers/kimi_quota.rs`).
- `Ui::sync_quota` (`fontaine-tui/src/ui/mod.rs:100`) points one `QuotaPoller` at
  `runtime.models.main.quota` and stores a `QuotaView` in `App.quota`; `statusbar::quota_segment`
  (`statusbar.rs:112`) renders `kimi 16% · 29d left`, shown from the first fetch, warning colour from
  90%.
- `Models.guardian: Option<GuardianModel>` carries the guardian's own `ModelHandle`, so its
  account's `quota` source exists but is never polled or shown.
- Headless `result_event` (`fontaine/src/headless.rs:283`) reports token usage per role
  (`usage.main/guardian/subagents/goal`) and no quota. No custom status line exists yet (#48 open),
  and #115 (pause state, 429 classification) is open: no paused state exists in core.

## Design

### Core: one per-account state type

In `fontaine-core/src/quota.rs`:

```text
pub enum UsageRole { Main, Guardian }

pub struct AccountUsage {
    pub role: UsageRole,
    pub account: String,                 // config account name
    pub label: String,                   // QuotaSource::name(), e.g. "kimi"; account name if no source
    pub quota: Option<Quota>,            // None: provider gives no data, or not fetched yet
    pub paused: Option<PausedUntil>,     // filled by #115
}
pub struct PausedUntil { pub reset_at: Option<SystemTime> }
```

`AccountUsage::json(now)` is the single serialisation (fields: `role`, `account`, `label`,
`windows[{label, used_ratio, reset_at}]`, `paused`, `paused_until`, `resets_in_secs`). Headless
and the future custom status line both call it (DRY). `reset_at` is RFC 3339; absent values are
omitted, not null-filled with guesses.

The paused slot is a read-only accessor, `fn paused(&self, account: &str) -> Option<PausedUntil>`
on a small `PauseView` trait defined here with a no-op default. #115 implements the real source;
this plan neither classifies 429s nor owns pause logic.

### Guardian account polling

`Ui` owns a second `QuotaPoller` for the guardian. `sync_quota` becomes `sync_usage` and loops over
`[(Main, models.main), (Guardian, models.guardian handle)]`, each with its own poller, receiver
and `quota_warned` entry. When the guardian's account equals the main account (same
`reference.account`), the guardian poller is pointed at `None` so one account is never polled or
shown twice. The event loop wakes on both receivers.

`App.quota: Option<QuotaView>` becomes `App.usage: Vec<AccountUsageView>` (role, label, quota,
paused, `as_of`).

### Status bar

`statusbar::quota_segment` keeps its text format. Rendering rules:

- Main: unchanged (always shown when data exists) — preserves documented behaviour.
- Guardian: shown only once its headline window reaches `HOT_RATIO` (70%) or it is paused, as
  `guardian kimi 82% · 2h left`. The `guardian` prefix always shows, since it distinguishes roles.
- Paused (either role): `kimi paused · resets in 1h 12m` in the warning colour, in place of the
  percentage. Without a reset time: `kimi paused`. The countdown derives from `as_of` like today's
  `left_text`; no clock read in render.
- No quota data and not paused: nothing is shown. No estimates.

On a narrow terminal the guardian segment is dropped before the main one (existing priority
rules; the main segment stays preferred over hint, goal, scheduled and agent counts).

### Headless `-p`

- `result` event gains `account_usage: [AccountUsage::json…]` for Main and Guardian, fetched once
  at the end of the turn with the 10 s source timeout (no background poller in headless). A failed
  or absent fetch yields an entry without `windows` (paused state still included when known); a
  fetch failure is a stderr warning via `Approver::warn`, once per account.
- Omitted entirely when neither account has a quota source and nothing is paused.
- Documented in `docs/headless.md` alongside the `usage` object.

### Custom status line (#48)

Not implemented here (the feature does not exist). `AccountUsage::json` is the contract #48's
status-line JSON embeds under `account_usage`; this plan records that in the `## Decisions` list
of the master plan when it ships.

## Implementation Steps

1. `fontaine-core/src/quota.rs`: add `UsageRole`, `AccountUsage`, `PausedUntil`, `PauseView`,
   `AccountUsage::json`. Unit tests.
2. `fontaine-tui/src/app.rs`: replace `quota` with `usage: Vec<AccountUsageView>`.
3. `fontaine-tui/src/ui/mod.rs`: second poller + `sync_usage`; same-account dedupe; wake on both
   receivers; per-account first-failure warning.
4. `fontaine-tui/src/statusbar.rs`: guardian segment with threshold, paused rendering, narrow-width
   priority; update existing tests.
5. `fontaine/src/headless.rs`: one-shot fetch and `account_usage` in `result_event`.
6. Docs: `docs/tui.md` (Subscription quota section), `docs/headless.md` (result fields),
   `docs/configuration.md` if wording about `quota` changes; `CHANGELOG.md`.

## Files to Modify

- `rust/fontaine-core/src/quota.rs`
- `rust/fontaine-tui/src/app.rs`, `ui/mod.rs`, `statusbar.rs`, `ui/tests.rs`
- `rust/fontaine/src/headless.rs`
- `docs/tui.md`, `docs/headless.md`, `CHANGELOG.md`

## Trade-offs

- Polling the guardian account separately vs. sharing one poller: accounts can differ, so each
  gets its own; same-account is deduped.
- One-shot fetch in headless vs. a background poller: a headless run has no draw loop; a poll per
  turn result is cheap and avoids a long-lived task.
- Building pause state here vs. waiting for #115: pause logic belongs to #115; a typed read slot
  keeps this work shippable and the status bar ready.

## Decisions

- 2026-11-02 Guardian segment is hidden below 70% while main stays always-shown, so the documented
  main behaviour does not change.
- 2026-11-02 The custom status line is deferred to #48; only the shared JSON shape ships here.

## Documentation

`docs/tui.md` (guardian segment, paused text, threshold), `docs/headless.md` (`account_usage`),
`CHANGELOG.md`.

## Testing Strategy

- Unit: `AccountUsage::json` (windows, paused with/without reset, omitted fields); statusbar
  segment cases (guardian hidden under 70%, shown at 70%, paused with countdown, paused without
  reset, same-account dedupe, narrow-width drop order, no data → nothing).
- Unit: `Ui::sync_usage` with fake `QuotaSource`s for both roles, including a repoint to the same
  account; `settle()`-style tests as in `quota.rs`.
- Integration through `fontaine -p --output-format stream-json` with a wiremock `/usages`
  endpoint: `result.account_usage` contains main and guardian entries; none when no source.
  Failure here would be silent for scripts consuming the JSON, so it is automated.

## Open Questions

- Whether `GuardianModel` exposes its `ModelHandle` for the quota source directly or only through
  the completion; confirm in `models.rs` before step 3 and add an accessor if needed.
