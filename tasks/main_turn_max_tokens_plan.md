# Main and subagent turns send an explicit max_tokens Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/571

## Overview

Main turns and subagent turns never set `max_tokens`. On the Anthropic Messages wire rig fills
it in from model facts (`default_max_tokens_for_model`), but rig's chat-completions wire
(`rig-core` `providers/openai/wire/chat.rs:336`) only copies `request.max_tokens`. On
`openrouter`, `openai-compatible`, `openai` and the other chat kinds the gateway's default output
cap applies, and that cap can be small or undocumented. One hosted gateway documents an omitted
`max_tokens` as "no limit", yet a long reasoning-heavy Claude turn stopped at `finish_reason=length`
with no answer. Claude Code always sends an explicit cap sized for the model, which
`CLAUDE_CODE_MAX_OUTPUT_TOKENS` can override. Fontaine should do the same: every main and
subagent turn sends a `max_tokens` taken from the model's known output limit, else a fixed
fallback, and a user-scope `[model] max_output_tokens` overrides it.

## Current State

- `fontaine-core/src/turn.rs` `drive_once` (~line 617) builds the rig runner per attempt with
  `.max_turns`, `.tool_concurrency` and, when set, `.options(generation_options)`. It never calls
  `.max_tokens`. The comment there says per-turn values exist so that `/effort` and `/model`
  apply without rebuilding the agent.
- `TurnParams` (turn.rs ~170) and `Drive` (turn.rs ~385) carry `generation_options`, which
  `run_turn` and `drive` pass through.
- Main turn callers: `fontaine/src/headless.rs:680` and `fontaine-tui/src/ui/turn.rs:1020` fill
  `generation_options` from `AgentRuntime::turn_generation_options()`
  (`fontaine-core/src/agent/models.rs:309`).
- Subagents: `child_runner.rs` `child_model` (~line 690) computes `ChildModel.generation_options`
  at spawn, and the child's `drive` call (~line 1025) passes it through.
- `ModelHandle` (`fontaine-core/src/models.rs`) has `completion.spec()`, which gives
  `max_output_tokens: Option<u32>`. Listed models have it, and so do models with derived facts
  (`providers/derived_facts.rs` picks a base only if it has one).
- Precedent: `side_question.rs:209-219` uses a constant, clamped to `spec.max_output_tokens`.
  `goal.rs:969` uses `EVALUATOR_MAX_TOKENS`.
- rig-agent's `PromptRequest` / runner exposes `.max_tokens(u64)` per run (`runner.rs:272`). The
  ollama chat wire maps `max_tokens` to `options.num_predict` (`ollama/chat.rs:144`).
- `claude-subscription` already sends the catalog limit when the request leaves `max_tokens`
  unset (`tests/claude_subscription.rs:962`). The `anthropic` kind gets the same limit from rig.
- Config: `ModelSection` (`config.rs:523`) has `model` and `effort`. The resolved `Config.effort`
  (`config.rs:2674`) is merged at ~2922. The whole `[model]` table is already user-trust only
  (`config_scope.rs:175`, `Rule::user_trust("model", TIGHTEN)`), so a new key under it cannot be
  set from project scope. The `/config` catalog lists `model.effort` at
  `config_catalog.rs:338`.

## Design

### Resolution

Add one method on `ModelHandle` (`models.rs`):

```rust
/// The output cap one turn on this model sends: the user's cap clamped to the model's known
/// limit, else that limit, else `FALLBACK_MAX_OUTPUT_TOKENS`. `None` (send nothing) only on
/// `ollama` with no known limit, where the daemon's own default is unbounded.
pub fn turn_max_tokens(&self, cap: Option<u32>) -> Option<u64>
```

| user cap | model limit | sent |
|---|---|---|
| `Some(c)` | `Some(l)` | `min(c, l)` |
| `Some(c)` | `None` | `c` |
| `None` | `Some(l)` | `l` |
| `None` | `None` | `32_000` (`FALLBACK_MAX_OUTPUT_TOKENS`); `None` on `ollama` |

`FALLBACK_MAX_OUTPUT_TOKENS: u64 = 32_000` is a `pub const` in `models.rs`, which matches the
issue's example and Claude Code's default cap.

### Plumbing

- `TurnParams` and `Drive` gain `pub max_tokens: Option<u64>`. It is passed through `run_turn`
  and `drive` like `generation_options`, and `drive_once` applies it with
  `runner.max_tokens(n)` when it is `Some`. Because it is computed per turn, `/model` takes
  effect on the next turn without a rebuild.
- `AgentRuntime::turn_max_tokens()` (`agent/models.rs`, next to `turn_generation_options`)
  returns `self.models.main.turn_max_tokens(self.config.max_output_tokens)`. Headless and the TUI
  fill the new field from it.
- Subagents: `ChildModel` gains `max_tokens: Option<u64>`. `child_model` computes it at spawn
  from the child's own model and the session's cap, so it is clamped to the child's model and not
  the parent's. The cap reaches `ChildContext` the same way the other spawn-time config values
  do: add a `max_output_tokens: Option<u32>` field, filled where the context is built from
  `Config`.

### Config

- `ModelSection` gains `max_output_tokens: Option<u32>`, and `Config` gains
  `max_output_tokens: Option<u32>`, merged like `effort`. A value of `0` is refused at config load
  with the key named in the error (follow the existing positive-integer validation pattern in
  `config.rs`).
- User scope only, which comes free from the existing `[model]` user-trust rule. A test pins
  this.
- `/config` catalog row `model.max_output_tokens`: `Scalar`, unset shown as
  `"unset (the model's output limit, else 32000)"`.
- No `CLAUDE_CODE_MAX_OUTPUT_TOKENS` environment variable. This follows the precedent that
  settings are config keys, which keeps the scope rules in force (see Decisions).

