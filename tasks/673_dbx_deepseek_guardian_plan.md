# Measure the Databricks DeepSeek v4.1 Flash Guardian Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/673

## Overview

Score `dbx/system.ai.deepseek-v4-1-flash` — the Databricks deployment of the model already measured
through `opencode-go` — on the live guardian harness, full profile, and record it: the scorecard
under `evals/scorecards/`, a row in `docs/evals.md`, the numbers on the issue, and, only if it clears
the inclusion bar, the `kind = "databricks"` entry the run itself writes into
`rust/fontaine-sdk/data/guardian_models.toml`. The same run settles whether this deployment can be
forced into a tool call at all, which is a property of the route, not of the model (#144).

## Current State

**What a scorecard run is.** A `live:` run asks a real model the guardian's question for every case
in `evals/dataset`, once, with the `[guardian]` settings a session builds (30-second timeout,
confidence threshold 0.7, verdict cache off) and either the forced `submit` call or the
schema-constrained answer (`rust/fontaine-evals/src/live.rs`, `runner.rs`). It is a measurement, not
a gate: it exits 0 whatever it scored, 1 when any case failed closed on infrastructure, 2 on a usage
or credential error. A model enters the ranked list `auto` picks its guardian from only on one
unfiltered `--profile full` run with at least 90% catch, at most 5% false deny and at most 3 s p95
(`rust/fontaine-sdk/src/guardian/models.rs`), with no infrastructure failures.

**The `opencode-go` precedent.** `evals/scorecards/go-deepseek-v4-1-flash-d1740.json`, measured
2026-10-27 on dataset `d1740a7c65ae`: 95.9% catch, 0.0% false deny, p50 2.3 s, p95 3.2 s, transport
`tool` — it misses only p95, by 0.2 s, and `docs/evals.md` notes a rerun could flip it. The
Databricks deployment is a different endpoint with its own latency profile, so its numbers have to
be measured on their own.

**The route.** The `dbx` account has `kind = "databricks"`, so this model id routes to the workspace
gateway's Chat wire: `POST {base_url}/ai-gateway/mlflow/v1/chat/completions`, Bearer token
(`rust/fontaine-sdk/src/core/providers/databricks.rs`). Unlike `opencode-go`, the kind defines no
`thinking_off` hook and no `output_probe`, so the guardian sends no reasoning-off parameter and
always asks on the forced-`submit` tool path (`docs/guardian.md`, "Choosing the guardian model").
A deployment that thinks by default and rejects the forced call has every case fail closed on
infrastructure; the run then exits 1, leaves the models file as it was, and keeps its sidecar.

**The credential.** The account names `ANTHROPIC_AUTH_TOKEN`; a raw key is never inline in config.
Fontaine withholds every account's `api_key_env` variable from the processes it starts
(`tools::scrubbed_env`, `rust/fontaine-sdk/src/core/tools.rs`), so the run takes the value from the
local token file for that one command (see Decisions).

**The models-file merge.** An unfiltered `--profile full` live run rewrites the models file in place
(`update_models`), and skips it only for a `minimal` profile, a `--filter` run, a `mixed` transport, a
cascade via `--screen`, or any infrastructure failure. It upserts this run's model and drops every
entry scored on a different `dataset_version` (`report::merge`). The shipped entry
`opencode-go/claude-haiku-5-5` is scored on `d1740a7c65ae`; the current dataset hashes to
`5daaae0b478a`, so this run drops it, and what the file holds afterwards is exactly the outcome: the
deepseek entry if it clears the bar, no entry otherwise. That drop is the file's own rule — an
entry is only validated for the dataset it was scored on — and it is not something this change
hand-edits away; the `opencode-go` numbers stay in the `docs/evals.md` tables either way.

**The dataset.** Fourteen `*.jsonl` files under `evals/dataset`; the version is the first 12 hex
characters of a SHA-256 over their bytes in file-name order (`report::dataset_version`),
`5daaae0b478a` as of 362a90d (2026-11-15). A dataset edit before the run changes the value the run
computes, and the report is the source for the file name and for the models entry.

**Scanner cases.** `[guardian] scanner` is unset, so it is `off`: the four `spawn-report-*` scanner
cases are listed as skipped with a reason, which never blocks the models file (as in the go
scorecard).

## Design

### 1. Preflight — feasibility, no repository writes

1. Build `fontaine-evals` (`cargo build -p fontaine-evals` from `rust/`); the first build dominates
   the run's wall clock.
2. Resolve the dataset version with the hash the runner uses, and write it into both paths below:
   `cd evals/dataset && cat $(ls -1 *.jsonl | LC_ALL=C sort) | sha256sum | cut -c1-12`.
3. Smoke test the route with a **filtered** live run — `--filter attack:iam-grant`, about ten
   guardian cases and half a minute. A filtered run never touches the models file, and without
   `--out` it leaves no report and no sidecar:
   - every case scored (exit 0) → auth, routing and the forced call all work; go to step 2;
   - cases `UNSCORED` with the provider's refusal in the explanation (exit 1) → the deployment
     rejects the forced call; go to Design 4;
   - exit 2 → the account or its credential did not resolve, and nothing was asked of the model.

### 2. The full run

```
cd rust
ANTHROPIC_AUTH_TOKEN="$(cat ~/.claude_databricks_token)" cargo run -p fontaine-evals -- \
  --dataset ../evals/dataset \
  --guardian live:dbx/system.ai.deepseek-v4-1-flash \
  --out ../evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json
```

- Run it as a background job and read the progress lines (`[n/total] <id> <verdict> <latency>ms` on
  stderr): a full-profile run is 289 cases, plus the pipeline cases that reach the guardian, at tens
  of minutes at this model's latencies.
- An interrupted run leaves `<out>.partial.jsonl`; continue it with the same command plus
  `--resume ../evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json.partial.jsonl`. `--resume`
  refuses a sidecar whose dataset version, guardian, profile or filter differ from the command's.
- An exit of 1 means the report is written but is not a measurement: the fail-closed cases are out of
  every rate and the models file is untouched. Clear the cause (rate limit, timeout, auth) and
  resume; rates computed over a subset are never committed.

### 3. Outcome A — it clears the bar

Catch at least 90%, false deny at most 5%, p95 at most 3 s, nothing fail-closed.

- The run writes the entry (`kind = "databricks"`, `model = "system.ai.deepseek-v4-1-flash"`, its
  rates, `scored_on`, `dataset_version`, with `transport` omitted for `tool`) and prints
  `<account>/<model>: clears the inclusion bar (N models in <path>)`.
- `docs/evals.md` gains a row in "Other small and hosted guardian models" (catch, false deny, p50,
  p95) inserted in the table's rank order, the row's text naming it as the Databricks deployment of
  the model the `go/` row already lists, and a bullet in that section's list next to the
  `opencode-go` one with the transport and the fail-closed counts. The prose that still reads as
  though the shipped ranked list is empty is corrected.
- `CHANGELOG.md`, `### Added`: one line naming the model, the deployment, the numbers, and that the
  ranked list is what `auto` picks its guardian from.
- The numbers go up as a comment on issue #673, whose acceptance asks for them there, together with
  the transport and the fail-closed counts.

### 4. Outcome B — a clean run that misses

- Commit the scorecard and record which bar it missed and by how much. On the `opencode-go` row that
  was p95 by 0.2 s, so a near miss is a "rerun" verdict rather than a settled one — the issue asks
  for that call too.
- The same `docs/evals.md` row and bullet, written as a miss, next to the `go/` row since it is the
  same model on another endpoint.
- The models file keeps only what clears the bar, so a miss leaves it with no entries and the
  guardian falls back to the main model. That is the documented lifecycle (`report::merge` and the
  file's own header), not a regression to paper over: the PR body calls the drop out.

### 5. Outcome C — the run fails closed on infrastructure

- Exit 1, no models-file write, sidecar kept: resume first (Design 2). Rate limits and timeouts
  usually clear.
- When every case fails closed because the deployment refuses the forced `submit` call, that is the
  issue's own predicted finding about the provider:
  - commit the report as the scorecard (it is the evidence, and `summary.infrastructure_failures`
    names each reason) and say in the docs that it is not a verdict on the model;
  - add one clause to `docs/guardian.md`'s "Choosing the guardian model", where the databricks
    limitation is already stated, pointing at the measurement;
  - record the numbers on issue #673.

### 6. The counts the issue asks for

The scorecard carries no per-reason counter; the counts come from the report, which is also where
the resolved transport is:

```
jq -r '.summary.infrastructure_failures[]?.reason' \
  evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json | sort | uniq -c
jq -r '.summary.verdict_transport' evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json
```

Per-case `fail_closed` carries the same reasons for the cases that did score.

## Files to Modify

| Path | Change |
|---|---|
| `evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json` | New report; the dataset version in the name is the value the run computes. |
| `rust/fontaine-sdk/data/guardian_models.toml` | Written by the run: the databricks entry when it clears the bar, and the stale `opencode-go` entry dropped. |
| `docs/evals.md` | The row, the bullet, the section's prose. |
| `CHANGELOG.md` | `### Added`, one line. |
| `docs/guardian.md` | Outcome C only: the pointer from the databricks limitation. |
| `tasks/completed/673_dbx_deepseek_guardian_plan.md` | This plan, moved by `pr`. |

## Trade-offs

- **The full 12-hex dataset version in the scorecard name** (as the issue writes
  `<dataset_version>`) rather than the five-character suffix the `*-d1740.json` files carry: the
  short form is lossy, and the full value is what the report and the models entry both record. Only
  the file name is affected.
- **Letting the run merge the models file** rather than pointing `--models-file` at a scratch copy
  and hand-applying the entry: the merge is the documented path, and a hand-applied entry would
  preserve one the same docs call no longer validated.
- **The row goes in "Other small and hosted guardian models"** beside the `go/deepseek-v4.1-flash`
  row rather than in a section of its own: it is the same model on another endpoint, and that
  section already reads its latencies with the hosted-endpoint caveat.
- **A ten-case smoke test before the full run** costs about ten calls and settles the forced-call
  question first; without it, a route that refuses or times out per case spends far longer reaching
  the same conclusion.

## Decisions

- 2026-11-15 The account's credential reaches the run from the local token file
  `~/.claude_databricks_token`, exported as `ANTHROPIC_AUTH_TOKEN` for the command only (user),
  because Fontaine withholds every account's `api_key_env` variable from the processes it starts;
  the value never enters the repository, a report or a log.

## Documentation

- `docs/evals.md`: the row in "Other small and hosted guardian models" and the bullet beside the
  `opencode-go` one, with the dataset version, the date, the transport and the fail-closed counts.
- `CHANGELOG.md`: the `### Added` line for the measurement and, when it clears the bar, for the
  ranked-list entry.
- `docs/guardian.md`: only in outcome C, one clause at the databricks limitation.

## Testing Strategy

- The only shipped artifact is data: `guardian_models.toml` is embedded with `include_str!`
  (`rust/fontaine-sdk/src/guardian/models.rs`) and parsed by `validated_models()`
  (`rust/fontaine-sdk/src/core/models.rs`). `cargo test -p fontaine-sdk --test models` covers it: the
  shipped list parses and every entry clears the bar
  (`the_shipped_model_list_parses_and_every_entry_clears_the_bar`), which is what a malformed entry
  or a bad hand-edit would break. `pr` runs the workspace's `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings` and `cargo test` as usual.
- The offline halves of the harness stay green on the changed tree:
  `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture`, with and without
  `--profile minimal`; a scorecard and a docs row change nothing they read.
- The measurement itself is a live, billed provider run and is not a CI gate (`evals/README.md`,
  "Live mode"). No unit test can reach it; the automated covering check for what ships is the models
  test above.

## Manual Verification

1. The smoke test of Design 1 → ten cases scored, none `UNSCORED`, exit 0; a wrong verdict is fine,
   exit 1 is the fail-closed signal. Not automated: it is a live billed call to the workspace
   gateway.
2. The full run of Design 2 → the summary prints the case counts, the catch rate, the false-deny
   rate and the per-guardian p50/p95, and the models-file line; exit 0. Then
   `jq '.summary | {dataset_version, verdict_transport, catch_rate, false_deny_rate, latency_ms}'`
   over the report shows the version matching the file name, the transport, and the two rates. A
   live run; nothing offline reproduces it.
3. `git diff rust/fontaine-sdk/data/guardian_models.toml` → the deepseek entry when it cleared the
   bar, and the `opencode-go` entry the run reports as dropped. Read by a human for the PR body; the
   parsing and the bar are covered by the test above.

## Open Questions

- None that block: the run's outcome selects Design 3, 4 or 5, and each is written out, including
  the near-miss verdict the issue asks for.
