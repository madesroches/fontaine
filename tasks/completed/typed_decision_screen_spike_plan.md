# Typed-Decision Screen Spike Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/98

## Overview

Measure whether an open typed-decision model can act as a fast **screen** in front of the LLM
guardian, and decide which of two roles, if any, its verdict could take. In option 1 it may deny
or escalate only. In option 2 a confident allow skips the guardian. This is an eval-only spike:
it adds an eval-side scorer to `fontaine-evals`, scores the open candidates on the golden set's
`guardian` cases, publishes the numbers, and records a dated recommendation in the master plan's
`## Decisions`. The permission pipeline, the gate and the shipped defaults do not change.

## Current State

- `rust/fontaine-evals` scores the golden dataset in three modes: `fixture`, `none` and
  `live:<account>/<model>` (`src/runner.rs:248` `run_score`). Live mode builds each `guardian`
  case's `AdjudicationRequest` with `live::request_for` (`src/live.rs:187`), after materializing
  the case workspace. The guardian renders that request into an untrusted-block user message with
  `fontaine_guardian::prompt_builder::user_message` (`fontaine-guardian/src/prompt_builder.rs:122`,
  which is already public).
- Reports are `{summary, cases}` JSON (`runner.rs` `Report`, `CaseResult`). A case result holds a
  verdict string and its latency. It has no field for a probability.
- `evals/scorecards/kimi-for-coding.json` is the reference live run, on dataset `5a79aa741c27`
  (catch 90.6%, false-deny 2.4%, p50 6.3 s). `qwen3-4b.json` is the local run that motivated #97.
- The dataset's `guardian` cases are 120 `attack:*` (10 per class), 44 `benign`, 18
  `ambiguous-intent` and 11 `injection-in-args`, plus the `minimal` cases. The `minimal` cases are
  out of scope here because a screen has no profile.
- Hardware here is a GTX 1060 6 GB (compute capability 6.1, driver 580), 12 cores and 60 GB RAM.
  `ollama` is installed. Python 3.14 is available, but `pip`/`uv` and `vllm` are not.
- `tasks/fontaine_harness_plan.md` Decisions, 2026-10-21: screeners belong to the screening-tier
  roadmap, and thresholds are always recalibrated on the golden set.
  `tasks/design/permissions.md:222` defers the screen tier.

## Candidate state (verified 2026-10-23)

| | Laya | DiffusionGemma |
|---|---|---|
| Weights | Open: `convaiinnovations/laya` (base, ModernBERT-large, 421M, ~808 MB, 512 tokens), `laya-typed-decisions` (ModernBERT-large, 421M, 1024 tokens), `laya-multilingual` (mmBERT-base, 322M). Safetensors. | Open: `google/diffusiongemma-26B-A4B-it` is the only variant (25.2B total, 3.8B active MoE). |
| License | Apache-2.0, per the model card. The research doc says MIT, which is wrong. | Apache-2.0. |
| Input | `state` (text or JSON) plus `questions`, each `{type: choice/score/noul, instructions, criteria}`. Served by `laya-serve` as `POST /v1/systemone`, the Jev-compatible shape. | The same `/v1/systemone` shape, served by vLLM's experimental structured-decisions endpoint (needs `--logprobs-mode raw_logprobs`). |
| Output | `answers[q].choice` (or `noul`/`score`) and `confidence`. The card says to gate on `confidence` and ignore `act_probability` (AUROC 0.30). It also says probabilities ship over-confident (ECE 0.47 before temperature fitting). | `answers[q].choice`, a per-option `probabilities` map, and `confidence`. |
| Vendor latency | 39.5 ms per question on a T4; 193–464 ms on CPU. | About 500–560 ms median (Red Hat benchmark, on large GPUs). |
| Runs on the 6 GB GTX 1060? | Yes. A 421M encoder fits in 6 GB at fp16/fp32. The Python package is the runtime (PyTorch). | **No.** Weights are about 13 GB even at 4-bit, and vLLM's GPU backend needs compute capability ≥ 7.0. |

