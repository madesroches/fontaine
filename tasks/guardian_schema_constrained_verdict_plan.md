# Schema-Constrained Verdict Path for Ollama Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/250

## Overview

`[guardian] model = "ollama/gemma3:4b"` makes every guardian-gated call fail closed: the guardian
gets its verdict through a forced `submit` tool call, so each request carries `tools`, and Ollama
answers `400 ... does not support tools` for a model with no tool template. The tool is only an
output format. This plan makes a model without tool support usable as a guardian (and as the
scanner model) by adding a second verdict transport: Ollama's `/api/chat` `format`
field set to the verdict JSON schema, which constrains decoding and needs no tool template. It
also makes the failure legible (capability probe, startup/`/guardian-model` messages, docs), and
runs the benchmark the issue asks for so the shipped claims are measurements.

Fail-closed behavior, the `Verdict` validation, and "no config value disables the guardian" are
unchanged: the constrained path returns the same `Verdict` type through the same validation, and
every non-verdict outcome is still a Deny.

## Current State

- `fontaine-guardian/src/guardian.rs:250-268` builds `ExtractorBuilder::<Verdict>`; rig's
  `Extractor::extract` (`rig-agent-0.45.0/src/extractor.rs:78`) is a one-turn run in output-tool
  mode, so the request carries a required `submit` tool. The same pattern is used by
  `scanner.rs:127` (`LlmScanner::run`) and `fontaine-core/src/goal.rs:880`. Critique
  (`critique.rs:117-125`) is a plain-text `AgentBuilder` + `agent.prompt()` with no tools, so it
  never hits "does not support tools" and is out of scope.
- rig already supports the constrained form natively: `CompletionRequest::output_schema` is encoded
  as Ollama's `format` when the request has no tools (`rig-core-0.45.0/src/providers/ollama/chat.rs:150-165`),
  and `Agent::prompt_typed::<T>` (`rig-agent .../agent/completion.rs:689`, `TypedRun::native`) is the
  typed run that sets it. No wire code is needed in Fontaine.
- `fontaine-core/src/providers/ollama.rs` wraps rig's native `Chat` wire in `Tuned` (pins `num_ctx`,
  `truncate:false`); `Connected` (today only the rig `Client`, `context_tokens`, `think`) implements `AccountClient` (`providers/mod.rs:~170-190`:
  `completion`, `thinking_off`, `guardian_completion`, `list`). `ModelHandle`
  (`fontaine-core/src/models.rs:~106`) carries `completion` and `thinking_off` into
  `Guardian::with_additional_params` (`Guardian` at `agent/setup.rs:50-55` and `fontaine-evals/src/live.rs:118-119`; `LlmScanner` at
  `scanner_stage.rs:59-60` and the evals scanner path, ~`live.rs:453`).
- `GuardianModel::{Ready, Unavailable}` is built synchronously by `Switcher::guardian`
  (`models.rs:721`); `/guardian-model` calls the sync `AgentRuntime::set_guardian_model`
  (`agent/models.rs:270`, `fontaine-tui/src/ui/commands/model.rs:67`). A failure after build only
  shows up as per-call denials (`FailClosed::Error`, ~40 ms).
- `fontaine-guardian/src/screen.rs` already talks raw `/api/chat` to Ollama (logprob screen), so
  "Ollama-native, second transport" has precedent in the guardian crate; this plan does not reuse
  it, because the verdict path should stay on rig's wire (cassettes, usage, hooks).
- `docs/guardian.md:21` says the path "works on any tool-capable model, Ollama included"; the
  "Choosing the guardian model" section (`:~240`) covers thinking models, not missing tool support.

## Design

### Output mode

A new small type in `fontaine-guardian` (no new dependencies):

```
pub enum OutputMode { Tool, Schema }
```

