# AskUserQuestion Tool Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/23

## Overview

Claude Code's `AskUserQuestion` lets the model stop and ask the human 1–4 multiple-choice
clarifying questions mid-task. No permission mode answers it for the human. Fontaine gets the
same capability as an `ask_user_question` tool: the TUI shows a question dialog, headless runs
(where nobody can answer) return an "unavailable, proceed with your best judgment" result, and
the human's own answers are recorded as user messages so the guardian sees them as intent.

## Current State

- Built-in tools are listed in `fontaine-tools/src/names.rs:2` (`BUILTIN_TOOL_NAMES`) and carry a
  risk class in `fontaine-tools/src/risk.rs` (`EXTENSION_RISK` holds the tools assembled by core:
  `agent`, `skill`, `send_message`).
- Core assembles those tools in `fontaine-core/src/tools.rs:63` (`ExtensionTools`) and wraps each
  with `register_extension`; `fontaine-tools/src/skill.rs` is the model for a tool whose
  behaviour lives behind a small trait (`SkillSource`) implemented in core.
- The gate (`fontaine-core/src/permissions.rs:647`) gives a read-only call the fast path when its
  paths are in scope, with a single carve-out for `skill`, which names no path. The deterministic
  floor (deny/ask rules, hooks) runs before it.
- The human reaches the pipeline through `Approver` (`permissions.rs:75`). The TUI's
  `ChannelApprover` (`fontaine-tui/src/lib.rs:92`) sends `Prompt` variants to the UI loop, which
  hosts them as modal panels (`app.rs` `begin_approval` / `begin_unavailable`). Headless uses
  `DenyAll` (`permissions.rs:122`).
- Guardian intent is built from `Record::UserMessage` entries in the ledger
  (`ledger.rs:213`, `adjudication.rs:109`); steering already appends mid-turn human text this way
  (`steering.rs:78`).
- Built-in subagent types list their tools explicitly (`fontaine-agents/src/registry.rs:153`), so
  a new tool is not given to children by default.
- `docs/tools.md` lists the tools and risk classes; `research/research-claude-code-parity.md:325`
  tracks this feature as missing.

## Design

### Tool

`fontaine-tools/src/ask_user.rs`, modelled on `skill.rs`:

```rust
pub struct AskUserArgs { pub questions: Vec<Question> }          // 1..=4
pub struct Question { pub question: String, pub header: String,   // header <= 12 chars
                      pub options: Vec<QuestionOption>,           // 2..=4
                      pub multi_select: bool }
pub struct QuestionOption { pub label: String, pub description: String }

pub trait QuestionSource: Send + Sync {
    fn ask<'a>(&'a self, agent: &'a str, questions: Vec<Question>)
        -> BoxFuture<'a, Reply>;
}
pub enum Reply { Answered(Vec<Answer>), Declined, Unavailable }
pub struct Answer { pub question: String, pub selected: Vec<String>, pub other: Option<String> }
```

The tool validates the shape (counts, non-empty labels, unique option labels per question;
`deny_unknown_fields`) and returns `invalid_args` errors the model can correct. "Other"
free-text is always available in the dialog and is not listed among the options, as in Claude
Code. A result is text: one `"<question>" = "<answer>"` line per question; `Unavailable`
returns `User input is unavailable in this session; proceed with your best judgment and state
your assumptions.`

The tool is **root-agent only**: if the calling `AgentId` is not `ROOT_AGENT` it returns the
same unavailable result without asking. Built-in child toolsets do not include it. Loaders accept
any built-in name, so user-defined agent types may name it, and a type naming no tools inherits
every parent tool (`AgentType::toolset`, `registry.rs`); the name is accepted and refused at call
time, as for `agent`, `skill` and `send_message`, so a background subagent can never put a
dialog in front of the human.

### Risk class and gate

`ask_user_question` is `ReadOnly` in `EXTENSION_RISK`, added to `BUILTIN_TOOL_NAMES`
(and its permission-rule name mapping beside `extensions.rs:115` as `AskUserQuestion`; the
rule takes no specifier, with an arm like `send_message` in `permission_rules.rs:164` and :588). The
fast-path carve-out in `permissions.rs:647` becomes "tools that name no path" (`skill`,
`ask_user_question`) via one small helper rather than a second `||` clause. Because the call can
only show the human text and collect their choice, it needs no guardian verdict; the floor still
precedes it, so a deny or ask rule or a `pre_tool_use` hook can block it. No mode, including
`auto` and `yolo`-style modes, ever answers the question: the answer comes only from
`QuestionSource`.