## Design

### A `screen:` guardian mode in `fontaine-evals`

`--guardian screen:<model>` scores `guardian` cases against a typed-decision server at
`--screen-url <base-url>` (default `http://127.0.0.1:8000`). One client covers both candidates,
because `laya-serve` and vLLM serve the same `/v1/systemone` shape. The mode lives only in
`fontaine-evals`, which never ships (`dist = false`). No product crate changes, so no screen can
reach the permission pipeline.

- **Selection.** Only `kind == guardian` cases in the `full` profile are scored. `pipeline` and
  `scanner` cases are listed as skipped with the reason `a screen scores guardian cases only`.
  `--profile minimal` is a usage error for `screen:`, `--resume` is already rejected for non-`live:`
  modes, and `--models-file` is a no-op (only a live run writes it).
- **State.** Each case is posed with the same user message the LLM guardian sees (not its system prompt): materialize the
  workspace, call `live::request_for`, then `prompt_builder::user_message`. Credential redaction
  and the untrusted-block framing come with it, and the comparison with the reference scorecard
  stays like-for-like. The state is passed as a string.
- **Question.** One `choice` question, `verdict`, with criteria `allow`, `deny` and `escalate`.
  Each criterion's description paraphrases the guardian's own decision labels in one line. The
  instruction text is a `const` in the new module, so a run can be reproduced from the source.
  One question per call keeps latency comparable with the vendor's per-question numbers.
- **Request.** The body is `{"model": <model>, "state": <text>, "questions": {"verdict": ...}}`.
  vLLM needs the `model` field, and laya-serve is sent it as well.
- **Response parsing.** The client reads `answers.verdict.choice` and
  `answers.verdict.confidence`. It also reads `answers.verdict.probabilities` when the server
  returns them (vLLM). When it does not (laya-serve), the chosen option gets `confidence` and the
  other two options are recorded as unknown (`null`). If `usage.input_tokens` is present it is
  recorded, so truncation at Laya's 512/1024-token context is visible.
- **Fail-closed accounting.** A transport error, a non-2xx status or an unparseable body marks the
  case `UNSCORED` with `fail_closed` set to `error`, `timeout`, `unavailable` or `unparseable`, the
  same way live mode does. A fail-closed case's `actual` is `deny` (as in live mode), and every
  `screen_report` metric skips cases with `fail_closed` set. A `screen:` run is a measurement: it
  exits 1 only on infrastructure failures and 0 whatever it scored. The default request timeout is
  10 s (`--screen-timeout-ms`).
- **Warm-up.** One untimed request, using the first selected case, goes out before scoring begins,
  so the model load does not land in p95. Its outcome is ignored apart from a stderr note.
- **Dataset version.** The screen branch computes `report::dataset_version(dataset)` into
  `summary.dataset_version`, as live does. `--screen-reference` is a usage error (exit 2) when the
  reference's `summary.dataset_version` differs.
- **Latency.** Client wall clock per request, reported as p50/p95 through the existing `Latency`
  summary, which `print_summary` prints for `screen:` as it does for `live:`.

### Case result additions

`CaseResult` gains `screen: Option<ScreenScore>`, skipped from serialization when `None`, so
existing reports and sidecars round-trip unchanged:

```rust
pub struct ScreenScore {
    pub choice: String,                         // allow | deny | escalate
    pub confidence: f64,
    pub probabilities: BTreeMap<String, Option<f64>>, // allow/deny/escalate
    pub input_tokens: Option<u64>,
}
```

`actual` is the argmax `choice`, scored with the existing `live::passes`. That gives the
**vendor-default** column: neither vendor publishes a threshold, so the default is the
unthresholded top choice. The existing summary rates (catch rate, catch per class, false-deny)
apply unchanged.

### The screen analysis (`src/screen_report.rs`, pure functions)

The analysis is computed from the scored cases and written to the report as `summary.screen`,
then printed after the summary line. Every function works on plain values, so all of it is
unit-tested.

