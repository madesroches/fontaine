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
fallback, and a `[model] max_output_tokens` (not settable from the shared project file)
overrides it. Where the wire already fills `max_tokens` in itself (the `anthropic` and
`claude-subscription` kinds) the fallback is not sent; a known limit and the user cap still apply.

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
  unset (`tests/claude_subscription.rs:962`). The `anthropic` kind gets the spec limit from rig's
  `default_max_tokens_for_model` (`providers/anthropic/completion.rs:102-110`), which returns 64000
  for an unspecced `claude-opus-4*`, `claude-sonnet-4*` or `claude-haiku-4-5*` id, and errors
  ("max_tokens must be set for Anthropic", `completion.rs:201`) for a fully unknown id. Gateway
  Messages dialects (`Quirks::gateway()`, `anthropic/wire.rs:137`) send `MaxTokens::Fixed(4096)`,
  including opencode-go's Messages route (`providers/opencode_go.rs:52`).
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
/// limit, else that limit, else `FALLBACK_MAX_OUTPUT_TOKENS`. `None` (send nothing) with no known
/// limit and no user cap on `ollama` (the daemon's own default is unbounded) and on the
/// `anthropic` and `claude-subscription` kinds (the wire fills it in itself).
pub fn turn_max_tokens(&self, cap: Option<u32>) -> Option<u64>
```

| user cap | model limit | sent |
|---|---|---|
| `Some(c)` | `Some(l)` | `min(c, l)` |
| `Some(c)` | `None` | `c` |
| `None` | `Some(l)` | `l` |
| `None` | `None` | `32_000` (`FALLBACK_MAX_OUTPUT_TOKENS`); `None` on `ollama`, `anthropic`, `claude-subscription` |

`FALLBACK_MAX_OUTPUT_TOKENS: u64 = 32_000` is a `pub const` in `models.rs`, which matches the
issue's example and Claude Code's default cap.

The kind is read with `self.kind.as_str()` (`AccountKind` is `struct AccountKind(&'static dyn
Provider)`, `config.rs:253`, not an enum), the way `composition_offer.rs:237` detects ollama
(`== "ollama"`). The model limit is read from `self.completion.spec()`. Nothing on `ModelHandle`
says which wire a handle encodes on, so the Messages-wire exemption is the kind list
`anthropic` and `claude-subscription`. opencode-go's Messages-routed models are not exempted: the
route depends on the account's `wires` overrides, which the handle does not carry.

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
  `Config`: copy it into `Setup` the way `explicit_budget_cap` is (field at `agent.rs:266`, set at
  `agent/setup.rs:904`), then into `ChildContext` where it is built (`agent/setup.rs:1018`, next
  to `explicit_budget_cap` at ~1025). `ChildContext` (`child_runner.rs:303`) holds no `Config`.
  `child_model` computes the value, so it is carried both at spawn (`child_runner.rs:~787`) and
  when a child is rebuilt on a fall-through entry (~1139).

### Config

- `ModelSection` gains `max_output_tokens: Option<u32>`, and `Config` gains
  `max_output_tokens: Option<u32>`, merged like `effort`. A value of `0` is refused at config load
  with the key named in the error: add `bad("model.max_output_tokens", "must be at least 1")` in
  `validate_values` (`config.rs:~1718`) next to the `[model]` check at ~1764, following the
  existing pattern at ~1908/1912.
- Not settable from the shared project file (user trust), which comes free from the existing
  `[model]` rule. The existing `effort_keys_refuse_the_shared_project_file` test pins it.
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
   tests for every row of the table, including the ollama and Messages-kind exemptions.
2. `config.rs`: add `max_output_tokens` to `ModelSection` and `Config`, merge it, refuse `0`. Add
   `"[model]\nmax_output_tokens = 4096\n"` to the snippet list of
   `effort_keys_refuse_the_shared_project_file` (`config_scope.rs:873-884`), and the
   `config_catalog.rs` row, plus an assertion for its unset string in
   `defaults_render_as_documented` (`config_catalog.rs:~755`); `every_settings_leaf_has_a_row`
   makes the row mandatory.
3. `turn.rs`: add the `max_tokens` field to `TurnParams` and `Drive`, pass it through, and apply
   it in `drive_once`.
4. `agent/models.rs`: add `AgentRuntime::turn_max_tokens()`. Fill the field in `headless.rs` and
   `fontaine-tui/src/ui/turn.rs`.
5. `child_runner.rs`: add `ChildContext.max_output_tokens` and `ChildModel.max_tokens`, compute
   it in `child_model`, and pass it to the child's `drive`. Add `Setup.max_output_tokens`
   (`agent.rs`, `agent/setup.rs`) and copy it into `ChildContext` at `agent/setup.rs:1018`.
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
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (Decisions → Providers)

## Trade-offs

- **The model's full limit rather than a lower default** (Claude Code sends 32000 by default for
  many models even when they allow more): the issue asks for the model's `max_output_tokens`.
  The user cap can lower it.
- **`[model]` key rather than a per-account key**: one global cap mirrors
  `CLAUDE_CODE_MAX_OUTPUT_TOKENS`, and clamping per model already adapts it to each account's
  models. A per-account key can be added later without breaking anything.

## Decisions

- 2026-11-10: Fallback output cap when the model's limit is unknown: 32000 tokens (issue's
  example, Claude Code's default cap).
- 2026-11-10: The override is the config key `[model] max_output_tokens` (not settable from the shared project file: user trust), not
  a `CLAUDE_CODE_MAX_OUTPUT_TOKENS`-style environment variable, following the `[network]`
  precedent that config keys keep the scope rules in force.
- 2026-11-10: On `ollama` with no known limit, no `max_tokens` is sent, so the daemon keeps its
  unbounded default.
- 2026-11-10: Accepted risk: an `openai-compatible` local server with a small context (vLLM
  rejects `prompt + max_tokens > max_model_len`) can now refuse an unknown model's 32000 fallback
  request. The fix is `[model] max_output_tokens`, which the docs say so.
- 2026-11-10: Accepted change: a known model limit now replaces the gateway dialect's fixed 4096
  on Messages gateways (opencode-go), and an unspecced opencode-go Messages model sends 32000.
- 2026-11-10: With no known limit and no user cap, the `anthropic` and `claude-subscription`
  kinds send nothing, so the wire's own fill-in stays.

## Documentation

- `docs/configuration.md` `[model]` table: add a `max_output_tokens` row. It covers what is sent
  by default (the model's output limit, else 32000; nothing on `ollama`, `anthropic` or
  `claude-subscription` with no known limit),
  clamping to the model's limit, that it applies to the main agent and subagents but not the
  guardian or `/btw`, that it is not settable from the shared project file, and the
  small-context local-server note.
- `docs/configuration.md` per-key scope table (~lines 105-125): change the row `[model] model` to
  cover `model`, `effort`, `max_output_tokens`.
- `CHANGELOG.md`: an `## [Unreleased]` entry under a `####` subject section (CONTRIBUTING.md:49-51)
  covering the explicit `max_tokens` on main and subagent turns and the new
  `[model] max_output_tokens` key.
