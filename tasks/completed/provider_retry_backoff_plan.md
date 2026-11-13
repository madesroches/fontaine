# Provider Retry with Backoff Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/600

## Overview

A subagent died on a single transient `503` and its parent saw a terminal failure. `turn::drive`
already retries retryable failures, but only twice, on a fixed 250 ms doubling with no jitter, no
`Retry-After`, no user-visible notice and no configuration. This plan upgrades that one loop into a
configurable policy: more attempts, exponential backoff with jitter, `Retry-After` honored under a
capped total delay, a `Approver::warn` line per retry, and a `[retry]` section settable in user scope.
Because `drive` serves the root agent and every subagent, one change covers both.

## Current State

- `rust/fontaine-core/src/turn.rs`: `MAX_RETRIES = 2`, `BACKOFF = 250ms`; `drive` loops
  `0..=MAX_RETRIES`, calls `drive_once`, and on `DriveEnd::Failed` retries when
  `retryable(&error)` (`PromptError::Provider(e).is_retryable()` / `Report.is_retryable()`, rig's
  classification) unless `prompt_usage_limit` says a spent window. The retry sleeps
  `BACKOFF * (1 << attempt)` and re-drives with `RETRY_NOTE`; the failed attempt's messages were
  saved by `RecoveryHook` (`turn_recovery.rs`), so only the model request is re-issued, never a tool
  call.
- `drive` is called by `run_turn` (root) and `child_runner.rs:~1092` (subagents).
- `claude-subscription` CLI server errors already classify retryable
  (`providers/claude_subscription/protocol/tests.rs`: `Native::Server` => retryable).
- `usage_limit.rs` already parses `Retry-After` (`retry_after(response, now)`, private) and defines
  `TRANSIENT_RETRY_AFTER_MAX = 300s`: longer than that is a usage window, handled elsewhere.
