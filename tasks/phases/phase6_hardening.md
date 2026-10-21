# Phase 6 — Guardian hardening: tool-result scanner

**Status:** not started · **Depends on:** [Phase 4](phase4_guardian.md) (evals already public;
Phase 5 for the subagent-result path) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Result scanning (the tool-output side of the guardian)

Like Claude Code's server-side probe, a scanner pass over incoming tool *results* that flags
injection-looking content ("ignore previous instructions…", credential-shaped strings) and
annotates the result before the main agent reads it. Scanner selection is one key,
`[guardian] scanner = "off" | "pg2" | "llm" | "command"`, default `"off"` (latency). `pg2` is
**PromptGuard 2 86M (mDeBERTa-v3-base) on `candle`** running the gated HF repo's safetensors weights
(per the LlamaFirewall findings — user decision 2026-10-21); `llm` is the cheap-LLM pass on the resolved
`[guardian] model`; `command` is the BYO scanner (`scanner_command`). PromptGuard 2's HF downloads are
gated and Llama 4 Community License-encumbered — that caveat is surfaced at first `pg2` enable; `command`
serves license-clean setups. A scanner that errors, times out or prints unparseable output yields
**flagged** (fail closed: the result is annotated as unscanned-and-suspect, never passed as clean).
Release builds enable the `pg2` cargo feature; CI lints and tests the feature path.

