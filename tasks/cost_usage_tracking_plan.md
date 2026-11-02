# Cost and Token Usage Tracking (`/cost`, `/usage`) Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/56

## Overview

Fontaine counts spend in several unrelated places (the agent tree, `GuardianUsage` for the
guardian, the goal evaluator and subagents, and a private `Totals` in `headless.rs`), but the
terminal UI has no way to show it: there is no `/cost` or `/usage`. Add one session-wide usage
ledger that every model call is recorded into, tagged by source (main model, guardian, subagents,
goal evaluator, compaction, side questions), and a `/cost` command (alias `/usage`) that prints a
breakdown of tokens, cache tokens and dollars per source, including the guardian's spend, and marks
a dollar figure as partial when some calls had no price.

## Current State

- `Spend` (`fontaine-agents/src/budget.rs`) holds input/output tokens, `cost_usd: Option<f64>` and
  `unpriced_tokens`. `AgentTree::debit` (`fontaine-agents/src/tree.rs:819`) adds a call to the
  agent's node and to the session total. It carries no cache tokens and no source.
- The root agent's node mixes everything debited to `ROOT_AGENT`: main-model turns, the guardian
  calls made for it (`SessionState::add_guardian_usage`, `gate.rs:655`), goal evaluations
  (`add_goal_usage`, `gate.rs:679`), compaction (`agent/goal.rs:50`) and `/btw`
  (`agent/goal.rs:72`). `AgentRuntime::tree_spend` (`agent/goal.rs:24`) is that sum plus
  subagents.
- `GuardianUsage` (`gate.rs:209`) is the only type with cache tokens. `SessionState` keeps three
  instances (guardian, goal, subagent) plus a per-agent guardian map.
- The main model's usage is only totalled in headless mode: `Totals` in `fontaine/src/headless.rs`
  (line ~202), fed from `SessionEvent::Usage`. The TUI never totals it.
- `turn.rs:555-563` is the single place a turn's `CompletionCall` usage enters: it clears
  `usage.cost` for unpriced accounts, debits the tree, and tallies subagent calls.
- Headless `result` already reports `usage.{main,guardian,subagents,goal}` and `total_cost_usd`
  (`docs/schemas/headless-v1.schema.json`, `headless.rs:300-340`). Compaction and `/btw` spend is
  in `total` tree spend but in none of those buckets.
- `docs/telemetry.md` already says "the session's `/cost` and budgets do not count the estimate"
  for dropped guardian requests; the command it names does not exist.
- Slash commands are `Builtin` entries in `fontaine-tui/src/commands.rs` (aliases supported);
  report text is built in `fontaine-tui/src/reports.rs` and posted by handlers in
  `fontaine-tui/src/ui/commands/reports.rs` (`recap`, `permissions`).

## Design

### One ledger, one recording path

New module `fontaine-core/src/usage_ledger.rs`:

```
enum UsageSource { Main, Subagent, Guardian, Goal, Compaction, SideQuestion }

struct Bucket {            // replaces GuardianUsage (same fields, plus the two below)
    calls: u64,
    input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens: u64,
    cost_usd: Option<f64>,
    unpriced_tokens: u64,  // tokens of calls with no price: cost_usd is a lower bound when > 0
}

struct UsageLedger { by_source: Mutex<BTreeMap<UsageSource, Bucket>> }
```

`SessionState::record_usage(source, agent, &Usage)` is the single entry point: it adds to the
ledger bucket and debits the agent tree (`spend_of`). The existing `add_guardian_usage`,
`add_goal_usage` and `add_subagent_usage` become thin wrappers (or are replaced), and the four
direct `debit` call sites (`turn.rs:560`, `agent/goal.rs:50,72`, and the goal/guardian methods) go
through it. This keeps the budget semantics identical (every call is still debited to the same
agent) and makes "forgot to record a source" structurally harder.

