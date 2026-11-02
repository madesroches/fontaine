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
  `goal.rs` (~827/876), `fontaine-evals/src/live.rs:127`). Ollama has its own `think` account key (`config.rs:352`).
- The main builder sets `additional_params` only for the native web-search tool (`agent/build.rs:287`).
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
- Scope: user scope only. The existing `user_trust` rows already cover `model.effort` (`[model]`,
  `config_scope.rs:156`) and `agents.types.<name>.effort` (`[agents]`, `:189`), which refuse the shared
  `.fontaine/settings.toml`; `guardian.effort` has no row today, so `config_scope.rs` gets an explicit
  user-only row for it (a key with no row is refused from project scope). Agent-file frontmatter `effort:` is
  barred from repository and plugin agent files the way `model` is (`extensions.rs:323-340`): user-scope
  agent files only.

### Applying it
A single resolver `ModelHandle::generation_options(effort: Option<Effort>) -> (Option<GenerationOptions>, Vec<UnsupportedOption>)`:
`None` yields no options (request unchanged). With a level it starts from rig's `ModelSpec::default_options`
(`spec.rs:345`) with `reasoning` set and, when the handle has a catalog `spec()`, checks it with
`ModelSpec::validate(&GenerationOptions)` (`spec.rs:429`, as `side_question.rs:132` does). A refusal is not
fatal: the level is dropped for that model and the refused options are returned alongside. `ModelHandle` has no
`Approver`, so the caller in `turn.rs`/session emits `Approver::warn` once, with the "warned" flag in session
state. There is no extra clamping: rig's
`ReasoningSupport::Listed` already refuses unlisted efforts (including `max`), and `Unknown` or missing specs
(custom `openai-compatible` ids, `opencode-go`) send the options as is, since rig maps them per dialect and
the gateways accept all values. An explicit effort overrides Ollama's account-level `think` key
(`ollama.rs:189-191`), and `effort = off` equals `think = false`.
- The effective level lives in session state, and `generation_options(effort)` reads it. `AgentBuilder::options`
  bakes options at build time, so it is not used for the main agent; instead each turn applies
  `.options(handle.generation_options(effort))` on rig's per-turn `AgentRunner::options` (rig-agent
  `runner.rs:330`) in `drive_once` (`fontaine-core/src/turn.rs:443-455`).
- Subagents: `fontaine-core/src/child_runner.rs` resolves `agent_type.effort.or(parent effective)` and applies
  it the same way on the child's runner; a child may differ from its parent (effort is not a permission).
- Guardian: `[guardian] effort` set → `with_generation_options` replaces `thinking_off` for that call
  only when the level is not `off`; **a forced tool call still needs thinking off on models that refuse it**,
  so a non-`off` guardian level on such a model yields `GuardianModel::Unavailable{reference, reason}` at
  guardian build, and every call is denied via the existing path (`agent/setup.rs:92-103`) (fail-closed, per
  the security invariants), never silently ignored. Unset or `off` keeps `thinking_off`. The scanner (and
  critique, `project_env`, goal evaluator) ignore `[guardian] effort` and always use `thinking_off`; the
  composed guardian's LLM stage follows `[guardian] effort`.
- `/effort` mid-session rebuilds nothing: it updates the session state read by the next turn's
  `AgentRunner::options`; `/model` switching keeps the level and re-validates against the new model.

### Not in scope
Token-budget style control (`Reasoning::Budget`), an `ultrathink`-style keyword, and the omp-style
accepted-values retry on a 400 (can be filed as a follow-up).

## Implementation Steps
1. `fontaine-core/src/effort.rs`: `Effort`, parsing, `to_reasoning`; unit tests.
2. `config.rs`: `effort` on `ModelSection`, `GuardianSection` (user scope only), agents-types entry; merge,
   source tracking and `config_catalog.rs` entry so `/config` lists them; schema regeneration. `config_scope.rs`: explicit user-only row for `guardian.effort`; tests that `model.effort`, `agents.types.<name>.effort` and `guardian.effort` are refused from project scope (the schema-pinning test).
