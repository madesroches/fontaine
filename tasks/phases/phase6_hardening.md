# Phase 6 — Guardian hardening: tool-result scanner

**Status:** not started · **Depends on:** [Phase 4](phase4_guardian.md) (evals already public;
Phase 5 for the subagent-result path) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Result scanning (the tool-output side of the guardian)

Like Claude Code's server-side probe, a scanner pass over incoming tool *results* that flags
injection-looking content ("ignore previous instructions…", credential-shaped strings) and
annotates the result before the main agent reads it. Default scanner: **PromptGuard 2 86M via
ONNX (`ort` crate)** (per the LlamaFirewall findings — user decision 2026-10-21); the cheap-LLM
pass is the fallback / BYO option. PromptGuard 2's HF downloads are gated and Llama 4 Community
License-encumbered — that caveat is surfaced at first enable; a BYO-scanner interface serves
license-clean setups. Off by default for latency; configurable.

Context for the choice: `research/research-llamafirewall.md` §7.2 (PG2 recall @ 1% FPR **97.5%**
English; 19–92 ms — a cheap first layer, not a judge; AgentDojo evidence that small detectors
catch little beyond explicit jailbreak phrasing, which is why this scans results *for the main
agent's benefit* and never substitutes for the guardian). Subagent reports flow through this same
path (`ResultPipeline`, Phase 2) — see the Phase 5 plan.

## Steps

1. Tool-result injection scanner, attached as the first stage of `ResultPipeline` (Phase 2; runs before Phase 5's `post_tool_use` hooks): default scanner is **PromptGuard 2 86M via ONNX (`ort` crate)**
   (a sidecar process and the cheap-LLM pass are the fallback / BYO options), off by default,
   configurable (`[guardian] scan_tool_results`). Surface the gated-download / Llama 4 Community
   License caveat at first enable; expose a BYO-scanner interface for license-clean setups.
   Acquisition: the weights are never bundled. Either the user supplies a local model directory
   (`[guardian] scanner_model_path`: the `.onnx` file plus `tokenizer.json`), or first enable
   downloads them from the gated HF repo using the user's HF token read from the env var named by
   `[guardian] scanner_hf_token_env` (default `HF_TOKEN`; never stored or logged) after the user has
   accepted the license on HF. Downloads land in `<data dir>/models/promptguard2-86m/`
   with their SHA-256 recorded alongside. Pins in `rust/Cargo.toml`: `ort = "=2.0.0-rc.15"` and
   `tokenizers = "=0.22.0"` (default features off) — confirm both resolve under the workspace
   toolchain when the step lands and pin the exact versions that do; both are optional dependencies
   of `fontaine-guardian` behind a `pg2` cargo feature so the crate stays dependency-light.
   Telemetry: scanner verdict and latency on the `tool_result` event (`query_source = scanner` for
   the cheap-LLM fallback's model usage).
2. Docs — result scanning in `docs/guardian.md`: what the scanner flags, how the annotation
   reaches the agent, why off-by-default (latency), and that scanned results carry into the
   subagent-report path; write the gated-download / Llama 4 Community License caveat up beside
   the in-app first-enable notice; `docs/configuration.md` for `scan_tool_results`, the BYO
   scanner interface, and the cheap-LLM fallback; the scanner fields in `docs/telemetry.md`; append
   a CHANGELOG entry.

## Files created or modified

`rust/fontaine-guardian/src/scanner.rs`; `[guardian] scan_tool_results`, `scanner_model_path`,
`scanner_hf_token_env` config plumbing
(`rust/fontaine-core/src/config.rs`); `docs/{guardian,configuration,telemetry}.md`.

## Tests owned by this phase

- Scanner logic tests with fixture payloads against a fake scanner implementing the BYO trait (injection-looking text and credential shapes flagged;
  benign results unflagged; annotation present before the agent reads the result).
- BYO-scanner interface: a test scanner plugs in via the trait; default-off means zero behavior
  change until enabled.
- Subagent reports (ordinary tool results from Phase 5) pass the scanner stage of `ResultPipeline` when enabled: an
  injection-bearing report is flagged before the parent reads it.
- PromptGuard 2 inference test behind the `pg2` cargo feature, skipped unless `FONTAINE_PG2_MODEL`
  points at a local model directory — never part of default CI; every other behavior (flag
  threshold, annotation, default-off, notice) is covered offline by the fake scanner.
- License-notice path: first-enable notice fires exactly once per model download.
- Telemetry: scanner verdict and latency present on `tool_result` and content-free.

## Definition of done

- Offline gates (no keys, no human): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green from `rust/`, including the fake-scanner tests (scanner logic, BYO trait,
  subagent reports, license notice, telemetry) and the default-off test.
- Needs a human and an HF token (not CI): scanning demonstrable in a live session behind the config
  flag with the license caveat surfaced; the `pg2`-feature PromptGuard 2 inference test run against a
  downloaded model (`FONTAINE_PG2_MODEL`).
- Off remains the default.
- `rust/fontaine-core/tests/docs.rs` green with the `docs/guardian.md` scanning section in place.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
