# Reasoning Effort Control Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/53

## Overview
Give the user one provider-neutral `effort` setting (`off | low | medium | high | max`) for the main agent,
overridable per session (`--effort`, `/effort`), per agent type (`effort` in agent-file frontmatter and
`[agents.types]`), and set separately for the guardian. Fontaine validates the value and maps it to the wire
through rig's portable `GenerationOptions::reasoning`; the gateway-level probes in the issue comments show
providers accept anything, so validation is Fontaine's job. Claude Code has `/effort`, `--effort` and a
per-subagent `effort`; this copies that surface.

## Current State
- The main agent sends no reasoning parameter, so each model thinks at its own default.
- The only thinking control is `thinking_off` (`providers/mod.rs:185`, `opencode_go.rs:185`), a raw
  `additional_params` JSON that the guardian and scanner use because a forced tool call is refused by a
  thinking model (`agent/setup.rs:75`, `scanner_stage.rs:61`, `project_env.rs:588`, `auto_mode.rs:305`,
  `fontaine-evals/src/live.rs:127`). Ollama has its own `think` account key (`config.rs:352`).
- The main builder sets `additional_params` only for the native web-search tool (`agent/build.rs:285`).
- rig 0.45 offers `GenerationOptions::reasoning(Reasoning::{Off, Effort(Effort), Budget})`, accepted by
  `AgentBuilder::options` (`rig-agent builder.rs:255`) and `CompletionRequest::options`, and a per-model
  catalog `spec()` whose `validate(&options)` refuses unsupported controls (used in `side_question.rs:132`).
- Agent types carry `model`, `mode`, `tools` (`fontaine-agents/src/registry.rs:60-85`); settings sections
  are in `fontaine-core/src/config.rs` (`ModelSection` l.424, `GuardianSection` l.430); CLI flags in
  `fontaine/src/cli.rs` (`--model` l.48); `/model` is `Command::Model` in `fontaine-tui`.

## Design

### Type
`fontaine_core::effort::Effort` — `Off | Low | Medium | High | Max`, serde lowercase, `FromStr` and
`Display`, JSON-schema derived. `to_reasoning(self) -> rig Reasoning` (`Off → Reasoning::Off`,
others → `Reasoning::Effort(..)`). Parsing is the validation: anything else is a config error naming the
valid set. `medium` is included because rig and Claude Code have it; `xhigh`/`minimal` are not exposed.

### Settings
- `[model] effort` — the main agent's level. Unset = send nothing (today's behavior; the model's own default).
- `[guardian] effort` — the guardian's level. Unset = today's behavior (`thinking_off`).
- `[agents.types.<name>] effort` and agent-file frontmatter `effort:` — per-type; unset inherits the
  session's effective level.