3. `models.rs`: `ModelHandle::generation_options`, with catalog validation via `ModelSpec::validate` returning the refused options; the once-only `Approver::warn` is emitted by the caller from session state.
4. `turn.rs` (`drive_once`) and `agent/setup.rs`: apply per-turn options for the main agent from session state (no build-time `builder.options`); guardian options and conflict check.
5. `turn::Drive` (`turn.rs:297`): add an options (effort) field, read by `drive_once` for the per-turn `AgentRunner::options`. Update every construction site: `turn.rs:193` and `:395`, `child_runner.rs:685`, `tests/billing.rs`, `tests/audit_telemetry.rs`.
6. Subagents: `AgentType.effort` in `fontaine-agents/src/registry.rs` and the config-side `AgentType` (`config.rs:664`); `SpawnRequest` field in `fontaine-agents/src/spawn.rs`; frontmatter parsing in `fontaine-core/src/extensions.rs`; config-to-registry conversion (`agent/setup.rs:255`); resolution and application in `fontaine-core/src/child_runner.rs` (builder at ~:532-541, drive call at ~:684-685). `fontaine-agents/src/runner.rs` is only the `ChildRunner` trait and resolves nothing.
7. `fontaine/src/cli.rs`: `--effort`; thread into startup config as a CLI-scope override.
8. `fontaine-tui`: `/effort` command (parse, completion, picker-free list of levels), status line mention in `statusbar.rs`.
9. Docs, changelog, master-plan Decisions.

## Files to Modify
`rust/fontaine-core/src/{effort.rs (new),lib.rs,config.rs,config_scope.rs,config_catalog.rs,models.rs,turn.rs,child_runner.rs,extensions.rs,goal.rs,agent/setup.rs}`,
`rust/fontaine-agents/src/{registry.rs,spawn.rs}`, `rust/fontaine/src/{cli.rs,lib.rs}`,
`rust/fontaine-tui/src/{commands.rs,statusbar.rs,ui/mod.rs,ui/commands/*}`, `docs/*`, `CHANGELOG.md`,
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

- 2026-11-02: AFK: `model.effort` and `agents.*.effort` stay under the existing `user_trust` rows, `guardian.effort` gets an explicit user-only row, and frontmatter `effort` is barred from repository/plugin agent files like `model`; reason: most conservative option, since a key with no row is refused from project scope anyway.

## Documentation
- `docs/configuration.md`: `[model] effort`, `[guardian] effort`, `[agents.types]` key, `--effort`.
- `docs/tui.md`: `/effort` in the commands table.
- `docs/headless.md`: `--effort`.
- `docs/agents.md` / `docs/extensions.md`: `effort` frontmatter.
- `docs/guardian.md`: guardian effort and the thinking-off interplay (near l.469).
- `CHANGELOG.md`.
- `tasks/fontaine_harness_plan.md` `## Decisions`, each appended at the end of its subsection:
  - Config: `off` level added beyond Claude Code's set; `xhigh` not exposed; unset sends nothing (model default), unlike a default level; `max` semantics follow the rig/provider mapping rather than Claude Code's session-only `max`.
  - TUI: `/effort` and `--effort` copy Claude Code's surface; the level survives `/model` and is re-validated.
  - Agents: per-type `effort` inherits the session level and may differ from the parent; frontmatter `effort` is user-scope agent files only.
  - Guardian: `[guardian] effort` is user-scope only; the plan's two Decisions above (unsupported level dropped with a warning; non-`off` guardian effort on a thinking-refusing model fails closed).

## Testing Strategy
Unit tests, all against deterministic models and `wiremock`, no live APIs:
- `Effort` parse/display round trip; unknown values rejected with the valid set listed.
- Config: precedence CLI > settings; `model.effort`, `agents.types.<name>.effort` and `guardian.effort` refused in project scope (schema-pinning test in `config_scope.rs`); frontmatter `effort` rejected in repository/plugin agent files; unknown key/value errors.
- `generation_options`: new assertion that `None` sends no reasoning field (no existing tests pin this);
  one `wiremock` check per dialect (Chat `reasoning_effort`, Messages `output_config.effort`) asserting the
  options reach the request; an unsupported level for a catalog spec is dropped and warned once.
- Agents: frontmatter parse; child inherits parent's level unless set; a child can differ from its parent.
- Guardian: non-`off` effort on a thinking-refusing model is Deny; `off`/unset keeps `thinking_off`.
- TUI: `/effort` parse and completion; setting and displaying the session level; `/model` revalidation.
- `fontaine -p --effort high` headless test through the cassette/wiremock harness.
