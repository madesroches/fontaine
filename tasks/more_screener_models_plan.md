# More Screener Models Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/403

## Overview

The hosted `screen` preset asks `typesafe/jev-1.14` the five-level employee question over
OpenRouter's `/v1/systemone`. Issue #403 asks to try other screener models: five more System One
"decision models" on OpenRouter (`baseten/joey-1`, `respan/span-02`,
`perplexity/pplx-decider-flash-v1.2`, `cloudflare/clef-flash`, `liquid/d1.1`) and OpenJev, an
open-weights Jev-compatible model that should be runnable locally. The work is mostly
measurement: score each with the existing eval scorer on the same dataset and framing as Jev,
record the scorecards, and report whether any beats or complements Jev as the default. Two small
code changes follow only if the measurements justify them: a keyless loopback account so a local
OpenJev can sit behind the preset, and (optionally) a different default model.

## Current State

- All six candidates speak the same wire as Jev. A probe on 2026-11-03 sent the
  `employee-levels` request (`systemone::request_body`, `rust/fontaine-guardian/src/systemone.rs`)
  to `https://openrouter.ai/api/v1/systemone` for each of the five hosted models. Every one
  returned `answers.q.probabilities.within` and `choice`, and `usage.cost`, so
  `systemone::parse_levels` and the scorer's `parse_employee_levels`
  (`rust/fontaine-evals/src/screen.rs`) read them unchanged. The hosted models are listed in
  OpenRouter's `/api/v1/models/<id>/endpoints` with output modality `decisions` and prompt prices
  of $0.00000001 to $0.000000038 per token (Jev ~$0.00000004); the `/api/v1/models` list omits
  them, which is why only `typesafe/jev-router` shows there.
- Context limits differ: joey-1 and clef-flash 16,384 tokens (clef-flash's providers 16,384 to
  24,576), liquid d1.1 65,536, span-02 131,072, decider-flash 262,144. The guardian's state for a
  large write can exceed 16K, and a state a model rejects is a fail-closed read, so the scorecards
  must report fail-closed counts per model.
- Eval path: `--guardian screen:<model> --screen-url https://openrouter.ai/api
  --screen-framing employee-levels --screen-api-key-file <file> --screen-max-cost-usd <usd>`
  (`docs/evals.md`, `evals/README.md` "Screen mode"). Existing scorecards:
  `evals/scorecards/screen-jev-1.14-employee-levels.json` and the other `screen-*.json`. The cascade
  run with the preset in front of a guardian is `--guardian live:<acct>/<model> --screen
  live:<acct>/<model>`.
- Preset path: `[guardian.composition] model = "<account>/<model>"` on an `openrouter` account
  accepts any model name; only `JEV_MODEL` in `rust/fontaine-core/src/composition_offer.rs:40` is
  Jev-specific, and the preset's defaults (`allow_threshold = 0.20`, `stop_threshold = 0.34`) are
  Jev's, fitted on the synthetic corpus. Other models need their own thresholds.
- The preset requires a resolvable OpenRouter key (`check_composition_credentials`,
  `rust/fontaine-core/src/config.rs` ~L2969), so it cannot point at a local keyless server.
  `key_may_travel` already allows a bearer key to loopback over http. The eval scorer has no such
  restriction: `--screen-url http://127.0.0.1:3000` works without a key.
- OpenJev (`openjev/openjev` on Hugging Face, CC BY-NC 4.0 weights, Apache-2.0 helper) serves
  `POST /v1/systemone` through `helper/shim.py` in front of vLLM (CUDA) or MLX (Apple silicon).
  There is no 4B build: the official sizes are the 27B flagship (FP8 29 GB, GGUF Q4_K_M 16.5 GB) and
  `OpenJev-Flash-9B` (Qwen3.5-9B; GGUF Q4_K_M 5.6 GB, MLX 4-bit). llama-server alone does not
  speak `/v1/systemone`; the shim is required, with the 9B readout settings from the GGUF card
  (`READOUT_T=1.07 READOUT_NOUL_T=1.074766 READOUT_NOUL_BIAS=0 READOUT_TARGETED=1
  READOUT_INSTR_STYLE=pyrepr`).

## Design

### 1. Measure the five hosted models

For each model run the employee-levels scorer on the current dataset, two repeats (the project
already notes repeated Jev answers differ by up to 0.22; a repeat is a separate run with a `-r2`
suffix on the scorecard name), and write
`evals/scorecards/screen-<model-slug>-employee-levels.json`. The slug drops the account/vendor
prefix (`joey-1`, `span-02`), with `-r2` appended for the second repeat. Jev is re-scored in the same session
as the control, since model versions move (`typesafe/jev-1.14-20261014` may have been re-pinned).
Compare on the scorecard measures already used in `research/analysis-jev-openrouter.md`: AUROC for
must-not-clear vs allow, `t_allow` (zero-missed-attack threshold) and the share of benign cases it
clears, catch rate, fail-closed count, latency p50/p95 and cost. These are measurements, not merge
blockers: a model that loses is recorded, not hidden.

