# Model Roles Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/324

## Overview

Add one user-scope table of **roles**: `[models.roles]`. Each role is a name mapped to an ordered
list of models, where every entry is an `<account>/<model>` or the name of another role. A bare
name (no `/`) resolves to its role's list everywhere a model is accepted: `--model`,
`[model] model`, `[agents.types.<name>] model`, an agent file's `model`, the `agent` tool's
`model` argument and `/model`. Fontaine itself reads four role names: `main`, `critique`, `setup`
and `compaction`. Every other name belongs to the user, so Claude Code's `sonnet`/`opus` and
dev-skills' `coding`/`thinking` port unchanged once the user defines them. Each list is a
preference order. When the serving entry fails with a quota-class error that persists past the
same-model retries, the caller falls through to the next entry and stays there. This also closes
#289 (an ordered list for the main model) and #54 (the main-model fallback chain, guardian
excluded). The usage ledger records which role and which entry served each call, so `/cost` can
break spend down by model.

## Current State

- **Main model.** `[model] model` is a single string (`ModelSection`,
  `rust/fontaine-core/src/config.rs:477`). `--model` (`rust/fontaine/src/cli.rs:59`) overrides it
  through `ResolveParams.model_override` (`rust/fontaine-core/src/models.rs:354`). `resolve_with`
  (`models.rs:628`) picks `model_override.or(config.model)` and resolves one `ModelRef`
  (`models.rs:86`) with `resolve_main_model` (`models.rs:592`).
- **Model values.** `model_ref_ok` (`config.rs:1152`) checks syntax only. Account existence is
  checked at resolve time (`account_of`, `models.rs:505`).
- **Scope.** `config_scope.rs:174` gives `model` `user_trust` scope (not the shared project
  file). `accounts` is `user_or_cli` (`config_scope.rs:170`), and `agents` is `user_trust` (:209).
- **Subagent models.** `ModelSpec { Main, GuardianDefault, Model(String) }`
  (`rust/fontaine-agents/src/registry.rs:13`) comes from `[agents.types.<name>] model` (validated
  at `config.rs:1491`, merged at `rust/fontaine-core/src/agent/setup.rs:286`) and from agent-file
  frontmatter (`model_of`, `rust/fontaine-core/src/extensions.rs:180`). Project and plugin files
  are refused any `model` (`extensions.rs:353`), and their types refuse a per-call `model`
  (`accepts_model_override`, `registry.rs:167`). Both rules are recorded in Decisions (Agents,
  2026-10-21 and 2026-10-22).
- **The `agent` tool.** Its `model` parameter description is a fixed string
  (`rust/fontaine-agents/src/spawn.rs:1001-1025`). `ChildContext::model_for`
  (`rust/fontaine-core/src/child_runner.rs:372`) resolves a spec to a handle through
  `Switcher::main(text)`.
- **Skills.** A skill's `model` is checked by `model_of` and then discarded (`extensions.rs:259`).
  A Claude Code skill with `model: sonnet` therefore stops startup.
- **Auxiliary jobs.**
  - Critique (`rust/fontaine/src/auto_mode.rs:268`) and the setup scan (CLI:
    `auto_mode.rs:489`, `rust/fontaine-core/src/project_env.rs:563`; TUI:
    `rust/fontaine-tui/src/ui/commands/auto_mode_setup.rs:161`) borrow the guardian model.
  - Compaction (`rust/fontaine-core/src/agent/goal.rs:38`, `session.rs:507/537`) borrows the
    main model.
  - `/btw`, the goal evaluator and the `llm` scanner are outside this issue's roles and keep
    their current models.
- **Retry.** `drive` (`rust/fontaine-core/src/turn.rs:395`) retries a retryable failure on the
  same model (`MAX_RETRIES = 2`) and then returns `TurnOutcome::Failed(String)`
  (`turn.rs:81`). The failure class survives only in `Record::RequestFailed`. Nothing falls
  through to another model.
- **Live switching.** `/model` calls `install_main` (`rust/fontaine-core/src/agent/models.rs`
  ~:130), which resolves the model, rebuilds the agent, updates `MainModel` and the root model in
  the tree, and moves the guardian when it is on `MainFallback`. `switch_main_model` then writes
  `[model] model` (`user_config::set_model`).
