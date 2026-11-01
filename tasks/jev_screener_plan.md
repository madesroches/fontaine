# Jev Screener Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/127 (Jev item)

## Overview

`fontaine-evals --guardian screen:<model>` already scores a typed-decision model over
`/v1/systemone`. Jev, the hosted closed-weights model, is now reachable through OpenRouter, which
serves the same endpoint shape at `https://openrouter.ai/api/v1/systemone` behind a bearer key.
This adds the two things the scorer lacks to run Jev: an auth option for the key file, and the
spend OpenRouter reports per call, so a run can report its cost and be capped. Jev stays a
reference measurement: it never reaches the permission pipeline, and a run needs an explicit key
flag.

## Current State

- `rust/fontaine-evals/src/screen.rs`: `connect(model, url, timeout)` builds a `Screen` that POSTs
  `{url}/v1/systemone` with only `content-type` (`Screen::ask`). No auth header.
- `parse` already handles Jev-shaped answers (`confidence` is the probability; see the comment at
  the `answer_confidence` lookup) and reads `usage.input_tokens`. It ignores `usage.cost`.
- `rust/fontaine-evals/src/runner.rs`: `--screen-url`, `--screen-timeout-ms`, `--screen-reference`
  are rejected unless `--guardian screen:<model>` (around line 435). `score_screen` (line 836)
  asks cases sequentially, so a running spend total needs no synchronization.
- `docs/evals.md` ("Not measured") says Jev is "hosted only, and reference only".

Probed against the live endpoint with the key at `/home/mad/open_router_key` (a handful of calls,
under a cent):

- `POST https://openrouter.ai/api/v1/systemone` with model `~typesafe/jev-latest` (or the pinned
  `typesafe/jev-1.14`) returns
  `{"model":"typesafe/jev-1.14-20261014","answers":{"verdict":{"type":"choice","choice":"deny","probabilities":{"allow":0,"deny":1,"escalate":0},"confidence":1}},"usage":{"input_tokens":335,"output_tokens":41,"cost":0.00001407},"provider":"TypeSafe"}`.
  The existing request body and `parse` work unchanged.
- `typesafe/jev-latest` and `typesafe/jev` do not exist; `~typesafe/jev-latest` is the alias.
  DiffusionGemma is not on OpenRouter, so it is out of scope here.
- At about $0.00001 per 335-token call, the about 258 full-profile guardian cases a screen run scores cost well under $0.10, far inside
  any experiment budget; the key's shared $100 limit (about $94 left) is no guard, so the cap
  below is.
- The probabilities are saturated (0 / 1), so Jev ranks cases poorly for threshold fitting.
  That is a finding to report, not something to work around.

## Design

### Auth

New `--screen-api-key-file <PATH>`, valid only with `--guardian screen:<model>`. The file's
trimmed contents are sent as `Authorization: Bearer <key>`. A file, not an env var or argv value,
keeps the secret out of the process list and shell history, and matches how the key is already
stored. `connect` takes an `Option<ApiKey>`, reusing `fontaine_tools::websearch::ApiKey` (redacting
`Debug`, `ApiKey::new`); it gains a small `pub` accessor (`expose() -> &str`) for the header value,
since its inner field is private.

Rules:

- `connect` sends the key only over `https://`, or to a loopback host (a local vLLM behind a test
  proxy): an IPv4/IPv6 host for which `is_loopback()` holds, or the literal domain `localhost`
  (case-insensitive). Anything else is a usage error, so a typo in `--screen-url` cannot send the key in
  cleartext.
- `read_key_file` returns `Err(String)`, a usage error naming the path, for an unreadable or empty
  file, or a trimmed key containing whitespace or control characters (the error does not echo the
  key; otherwise reqwest fails at `send()` and every case fails closed instead of exiting 2),
  rendering the io error with `error_chain::chain` like `read_reference`.
- The key never appears in `request_body`, the report, `eprintln!` progress or error details. The
  HTTP error detail already truncates the response body to 200 chars; the request is never echoed.

### Cost

