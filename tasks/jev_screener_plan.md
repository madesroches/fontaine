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
- At about $0.00001 per 335-token call, the 193-case dataset costs well under $0.10. The $10
  worry is not a real constraint at this volume, but the key itself has a $100 limit with about
  $94 left, so it is no guard either; the cap below is.
- The probabilities are saturated (0 / 1), so Jev ranks cases poorly for threshold fitting.
  That is a finding to report, not something to work around.

## Design

### Auth

New `--screen-api-key-file <PATH>`, valid only with `--guardian screen:<model>`. The file's
trimmed contents are sent as `Authorization: Bearer <key>`. A file, not an env var or argv value,
keeps the secret out of the process list and shell history, and matches how the key is already
stored. `connect` takes an `Option<ApiKey>`; `ApiKey` has a redacting `Debug`.

Rules, each enforced in `connect`:

- The key is sent only over `https://`, or to a loopback host (a local vLLM behind a test proxy).
  Anything else is a usage error, so a typo in `--screen-url` cannot send the key in cleartext.
- An unreadable or empty key file is a usage error naming the path, with the cause as `source()`.
- The key never appears in `request_body`, the report, `eprintln!` progress or error details. The
  HTTP error detail already truncates the response body to 200 chars; the request is never echoed.

### Cost

`ScreenScore` gains `cost_usd: Option<f64>`, read from `usage.cost` in `parse` (absent from
vLLM/Laya, so `None`, and `skip_serializing_if` keeps old reports byte-identical). The screen
summary gains `cost_usd` (sum over scored cases, omitted when no case reported one) and
`model_resolved` (the `model` field of the response, so `~typesafe/jev-latest` runs are
reproducible; if it varies across cases the summary lists each distinct value).

New `--screen-max-cost-usd <USD>`: `score_screen` adds each case's cost to a running total and,
once the total reaches the cap, stops asking and records every remaining case as skipped with
reason `cost cap reached`. The cap is checked after each call, so the overshoot is at most one
call. Both cost flags need `--guardian screen:<model>`, like the other `--screen-*` flags.

### Invocation

```
fontaine-evals --dataset ../evals/dataset --guardian screen:~typesafe/jev-latest \
  --screen-url https://openrouter.ai/api \
  --screen-api-key-file /home/mad/open_router_key \
  --screen-max-cost-usd 1 --out jev.json
```

`--screen-url` is the base without `/v1/systemone`, so `https://openrouter.ai/api` composes to
OpenRouter's path with no change to `endpoint` construction. Per-call latency over the internet
exceeds the 10 s default timeout only on stalls; leave the default.

## Implementation Steps

1. `rust/fontaine-evals/src/screen.rs`
   - Add `ApiKey` (redacting `Debug`), `read_key_file(path)`, and the https-or-loopback check.
   - Thread `Option<ApiKey>` through `connect` and add the bearer header in `Screen::ask`.
   - Add `cost_usd` to `ScreenScore` and parse `usage.cost`.
2. `rust/fontaine-evals/src/runner.rs`
   - Add `--screen-api-key-file` and `--screen-max-cost-usd`; extend the "need `--guardian
     screen:<model>`" check (line 435) and its message.
   - In `score_screen`, keep the running cost and skip the remainder at the cap.
3. `rust/fontaine-evals/src/screen_report.rs`: add `cost_usd` and `model_resolved` to the screen
   summary.
4. Docs and changelog (below).
5. Live run: score the dataset with Jev, save `--out`, and post the numbers on #127
   (see Manual Verification).

## Files to Modify

- `rust/fontaine-evals/src/screen.rs`
- `rust/fontaine-evals/src/runner.rs`
- `rust/fontaine-evals/src/screen_report.rs`
- `rust/fontaine-evals/tests/screen.rs`, `tests/screen_unit.rs`
- `docs/evals.md`
- `CHANGELOG.md`

## Trade-offs

- **Key file vs env var vs argv.** A file keeps the secret off argv and out of the environment of
  child processes, and it is where the key already lives. An env var would need a name convention
  the project does not have for evals.
- **Cost cap in the scorer vs relying on OpenRouter's key limit.** The key limit is $100 and is
  shared with other work, so it cannot express "this experiment may spend $10". Cost is already in
  the response, so the cap is a few lines.
- **A generic `--screen-header` flag.** More open, but it invites putting secrets on argv; a
  purpose-built key-file flag is safer and the only auth anyone needs here.

## Decisions

- 2026-10-31: Pin `typesafe/jev-1.14` for the comparison run and record the resolved model in the
  report, so the posted numbers do not drift when `jev-latest` moves.
- 2026-10-31: DiffusionGemma is out of scope; it is not served on OpenRouter, so #127 keeps that
  item open for someone with a GPU.

## Documentation

- `docs/evals.md`: replace "**Jev**: hosted only, and reference only" under "Not measured" with
  how to run it (command above), the measured numbers once the live run exists, and the
  saturated-probability caveat if it holds. Add the two new flags to the flag reference. Note that
  Jev is a reference measurement and that the key is sent only over https.
- `CHANGELOG.md`: one line for the key-file and cost-cap flags.

## Testing Strategy

All automated tests use the existing scripted-endpoint harness in `rust/fontaine-evals/tests/screen.rs`
and never touch OpenRouter.

- `screen_unit.rs`: `parse` reads `usage.cost`; a response without it gives `None`; an old
  report round-trips without a `cost_usd` field; `ApiKey`'s `Debug` and the error strings never
  contain the key.
- `screen.rs` (scripted endpoint): the request carries `Authorization: Bearer <key>` when a key
  file is given and no `Authorization` header otherwise; a key over plain `http://` to a
  non-loopback host is a usage error; an empty or missing key file is a usage error; the key text
  is absent from the `--out` report and from stdout/stderr.
- `screen.rs`: with a scripted per-call cost and `--screen-max-cost-usd`, the run stops after the
  call that reaches the cap, the remaining cases are skipped with `cost cap reached`, and the
  summary `cost_usd` equals the sum of the scored cases.
- Flag validation: both new flags are rejected without `--guardian screen:<model>`, extending the
  table at the end of `tests/screen.rs`.

## Manual Verification

Needs the real OpenRouter service, the one thing the scripted endpoint cannot reproduce (that the
live response still has this shape and that the bearer key is accepted). It is not automated
because tests must never touch live APIs.

1. From `rust/`, run:
   `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian screen:typesafe/jev-1.14 --screen-url https://openrouter.ai/api --screen-api-key-file /home/mad/open_router_key --screen-max-cost-usd 1 --out /tmp/jev.json`
   Expected: every guardian case is scored, none fail closed with `error`, and the summary shows
   `cost_usd` well under $0.10 and `model_resolved` `typesafe/jev-1.14-20261014`.
2. Run it again with `--screen-max-cost-usd 0.0001`. Expected: it stops after a few cases and
   lists the rest as skipped with `cost cap reached`.
3. `rg "$(cat /home/mad/open_router_key)" /tmp/jev.json` finds nothing.
4. Post the scorecard on #127.

## Open Questions

- Should the live Jev scorecard be committed under `research/typed-decision-screen/`, or only
  posted on the issue? The issue asks for the numbers to be posted, so this plan only posts them.
