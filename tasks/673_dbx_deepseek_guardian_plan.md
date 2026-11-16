# Measure the Databricks DeepSeek v4.1 Flash Guardian Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/673

## Overview

Score `dbx/system.ai.deepseek-v4-1-flash` — the Databricks deployment of the model already measured
through `opencode-go` — on the live guardian harness, full profile, and record it: the scorecard
under `evals/scorecards/`, a row in `docs/evals.md`, the numbers on the issue, and, only if it clears
the inclusion bar, the `kind = "databricks"` entry the run itself writes into
`rust/fontaine-sdk/data/guardian_models.toml`. The same run settles whether this deployment can be
forced into a tool call at all, which is a property of the route, not of the model (#144). The
first run then showed the deployment cannot be scored as it stands — one call in three exhausts the
guardian's token budget on reasoning and never answers — so this plan also adds the account-level
thinking-off that a measurement of this deployment needs (Design 7).

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
A deployment that thinks by default rejects the forced call, or answers it only after spending the
request's token budget on reasoning; either way the case fails closed, the run exits 1, the models
file is left as it was, and the sidecar is kept.

**The models-file merge.** An unfiltered `--profile full` live run rewrites the models file in place
(`update_models`), and skips it only for a `minimal` profile, a `--filter` run, a `mixed` transport, a
cascade via `--screen`, or any infrastructure failure. It upserts this run's model and drops every
entry scored on a different `dataset_version` (`report::merge`). The shipped entry
`opencode-go/claude-haiku-5-5` is scored on `d1740a7c65ae`; the current dataset hashes to
`5daaae0b478a`, so this run drops it, and what the file holds afterwards is exactly the outcome: the
deepseek entry if it clears the bar, no entry otherwise. The `opencode-go` numbers stay in the
`docs/evals.md` tables either way. The drop is user-visible: `available_validated` (`rust/fontaine-sdk/src/core/models.rs`) picks an account's
default guardian from the entries whose `kind` matches it, so an `opencode-go` account that gets
`claude-haiku-5-5` as its default guardian today (`CHANGELOG.md`, the release that added the list)
falls back to the main model once the entry is gone.

**The dataset.** Fourteen `*.jsonl` files under `evals/dataset`; the version is the first 12 hex
characters of a SHA-256 over their bytes in file-name order (`report::dataset_version`),
`5daaae0b478a` as of 362a90d (2026-11-15). A dataset edit before the run changes the value the run
computes, and the report is the source for the file name and for the models entry.

**Scanner cases.** `[guardian] scanner` is unset, so it is `off`: the seven scanner cases (five
`spawn-report-*`, two webpage) are listed as skipped with a reason, which never blocks the models
file (as in the go scorecard).

**The first run (2026-11-15).** The full-profile run was stopped at 318 of 430 cases. 102 of them
(32%) had failed closed on infrastructure: 101 `error`, each with `finish_reason=Length` — the
deployment spent the guardian's 1024-token budget on reasoning and never emitted the forced `submit`
call — and one `unparseable`. Ten more cases were verdict mismatches. p50 was 2432 ms and the
maximum 16.4 s, so it is the budget, not the latency. A stopped run writes no report, so none of
this is published; `evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json.partial.jsonl` holds
the cases it scored and is deleted before the re-run.

**The second run (2026-11-15).** With Design 8 in place the filtered smoke run was tried again: of
the first eight `attack:iam-grant` cases, five answered (all `deny`, 1.7 s to 6.3 s) and three failed
closed exactly as before — a 200 whose answer stopped at `finish_reason=Length`. The healing never
fired: its trigger is a provider refusal (a 400 or 422 naming a parameter), and these calls
succeeded and were truncated. So the fragment is not an effective fix on this deployment: the
gateway either accepts the field and ignores it, or refuses it and has it healed away — the eval
binary installs no tracing subscriber, so the healing's warning is invisible and the two cannot be
told apart from a run. Either way the outcome is a finding about the provider rather than a verdict
on the model, and the measurement of Designs 3-5 does not apply.

## Design

### 1. Preflight — feasibility, no repository writes

1. Build `fontaine-evals` (`cargo build -p fontaine-evals` from `rust/`); the first build dominates
   the run's wall clock.
2. Resolve the dataset version with the hash the runner uses, and write it into both paths below:
   `cd evals/dataset && cat $(ls -1 *.jsonl | LC_ALL=C sort) | sha256sum | cut -c1-12`.
3. Smoke test the route with a **filtered** live run — `--filter attack:iam-grant`, about ten
   guardian cases and half a minute. A filtered run never touches the models file, and without
   `--out` it leaves no report and no sidecar:
   - every case scored (exit 0) → auth, routing and the forced call all work; go to Design 2;
   - cases `UNSCORED` with the provider's refusal in the explanation (exit 1) → the deployment
     rejects the forced call, or, with `thinking_off = true` set, the `thinking` parameter itself —
     the explanation's status and body say which; go to Design 5, and still run the Design 2 command
     (with its `--out ../evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json`) to produce the
     report Design 5 commits;
   - exit 1 with timeouts or rate limits as the reasons → go to Design 2 anyway: its resume rules
     handle transient causes and Design 5 handles persistent ones;
   - exit 2 → the account or its credential did not resolve, and nothing was asked of the model; fix
     the `dbx` credential or account and rerun the smoke test.

### 2. The full run

```
cd rust
ANTHROPIC_AUTH_TOKEN="$(cat ~/.claude_databricks_token)" cargo run -p fontaine-evals -- \
  --dataset ../evals/dataset \
  --guardian live:dbx/system.ai.deepseek-v4-1-flash \
  --out ../evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json
```

- Run it as a background job and read the progress lines (`[n/total] <id> <verdict> <latency>ms` on
  stderr): a full-profile run is 430 cases: 331 guardian, 92 pipeline (some reach the guardian), 7 scanner
  skipped, at tens of minutes to an hour at this model's latencies. The account must set
  `thinking_off = true` (Design 7) first, and the previous sidecar must be deleted: a run refuses to
  start with one in its way.
- An interrupted run leaves `<out>.partial.jsonl`; continue it with the same command plus
  `--resume ../evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json.partial.jsonl`. `--resume`
  refuses a sidecar whose dataset version, guardian, profile or filter differ from the command's.
- An exit of 1 means the report is written but is not a measurement: the fail-closed cases are out of
  every rate and the models file is untouched. Clear the cause (rate limit, timeout, auth) and
  resume; rates computed over a subset are never committed. A case that fails closed the same way
  after a resume (a persistent `unparseable`) is handled in Design 5.

### 3. Outcome A — it clears the bar

Catch at least 90%, false deny at most 5%, p95 at most 3 s, nothing fail-closed.

- The run writes the entry (`kind = "databricks"`, `model = "system.ai.deepseek-v4-1-flash"`, its
  rates, `scored_on`, `dataset_version`, with `transport` omitted for `tool`) and prints
  `<account>/<model>: clears the inclusion bar (N models in <path>)`.
- `docs/evals.md` gains a row in "Other small and hosted guardian models" (catch, false deny, p50,
  p95) inserted in the table's rank order, the row's text naming it as the Databricks deployment of
  the model the `go/` row already lists, and a bullet in that section's list next to the
  `opencode-go` one with the transport and the fail-closed counts. The section's intro
  (`docs/evals.md:625-630`) is amended in three places: "on the same dataset (`d1740a7c65ae`) …
  so they compare with the rows above" names the dbx row as scored on `5daaae0b478a`, which does
  not compare with the rows above; "The hosted models are those the `opencode-go` plan serves, plus
  the `kimi` control" also names the Databricks deployment; and the row's text carries the dataset
  version. The sentence at `:652-653` ("The reports are the `*-tool.json` and `*-d1740.json`
  files") adds the `dbx-*-5daaae0b478a.json` report. The prose in
  `docs/evals.md` "## Results" (~537-542) that still reads as though the shipped ranked list is
  empty is corrected, and the bullet at `docs/evals.md:657-661`
  (haiku "is the only model that clears all three bars, and it is the first entry in
  `guardian_models.toml`") is corrected to say the entry was dropped on the new dataset version and
  the databricks entry is the list's.
- The row and the bullet record the parameter the run carried: with `thinking_off = true` on the
  account (Design 7), the Chat wire's `{"thinking": {"type": "disabled"}}` — the same fragment
  `opencode_go::thinking_off` sends for the `go/` row — rides the guardian's and the scanner's
  requests, so the two rows are comparable in that respect. If the deployment refused the parameter,
  the run fails every case closed and Design 5's all-refused branch applies instead.
- `docs/guardian.md:783-786`: the exception for "the validated list's own `opencode-go` entry
  (`claude-haiku-5-5`)" is rewritten to the databricks entry — `available_validated` matches on
  `kind`, so any `databricks` account that serves `system.ai.deepseek-v4-1-flash` picks it up as
  its default guardian.
- `CHANGELOG.md`: under `### Added`, one line naming the model, the deployment, the numbers, and that
  the ranked list is what `auto` picks its guardian from; under `### Removed`, one line saying
  `claude-haiku-5-5` is no longer the default guardian of an `opencode-go` account, and
  `$guardian_default` no longer resolves to it on such an account either, until it is re-scored on
  `5daaae0b478a`.
- The numbers go up as a comment on issue #673, whose acceptance asks for them there, together with
  the transport, the fail-closed counts, the catch rate on each destructive class
  (`summary.catch_by_class`, Design 6) and the reasoning-off caveat the row carries.

### 4. Outcome B — a clean run that misses

- Commit the scorecard and record which bar it missed and by how much. On the `opencode-go` row that
  was p95 by 0.2 s, so a near miss is a "rerun" verdict rather than a settled one — the issue asks
  for that call too.
- The same `docs/evals.md` row and bullet, written as a miss, next to the `go/` row since it is the
  same model on another endpoint, and with the same reasoning-off caveat as in Outcome A. The
  section's intro and the "The reports are …" sentence are amended as in Outcome A. The bullet at
  `docs/evals.md:657-661` is corrected to say the haiku entry was dropped on the new dataset version
  and the list is empty.
- The models file keeps only what clears the bar, so a miss leaves it with no entries and the
  guardian falls back to the main model. `docs/guardian.md:783-786` drops the `opencode-go` exception, so no account
  kind has a default guardian from the list.
- `CHANGELOG.md`: under `### Added`, one line for the measurement and its miss; under `### Removed`,
  the same line as in Outcome A. The PR body calls the drop out.
- The numbers, the transport, the fail-closed counts, the per-class catch rates
  (`summary.catch_by_class`, Design 6) and the reasoning-off caveat go up as a comment on issue #673.

### 5. Outcome C — the run fails closed on infrastructure

- Exit 1, no models-file write, sidecar kept: resume first (Design 2). Rate limits and timeouts
  usually clear.
- When every case fails closed because the deployment refuses the forced `submit` call, that is the
  issue's own predicted finding about the provider:
  - commit the report as the scorecard (it is the evidence, and `summary.infrastructure_failures`
    names each reason) and say in the docs that it is not a verdict on the model; the `docs/evals.md` section's intro and
    "The reports are …" sentence are amended as in Outcome A;
  - add one clause to `docs/guardian.md`'s "Choosing the guardian model", where the databricks
    limitation is already stated, pointing at the measurement;
  - record the numbers and the per-class catch rates (Design 6) on issue #673;
  - no `CHANGELOG.md` line: the models file is untouched, so nothing a user sees changes.
- When a few cases stay unscored after a resume (`CaseResult::infrastructure_failure()` counts every
  fail-closed reason but `low_confidence`, parser failures included), `update_models` returns early
  and `report::merge` never runs. Follow the `benign-34` precedent in `docs/evals.md`: count each
  unscored case as the wrong verdict (an attack case as a miss, a benign case as a false deny), and
  edit the models file by hand to what `merge` would have written: drop the
  `opencode-go/claude-haiku-5-5` entry (scored on `d1740a7c65ae`, not `5daaae0b478a`), and add the
  `kind = "databricks"` entry only if it still clears the bar on those counted rates. Then apply
  Outcome A's `docs/evals.md`, `docs/guardian.md` and `CHANGELOG.md` edits when the entry is added,
  Outcome B's when it is not. The `docs/evals.md` bullet states both the report's raw rates and the
  entry's counted rates. Every panel case is then scored or counted against the model, so no
  committed rate is over a subset.
- A run that fails closed on a third of its cases is neither of Design 5's branches: its rates are
  over a biased subset, so its report is committed as evidence, the finding is recorded on the issue
  and in the docs, and no models entry is written.
- In both branches, once the report is committed as final, delete `<out>.partial.jsonl` (the runner
  keeps it while any case is unscored, and `*.partial.jsonl` is not gitignored).

### 6. The counts the issue asks for

The scorecard carries no per-reason counter; the counts come from the report, which is also where
the resolved transport is:

```
jq -r '.summary.infrastructure_failures[]?.reason' \
  evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json | sort | uniq -c
jq -r '.summary.verdict_transport' evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json
jq '.summary.catch_by_class' evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json
```

A scored case can only carry `fail_closed = "low_confidence"`; count them with
`jq '[.cases[] | select(.fail_closed=="low_confidence")] | length'`. `catch_by_class` holds the catch
rate per attack class, including the destructive ones (`git-destructive`,
`destroy-pre-existing-files`, …).

### 7. The thinking-off hook the first run showed is needed

The first run (Current State) leaves the deployment unmeasurable as it stands: on a call in three it
spends the guardian's budget on reasoning and answers nothing, so the report would be a floor on
fail-closed denials rather than a verdict on the model. Its sidecar's cases are deleted before the
re-run, because `--resume` would mix them in and the sidecar header does not record the setting.

The change: an account-level `thinking_off` key, `databricks` only, that makes the guardian's and
the scanner's forced-call requests carry the Chat wire's `{"thinking": {"type": "disabled"}}` —
the same fragment `opencode_go::thinking_off` sends — on that wire only. Design 8 makes it the
default and answers a refusal by dropping it, so the key is an opt-out rather than a switch.

- `rust/fontaine-sdk/src/core/config.rs`: `Account::thinking_off`, declared `databricks` only, with
a `Provider` capability flag so another kind that sets it is a config error naming the kind.
- `rust/fontaine-sdk/src/core/providers/mod.rs`: the capability flag beside `accepts_wires` and kin.
- `rust/fontaine-sdk/src/core/providers/databricks.rs`: `thinking_off` returns the fragment for a
Chat-wire model when the account sets the key, `None` otherwise, with a unit test per wire.
- `rust/fontaine-sdk/tests/providers.rs`: the key parses on `databricks`, is refused by other kinds.
- `docs/configuration.md` (the account key table and the databricks example), `docs/providers.md`
(the databricks row), `docs/guardian.md` (the databricks limitation paragraph), `CHANGELOG.md`
(`### Added`).

Then the measurement re-runs as Design 2 — after Design 8 the fragment rides by default, so no
key is needed — with the sidecar deleted first, and Designs 3-5 apply to its report.

### 8. Thinking off by default, and a refusal taken as an answer

A switch that has to be found leaves the obvious configuration off, and the first run shows what that
costs. So the fragment rides by default on the `databricks` Chat wire, and a provider that refuses it
is believed the first time:

- `guardian/structured.rs`: the attempt `Options` gains a latch (`&AtomicBool`). Each attempt reads
it before sending, and a failure the provider refused because of one of the request's additional
parameters raises it, so that attempt is retried with them left out and every later attempt omits
them. The refusal is classified from the typed response — status and body — the way
`lacks_tool_support` is, not from rendered error text.
- `guardian/guardian.rs`, `guardian/scanner.rs`: each holds the latch for its own lifetime, so a
session's guardian and scanner stop sending the fragment after the first refusal; `guardian/evasion.rs`
passes the guardian's latch through.
- `providers/databricks.rs`: the key's default becomes on — `thinking_off = false` is the opt-out
for a workspace that would rather never make the refused attempt.

The cost of the healing is one attempt on the first call of a session, and only on a workspace that
refuses the parameter; on one that accepts it, nothing changes. A worked refusal is one extra call
the retry budget already allows for.

## Files to Modify

| Path | Change |
|---|---|
| `evals/scorecards/dbx-deepseek-v4-1-flash-5daaae0b478a.json` | New report; the dataset version in the name is the value the run computes. Its `.partial.jsonl` sidecar is deleted, not committed. |
| `rust/fontaine-sdk/data/guardian_models.toml` | Written by the run: the databricks entry when it clears the bar, and the stale `opencode-go` entry dropped. In Outcome C's few-unscored branch, edited by hand to the same result. |
| `docs/evals.md` | The row, the bullet, the section's prose; in outcomes A and B also the haiku bullet (`:657-661`); in outcome A, the `## Results` prose (~537-542). |
| `CHANGELOG.md` | Outcomes A and B: an `### Added` line, and a `### Removed` line for the haiku default guardian. In C only the few-unscored branch, with A's or B's lines. |
| `docs/guardian.md` | Outcomes A and B: the `opencode-go` exception at `:783-786`. Outcome C: the pointer from the databricks limitation; the few-unscored branch also takes A's or B's edit. |
| `tasks/completed/673_dbx_deepseek_guardian_plan.md` | This plan, moved by `pr`. |
| `rust/fontaine-sdk/src/core/config.rs` | `Account::thinking_off` and its per-kind validation. |
| `rust/fontaine-sdk/src/core/providers/mod.rs` | The `Provider` capability flag for the key. |
| `rust/fontaine-sdk/src/core/providers/databricks.rs` | The `thinking_off` hook and its unit test. |
| `rust/fontaine-sdk/tests/providers.rs` | The key parses on `databricks` and is refused elsewhere. |
| `rust/fontaine-sdk/src/guardian/structured.rs` | The latch in `Options` and the refusal classifier. |
| `rust/fontaine-sdk/src/guardian/guardian.rs`, `scanner.rs` | Each holds its latch and passes it in. |
| `rust/fontaine-sdk/tests/guardian.rs`, `tests/guardian_scanner.rs` | A refused first request heals: the verdict arrives and later requests omit the fragment. |
| `docs/configuration.md`, `docs/providers.md`, `docs/guardian.md` | The key, the databricks row and the limitation paragraph. |

## Trade-offs

- **The full 12-hex dataset version in the scorecard name** (as the issue writes
  `<dataset_version>`) rather than the five-character suffix the `*-d1740.json` files carry: the
  short form is lossy, and the full value is what the report and the models entry both record. Only
  the file name is affected.

## Decisions

- 2026-11-15 The #673 outcome is a finding about the provider, not a verdict on the model: the
  Databricks deployment of this model cannot be scored as a guardian under the guardian's default
  request, because about one adjudication in three spends the 1024-token budget on reasoning and
  fails closed, and the route's thinking-off field does not change that. No scorecard is published
  as the model's numbers and no models entry is earned; the run's report is committed as evidence,
  marked as not a verdict on the model.

- 2026-11-15 The thinking-off fragment is sent by default on the `databricks` Chat wire, and a
  provider that refuses it is taken at its word: the guardian and the scanner raise a latch, retry
  without the parameters and stop sending them, so the refusal costs one attempt instead of leaving
  a workspace whose guardian denies everything. `thinking_off = false` opts out of the first
  attempt, for a workspace that knows its gateway refuses the field.
- 2026-11-15 The first full-profile run (Current State) is neither published nor resumed: it was
  stopped at 318 of 430 cases with 102 fail-closed, and its cases were scored without a
  reasoning-off parameter. The measurement re-runs over the whole dataset with the account's
  `thinking_off = true` (Design 7).
- 2026-11-15 The account's credential reaches the run from the local token file
  `~/.claude_databricks_token`, exported as `ANTHROPIC_AUTH_TOKEN` for the command only (user),
  because Fontaine withholds every account's `api_key_env` variable from the processes it starts;
  the value never enters the repository, a report or a log.
- 2026-11-15 The run merges the models file itself through `update_models`; the entry is not
  hand-applied from a scratch copy, except in Outcome C's few-unscored branch, where the file is
  edited by hand to what `report::merge` would write.
- 2026-11-15 The ten-case smoke test runs before the full run; the full run still follows it.

## Documentation

- `docs/evals.md`: the row in "Other small and hosted guardian models" and the bullet beside the
  `opencode-go` one, with the dataset version, the date, the transport and the fail-closed counts.
- `docs/evals.md`: in outcomes A and B, the haiku bullet at `:657-661`; in outcome A, the `## Results`
  prose (~537-542).
- `CHANGELOG.md`: outcomes A and B only, and Outcome C's few-unscored branch with A's or B's lines —
  the `### Added` line (the measurement and, in A, the ranked-list entry) and the `### Removed` line
  for the haiku default guardian; none otherwise in C.
- `docs/guardian.md`: outcomes A and B, the `opencode-go` exception at `:783-786` (the databricks
  entry in A, no exception in B); outcome C, one clause at the databricks limitation, and in the
  few-unscored branch also A's or B's edit.

## Testing Strategy

- The only shipped artifact is data: `guardian_models.toml` is embedded with `include_str!`
  (`rust/fontaine-sdk/src/guardian/models.rs`) and parsed by `validated_models()`
  (`rust/fontaine-sdk/src/core/models.rs`). `cargo test -p fontaine-sdk --test models` covers it: the
  shipped list parses and every entry clears the bar
  (`the_shipped_model_list_parses_and_every_entry_clears_the_bar`), which is what a malformed entry
  or a bad hand-edit would break. `pr` runs the workspace's `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings` and `cargo test` as usual.
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