### Core wiring

`Approver` gains a defaulted method:

```rust
fn ask_user<'a>(&'a self, agent: &'a str, questions: Vec<Question>) -> BoxFuture<'a, Reply> {
    Box::pin(async { Reply::Unavailable })
}
```

so `DenyAll`, test approvers and headless are unavailable by default with no change. Core's
`QuestionSource` implementation (next to the `SkillRuntime` wiring in `agent.rs`) calls
`approver.ask_user`, and on `Answered` appends one `Record::UserMessage` containing only
human-supplied content: the selected option labels and typed "Other" text. The model-authored
question text and option descriptions are not appended, so model text never becomes guardian
intent. Esc yields `Reply::Declined`, rendered as "The user declined to answer; proceed with your
best judgment or ask in plain text", and appends nothing; `Reply::Unavailable` renders the
unavailable text above.

### TUI

`fontaine-tui/src/question.rs` (new), a panel in the style of `approval.rs`/`server_prompt.rs`:
header chip, question, numbered options with descriptions, an "Other" row that opens an
`InputBuffer`, Up/Down/number keys to move, Space to toggle for `multi_select`, Enter to confirm
and advance to the next question, Esc to cancel the whole call. Keys typed ahead during the turn
must not answer it: import `ANSWER_GRACE` from `server_prompt.rs` (`pub(crate)`; `approval.rs` and
`denials.rs` keep private copies). `ChannelApprover` gets a `Prompt::Question` variant mirrored
on `Prompt::Ask` (oneshot reply, dropped sender = unavailable), with `on_prompt` (~`lib.rs:277`)
and `drain_prompts` (~:290) gaining a Question arm. `App` keeps one slot per panel (`app.rs:87-105`)
and `prune_closed` / `pending_for` handle only approvals, so a question gets its own slot, queue
behind an open approval, and closed-reply pruning. `view.rs:177-221` picks overlays by title
(`is_approval` flag), so it gains a branch for the question title. Screen-reader mode renders
linearly in `linear.rs` (`accessibility.rs` only resolves the screen-reader flag): `NEEDS_ANSWER`
(`linear.rs:24`) must gain the new overlay title.

### Headless

Nothing to add beyond the default: `fontaine -p` returns the unavailable result. The base
system prompt is static and `PromptParts` has no surface field, so the guidance line is the same
on all surfaces and headless relies on the unavailable result.

### Security

The call is not a path to any side effect, the guardian is unchanged, the floor still precedes
it, the answer is never model- or mode-supplied, and children cannot reach the human. The one new
trust edge is human answer text becoming guardian intent; `docs/guardian.md` counts only
human-typed messages as intent, and `adjudication.rs:109-123` builds `task_intent` /
`recent_user_messages` verbatim from `UserMessage` records. Recording the question text would
launder model-authored text into intent, so only selected labels and typed "Other" text are
appended. A selected label is still chosen from model-written options; the human's selection, not
the label's wording, is what counts as intent.

## Implementation Steps

1. `fontaine-tools/src/ask_user.rs`: types, validation, `QuestionSource`, tool; register in
   `lib.rs`, `names.rs` (`BUILTIN_TOOL_NAMES` `[&str; 11]` → 12), `risk.rs` (`EXTENSION_RISK`
   size 3 → 4).
2. `fontaine-core/src/permissions.rs`: `Approver::ask_user` default; fast-path helper for
   pathless read-only tools; `permission_rules.rs` specifier validation (:157-165) and rule-spec
   derivation (:586-588) get a no-specifier arm like `send_message`; `destructive.rs:62` lists
   the tool beside `skill` as non-destructive (else `_ => true`).
3. `fontaine-core/src/tools.rs`, `agent.rs`, `extensions.rs`: add to `ExtensionTools`, build the
   `QuestionSource` over the approver and ledger, rule-name mapping beside `extensions.rs:115`,
   and add `AskUserQuestion` to the known-Claude-tool-names error string (`extensions.rs:133`).
4. `fontaine-tui`: `question.rs` panel, `Prompt::Question`, `App` hosting/queueing/pruning, view
   (overlay-title branch), `lib.rs` `on_prompt` / `drain_prompts` arms, key routing, `linear.rs`
   (`NEEDS_ANSWER`).
