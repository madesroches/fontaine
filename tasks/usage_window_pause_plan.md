# Pause the run when an account's usage window is exhausted Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/115

## Overview

A subscription account that has spent its usage window answers every request with a 429 until the
window resets. Fontaine treats that like any transient 429: three tries, then the run fails (the non-retryable `claude-subscription` limit fails on the first), or
the guardian's account is the exhausted one and every gated call is denied with
`GuardianUnavailable` while background agents fail one by one. This adds an explicit, session-wide
**usage pause**: a spent window is classified apart from a transient rate limit, the whole run
(main agent, guardian, subagents) stops at its next provider call, pending gated calls stay
pending, and the run resumes when the window resets, on a deadline, with fail-closed handling on
every other exit. The TUI shows a countdown with resume / cancel / switch-account actions; headless
`-p` waits up to a configurable cap or exits at once with a distinct exit code.

## Current State

- `rust/fontaine-core/src/turn.rs`: `drive` retries a `retryable` failure `MAX_RETRIES` (2) times
  with a 250 ms doubling `BACKOFF` (`turn.rs:33`, `turn.rs:562-615`); `drive` returns on a non-retryable failure at `turn.rs:602`,
  and `claude-subscription`'s `Limit` is non-retryable, so that provider never retries. A 429 is retryable, so a
  spent window is retried like a hiccup and then settles as `TurnOutcome::Failed`.
- `turn.rs:472-550`: `quota_reply` / `falls_through` / `error_falls_through` already recognise
  quota-class replies by status or machine code (`QUOTA_CODES`) and for `claude-subscription` by
  `ErrorKind::Limit`; a model list falls through to its next entry on them. Nothing matches on
  error text. This classifier cannot tell "window spent" from "slow down".
- The guardian crate wraps rig errors (`fontaine-guardian/src/guardian.rs`). A guardian 429 is
  `FailClosed::Error` (`guardian.rs:337-350` flattens the error to an explanation `String`), not an
  unavailable kind: `is_unavailable()` matches only `Unavailable | NotSignedIn`
  (`verdict.rs:166-168`), so the `guardian_unavailable` branch (`permissions.rs:1910-1954`:
  retry / manual / deny when a human is present, else deny) never fires for a 429. `Adjudication`
  is `Arc`-shared and cloned (`composition/eval.rs:47,90`, `composition/mod.rs:596`). A failed
  composition becomes `Decided::Deny` with no `Adjudication` (`composition/mod.rs:635-639`). The
  guardian retries twice more inside its 30 s budget with no backoff (`structured.rs:225-263`) and
  has no model list.
- `drive` returns only `TurnFailure { falls_through }`; callers decide fall-through after it
  returns (TUI `ui/turn.rs:905-909`, `headless.rs:711`, `child_runner.rs:1066-1070`,
  `ModelChain::run_through` `model_roles.rs:342`).
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

A reply is a **spent window** when either (a) its machine code is in `WINDOW_CODES`
(`exceeded_current_quota_error`, `GoUsageLimitError`, …), with or without an HTTP status (a
mid-stream error carries none), or (b) status is 429/402 and its `Retry-After` exceeds `TRANSIENT_RETRY_AFTER_MAX` (5 minutes, far
above the 250 ms backoff ceiling). `claude-subscription`'s plan-limit form of `ErrorKind::Limit` is a spent
window; `billing_error` and the generic `rate_limit` form (`protocol.rs:861,872,1310`) stay
failures, since a billing failure never resets. The Anthropic subscription is served only by the
`claude-subscription` kind, so there is no Anthropic `WINDOW_CODES` entry; an Anthropic API-key
`rate_limit_error` is a per-minute limit rig classes as transient (`provider_response.rs:268`) and
stays transient. `drive` classifies with `usage_limit()` before its `!retryable` return, so the
non-retryable `Limit` is seen, and settles a usage-limit failure without waiting (see Per call
site). Anything else
stays a transient 429: the existing retry with backoff, unchanged. `WINDOW_CODES` is a flat
slice like `QUOTA_CODES` (`turn.rs:475-488`), so a new provider adds a row, not a branch; the
account (or its `QuotaSource`) is passed separately to the reset-time step. `WINDOW_CODES` entries are not in `QUOTA_CODES`' generic set, so `quota_reply`
(`turn.rs:494`) also matches on `WINDOW_CODES`, which keeps a status-less mid-stream window error
falling through: `falls_through` stays true for them, so a model list still moves to its next entry before any pause is engaged (the pause is the
outcome when no next entry exists or the chain is spent).