- `--effort <LEVEL>` CLI flag overrides `[model] effort`; `/effort [level]` sets it for the session
  (no argument shows the current value and the model's supported set).
- Scope: all of these are user-or-project safe — effort only changes cost and latency, not permissions —
  so no new loosening rules. A repository agent file may set `effort` (it names no model but may narrow
  nothing about authority). Guardian effort is excluded from project scope (the guardian's behavior is a
  user-scope concern, as `[guardian] model` is).

### Applying it
A single resolver `ModelHandle::generation_options(effort: Option<Effort>) -> Option<GenerationOptions>`:
`None` yields no options (request unchanged). With a level it builds `default_options`-style options with
`reasoning` set, and, when the handle has a catalog `spec()`, calls `spec.validate`; a refusal is not fatal:
the level is dropped for that model and an `Approver::warn` says why once. Without a catalog spec (custom
`openai-compatible` ids, `opencode-go`) the options are sent as is, since rig maps them per dialect and the
gateways accept all values.
- Main agent: `builder.options(..)` in `agent/build.rs` beside the native-search `additional_params`.
- Subagents: `runner.rs` resolves `agent_type.effort.or(parent effective)`; a child may differ from its
  parent (effort is not a permission), recorded per agent in the tree info for the status list.
- Guardian: `[guardian] effort` set → `with_generation_options` replaces `thinking_off` for that call
  only when the level is not `off`; **a forced tool call still needs thinking off on models that refuse it**,
  so a non-`off` guardian level on such a model is rejected at guardian build with a clear
  `GuardianUnavailable`-style error (fail-closed, per the security invariants), never silently ignored.
  Unset or `off` keeps `thinking_off`. The scanner and screen follow `[guardian] effort` unchanged: they
  always use `thinking_off`.
- `/effort` mid-session rebuilds nothing: the effective level lives in session state and is read when each
  request's options are built; `/model` switching keeps the level and re-validates against the new model.

### Not in scope
Token-budget style control (`Reasoning::Budget`), an `ultrathink`-style keyword, and the omp-style
accepted-values retry on a 400 (can be filed as a follow-up).

## Implementation Steps
1. `fontaine-core/src/effort.rs`: `Effort`, parsing, `to_reasoning`; unit tests.
2. `config.rs`: `effort` on `ModelSection`, `GuardianSection` (user scope only), agents-types entry; merge,
   source tracking and `config_catalog.rs` entry so `/config` lists them; schema regeneration.
3. `models.rs`: `ModelHandle::generation_options`, with catalog validation and once-only warning.
4. `agent/build.rs` and `agent/setup.rs`: apply options for main agent and guardian; guardian conflict check.
5. `fontaine-agents`: `AgentType.effort`, frontmatter parsing, registry resolution, runner application.
6. `fontaine/src/cli.rs`: `--effort`; thread into startup config as a CLI-scope override.
7. `fontaine-tui`: `/effort` command (parse, completion, picker-free list of levels, status line mention).
8. Docs, changelog, master-plan Decisions.

## Files to Modify
`rust/fontaine-core/src/{effort.rs (new),lib.rs,config.rs,config_catalog.rs,models.rs,agent/build.rs,agent/setup.rs}`,
`rust/fontaine-agents/src/{registry.rs,runner.rs,tree.rs}`, `rust/fontaine/src/{cli.rs,lib.rs}`,
`rust/fontaine-tui/src/{commands.rs,ui/mod.rs,ui/commands/*}`, `docs/*`, `CHANGELOG.md`,
`tasks/fontaine_harness_plan.md`.

## Trade-offs
- Rig's portable `Reasoning` over hand-written per-provider `additional_params`: rig already maps to
  `reasoning_effort`, `output_config.effort` and the rest, and keeps us off a custom provider layer.
- Unset sends nothing rather than defaulting to `high`: it preserves current behavior and Claude Code's
  "model default" semantics, at the cost of the effective level being opaque.
- `medium` kept despite dsh's `off|low|high|max`: Claude Code and rig have it.

## Decisions
- 2026-11-02: Unsupported level for a catalog-known model is dropped with a warning, not an error; reason: effort is a hint, and a session should not die on `/model` to a model with fewer levels.
- 2026-11-02: A non-`off` guardian effort on a model that refuses forced tool calls under thinking fails closed (Deny via unavailable guardian); reason: the guardian invariant allows no silent fallback.

## Documentation
- `docs/configuration.md`: `[model] effort`, `[guardian] effort`, `[agents.types]` key, `--effort`.
- `docs/tui.md`: `/effort` in the commands table.
- `docs/agents.md` / `docs/extensions.md`: `effort` frontmatter.
- `docs/guardian.md`: guardian effort and the thinking-off interplay (near l.469).
- `CHANGELOG.md`; master plan `## Decisions` lines (Claude Code parity: semantic `max` and `xhigh` differ).

## Testing Strategy
Unit tests, all against deterministic models and `wiremock`, no live APIs:
- `Effort` parse/display round trip; unknown values rejected with the valid set listed.
- Config: precedence CLI > settings; guardian effort refused in project scope; unknown key/value errors.
- `generation_options`: `None` leaves the request body unchanged (pin on the existing no-params body tests);
  each level reaches the wire body on the Chat (`reasoning_effort`) and Messages (`output_config.effort`)
  wires via `wiremock`; an unsupported level for a catalog spec is dropped and warned once.
- Agents: frontmatter parse; child inherits parent's level unless set; a child can differ from its parent.
- Guardian: non-`off` effort on a thinking-refusing model is Deny; `off`/unset keeps `thinking_off`.
- TUI: `/effort` parse and completion; setting and displaying the session level; `/model` revalidation.
- Choke-point and `fontaine -p --effort high` headless test through the cassette/wiremock harness.

## Open Questions
- Does a large `max` need per-model clamping (e.g. models without a `max` level) beyond catalog validation?
