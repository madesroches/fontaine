# Usage-Window State in the Status Line Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/116

## Overview

On a subscription account nothing tells the user how much of the usage window is left or that an
account is out until requests fail. The main agent's Kimi quota already reaches the status bar
(`subscription_quota_plan`). This extends it to the guardian's account (which can be a different
subscription), exposes the same per-account state to headless `-p` output, and defines the shared
serialisation the custom status line (#48) will reuse. Estimates are never shown as exact
figures: with no provider data nothing is shown.

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
  and #115 (pause state, 429 classification) is open.

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
}
```

`AccountUsage::json()` is the single serialisation (fields: `role`, `account`, `label`,
`windows[{label, used_percentage, resets_at}]`). Per window it follows Claude Code's status-line
`rate_limits` schema: `used_percentage` is 0-100 and `resets_at` is Unix epoch seconds (internal
types keep `used_ratio`). Headless and the future custom status line both call it (DRY). Absent
values are omitted, not null-filled with guesses.

### Guardian account polling

`Ui` owns a second `QuotaPoller` for the guardian. `sync_quota` becomes `sync_usage` and loops over
`[(Main, models.main), (Guardian, live guardian)]`, each with its own poller, receiver
and `quota_warned` entry. The guardian's quota source comes from live gate state, not
`models.guardian` (which `retry_guardian`/`use_guardian` do not update): match
`runtime.state.guardian()` on `GuardianModel::Ready(h)` and take `h.quota`, as headless already
reads it. A missing or `Unavailable` guardian points the guardian poller at `None` and yields no
entry. Screen/composition models are out of scope. When the guardian's account equals the main account (same
`reference.account`), the guardian poller is pointed at `None` so one account is never polled or
shown twice. The event loop wakes on both receivers.

`App.quota: Option<QuotaView>` becomes `App.usage: Vec<AccountUsage>` plus one
`usage_as_of: SystemTime`.

### Status bar

`statusbar::quota_segment` keeps its text format. Rendering rules:

- Main: unchanged (always shown when data exists) — preserves documented behaviour.
- Guardian: shown only once its headline window reaches `HOT_RATIO` (70%), as
  `guardian kimi 82% · 2h left`. The `guardian` prefix always shows, since it distinguishes roles.
- No quota data: nothing is shown. No estimates.

The guardian span is pushed right after the main quota span and before effort, so position alone
decides the drop order (render has no width logic).

### Headless `-p`

- `result` event gains `account_usage: [AccountUsage::json…]` for Main and Guardian, fetched once
  at the end of the turn with the 10 s source timeout (no background poller in headless). Main and
  guardian are fetched concurrently (join) and awaited before the sync `result_event` call, with
  the result passed in. A failed or absent fetch yields an entry without `windows`; a fetch failure
  is reported through `runtime.approver().warn`, once per account, with the warned-account set kept
  across the prompt loop.
- Omitted entirely when neither account has a quota source.
- Documented in `docs/headless.md` alongside the `usage` object.

### Custom status line (#48)

Not implemented here (the feature does not exist). `AccountUsage::json` is the contract #48's
status-line JSON embeds under `account_usage`; this plan adds the line to the master plan's
`## Decisions` (TUI subsection) as part of step 6.

## Implementation Steps

1. `fontaine-core/src/quota.rs`: add `UsageRole`, `AccountUsage`, `AccountUsage::json`. Unit tests.
2. `fontaine-tui/src/app.rs`: replace `quota` with `usage: Vec<AccountUsage>` and `usage_as_of`.
3. `fontaine-tui/src/ui/mod.rs`: second poller + `sync_usage`; same-account dedupe; wake on both
   receivers; per-account first-failure warning.
4. `fontaine-tui/src/statusbar.rs`: guardian segment with threshold, placed after the main quota span
   and before effort; update existing tests.
5. `fontaine/src/headless.rs`: one-shot fetch and `account_usage` in `result_event`.
6. Docs: `docs/tui.md` (Subscription quota section), `docs/headless.md` (result fields),
   `docs/configuration.md` (quota is described as status-bar-only at lines 167, 191); `CHANGELOG.md`;
   the master plan's `## Decisions` (TUI subsection) lines for the `account_usage` grouping.

## Files to Modify

- `rust/fontaine-core/src/quota.rs`
- `rust/fontaine-tui/src/app.rs`, `ui/mod.rs`, `statusbar.rs`, `ui/tests.rs`
- `rust/fontaine-tui/src/lib.rs` (`Ui` fields `quota`/`quota_rx`/`quota_warned`, lines 259-263,
  321-345)
- `rust/fontaine/src/headless.rs`
- `docs/tui.md`, `docs/headless.md`, `docs/configuration.md`, `CHANGELOG.md`,
  `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- Polling the guardian account separately vs. sharing one poller: accounts can differ, so each
  gets its own; same-account is deduped.
- One-shot fetch in headless vs. a background poller: a headless run has no draw loop; a fetch per
  turn result avoids a long-lived task. Worst case adds one 10 s source-timeout window per result
  line (main and guardian are fetched concurrently).

## Decisions

- 2026-11-02 Guardian segment is hidden below 70% while main stays always-shown, so the documented
  main behaviour does not change.
- 2026-11-02 The custom status line is deferred to #48; only the shared JSON shape ships here.
- 2026-11-02 `account_usage` windows use Claude Code's `rate_limits` field names and units
  (`used_percentage` 0-100, `resets_at` epoch seconds); the per-role `account_usage` grouping is a
  Fontaine addition, recorded in the master plan's Decisions (TUI subsection) in step 6.
- 2026-11-02 This plan covers only `QuotaSource` endpoints (Kimi); header-derived usage (e.g.
  Anthropic rate-limit headers) goes to a follow-up issue.
- 2026-11-02 Paused-state rendering and JSON fields ship with #115; this plan does not build them.

## Documentation

`docs/tui.md` (guardian segment, threshold), `docs/headless.md` (`account_usage`),
`docs/configuration.md` (quota no longer status-bar-only), `CHANGELOG.md`.

## Testing Strategy

- Unit: `AccountUsage::json` (windows with `used_percentage`/`resets_at`, omitted fields);
  statusbar segment cases (guardian hidden under 70%, shown at 70%, same-account dedupe, no data
  → nothing).
- Unit: `Ui::sync_usage` with fake `QuotaSource`s for both roles, including a repoint to the same
  account; `settle()`-style tests as in `quota.rs`.
- Integration through `fontaine -p --output-format stream-json` with a wiremock `/usages`
  endpoint: `result.account_usage` contains main and guardian entries; none when no source.
  Failure here would be silent for scripts consuming the JSON, so it is automated.
