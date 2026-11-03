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
    pub label: String,                   // QuotaSource::name(), e.g. "kimi"
    pub quota: Option<Quota>,            // None: fetch failed, or no data yet
}
```

Only an account with a quota source gets an `AccountUsage`; a failed fetch or no data yet gives an
entry with `quota: None` (JSON without `windows`). An account without a source gets no entry, in
every surface.

`AccountUsage::json()` is the single serialisation (fields: `role`, `account`, `label`,
`windows[{label, used_percentage, resets_at}]`). Per window it follows Claude Code's status-line
`rate_limits` schema: `used_percentage` is `used_ratio` x 100 clamped to 0-100 and rounded to the nearest integer (as the status bar's `percent()` does), and `resets_at` is Unix epoch seconds (internal
types keep `used_ratio`). Headless and the future custom status line both call it (DRY). Absent
values are omitted, not null-filled with guesses.

### Guardian account polling

`Ui` owns a second `QuotaPoller` for the guardian. `sync_quota` becomes `sync_usage` and loops over
`[(Main, models.main), (Guardian, live guardian)]`, each with its own poller, receiver
and `quota_warned` entry. The guardian's quota source is not on the gate state
(`GateState::guardian()` returns the built `Arc<Guardian>`, which carries no handle or quota, and
`build_guardian_telling` drops `handle.quota`), and `models.guardian` is stale after
`retry_guardian`/`use_guardian`. So `AgentRuntime::install_guardian` (the one path `install_main`,
`retry_guardian` and `use_guardian` all go through) records the live guardian's account and quota
source in a new interior-mutable slot on `AgentRuntime`, beside the `llm_scanner` slot:
`Arc<RwLock<Option<(String, Arc<dyn QuotaSource>)>>>`, set from `GuardianModel::Ready(h)` (account
`h.reference.account`, source `h.quota`) and `None` for `Unavailable` or a handle without a source.
It is initialised at runtime construction from `models.guardian`, and read through
`AgentRuntime::guardian_quota()`; `sync_usage` calls it on every sync, so a retry or switch repoints
the poller. A guardian with no slot value points the guardian poller at `None` and yields no entry. Screen/composition models are out of scope. When the guardian's account equals the main account (same
`reference.account`), the guardian poller is pointed at `None` so one account is never polled or
shown twice. The event loop wakes on both receivers. The decisions of `sync_usage` (which source each poller
points at, same-account dedupe, building the entries) live in a pure function `usage_targets` over
`(role, account, Option<&Arc<dyn QuotaSource>>)` inputs; `Ui::sync_usage` only applies its result.

`App.quota: Option<QuotaView>` becomes `App.usage: Vec<AccountUsage>` plus one
`usage_as_of: SystemTime`.

### Status bar

`statusbar::quota_segment` keeps its text format. `QuotaView` is removed; `quota_segment` takes
`(&AccountUsage, now)` (`now` from `usage_as_of`) and adds the `guardian ` prefix for the Guardian
role. Rendering rules:

- Main: unchanged (always shown when data exists) — preserves documented behaviour.
- Guardian: shown only once any of its windows reaches `HOT_RATIO` (70%), as
  `guardian kimi 16% · 12d left · 5h 82%`. The `guardian` prefix always shows, since it distinguishes
  roles.
- No quota data: nothing is shown. No estimates.

The guardian span is pushed right after the main quota span and before effort, so position alone
decides the drop order (render has no width logic).

### Headless `-p`

- `result` event gains `account_usage: [AccountUsage::json…]` for Main and Guardian, fetched once
  with the 10 s source timeout (no background poller in headless), only right before a `result`
  event is actually emitted: per turn for stream-json, once for json, never for text. The sources
  are `runtime.models.main.quota` and, for the guardian, `AgentRuntime::guardian_quota()` (the same
  slot the TUI reads; headless.rs currently reads `runtime.models.guardian` only for `is_none()`
  in `init_event`). When the guardian's `reference.account` equals main's,
  fetch once and emit only the Main entry (the TUI's same-account rule). Main and
  guardian are fetched concurrently (join) and awaited before the sync `result_event` call, with
  the result passed in. A failed fetch, or no data yet, yields an entry without `windows`; a fetch failure
  is reported through `runtime.approver().warn`, once per account, with the warned-account set kept
  across the prompt loop.
- The field is omitted entirely when neither account has a quota source; an account without one has no entry.
- Documented in `docs/headless.md` alongside the `usage` object.

### Custom status line (#48)

Not implemented here (the feature does not exist). `AccountUsage::json` is the contract #48's
status-line JSON embeds under `account_usage`; this plan adds the line to the master plan's
`## Decisions` (Headless subsection) as part of step 7.

