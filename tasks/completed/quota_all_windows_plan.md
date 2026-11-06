# Status Bar: Every Quota Window with Its Absolute Reset Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/447

## Overview

With `quota = "kimi"` the status bar shows only the monthly window with a relative reset
(`kimi 51% · 23d left`) and hides the shorter windows until one reaches 70%. The provider already
reports every window with its reset time, so the rendering drops data the user wants. This changes
the main account's segment to always show every window, shortest first, each with its used share
and its reset as an absolute local time:

```
kimi · 5h 60% (15:00) · 7d 30% (Sun) · code 5% · month 51% (Dec 1)
```

## Current State

- `rust/fontaine-core/src/quota.rs`: `QuotaWindow { label, used_ratio, reset_at: Option<SystemTime> }`;
  `Quota.windows` is documented "longest-period window first: it is the one the status bar always
  shows". `AccountUsage::json()` serialises the windows in that order for headless
  `result.account_usage` and the custom status line.
- `rust/fontaine-core/src/providers/kimi_quota.rs`: `WINDOWS` lists `month, code, 7d, 5h` (longest
  first); `code` usually has no `reset_time`.
- `rust/fontaine-tui/src/statusbar.rs`:
  - `quota_segment(usage, now: SystemTime) -> Option<(String, bool)>` (line ~144) splits off the
    first window as the headline, prints `<label> <pct>%` plus `· <N>d|h|m left` (`left_text`),
    appends any other window at `HOT_RATIO` (0.70) or more, and flags `warn` at `WARN_RATIO`
    (0.90) on any shown window. A guardian segment (`UsageRole::Guardian`) gets a `guardian `
    prefix and returns `None` unless some window is at `HOT_RATIO`.
  - `render` calls it with `app.usage_as_of` (set to `SystemTime::now()` by `Ui::sync_usage`,
    `ui/mod.rs:310`).
  - Tests (lines ~548–690) pin the old shape.
- `fontaine-tui` has no `chrono` dependency; the workspace has `chrono = { version = "0.4.45",
  default-features = false, features = ["clock"] }`, which provides `Local` and `format`.
  `fontaine-core/src/schedule.rs` already uses `chrono::Local` for local-time reasoning.
- `docs/tui.md`, "Subscription quota" (lines 120–140) documents the old display, including the
  70% rule and `29d left`.

## Design

### Order

The core contract stays longest first: headless `account_usage.windows` and the custom status
line's JSON keep their order, so no machine-read output changes. The status bar iterates
`quota.windows.iter().rev()` to show shortest first. The `Quota.windows` doc comment is reworded
to "longest-period window first; the status bar shows them in reverse".

### Segment shape

```
<prefix><label> · <win> <pct>%[ (<reset>)] · <win> <pct>%[ (<reset>)] ...
```

- `<prefix>` is `guardian ` for the guardian's account, empty for the main one.
- `<pct>` is the existing `percent()` (rounded, capped at 100).
- `(<reset>)` is present only when the window has a `reset_at` strictly after `now`; a missing or
  past reset omits the parentheses (as the old code omitted `left`).
- `warn` is true when any window is at `WARN_RATIO` or more (every window is shown now).
- Main account: always shown when it has at least one window. `HOT_RATIO` no longer applies.
- Guardian account: still returns `None` unless some window is at `HOT_RATIO`; when shown, it uses
  the same run of windows as the main one, all of them.
- No windows (or no quota) → `None`, as today.

### Absolute reset label

A pure function, generic over the time zone so tests are deterministic:

```rust
fn reset_label<Tz: TimeZone>(reset: &DateTime<Tz>, now: &DateTime<Tz>) -> String
where Tz::Offset: Display
```

Compare local calendar dates (`reset.date_naive()` vs `now.date_naive()`), `days` = difference:

| `days` | label | example |
|---|---|---|
| 0 | `%H:%M` (24-hour) | `15:00` |
| 1 | `tomorrow` | `tomorrow` |
| 2–6 | `%a` | `Sun` |
| ≥ 7 | `%b %-d` | `Dec 1` |

Seven days out is the same weekday as today, so it shows the date rather than a weekday that
reads as this week.

