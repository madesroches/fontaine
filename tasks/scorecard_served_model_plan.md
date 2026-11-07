# Record the Served Model on Scorecards Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/440

## Overview

`kimi/kimi-for-coding` is an alias that moves: on 2026-11-05 `GET /models` lists it as "K2.8
Preview" (1,048,576-token window). A scorecard records only the alias, so a later re-run cannot be
compared with the row, nor can a screen replay of its verdicts be tied to a model. The live eval
will record the provider's `/models` entry (id, display name, context length) for the guardian
model in the scorecard `summary`, and the docs table will print it. The
control row is then re-measured once.

## Current State

- `fontaine-evals/src/runner.rs` `Summary` (line ~265) is the scorecard header; `guardian` is only
  the `live:<account>/<model>` label. 
- `fontaine-evals/src/live.rs` `connect()` resolves the account through `Switcher`. The model list
  is only used for validation: `AccountClient::list()` (`fontaine-core/src/providers/mod.rs:233`)
  returns `Vec<String>` of ids; `openai_compatible.rs:86` drops everything but `m.id` from rig's
  `list_models()`.
- `docs/evals.md` tables (e.g. line ~518) show `<account>/<model>` rows without a served version.
- The capture `research/kimi-harness/captures/e1_models_kimi_coding.json` shows the Kimi coding
  endpoint's entry shape: `id`, `display_name`, `context_length`.

## Design

**Type** (in `fontaine-core/src/providers/mod.rs`):

```text
pub struct ServedModel { id: String, display_name: Option<String>, context_length: Option<u64> }
```

**Trait**: add `AccountClient::served_model(&self, model) -> BoxFuture<'_, Option<ServedModel>>`
(no error type) with a default returning `None` ("when the endpoint offers it"). Failures are
reported by the implementation or eval caller via `eprintln!("fontaine-evals: ...")`. Open/closed: only kinds that
can supply more than an id override it.

**openai-compatible**: override it. rig's `list_models()` item type exposes only `id`, so issue
the same `GET {base_url}/models` through the account's network client and parse `data[]` (or a
bare array, as the Kimi coding endpoint returns), picking the entry whose `id` equals the model and
reading `display_name` and `context_length` (fall back to `limit.context`). Any failure or absent
entry yields `None`. `connect` always builds a small models fetcher on `Connected` (today it holds
only the rig client and `quota`, which is `Some` only for Kimi-quota accounts): a reqwest client
from `ctx.network`, the trimmed base URL and the optional bearer api key, reusing the helpers in
`kimi_quota.rs` (`reqwest_client`, the 10s `REQUEST_TIMEOUT`, bearer auth, `without_url()` error
mapping).

**Handle accessor**: `ModelHandle.client` is private to `fontaine-core/src/models.rs`, and `connect()` only sees the resolved `ModelHandle`. Add `ModelHandle::served_model(&self) -> BoxFuture<'_, Option<ServedModel>>`, which calls the client's `served_model(&self.reference.model)` and yields `None` when the handle holds no client.

**Eval**: `Live` gains `served: Option<ServedModel>`, filled in `connect()` from the guardian handle's `served_model()` after it resolves, with the fetcher's 10s `REQUEST_TIMEOUT`. `Summary` gains
`served_model: Option<ServedModel>` (`skip_serializing_if = "Option::is_none"`),. The served data passes through `SummaryParams` into `summarize()`, and `print_summary` reads it from
`Summary`, printing one line `served: <display_name> (<id>), <context> tokens` when present. The sidecar `Header`
is unchanged: the served model drifts independently of the resume identity, and a resumed run
records what answered at its end.

## Implementation Steps

1. `fontaine-core/src/providers/mod.rs`: add `ServedModel` and the defaulted trait method; `fontaine-core/src/models.rs`: add `ModelHandle::served_model`.
2. `fontaine-core/src/providers/openai_compatible.rs`: build the models fetcher in `connect`, store it on `Connected`, and implement the method with a parsed `/models` fetch.
3. `fontaine-evals/src/live.rs`: capture it in `connect()`; expose it on `Live`.
4. `fontaine-evals/src/runner.rs`: add the field to `Summary`, pass the served data through `SummaryParams`
   into `summarize()`, print the line from `Summary`.
5. `docs/evals.md`: describe the field near the scorecard prose (~lines 249 and 336; no
   scorecard-field schema section exists) and add a footnote under the guardian tables that name `kimi`;
   `CHANGELOG.md` entry under a new `### Added` heading in `[Unreleased]`.
6. Re-run the control row (`--guardian live:kimi/kimi-for-coding`, full profile, default timeout)
   once, write `evals/scorecards/kimi-for-coding-k2-8.json` (the original stays) and
   compare the result against the 89.7% row (`docs/evals.md` ~line 518, from
   `kimi-kimi-for-coding-d1740.json`); note the delta in `docs/evals.md`. This is a measurement, not a ship decision, and needs live keys (a human step
   if the key is unavailable; then state so in the PR).

## Files to Modify

- `rust/fontaine-core/src/providers/mod.rs`, `providers/openai_compatible.rs`, `models.rs`
- `rust/fontaine-evals/src/live.rs`, `runner.rs`
- `docs/evals.md`, `CHANGELOG.md`, `evals/scorecards/` (new file)

## Trade-offs

- Fetching `/models` in the eval only (bypassing the trait) was rejected: the account's base URL,
  auth and network settings live behind `AccountClient`, and other kinds can later expose a version.

## Decisions

- AFK: The cascade's screen model is out of scope (no separate `Live` exists for it, it can be a comma-joined list, and its accounts get no override); only the guardian model's served entry is recorded, and the issue's screen-model mention is deferred.
- AFK: `connect()` reaches the account client through a new `ModelHandle::served_model` accessor in `fontaine-core/src/models.rs` (the handle's `client` stays private).
- Failure to fetch metadata never fails a run; the field is simply absent.
- `opencode-go` and `openrouter` do not override `served_model` in this change; the default `None` applies.

## Documentation

`docs/evals.md` (scorecard fields, table, control-row note), `CHANGELOG.md`.

## Testing Strategy

- Unit tests with `wiremock` for the `openai-compatible` `served_model`: wrapped `{data:[...]}`
  and bare-array shapes, missing entry, HTTP error, missing optional fields (all yield the right
  `Some`/`None`).
- `fontaine-evals` test that a `Summary` with `served_model` serializes the keys and one without
  omits them.
- A default-method test that a kind without an override yields `None`.
