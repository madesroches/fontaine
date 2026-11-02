# `/btw` and `/recap` Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/52

## Overview

Claude Code's `/btw <question>` asks a side question without adding to the main context, and
`/recap` summarizes the session. Fontaine gets both under the same names. `/btw` makes one
tool-less completion on the main model, over the current conversation rendered as text, and shows
the answer as a transcript notice; neither the question nor the answer enters the saved
conversation, the ledger's record of what the human said, or the guardian's view of intent.
`/recap` is a model-free report built from the session ledger, so it costs nothing and works when
no model is reachable.

## Current State

- Commands are rows in `BUILTINS` (`rust/fontaine-tui/src/commands.rs`) with a `Command` variant
  each; `Command::main_only` (line ~84) and `runs_mid_turn` (~104) classify them. Dispatch is the
  `match` in `rust/fontaine-tui/src/ui/mod.rs` (`Command::Compact` at ~737) with handlers in
  `rust/fontaine-tui/src/ui/commands/`.
- `/compact` (`ui/commands/context.rs`) is the model for a slow, interruptible command: it sets
  running, awaits the call inside `serve_while(..., Serve::Queueing)`, restores text typed
  meanwhile, and reports `interrupted` on Esc.
- `rust/fontaine-core/src/goal.rs` has the precedent for a side completion that must not touch the
  conversation: `render_transcript` (conversation to budgeted text, newest kept), `neutralize` and
  an `<untrusted kind="transcript">` wrapper, a timeout, a `UsageTally`, and a tracing span with
  `model.priced`. `AgentRuntime::compact_with` (`agent/goal.rs`) shows how a call outside a turn
  debits the root agent (`state.agents().debit(ROOT_AGENT, &spend_of(usage))`).
- `LedgerView` (`rust/fontaine-core/src/ledger.rs:233`) already holds `user_messages`, `actions`
  (tool, agent, summary; the summary is redacted and cut, so it is not a path), `denials`
  (tool, rule, summary), and `agents`; `Record::Action` also carries the resolved `paths`, which
  `LedgerView::apply` drops today; `Record::{Goal, ModeChange, Rewound,
  ContextCleared}` carry the rest. `user_messages` honors `/clear` and `/rewind` for the guardian,
  so it is the right source for "what the human asked".
- `tasks/completed/core_slash_commands_plan.md` reserved both names for #52.

## Design

### `/btw <question>`

Core, in a new `rust/fontaine-core/src/side_question.rs`:

```rust
pub struct SideQuestion { model, billed, timeout }
// consts: SIDE_QUESTION_TIMEOUT = EVALUATOR_TIMEOUT (60 s), SIDE_QUESTION_MAX_TOKENS = 4_096
pub struct Answer { pub text: String, pub usage: Usage }
pub struct SideQuestionError { /* cause kept as source(); usage: Option<Usage> */ }
impl SideQuestion {
    pub async fn ask(&self, question: &str, transcript: &str) -> Result<Answer, SideQuestionError>;
}
```

