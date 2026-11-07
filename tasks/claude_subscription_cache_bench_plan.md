# claude-subscription: Live Prompt-Cache Bench Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/483

## Overview

`claude-subscription` token usage depends on CLI behavior Fontaine does not control: what the CLI
puts in the prompt, where it places cache breakpoints, and its cache TTL. #449 found two silent
regressions of this kind by hand; a cache miss changes no output, only usage, so nothing catches the
next one. This plan adds a manual/nightly bench to `fontaine-evals` that runs one fixed multi-step
read task through `fontaine -p` on a claude-subscription account and through `claude -p`, compares
the per-step cache write, flags a ratio above a threshold, and writes a scorecard. It needs one small
product addition: a per-model-call usage event on `fontaine -p --output-format stream-json`, because
today a headless run only reports totals.

## Current State

- The usage ledger keeps **totals per source**, not per request
  (`rust/fontaine-core/src/usage_ledger.rs`; `Bucket::add`). The issue assumes per-request figures
  exist; they do not outside the TUI.
- Each main or subagent model call already raises `SessionEvent::Usage { usage, calls_tool }`
  (`rust/fontaine-core/src/turn.rs:583`). Headless ignores everything but `calls_tool` there
  (`rust/fontaine/src/headless.rs:556`), so no per-call usage reaches stdout.
- The stream-json schema is versioned with additive fields allowed (`docs/headless.md`, "Output").
- `claude -p --output-format stream-json --verbose` emits `system/init` (model, CLI version) and
  `assistant` events whose `message.usage` carries `input_tokens`, `cache_read_input_tokens` and
  `cache_creation_input_tokens`; `fontaine-evals baseline capture` already drives it
  (`rust/fontaine-evals/src/baseline.rs`, `capture_one`).