Reset time, in order: `Retry-After` / reset header via rig's `ErrorReport::retry_after(now)`
(rig-core 0.46.0 `error/retry.rs:24`; headers are absent, so `None`, for mid-stream errors, which
fall back to the body codes and quota source below); reset in the body's structured fields; the
account's `QuotaSource` windows (the soonest `reset_at` of a window at or above 1.0 used ratio);
else unknown. For `claude-subscription`, `ClaudeError` carries no reset time (`mod.rs:86-90`) and
the rate-limit event handling drops what is needed (`protocol.rs:884` ignores status,
`usage.rs:79-81` drops windows lacking utilization, `protocol.rs:889` skips frames with no
windows), so a `rejected` frame carrying only `resetsAt` never updates the reset; the provider
records that frame's `resetsAt` and status so the reset time can be found.

### The pause (`fontaine-core/src/usage_pause.rs`, new)

A session-wide `UsagePause` owned by `SessionState`, built on a `tokio::sync::watch` channel:

```rust
pub struct PauseInfo { role: UsageRole, account: String, reset_at: Option<SystemTime>,
                       since: Instant, deadline: Option<Instant> }
pub enum PauseEnd { Reset, Probed, Resumed /* user */, Cancelled, CapReached }
impl UsagePause {
    pub fn engage(&self, info: PauseInfo);              // idempotent; first engager wins
    pub async fn wait_open(&self) -> Option<PauseEnd>; // None when not paused
    pub fn resume(&self); pub fn cancel(&self);
    pub fn total_paused(&self) -> Duration;             // for budget/elapsed clocks
}
```

Callers treat `Some(Cancelled | CapReached)` as failure; the full enum is kept for the ledger
record. `wait_open` is awaited **before every provider call**, outside any hook or judge timeout:

- Main agent and subagent streams: a per-request gate in an `on_completion_call` hook, added at
  `agent/build.rs:388-397` for the main agent and next to `KillHook` (`child_runner.rs:741`) for
  subagents, so each request, not only each `drive` attempt, is gated.
- Role-chain compaction (`agent/goal.rs:196-245`), advisor, env scan (`project_env.rs:383`) and
  critique (`auto_mode.rs:300`): through `Switcher::run_through` (`models.rs:1086`).
- Not covered by `run_through`, so each gets its own `wait_open`: default compaction (when no
  compaction role chain exists it goes through `session.compact_logged_via`,
  `agent/goal.rs:152-162`), the AFK evasion assessment (`assess_evasion`, `agent/afk.rs:59` →
  `fontaine-guardian/src/evasion.rs:261`, called from `ui/mod.rs:580` and `headless.rs:1213`),
  `/btw` (`SideQuestion::ask`),
  the goal judge (`goal.rs:913,975`), prompt-type user hooks that use the judge
  (`user_hooks.rs:499`), the LLM scanner (`scanner_stage.rs:55-74`), composition screen reads
  (`permissions.rs:1855-1863`), and the guardian. Hook timeouts (30 s, `permissions.rs:795,1606`)
  and the judge timeout wrap only the call after the wait, so a spent window is never turned into
  a timeout and a fail-closed hook Deny. Hook `run_through` runs once for jobs that use it.

A pause is an explicit state; in the TUI it has no deadline (`PauseInfo.deadline` is `None`)
because the user can cancel; headless sets it from the cap.

Driver: a task spawned and owned by `UsagePause`/`SessionState`, not by a waiter, because caller
futures are dropped (Esc drops the run, `ui/turn.rs:1121-1139`; a killed subagent is dropped after
`KILL_GRACE`, `child_runner.rs:1053-1062`; headless `kill_all`, `headless.rs:1360`), which would
strand every other waiter. If `reset_at` is known, sleep until it plus a small margin
(30 s), then **probe** by letting exactly one caller through (`QuotaSource::fetch` when the account
has one, else the next real call); a probe that 429s again re-engages with the new reset time. If
unknown, probe at intervals starting at 1 minute, doubling to a 15-minute ceiling. A probe that
succeeds ends the pause `Probed`/`Reset` and releases every waiter.

