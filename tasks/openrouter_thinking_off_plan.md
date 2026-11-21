# OpenRouter Thinking Off Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/739

## Overview

An `openrouter` account cannot turn thinking off on the guardian's and the scanner's forced-call
requests, so a reasoning model such as `deepseek/deepseek-v4.1-flash-1017` thinks on them and runs
out of output budget (`finish_reason=Length`) on some cases. This plan lets an `openrouter` account
set `thinking_off = true`, which adds OpenRouter's reasoning-disabled parameter to those requests.

## Current State

- `Provider::accepts_thinking_off` (`rust/fontaine-sdk/src/core/providers/mod.rs:154`) defaults to
  false; only `databricks` overrides it. `config.rs:1806` rejects `accounts.<name>.thinking_off` on
  any other kind with "not used by kind".
- `AccountClient::thinking_off(model)` (`providers/mod.rs:338`) defaults to `None`. `ModelHandle`
  carries the result (`models.rs:289`) and the guardian, scanner, goal, hook and eval call sites
  already pass `handle.thinking_off` as additional request params. No wiring beyond the provider is
  needed.
- `openrouter.rs` has a `Connected` client holding `client`, `caching`, `warn`; it does not
  implement `thinking_off`. `connect` has the `Account` in `ctx.account`.
- `databricks.rs:472` is the precedent: a free `thinking_off(account, model, ..)` function, a
  `thinking_off: Option<bool>` field on the client set from `ctx.account.thinking_off`, and a unit
  test of the function.
- `tests/providers.rs:342` (`thinking_off_belongs_to_databricks_only`) pins which kinds accept the
  key.

## Design

- `OpenRouter::accepts_thinking_off` returns true.
- `Connected` gains `thinking_off: bool`, set from `ctx.account.thinking_off.unwrap_or(false)`.
- `AccountClient::thinking_off` for `Connected` returns
  `Some(json!({"reasoning": {"enabled": false}}))` when the flag is set, else `None`. A free
  function `thinking_off(account: Option<bool>) -> Option<Value>` holds the logic so a unit test can
  call it, matching the Databricks shape.
- The value is model-independent: OpenRouter normalizes `reasoning` across upstreams and drops it
  for a model that does not support it.
- Unlike `databricks`, the default is **off** (unset equals `false`): OpenRouter routes to many
  upstreams, some of which cannot disable reasoning, and the issue asks for an opt-in key.

## Implementation Steps

1. `rust/fontaine-sdk/src/core/providers/openrouter.rs`: `accepts_thinking_off`, the `thinking_off`
   field, the free function and the `AccountClient::thinking_off` method; unit test beside it.
2. `rust/fontaine-sdk/tests/providers.rs`: extend the key-acceptance test so `openrouter` accepts
   `thinking_off` and a non-accepting kind (`ollama`) still rejects it (rename the test to say
   "belongs to databricks and openrouter").
3. `rust/fontaine-sdk/tests/account_session.rs`: using `openrouter_session` (line 428), add a test
   that with `thinking_off = true` the guardian's chat request body carries `reasoning.enabled ==
   false`, and that without the key the body has no `reasoning` key.
4. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/openrouter.rs`
- `rust/fontaine-sdk/tests/providers.rs`
- `rust/fontaine-sdk/tests/account_session.rs`
- `docs/configuration.md`, `docs/guardian.md`, `docs/providers.md`
- `CHANGELOG.md`

## Trade-offs

- **Opt-in vs. on by default (Databricks style).** Default-on would change requests on every
  existing OpenRouter guardian and send a parameter to upstreams whose behavior on it is unmeasured.
  Opt-in changes nothing for existing accounts.
- **Per-model rule vs. one value.** OpenRouter's `reasoning` object is gateway-normalized, so no
  per-wire or per-family table is needed.

## Documentation

- `docs/configuration.md`: the `thinking_off` row (line 204) names `openrouter` as well, with its
  opt-in default and the `{"reasoning": {"enabled": false}}` value; the Databricks paragraph at
  line 383 stays.
- `docs/guardian.md` ("Choosing the guardian model"): one sentence that an `openrouter` account
  sends `{"reasoning": {"enabled": false}}` on the guardian's and the scanner's calls when
  `thinking_off = true`.
- `docs/providers.md` (line 19): add `thinking_off` to the `openrouter` row's extras.
- `CHANGELOG.md`: an `### Added` entry under `[Unreleased]`, via the PR step.

## Testing Strategy

All automated with wiremock; no live API. Covered: the unit test of the free function (set, unset,
`false`); the config test for key acceptance; the session test that pins the request body for the
guardian with and without the key.

## Manual Verification

Re-run the live scorecard from the issue (`live:router/deepseek/deepseek-v4.1-flash-1017`) with
`thinking_off = true` on the account and confirm fewer `finish_reason=Length` cases. Not automated:
it depends on the live upstream honoring the parameter, which a stand-in cannot show.

## Open Questions

None.
