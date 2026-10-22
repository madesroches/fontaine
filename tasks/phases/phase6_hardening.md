# Phase 6 — Guardian hardening: tool-result scanner

**Status:** not started · **Depends on:** [Phase 4](phase4_guardian.md) (evals already public;
Phase 5 for the subagent-result path) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Result scanning (the tool-output side of the guardian)

Like Claude Code's server-side probe, a scanner pass over incoming tool *results* that flags
injection-looking content and annotates the result before the main agent reads it. Selector, defaults,
fail-closed behavior, license caveat and release features are the master plan's Decisions (2026-10-21);
background in `research/research-llamafirewall.md`. Subagent reports flow through this same path
(`ResultPipeline`, Phase 2) — see the Phase 5 plan.

## Steps

1. Tool-result injection scanner, attached as the `StageKind::Scanner` stage of `ResultPipeline` (after notifications, before `post_tool_use` hooks), selected by `[guardian] scanner`
   (`off` default | `pg2` | `llm` | `command`; user file only; the one switch — there is no separate on/off boolean).
   A `Scanner` trait in `fontaine-guardian` has three implementations:
   - `pg2`: PromptGuard 2 86M via `candle` — `candle_transformers::models::debertav2::DebertaV2SeqClassificationModel`
     over `model.safetensors`, with `tokenizer.json` and `config.json` (label map from `id2label`); the
     malicious-class score ≥ `FLAG_THRESHOLD = 0.9`, a fixed constant in `scanner.rs` (not configurable; LlamaFirewall's `PromptGuardScanner` default `block_threshold`, PurpleLlama `prompt_guard_scanner.py`) ⇒ flagged. Pins in `rust/Cargo.toml`: `candle-core`,
     `candle-nn`, `candle-transformers` `=0.11.0` and `tokenizers` `=0.23.2` (default features off;
     `features = ["fancy-regex"]`, since tokenizers does not compile without `onig` or `fancy-regex`) — confirm they resolve under the workspace toolchain
     when the step lands. All are optional dependencies of `fontaine-guardian` behind the `pg2` cargo
     feature so the crate stays dependency-light; forwarding features `fontaine/pg2 = ["fontaine-core/pg2"]` and `fontaine-core/pg2 = ["fontaine-guardian/pg2"]` expose it from the bin; `config.rs` checks `cfg(feature = "pg2")`, so `scanner = "pg2"` in a build
     without it is a config error (exit 2). Release builds enable it via `features = ["pg2"]` in `[package.metadata.dist]` of `rust/fontaine/Cargo.toml` (updated in this step). `cargo deny check` must see that tree: add `[graph] features = ["fontaine/pg2"]` to `rust/deny.toml`. Existing tests that load `scanner = "pg2"` and unwrap (`rust/fontaine-core/tests/tools.rs`, `rust/fontaine-core/tests/user_hooks.rs`) switch to `scanner = "llm"`, since it is now a config error in non-pg2 builds. A first spike loads the real weights and checks logits against
     the model card's reference outputs before the step builds on it.
     Add the `pg2` lint/test steps to `.github/workflows/ci.yml` (`cargo clippy --all-targets --features fontaine/pg2 -- -D warnings`; `cargo test --features fontaine/pg2`).
   - `llm`: the cheap-LLM pass on the resolved `[guardian] model` (`query_source = scanner`; the scan runs inside a `fontaine.scanner` span, with a matching `scanner` branch added to the parent-span derivation in `rust/fontaine/src/telemetry/span_bridge.rs`), bounded by `[guardian] timeout_secs` (the guardian's `tokio::time::timeout` pattern; a timeout counts as flagged), prompt in the versioned asset `rust/fontaine-guardian/prompts/scanner_llm.md`; the verdict is extracted via rig's extractor into the same `{verdict: clean|flagged, reason}` shape as the `command` scanner, with the scanned content inside untrusted delimiters; malformed output ⇒ flagged.
   - `command`: `scanner_command` is run directly (no shell) with the hook-style scrubbed environment;
     it receives JSON `{"tool": ..., "content": ...}` on stdin and prints
     `{"verdict":"clean"|"flagged","reason":...}` on stdout; bounded by `[guardian] timeout_secs`. The `pg2` scan is bounded by the same timeout; a timeout counts as flagged.
   Annotation: the stage uses `append_note` (`rust/fontaine-tools/src/result_pipeline.rs`); the result is never withheld. Flagged: a note stating the scanner flagged the result as possible prompt injection, including the scanner's `reason`. Scanner error/timeout/invalid output: a note stating the result is unscanned and suspect.
   Any scanner error, timeout or unparseable/invalid output ⇒ flagged. Surface the gated-download /
   Llama 4 Community License caveat at first `pg2` enable.
   Acquisition (`pg2`): the weights are never bundled. Either the user supplies a local model directory
   (`[guardian] scanner_model_path`: `model.safetensors`, `tokenizer.json`, `config.json`), or first enable
   downloads those three files from the gated HF repo using the user's HF token read from the env var named by
   `[guardian] scanner_hf_token_env` (default `HF_TOKEN`; never stored or logged) after the user has
   accepted the license on HF. Downloads land in `<data dir>/models/promptguard2-86m/`.
   The `pg2` model loads (and downloads, if needed) at startup, not on the first result; the license notice is pushed through `runtime.notices` (`rust/fontaine/src/lib.rs`) and printed in every headless mode (`announce` in `rust/fontaine/src/headless.rs` is changed to stop returning early unless mode is Auto or Plan).
   The default `scanner_hf_token_env = "HF_TOKEN"` is resolved in the resolved config (`rust/fontaine-core/src/config.rs`) whenever unset, regardless of `scanner`, so `scrubbed_env` (`rust/fontaine-core/src/tools.rs`) always removes it from shell/hook environments.
   Resolved-config validation (in `load_unresolved`, not `validate_values`, so docs examples under default features still load) rejects `scanner = "command"` without `scanner_command` and `scanner = "pg2"` in a build without the `pg2` feature (exit 2).
   Behavior is wired to the `[guardian] scanner*` keys already parsed and scope-validated by Phase 0's `config.rs`. The eval runner's `--guardian live:<account>/<model>` mode (CLI in `tasks/design/testing.md`) also routes `kind = scanner` cases to the configured scanner (`rust/fontaine-evals/src/live.rs`): `score_live` in `rust/fontaine-evals/src/runner.rs` routes `Kind::Scanner` to the scanner built from `live::connect`'s `config.guardian.scanner`, and keeps the skip-with-reason when it is `off`.
   Telemetry: scanner verdict and latency on the `tool_result` event (`query_source = scanner` for
   the `llm` scanner's model usage).
2. Docs — result scanning in `docs/guardian.md`: what the scanner flags, how the annotation
   reaches the agent, why off-by-default (latency), the fail-closed behavior, and that scanned results carry into the
   subagent-report path; write the gated-download / Llama 4 Community License caveat up beside
   the in-app first-enable notice; `docs/configuration.md` (resolved `HF_TOKEN` default at the `scanner_hf_token_env` entry; remove the "accepted and validated; they take effect when the scanner ships" line) for `scanner`, `scanner_command` (the BYO
   stdin/stdout JSON contract), `scanner_model_path`, `scanner_hf_token_env` and the `llm` option; the scanner fields in `docs/telemetry.md`; say `pg2` is in release binaries and in source builds only with `--features pg2` (add pg2 to the binary's cargo feature list in `docs/architecture.md` and an install note in `README.md`); append
   a CHANGELOG entry.

## Files created or modified

`rust/fontaine-guardian/src/scanner.rs` (trait + `pg2`/`llm`/`command` implementations; `pg2` behind the cargo feature); `rust/fontaine-guardian/prompts/scanner_llm.md` (compiled in via `include_str!`); `rust/fontaine-core/src/agent.rs` (attach the scanner stage where the `ResultPipeline` is built); `rust/fontaine-core/src/scanner_stage.rs` (attaches the configured scanner as the `ResultPipeline` scanner stage); `rust/fontaine-tools/src/result_pipeline.rs` (only if its stage trait changes); `.github/workflows/ci.yml` (pg2 clippy/test steps); `rust/fontaine/Cargo.toml` (`features = ["pg2"]` in `[package.metadata.dist]`); `rust/Cargo.toml` (candle and tokenizers pins); `rust/deny.toml` (`[graph] features`); `rust/fontaine-core/src/config.rs` (resolved `HF_TOKEN` default, `command` validation); `rust/fontaine-core/src/tools.rs` (`scrubbed_env`); `rust/fontaine/src/{lib,headless}.rs` (startup load, notice delivery); `rust/fontaine/src/telemetry/span_bridge.rs` (`scanner` span); `rust/fontaine-core/tests/{tools,user_hooks}.rs` (`scanner = "llm"`); `rust/fontaine-evals/src/runner.rs`;
`rust/fontaine-evals/src/live.rs` (`scanner`-kind live routing); `docs/{guardian,configuration,telemetry,architecture}.md`; `README.md` (install note).

## Tests owned by this phase

- Scanner logic tests with fixture payloads through `scanner_stage.rs` with a fake `Scanner` implementation: flagged ⇒ flagged annotation; clean ⇒ result unchanged; scanner error ⇒ unscanned-and-suspect annotation; annotation present before the agent reads the result. `HF_TOKEN` is scrubbed from shell/hook env by default.
- Eval live routing (`rust/fontaine-evals/tests/live.rs`): under `--guardian live:<account>/<model>`, `kind = scanner` cases are scored by the configured scanner (fake BYO scanner in the test), not the guardian; this replaces the skip assertion in that file.
- Scanner selection: `scanner` accepts exactly `off`/`pg2`/`llm`/`command` (anything else, or `command` without `scanner_command`, is a config error); default `off` means zero behavior
  change. A `cfg(not(feature = "pg2"))` test asserts `scanner = "pg2"` exits 2; a `cfg(feature = "pg2")` test asserts it is accepted.
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
- CI covers the `pg2` feature: `.github/workflows/ci.yml` also runs `cargo clippy --all-targets --features fontaine/pg2 -- -D warnings` and `cargo test --features fontaine/pg2` (the real-model test skips without `FONTAINE_PG2_MODEL`; the build must resolve offline-safe).
- `scanner = "off"` remains the default; the cargo-dist release build enables the `pg2` feature.
- `rust/fontaine-core/tests/docs.rs` green with the `docs/guardian.md` scanning section in place.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
