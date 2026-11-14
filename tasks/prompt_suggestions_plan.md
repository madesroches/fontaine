# Prompt Suggestions and Input Autocomplete Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/620

## Overview

Two ghost-text aids in the TUI input box: a **prompt suggestion** (after a turn ends, a model proposes the
user's likely next prompt, shown dimmed in the empty input) and a **history completion** (while typing, the
most recent history entry that extends what is typed is shown dimmed after the cursor). Tab accepts either;
typing dismisses the suggestion.

## Current State

- Claude Code (interactive-mode docs, "Prompt suggestions"): after a response it proposes the next prompt
  as grey text; `Tab` or `Right arrow` places it in the input, `Enter` then submits; typing dismisses it.
  It is a background request on the session's model that reuses the prompt cache, skipped on a cold cache,
  after an errored response, in plan mode and near a usage limit. Off via `/config`,
  `promptSuggestionEnabled`, or `CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=false`. Claude Code has no
  history-based inline completion.
- Fontaine completion today: `/` command and skill menus and `@` mentions (`fontaine-tui/src/app.rs`
  ~1690-1760, `complete.rs`); Tab is only consumed while one of those menus is open. History:
  `fontaine-tui/src/history.rs` (`History::search`, Up/Down navigation).
- The input box is drawn by `draw_input` in `fontaine-tui/src/view.rs`; `InputBuffer` is in `input.rs`.
- `/btw` (`fontaine-core/src/side_question.rs`, `SideQuestion::ask`; runtime wrapper
  `ask_side_question` in `fontaine-core/src/agent/goal.rs`) is the precedent for a tool-less one-shot
  completion over the conversation rendered as text (`goal::transcript_of`), debited to the ledger under a
  `UsageSource` and never written to the conversation or the guardian's intent record. It has no tools, so
  it is outside the guardian path by construction. It does not share the main turn's prompt cache.
- `[tui]` config: `TuiSection`/`Tui` in `fontaine-core/src/config.rs` (~1451, ~2819), settable keys listed in
  `config_catalog.rs`; docs in `docs/tui.md` and `docs/configuration.md`.

## Design

### Prompt suggestion (model)