- **Spend.** `UsageLedger` (`rust/fontaine-core/src/usage_ledger.rs`) keeps one bucket per
  `UsageSource` and none per model. `/cost` (`rust/fontaine-tui/src/reports.rs:696`) prints the
  sources.

## Design

### Config

```toml
[models.roles]                      # user file or --settings only
main       = ["anthropic/claude-opus-5-5"]
setup      = ["anthropic/claude-haiku-5-5"]
sonnet     = ["anthropic/claude-sonnet-5-5", "anthropic/claude-sonnet-4-6"]
coding     = ["sonnet", "kimi/kimi-for-coding"]
thinking   = ["anthropic/claude-opus-5-5", "openai/gpt-5.5"]
```

- **New section.** `Settings` gains `models: Option<ModelsSection>`, where
  `ModelsSection { roles: Option<BTreeMap<String, Vec<String>>> }` uses `deny_unknown_fields`.
  The merged `Config` gains `roles: Roles`. Later scopes replace a whole role (per-key replace,
  as `accounts` merges).
- **Scope.** A new `config_scope` rule `Rule::user_or_cli("models", USER_FILE)`, since entries
  name endpoints exactly as `[accounts]` do.
- **Validation at merged-config load** (exit 2, `ConfigError::model`):
  - A role name matches `[A-Za-z0-9_-]+`. It cannot be `inherit` (agent-file sentinel) and cannot
    start with `$`.
  - A list is non-empty.
  - Each entry passes `model_ref_ok` or names a defined role. `$main` is accepted as a synonym
    for `main`.
  - Each `<account>/<model>` entry names a configured account.
  - No role reaches itself (a DFS over role references), and the error names the cycle path.
- **`[model] model` accepts a string or a list** (`#[serde(untagged)]` enum `OneOrMany`). It is
  shorthand for the `main` role. Setting both `[model] model` and `[models.roles] main` in the
  merged config is a config error naming both keys. `--model` is a string: an `<account>/<model>`
  or a role name, and it overrides both.
- **`[agents.types.<name>] model` also accepts a string or a list.** Each entry may be
  `<account>/<model>`, a role name, `$main` or (alone) `$guardian_default`.
- **The `[guardian] model` key and `[guardian.composition]` models stay a single
  `<account>/<model>`.** No role name, no list and no fall-through: a pinned guardian is never
  substituted.

### Resolution: `Roles` and `ModelChain`

The new module `rust/fontaine-core/src/model_roles.rs` holds both types.

```rust
/// The config's roles with each list flattened to concrete references.
pub struct Roles { flat: BTreeMap<String, Vec<ModelRef>> }

impl Roles {
    pub fn from_config(raw: &BTreeMap<String, Vec<String>>) -> Result<Roles, ConfigError>;
    pub fn get(&self, name: &str) -> Option<&[ModelRef]>;
    pub fn describe(&self) -> String; // "- `coding`: anthropic/claude-sonnet-5-5, kimi/kimi-for-coding"
}

/// One preference list being served, with the entry currently in use.
pub struct ModelChain {
    pub role: Option<String>,        // None for an inline list or a lone `<account>/<model>`
    pub entries: Vec<ModelRef>,
    active: AtomicUsize,
}

impl ModelChain {
    pub fn active(&self) -> usize;
    pub fn active_ref(&self) -> &ModelRef;
    /// The order to try after the active entry failed: the other entries from the most
    /// preferred down, each at most once.
    pub fn fall_through_order(&self) -> Vec<usize>;
    pub fn settle(&self, index: usize);
}
```

- **Flattening.** An entry naming a role splices that role's flattened list in place. Duplicate
  references keep their first position.
- **One shared chain per role.** A session-wide `RoleChains` (`Arc<Mutex<BTreeMap<String,
  Arc<ModelChain>>>>`, a lock that feeds model selection, so it uses `fontaine_tools::lock::lock`)
  holds them. Every subagent spawned on `coding` shares one `active` index, which is the issue's
  stickiness rule: later calls stay on the entry that worked.
- **Inline lists.** An inline list (a `[agents.types]` list, `[model] model` as a list) gets its
  own chain keyed `type:<name>` / `main`.