- Main-model turns: `turn.rs` records `Main` when `agent_id == ROOT_AGENT`, else `Subagent`.
- `GuardianUsage` is renamed/replaced by `Bucket`; the per-agent guardian map keeps its own
  `Bucket` values. Headless keeps the same JSON keys (`cache_*`, `cost_usd`), so
  `headless-v1.schema.json` does not change.
- Headless `Totals`/`Others` read from the ledger (`Main`, `Guardian`, `Subagent`, `Goal`) instead
  of keeping a private main tally. `model_calls` becomes `Main.calls`. Output is unchanged.
- Pricing follows the existing rule: `turn.rs` already clears `cost` for unpriced accounts before
  recording; the ledger treats `cost.is_none()` as unpriced.
- Not counted, as today: the estimate for a guardian request dropped by a composition (no usage
  came back), and the screen/scanner calls whose usage is not reported through
  `llm_usage`/`adjudication.usage`. The report footnotes the estimate exclusion.

### `/cost` command (alias `/usage`)

`Builtin { name: "cost", aliases: ["usage"], description: "show this session's token usage and
cost, by source" }`, `main_only() == false` (read-only, runs during a turn like `/recap`). Handler
`cost(&mut self, runtime)` in `ui/commands/reports.rs` snapshots the ledger and posts
`reports::cost(&snapshot)` as a transcript notice. No arguments.

Output (plain text, one row per non-empty source, a total row, tokens via the existing compact
count formatter):

```
Session usage
  source        calls   input   output  cache read  cache write  cost
  main             12   84.2k    9.1k       61.0k       3.2k      $0.4120
  guardian         31   22.0k    1.4k        9.8k          0      $0.0150
  subagents         8   40.1k    5.0k          0           0      $0.1900 + unpriced
  total            51  146.3k   15.5k       70.8k       3.2k      $0.6170 + unpriced
```

- A source with no priced call prints `n/a` for cost; the total prints a dollar figure when any
  bucket is priced and appends ` + unpriced` when any bucket has `unpriced_tokens > 0` (same
  wording as the agents panel).
- Rows for compaction and side questions are shown only when non-zero.
- A footer line shows the session cost budget when `[agents]` sets one (`$spent / $cap`), using
  the existing session limit accessors on `AgentTree`.
- The cache columns are omitted entirely when every cache counter is zero, so a provider that
  reports none does not show a column of zeros.
- Row/column formatting lives in `reports.rs` next to `context`, as a pure function of a plain
  snapshot struct so it is unit-testable without a runtime.

### Out of scope

- Cross-session history, `/stats`, plan-limit/allowance display (`usage.opencode.allowance`) and
  user-supplied prices for gateway accounts: separate features. The gateway's `list_cost` is not
  used.
- Persisting the ledger across `--resume`: usage is in-memory today (guardian, goal, subagent
  tallies and the agent tree all reset); `/cost` reports the current process's session, and says
  so in the docs.

## Implementation Steps

1. `fontaine-core/src/usage_ledger.rs`: `UsageSource`, `Bucket` (with `add(&Usage)`), `UsageLedger`
   (`record`, `snapshot`, `total`), unit tests. Export from `lib.rs`.
2. `fontaine-core/src/gate.rs`: replace `GuardianUsage` and the three standalone mutexes with the
   ledger; add `record_usage`; keep `guardian_usage_of(agent)` on a per-agent `Bucket` map; update
   `add_*_usage` wrappers.
3. `fontaine-core/src/turn.rs` and `agent/goal.rs`: route the four debit sites through
   `record_usage` with the right `UsageSource`.
4. `fontaine/src/headless.rs`: read main/guardian/goal/subagent buckets from the ledger; delete the
   private `Totals` tally. Existing headless contract tests must pass unchanged.