Core (`fontaine-core`):
- New `Purpose::PromptSuggestion` in `side_question.rs` with its own preamble ("predict the next message the
  user is most likely to type; at most 12 words; reply with the message only, or nothing if no obvious
  follow-up"; the conversation is data), a small `max_tokens` (64) and a short timeout (10 s). It reuses
  the request building, `neutralize` fencing and failure handling, so a safety fix reaches all three
  purposes. The purpose also needs arms in `preamble`, `max_tokens`, `prompt`, `unsuitable`,
  `unreachable_what` and the tracing match in `ask`; `prompt()` handles an empty question (no question is
  asked for this purpose, the transcript is the input), and the 10 s timeout is set via `with_timeout`.
- The transcript for this purpose excludes tool results: only user and assistant text and tool call names,
  so the call never sees more than the user and assistant did in the visible conversation. `blocks_of`,
  `render_transcript` and `transcript_of` currently include both tool results (`[Tool result] name\n<text>`)
  and tool-call arguments (`[Tool call] name {args JSON}`), which `/btw` relies on. The new purpose adds a
  flag on the transcript builder that reaches `blocks_of` (no second renderer): it drops tool results and
  reduces calls to their names. `/btw` keeps its current output. A test pins the argument stripping.
- New `UsageSource::PromptSuggestion` (`usage_ledger.rs`, added last because `index()` is `self as usize`;
  `UsageSource::ALL` grows to 8; the fixed-size `[Bucket; 7]` sites become 8: `usage_ledger.rs` :135
  (`Mutex<[Bucket; 7]>`) and :160 (`snapshot()`), `reports.rs` :758 (`UsageReport.by_source`) and its test
  at :1642; a `/cost` row in `fontaine-tui/src/reports.rs` in `ALL` order, which also needs the new row
  in the hard-coded `shown` list at :767-776; the row
  order in `docs/tui.md` ~817 follows) so the spend is visible, separate from `/btw`. Debited to the root
  like a compaction, including a billed but unusable answer.
- `AgentRuntime` is not `Clone` and `Ui::run` holds `&mut AgentRuntime`, so a spawned task cannot borrow
  it. The core is split in two next to `ask_side_question` in `agent/goal.rs`: an async
  `prepare_prompt_suggestion(&Inputs)` that runs the skip checks and returns `None` when one holds,
  otherwise loads the conversation from the session store with `load_async`, as `ask_side_question`
  does (no in-memory accessor exists), renders the tool-result-free transcript text, and returns owned
  values (that text, a clone of `models.main.completion`, the `Arc<SessionState>`, `billed`); and a `run`
  step on those owned values that makes the call, cleans the output, and debits the ledger via
  `state.record_usage`. The spawned task owns those values and calls `run`.
- `Inputs` is a plain struct in `fontaine-core`, next to the suggestion code in `agent/goal.rs`, with
  fields: enabled flag, permission mode, last-turn outcome, completed-turn count, unused streak.
- Skip conditions (pure function `should_suggest(&Inputs)`, unit-tested): feature off; permission mode is
  plan; last turn did not finish normally (error, interrupt); fewer than 1 completed turn; the previous
  suggestion was shown and left unused three times in a row (back-off, reset when one is accepted); the
  usage gate is paused; headless runs and child agents never ask. The TUI owns the streak, turn count,
  last outcome and enabled flag and passes them in `Inputs`. `usage_gate_idle()` waits, so the prepare step
  calls the existing non-blocking `state.usage_pause().is_paused()` and skips when paused. `billed` is passed to
  `SideQuestion::new` as `/btw` does.
- Output cleaning (pure function, unit-tested): trim, strip surrounding quotes and a leading "You:"/"User:",
  take the first line, reject empty, longer than 200 chars, starting with `/` or `!` (a suggestion must not
  pre-type a command or shell line), or equal to the user's last message.
- A suggestion is not intent until the user submits it; the call itself writes no conversation or
  ledger-intent record. Once submitted (Enter, the same path as a typed message) it is an ordinary
  `Record::UserMessage`; the `/` and `!` rejection plus the second Enter are the safeguard.

TUI (`fontaine-tui`):
- After a turn ends and the idle input loop starts, `Ui` awaits `prepare_prompt_suggestion` (a short store read) and spawns one
  task that owns the prepared values (transcript text, model clone, `Arc<SessionState>`, `billed`), so the model call does not block the
  idle input loop, and delivers its result over a channel polled in that loop, the way `statusline_task` is
  handled. The task is aborted when the user submits, starts a new turn, switches session, or quits
  (`request_quit`), so the turn is never slowed or delayed by it.
- `App` gains `suggestion: Option<String>` shown only when the input is empty, has no attachments, no
  overlay or menu is open and the view is the main agent. Cleared by any edit of the buffer, by Esc, by
  starting a turn and by a `/clear`-style reset.
- Keys: `Tab` or `Right` (plain, no menu open) fills the input; Enter then submits. `Right` is also handled
  in vim Insert mode (`input.rs` ~465, 492), so the accept is gated on `VimState`, and Ctrl/Alt+Right are
  excluded like the existing `plain` check. Typing any character dismisses it and is inserted normally.

### History completion (local)

- Pure function `History::completion(&self, typed: &str) -> Option<&str>` in `history.rs`: when `typed`
  is non-empty, single-line, and not beginning with `/` or `!`, return the remainder of the most recent
  entry that starts with `typed` and is longer than it. Entries are stored oldest-first, so this is a
  reverse scan; `starts_with` is case-sensitive, unlike `search`.
- Shown as ghost text after the cursor only when the cursor is at the end of the buffer, no completion
  menu is open and the input is not in history navigation. `Tab` or `Right` at end of buffer accepts it
  (appends the remainder); any other key behaves as before. Vim normal mode is unaffected (only insert
  mode shows or accepts it).
- No model call, no new data: it reads the history the TUI already loads.

### Rendering

`draw_input` appends a dim span (same `Modifier::DIM` style used for the viewed-agent placeholder) after the
typed rows: the suggestion on an empty input, or the completion remainder after the last row. The ghost is
clipped to the remaining width of the last row (the first row for a suggestion) so the input height
(`rows.len()` + chip rows + 2 in `view.rs`; rows are `Line::raw`) is unchanged. The ghost text is never part
of `InputBuffer::text()`, so it cannot be submitted or copied by accident. The linear renderer never calls
`draw_input`, so it shows no ghost text.

### Config

- `[tui] prompt_suggestions` (bool, default `false`): the model-generated suggestion.
- `[tui] history_completion` (bool, default `true`): the history ghost text.
- Both are preferences in `TuiSection`/`Tui` (user scope, like the rest of `[tui]`), listed in
  `config_catalog.rs` as editable booleans so `/config` toggles them (applied from the next launch, as
  the other startup-read keys). The settings schema is derived at test time, so nothing is generated.
- No environment variable (Claude Code's `CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION` is not copied; the config
  key is the single switch).

## Implementation Steps

1. `fontaine-core`: `Tui` fields and `TuiSection` options, manual defaults (~3044) and merge (~3107) in
   `config.rs`, catalog rows; update `editable_rows_are_exactly_the_preference_set` and the defaults test in
   `config_catalog.rs`, and `fontaine-core/tests/config.rs`.
2. `fontaine-core`: `Purpose::PromptSuggestion`, tool-result-free transcript rendering, output cleaning,
   `should_suggest`, `UsageSource::PromptSuggestion`, and the prepare/run split
   of `suggest_prompt`.
3. `fontaine-tui`: `History::completion`; `App` state for suggestion and completion; key handling in
   `app.rs`; `/cost` row in `reports.rs`.
4. `fontaine-tui`: await prepare, spawn `run`, abort and poll the task in `ui/mod.rs` and
   `ui/turn.rs`; back-off counter and `Inputs` state.
5. `fontaine-tui`: ghost rendering in `view.rs`.
6. Docs and CHANGELOG.

## Files to Modify

- `rust/fontaine-core/src/{config.rs,config_catalog.rs,side_question.rs,goal.rs,usage_ledger.rs}`,
  `rust/fontaine-core/src/agent/goal.rs`,
  `rust/fontaine-core/tests/{config.rs,side_question.rs}`
- `rust/fontaine-tui/src/{app.rs,history.rs,view.rs,reports.rs,lib.rs}` (`lib.rs`: `Ui`/`App` fields and the
  task handle, like `statusline_task` at ~391),
  `rust/fontaine-tui/src/ui/{mod.rs,turn.rs}`
- `docs/tui.md` (settings sample, keys table, `/cost` row order), `docs/configuration.md` (`[tui]` table and
  TOML example), `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`
  (Decisions)

## Trade-offs

- **Autocomplete while typing is history-only.** A model call per keystroke pause is slow and costly; the
  history ghost covers repeated prompts at zero cost.

## Decisions

- 2026-11-13: AFK (#620): the model suggestion defaults to off, although the issue asks for an opt-out, because without cache
  reuse it would inflate token usage on every turn; flipping the default is a one-line change once the call
  shares the main turn's cache.
- 2026-11-13: AFK (#620): accepting a suggestion only fills the input (Tab/Right); Enter then submits, as in Claude Code.
- 2026-11-13: AFK (#620): model-based autocomplete of text being typed is not implemented; file a follow-up issue.
- 2026-11-13: Deviation from Claude Code (#620): history ghost-text completion is added (Claude Code has none); recorded in the
  master plan's TUI Decisions with the feature.
- 2026-11-13: AFK (#620): the transcript flag drops tool results and reduces tool calls to names for the suggestion purpose only; `blocks_of` includes both today, so `/btw` output stays unchanged (the earlier claim that it already omitted them was wrong).
- 2026-11-13: AFK (#620): no new gate accessor; the prepare step uses the existing public `SessionState::usage_pause().is_paused()`, which does not block.

## Documentation

- `docs/tui.md`: new "Prompt suggestions and history completion" section (keys, skip conditions, cost).
- `docs/configuration.md`: the two `[tui]` keys.
- `CHANGELOG.md` entry; master plan `## Decisions` lines for the deviations above, added at the end of the `### TUI` subsection.

## Testing Strategy

Unit tests (no live API; deterministic completion model from `rig_core::test_utils`):
- `should_suggest` over every skip condition, including back-off after three unused suggestions.
- Output cleaning: quotes, prefixes, multiline, empty, over-long, `/`- and `!`-leading, equal to last message.
- Transcript rendering excludes tool results for this purpose; includes them for `/btw` unchanged.
- Transcript argument stripping: tool-call arguments are reduced to names for this purpose.
- `run` step against a fake model: usage debited under `UsageSource::PromptSuggestion` on success and
  on a billed unusable answer; nothing written to the conversation or guardian intent; timeout and error
  return `None` without surfacing a failure.
- `History::completion`: most recent match wins, exact match yields none, slash/shell/multiline excluded.
- `App` key handling: Tab/Right accept, typing dismisses, menus take precedence, vim normal mode ignores,
  suggestion hidden with attachments or overlays, Right gated off in vim Insert and with Ctrl/Alt.
- `view.rs` render tests with `TestBackend`: ghost text is dim, not in `InputBuffer::text()`, and clipped
  so the input height is unchanged.
- Config: defaults, scope handling, catalog stays in sync (existing tests).
- A cost-ledger test pins that the suggestion call is visible in `/cost`.

## Manual Verification

1. `fontaine` with `[tui] prompt_suggestions = true`, finish a turn; expect dim text in the empty input
   that Tab fills and typing dismisses. Not automated because the real provider response and terminal
   rendering must be seen together; the call itself is covered by the fake-model tests.
2. In a real tmux (see `tasks/design/testing.md`), type the start of an earlier prompt; expect dim
   completion that Tab accepts, with no layout shift on wrapped input.