`ScreenScore` gains `cost_usd: Option<f64>` and `model: Option<String>`, read from `usage.cost`
and `/model` in `parse` (absent from vLLM/Laya, so `None`; both take `#[serde(default,
skip_serializing_if = "Option::is_none")]` like `truncated`, keeping old reports byte-identical). The
`ScreenAnalysis` (`screen_report.rs`) gains `cost_usd` (the warm-up plus the scored cases, omitted
when no response reported one) and `model_resolved: Vec<String>` (the distinct `model` values of
the warm-up and scored responses, sorted, omitted when empty, so `~typesafe/jev-latest` runs are
reproducible). `render` prints them on a `cost_usd=... model_resolved=...` line.

New `--screen-max-cost-usd <USD>`, validated as `!(c > 0.0 && c.is_finite())` → usage error, in the
style of the other numeric flags. `warm_up` is a billed call, so it returns
`Option<ScreenScore>` (`None` when it returns early or fails). Its cost seeds the running total, and
`score_screen` (`runner.rs`) returns it alongside `(results, scored, skipped)` so that
`screen_report::analyze` takes it as an extra parameter and folds it into `cost_usd` and
`model_resolved`. `score_screen` adds each case's cost to the total and,
once the total reaches the cap, stops asking and records every remaining case as skipped with
reason `cost cap reached`. The cap is checked after each call, so the overshoot is at most one
call. Both cost flags need `--guardian screen:<model>`, like the other `--screen-*` flags.

### Invocation

`~typesafe/jev-latest` is the floating alias. `--screen-url` is the base without `/v1/systemone`, so `https://openrouter.ai/api` composes to
OpenRouter's path with no change to `endpoint` construction. Per-call latency over the internet
exceeds the 10 s default timeout only on stalls; leave the default.

## Implementation Steps

1. `rust/fontaine-evals/src/screen.rs`
   - Reuse `fontaine_tools::websearch::ApiKey` (add a `pub` `expose()` accessor in
     `rust/fontaine-tools/src/websearch.rs`), add `read_key_file(path)`, and the https-or-loopback
     check.
   - Thread `Option<ApiKey>` through `connect` and add the bearer header in `Screen::ask` with `.bearer_auth(key.expose())`, the repo's convention
     (`websearch.rs`, `ollama.rs`).
   - Add `cost_usd` and `model` to `ScreenScore`; parse `usage.cost` and `/model`; have `warm_up`
     return `Option<ScreenScore>`.