The write-up is a new doc `research/analysis-screener-models.md`, with a summary table
and a new `docs/evals.md` "Other hosted screens" subsection (beside "Hosted screen scorecards") holding the commands and the headline
numbers.

### 2. Measure OpenJev locally

Document and run the local recipe with the Flash-9B build (the smallest official one; the issue's
"Qwen 3.5 4B" does not match any official OpenJev build, see Decisions): vLLM or MLX plus
`helper/shim.py` on `127.0.0.1:3000`, then
`--screen-url http://127.0.0.1:3000 --screen-framing employee-levels` with model `openjev`.
Record the same measures, plus the hardware and the readout settings used, so the run is
reproducible. The weights licence (CC BY-NC) goes into the doc beside the numbers, as the
Llama-licence caveat does for PromptGuard 2 in `AGENTS.md`.

### 3. Keyless loopback account (if the OpenJev run, Implementation step 3, is competitive)

Let an `openrouter`-kind account with a loopback `base_url` run without a key:

- `check_composition_credentials` and `build_reader` (`agent/setup.rs`) treat a missing key as
  allowed when `systemone_endpoint(account)` is loopback (`systemone::loopback`), and build
  `SystemOneSettings` with an empty `Secret` key when no key resolves (the reader is unchanged:
  `rust/fontaine-guardian/src/screen.rs` ~L426-440 sets `keyed = !api_key.is_empty()`, so an empty key
  sends no bearer header and `key_may_travel` is enforced only when keyed; only
  `check_composition_credentials` in `config.rs` and `build_reader` in `agent/setup.rs` need the
  loopback exception); a non-loopback endpoint keeps today's error.
- The exception goes in `screen_key` (`rust/fontaine-core/src/config.rs` ~L1655-1678), which both
  `check_composition_credentials` and `build_reader` call.
- Setup-offer detection (`detect_hosted`, `composition_offer.rs`) is intentionally unchanged: a
  keyless loopback account is never auto-offered and must be configured by hand.
- A remote host without a key stays an exit-2 error, so nothing becomes keyless by accident.
- No new account kind: the existing kind already means "a System One endpoint at `base_url`".

### 4. Default model (only if a candidate clearly wins)

If a candidate wins at zero missed attacks, open an issue to change `JEV_MODEL` and refit
thresholds (`research/typed-decision-screen/scripts/jev_synth.py`).

### 5. Thresholds for non-`typesafe/` models (unconditional)

Today `rust/fontaine-guardian/src/composition/preset.rs` applies the Jev thresholds
(`JEV_ALLOW_THRESHOLD = 0.20`, `JEV_STOP_THRESHOLD = 0.34`) to any hosted System One model; the
"there is no default" error is Ollama-only. Change it so the Jev defaults apply only when the model
id starts with `typesafe/`; any other hosted model must set `allow_threshold` and/or
`stop_threshold`, or keep `shadow = true`, with the same error as the Ollama path, whose text (`preset.rs` ~L203) should name the model and
say thresholds are measured per model. The `typesafe/` requirement applies only when the model is
known (non-empty, the compile path): `check_preset_params` (`composition/mod.rs`, used by
`config.rs` and `composition_offer.rs`) resolves with an empty model and skips it, so parameter
validation does not reject on the empty id.

`rust/fontaine-evals/src/runner.rs` (~L606-616) hard-codes the Jev defaults for any `--screen live:`
model: apply the same `typesafe/`-only defaults there, require explicit `--stop-threshold` and/or
`--allow-threshold` for other models, and update the `--screen` and threshold help text
(~L104-122), which says an `openrouter` account runs the preset defaults.

## Implementation Steps

1. Write a probe-and-run script in `evals/` (shell, not part of CI) that loops the five models plus
   Jev through the scorer with `--screen-max-cost-usd 1` and a key file path argument; document the
   loop in `docs/evals.md`. Repeats are separate runs with a `-r2` suffix on the scorecard name.
2. Run the hosted scorecards (two repeats each) with the key in `~/open_router_key`; commit the
   scorecards under `evals/scorecards/`.
3. Stand up OpenJev Flash-9B locally, run the scorer against it, commit the scorecard.
4. Write `research/analysis-screener-models.md` and the `docs/evals.md` subsection.
5. If the OpenJev run (Implementation step 3) is competitive: implement the keyless loopback
   account (Design 3) with tests, and check the setup error text in
   `rust/fontaine-core/src/composition_offer.rs` (~L337) that mentions the OpenRouter key.