Per call site:
- **Main/subagent `drive`**: classifies with `usage_limit()` before the `!retryable` return and
  settles a usage-limit failure without waiting. The callers that own the model list (TUI turn,
  headless, `child_runner`, `ModelChain::run_through`) engage the pause only once the list is
  spent, wait, then the caller clears its `Tried` set (`ui/turn.rs:904`) and restarts from the head
  of the model list. On
  `Some(Cancelled|CapReached)` the turn settles `Failed` with the usage-limit message (names
  account, reset time).
- **Guardian**: `Adjudication` gains the failure's cause as
  `Option<Arc<dyn Error + Send + Sync>>` (it is cloned, so not a `Box`), and so does the
  composition's fail-closed `Decided::Deny`, so core classifies it (the guardian crate stays free
  of core types and gains no new dependency). When the cause is a spent window, tested
  independently of `is_unavailable()` and before, independent of, the `no_human()` check
  (never calling the human prompt in headless or AFK), `permissions.rs` engages the pause
  *instead of* the Deny, after recording the failed attempt's guardian usage (`add_guardian_usage`
  runs after the unavailable branch, `permissions.rs:1955-1960`), awaits `wait_open`, and re-runs the guardian check from the start. A pending call is neither denied nor
  run while waiting. On `Cancelled`/`CapReached` the call is **Denied**, with a new fail-closed
  kind `UsagePauseEnded` (error text may explain a Deny, never change it). `UsagePauseEnded` does not count toward the breaker (it is a user or cap
  decision, not a guardian failure; failed compositions do count, `permissions.rs:1888`) and is
  excluded from `FAIL_CLOSED_WEIGHT` in `afk_report.rs` (:109), as the breaker exclusion is. A 429
  never becomes an Allow; if resume itself fails, the call is denied exactly as today.
- Calls already executing (a running shell) are not interrupted; only provider calls pause.

### TUI

Status line while paused: `paused — <account> usage window spent, resets 14:30 (in 2h 14m)`, or
`… reset time unknown, next check in 4m`. A dialog-less key row offers `r` resume now (an early
probe), `c` cancel, `s` switch account. Switch reuses `use_guardian` (as `answer_unavailable` does,
`ui/commands/model.rs:280-320`, `agent/models.rs:621`) for the guardian role and the
between-attempts `fall_through` point (`models.rs:391`) for the main role: `/model` and
`/guardian-model` are not in `runs_mid_turn` (`commands.rs:137-161`), so a mid-turn pick would only
wait as `pending_pick`. The exhausted role's account is replaced for the live session with a
validated alternative, then `resume`. Switching is not offered for a subagent's account. The `Approver` trait gains `usage_paused(info) -> BoxFuture<PauseChoice>` (default:
wait), mirroring `guardian_unavailable`. `AfkApprover` (`permissions.rs:179-240`) wraps every
session's approver (`agent/setup.rs:548`) and forwards each method explicitly, so it gets a
forwarding `usage_paused`; AFK waits (it never prompts). Headless uses `DenyAll`
(`fontaine/src/lib.rs:1098`), so its wait/exit/cap come from flags and config, not the trait
default.

### Headless `-p`