2. `rust/fontaine-evals/src/runner.rs`
   - Add `--screen-api-key-file` and `--screen-max-cost-usd`; extend the "need `--guardian
     screen:<model>`" check (line 435) and its message; validate `--screen-max-cost-usd`.
   - In `score_screen`, keep the running cost and skip the remainder at the cap; return the
     warm-up's `Option<ScreenScore>` alongside `(results, scored, skipped)`.
3. `rust/fontaine-evals/src/screen_report.rs`: add `cost_usd` and `model_resolved` to
   `ScreenAnalysis`, take the warm-up `Option<ScreenScore>` as an extra parameter of `analyze`, and
   print both in `render`.
4. Docs and changelog (below).
5. Live run: score the dataset with Jev and save `--out` (see Manual Verification).
6. Record the results in md files:
   - `research/analysis-jev-openrouter.md`: add a "Results" section with the scorecard (catch,
     benign false-deny, escalation rate, AUROC, calibration/ECE, latency, `cost_usd`,
     `model_resolved`), a comparison with the Laya and qwen3-4b rows in
     `research/analysis-typed-decision-screen.md` (those Laya and qwen3-4b numbers come from an older
     dataset version, 5a79aa741c27 with 273 cases, so label the comparison cross-version; re-scoring
     them is out of scope since Laya needs a local GPU server), and whether the 0/1 probabilities held. Drop
     the "No full-dataset Jev numbers yet" line from "Not covered".
   - `research/analysis-typed-decision-screen.md`: replace "Jev: hosted, needs an API key, not
     opted into" (around line 606) with a pointer to the Results section and a one-line outcome.
   - `docs/evals.md`: update the Jev bullet as described under Documentation.
   - Commit the `--out` scorecard as `evals/scorecards/screen-jev-1.14.json` (it holds no key) so
     the numbers can be re-derived.
   - Post the scorecard and a link to the Results section on #127.

## Files to Modify

- `rust/fontaine-tools/src/websearch.rs` (`ApiKey::expose`)
- `rust/fontaine-evals/src/screen.rs`
- `rust/fontaine-evals/src/runner.rs`
- `rust/fontaine-evals/src/screen_report.rs`
- `rust/fontaine-evals/tests/screen.rs`, `tests/screen_unit.rs`
- `docs/evals.md`, `evals/README.md`
- `CHANGELOG.md`
- `research/analysis-jev-openrouter.md`, `research/analysis-typed-decision-screen.md`
- `evals/scorecards/screen-jev-1.14.json` (new)

## Decisions

- 2026-10-31: Pin `typesafe/jev-1.14` for the comparison run and record the resolved model in the
  report, so the posted numbers do not drift when `jev-latest` moves.
- 2026-10-31: DiffusionGemma is out of scope; it is not served on OpenRouter, so #127 keeps that
  item open for someone with a GPU.
- 2026-10-31: purpose-built `--screen-api-key-file` rather than a generic `--screen-header`, which
  invites secrets on argv.

## Documentation

- `docs/evals.md`: replace "**Jev**: hosted only, and reference only" under "Not measured" with
  how to run it (the Manual Verification command), the measured numbers once the live run exists, and the
  saturated-probability caveat if it holds. Note that Jev is a reference measurement and that the
  key is sent only over https.
- `evals/README.md`: add the two new flags to the `--screen-*` rows under "Running the runner", the
  cost fields to "Screen mode", and the new cost-flag usage error to the exit-code 2 row.
- `rust/fontaine-evals/src/screen.rs` module doc (lines 1-4, names Laya/DiffusionGemma) and the
  `evals/README.md` "Screen mode" intro (assumes a local server): name Jev via OpenRouter with
  `--screen-api-key-file` against an https `--screen-url`.
- `CHANGELOG.md`: one line for the key-file and cost-cap flags.

## Testing Strategy

All automated tests use the existing scripted-endpoint harness in `rust/fontaine-evals/tests/screen.rs`
and never touch OpenRouter.

- `screen_unit.rs`: `parse` reads `usage.cost` and `/model`; a response without them gives `None`;
  an old report round-trips without `cost_usd`/`model` fields; the invalid-key error does not
  echo the key.
- `screen.rs` (scripted endpoint): the request carries `Authorization: Bearer <key>` when a key
  file is given and no `Authorization` header otherwise; a key over plain `http://` to a
  non-loopback host is a usage error, while `http://127.0.0.1`, `http://[::1]` and
  `http://localhost` (any case) are accepted; an empty or missing key file, or a key containing
  whitespace, is a usage error; the key text
  is absent from the `--out` report and from stdout/stderr.
- `screen.rs`: with a scripted per-call cost and `--screen-max-cost-usd`, the run stops after the
  call that reaches the cap, the remaining cases are skipped with `cost cap reached`, and the
  `ScreenAnalysis` `cost_usd` equals the sum of the warm-up and the scored cases.
- Flag validation: both new flags are rejected without `--guardian screen:<model>`, and a
  non-positive or non-finite `--screen-max-cost-usd` is a usage error, extending the
  `usage_errors_exit_two` table at the end of `tests/screen.rs`.

## Manual Verification

Needs the real OpenRouter service, the one thing the scripted endpoint cannot reproduce (that the
live response still has this shape and that the bearer key is accepted). It is not automated
because tests must never touch live APIs.

1. From `rust/`, run:
   `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian screen:typesafe/jev-1.14 --screen-url https://openrouter.ai/api --screen-api-key-file /home/mad/open_router_key --screen-max-cost-usd 1 --out ../evals/scorecards/screen-jev-1.14.json`
   Expected: every guardian case is scored, none fail closed with `error`, and the printed analysis shows
   `cost_usd` well under $0.10 and `model_resolved` `typesafe/jev-1.14-20261014`.