6. Implement the `typesafe/`-only threshold defaults (Design 5) with tests.
7. `CHANGELOG.md` entry; docs updates below.

## Files to Modify

- `evals/scorecards/screen-*-employee-levels.json` (new)
- `research/analysis-screener-models.md` (new)
- `docs/evals.md`, `docs/guardian.md` (hosted-screen section: other models, local OpenJev); stale
  passages to fix: `docs/guardian.md` ~L378-381 (key required; sent only over https or loopback) and
  ~L310, `docs/evals.md` ~L315 (hosted screen described as Jev-specific)
- `rust/fontaine-guardian/src/composition/preset.rs` and its tests (Design 5), `docs/configuration.md`
- `rust/fontaine-evals/src/runner.rs` (Design 5: defaults and `--screen` help text)
- `CHANGELOG.md`
- Conditional (Design 3): `rust/fontaine-core/src/config.rs`, `rust/fontaine-core/src/agent/setup.rs`,
  `rust/fontaine-core/src/composition_offer.rs`, their tests, and `docs/configuration.md`

## Trade-offs

- Measure first, ship nothing, versus swapping the default now: the issue says "try", the
  existing thresholds are Jev-fitted, and the project records opt-in knob results as measurements
  rather than merge blockers.
- Keyless loopback via the existing `openrouter` kind versus a new `systemone` account kind: the
  kind already means "System One endpoint at `base_url`"; a new kind adds config surface for one
  difference (the key).
- Flash-9B versus the 27B flagship for the local run: 9B is the only official build that fits a
  consumer GPU or a Mac; the 27B is recorded as out of scope.

## Decisions

- 2026-11-03 The issue's "Qwen 3.5 4B" OpenJev does not exist as an official build (only 27B and
  Flash-9B on Qwen3.5-9B); the plan uses Flash-9B.
- 2026-11-03 The maintainer authorized live hosted benchmarks up to $3 in total with the key in
  `~/open_router_key`; every scorer run passes `--screen-max-cost-usd` so the sum of caps stays
  under $3 (at ~$0.00003 a call, 258 cases x 2 repeats x 6 models is about $0.016 per model).
- 2026-11-03 No code change is needed to score the hosted models: all five answer the
  employee-levels request in the shape the parser already reads.
- 2026-11-03 AFK: the Jev threshold defaults apply only when the model id starts with `typesafe/`;
  any other hosted model must set both thresholds or leave `shadow = true` (chosen conservatively:
  an allow path on another model with unmeasured thresholds would loosen the guardian).
- AFK: 2026-11-03 Hardware for the local OpenJev is unknown; the OpenJev run (Implementation step 3) is attempted only on a CUDA GPU (vLLM) or Apple-silicon Mac (MLX) that is available; otherwise the doc records the recipe as unmeasured and Design 3 is not built.

## Documentation

- `docs/evals.md`: other hosted screens and the local OpenJev recipe, commands and headline numbers.
- `docs/guardian.md`: that the hosted screen accepts other System One models, that thresholds are
  per model with Jev defaults only for `typesafe/` (also `docs/configuration.md`), and (if
  Implementation step 5 ships) the keyless loopback account.
- `research/analysis-screener-models.md`: the full comparison.

## Testing Strategy

- Measurements are live runs (manual, like every live eval; they are not a CI gate).
- Implementation step 5 (if built), all unit/integration with wiremock or the fixture reader: a loopback
  `openrouter` account with no key builds a reader and sends no `Authorization` header; a
  non-loopback account with no key is still exit 2; `key_may_travel` behavior is unchanged. Composition
  and accounts stay user-only (already enforced in `config_scope.rs`). Implementation step 6: a
  `typesafe/` model gets the Jev defaults; another hosted model with neither threshold and
  `shadow = false` is rejected, and with a threshold or `shadow = true` is accepted. The hosted-one assertion in `an_ollama_preset_still_needs_a_threshold_when_enforced_and_a_hosted_one_does_not`,
  `rust/fontaine-guardian/tests/composition.rs` (~L1430-1440), is split into a `typesafe/` case (no
  thresholds accepted) and a non-typesafe case (rejected, error names the model); the
  empty-model `check_preset_params` path stays accepted.

## Manual Verification

1. `curl` the local shim at `http://127.0.0.1:3000/v1/systemone` with the employee-levels body; the
   response carries `answers.q.probabilities.within`. Not automated because it needs a GPU and
   the 9B weights.
2. Run the scorer against it (`--screen-url http://127.0.0.1:3000`) and check fail-closed is 0.