## Implementation Steps

1. `fontaine-core/src/quota.rs`: add `UsageRole`, `AccountUsage`, `AccountUsage::json`. Unit tests.
2. `fontaine-tui/src/app.rs`: replace `quota` with `usage: Vec<AccountUsage>` and `usage_as_of`.
3. `fontaine-core/src/agent/models.rs` (+ `agent.rs`/`agent/setup.rs` for the field): add the
   guardian quota slot on `AgentRuntime`, set in `install_guardian`, initialised from
   `models.guardian`, with `guardian_quota()`. Unit test: install a Ready guardian with a source,
   then an Unavailable one, and the slot follows.
4. `fontaine-tui/src/ui/mod.rs`: second poller + `sync_usage` over the pure `usage_targets`; same-account dedupe; wake on both
   receivers; per-account first-failure warning.
5. `fontaine-tui/src/statusbar.rs`: `QuotaView` removed, `quota_segment(&AccountUsage, now)`; guardian segment with threshold, placed after the main quota span
   and before effort; update existing tests.
6. `fontaine/src/headless.rs`: one-shot fetch and `account_usage` in `result_event`.
7. Docs: `docs/tui.md` (Subscription quota section; line 109, the status-bar sentence "the model (followed by the account's subscription quota…" names only the main quota), `docs/headless.md` (result fields),
   `docs/configuration.md` (quota is described as status-bar-only at lines 167, 191);
   `docs/schemas/headless-v1.schema.json` (`result.account_usage`, additive, stays v1); in
   `docs/tui.md` lines ~123-124, the "hidden when the main model is on an account without quota"
   sentence and the drop-order sentence change with a guardian segment; `CHANGELOG.md`;
   the master plan's `## Decisions` (Headless subsection) lines for the `account_usage` grouping and the `windows` array shape.

## Files to Modify

- `rust/fontaine-core/src/quota.rs`
- `rust/fontaine-core/src/agent/models.rs`, `agent/setup.rs`, `agent.rs` (guardian quota slot)
- `rust/fontaine-tui/src/app.rs`, `ui/mod.rs`, `statusbar.rs`, `ui/tests.rs`
- `rust/fontaine-tui/src/lib.rs` (`Ui` fields `quota`/`quota_rx`/`quota_warned`, lines 259-263,
  321-345)
- `rust/fontaine/src/headless.rs`
- `docs/schemas/headless-v1.schema.json`
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
  Fontaine addition, recorded in the master plan's Decisions (Headless subsection) in step 7.
- 2026-11-02 `account_usage` carries `windows` as a labelled array, not Claude Code's keyed
  `rate_limits` object: Kimi's windows (month/code/7d/5h) do not map onto Claude Code's fixed keys.
- 2026-11-02 This plan covers only `QuotaSource` endpoints (Kimi); header-derived usage (e.g.
  Anthropic rate-limit headers) goes to a follow-up issue.
- 2026-11-02 AFK: the guardian's quota source is recorded by `install_guardian` in a slot on
  `AgentRuntime` (like `llm_scanner`), read by both TUI and headless; `GateState::guardian()`
  carries no quota and `models.guardian` is stale after retry/switch, so neither can be the source.
- 2026-11-02 AFK: only an account with a quota source gets an `AccountUsage` entry; a failed fetch or
  no data yet gives an entry without `windows`; no label fallback to the account name.
- 2026-11-02 Paused-state rendering and JSON fields ship with #115; this plan does not build them.

## Documentation

`docs/tui.md` (guardian segment, threshold), `docs/headless.md` (`account_usage`),
`docs/configuration.md` (quota no longer status-bar-only), `CHANGELOG.md`.

## Testing Strategy

- Unit: `AccountUsage::json` (windows with `used_percentage`/`resets_at`, omitted fields);
  statusbar segment cases (guardian hidden under 70%, shown at 70%, shown when only a non-headline window is at 70%,
  no data → nothing).
- Unit: the pure `usage_targets` function (existing tui test helpers cannot drive `Ui::sync_usage`)
  with fake `QuotaSource`s for both roles: source choice per role, same-account dedupe, a repoint
  to the same account, entry building (a role with no source yields no entry; a source with no data
  yet yields an entry with `quota: None`).
- Integration through `fontaine -p --output-format stream-json` with a wiremock `/usages`
  endpoint: `result.account_usage` contains main and guardian entries; none when no source.
  Failure here would be silent for scripts consuming the JSON, so it is automated.