5. One system prompt guidance line for the tool, on all surfaces (`fontaine-core/src/prompt.rs`).
6. Docs: the files listed under Documentation; flip the
   parity row in `research/research-claude-code-parity.md`; `CHANGELOG.md`.
7. Add dated `## Decisions` lines to `tasks/fontaine_harness_plan.md` for the deviations
   named in this plan's Decisions (root-only, pathless fast-path carve-out, Esc declined
   result, answers-as-intent rule).

## Files to Modify

- `rust/fontaine-tools/src/{ask_user.rs (new), lib.rs, names.rs, risk.rs}`
- `rust/fontaine-core/src/{permissions.rs, permission_rules.rs, destructive.rs, tools.rs, agent.rs, extensions.rs, prompt.rs}`
- `rust/fontaine-tui/src/{question.rs (new), lib.rs, app.rs, view.rs, linear.rs}`
- `rust/fontaine-core/tests/{choke_point.rs (ExtensionTools literal at :71), permission_rules.rs (tool-name lists at :71, :84)}`,
  `rust/fontaine-tools/tests/choke_point.rs`, `rust/fontaine/tests/headless_smoke.rs` (hard-coded
  tool names)
- `docs/{tools,tui,headless,permissions,configuration,guardian,agents,architecture,extensions}.md`, `CHANGELOG.md`,
  `research/research-claude-code-parity.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Reuse `Approver` vs a separate channel.** A defaulted method on `Approver` keeps every
  existing approver (headless, tests) correct and reuses the TUI's single prompt channel and
  queueing; a separate trait would need its own plumbing through every surface.
- **Read-only fast path vs prompting for the call itself.** Asking the human whether the model
  may ask them a question would be absurd; the dialog is the human interaction.
- **Root only.** Claude Code does not offer the tool to subagents; background children
  interrupting the human is also hard to attribute.

## Decisions

- Tool name is `ask_user_question` (snake_case like the other tools); the permission-rule name
  is `AskUserQuestion`, matching the existing mapping convention.
- Esc cancels the whole call and returns a distinct "declined" result (`Reply::Declined`) rather than an error.
- 2026-10-25: `ask_user_question` is root-agent only, refused at call time for children (including custom types that inherit it), because background subagents must not put a dialog in front of the human (#23).
- 2026-10-25: `ask_user_question` is `ReadOnly` and takes the fast path through a pathless-tool carve-out (with `skill`), because it only shows the human text and collects a choice (#23).
- 2026-10-25: Esc gives a "declined" result rather than an error or the unavailable text, so the model can tell a refusal from a headless session (#23).
- 2026-10-25: Only human-supplied content (selected labels, typed "Other" text) is appended as a `UserMessage`, never question text or option descriptions, because model text must not become guardian intent (#23).

## Documentation

`docs/tools.md` (table row, fast-path note; ~:33), `docs/tui.md` (dialog keys),
`docs/headless.md` (unavailable result), `docs/permissions.md` (never auto-answered; answers
become intent; ~:60 and ~:108-115), `docs/configuration.md` (~:457-458), `docs/guardian.md`
(~:56-60 intent rule, ~:130), `docs/agents.md` (~:38-39, ~:93), `docs/architecture.md` (~:108),
`docs/extensions.md` (~:65), `CHANGELOG.md`.

## Testing Strategy

- Unit (`ask_user.rs`): argument validation (0 or 5 questions, 1 or 5 options, duplicate labels,
  unknown field, over-long header), result rendering for single/multi-select/other, root-only
  refusal, unavailable and declined texts.
- Unit (`question.rs`): key handling — navigation, toggle, Other entry, advance, Esc cancel,
  ahead-typed keys ignored within the grace period, dropped sender → closed.
- Core: with a scripted `Approver`, `Answered` appends a `UserMessage` that appears in the
  adjudication request's `recent_user_messages`; `DenyAll`/default yields the unavailable result
  and appends nothing; every permission mode (including `auto`) still reaches the approver and
  never answers itself; a deny rule blocks the call before any dialog.
- Choke-point tests: the tool is registered wrapped with `ReadOnly` risk (update the table-size
  assertions); built-in child toolsets exclude it, and a custom agent type that inherits every parent tool is refused at call time.
- Headless integration (`fontaine -p` with a deterministic completion model): the tool call
  returns the unavailable result and the run continues.