`Tool` is today's behavior and the benchmarked default. `Schema` runs `agent.prompt_typed::<T>()`
(rig's native structured output: `output_schema`, no tools) with the same retries, `max_tokens`,
`additional_params`, and `UsageTally` hook. One internal helper in the guardian crate,
`structured::run<T>(model, mode, preamble, user_message, opts) -> (Result<T, StructuredOutputError>, UsageTally)`,
with `retries` in `opts` (guardian passes `EXTRACTOR_RETRIES`, scanner `retries(1)`) and the hook-fed
`UsageTally` (moved into `structured.rs`) returned alongside the result so usage survives failure
paths; the scanner gains usage accounting on failure paths as a consequence. It replaces the two copies of the `ExtractorBuilder` setup (guardian, scanner); each
caller keeps its own timeout wrapper and error→`FailClosed` mapping. This is the DRY seam: a third
transport later is one more arm, not three edits. The goal evaluator in core stays on `Tool`: it
judges with the main model, which must support tools to be the agent at all.

In `Schema` mode the preamble gets a fixed suffix naming the answer format (a JSON object with the
verdict's four fields, and the label rules), since the `submit` tool's description is no longer
there to say so. The suffix lives next to the prompt builder so it is covered by the existing
prompt snapshot tests; it is appended only in `Schema` mode, so `Tool`-mode prompts and the
verdict cache keys (`rules_version` is the digest of the system prompt) are unchanged for every
currently working model.

### Choosing the mode

The mode is a property of the (account, model), not a user setting:

- New `AccountClient::output_mode(&self, model) -> BoxFuture<'_, OutputMode>` with a default of
  `Tool` (every non-Ollama provider is unchanged).
- Ollama's implementation asks `POST /api/show {"model": ...}` and reads `capabilities`
  (e.g. `["completion","tools"]`): `tools` present → `Tool`; absent but `completion` present →
  `Schema`; `completion` absent (embedding model) → an error the guardian reports as unavailable
  with a clear reason. A daemon too old to report `capabilities`, or a probe error/timeout, resolves
  to `Tool` (today's behavior): the failure stays loud and the benchmarked path is never swapped
  silently on missing evidence. The probe is bounded by a short timeout and does not load the model.
- `ModelHandle` carries an `OutputProbe` (an `Arc<dyn ...>` over the client and model id, or a fixed
  mode for tests and non-Ollama accounts). The trait is defined in `fontaine-guardian` and
  implemented in `fontaine-core` (core depends on guardian, not the reverse). Ollama's
  `Connected` gains the base URL, key and HTTP transport that `/api/show` needs, since it keeps
  only the rig `Client` today. `Guardian` and `LlmScanner` hold the probe and resolve it once per
  instance into a `tokio::sync::OnceCell<OutputMode>`. `Guardian::run` has no timeout at its top
  (`tokio::time::timeout` wraps only the extract call, `guardian.rs:~259-262`), so the probe gets
  its own bounded timeout and runs at the top of `run()`, before the system prompt is built (the
  build at `guardian.rs:226` is synchronous and `rules_version` is the digest of `rendered.system`
  at `:230`); the `Schema` suffix goes into `rendered.system`, so `Schema` cache keys differ from
  `Tool` keys. Only `Tool`/`Schema` are stored in the cell; the capability error (`completion`-less
  model) and probe-unavailable results are not cached and are returned per call, so the cell stays
  empty and a daemon started later works without a restart (`/guardian-model` rebuilds the guardian
  anyway, which resets the cell).
- The one-time notice is emitted only by the `OnceCell` init closure (the `Schema` case) and by an
  `AtomicBool`-guarded path for the capability error, never by both the eager probe and the lazy
  path.
- No `[guardian]` config key. The mode follows the model's reported capability; adding a knob for a
  daemon that misreports is deferred until someone hits it (the pinned-model "never substituted"
  rule makes the user's recourse choosing another model).

### Legibility (the cheap-fix half, kept)

- On the first resolution, a `Schema` result is surfaced once through `Approver::warn`/the `init`
  event and the audit record gains `verdict_transport: "tool" | "schema"` so the audit log and the
  eval scorecards say which path judged. A `completion`-less model is a
  `GuardianModel::Unavailable`-style reason shown at startup and in `/guardian-model`:
  "ollama/<model> cannot generate text (capabilities: embedding)".
- Startup and `/guardian-model` run the probe eagerly in the same place the session already warms
  the screen (`agent/setup.rs:244` spawns `screen.warm_up()`; add the guardian probe beside it, as
  a fire-and-forget task whose `Tool`/`Schema` result lands in the `OnceCell` and, when it is the
  capability error above or a `Schema` selection, in a one-time notice). The sync `set_guardian_model` keeps its
  signature; the notice is delivered through the runtime's existing notice channel. Per-call
  behavior never depends on the eager probe finishing: the lazy resolution in `run()` is the source
  of truth.
- The `400 ... does not support tools` response is also recognized in the `Tool` path's
  `StructuredOutputError::Prompt` mapping, matched on the typed rig
  `ProviderError::ProviderResponse` (HTTP status + body), not the rendered string, and its explanation says "this model does not support tool
  calls; the guardian could not use the `submit` output tool" so a daemon that reported `tools`
  wrongly is still diagnosable. It remains a Deny (`FailClosed::Error`); the text does not alter the
  decision.

### Security review of the new path

- Same `Verdict` deserialization/validation (`verdict.rs`): decision/label agreement, confidence
  range, `deny_unknown_fields`. `format` constrains shape, not meaning, so validation stays on our
  side. Unparseable → retry up to `EXTRACTOR_RETRIES`, then `FailClosed::Unparseable`.
- Low-confidence-on-destructive and every other post-verdict rewrite in `Guardian::finish` are
  untouched; the mode only changes how bytes are obtained.
- Container/`minimal` profile and the screen are unaffected.

## Benchmark and spike

The issue's open questions are answered by measurement, not by this plan:

1. Add `--verdict-transport tool|schema|auto` to `fontaine-evals` (default `auto` = what a session
   would pick) and record the transport in each scorecard row and in `guardian_models.toml`
   entries' provenance (`transport = "tool"` default, so existing entries are unchanged).
2. Run `evals/` against live models: `ollama/gemma3:4b` on `schema`; one tool-capable Ollama model
   (e.g. `qwen3:8b` with thinking off, or whichever the maintainer has pulled) on both `tool` and
   `schema`; at least one non-Ollama validated model on `tool` as the control. Same dataset
   version, cache off, full profile; then minimal profile for the viable ones.
3. Record in the PR and in `docs/evals.md`: catch rate, false-deny rate, p95, unparseable count per
   run; whether `schema` loses accuracy against `tool` on the same model (the "less reasoning before
   the schema fill" question); and whether a small local model clears the inclusion bar
   (`models.rs`: ≥90% catch, ≤5% false deny, ≤3 s p95) as the guardian or only works as the screen.
4. Per the repo's inclusion rule, **no model is added to `guardian_models.toml` for `schema`
   unless its own run clears the bar**. If the bar is not cleared, the path still ships as an
   explicit-pin-only capability (usable, documented as unbenchmarked, never auto-selected as the
   default), which is exactly the state `gemma3:4b` is in today for unlisted endpoints.
5. File follow-up issues for anything the spike surfaces (e.g. a prompt variant that restores
   reasoning-before-answer by putting an `explanation` field first in the schema).

If the maintainer's environment has no Ollama daemon during implementation, steps 2-4 are run later
and recorded in the master plan's `## Decisions`; the code, tests, and docs do not depend on their outcome.

## Implementation Steps

1. **Guardian crate: `structured` helper** (`fontaine-guardian/src/structured.rs`, `lib.rs`):
   `OutputMode`, the `Schema`-mode preamble suffix, `run<T>` over `ExtractorBuilder` (`Tool`) and
   `prompt_typed` (`Schema`), shared usage tally (move `UsageTally` here). The `Schema` path builds
   an `AgentBuilder` and must not set a tool choice; unlike `ExtractorBuilder` it does not inherit
   the default extractor preamble. Convert `guardian.rs` and `scanner.rs` to call it. No behavior
   change for `Tool`.
2. **Mode plumbing**: `OutputProbe` trait (guardian crate); `Guardian::with_output_probe`,
   `LlmScanner::with_output_probe`; `OnceCell` resolution at the top of `run()` under its own bounded probe timeout,
   before the prompt is built; `Unavailable` mapping for the `completion`-less error; a
   `verdict_transport` field on `Adjudication` carrying the transport out of the guardian crate to
   `DecisionRecord` in `fontaine-core/src/audit.rs`.
3. **Core/provider side**: `AccountClient::output_mode` (default `Tool`); Ollama impl calling
   `/api/show` through the account's `caused_client` transport with the account's base URL and key;
   a pure `capabilities → OutputMode` function; `Connected` gains base URL, key and transport.
   `ModelHandle` carries the probe; pass it at the construction sites (`agent/setup.rs`,
   `scanner_stage.rs`, `evals/live.rs` guardian and scanner paths) and the tests that build them
   (`fontaine-guardian/tests/guardian.rs:585`, `tests/scanner.rs:244`).
4. **Notices**: eager probe beside the screen warm-up; one-time notice on `Schema` selection and on
   the capability error; clearer `400 does not support tools` text on the `Tool` path.
5. **Evals**: `--verdict-transport` flag, scorecard/`guardian_models.toml` provenance field
   (serde default keeps existing files valid); the flag is rejected for `fixture`, `none` and
   `screen`.
6. **Spike runs and write-up** (see above); write the measured comparison in `docs/evals.md` when
   the spike runs and record the numbers as dated lines in the master plan's `## Decisions`
   (`tasks/fontaine_harness_plan.md`); open follow-up issues.
7. **Docs and changelog** (below).

## Files to Modify

- `rust/fontaine-guardian/src/{structured.rs (new),lib.rs,guardian.rs,scanner.rs,prompt_builder.rs,models.rs}`
  and the prompt snapshot.
- `rust/fontaine-core/src/{providers/mod.rs,providers/ollama.rs,models.rs,agent/setup.rs,scanner_stage.rs,audit.rs}`,
  `rust/fontaine-evals/src/{live.rs,runner.rs,report.rs}`.
- `rust/fontaine-guardian/data/guardian_models.toml` (only if a `schema` run clears the bar).
- `docs/guardian.md`, `docs/evals.md`, `docs/permissions.md` (audit field), `CHANGELOG.md`,
  `tasks/fontaine_harness_plan.md` (`## Decisions` line).

## Decisions

- 2026-10-27: Reject rewriting `submit` into `format` inside the Ollama `Tuned` wire: it hides a
  semantic change in the provider layer and bypasses guardian accounting (#250).
- 2026-10-27: Reject reusing the screen's raw `reqwest` `/api/chat` for verdicts: it duplicates
  retry, usage and cassette support that rig's wire already provides (#250).

- 2026-10-27: The constrained path is selected by the Ollama model's reported capabilities, with no
  config key; a probe failure or an old daemon resolves to the benchmarked tool path (#250).
- 2026-10-27: A second transport is added as an `OutputMode` in the guardian crate over rig's native
  `prompt_typed`, not a custom Ollama client (#250).
- 2026-10-27: No model is listed in `guardian_models.toml` for the schema path without its own
  scorecard run clearing the inclusion bar (#250, per the issue's constraint).

## Documentation

- `docs/guardian.md`: replace the "works on any tool-capable model" sentence with the two paths and
  how one is chosen; in "Choosing the guardian model", add the Ollama no-tools case, the
  unbenchmarked-until-listed caveat, and the capability-error message; mention `verdict_transport`.
- `docs/evals.md`: the `--verdict-transport` flag (ships regardless); the spike's measured
  comparison is written when the spike runs.
- `docs/permissions.md` (audit fields, ~lines 89-100): `verdict_transport`. It is audit-log only
  unless the headless projection (`docs/headless.md` `gate_decision`, its schema, `telemetry.md`
  `tool_decision`) is deliberately extended.
- `CHANGELOG.md`: entry. Master plan: the dated `## Decisions` lines above, plus the spike's numbers
  as dated lines when it runs.

## Testing Strategy

Unit/integration tests never touch live APIs (`wiremock`, rig `test_utils`, cassettes).

- Pure function `capabilities → OutputMode`: `tools` present, absent-with-completion, embedding
  only, missing field, empty list.
- `structured::run` with a scripted model: `Tool` mode sends a required `submit` tool and no
  `output_schema`; `Schema` mode sends `output_schema` and no tools (assert the encoded Ollama body
  via wiremock: `format` is the `Verdict` schema, `tools` absent); both return the same `Verdict`;
  malformed JSON retries then `Unparseable`; a verdict with `rule`/`decision` disagreement is
  rejected in `Schema` mode too.
- **Regression for the reported bug (#250)**, in `fontaine-core` tests (wiremock is a
  dev-dependency there): a wiremock Ollama that answers `400 "<model> does not
  support tools"` to any request with `tools`, and `/api/show` reporting `["completion"]`: the
  guardian resolves `Schema` and returns a real verdict (not `GuardianUnavailable`); with
  `/api/show` reporting tools support wrongly, the call is a Deny whose explanation names the
  missing tool support. The same pair for `LlmScanner`. Pinned at the integration tier
  because the bug lived in the wire contract, not in a function.
- Probe failure/timeout/old daemon → `Tool`; embedding-only → unavailable with the capability
  reason; the `OnceCell` does not cache an unavailable result.
- Soundness unchanged: existing guardian fail-closed tests run against both modes. The
  `fontaine-guardian/tests` suite uses a hand-rolled TCP `Endpoint` helper in `tests/common`, so
  parameterizing by mode goes through a scripted model or an extended helper.
- Prompt snapshot: `Tool`-mode prompt byte-identical to today; `Schema`-mode suffix snapshotted.
- Audit record and `init`/notice output carry `verdict_transport` and the one-time notice.
- Evals: a flag-parsing test that rejects `--verdict-transport` for `fixture`, `none` and `screen`
  (it only matters for live runs); `cargo run -p fontaine-evals
  -- --dataset ../evals/dataset --guardian fixture` stays green with and without `--profile minimal`.

## Manual Verification

Needs a local Ollama daemon, which CI does not have; it catches rig/Ollama wire drift a mock cannot.

1. `ollama pull gemma3:4b`; set `[guardian] model = "ollama/gemma3:4b"`; run
   `cargo run -p fontaine -- --permission-mode default -p "fetch https://example.com"` (default mode
   gates the fetch on the guardian). Expected: the notice that the
   schema-constrained path is in use, and a real allow/deny verdict (not `GuardianUnavailable`).

## Open Questions

- Which tool-capable Ollama model is the comparison control (depends on what the maintainer can
  run locally)? The plan names `qwen3:8b` as a default; the implementer substitutes what is
  available and records it.