### Unchanged

The guardian, side questions / advisor, the goal evaluator and the other one-shot callers keep
their own caps. A turn that stops at the cap with no answer is out of scope (separate issue), and
so is mapping host deployment names to facts (#167).

## Implementation Steps

1. `models.rs`: add `FALLBACK_MAX_OUTPUT_TOKENS` and `ModelHandle::turn_max_tokens`, with unit
   tests for every row of the table, including the ollama exemption.
2. `config.rs`: add `max_output_tokens` to `ModelSection` and `Config`, merge it, refuse `0`, and
   regenerate the JSON schema if the repo checks one in (`grep -r JsonSchema` output target).
   Add the `config_scope.rs` test that a project file setting it is refused, and the
   `config_catalog.rs` row.
3. `turn.rs`: add the `max_tokens` field to `TurnParams` and `Drive`, pass it through, and apply
   it in `drive_once`.
4. `agent/models.rs`: add `AgentRuntime::turn_max_tokens()`. Fill the field in `headless.rs` and
   `fontaine-tui/src/ui/turn.rs`.
5. `child_runner.rs`: add `ChildContext.max_output_tokens` and `ChildModel.max_tokens`, compute
   it in `child_model`, and pass it to the child's `drive`.
6. Update every test that constructs `TurnParams` / `Drive` (`tests/*.rs`, see the
   `generation_options: None` sites) to add `max_tokens: None`.
7. Docs and the master plan's Decisions (below).

## Files to Modify

- `rust/fontaine-core/src/models.rs`
- `rust/fontaine-core/src/config.rs`, `config_scope.rs`, `config_catalog.rs`
- `rust/fontaine-core/src/turn.rs`
- `rust/fontaine-core/src/agent/models.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine-tui/src/ui/turn.rs`
- `rust/fontaine-core/tests/*.rs` (the `TurnParams` / `Drive` construction sites, plus new tests)
- `docs/configuration.md`
- `tasks/fontaine_harness_plan.md` (Decisions → Providers)

## Trade-offs

- **Per turn rather than at agent build (`AgentBuilder::max_tokens`)**: building it in would
  freeze the cap to the model at build time and duplicate the `/model` rebuild concerns. A
  per-turn value matches how `generation_options` already works.
- **The model's full limit rather than a lower default** (Claude Code sends 32000 by default for
  many models even when they allow more): the issue asks for the model's `max_output_tokens`.
  It equals what the `anthropic` and `claude-subscription` kinds already send, so those kinds do
  not change, and the user cap can lower it.
- **`[model]` key rather than a per-account key**: one global cap mirrors
  `CLAUDE_CODE_MAX_OUTPUT_TOKENS`, and clamping per model already adapts it to each account's
  models. A per-account key can be added later without breaking anything.
- **Ollama exempt from the fallback**: an unknown local model on `ollama` gets
  `num_predict = 32000` instead of the daemon's unbounded default, which is a new limit with no
  gateway problem to fix. A known limit or the user cap still applies there.

## Decisions

- 2026-11-10: Fallback output cap when the model's limit is unknown: 32000 tokens (issue's
  example, Claude Code's default cap).
- 2026-11-10: The override is the config key `[model] max_output_tokens` (user scope only), not
  a `CLAUDE_CODE_MAX_OUTPUT_TOKENS`-style environment variable, following the `[network]`
  precedent that config keys keep the scope rules in force.
- 2026-11-10: On `ollama` with no known limit, no `max_tokens` is sent, so the daemon keeps its
  unbounded default.
- 2026-11-10: Accepted risk: an `openai-compatible` local server with a small context (vLLM
  rejects `prompt + max_tokens > max_model_len`) can now refuse an unknown model's 32000 fallback
  request. The fix is `[model] max_output_tokens`, which the docs say so.

## Documentation

- `docs/configuration.md` `[model]` table: add a `max_output_tokens` row. It covers what is sent
  by default (the model's output limit, else 32000; nothing on `ollama` with no known limit),
  clamping to the model's limit, that it applies to the main agent and subagents but not the
  guardian or `/btw`, that it is not settable from the shared project file, and the
  small-context local-server note.
- `tasks/fontaine_harness_plan.md` `### Providers`: append dated lines for the Decisions above
  that deviate from Claude Code (the config key instead of an env var, and the ollama exemption).

## Testing Strategy

All unit or in-process integration tests against the mock model or a local HTTP stub. No live
service is needed.

- `models.rs` unit tests: `turn_max_tokens` for each table row, the clamp, and the ollama
  `None`.
- `config.rs` tests: `[model] max_output_tokens = 4096` resolves, `0` is refused with the key
  named, and the project file setting it is refused (`config_scope.rs`).
- `tests/` integration through `run_turn` with `MockCompletionModel` (the `tests/effort.rs`
  harness: `built.main.requests()`):
  - A main turn on a model with no spec sends `max_tokens == Some(32000)`. With a priced spec it
    sends that spec's `max_output_tokens`. With `[model] max_output_tokens` it sends the clamped
    cap.
  - A spawned subagent's request carries its own model's limit, or the cap.
  - `/model` mid-session: the next turn's request carries the new model's limit.
- **Regression for the bug seen in the wild** (#571): on an `openrouter` account over the real
  chat-completions wire, against a local HTTP stub (reuse the stub server already used by the
  wire/provider tests in `fontaine-core/tests`, e.g. `native_search.rs` /
  `providers/reasoning_parts.rs`), assert that the JSON body of a main-turn request contains
  `"max_tokens"`. This pins the actual encoding through rig's chat wire, which the mock-model
  tests cannot see.

## Open Questions

None.