Context for the choice: `research/research-llamafirewall.md` §7.2 (PG2 recall @ 1% FPR **97.5%**
English; 19–92 ms — a cheap first layer, not a judge; AgentDojo evidence that small detectors
catch little beyond explicit jailbreak phrasing, which is why this scans results *for the main
agent's benefit* and never substitutes for the guardian). Subagent reports flow through this same
path (`ResultPipeline`, Phase 2) — see the Phase 5 plan.

## Steps

1. Tool-result injection scanner, attached as the first stage of `ResultPipeline` (Phase 2; runs before Phase 5's `post_tool_use` hooks), selected by `[guardian] scanner`
   (`off` default | `pg2` | `llm` | `command`; user file only; the one switch — there is no separate on/off boolean).
   A `Scanner` trait in `fontaine-guardian` has three implementations:
   - `pg2`: PromptGuard 2 86M via `candle` — `candle_transformers::models::debertav2::DebertaV2SeqClassificationModel`
     over `model.safetensors`, with `tokenizer.json` and `config.json` (label map from `id2label`); the
     malicious-class score ≥ `FLAG_THRESHOLD = 0.9`, a fixed constant in `scanner.rs` (not configurable; LlamaFirewall's `PromptGuardScanner` default `block_threshold`, PurpleLlama `prompt_guard_scanner.py`) ⇒ flagged. Pins in `rust/Cargo.toml`: `candle-core`,
     `candle-nn`, `candle-transformers` `=0.11.0` and `tokenizers` `=0.23.2` (default features off;
     enable only what `tokenizer.json` loading needs) — confirm they resolve under the workspace toolchain
     when the step lands. All are optional dependencies of `fontaine-guardian` behind the `pg2` cargo
     feature so the crate stays dependency-light; release builds enable `pg2`; `scanner = "pg2"` in a build
     without it is a config error (exit 2). A first spike loads the real weights and checks logits against
     the model card's reference outputs before the step builds on it.
     Add the `pg2` lint/test steps to `.github/workflows/ci.yml` (`cargo clippy --all-targets --features fontaine-guardian/pg2 -- -D warnings`; `cargo test --features fontaine-guardian/pg2`).
   - `llm`: the cheap-LLM pass on the resolved `[guardian] model` (`query_source = scanner`), prompt in the versioned asset `rust/fontaine-guardian/prompts/scanner_llm.md`; the verdict is extracted via rig's extractor into the same `{verdict: clean|flagged, reason}` shape as the `command` scanner, with the scanned content inside untrusted delimiters; malformed output ⇒ flagged.
   - `command`: `scanner_command` is run directly (no shell) with the hook-style scrubbed environment;
     it receives JSON `{"tool": ..., "content": ...}` on stdin and prints
     `{"verdict":"clean"|"flagged","reason":...}` on stdout; bounded by `[guardian] timeout_secs`.
   Any scanner error, timeout or unparseable/invalid output ⇒ flagged. Surface the gated-download /
   Llama 4 Community License caveat at first `pg2` enable.
   Acquisition (`pg2`): the weights are never bundled. Either the user supplies a local model directory
   (`[guardian] scanner_model_path`: `model.safetensors`, `tokenizer.json`, `config.json`), or first enable
   downloads those three files from the gated HF repo using the user's HF token read from the env var named by
   `[guardian] scanner_hf_token_env` (default `HF_TOKEN`; never stored or logged) after the user has
   accepted the license on HF. Downloads land in `<data dir>/models/promptguard2-86m/`
   with their SHA-256 recorded alongside.
   Telemetry: scanner verdict and latency on the `tool_result` event (`query_source = scanner` for
   the `llm` scanner's model usage).
2. Docs — result scanning in `docs/guardian.md`: what the scanner flags, how the annotation
   reaches the agent, why off-by-default (latency), the fail-closed behavior, and that scanned results carry into the
   subagent-report path; write the gated-download / Llama 4 Community License caveat up beside
   the in-app first-enable notice; `docs/configuration.md` for `scanner`, `scanner_command` (the BYO
   stdin/stdout JSON contract), `scanner_model_path`, `scanner_hf_token_env` and the `llm` option; the scanner fields in `docs/telemetry.md`; append
   a CHANGELOG entry.

## Files created or modified

`rust/fontaine-guardian/src/scanner.rs` (trait + `pg2`/`llm`/`command` implementations; `pg2` behind the cargo feature); `rust/fontaine-guardian/prompts/scanner_llm.md`; `.github/workflows/ci.yml` (pg2 clippy/test steps); `rust/Cargo.toml` (candle and tokenizers pins, `pg2` feature enabled for release builds); `[guardian] scanner`, `scanner_command`, `scanner_model_path`,
`scanner_hf_token_env` config plumbing
(`rust/fontaine-core/src/config.rs`); `docs/{guardian,configuration,telemetry}.md`.

## Tests owned by this phase

- Scanner logic tests with fixture payloads against a fake scanner implementing the BYO trait (injection-looking text and credential shapes flagged;
  benign results unflagged; annotation present before the agent reads the result).
- Scanner selection: `scanner` accepts exactly `off`/`pg2`/`llm`/`command` (anything else, or `command` without `scanner_command`, is a config error); default `off` means zero behavior
  change; each scanner key is rejected from project and project-local scope.
- `command` scanner (a script in a temp dir): clean and flagged verdicts honored; non-zero exit, timeout (time-paused), invalid JSON and a missing program each ⇒ flagged; stdin carries exactly `{tool, content}`; configured credential env vars are absent from its environment.
- `llm` scanner (scripted endpoint): verdict parsed into `{verdict, reason}`; scanned content appears only inside untrusted delimiters in the request; malformed/unparseable verdict, endpoint error or timeout ⇒ flagged.
- Subagent reports (ordinary tool results from Phase 5) pass the scanner stage of `ResultPipeline` when enabled: an
  injection-bearing report is flagged before the parent reads it.
- PromptGuard 2 inference test behind the `pg2` cargo feature, skipped unless `FONTAINE_PG2_MODEL`
  points at a local model directory (safetensors + `tokenizer.json` + `config.json`) — never part of default CI; every other behavior (flag
  threshold, annotation, default-off, notice) is covered offline by the fake scanner.
- License-notice path: first-enable notice fires exactly once per model download.
- Telemetry: scanner verdict and latency present on `tool_result` and content-free.

## Definition of done

- Offline gates (no keys, no human): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green from `rust/`, including the fake-scanner tests (scanner logic, BYO trait, `command`/`llm` fail-closed, selection,
  subagent reports, license notice, telemetry) and the default-off test.
- Needs a human and an HF token (not CI): scanning demonstrable in a live session with `scanner = "pg2"` set, with the license caveat surfaced; the `pg2`-feature PromptGuard 2 inference test run against a
  downloaded model (`FONTAINE_PG2_MODEL`).
- CI covers the `pg2` feature: `.github/workflows/ci.yml` also runs `cargo clippy --all-targets --features fontaine-guardian/pg2 -- -D warnings` and `cargo test --features fontaine-guardian/pg2` (the real-model test skips without `FONTAINE_PG2_MODEL`; the build must resolve offline-safe).
- `scanner = "off"` remains the default; the release build enables the `pg2` feature; `ci.yml` clippy (and tests) with `fontaine-guardian/pg2` are green.
- `rust/fontaine-core/tests/docs.rs` green with the `docs/guardian.md` scanning section in place.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