- **Text to chain.** `parse_model_text(text, &Roles) -> Result<ChainSpec, _>` turns any model
  text into a chain. It replaces the ad-hoc `ModelRef::parse` call sites that accept a reference
  from the user (`Switcher::main`, `ModelSpec::parse`).

### Where the chain comes from

| Caller | Chain | Default when unset |
|---|---|---|
| Session | `--model`, else `[model] model`, else `[models.roles] main` | exit 2 (today's "no main model") |
| Subagent | per-call `model` > type `model` > session `main` | `main` chain (today: session model) |
| Compaction | `compaction` role | the session's `main` chain, current entry |
| Critique | `critique` role | the guardian model (today) |
| Setup scan | `setup` role | the guardian model (today) |

- **Startup.** Startup resolves the chain's first entry exactly as today: a resolution failure is
  exit 2. Fall-through reacts to runtime quota failures, never to a config or credential error at
  startup, so a typo is never silently skipped.
- **Spawns.** A spawn resolves the chain's active entry. A resolve error there fails the spawn as
  today.
- **`$main` in a subagent spec.** It shares the session's `main` chain, including its active
  entry. That keeps today's behaviour of `$main` following the current session model.
- **`ModelSpec`** becomes `{ Main, GuardianDefault, Chain(ChainSpec) }`. `as_text` renders a
  role name or the reference list. Revival (`child_runner.rs:715`) keeps mapping a recorded main
  reference back to `Main`.

### Fall-through

- **Trigger.** `fn falls_through(error: &PromptError) -> bool` in `turn.rs` is true for a
  provider response with status 402, 429 or 529, and for the `claude-subscription` plan-limit
  error (`ErrorKind::Limit`, `providers/claude_subscription/mod.rs:70`). It applies only after
  `drive`'s same-model retries are spent. Fontaine's own token and cost budgets never trigger it:
  they are the user's limit, not the account's.
- **Typed outcome.** `TurnOutcome::Failed(String)` becomes `TurnOutcome::Failed(TurnFailure)`
  with `TurnFailure { message: String, falls_through: bool }` (it implements `Display`), so
  callers can act on the class.
- **Root session.** `Runtime::fall_through(&mut self) -> Option<FellThrough>`, in
  `agent/models.rs`, walks `main_chain.fall_through_order()`. For each index it tries
  `install_main` with that reference: the existing session-only path, which rebuilds the agent,
  updates `MainModel` and the tree root model, and moves a `MainFallback` guardian along. The
  first index that resolves is `settle`d. An entry that fails to resolve is skipped with a
  warning. The TUI (`rust/fontaine-tui/src/ui/turn.rs`) and headless
  (`rust/fontaine/src/headless.rs`) callers handle `Failed` with `falls_through` the same way:
  1. call `fall_through`;
  2. on `Some`, warn through `Approver::warn` (`main: anthropic/x is out of quota (429);
     continuing on kimi/y`);
  3. run one continuation turn with `RETRY_NOTE` as its prompt. The recovery hook already saved
     the failed attempt, as the in-`drive` retry relies on.
  The continuation is not a human prompt, so it is not recorded as one. The implementer reuses
  the synthetic continuation-turn path the goal loop uses, if it fits both callers. If the list
  is spent, the turn fails as today, and the message adds `every model in <role> failed`.
- **Subagent.** `run_child`'s loop (`child_runner.rs:811`) does the same on `falls_through`:
  1. resolve the next entry of the child's chain through `ChildContext`;
  2. rebuild the child's agent with the spawn-time builder (`RunShared.agent` becomes
     replaceable);
  3. update the tree's model for the child;
  4. re-drive on `RETRY_NOTE`.
- **One-shot jobs** (compaction, critique, setup scan). A helper
  `ModelChain::run_through(resolve, call)` tries the active entry's handle and, on an error whose
  provider cause `falls_through`, the remaining order. `CritiqueError`
  (`rust/fontaine-guardian/src/critique.rs:27`) and `ScanError`
  (`rust/fontaine-guardian/src/env_scan.rs:62`) gain a `fn quota_exhausted(&self) -> bool` read
  from the provider error they wrap. They keep `fontaine-guardian` free of the core types.
  - Compaction: `CompactResult::Failed` carries the same flag from the summary request.
  - Compaction keeps triggering on the main model's window and sends the summary request to the
    compaction chain's model. A model that cannot take the conversation fails the compaction as
    an overflow does today.
