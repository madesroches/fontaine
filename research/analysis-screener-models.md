# Analysis: other System One screener models

Issue #403 asked whether another System One "decision model" beats or complements
`typesafe/jev-1.14` as the hosted screen. Five more are served on OpenRouter's `/v1/systemone`
(`baseten/joey-1`, `respan/span-02`, `perplexity/pplx-decider-flash-v1.2`, `cloudflare/clef-flash`,
`liquid/d1.1`). All five answer the `employee-levels` request in the shape the existing parser
reads, so no code was needed to score them.

## Method

`fontaine-evals --guardian screen:<model> --screen-framing employee-levels` on dataset
`f49709411624` (279 guardian cases), through OpenRouter, run twice per model on 2026-11-03 by
`evals/screen_models.sh`. Jev was re-scored in the same session as the control
(`screen-jev-1.14-control-employee-levels*.json`); the earlier committed
`screen-jev-1.14-employee-levels.json` is on the older dataset `8f149c7981df` (258 cases) and is
not directly comparable. AUROC is must-not-clear against allow, from
`research/typed-decision-screen/scripts/scorecard_auroc.py`. `t_allow` is the scorer's
zero-missed-attack allow threshold and "benign cleared" the share of benign cases below it.
Catch and false-deny are at the Jev preset thresholds (0.20 allow, 0.34 stop), which are only
meaningful for Jev. Total spend for the twelve runs was $0.032.

## Results

Runs 1 and 2 are separated by a slash.

| Model | AUROC | atk vs benign | `t_allow` | Benign cleared | Catch @Jev thr. | False-deny @Jev thr. | Fail-closed | p50 ms | p95 ms | Cost / run |
|---|---|---|---|---|---|---|---|---|---|---|
| `typesafe/jev-1.14` | 0.974 / 0.974 | 0.983 / 0.983 | 0.21 / 0.25 | 74.4% / 77.9% | 0.910 / 0.917 | 0.070 / 0.070 | 0 / 0 | 123 / 144 | 182 / 203 | $0.0076 |
| `perplexity/pplx-decider-flash-v1.2` | 0.962 / 0.964 | 0.978 / 0.979 | 0.31 / 0.35 | 61.6% / 67.4% | 0.955 / 0.962 | 0.174 / 0.151 | 0 / 0 | 113 / 110 | 152 / 148 | $0.0011 |
| `cloudflare/clef-flash` | 0.952 / 0.955 | 0.965 / 0.969 | 0.45 / 0.45 | 57.0% / 57.0% | 0.994 / 0.987 | 0.314 / 0.337 | 0 / 0 | 200 / 205 | 370 / 472 | $0.0029 |
| `liquid/d1.1` | 0.944 / 0.948 | 0.952 / 0.958 | 0.15 / 0.13 | 58.1% / 55.8% | 0.821 / 0.827 | 0.116 / 0.093 | 0 / 0 | 134 / 137 | 190 / 188 | $0.0018 |
| `respan/span-02` | 0.942 / 0.945 | 0.952 / 0.949 | 0.13 / 0.16 | 51.2% / 59.3% | 0.705 / 0.744 | 0.058 / 0.070 | 0 / 0 | 463 / 579 | 840 / 2363 | $0.0012 |
| `baseten/joey-1` | 0.877 / 0.878 | 0.909 / 0.910 | 0.52 / 0.52 | 15.1% / 17.4% | 1.000 / 1.000 | 0.860 / 0.849 | 0 / 0 | 138 / 137 | 175 / 172 | $0.0011 |

## Reading

- Jev is still the best ranker (AUROC 0.974) and clears the most benign cases at zero missed
  attacks (74 to 78%). No candidate beats it, and none is a clear complement worth a second read.
- `pplx-decider-flash-v1.2` is the closest: AUROC 0.962 to 0.964, 62 to 67% of benign cleared, the
  lowest latency (p50 about 110 ms) and a seventh of Jev's cost. It is the one to try next as a
  cheaper screen, with its own thresholds (its `t_allow` is about 0.31 to 0.35 against Jev's 0.2
  to 0.25).
- `clef-flash` ranks well (0.95) but its scores sit high: at Jev's thresholds it stops a third of
  benign cases, and its `t_allow` of 0.45 is far from Jev's. It is also the slowest of the fast
  ones (p95 370 to 470 ms).
- `d1.1` and `span-02` rank 0.94 to 0.95. `span-02` is slow and erratic (p50 460 to 580 ms, p95
  up to 2.4 s). At Jev's thresholds `span-02` and `d1.1` catch only 70 to 83%.
- `joey-1` is the worst: AUROC 0.88, a `t_allow` above 0.5 and 15 to 17% of benign cleared. At Jev's
  thresholds it stops 85% of benign calls.
- The `t_allow` of a model moved between its two runs (Jev 0.21 and 0.25, `pplx` 0.31 and 0.35,
  `span-02` 0.13 and 0.16), as the nondeterminism margin in the preset anticipates.

## Limits

- Every guardian state in the dataset is under 1,000 tokens (the longest, 823, was Jev's). The
  16,384-token windows of `joey-1` and `clef-flash` were never reached, so the fail-closed count
  of 0 says nothing about long states. A scorecard with large write states is needed before either
  is trusted for them; a rejected state is a fail-closed read in the product.
- Two runs per model on one dataset: a ranking within 0.01 AUROC (`d1.1`, `span-02`; `clef-flash`,
  `d1.1`) is not established.
- `t_allow` is set by the single hardest attack case, so it is a noisy measure.
- No model was fitted for the preset; a threshold taken from `t_allow` here would need the
  synthetic-corpus fit (`research/typed-decision-screen/scripts/jev_synth.py`) before it ships.
- No run timed out or hit its cost cap: every guardian case got an answer.

## OpenJev

Not measured. OpenJev-Flash-9B (Qwen3.5-9B; there is no 4B build) needs vLLM on a CUDA GPU that
holds the weights or MLX on Apple silicon; this machine has a 6 GB GTX 1060 and no Mac was
available, and downloading the weights was out of scope. The recipe is in
[`docs/evals.md`](../docs/evals.md#local-openjev). The weights are CC BY-NC 4.0, so a product that
bundles or recommends them for commercial use needs a licence review, as the Llama licence does for
PromptGuard 2. Because it was not measured, the keyless loopback account was not built.

## Recommendation

Keep `typesafe/jev-1.14` as the default; do not open an issue to change `JEV_MODEL`. The preset's
defaults now apply only to `typesafe/` models, so any other model used in the preset needs
thresholds measured for it. `pplx-decider-flash-v1.2` is the candidate for a follow-up threshold
fit.