`quota_segment` becomes generic: `quota_segment<Tz: TimeZone>(usage, now: &DateTime<Tz>)`,
converting each `reset_at` with `DateTime::<Utc>::from(system_time).with_timezone(&now.timezone())`.
`render` passes `DateTime::<Local>::from(app.usage_as_of)`. `left_text` and the headline split
are removed.

### Dependency

Add `chrono.workspace = true` to `rust/fontaine-tui/Cargo.toml` (already pinned in the workspace
and in the lock file; no new crate).

## Implementation Steps

1. `rust/fontaine-tui/Cargo.toml`: add `chrono.workspace = true`.
2. `rust/fontaine-tui/src/statusbar.rs`:
   - Add `reset_label`.
   - Rewrite `quota_segment` per the Design; drop `left_text`; keep `HOT_RATIO` (guardian only)
     with its doc comment changed to say so.
   - In `render`, build `DateTime::<Local>::from(app.usage_as_of)` once and pass it.
3. `rust/fontaine-core/src/quota.rs`: reword the `Quota.windows` doc comment.
   `rust/fontaine-core/src/providers/kimi_quota.rs`: reword the `WINDOWS` doc comment from
   "headline first" to "longest-period first" and rename the test
   `a_full_reply_lists_the_headline_first` to `a_full_reply_lists_the_longest_window_first`.
4. Tests in `statusbar.rs` (replace the old quota-shape tests).
5. `docs/tui.md`: rewrite the "Subscription quota" section's first two paragraphs.
6. `CHANGELOG.md`: add an `[Unreleased]` → `Changed` entry citing #447 (every window shown
   shortest first with absolute resets; the 70% rule dropped for the main account; the guardian
   segment uses the same run of windows). The released entries describing the old display
   (lines ~584–588) stay untouched.

## Files to Modify

- `rust/fontaine-tui/Cargo.toml`
- `rust/fontaine-tui/src/statusbar.rs`
- `rust/fontaine-core/src/quota.rs` (doc comment only)
- `rust/fontaine-core/src/providers/kimi_quota.rs` (doc comment and test name only)
- `docs/tui.md`
- `CHANGELOG.md`

## Trade-offs

- **24-hour clock.** The issue's example is `15:00`; locale-aware 12/24-hour detection needs a
  locale crate the workspace does not have.

## Decisions

- 2026-11-06: The guardian's segment, when shown, uses the new run of every window (with the
  `guardian ` prefix) rather than keeping the old headline-plus-hot-windows shape — the issue fixes
  only its prefix and its show-when-hot rule, and one formatter avoids two shapes on one bar.

## Documentation

`docs/tui.md`, "Subscription quota": new example
(`kimi · 5h 60% (15:00) · 7d 30% (Sun) · code 5% · month 51% (Dec 1)`), shortest first, every
window always shown for the main account, the reset rules (clock time today, `tomorrow`, weekday
within the week, date beyond, omitted when the provider gives none or it has passed, local time),
the 90% warning on any window, and the guardian example
(`guardian kimi · 5h 82% (15:00) · … · month 16% (Nov 18)`) still shown only from 70%. The
status-bar paragraph above it and `docs/headless.md` need no change.

## Testing Strategy

Unit tests in `statusbar.rs`, all with fixed `DateTime<FixedOffset>` instants:

- `reset_label`: same day → `15:00`; next day (including a reset at 00:30 when now is 23:50) →
  `tomorrow`; 2 and 6 days → weekday; 7 days → date; 25 days → `Dec 1`; a non-UTC offset where
  the UTC date and the local date differ, proving local dates are compared.
- `quota_segment`, main account: the four Kimi windows given longest first render as
  `kimi · 5h 60% (15:00) · 7d 30% (Sun) · code 5% · month 51% (Dec 1)`; every window shows at low
  usage (no 70% rule); a window with no reset or a past reset has no parentheses; rounding and
  the 100% cap still hold; no windows → `None`.
- `warn`: false below 90% everywhere, true when any single window (first, middle or last) is at 90%.
- Guardian: `None` below 70% on every window; shown with the `guardian ` prefix and every window
  once one is at 70%; no data → `None`.
- Render-level: the segment still sits after the model and before `effort`, and the guardian
  segment follows the main one (use windows without resets so the assertion does not depend on
  the machine's zone).

## Open Questions

None.