- **Cost-capped runs.** With an explicit `--max-budget-usd`, fall-through skips any entry whose
  model is unpriced (`ModelHandle::priced`). A capped run must stay measurable, and an unpriced
  model under an explicit cap is already a usage error at startup.
- **Durable record.** A new `Record::ModelFellThrough { agent, role, from, to, class }` is
  appended to the session log, so a resumed session and the audit trail show the switch. Resume
  starts again from each chain's first entry, as the issue allows.

### Project agent files and skills

- **Project and plugin agent files** may set `model` to a role name or `$main`. They still may
  not set an `<account>/<model>`, a list or `$guardian_default`. A role reference names no
  endpoint, because the user defines every role.
  - A role the user has not defined runs the type on the session model, with one startup warning
    naming the file and role. A repository cannot know the user's roles, and Claude Code files
    that say `sonnet` must not stop startup.
  - In a user-scope agent file, an undefined role stays an exit-2 error, as an unknown model is
    today.
  - This loosens the 2026-10-21 Agents decision. A dated Decisions line records it.
- **The per-call `model`** on a project or plugin type stays refused, as recorded on 2026-10-22.
  The model, which a repository can steer, still cannot move a repository-defined type.
- **A skill's `model`** accepts a bare role name syntactically and stays unused. Claude Code
  skills with `model: sonnet` now load. Claude Code's per-skill model override is not adopted
  here, and a Decisions line records the gap.

### Discovery

- **The `agent` tool's `model` parameter.** Its description is built from `Roles::describe()`
  when the `agent` tool is registered:
  ```
  Optional model for this agent: a role name, `$main`, `$guardian_default` or `<account>/<model>`.
  Defaults to the agent type's model. Roles:
  - `coding`: anthropic/claude-sonnet-5-5, kimi/kimi-for-coding
  - `main`: anthropic/claude-opus-5-5
  ```
  Roles are fixed for a session (config), so the description joins the cached prefix and never
  changes mid-session.
- **The `models` tool** (`fontaine-tools/src/models_tool.rs`) appends the same role lines.

### Ledger and `/cost`

- **Per-model buckets.** `UsageLedger` gains `by_model: Mutex<BTreeMap<Served, Bucket>>`, where
  `Served { role: Option<String>, model: String /* <account>/<model> */ }`.
  - `SessionState::record_usage` (`gate.rs:953`) gains a `served: &Served` argument.
  - The served value is set by `drive` (`Drive.served`), by compaction and by the guardian.
    The guardian records `role: None` with its reference.
  - The `by_source` buckets are unchanged, so headless `usage` keeps its v1 shape.
- **`/cost`.** It adds a "by model" block under the source table, one row per `Served`, printed
  as `coding · anthropic/claude-sonnet-5-5` (or the reference alone without a role), with the
  same columns.

### `/model`

- **What `/model <text>` accepts.** Any model text: a reference or a role name. The session's
  main chain becomes the parsed chain.
- **Persisting the choice.** `[model] model` is written only when the user file's main is unset
  or a single string. When main comes from `[models.roles] main` or a list, the switch is
  session-only, and the `Switched` notice says which key to edit. Overwriting it would drop the
  user's preference list.
- **The picker** keeps listing concrete models.

## Implementation Steps

1. **Config.**
   - In `config.rs`, add `ModelsSection` and the `OneOrMany` type for `[model] model` and
     `[agents.types.<name>] model`.
   - Add the merged `roles` and the main source (`MainSource::{Override, ModelKey, Role}`).
   - Validate as described in the Config section.
   - In `config_scope.rs`, add the `models` rule.
   - In `config_catalog.rs`, add the `/config` panel entry if the catalog lists every key: read
     only, edited in the file.
2. **`model_roles.rs`.** Add `Roles`, `ChainSpec`, `ModelChain`, `RoleChains`,
   `parse_model_text` and `run_through`, with unit tests.
3. **`models.rs`.**
   - `resolve_with` and `Switcher::main` take model text and build the main chain from
     `MainSource`.
   - `Models` gains `main_chain: Arc<ModelChain>`.
   - `Switcher` carries the `Roles`.