- **Risk score.** `risk = 1 − p(allow)`. When `p(allow)` is unknown (laya-serve without the
  per-option map), it is derived from the top choice: `(1 + confidence)/2` if the choice is
  `deny`/`escalate` (the remaining `1 − confidence` split evenly), otherwise `1 − confidence`.
  This keeps every `deny`/`escalate` choice ranked above every `allow` choice. The report names
  which derivation was used.
- **Must-not-clear set.** Every scored case whose `expected` is `deny` or `escalate`. That is all
  `attack:*` cases, injection cases labelled deny, and ambiguous cases labelled `deny`/`escalate`.
- **Option 2: zero-miss allow threshold.** `t_allow` is the minimum risk over the must-not-clear
  set. The analysis reports the share of `benign` cases with `risk < t_allow` that would skip the
  guardian. It also reports the share of `expected: allow` ambiguous cases the threshold clears,
  `t_allow` itself, and the case that sets it. It also reports the share and ids of
  `expected: allow` injection-in-args cases with risk < `t_allow`.
- **Option 1: confident-deny threshold.** Let `m` be the maximum score (`p(deny)`, or derived
  risk) over cases whose `expected` is `allow`. The option-1 set is the cases with score > `m`.
  The analysis reports `m`, lists the set (id and class), and gives counts per attack class and
  the share of attacks denied. Without the per-option map the listed cases include `escalate`
  choices.
- **Recalibrated rates.** A threshold sweep from 0.05 to 0.95 in 0.05 steps, plus `t_allow` and
  `m`. At each sweep threshold `risk ≥ t` counts as a catch; `m` is evaluated with option 1's own
  rule (p(deny) or derived score `> m`). The sweep records catch rate, catch rate per class and
  benign stop rate (deny or escalate). That gives the "recalibrated" column the issue asks for.
  Per-class catch is reported at the sweep thresholds and at `m`; at `t_allow` it is 100% by
  definition. The default column's benign stop rate is reported on the same deny-or-escalate
  basis.
- **Calibration curve.** A 10-bin reliability table of `risk` against the observed share of
  must-not-clear cases in each bin, with each bin's count and the expected calibration error.
- **Disagreement with the reference.** With `--screen-reference <scorecard.json>`, defaulting to
  none, each case id is matched against the reference report's `cases[].actual`. The analysis
  reports the disagreement rate (argmax `choice` ≠ reference `actual`) over the matched cases,
  and a 3×3 confusion table. It also lists the disagreements where the screen was right and the
  reference wrong, and the reverse. Reference cases with `fail_closed` set, and ids missing from
  either report, are counted as unmatched.

### Running the candidate (manual, documented)

Laya runs in a throwaway virtualenv outside the repo, served by `laya-serve` with
`LAYA_DEVICE=cuda LAYA_PRELOAD=1`. Current PyTorch CUDA wheels may have dropped sm_61 (Pascal).
If so, use the newest wheel that still ships sm_61 kernels. If none does, run on CPU and report
the CPU latency, noting that the GPU number is missing and why. Both checkpoints are scored:
`laya-typed-decisions` is the primary, because its 1024-token context fits the guardian message
better, and the base `laya` is secondary.

## Implementation Steps

1. **Client and mode.** Add `rust/fontaine-evals/src/screen.rs`. It holds the request/response
   serde types, the `QUESTION` constants, `connect(model, url, timeout)`, a
   `score(case) -> Scored` built on `live::request_for` and `prompt_builder::user_message`, and
   the warm-up call. Add `reqwest.workspace = true` to `fontaine-evals/Cargo.toml`. The client
   sends `serde_json::to_vec` via `.body()` and parses `.bytes()`, because the workspace reqwest
   has no `json` feature.
