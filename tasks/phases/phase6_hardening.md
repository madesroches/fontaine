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
     malicious-class score ≥ `FLAG_THRESHOLD = 0.9`, a fixed constant in `scanner.rs` ⇒ flagged. The whole content is
     tokenized without truncation and scored in 512-token windows (including the special tokens) at stride 256; the result is
     flagged if any window scores ≥ `FLAG_THRESHOLD`. Tokenization and inference run in `tokio::task::spawn_blocking` (model
     held in an `Arc`) wrapped in `tokio::time::timeout` bounded by `[guardian] timeout_secs` for the whole scan; a timeout or
     `JoinError` counts as flagged. The startup model load runs the same way under the same bound. Pins in `rust/Cargo.toml`: `candle-core`,
     `candle-nn`, `candle-transformers` `=0.11.0` and `tokenizers` `=0.23.2` (default features off;
     `features = ["fancy-regex"]`, since tokenizers does not compile without `onig` or `fancy-regex`) — confirm they resolve under the workspace toolchain
     when the step lands. All are optional dependencies of `fontaine-guardian` behind the `pg2` cargo
     feature so the crate stays dependency-light; forwarding features `fontaine/pg2 = ["fontaine-core/pg2"]` and `fontaine-core/pg2 = ["fontaine-guardian/pg2"]` expose it from the bin; `config.rs` checks `cfg(feature = "pg2")`, so `scanner = "pg2"` in a build
     without it is a config error (exit 2). Release builds enable it via `features = ["pg2"]` in `[package.metadata.dist]` of `rust/fontaine/Cargo.toml` (updated in this step). `cargo deny check` must see that tree: add `[graph] features = ["fontaine/pg2"]` to `rust/deny.toml`. Existing tests that load `scanner = "pg2"` and unwrap (`rust/fontaine-core/tests/tools.rs`, `rust/fontaine-core/tests/user_hooks.rs`) switch to `scanner = "llm"`, since it is now a config error in non-pg2 builds. A first spike loads the real weights and checks logits against
     the model card's reference outputs before the step builds on it.
     Add the `pg2` lint/test steps to `.github/workflows/ci.yml` (`cargo clippy --all-targets --features fontaine/pg2 -- -D warnings`; `cargo test --features fontaine/pg2`).
   - `llm`: the cheap-LLM pass on the resolved `[guardian] model` (`query_source = scanner`; the scan runs inside a `fontaine.scanner` span, with a matching `scanner` branch added to the parent-span derivation in `rust/fontaine/src/telemetry/span_bridge.rs`), bounded by `[guardian] timeout_secs` (the guardian's `tokio::time::timeout` pattern; a timeout counts as flagged), prompt in the versioned asset `rust/fontaine-guardian/prompts/scanner_llm.md`; the verdict is extracted via rig's extractor into the same `{verdict: clean|flagged, reason}` shape as the `command` scanner, with the scanned content inside untrusted delimiters; malformed output ⇒ flagged.
   - `command`: `scanner_command` is a single program path run with no arguments and no shell, with the hook-style scrubbed environment; the scanner receives the `scrubbed_env` name list (built in `fontaine-core`'s `scanner_stage.rs`) as a constructor argument, since `fontaine-guardian` cannot depend on `fontaine-core`;
     it receives JSON `{"tool": ..., "content": ...}` on stdin and prints
     `{"verdict":"clean"|"flagged","reason":...}` on stdout; bounded by `[guardian] timeout_secs`; a timeout counts as flagged.
   Annotation: the stage uses `append_note` (`rust/fontaine-tools/src/result_pipeline.rs`); the result is never withheld. Flagged: a note stating the scanner flagged the result as possible prompt injection, including the scanner's `reason`. Scanner error/timeout/invalid output: a note stating the result is unscanned and suspect.
   Any scanner error, timeout or unparseable/invalid output ⇒ flagged. Surface the gated-download /
   Llama 4 Community License caveat on every startup with `scanner = "pg2"`.
   Acquisition (`pg2`): the weights are never bundled. Either the user supplies a local model directory
   (`[guardian] scanner_model_path`: `model.safetensors`, `tokenizer.json`, `config.json`), or a startup with any of them missing
   downloads the missing ones from the gated HF repo using the user's HF token read from the env var named by
   `[guardian] scanner_hf_token_env` (default `HF_TOKEN`; never stored or logged) after the user has
   accepted the license on HF. Downloads land in `<data dir>/models/promptguard2-86m/`.
   The download lives in a new `rust/fontaine-core/src/scanner_model.rs` (behind `fontaine-core/pg2`, with `reqwest` as an
   optional workspace dependency of `fontaine-core` enabled by that feature; `fontaine-guardian` only loads from a directory):
   it fetches only missing files from a base URL constant (a parameter, so tests point it at `wiremock`) with
   `Authorization: Bearer <token>`, writes each to `<file>.part` and renames it into place only after a complete, successful
   response, and removes the `.part` on failure. A missing token, a 401/403 (license not accepted), a network error, or a load
   failure (missing/corrupt files, load timeout) never aborts startup: the session runs with the scanner stage attached in an
   unavailable state that gives every result the unscanned-and-suspect note, plus a startup notice naming the cause, the
   Llama 4 Community License acceptance on HF and the token variable from `scanner_hf_token_env`.
   The `pg2` model loads (and downloads, if needed) at startup, not on the first result. The license notice and any pg2 load-failure
   notice go in a new `AgentRuntime::scanner_notices` (`rust/fontaine/src/lib.rs`), separate from `runtime.notices`: headless
   `announce` (`rust/fontaine/src/headless.rs`) prints `scanner_notices` before the Auto/Plan early return, so in every mode, and leaves
   the `runtime.notices` loop and the guardian-unavailable line where they are; the TUI (`rust/fontaine-tui/src/lib.rs`) shows them
   with its other startup notices.
   The default `scanner_hf_token_env = "HF_TOKEN"` is resolved in the resolved config (`rust/fontaine-core/src/config.rs`) whenever unset, regardless of `scanner`, so `scrubbed_env` (`rust/fontaine-core/src/tools.rs`) always removes it from shell/hook environments.
   Resolved-config validation (in `load_unresolved`, not `validate_values`, so docs examples under default features still load) rejects `scanner = "command"` without `scanner_command` and `scanner = "pg2"` in a build without the `pg2` feature (exit 2).
   Behavior is wired to the `[guardian] scanner*` keys already parsed and scope-validated by Phase 0's `config.rs`. The eval runner's `--guardian live:<account>/<model>` mode (CLI in `tasks/design/testing.md`) also routes `kind = scanner` cases to the configured scanner (`rust/fontaine-evals/src/live.rs`): `score_live` in `rust/fontaine-evals/src/runner.rs` routes `Kind::Scanner` to the scanner built from `live::connect`'s `config.guardian.scanner`, and keeps the skip-with-reason when it is `off`.
   Telemetry: scanner verdict and latency on the `tool_result` event (`query_source = scanner` for
   the `llm` scanner's model usage). `result_pipeline.rs` gains `ScanRecord { verdict: clean|flagged|error, latency_ms }` and a
   provided `ResultStage::process_reported` (default: `process`, then `None`) that the scanner stage overrides;
   `ResultPipeline::run` returns `(ToolOutcome, Option<ScanRecord>)`, and `rust/fontaine-tools/src/guarded.rs` adds
   `scanner_verdict` / `scanner_latency_ms` to its `tool_result` event (absent when no scan ran); the other
   `ResultPipeline::run` caller in `agent.rs` takes the outcome.
   `fontaine-evals` gets a forwarding feature `pg2 = ["fontaine-core/pg2"]`, so live runs with `scanner = "pg2"` in the user config
   need `cargo run -p fontaine-evals --features pg2` (documented in `docs/evals.md`); without it `live::connect` fails with the config error.
2. Docs — result scanning in `docs/guardian.md`: what the scanner flags, how the annotation
   reaches the agent, why off-by-default (latency), the fail-closed behavior, and that scanned results carry into the
   subagent-report path; write the gated-download / Llama 4 Community License caveat up beside
   the in-app startup notice; `docs/configuration.md` (resolved `HF_TOKEN` default at the `scanner_hf_token_env` entry; remove the "accepted and validated; they take effect when the scanner ships" line) for `scanner`, `scanner_command` (the BYO
   stdin/stdout JSON contract), `scanner_model_path`, `scanner_hf_token_env` and the `llm` option; the scanner fields in `docs/telemetry.md`; say `pg2` is in release binaries and in source builds only with `--features pg2` (add pg2 to the binary's cargo feature list in `docs/architecture.md` and an install note in `README.md`); append
   a CHANGELOG entry.

## Files created or modified

`rust/fontaine-guardian/src/scanner.rs` (trait + `pg2`/`llm`/`command` implementations; `pg2` behind the cargo feature); `rust/fontaine-guardian/prompts/scanner_llm.md` (compiled in via `include_str!`); `rust/fontaine-core/src/agent.rs` (attach the scanner stage where the `ResultPipeline` is built); `rust/fontaine-core/src/scanner_stage.rs` (attaches the configured scanner as the `ResultPipeline` scanner stage); `rust/fontaine-tools/src/result_pipeline.rs` (`ScanRecord`, `process_reported`, `run` return); `rust/fontaine-tools/src/guarded.rs` (scanner fields on `tool_result`); `rust/fontaine-core/src/scanner_model.rs` (pg2 download); `rust/fontaine-core/tests/scanner_model.rs` (`cfg(feature = "pg2")`, wiremock); `rust/fontaine-tui/src/lib.rs` (`scanner_notices`); `rust/fontaine-evals/Cargo.toml` (`[features] pg2`); `docs/evals.md` (`--features pg2`); `.github/workflows/ci.yml` (pg2 clippy/test steps); `rust/fontaine/Cargo.toml` (`[features] pg2 = ["fontaine-core/pg2"]` and `features = ["pg2"]` in `[package.metadata.dist]`); `rust/fontaine-core/Cargo.toml` (new `[features] pg2` forwarding to `fontaine-guardian/pg2` and enabling optional `reqwest`); `rust/fontaine-guardian/Cargo.toml` (new `[features] pg2` with optional candle/tokenizers deps); `rust/Cargo.toml` (candle and tokenizers pins); `rust/deny.toml` (`[graph] features`); `rust/fontaine-core/src/config.rs` (resolved `HF_TOKEN` default, `command` validation); `rust/fontaine-core/src/tools.rs` (`scrubbed_env`); `rust/fontaine/src/{lib,headless}.rs` (startup load, notice delivery); `rust/fontaine/src/telemetry/span_bridge.rs` (`scanner` span); `rust/fontaine-core/tests/{tools,user_hooks}.rs` (`scanner = "llm"`); `rust/fontaine-evals/src/runner.rs`;
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
- pg2 windowing: an offline unit test of the window ranges (512 tokens, stride 256, short content ⇒ one window); with
  `FONTAINE_PG2_MODEL`, an injection placed after token 512 of benign text is flagged.
- pg2 download (`rust/fontaine-core/tests/scanner_model.rs`, `cfg(feature = "pg2")`, wiremock): the Bearer token comes from the
  `scanner_hf_token_env` variable; files appear only after a complete response (an interrupted response leaves no final file and
  no `.part`); missing token, 401/403 and a bad model directory each keep startup going with a notice naming the license and the
  token variable, and results then carry the unscanned-and-suspect note.
- License-notice path (`cfg(feature = "pg2")`): the license notice is in `scanner_notices` on every startup with `scanner = "pg2"`
  (from `scanner_model_path` and after a download, whether or not the model loads), and headless prints it in every permission mode
  while `runtime.notices` still prints only in auto/plan.
- Telemetry: `scanner_verdict` and `scanner_latency_ms` present on `tool_result` when the scanner ran, absent when `off`, and content-free.

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

## Decisions

- 2026-10-22 AFK: pg2 scores the whole result in 512-token windows at stride 256 and flags if any window reaches `FLAG_THRESHOLD`, instead of LlamaFirewall's truncation to 512 tokens — an injection past the first window would otherwise pass unflagged; the whole scan stays under `timeout_secs` and a timeout is flagged.
- 2026-10-22 AFK: Scanner verdict and latency reach `tool_result` through a `ScanRecord` returned by `ResultPipeline::run` (via a provided `ResultStage::process_reported` only the scanner stage overrides), recorded by `guarded.rs` as `scanner_verdict` / `scanner_latency_ms` — the smallest change that keeps them on the event without touching the other stages.
- 2026-10-22 AFK: The pg2 license notice shows on every startup with `scanner = "pg2"` (download or `scanner_model_path`, loaded or not) through a separate `AgentRuntime::scanner_notices` that headless prints in every mode; `runtime.notices` and the guardian-unavailable line keep their auto/plan gate.
- 2026-10-22 AFK: The pg2 download lives in `fontaine-core/src/scanner_model.rs` (optional workspace `reqwest` behind `pg2`; `fontaine-guardian` only loads from a directory), writes `*.part` then renames; any download or load failure keeps the session running, with a startup notice naming the cause, license and token variable, and every result gets the unscanned-and-suspect note — the same keep-running, fail-closed-per-call shape as an unavailable guardian.
- 2026-10-22 AFK: `fontaine-evals` gets a forwarding `pg2 = ["fontaine-core/pg2"]` feature, documented as `--features pg2` for live runs with `scanner = "pg2"`, rather than skipping scanner cases — keeps live scanner evals possible and the config error explicit.
- 2026-10-22 AFK: pg2 tokenization, inference and the startup model load run in `spawn_blocking` (model in an `Arc`) under `tokio::time::timeout` bounded by `timeout_secs`, a timeout or `JoinError` counting as flagged — synchronous candle work would otherwise never yield to the timeout and would block a runtime worker.