- Existing tests in `turn.rs` cover `retryable` classification; `docs/architecture.md` ("Runs and the
  turn runner") documents "retried twice with a short backoff".
- Config precedent: `[goal]` / `[headless]` sections in `config.rs` (`*Section` with
  `deny_unknown_fields`, validation near `config.rs:2026`, merge near `:3221`), the key catalog
  `config_catalog.rs`, the scope rules `config_scope.rs`, and the per-key table in
  `docs/configuration.md`.
- `SessionState` (`gate.rs`) holds the `Approver` (`approver.warn`) and is reachable from `Drive`.

## Design

**Policy type** (new, `fontaine-core/src/retry.rs`):

```
pub struct RetryPolicy { max_retries: u32, base_delay: Duration, max_delay: Duration, max_total_delay: Duration }
```

Defaults: `max_retries = 4` (5 attempts), `base_delay = 500ms`, `max_delay = 30s` (per-wait cap),
`max_total_delay = 120s`. Pure function
`next_delay(&self, attempt, retry_after: Option<Duration>, elapsed: Duration, jitter: f64) -> Option<Duration>`:
exponential `base * 2^attempt` capped at `max_delay`, multiplied by full-ish jitter in `[0.5, 1.0]`;
order: `delay = max(jittered exponential, Retry-After)`; returns `None` (ends the retries) when
`delay > max_delay` or `elapsed + delay > max_total_delay`. A `Retry-After` above `max_delay` therefore
settles the failure now: the server asked for longer than we will wait. The jitter source is injected (a `fn() -> f64` or a parameter) so unit tests are
deterministic; production derives the fraction from the first bytes of a `uuid` v4 (already a direct
dependency of `fontaine-core`), so no dependency is added.

**Retry-After**: make `usage_limit::retry_after` `pub(crate)` and reuse it (DRY) on the provider
response (`ProviderError::provider_response()` / `ErrorReport.provider_response`). The 300 s
usage-window exit in `usage_limit.rs` applies only to status 402/429, so a 503 with `Retry-After: 600`
does reach the policy (and, exceeding `max_delay`, settles as failed).

**Quota-class statuses**: 402/429/529 (the statuses `turn.rs::quota_reply` treats as quota) get a
shorter budget than other retryable failures: at most 2 retries and a total delay of at most
`min(max_total_delay, 10s)`, so a model-list fall-through is delayed by seconds, not up to 120 s. Other
retryable failures (5xx, transport, truncated replies) use the full policy.

**Classification**: keep `retryable()` (rig's `is_retryable`) as the single decision; rig-core 0.46
already retries 408/425/429, every 5xx (including 502/504/529), truncated and stalled replies, and
transient transport errors, so nothing is added to it. Guardian denials never pass through `drive`
(they are tool-gate results, not model failures), so they are untouched.

**drive loop**: replace the constants with `params.state.retry_policy()`; track `started =
tokio::time::Instant::now()` across the loop; on a retryable failure compute `next_delay`; `None` or no attempts
left settles `TurnOutcome::Failed` exactly as today (full chain via `failure_text`). Before sleeping,
call `state.warn(...)` with the agent id, attempt `n of N`, the delay, and the error's
one-line status/class (no body: a provider's message can echo the request). The sleep must stay
cancellable the way the surrounding turn is (the future is dropped on interrupt, as today).

**Config**: new `[retry]` section (`max_retries`, `base_delay_ms`, `max_delay_secs`,
`max_total_delay_secs`), validated (`max_retries` 0..=10; delays positive; `max_delay >= base`),
merged into `Config.retry`, listed in `config_catalog.rs`, user-scope and CLI only
(`config_scope.rs`; project scope may not loosen, and retrying more is spend, so no project key at
all). `Config.retry` flows into `SessionState` (new field via its params struct) and is read through a
`retry_policy()` accessor; subagents share the session state, so the policy is the same for the tree.

**Gating unchanged**: only the model request is re-issued; `RETRY_NOTE` continues from the saved
conversation; no tool call is replayed. No change to the permission pipeline, guardian or floor.

## Implementation Steps

1. `fontaine-core/src/retry.rs`: `RetryPolicy`, defaults, `next_delay`; export from `lib.rs`.
2. `usage_limit.rs`: expose `retry_after` as `pub(crate)`; add a helper in `turn.rs` that extracts the
   `Retry-After` from a `PromptError` (provider response or report).
3. `config.rs`: `RetrySection`, validation, merge, `Config.retry`; `config_catalog.rs`;
   `config_scope.rs` (user/CLI only); regenerate any schema fixture the repo checks in.
4. `gate.rs`: carry `RetryPolicy` in `SessionState` (new `StateParams` field, which has no `Default`, so it
   is added explicitly at every construction site: `agent/setup.rs:~499` takes `Config.retry`; the
   others pass `RetryPolicy::default()`; test sites that need no delay pass a zero-delay policy) with `retry_policy()` and a `SessionState::warn` method (the
   `approver` field is private with no accessor). Update all seven sites: `agent/setup.rs:~499`, `gate.rs:~1796`,
   `permissions.rs:~2927`, `questions.rs:~96`, `steering.rs:~333`, `tests/common/pipeline.rs:~269`,
   `fontaine-evals/src/none.rs:~135`.
5. `turn.rs`: use the policy in `drive`, emit the warn line, drop `MAX_RETRIES`/`BACKOFF`.
6. Update tests broken by the new defaults: `tests/turn_recovery.rs::retries_run_out_and_the_failure_is_reported_once`
   (3 scripted turns, `request_count() == 3` becomes 5 with `max_retries = 4`) and
   `tests/usage_pause_turn.rs::a_throttle_is_still_retried_and_is_not_a_usage_limit` (5 scripted turns,
   `request_count() == 5`); test helpers use a zero-delay/small policy
   and the 3 -> 5 request-count change is explicit.
7. Docs and CHANGELOG (below).

## Files to Modify

`fontaine-core/src/{retry.rs (new), lib.rs, turn.rs, usage_limit.rs, config.rs, config_catalog.rs,
config_scope.rs, gate.rs, agent/setup.rs, permissions.rs, questions.rs, steering.rs}`,
`fontaine-core/tests/common/pipeline.rs`, `fontaine-evals/src/none.rs`,
`tests/{turn_recovery,usage_pause_turn}.rs`,
`docs/architecture.md`, `docs/configuration.md`, `docs/claude-subscription.md`, `docs/headless.md`,
`docs/agents.md` (one sentence), `CHANGELOG.md`.

## Trade-offs

- Extend `drive`'s existing loop vs a rig-level retry middleware: the repo rule is no custom provider
  layer, and `drive` already owns continuation-note retries that preserve the saved conversation; a
  transport-level retry would not cover the CLI provider's server errors.
- Policy in `SessionState` vs a global: the tree must share one policy and tests need to override it.
- Fixed attempts only vs total-delay cap: the cap bounds a subagent's stall regardless of
  `Retry-After` hints.

## Decisions

- 2026-11-13: No project-scope `[retry]` keys; retrying is spend and a repository must not raise it.
- 2026-11-13: Retries run first inside `drive`; model-list fall-through happens only once attempts are exhausted.
- AFK: Jitter fraction comes from uuid v4 bytes (uuid is already a direct dependency of fontaine-core); no rand/fastrand dependency is added.
- AFK: `SessionState`'s policy is a new explicit `StateParams` field set at all seven construction sites (StateParams has no Default), not a defaulted setter.
- AFK: Quota-class statuses (402/429/529) get at most 2 retries and a total delay of `min(max_total_delay, 10s)`, keeping model-list fall-through near today's latency; other retryable failures use the full policy.

## Documentation

- `docs/architecture.md`: replace "retried twice with a short backoff" with the new behavior.
- `docs/configuration.md`: `[retry]` key reference, per-key scope table row (user file and CLI only).
- `docs/claude-subscription.md` (~line 182) and `docs/headless.md` (~line 75): replace "retried twice".
- `docs/agents.md`: a failed subagent now means retries were exhausted.
- `CHANGELOG.md` entry.

## Testing Strategy

Unit tests (`retry.rs`): the delay grows exponentially, is capped by `max_delay`, jitter stays within
bounds, a `Retry-After` above `max_delay` returns `None`, and the total-delay cap returns `None`.
Config tests (`tests/config.rs`): defaults, user override, out-of-range refused, project scope refused.
Integration tests extend the `error_turn` deterministic-model tests in `turn_recovery.rs` with tokio
paused time (`start_paused`; the elapsed clock is `tokio::time::Instant`, so the total-delay cap is
exercisable) and a zero-jitter policy: a 429 with a short
`Retry-After` waits at least that long; a retry never re-executes a tool call (tool-call counter stays
1 across a retry after a tool result); each retry produces one `Approver::warn`; a subagent run through
`child_runner` retries the same way (pins the issue's reported bug: a worker dying on a 503). The
`claude-subscription` double (`claude_cli_double`) with a CLI-reported server error then success covers
the CLI path.
