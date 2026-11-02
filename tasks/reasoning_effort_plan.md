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
valid set. `medium` is included because rig and Claude Code have it (dsh's set is `off|low|high|max`); `xhigh`/`minimal` are not exposed.

### Settings
- `[model] effort` — the main agent's level. Unset = send nothing (today's behavior; the model's own default), so the effective level is opaque.
- `[guardian] effort` — the guardian's level. Unset = today's behavior (`thinking_off`).
- `[agents.types.<name>] effort` and agent-file frontmatter `effort:` — per-type; unset inherits the
  session's effective level.
- `--effort <LEVEL>` CLI flag overrides `[model] effort`; `/effort [level]` sets it for the session
  (no argument shows the current value and the model's supported set).
- Scope: user scope only. The existing `user_trust` rows already cover `model.effort` (`[model]`,
  `config_scope.rs:156`) and `agents.types.<name>.effort` (`[agents]`, `:189`), which refuse the shared
  `.fontaine/settings.toml`; `guardian.effort` has no row today, so `config_scope.rs` gets an explicit
  user-only row for it (a key with no row is refused from project scope). Agent-file frontmatter `effort:` is
  barred from repository and plugin agent files the way `model` is (`extensions.rs:323-330`): user-scope
  agent files only.

### Applying it
A single resolver `ModelHandle::generation_options(effort: Option<Effort>) -> (Option<GenerationOptions>, Option<UnsupportedOption>)`:
`None` yields no options (request unchanged). With a level it starts from rig's `ModelSpec::default_options(Option<Reasoning>)`
(`spec.rs:345`) with `reasoning` set and, when `self.completion.spec()` yields a catalog spec, checks it with
`ModelSpec::validate(&GenerationOptions)` (`spec.rs:429`, as `side_question.rs:132` does). A refusal is not
fatal: the level is dropped for that model and the refused option is returned alongside. `ModelHandle` has no
`Approver`, so the caller in `turn.rs`/session emits `Approver::warn` once, with the "warned" flag in session
state. There is no extra clamping: rig's
`ReasoningSupport::Listed` already refuses unlisted efforts (including `max`), and `Unknown` or missing specs
(custom `openai-compatible` ids, `opencode-go`) send the options as is, since rig maps them per dialect and
the gateways accept all values. An explicit effort overrides Ollama's account-level `think` key
(`ollama.rs:189-191`), and `effort = off` equals `think = false`.
- The effective level lives in session state. `AgentBuilder::options` bakes options at build time, so it is not
  used; each turn applies finished options on rig's per-turn `AgentRunner::options` (`rig-agent-0.45.0/src/agent/runner.rs:330`) in `drive_once` (`fontaine-core/src/turn.rs:443-455`). `drive_once` has no `ModelHandle` (`Drive`/`TurnParams` carry only `model: String` and `provider: String`; the main handle sits in `Arc<RwLock<MainModel>>`, `child_runner.rs:100,262`, and a child's handle is a local in the spawn code), so the options are resolved and validated before `drive_once`, by the code that holds the current handle (main turn start; child spawn), and passed in a new `Drive` field. For the main turn, `run_turn` (`turn.rs:178`) builds the root `Drive` from `TurnParams`, so `TurnParams` gains one `generation_options` field, set by the two callers that hold the `MainModel` handle (`fontaine-tui` `ui/turn.rs:743`, `fontaine/src/headless.rs:507`) from the current handle and session state; `run_turn` copies it into the root `Drive`. Only `drive_once` turns (main and child) receive the level; side questions (`side_question.rs`) and other internal calls (compaction summarizer) do not. Options are recomputed at each turn start from the current handle, which is how `/model` revalidation reaches the next turn. The once-only warned flag lives in session state.
- Session state: `SessionState`/`StateParams` (`gate.rs:110-139`) gain the effort field and the warned flag, seeded at session setup from `[model] effort` or `effort_override`.
- Subagents: `fontaine-core/src/child_runner.rs` resolves `agent_type.effort.or(parent effective)` and applies
  it the same way on the child's runner; a child may differ from its parent (effort is not a permission).
- Guardian: `[guardian] effort` set → a new generation-options field on `Guardian` (applied to its request) replaces `thinking_off` for that call
  only when the level is not `off`; when the level is non-`off`, `thinking_off` is not passed (`with_additional_params(None)`); **a forced tool call still needs thinking off on models that refuse it**,
  so a non-`off` guardian level on such a model yields `GuardianModel::Unavailable{reference, reason}` at
  guardian build, and every call is denied via the existing path (`agent/setup.rs:~89-91`) (fail-closed, per
  the security invariants), never silently ignored. Unset or `off` keeps `thinking_off` and sends no `Reasoning` options. `agent/setup.rs` reads `[guardian] effort` and builds the Guardian options where `thinking_off` is passed today (`setup.rs:75`, and the composed guardian ~:236). The scanner (and
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
   source tracking and `config_catalog.rs` entry so `/config` lists them; schema regeneration. `config_scope.rs`: explicit user-only row for `guardian.effort`, with its `SNIPPETS` entry (so `every_settings_leaf_has_a_row` in the config_scope.rs tests passes); tests that `model.effort`, `agents.types.<name>.effort` and `guardian.effort` are refused from project scope (the schema-pinning test).
3. `models.rs`: `ModelHandle::generation_options`, with catalog validation via `ModelSpec::validate` returning the refused options; the once-only `Approver::warn` is emitted by the caller from session state.
4. `gate.rs` (`SessionState`/`StateParams`, l.110-139): add the effort field and once-only warned flag, seeded at session setup from `[model] effort` or `effort_override`; update the `StateParams` construction sites (~7, including `fontaine-core/tests/common/pipeline.rs:245` and `fontaine-evals/src/none.rs:133`).
5. `turn::Drive` (`turn.rs:297`): add a finished-options field, filled before `drive_once` by the code holding the current `ModelHandle` (main turn start; child spawn with the child override computed the same way). Update every construction site: `turn.rs:193` and `:395`, `child_runner.rs:685`, `tests/billing.rs`, `tests/audit_telemetry.rs`. Add one `generation_options` field to `TurnParams` (copied into the root `Drive` at `turn.rs:178`), set by `fontaine-tui` `ui/turn.rs:743` and `fontaine/src/headless.rs:507` from the current `MainModel` handle and session state; the ~20 `TurnParams` construction sites, tests included, are mechanical updates.
6. `turn.rs` (`drive_once`) and `agent/setup.rs`: apply the `Drive` options on `AgentRunner::options`; `TurnParams` carries the finished options only, no handle or effort level. No build-time `builder.options`; guardian conflict check. `agent/setup.rs` reads `[guardian] effort` and builds the Guardian options at the site where `thinking_off` is passed today (`setup.rs:75`, composed guardian ~:236).
7. `fontaine-guardian/src/guardian.rs` and `structured.rs`: introduce an options field on `Guardian` (applied to its request) alongside `with_additional_params(Option<Value>)` (l.130); `structured.rs` (`Options` and both builder sites, Tool ~l.251 / Schema ~l.265) threads the field. A non-`off` level passes `with_additional_params(None)` instead of `thinking_off`; `off` or unset keeps `thinking_off` and sends no `Reasoning` options.
8. Subagents: `AgentType.effort` in `fontaine-agents/src/registry.rs` and the config-side `AgentType` (`config.rs:664`); frontmatter parsing in `fontaine-core/src/extensions.rs`; a clause rejecting frontmatter `effort` when `scope.is_tightened()`, mirroring the `model` check (`extensions.rs:323-330`); config-to-registry conversion (`agent/setup.rs:255`); resolution and application in `fontaine-core/src/child_runner.rs` (builder at ~:532-541, drive call at ~:684-685). `fontaine-agents/src/runner.rs` is only the `ChildRunner` trait and resolves nothing; resolution is `agent_type.effort.or(parent effective)` in `child_runner.rs`, and the model-callable `AgentArgs` schema does not expose effort.
9. `fontaine/src/cli.rs`: `--effort`; its mechanism is an `effort_override` parameter alongside `model_override` (`models.rs:263,489`; set at `fontaine/src/lib.rs:841,1034,1047` and `fontaine/src/auto_mode.rs:280,487`), not a CLI-scope config layer. `fontaine-tui/src/setup.rs` also threads `model_override` (lines 54,164,254,419,447,688), so `effort_override` is threaded there too.
10. `fontaine-tui`: `/effort` command (parse, completion, picker-free list of levels), status line mention in `statusbar.rs`; the `/effort` handler in `ui/mod.rs` beside `Command::Model` (~:573), and the `/model` revalidation there.
11. Docs, changelog, master-plan Decisions.

## Files to Modify
`rust/fontaine-core/src/{effort.rs (new),lib.rs,config.rs,config_scope.rs,config_catalog.rs,models.rs,gate.rs,turn.rs,child_runner.rs,extensions.rs,goal.rs,agent/setup.rs}`,
`rust/fontaine-agents/src/registry.rs`, `rust/fontaine-guardian/src/{guardian.rs,structured.rs}`, `rust/fontaine/src/{cli.rs,lib.rs,auto_mode.rs,headless.rs}`,
`rust/fontaine-core/tests/{billing.rs,audit_telemetry.rs}`, `rust/fontaine-core/tests/common/pipeline.rs`, `rust/fontaine-evals/src/none.rs`,
`rust/fontaine-tui/src/{commands.rs,statusbar.rs,setup.rs,ui/mod.rs,ui/turn.rs}`, `docs/*`, `CHANGELOG.md`,
`tasks/fontaine_harness_plan.md`.

## Trade-offs
- Rig's portable `Reasoning` over hand-written per-provider `additional_params`: rig already maps to
  `reasoning_effort`, `output_config.effort` and the rest, and keeps us off a custom provider layer.

## Decisions
- 2026-11-02: Unsupported level for a catalog-known model is dropped with a warning, not an error; reason: effort is a hint, and a session should not die on `/model` to a model with fewer levels.
- 2026-11-02: A non-`off` guardian effort on a model that refuses forced tool calls under thinking fails closed (Deny via unavailable guardian); reason: the guardian invariant allows no silent fallback.

- 2026-11-02: AFK: Options are resolved before `drive_once` by the handle-holding code and passed in a `Drive` field, recomputed each turn start; reason: `drive_once` has no `ModelHandle` and `/model` changes it.
- 2026-11-02: AFK: `TurnParams` gains one `generation_options` field, set by the two callers holding the `MainModel` handle (`ui/turn.rs`, `headless.rs`); reason: `run_turn` builds the root `Drive` from `TurnParams` and has no other seam, so this is the smallest change that works.
- 2026-11-02: AFK: `model.effort` and `agents.*.effort` stay under the existing `user_trust` rows, `guardian.effort` gets an explicit user-only row, and frontmatter `effort` is barred from repository/plugin agent files like `model`; reason: most conservative option, since a key with no row is refused from project scope anyway.

## Documentation
- `docs/configuration.md`: `[model] effort` (~l.467), `[guardian] effort` (~l.473), `[agents.types]` key (~l.753), and `--effort` in the "Command line and environment" table (~l.1079, beside the `--model` row).
- `docs/tui.md`: "Choosing a model" (~l.555) mention of `/effort`; the `statusbar.rs:116` `/model` hint string; `/effort` in the commands table, the `/model` row (~l.469), the `/config` panel table (~l.629) and the subagent-view command list (~l.889).
- `docs/configuration.md` ollama `think` row (~l.166): cross-reference to `effort`.
- `docs/index.md`: `/effort` in the command list (l.35).
- `docs/headless.md`: `--effort`.
- `docs/agents.md` / `docs/extensions.md`: `effort` frontmatter.
- `docs/guardian.md`: guardian effort and the thinking-off interplay (near l.469).
- `CHANGELOG.md`.
- `tasks/fontaine_harness_plan.md` `## Decisions`, each appended at the end of its subsection:
  - Config: `off` level added beyond Claude Code's set; `xhigh` not exposed; unset sends nothing (model default), unlike a default level; `max` semantics follow the rig/provider mapping rather than Claude Code's session-only `max`.
  - TUI: `/effort` and `--effort` copy Claude Code's surface; the level survives `/model` and is re-validated.
  - Agents: per-type `effort` inherits the session level and may differ from the parent; frontmatter `effort` is user-scope agent files only.
  - Guardian: `[guardian] effort` is user-scope only; the two Decisions above.

## Testing Strategy
Unit tests, all against deterministic models and `wiremock`, no live APIs:
- `Effort` parse/display round trip; unknown values rejected with the valid set listed.
- Config: precedence CLI > settings; `model.effort`, `agents.types.<name>.effort` and `guardian.effort` refused in project scope (schema-pinning test in `config_scope.rs`); frontmatter `effort` rejected in repository/plugin agent files; unknown key/value errors.
- `generation_options`: new assertion that `None` sends no reasoning field (no existing tests pin this);
  a `MockCompletionModel` assertion on `requests()[0].options.reasoning` for `Some(level)` and `None` (as `fontaine-core/tests/side_question.rs`); an unsupported level for a catalog spec is dropped and warned once.
- Core: two turns on an unsupported level emit exactly one warning; a `/model` switch to a model with fewer levels drops the level and emits a warning.
- Agents: frontmatter parse; child inherits parent's level unless set; a child can differ from its parent.
- Guardian: non-`off` effort on a thinking-refusing model is Deny; `off`/unset keeps `thinking_off`.
- TUI: `/effort` parse and completion; setting and displaying the session level; `/model` revalidation.
- `fontaine -p --effort high` headless test through the cassette/wiremock harness: the only wire-level proof.