- `tasks/fontaine_harness_plan.md` `### Providers`: append dated lines for the Decisions above
  that deviate from Claude Code (the config key instead of an env var, and the ollama exemption).

## Testing Strategy

All unit or in-process integration tests against the mock model. No live service is needed.

- `models.rs` unit tests: `turn_max_tokens` for each table row, the clamp, and the ollama
  and Messages-kind `None`.
- `config.rs` tests: `[model] max_output_tokens = 4096` resolves, `0` is refused with the key
  named, and the project file setting it is refused (the `effort_keys_refuse_the_shared_project_file` snippet
  list in `config_scope.rs`).
- `tests/` integration through `run_turn` with `MockCompletionModel` (the `tests/effort.rs`
  harness: `built.main.requests()`):
  - A main turn on a model with no spec sends `max_tokens == Some(32000)`. With a priced spec it
    sends that spec's `max_output_tokens`. With `[model] max_output_tokens` it sends the clamped
    cap.
  - A main turn on an unspecced handle of a Messages-wire kind (`anthropic`) sends no explicit
    `max_tokens`.
  - A spawned subagent's request carries its own model's limit, or the cap. The `tests/effort.rs`
    harness runs the child on the parent's handle, so use a `tests/model_roles_runtime.rs`-style
    setup with a child model whose spec differs from the main model's.
  - `/model` mid-session: the `tests/effort.rs` harness has no Switcher, so `switch_main_model`
    fails there. The test assigns `runtime.models.main` directly (the `effort.rs:145-147`
    precedent), then checks that `turn_max_tokens()` and the next request carry the new model's
    limit.
- **Regression for the bug seen in the wild** (#571): the `run_turn` mock-model test that a main
  turn on a chat kind sends `max_tokens`, both with a spec and with no spec, is the pinned
  regression. In `a_request_without_reasoning_encodes_byte_identically`
  (`providers/reasoning_parts.rs:336-354`), which already sets `.max_tokens(16)` but only
  destructures `Body::Bytes(a)`, parse it first with
  `let body: serde_json::Value = serde_json::from_slice(a)` and then
  `assert_eq!(body["max_tokens"], 16)`, so the chat wire's encoding of the field is pinned.

## Open Questions

None.