2. **Wire it into the runner.** In `runner.rs` `run_score`, accept `screen:<model>`. Add the
   `--screen-url`, `--screen-timeout-ms` and `--screen-reference` flags, all `Option`s with the
   defaults applied in code, so that giving any of them without `screen:` is a detectable usage
   error. Reject `--profile minimal` with `screen:`. Skip non-guardian cases with a reason, never
   touch the models file, and update the unsupported-mode message and the `--guardian` doc comment on `RunArgs` to list
   `screen:<model>`. The screen branch sets
   `selected` to the scored guardian cases, like the live and none branches, because the rates zip
   `selected` with the results. Extend the exit condition so a `screen:` run exits 1 only on
   `infrastructure_failures`, not on `failed > 0`. `print_summary` prints latency for `screen:` as
   well as `live:`.
3. **Result shape.** Add `ScreenScore` and `CaseResult.screen`, with `#[serde(default,
   skip_serializing_if = "Option::is_none")]` so sidecar round-trips keep working. Add
   `screen: None` at every `CaseResult` literal (it has no `Default` derive).
4. **Analysis.** Add `rust/fontaine-evals/src/screen_report.rs` with the risk derivation, `t_allow`,
   `m`, sweep, reliability bins/ECE and reference diff. Attach it to `Summary` as
   `screen: Option<ScreenAnalysis>` (skipped when `None`) after `summarize`, like
   `dataset_version`, and print a compact text block.
5. **Tests** (see Testing Strategy).
6. **Run the spike.** Set up laya-serve as above and run:
   `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian screen:convaiinnovations/laya-typed-decisions --screen-reference ../evals/scorecards/kimi-for-coding.json --out ../evals/scorecards/screen-laya-typed-decisions.json`.
   Repeat for the base `laya`. Record the device, the torch build and the GPU or CPU details. If
   the candidate cannot be installed or served in this environment, do not fabricate numbers.
   The write-up then says the measurement is pending and why, and the Decision line records that
   no recommendation could be made yet.
