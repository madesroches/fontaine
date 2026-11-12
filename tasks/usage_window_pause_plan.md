# Pause the run when an account's usage window is exhausted Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/115

## Overview

A subscription account that has spent its usage window answers every request with a 429 until the
window resets. Fontaine treats that like any transient 429: three tries, then the run fails, or
the guardian's account is the exhausted one and every gated call is denied with
`GuardianUnavailable` while background agents fail one by one. This adds an explicit, session-wide
**usage pause**: a spent window is classified apart from a transient rate limit, the whole run
(main agent, guardian, subagents) stops at its next provider call, pending gated calls stay
pending, and the run resumes when the window resets, on a deadline, with fail-closed handling on
every other exit. The TUI shows a countdown with resume / cancel / switch-account actions; headless
`-p` waits up to a configurable cap or exits at once with a distinct exit code.

## Current State

- `rust/fontaine-core/src/turn.rs`: `drive` retries a `retryable` failure `MAX_RETRIES` (2) times
  with a 250 ms doubling `BACKOFF` (`turn.rs:31`, `turn.rs:562-615`). A 429 is retryable, so a
  spent window is retried like a hiccup and then settles as `TurnOutcome::Failed`.
- `turn.rs:472-550`: `quota_reply` / `falls_through` / `error_falls_through` already recognise
  quota-class replies by status or machine code (`QUOTA_CODES`) and for `claude-subscription` by
  `ErrorKind::Limit`; a model list falls through to its next entry on them. Nothing matches on
  error text. This classifier cannot tell "window spent" from "slow down".
- The guardian crate wraps rig errors (`fontaine-guardian/src/guardian.rs`); an unreachable
  guardian yields an `Adjudication` with `fail_closed: Some(FailClosed::…unavailable)`;
  `permissions.rs:1913-1960` then, when a human is present, calls
  `Approver::guardian_unavailable` (retry / manual / deny) and otherwise denies.
- `fontaine-core/src/quota.rs`: `QuotaSource` polls a provider's own usage windows
  (`QuotaWindow.reset_at`) for display; `AccountUsage { role, account, label, quota }` is shown by
  every surface and by headless `result.account_usage`.
- `fontaine-agents/src/budget.rs`: agent budgets are token/cost caps, not wall-clock. Elapsed
  clocks live on tree nodes (`fontaine-agents/src/tree.rs`: `started` / `ended` `Instant`s).
- Headless exit codes: 0 completed, 1 run error / unmet goal, 2 usage error
  (`fontaine/src/headless.rs` `run`).
- Kimi captures (`research/kimi-harness/captures/e2_go_429.json`): the OpenCode Go gateway answers
  a spent monthly allowance with 429, body type `GoUsageLimitError` and a `retry-after` of about
  15 days; Kimi's code plan answers `exceeded_current_quota_error`.

## Design

### Classification (`fontaine-core/src/usage_limit.rs`, new)

```rust
pub struct UsageLimit { pub reset_at: Option<SystemTime>, pub window: Option<String> }
pub fn usage_limit(error: &(dyn Error + 'static)) -> Option<UsageLimit>
```

Walks the `source()` chain like `error_falls_through` and reads structured fields only: HTTP
status, the provider body's machine code (`provider_response().machine_code()`), and headers
(`Retry-After`, provider reset headers). Never error text, never tool arguments.

A reply is a **spent window** when status is 429/402 and either (a) its machine code is in
`WINDOW_CODES` (`exceeded_current_quota_error`, `GoUsageLimitError`, the Anthropic subscription
limit code, …), or (b) its `Retry-After` exceeds `TRANSIENT_RETRY_AFTER_MAX` (5 minutes, far
above the 250 ms backoff ceiling). `claude-subscription`'s `ErrorKind::Limit` is a spent window,
its reset coming from the CLI's rate-limit event the quota source already holds. Anything else
stays a transient 429: the existing retry with backoff, unchanged. The code table is data keyed
by account kind (one entry per provider), so a new provider adds a row, not a branch (open/closed).
`WINDOW_CODES` entries are not in `QUOTA_CODES`' generic set; `falls_through` stays true for them,
so a model list still moves to its next entry before any pause is engaged (the pause is the
outcome when no next entry exists or the chain is spent).

Reset time, in order: `Retry-After` / reset header; reset in the body's structured fields; the
account's `QuotaSource` windows (the soonest `reset_at` of a window at or above 1.0 used ratio);
else unknown.