New flags and config, user-scope for the waiting cap (project scope may only choose exit, a
tightening). Config keys: `headless.max_usage_wait_secs` (integer seconds, following
`guardian.timeout_secs`) and `headless.on_usage_limit` (`wait`|`exit`). In
`fontaine-core/src/config_scope.rs`, `headless.on_usage_limit` is a `Rule::if_project_local` row with
an is-exit predicate and `headless.max_usage_wait_secs` a `Rule::user_or_cli` row (as
`permissions.auto_tracked_edits` is at ~:196); the per-key scope table in `docs/configuration.md`
(~:105) gains both. Flags: `--on-usage-limit=wait|exit` (default `wait`) and `--max-usage-wait=<duration>`
(default 6h, longer than a 5-hour window). The flag value is parsed by the "6h"-style duration parser
moved from the TUI (`unit_seconds`/`compact_interval`, `fontaine-tui/src/commands.rs:692-711`) into
`fontaine-core`; the config key takes integer seconds. `exit` and an expired cap end the run with exit code 3
and a `result` subtype `error_usage_limit` (the `Subtype` enum, `headless.rs:228-243`, and the
schema enum use `success`/`error_*`) carrying the account and reset time. A pause that ends
`Cancelled`/`CapReached` from any role, including the guardian, ends the run with exit 3 and that
result (headless exits only 0/1/2 today, and a Deny alone would let the model keep making gated
calls). `UsagePauseEnded` is added to the closed `fail_closed_reason` enum in
`docs/schemas/headless-v1.schema.json` (~:213-215; labels from `verdict.rs:173-178`).
`stream-json` emits additive `usage_paused` / `usage_resumed` events.
`docs/schemas/headless-v1.schema.json` is updated (additive within `v1`): a `$defs` entry and a
top-level `oneOf` entry per new event (lines ~5-16) and the new subtype in the enum (~283-289).

### Ledger, audit, accounting

- Ledger `Record::UsagePaused { role, account, reset_at }` (like `ModelFellThrough`) /
  `Record::UsageResumed { end: PauseEnd }`; `PauseEnd` derives `Serialize`/`Deserialize` with
  snake_case (`Record` is serde-tagged, `ledger.rs:30-32`). The audit log records the
  same. Telemetry emits two events, `usage_paused` and `usage_resumed`, with the same fields as the
  ledger records, as `tracing::info!(target: "fontaine::telemetry", …)` calls (e.g. `turn.rs:810-819`).
- Agent budgets are token/cost, so waiting spends nothing. Tree nodes hold raw `started`/`ended` `Instant`s
  (`tree.rs:340-341,545-547,1970-1971`; `started` is reset on revive, `tree.rs:1298-1299`; age uses
  `started.elapsed()`, `tree.rs:1849`). A node snapshots `total_paused()` when it starts (and is
  revived) and when it ends, and subtracts only the difference, so a subagent's displayed run time
  and wall-clock accounting stop during the pause without undercounting nodes that started after an
  earlier pause. The paused-time subtraction is carried into the tree snapshot that the TUI agent
  list reads (`agent_list.rs:84,106-108`), and the spinner's turn clock subtracts `total_paused()`
  (`spinner.rs:127-131`).
- Not paused by this: tool execution already in flight, MCP, hooks.

## Implementation Steps

1. `usage_limit.rs`: classifier, flat `WINDOW_CODES` slice, reset-time extraction via
   `ErrorReport::retry_after`; `claude-subscription` provider records the rejected rate-limit
   frame's `resetsAt` and status; unit tests over constructed `ProviderError`s, the Go 429
   capture, and the `exceeded_current_quota_error` body quoted in issue #115 (not a `research/`
   capture).
2. `usage_pause.rs`: `UsagePause`, `PauseInfo`, `PauseEnd`, driver/probe loop (tokio paused time
   in tests); `SessionState` owns one and exposes `total_paused`.
3. Wire `wait_open` at every provider call site listed under The pause (an `on_completion_call`
   gate for streams; `run_through` for its jobs; explicit calls at `/btw`, judge/prompt hooks,
   scanner, composition screen, guardian), outside hook and judge timeouts; `drive` classifies
   before its `!retryable` return, and the callers that own the model list engage the pause once
   it is spent, then clearing `Tried` and restarting from the head of the list; settle `Failed` with the usage-limit message
   on cancel/cap.
4. Guardian: carry the cause as `Option<Arc<dyn Error + Send + Sync>>` on `Adjudication` and the
   composition's fail-closed `Decided::Deny`; `permissions.rs` pause-and-re-adjudicate branch;
   `FailClosed::UsagePauseEnded` and its audit/`Converted` label.
5. Ledger/audit/telemetry records; per-node `total_paused()` snapshots in
   `fontaine-agents/src/tree.rs`.
6. Approver `usage_paused` (with the `AfkApprover` forwarder) + TUI status line, countdown, keys, switch-account hook.
7. Headless flags, config keys, exit code 3, `error_usage_limit` result subtype, stream events, schema (including the `UsagePauseEnded` `fail_closed_reason` value).
8. Docs and CHANGELOG; add the `## Decisions` lines.