5. `fontaine-tui/src/reports.rs`: `UsageReport` snapshot struct and `cost()` formatter.
6. `fontaine-tui/src/commands.rs` + `ui/mod.rs` + `ui/commands/reports.rs`: `Command::Cost`, the
   `cost`/`usage` builtin, dispatch, handler; include in `main_only` exemptions like `Recap`.
7. Docs and tracking: `docs/tui.md` command table and a short "Usage and cost" section;
   `docs/telemetry.md` already references `/cost` (verify wording still true); master plan
   `## Decisions` line (below); `tasks/completed/core_slash_commands_plan.md` is archived and left
   alone. CHANGELOG entry is added by the PR step.

## Files to Modify

- `rust/fontaine-core/src/usage_ledger.rs` (new), `lib.rs`, `gate.rs`, `turn.rs`, `agent/goal.rs`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine-tui/src/reports.rs`, `commands.rs`, `ui/mod.rs`, `ui/commands/reports.rs`
- `docs/tui.md`, `docs/telemetry.md` (if needed), `tasks/fontaine_harness_plan.md` (Decisions)
- Tests that construct `GuardianUsage` (`fontaine-core/tests/{billing,goal,agent_pipeline,...}.rs`)

## Trade-offs

- One ledger keyed by source over reading the agent tree: the root node mixes main, guardian, goal,
  compaction and `/btw` spend, so the breakdown cannot be derived from it, and the tree has no
  cache counters.
- Recording through one `SessionState` method over adding a fifth parallel tally: the current
  pattern (a mutex per source plus a separate `debit`) is how compaction and `/btw` ended up in no
  bucket.
- `/usage` as an alias of `/cost` rather than a separate screen: Fontaine has no plan limits to
  show, so two screens would be the same report.
- Rebuilding headless on the ledger rather than leaving its `Totals`: avoids counting main usage
  in two places that can drift; the JSON contract is pinned by existing tests.

## Decisions

- 2026-11-02: `/usage` is an alias of `/cost`, not a separate report. Fontaine has no subscription
  plan limits to show.
- 2026-11-02: the ledger is in-memory and not restored on `--resume`, matching the guardian, goal
  and subagent tallies and the agent tree today.

## Documentation

- `docs/tui.md`: add `/cost [alias /usage]` to the command table and a "Usage and cost" section
  with the sample output, the `+ unpriced` meaning, and the not-counted cases (dropped guardian
  estimate, resume).
- `docs/telemetry.md`: confirm the `/cost` sentence now points at a real command.
- Master plan `## Decisions`: a dated line under the subject for slash commands/observability
  that `/usage` aliases `/cost` and the ledger is in-memory.

## Testing Strategy

- `usage_ledger.rs` unit tests: `Bucket::add` sums tokens and cache tokens, accrues cost only from
  priced calls, counts `unpriced_tokens` for a call without a cost, counts `calls`; `total` sums
  buckets and propagates "partial".
- `gate.rs` tests: `record_usage` for each source lands in the right bucket and still debits the
  right agent (a subagent call raises that agent's `Spend`, a guardian call raises the caller's);
  compaction and `/btw` appear in their own buckets (the bug this fixes: they were in no bucket).
- `reports.rs` unit tests for `cost()`: all-priced, mixed priced/unpriced shows `+ unpriced`,
  no priced source shows `n/a` and no dollar total, cache columns hidden when all zero,
  empty sources omitted, budget footer present only with a configured cap.
- `commands.rs` parse tests: `/cost`, `/usage`, with trailing text ignored, `main_only()` false.
- TUI integration test (existing routed/wiremock harness in `fontaine-tui/tests`): run a turn that
  triggers a guardian call, then `/cost`, and assert the transcript notice has `main` and
  `guardian` rows. Headless contract tests (`fontaine/tests/headless_contract.rs`) must pass
  unchanged, which pins the refactor.
- No live-API tests.

## Open Questions

- Should headless `result` also expose compaction and side-question buckets (schema bump)? This
  plan leaves the v1 schema unchanged; their spend stays in the tree total only.