### The pause (`fontaine-core/src/usage_pause.rs`, new)

A session-wide `UsagePause` owned by `SessionState`, built on a `tokio::sync::watch` channel:

```rust
pub struct PauseInfo { role: UsageRole, account: String, reset_at: Option<SystemTime>,
                       since: Instant, deadline: Option<Instant> }
pub enum PauseEnd { Reset, Probed, Resumed /* user */, Cancelled, CapReached }
impl UsagePause {
    pub fn engage(&self, info: PauseInfo);              // idempotent; first engager wins
    pub async fn wait_open(&self) -> Result<(), PauseEnd>; // Ok when not paused or resumed
    pub fn resume(&self); pub fn cancel(&self);
    pub fn total_paused(&self) -> Duration;             // for budget/elapsed clocks
}
```

`wait_open` is awaited **before every provider call** by the main agent's `drive` loop, subagent
runs (they share `drive`), the guardian adjudication, and one-shot jobs (scanner, advisor, side
questions). One call site per path, no per-feature copies. A pause is an explicit state with a
deadline, never an open-ended wait.

Driver task (the waiter that engaged): if `reset_at` is known, sleep until it plus a small margin
(30 s), then **probe** by letting exactly one caller through (`QuotaSource::fetch` when the account
has one, else the next real call); a probe that 429s again re-engages with the new reset time. If
unknown, probe at intervals starting at 1 minute, doubling to a 15-minute ceiling. A probe that
succeeds ends the pause `Probed`/`Reset` and releases every waiter.

Per call site:
- **Main/subagent `drive`**: a failure with `usage_limit()` engages the pause, waits, then retries
  the attempt on `RETRY_NOTE` **without consuming** `MAX_RETRIES`. On `Err(Cancelled|CapReached)`
  the turn settles `Failed` with the usage-limit message (names account, reset time).
- **Guardian**: `Adjudication` gains the failure's cause so core classifies it (the guardian crate
  stays free of core types and gains no new dependency). When the cause is a spent window,
  `permissions.rs` engages the pause *instead of* the `GuardianUnavailable` prompt or Deny, awaits
  `wait_open`, and re-runs the guardian check from the start. A pending call is neither denied nor
  run while waiting. On `Cancelled`/`CapReached` the call is **Denied**, with a new fail-closed
  kind `UsagePauseEnded` (error text may explain a Deny, never change it). A 429 never becomes an
  Allow; if resume itself fails, the call is denied exactly as today.
- Calls already executing (a running shell) are not interrupted; only provider calls pause.

### TUI

Status line while paused: `paused — <account> usage window spent, resets 14:30 (in 2h 14m)`, or
`… reset time unknown, next check in 4m`. A dialog-less key row offers `r` resume now (an early
probe), `c` cancel, `s` switch account. Switch reuses the `/model` and `/guardian-model` machinery
(the exhausted role's account is replaced for the live session with a validated alternative, then
`resume`). The `Approver` trait gains `usage_paused(info) -> BoxFuture<PauseChoice>` (default:
wait; headless default per below), mirroring `guardian_unavailable`.

### Headless `-p`

New flags and config, user-scope for the waiting cap (project scope may only choose exit, a
tightening): `--on-usage-limit=wait|exit` (default `wait`) and `--max-usage-wait=<duration>`
(default 6h, longer than a 5-hour window). `exit` and an expired cap end the run with exit code 3
and a `result` subtype `usage_limit` carrying the account and reset time. `stream-json` emits
additive `usage_paused` / `usage_resumed` events; `docs/schemas/headless-v1.schema.json` is
updated (additive within `v1`).

### Ledger, audit, accounting

- Ledger `Record::UsagePaused` / `Record::UsageResumed { end: PauseEnd }`; the audit log and
  telemetry record the same.
- Agent budgets are token/cost, so waiting spends nothing. Elapsed clocks on tree nodes subtract
  `UsagePause::total_paused()` so a subagent's displayed run time and any wall-clock accounting
  stop during the pause.
- Not paused by this: tool execution already in flight, MCP, hooks.

## Implementation Steps

1. `usage_limit.rs`: classifier, `WINDOW_CODES` table per account kind, reset-time extraction;
   unit tests over constructed `ProviderError`s and the Kimi capture bodies.