- Measured gap today (CLI 2.1.319): about 12.3k tokens written per step against a native 6k (measured on four 6000-token user messages, not this bench's ~2k-token tool
  results), because
  the CLI adds context to the final message (`docs/claude-subscription.md`, "What the CLI still
  adds"; #480).
- `fontaine-evals` is a clap CLI with a `baseline` subcommand (`rust/fontaine-evals/src/runner.rs`,
  `Command`); scorecards live in `evals/scorecards/*.json`.

## Design

### Per-call usage in headless output

A new `model_call` stream-json event, emitted from the `SessionEvent::Usage` arm of
`run_one` in `headless.rs`, after the call's assistant text:

```
{"type":"model_call","input_tokens":..,"output_tokens":..,"cache_read_input_tokens":..,
 "cache_creation_input_tokens":..,"calls_tool":bool, ...envelope}
```

Additive, so `schema_version` stays 1. `docs/schemas/headless-v1.schema.json` is a top-level `oneOf`
over explicit event types, so it gains a `model_call` `$def` (envelope plus integer token fields and
boolean `calls_tool`) referenced from that `oneOf`. `cache_creation_input_tokens` is taken from
`usage.cache_creation_input_tokens.unwrap_or(0)`. It covers main and subagent calls in event order; guardian
calls are not `SessionEvent`s and stay out. The bench runs with no subagents, so every `model_call`
is a main step. No cost field (headless usage lines carry token counts only).

### Bench (`fontaine-evals cache-bench`)

New module `rust/fontaine-evals/src/cache_bench.rs`, new `Command::CacheBench`:

| Flag | Meaning |
|---|---|
| `--model <account>/<model>` | Fontaine model reference on a claude-subscription account (required) |
| `--claude-model <alias>` | `claude -p --model` value; default is the part after `/` in `--model`, passed verbatim (the CLI also accepts a full model id) |
| `--files <n>` | Fixture files read one at a time, default 6 |
| `--threshold <x>` | Flag ratio, default 2.0 |
| `--fontaine <path>`, `--claude <path>` | Executables, defaults `fontaine` / `claude` |
| `--out <path>` | Scorecard, normally `../evals/scorecards/claude-subscription-cache-<model>.json` |

Flow:

1. Materialize one scratch workspace per side with `n` files of fixed, equal-sized text (about 2k
   tokens each, so the per-step write is far above noise).
2. Fixed prompt: read `f1.txt`..`fn.txt` one at a time, then reply DONE.
3. Run `fontaine -p <prompt> --model <ref> --output-format stream-json --allowed-tools read` and
   `claude -p <prompt> --model <alias> --output-format stream-json --verbose
   --no-session-persistence --allowed-tools Read`, each with a 300 s timeout (the live-bench
   default timeout, not the targeted-probe one).
4. Parse each stream into `Vec<Step>` (`input`, `cache_read`, `cache_creation`, `output`):
   Fontaine from `model_call` events; Claude Code from `assistant` events, one step per distinct
   `message.id` (keep the last usage seen for an id, since a message can span several events).
   Record the CLI version (`claude --version`) and each side's served model (`init.model`).
5. Summarize: per-step table, totals, base context (step 1 read + creation), and the ratio
   the mean creation of all steps except the first and last, Fontaine over Claude Code. The first step
   differs by base context and the last by the answer-only turn, so both are excluded from the ratio
   and shown in the table. Fewer than 3 steps on either side is a bench failure, not a pass.
6. Write the scorecard (steps, totals, ratio, threshold, flagged, versions, served models, date) and
   print the table. Exit 0 under the threshold, `EXIT_FAIL` (1) when flagged or a side failed,
   `EXIT_USAGE` (2) for bad flags (both constants from `rust/fontaine-evals/src/lib.rs`).

The 2.0 default matches the documented 2× from #480; lowering it after a fix is a one-line change.
Token counts only: the 5-minute versus 1-hour TTL write rates (#493) are not in rig's `Usage`, so
the ratio does not credit that saving.

## Implementation Steps

1. `rust/fontaine/src/headless.rs`: emit `model_call` from the `SessionEvent::Usage` arm, mapping rig's
   `cached_input_tokens` to `cache_read_input_tokens` as `usage_ledger.rs` does and using `unwrap_or(0)`
   for every Option field (`cache_creation_input_tokens` from `usage.cache_creation_input_tokens.unwrap_or(0)`). Keep the
   budget-cap logic unchanged. Add the `model_call` `$def` to `docs/schemas/headless-v1.schema.json`
   and reference it from the top-level `oneOf`.
2. `rust/fontaine-evals/src/cache_bench.rs`: `Step`, `parse_fontaine_stream`, `parse_claude_stream`,
   `summarize`, `Scorecard`, the fixture writer. It reuses `claude_version` and `events` from
   `rust/fontaine-evals/src/baseline.rs` (made `pub(crate)`) and one shared timeout-wrapped spawn
   helper instead of two new runners.
3. `rust/fontaine-evals/src/runner.rs` and `lib.rs`: `Command::CacheBench` and its dispatch; export
   the module; return `EXIT_FAIL` / `EXIT_USAGE` from `lib.rs`.
4. Docs and changelog (below).

## Files to Modify

- `rust/fontaine/src/headless.rs`
- `rust/fontaine-evals/src/cache_bench.rs` (new), `lib.rs`, `runner.rs`
- `rust/fontaine/tests/headless_contract.rs`, `docs/schemas/headless-v1.schema.json`
- `docs/headless.md`, `docs/claude-subscription.md`, `evals/README.md`, `CHANGELOG.md`

## Trade-offs

- **Drive `fontaine -p` and add an event** versus calling the provider from the bench through
  `fontaine-core`: the issue asks for the real headless surface, and the event is small, additive
  and useful to any consumer.
- **Per-run totals diffed across runs of different length** gives the same per-step write only on
  average and doubles live cost; the event gives exact steps.
- **Token ratio versus billed cost ratio**: tokens are what the issue asks for and what rig reports.

## Decisions

- Guardian calls are excluded from `model_call`; the bench runs in the default mode with only
  fast-path reads, so there are none to count.

## Documentation

- `docs/headless.md`: the `model_call` row in the event table, and the "in this order" event-sequence sentence.
- `docs/claude-subscription.md`: a "Checking cache usage" subsection with the command and how to read
  the scorecard; it links the measured numbers in "What the CLI still adds".
- `evals/README.md`: the `cache-bench` command and the new scorecard family.
- `CHANGELOG.md`: the bench and the event.

## Testing Strategy

All automated tests use canned streams and never touch live APIs or the real CLI:

- `cache_bench` unit tests: the Fontaine parser (steps in order, `calls_tool` flag); the Claude Code
  parser (dedupe by `message.id`, last usage wins); `summarize` (ratio over the middle steps,
  excluded first and last, flagged above and unflagged at the threshold, fewer than 3 steps is an
  error, a zero native write is an error rather than a division by zero); scorecard round trip.
- A `cfg(unix)` integration test runs the whole bench against two stub scripts that print canned
  streams, pinning argv (`--output-format stream-json`, `--allowed-tools`, the shared prompt) and the
  exit codes.
- `headless_contract.rs`: a scripted-model run emits one `model_call` per model call with the
  ledger's totals as their sum, and `schema_version` stays 1.

## Manual Verification

1. From `rust/`, run `cargo run -p fontaine-evals -- cache-bench --model claudemax/sonnet --out
   ../evals/scorecards/claude-subscription-cache-sonnet.json` on a signed-in machine. Expect a
   per-step table and a ratio of roughly 2.0 on CLI 2.1.319, to be recorded; commit the scorecard. Not automatable: it
   needs a live subscription and the real CLI, and it measures their behavior, which is the point.
2. Sanity check on the first run that `claude -p` stream-json repeats `message.id` across the events
   of one message.