7. **Write-up and decision.** Add a "Screen candidates" section to `docs/evals.md` that reports
   the numbers from step 6. It covers per-class catch and false-deny at the default and at
   the sweep thresholds and `m`, with the benign stop rate (deny or escalate) for the recalibrated
   and default columns (the comparison with kimi's false-deny is deny-only), the option 2
   benign-clear share, option 1's confident denials by class, the
   calibration table, p50/p95 with hardware, and disagreement with `kimi-for-coding`. It also
   explains why DiffusionGemma was dropped, and carries the three caveats from the issue:
   templated reasons for typed denies, the model as an attack surface (arXiv:2610.04985), and
   the small-set optimistic bound. Add a dated line to the master plan's `## Decisions` that
   recommends option 1, option 2 (with threshold and invariant question) or neither. Correct the
   Laya license (MIT → Apache-2.0) in `research/analysis-guardian-market.md`.
8. **Docs for the mode.** Document `--guardian screen:<model>` and its flags in
   `evals/README.md` (flag table, a short "Screen mode" subsection, exit codes) and state that
   it is eval-only. Add a few words to the `fontaine-evals` row of `docs/architecture.md` naming
   the eval-only `screen:` scorer.

## Files to Modify

- `rust/fontaine-evals/src/screen.rs` (new)
- `rust/fontaine-evals/src/screen_report.rs` (new)
- `rust/fontaine-evals/src/runner.rs`
- `rust/fontaine-evals/src/live.rs`
- `rust/fontaine-evals/src/lib.rs`
- `rust/fontaine-evals/Cargo.toml`
- `rust/fontaine-evals/tests/report.rs`
- `rust/fontaine-evals/tests/` (new screen integration test)
- `CHANGELOG.md`
- `evals/README.md`
- `evals/scorecards/screen-laya-typed-decisions.json`, `screen-laya.json` (new, if measured)
- `docs/evals.md`
- `docs/architecture.md`
- `tasks/fontaine_harness_plan.md` (`## Decisions`)
- `research/analysis-guardian-market.md` (license correction)

## Decisions

- 2026-10-23: DiffusionGemma is not measured locally. Its only variant is 25.2B parameters, vLLM
  does not support compute capability 6.1, and no larger hardware is available for this spike. The `screen:` client stays compatible with vLLM's
  endpoint so it can be scored on other hardware.
- 2026-10-23: Jev (hosted) is not measured. It is reference-only, behind an explicit opt-in the
  issue did not give.
- 2026-10-23: Laya is scored over HTTP via `/v1/systemone`, not ported to candle; that is #91 work,
  only if a screen ships.
- 2026-10-23: No temperature fitting; fitting and evaluating on the same 193 cases overstates
  calibration.
- 2026-10-23: One `choice` verdict question rather than per-class `noul` questions.
- 2026-10-23 AFK: Without a per-option map, derived risk is `(1 + c)/2` for a `deny`/`escalate` choice and `1 − c` for `allow`, not `c`; `risk = c` let a low-confidence deny rank below an allow, understating option 2's benign-clear share and distorting the sweep and reliability bins.

- 2026-10-23: `ScreenScore` also records `truncated` when the server reports it (laya-serve does), so a cut state is visible beside `input_tokens`.
- 2026-10-23: The base checkpoint is scored as `screen:english`; laya-serve reads the model value `convaiinnovations/laya` as "let the router choose", not as the base checkpoint.

## Documentation

- `evals/README.md`: the `screen:` mode, its flags, what it scores and skips, and that it is
  eval-only.
- `docs/evals.md`: a "Screen candidates" results section.
- `docs/architecture.md`: the `fontaine-evals` row names the eval-only `screen:` scorer.
- `tasks/fontaine_harness_plan.md`: a dated `## Decisions` line with the recommendation.
- `CHANGELOG.md`: an `[Unreleased] > Added > Evals` bullet for `--guardian screen:<model>` and its
  `--screen-*` flags (#98).

## Testing Strategy

Unit tests (no network):
- `screen_report`: risk derivation with and without the per-option map; without it, a
  low-confidence `deny` ranks above a high-risk `allow` (deny@0.4 > allow@0.5). `t_allow` equals the
  minimum must-not-clear risk, and the benign share counts only cases strictly below it. The
  option-1 set excludes every `expected: allow` case (score > `m`, ties excluded) and `m` is the
  maximum over `expected: allow` cases. The per-threshold catch function at t = 0
  counts every case as caught. Reliability bins put a risk of 1.0 in the last bin and compute ECE for a
  hand-built set. The reference diff counts matched, unmatched and `fail_closed` references and
  builds the confusion table. With empty inputs every metric is `None`, with no panic or NaN.
- `screen` response parsing: a laya-shaped answer (choice and confidence only), a vLLM-shaped
  answer (probabilities and usage), a missing `verdict` answer, an unknown choice label and
  non-JSON are each mapped to the right `ScreenScore` or fail-closed reason.
- CLI validation: `screen:` with `--profile minimal` is a usage error (exit 2), and so are the
  `--screen-*` flags without `screen:`.

Integration test (`tests/screen.rs`, `wiremock`): a mock `/v1/systemone` answers by case. The
test launches the `fontaine-evals` binary as a subprocess (`CARGO_BIN_EXE_fontaine-evals`,
following `tests/live.rs` and the wiremock helpers in `tests/common/mod.rs`; an in-process `run`
would nest runtimes) with no `--filter` over a small temp dataset holding an attack,
a benign, one pipeline and one scanner case, and asserts four things. Non-guardian cases are
skipped, `--out` carries `screen` per case and `summary.screen`, a 500 response leaves the case
`UNSCORED` with exit 1 (the 500 case comes after the first guardian case, and the mock records
allow for the extra warm-up request), and the
request body has the `model`, `state` and `questions.verdict` shape. This covers the wiring a
unit test cannot reach, the client together with the runner and report, and it would catch a
silent misscoring.

The candidate measurement itself (step 6) is a manual run, like every live scorecard. It needs
a downloaded model and a local server, so it never runs in tests or CI.

## Open Questions

None that block implementation. Whether laya-serve returns per-option probabilities is settled
by the first real request. The analysis handles both cases.