2. `usage_pause.rs`: `UsagePause`, `PauseInfo`, `PauseEnd`, driver/probe loop (tokio paused time
   in tests); `SessionState` owns one and exposes `total_paused`.
3. Wire `wait_open` before each provider call (drive, guardian, one-shot jobs); change `drive` to
   engage without consuming retries; settle `Failed` with the usage-limit message on cancel/cap.
4. Guardian: carry the cause on `Adjudication`; `permissions.rs` pause-and-re-adjudicate branch;
   `FailClosed::UsagePauseEnded` and its audit/`Converted` label.
5. Ledger/audit/telemetry records; elapsed-clock subtraction in `fontaine-agents/src/tree.rs`.
6. Approver `usage_paused` + TUI status line, countdown, keys, switch-account hook.
7. Headless flags, config keys, exit code 3, `usage_limit` result subtype, stream events, schema.
8. Docs and CHANGELOG; add the `## Decisions` lines.

## Files to Modify

- New: `rust/fontaine-core/src/usage_limit.rs`, `rust/fontaine-core/src/usage_pause.rs`
- `rust/fontaine-core/src/{turn,permissions,gate,ledger,audit,quota,config,lib}.rs`
- `rust/fontaine-guardian/src/{guardian,verdict}.rs`
- `rust/fontaine-agents/src/tree.rs`
- `rust/fontaine-tui/src/{lib,app,spinner}.rs` (status line, keys)
- `rust/fontaine/src/{headless,lib}.rs`, `docs/schemas/headless-v1.schema.json`
- `docs/{headless,tui,guardian,configuration}.md`, `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

## Trade-offs

- **Global pause vs per-account pause.** A pause scoped to the exhausted account would let the
  rest of the run continue on, say, a healthy main account while the guardian's is spent; but then
  gated calls pile up denied or pending with no single state to show, and the issue asks for the
  whole run to stop. One global pause is simpler and fail-closed.
- **Pause in the guardian crate vs in core.** Putting `wait_open` in `fontaine-guardian` would
  pull session state into a crate meant to be lift-out-able; carrying the cause out and pausing in
  core keeps it dependency-light.
- **Header/code classification vs text matching.** Matching "usage limit" in messages would be
  brittle and breaks the no-text-matching rule; structured codes and `Retry-After` magnitude
  suffice for the providers captured.
- **Probe vs wait for the reset.** Waiting only to `reset_at` is exact when known; the margin plus
  a probe covers early/late resets and unknown times at one cheap request per interval.

## Decisions

- Automatic resume is a deliberate deviation from Claude Code, which per the issue reports its
  limit and reset time and then stops. The implementer first checks Claude Code's behaviour
  (`claude -p` cannot provoke a spent window on demand; use its documented behaviour and the
  issue's account) and adds the dated line to the master plan's `## Decisions`, under Providers.
- Headless exit code 3 is the distinct "usage window spent" code; the cap default (6h) exceeds one
  5-hour window.

## Documentation

`docs/headless.md` (flags, exit code 3, events), `docs/tui.md` (status line, keys),
`docs/guardian.md` (a spent guardian account pauses rather than denies; cancel denies),
`docs/configuration.md` (keys), the schema file, `CHANGELOG.md`. No plan-section citations in docs.

## Testing Strategy

- Unit: classifier on constructed provider errors (spent code, long `Retry-After`, short
  `Retry-After` stays transient, header/body reset extraction, no text matching); pause state
  machine under tokio paused time (engage idempotent, deadline, resume, cancel, probe re-engage,
  total_paused); `drive` does not consume retries across a pause and settles `Failed` on cancel.
- Integration (`rig_core::test_utils` models, `wiremock`): a guardian whose provider answers the
  Kimi 429 body keeps the call pending, then re-adjudicates and allows/denies by verdict after
  reset; cancel yields `UsagePauseEnded` Deny; a 429 never produces an Allow. Headless: `exit`
  and expired cap give exit code 3 and the `usage_limit` result; `stream-json` events validate
  against the schema. Regression test pinning the bug in the issue: spent guardian window must not
  produce a series of `GuardianUnavailable` denials.
- TUI: `TestBackend` for status line/countdown and keys; a tmux repro of the live-area layout.

## Open Questions

- Which exact error codes/headers do the Anthropic subscription and Anthropic API accounts return
  for a spent window (as opposed to an API rate limit)? Populate `WINDOW_CODES` from captures
  under `research/`; unrecognised providers keep today's behaviour.
