# Migrate from chrono to jiff Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/691

## Overview

Replace `chrono 0.4` (the workspace's only direct datetime dependency) with `jiff 0.2`, already in
`Cargo.lock` via `gix-date`. Pure refactor: no timestamp type is serialized (on-disk timestamps are
RFC 3339 `String`s), so no on-disk or wire format changes, and rendered output must stay identical.

## Current State

`chrono = { version = "0.4.45", default-features = false, features = ["clock"] }` in `rust/Cargo.toml:29`,
consumed by `fontaine-sdk` and `fontaine-tui`. Call sites:

- `fontaine-sdk/src/core/schedule.rs`: `local()` (l.74), `representable()` (l.82), cron search
  (`day_matches`, `advance`, `next_after<Tz: TimeZone>` l.195-275) resolving `LocalResult`; tests
  hand-roll a `Spring` timezone implementing `chrono::Offset`.
- `core/project_env.rs` (`expire` is `pub`, takes `DateTime<Utc>`; `TimeDelta::days`), `project_env/tests.rs`,
  `fontaine-sdk/tests/project_environment.rs`.
- `core/usage_limit.rs:139` (`parse_from_rfc2822` → `SystemTime`), `core/providers/kimi_quota.rs:105`
  (`parse_from_rfc3339`), `core/usage_pause.rs:557` (`%H:%M` local), `core/bug_submit.rs:66`
  (`%Y%m%dT%H%M%SZ`), `core/agent.rs:1049,1082` (`Utc::now()`).
- `fontaine-tui/src/statusbar.rs`: `reset_label`/`quota_segment` generic over `Tz`, `DateTime::<Local>::from`.

## Design

**Version**: jiff `0.2` (the copy already locked; `gix-date` needs `^0.2.25`). Type names follow 0.2 (`Zoned`).
Features: `default-features = false, features = ["std", "tz-system", "tzdb-zoneinfo", "tzdb-concatenated", "tzdb-bundle-platform"]`
(Windows needs `tzdb-bundle-platform`; the windows-gnu clippy check is compile-only and does not cover it).
Tests use the bundled tzdb: `tzdb-bundle-always` as a dev-dependency feature.

**Mapping**:

| chrono | jiff |
| --- | --- |
| `DateTime<Local>` | `Zoned` (`Zoned::now()`, `Timestamp::to_zoned(TimeZone::system())`) |
| `DateTime<Utc>` / `Utc::now()` | `Timestamp` / `Timestamp::now()` |
| `NaiveDateTime`/`NaiveDate` | `civil::DateTime`/`civil::Date` |
| `TimeDelta` | `Span` for calendar units, `SignedDuration` for fixed |
| `parse_from_rfc3339` | `str::parse::<Timestamp>()` |
| `parse_from_rfc2822` | `jiff::fmt::rfc2822::parse` → `Zoned`/`Timestamp` → `SystemTime` |
| `.format("..")` | `.strftime("..")` (check `%H:%M`, `%Y-%m-%d %H:%M`, `%Y%m%dT%H%M%SZ` render identically; use `Timestamp` for the `Z` form) |
| `LocalResult::{Single,Ambiguous,None}` | `tz.to_ambiguous_zoned(dt)` → explicit match on `Unambiguous`/`Fold`/`Gap`: `Fold(first, second)` → first if > after, else second if > after, else continue; `Gap` skipped, as `LocalResult::None` is |

**schedule.rs**: `next_after` stops being generic over a chrono `TimeZone`; it takes `&Zoned` and
uses the zone carried by it (`after.time_zone()`), so tests pass a real zone. A gap is detected via
`ambiguous.offset()`/`AmbiguousOffset::Gap` and passed over; a fold takes the first occurrence if > after, else the second if > after, else continues (matching the current `first`/`second` handling). `representable(ms)` becomes `Timestamp::from_millisecond(ms).is_ok()`
combined with a successful zoned conversion; the guard is kept because a saved file can hold an unrepresentable value.

**statusbar.rs**: `reset_label`/`quota_segment` take `&Zoned` for `now`; reset converted with
`Timestamp::try_from(SystemTime)` then `.to_zoned(now.time_zone().clone())`.

**project_env.rs**: `expire` takes `Timestamp`; age cutoff computed with `SignedDuration::from_hours(24 * n)`
(days as fixed 24h matches `TimeDelta::days`; `Timestamp` arithmetic rejects calendar units).

## Implementation Steps

1. Before migrating, add characterization assertions (see Testing) for fixed-instant formatted strings
   against chrono, so the same assertions prove identical output after the swap.
2. Add `jiff` alongside `chrono` in `rust/Cargo.toml`, `fontaine-sdk/Cargo.toml` and `fontaine-tui/Cargo.toml`.
3. Migrate `project_env.rs`, `usage_limit.rs`, `kimi_quota.rs`, `usage_pause.rs`, `bug_submit.rs`, `agent.rs`.
4. Migrate `schedule.rs` including the `Spring` test fake → real-zone gap test (e.g. `Europe/Paris`,
   2026-03-29 02:00–03:00 gap; a fold on 2026-10-25), and rename
   `a_saved_time_chrono_cannot_show_is_dropped_not_a_panic`.
5. Migrate `statusbar.rs` and its tests.
6. Update tests: `project_env/tests.rs`, `fontaine-sdk/tests/project_environment.rs`.
7. Fix doc comments mentioning chrono (`schedule.rs:81`); grep for stragglers.
8. Remove `chrono` from the three Cargo files; run the full gate; confirm no workspace crate is a direct
   dependent of `chrono` (`cargo tree -i chrono` still lists it via micromegas, oauth2, rmcp, schemars; the
   RUSTSEC-2026-0543 ignore in `rust/deny.toml` stays).

## Files to Modify

`rust/Cargo.toml`, `rust/Cargo.lock`, `rust/fontaine-sdk/Cargo.toml`, `rust/fontaine-tui/Cargo.toml`,
`core/{schedule,project_env,project_env/tests,usage_limit,usage_pause,bug_submit,agent}.rs`,
`core/providers/kimi_quota.rs`, `fontaine-sdk/tests/project_environment.rs`, `fontaine-tui/src/statusbar.rs`, `CHANGELOG.md`.

## Decisions

- jiff 1.0 rejected: second jiff major in the lockfile and `ZonedDateTime` renaming for no benefit now.
- Bundled tzdb in tests only: production keeps the system tzdb (current chrono behavior via the OS).

## Documentation

No docs page mentions chrono (grep confirmed); `CHANGELOG.md` gets one line noting that `core::project_env::expire` changes signature.

## Testing Strategy

- Existing schedule tests (cron search, DST) keep passing on `Zoned`; the spring-forward test uses a real
  zone's real gap with the bundled tzdb; add a fold (autumn) test pinning the earlier-instant choice, and one where `after` is inside the first occurrence so the second fires.
- Format characterization: unit tests asserting `local_time`, the `%H:%M` pause clock, bug-submit stamp
  and statusbar labels for a fixed instant in a fixed zone, committed before the swap with chrono and unchanged after.
- RFC 2822 / RFC 3339 parse tests for `usage_limit`/`kimi_quota` including a malformed input returning `None`.
- Gate: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check`,
  windows-gnu clippy check.