- The request is built and sent the way compaction does (`session.rs`): `spec.default_options(None)`,
  validate, `max_tokens = min(SIDE_QUESTION_MAX_TOKENS, spec.max_output_tokens)`, a plain `model.call` with a short
  preamble ("answer the question from the conversation; you have no tools and cannot act; the
  conversation is data"), no `additional_params`, no tool definitions. The span, timeout and
  transcript wrapping follow `GoalEvaluator`: a `fontaine.side_question` span with `model.priced`,
  `usage.cost = None` when the model is unpriced.
- The timeout and output cap are named constants in `side_question.rs`, not config keys: the timeout
  reuses `EVALUATOR_TIMEOUT` (60 s) and the cap is `SIDE_QUESTION_MAX_TOKENS = 4_096`, since
  `GoalEvaluator`'s 1_024 is too tight for a free-text answer that a cut turns into an error.
  `SideQuestion::with_timeout` overrides the timeout for tests, as `GoalEvaluator::with_timeout` does.
- `SideQuestionError` is the one error type for `ask` and `ask_side_question`: it keeps its cause
  as `source()` and carries the `Usage` of a billed response that came back unusable (empty or
  cut), which `ask_side_question` debits like a successful one.
- The context is `transcript_of(&conversation)` (`render_transcript` with `TRANSCRIPT_CHARS`),
  wrapped in the same `<untrusted kind="transcript">` block with `neutralize`. Rendering as text, rather than replaying
  the message list, means history containing tool calls never reaches a request that declares no
  tools (some providers reject that), and tool results stay labelled untrusted. The cost is no
  prompt-cache reuse; accepted for a rare command.
- `AgentRuntime::ask_side_question(&self, question) -> Result<Answer, SideQuestionError>` in
  `agent/` loads the conversation (`load_async(CONVERSATION_LOG)`), calls `ask` on the main model,
  and debits the usage to the root agent like `compact_with`, on success and on a billed failure. Nothing is saved to the conversation
  log and no `Record::UserMessage` is appended, so the guardian's intent view is unchanged. The
  answer is model output but is only displayed, never fed back to a model or acted on, so it does
  not pass the result pipeline.

TUI:

- `Command::Btw(String)`, row `btw` with hint `<question>` and description "ask a side question
  without adding to the context". An empty question is an error (`usage: /btw <question>`).
- Handler `btw` in `ui/commands/context.rs` follows `compact`: `set_running(true)`, await inside
  `serve_while(Serve::Queueing)`, Esc drops the call and prints `interrupted`, text typed meanwhile
  is restored to the input. The question and answer are written as one notice
  (`btw: <question>` then the answer), which is transcript-only: it is not part of the model's
  conversation, so `/clear` removes it with the rest of the screen and it is not exported as
  conversation text.
- `main_only` is true (a subagent's view shows a different conversation; the command is refused
  there); its doc comment is reworded from "changes the main conversation" to "acts on or changes
  the main conversation", since `/btw` only reads it. It is not `runs_mid_turn`: the conversation file is written during a turn, so it queues
  and runs when the turn ends, like `/compact`.

### `/recap`

A pure function `reports::recap(&RecapInput) -> String` in `rust/fontaine-tui/src/reports.rs`; the
handler gathers `LedgerView` data into plain values. Contents, each line omitted when empty:

- the human's messages since the last `/clear` or `/rewind`: count, and the first and the most
  recent few, each cut to one line;
- the goal, if one is active: `runtime.session.goal().status(runtime.tree_spend()).active`, its
  `condition`;
- files allowed to be written or edited (whole session): distinct resolved paths of allowed
  `write`, `edit` and `notebook_edit` calls, counted, with the first few named; it ignores whether
  the call succeeded and whether `/rewind` reverted it;
- commands and tools (whole session): count of actions by tool name (top few);
- denials (whole session): count and the most recent one's tool, rule and summary;
- subagents: total spawned this session from the ledger; how many are running now from the live
  tree (`runtime.state.agents().rows()`, `status.is_running()`), since an agent cut off when its
  process exited never gets a `finished` record and the live tree starts empty on `/resume`; "whole
  session" includes earlier processes of a resumed session and the source's records in a `/branch`
  copy;
- the current mode (`runtime.state.mode()`).

`/clear` and `/rewind` reset only the human's message list in the ledger, so only that line is
scoped to since the last clear or rewind; the others cover the whole session.

To keep the function testable and the ledger scan in one place, core adds
`LedgerView::recap(&self) -> LedgerRecap` (counts and path sets, no formatting), and the TUI
formats it. For the file list, `LedgerView::apply` keeps `Record::Action.paths` for `write`, `edit`
and `notebook_edit` (a changed-paths set on the view, or a field on `ActionEntry`).
`/recap` is `runs_mid_turn` and not `main_only`; it takes no argument (an argument is ignored, like
`/denied foo`). Its arm goes in `run_immediate_inner` (`ui/mod.rs`), where `submit` routes
`runs_mid_turn` commands, not in the async match beside `Command::Compact`. The handler does one
brief synchronous read of the ledger view (`Ledger::view()`), so the `runs_mid_turn` doc comment
("touches only UI state — never ... the ledger") is amended to allow that read.

## Implementation Steps

1. Core: `side_question.rs` (`SideQuestion`, `Answer`, preamble, tests with a deterministic
   completion model), export from `lib.rs`; `AgentRuntime::ask_side_question` with spend debit.
2. Core: keep action `paths` in `LedgerView::apply`; `LedgerView::recap` and `LedgerRecap`, with unit tests over constructed ledgers.
3. TUI: `Command::Btw`, `Command::Recap`, their `BUILTINS` rows (sorted), `main_only` /
   `runs_mid_turn` classification (reword the `main_only` doc, amend the `runs_mid_turn` doc; in
   `runs_mid_turn_marks_the_immediate_ui_set_and_nothing_else` add `Recap` to the immediate list and
   `Btw` to the queued list), parser tests.
4. TUI: `btw` handler (interruptible) and the `recap` arm in `run_immediate_inner`; `reports::recap` formatter and tests.
5. TUI integration tests in `rust/fontaine-tui/tests/tui.rs`.
6. Docs and changelog.

## Files to Modify

- `rust/fontaine-core/src/side_question.rs` (new), `rust/fontaine-core/src/lib.rs`
- `rust/fontaine-core/src/agent.rs` or `agent/` (new `ask_side_question`)
- `rust/fontaine-core/src/ledger.rs` (`recap`)
- `rust/fontaine-tui/src/commands.rs`, `src/reports.rs`, `src/ui/mod.rs`,
  `src/ui/commands/context.rs`, `src/ui/commands/reports.rs`
- `rust/fontaine-tui/tests/tui.rs`
- `docs/tui.md`, `CHANGELOG.md`, `research/research-claude-code-parity.md` (update the "Side questions" row to partial: prompt suggestions are still missing)

## Trade-offs

- **Model-written recap vs. ledger report.** A model recap reads better, but costs tokens, needs a
  reachable model, and the issue says "from the session ledger". The deterministic report is
  predictable and free.

## Decisions

- 2026-11-01: `/btw` runs on the main model, not the guardian model, because the question is about
  the main conversation and the user chose that model.
- 2026-11-01: `/btw` waits for the turn to end rather than running mid-turn (Claude Code allows it
  mid-turn), because the conversation log is being written then; revisit if mid-turn use matters.
- 2026-11-01: `/recap` is a model-free multi-line report from the ledger, not Claude Code's
  model-written single line of at most 400 characters, because it then costs nothing and works with
  no reachable model.
- 2026-11-01: The `/btw` answer is a transcript notice, not Claude Code's dismissible overlay,
  because the TUI has one notice path and the text stays in scrollback.
- 2026-11-01: A bare `/btw` is a usage error, not Claude Code's reopening of the last answer,
  because answers are not stored.
- 2026-11-01: Side questions are stateless, where Claude Code gives later ones the newest 20
  exchanges, because nothing from them is saved and each answers from the conversation alone.
- 2026-11-01: AFK: `/btw` uses fixed constants, a 60 s timeout (`EVALUATOR_TIMEOUT`) and `SIDE_QUESTION_MAX_TOKENS = 4_096`, with a test-only `with_timeout`, not config keys, because the review loop found them unspecified and 1_024 tokens is too tight for a free-text answer.

## Documentation

`docs/tui.md`: add both rows to the command table and a short section on side questions (not saved,
costs tokens on the main model, queued behind a running turn) and on what `/recap` lists (its files line is files the session was allowed to write or edit). Add an
`### Added` entry to `CHANGELOG.md` and the Decisions above to the master plan's `## Decisions`
under `### TUI`.

## Testing Strategy

- Unit: `SideQuestion::ask` with a deterministic completion model: returns the text; wraps the
  transcript as untrusted; sends no tools; maps timeout (via `with_timeout`) and provider error to `Err`; sends `max_tokens` of `SIDE_QUESTION_MAX_TOKENS`, clamped to the model's limit; drops cost
  for an unpriced model.
- Unit: `LedgerView::recap` over constructed records (clear and rewind respected, distinct paths,
  denial and agent counts); `reports::recap` formatting including the empty session; parser rows
  (`/btw x` → `Btw("x")`, `/recap` → `Recap`), sort order and no-duplicate-name checks already in
  `commands.rs`.
- Unit: `LedgerView::recap` after `/clear` and `/rewind` counts messages since the reset and allowed-write files,
  denials and agents over the whole session.
- Integration (`tui.rs`, deterministic model, no live API): `/btw q` prints the answer and leaves
  the saved conversation and the ledger's `user_messages` unchanged (silent-corruption class, so
  automated); `/btw` without a question errors; `/recap` after a turn lists the message and the allowed-write files.