4. **Registry and spawns.**
   - `registry.rs`: `ModelSpec::Chain`.
   - `setup.rs`: build types from list or role specs.
   - `extensions.rs`: the scope rules for project, plugin and user files, the undefined-role
     warning, and skills accepting a bare name.
   - `spawn.rs`: the dynamic `model` description and parse through `parse_model_text`.
   - `child_runner.rs`: `model_for` over chains.
5. **Fall-through.**
   - `turn.rs`: `falls_through` and `TurnFailure`, then update every `TurnOutcome::Failed`
     match.
   - `agent/models.rs`: `Runtime::fall_through`.
   - The TUI `ui/turn.rs` and `headless.rs` continuation.
   - `child_runner.rs`: the child rebuild and re-drive.
   - `ledger.rs`: `Record::ModelFellThrough`.
6. **One-shot jobs.**
   - `compaction`, `critique` and `setup` chains: `agent/goal.rs` (compaction), `auto_mode.rs`
     and `project_env.rs` (critique and the CLI scan), and `auto_mode_setup.rs` (the TUI scan).
   - Add `quota_exhausted` to `CritiqueError` and `ScanError`.
7. **Ledger and `/cost`.**
   - `usage_ledger.rs`: `Served` and `by_model`.
   - Thread `Served` through `record_usage` callers.
   - `reports.rs`: the by-model block.
8. **`/model` persistence.** `agent/models.rs` and `ui/commands/model.rs`.
9. **Docs** and the master plan's Decisions lines (below).

## Files to Modify

- `rust/fontaine-core/src/config.rs`, `config_scope.rs`, `config_catalog.rs`
- `rust/fontaine-core/src/model_roles.rs` (new), `lib.rs`
- `rust/fontaine-core/src/models.rs`, `agent/models.rs`, `agent/setup.rs`, `agent/goal.rs`
- `rust/fontaine-core/src/extensions.rs`, `child_runner.rs`, `turn.rs`, `ledger.rs`,
  `usage_ledger.rs`, `gate.rs`, `session.rs`, `project_env.rs`
- `rust/fontaine-agents/src/registry.rs`, `spawn.rs`
- `rust/fontaine-tools/src/models_tool.rs`
- `rust/fontaine-guardian/src/critique.rs`, `env_scan.rs`
- `rust/fontaine/src/cli.rs` (help text), `headless.rs`, `auto_mode.rs`
- `rust/fontaine-tui/src/ui/turn.rs`, `ui/commands/model.rs`, `ui/commands/auto_mode_setup.rs`,
  `reports.rs`
- `docs/configuration.md`, `docs/agents.md`, `docs/extensions.md`, `docs/guardian.md`,
  `docs/tui.md`, `docs/headless.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Fall-through at the turn, not inside a completion-model wrapper.** A `DynModel` wrapper that
  resent each request to the next entry would be invisible to the loop. But it would send a
  request built for one provider (generation options, `thinking_off`, native search, cache
  markers) to another, and it would switch models without the rebuild boundary every model switch
  takes. Retrying at the turn reuses `install_main` and the recovery hook that already make
  `/model` and the same-model retry safe.
- **Fall-through only on quota-class failures.** Retrying any failure on another model would hide
  real request bugs (a 400 from a malformed tool schema) behind silent switches. 529 is included
  because Claude Code's fallback model triggers on overload.
- **Startup does not skip unresolvable entries.** A misspelled account would otherwise run every
  session on the second choice without anyone noticing.
- **Undefined roles in repository files fall back to the session model.** Failing would make any
  shared Claude Code agent pack unusable without per-user setup. Defining the roles implicitly
  (`sonnet` means some Anthropic model) would make Fontaine pick an endpoint, which it never does.
- **One shared chain per role.** Per-spawn chains would re-hit an exhausted account on every new
  subagent, which is the behaviour the issue rules out.

## Decisions

- Roles do not apply to the guardian, the composition models, `/btw`, the goal evaluator or the
  `llm` scanner. The guardian is never substituted (#54), and the rest are not among the issue's
  roles.
- `[model] model` and `[models.roles] main` together are a config error, not a precedence rule.
- `/model` does not persist over a role or a list.
- Resume restarts each chain at its first entry. Active indexes are not persisted.

Lines to add to `tasks/fontaine_harness_plan.md` `## Decisions`, each at the end of its
subsection, dated on the day of implementation:

- Agents: project and plugin agent files may name a role or `$main` as `model`, loosening the
  2026-10-21 "no `model`" rule toward Claude Code. A role names no endpoint, because the user
  defines every role. An undefined role runs the type on the session model, with a warning (#324).
- Agents: a skill's `model` accepts a role name and stays unused. Claude Code's per-skill model
  override is not adopted (#324).
- Config: `[models.roles]` is user file or `--settings` only. `[model] model` and
  `[models.roles] main` together are a config error. Unknown accounts, unknown roles and cycles
  are load errors (#324).
- Providers: a model list falls through to its next entry only on a 402, 429 or 529 or a
  `claude-subscription` plan limit after the same-model retries, and stays on the entry that
  worked. Claude Code's `--fallback-model` triggers on overload only. With an explicit
  `--max-budget-usd`, unpriced entries are skipped. This supersedes the 2026-11-04 "plan-limit
  failure is reported without … an alternate route" for accounts listed in a chain with a next
  entry (#324, #289, #54).

## Documentation

- `docs/configuration.md`:
  - a new `[models.roles]` section;
  - the list form of `[model] model` and `[agents.types.<name>] model`;
  - a scope-table row;
  - fall-through behaviour.
- `docs/agents.md`: role names in `model`, `$main` sharing the main chain, the project-file rule,
  and the tool description listing roles.
- `docs/extensions.md`:
  - agent-file `model` accepts a role;
  - repository files may name roles;
  - skill `model` accepts a role;
  - remove the porting note that says to rewrite `sonnet`/`opus` and say to define those roles
    instead;
  - update the `sonnet` error example.
- `docs/guardian.md`: critique and setup scan run on the `critique` / `setup` role when set; the
  guardian itself takes no role.
- `docs/tui.md`: `/model` accepting a role and its session-only case, and the `/cost` by-model
  block.
- `docs/headless.md`: `--model` accepting a role, and fall-through warnings on stderr.

## Testing Strategy

Unit tests (all reachable with constructed inputs):

- `model_roles.rs`:
  - flattening with splices and dedup;
  - cycle detection, direct and indirect, with the path in the message;
  - an undefined role reference;
  - name validation (`inherit`, `$x`, `a/b`);
  - `fall_through_order` from each active position;
  - `settle` shared across clones;
  - `run_through` stopping on the first success and on a non-quota error.
- `config.rs`:
  - string and list forms;
  - `[model] model` together with `main` rejected;
  - an unknown account in a role;
  - project-file `[models]` rejected by scope;
  - `--settings` accepted.
- `extensions.rs`:
  - a project agent file with a role: accepted, and a warning when the role is undefined;
  - a project agent file with `<account>/<model>`: rejected;
  - a user file with an undefined role: an error;
  - a skill with `model: sonnet`: loads.
- `spawn.rs`:
  - the `model` description lists the roles;
  - a role per-call `model` resolves;
  - a project type's per-call `model` is still refused.
- `turn.rs`: `falls_through` for 402, 429 and 529 and the plan-limit error, false for 400 and 500
  and for cancellation.
- `usage_ledger.rs` and `reports.rs`: by-model buckets and the `/cost` block.
- `agent/models.rs`: `/model` persistence over a role or list is skipped; a single string is
  written.

Integration tests (headless `fontaine -p`, wiremock providers):

- **Root fall-through.** The first account returns 429 on every call and the second answers. The
  run succeeds on the second entry, warns once, appends `ModelFellThrough`, and the next turn goes
  straight to the second entry with no request to the first. This is the stickiness a unit test
  cannot see across a rebuilt agent.
- **Subagent fall-through.** A child on role `coding` falls through. A second child on `coding`
  starts on the settled entry.
- **List spent.** Every entry returns 429. The turn fails with the "every model" message and the
  recovery state intact.
- **Cost cap.** A cost-capped run skips an unpriced second entry.

## Open Questions

- Whether the continuation turn after a root fall-through can reuse the goal loop's synthetic-turn
  path in both the TUI and headless, or needs a small shared helper. The implementer settles this
  from the code; it does not change the behaviour above.