## Files to Modify

- New: `rust/fontaine-core/src/usage_limit.rs`, `rust/fontaine-core/src/usage_pause.rs`, `rust/fontaine-core/src/duration.rs` (the moved duration parser)
- `rust/fontaine-core/src/{turn,permissions,gate,ledger,audit,quota,config,config_scope,lib}.rs`
- `rust/fontaine-core/src/{child_runner,models,model_roles,goal,user_hooks,scanner_stage,project_env,side_question,afk_report,duration}.rs`
- `rust/fontaine-core/src/agent/{build,goal,setup,afk}.rs`
- `rust/fontaine-core/src/providers/claude_subscription/{protocol,usage,mod,cli}.rs`
- `rust/fontaine-guardian/src/{guardian,verdict,structured,evasion}.rs`, `rust/fontaine-guardian/src/composition/{mod,eval}.rs`
- `rust/fontaine-agents/src/tree.rs`
- `rust/fontaine-tui/src/{lib,app,spinner,statusbar,view,agent_list,commands}.rs` (status line, keys, clocks), `rust/fontaine-tui/src/ui/{turn,mod}.rs`, `rust/fontaine-tui/src/ui/commands/model.rs`
- `rust/fontaine/src/{headless,lib,auto_mode}.rs`, `docs/schemas/headless-v1.schema.json`
- `rust/fontaine-evals/src/live.rs` (destructures `Decided::Deny` without `..`)
- `docs/{headless,tui,guardian,configuration,claude-subscription,architecture,telemetry,agents}.md`, `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

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
- Automatic resume supersedes the master plan's Providers decision (`tasks/fontaine_harness_plan.md:987`: plan-limit failure reported without retry or alternate route) for spent windows.
- Global pause, not per-account: one session-wide state to show, and gated calls never pile up denied or pending across accounts.
- Probe, not wait only: waiting to `reset_at` plus a margin and one probe covers early/late resets and unknown times at one cheap request per interval.
- The `error_usage_limit` result subtype is Fontaine-only and deviates from the master Decision at `tasks/fontaine_harness_plan.md:819`.
- Exit code 3 under `-p` means usage window spent; for `fontaine update` it means "not self-updatable" (`update/mod.rs:21`, `docs/installing.md:203`).
- An Anthropic API-key spend/credit limit keeps today's behaviour: no distinct machine code, text matching is forbidden, and rig already classes `insufficient_quota`/`billing_hard_limit_reached` as final via `QUOTA_CODES`.
- Headless exit code 3 is the distinct "usage window spent" code; the cap default (6h) exceeds one
  5-hour window.

## Documentation

`docs/headless.md` (flags, exit code 3, events), `docs/tui.md` (status line, keys),
`docs/guardian.md` (a spent guardian account pauses rather than denies; cancel denies),
`docs/configuration.md` (keys; `:650-665` "When the whole list is spent the turn fails"),
`docs/claude-subscription.md` (`:164,174` "reported once, without a retry"),
`docs/architecture.md` (`:200-208` ledger record kinds), `docs/telemetry.md` (`:129,152-166` events
and fail-closed reasons), `docs/agents.md` (`:103`), the schema file, `CHANGELOG.md`. No plan-section citations in docs.

## Testing Strategy

- Unit: classifier on constructed provider errors (spent code, long `Retry-After`, short
  `Retry-After` stays transient, header/body reset extraction, no text matching); pause state
  machine under tokio paused time (engage idempotent, deadline, resume, cancel, probe re-engage,
  total_paused); a cancelled pause settles the turn `Failed`.
- Integration (`rig_core::test_utils` models, `wiremock`): a guardian whose provider answers the
  Kimi 429 body keeps the call pending, then re-adjudicates and allows/denies by verdict after
  reset; cancel yields `UsagePauseEnded` Deny; a 429 never produces an Allow. Headless: `exit`
  and expired cap (also from a guardian-side pause) give exit code 3 and the `error_usage_limit` result; `stream-json` events validate
  against the schema. Regression test pinning the bug in the issue: spent guardian window must not
  produce a series of `GuardianUnavailable` denials.
- TUI: `TestBackend` for status line/countdown and keys.
