# Prompt Suggestions and Input Autocomplete Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/620

## Overview

Two ghost-text aids in the TUI input box: a **prompt suggestion** (after a turn ends, a model proposes the
user's likely next prompt, shown dimmed in the empty input) and a **history completion** (while typing, the
most recent history entry that extends what is typed is shown dimmed after the cursor). Tab accepts either;
typing dismisses the suggestion. The suggestion costs one small tool-less model call per turn, so it is
opt-in; the history completion is local and free, so it is on by default.

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
- `[tui]` config: `TuiSection`/`Tui` in `fontaine-core/src/config.rs` (~1449, ~2819), settable keys listed in
  `config_catalog.rs`; docs in `docs/tui.md` and `docs/configuration.md`.

## Design

### Prompt suggestion (model)

Core (`fontaine-core`):
- New `Purpose::PromptSuggestion` in `side_question.rs` with its own preamble ("predict the next message the
  user is most likely to type; at most 12 words; reply with the message only, or nothing if no obvious
  follow-up"; the conversation is data), a small `max_tokens` (64) and a short timeout (10 s). It reuses
  the request building, `neutralize` fencing and failure handling, so a safety fix reaches all three
  purposes.
- The transcript for this purpose excludes tool results: only user and assistant text and tool call names,
  so the call never sees more than the user and assistant did in the visible conversation. If
  `transcript_of` does not already allow this, add a parameter on `render_transcript` rather than a second
  renderer.
- New `UsageSource::PromptSuggestion` (`usage_ledger.rs`, `UsageSource::ALL`, the `/cost` rows in
  `fontaine-tui/src/reports.rs`) so the spend is visible, separate from `/btw`. Debited to the root like a
  compaction, including a billed but unusable answer.
- `AgentRuntime::suggest_prompt()` (next to `ask_side_question` in `agent/goal.rs`): returns `Ok(None)`
  without calling the model when any skip condition holds, otherwise the cleaned suggestion. Skip
  conditions (pure function `should_suggest(&Inputs)`, unit-tested): feature off; permission mode is plan;
  last turn did not finish normally (error, interrupt); fewer than 1 completed turn; the previous
  suggestion was shown and left unused three times in a row (back-off, reset when one is accepted);
  the usage gate is paused; main model is not billed per token and not subscription-backed unsuitable
  (reuse `catalog_billed` handling like `/btw`); headless runs and child agents never ask.
- Output cleaning (pure function, unit-tested): trim, strip surrounding quotes and a leading "You:"/"User:",
  take the first line, reject empty, longer than 200 chars, starting with `/` or `!` (a suggestion must not
  pre-type a command or shell line), or equal to the user's last message.
- The call goes through the same completion model as `/btw`, no tools, no guardian involvement and no
  conversation or ledger-intent write, so it cannot act and is never guardian intent.

TUI (`fontaine-tui`):
- After a turn ends and the idle input loop starts, `Ui` spawns one task that owns what it needs (a clone of
  the main completion model and a snapshot of the conversation text), so the model call does not block the
  idle input loop, and delivers its result over a channel polled in that loop, the way `statusline_task` is
  handled. The task is aborted when the user submits, starts a new turn, switches session, or quits
  (`request_quit`), so the turn is never slowed or delayed by it.
- `App` gains `suggestion: Option<String>` shown only when the input is empty, has no attachments, no
  overlay or menu is open and the view is the main agent. Cleared by any edit of the buffer, by Esc, by
  starting a turn and by a `/clear`-style reset.
- Keys: `Tab` or `Right` (plain, no menu open) writes the suggestion into the input; `Enter` on an empty
  input with a suggestion does nothing special: accepting only fills the input and a second Enter
  submits, as in Claude Code. Typing any character dismisses it and is inserted normally.

### History completion (local)

- Pure function `History::completion(&self, typed: &str) -> Option<&str>` in `history.rs`: when `typed`
  is non-empty, single-line, and not beginning with `/` or `!`, return the remainder of the most recent
  entry that starts with `typed` and is longer than it.
- Shown as ghost text after the cursor only when the cursor is at the end of the buffer, no completion
  menu is open and the input is not in history navigation. `Tab` or `Right` at end of buffer accepts it
  (appends the remainder); any other key behaves as before. Vim normal mode is unaffected (only insert
  mode shows or accepts it).
- No model call, no new data: it reads the history the TUI already loads.

### Rendering

`draw_input` appends a dim span (same `Modifier::DIM` style used for the viewed-agent placeholder) after the
typed rows: the suggestion on an empty input, or the completion remainder after the last row. The ghost text
is never part of `InputBuffer::text()`, so it cannot be submitted or copied by accident. In screen-reader
mode the ghost is not drawn; a one-line hint ("suggestion available, Tab to accept") is announced instead
through the existing announcement path.

### Config

- `[tui] prompt_suggestions` (bool, default `false`): the model-generated suggestion.
- `[tui] history_completion` (bool, default `true`): the history ghost text.
- Both are preferences in `TuiSection`/`Tui` (user scope, like the rest of `[tui]`), listed in
  `config_catalog.rs` as editable booleans so `/config` toggles them (applied from the next launch, as
  the other startup-read keys). Add them to the JSON schema in `docs/schemas` via its generator.
- No environment variable (Claude Code's `CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION` is not copied; the config
  key is the single switch).

## Implementation Steps

1. `fontaine-core`: `Tui` fields and `TuiSection` options, merge in `config.rs`, catalog rows, schema.
2. `fontaine-core`: `Purpose::PromptSuggestion`, tool-result-free transcript rendering, output cleaning,
   `should_suggest`, `UsageSource::PromptSuggestion`, `AgentRuntime::suggest_prompt`.
3. `fontaine-tui`: `History::completion`; `App` state for suggestion and completion; key handling in
   `app.rs`; `/cost` row in `reports.rs`.
4. `fontaine-tui`: spawn/abort/poll of the suggestion task in `ui/mod.rs` and `ui/turn.rs`; back-off counter.
5. `fontaine-tui`: ghost rendering in `view.rs`; screen-reader hint.
6. Docs and CHANGELOG.

## Files to Modify

- `rust/fontaine-core/src/{config.rs,config_catalog.rs,side_question.rs,goal.rs,usage_ledger.rs}`,
  `rust/fontaine-core/src/agent/goal.rs`
- `rust/fontaine-tui/src/{app.rs,history.rs,view.rs,reports.rs}`,
  `rust/fontaine-tui/src/ui/{mod.rs,turn.rs}`
- `docs/tui.md`, `docs/configuration.md`, `docs/schemas/*`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`
  (Decisions)

## Trade-offs

- **Default off for the model suggestion.** Fontaine's `SideQuestion` renders the conversation as text and
  does not reuse the main turn's prompt cache, so each suggestion re-reads the whole transcript as fresh
  input; Claude Code's version is cheap only because of cache reuse. Making it opt-in keeps token usage
  from inflating silently. A cache-sharing request path is a separate optimization.
- **Autocomplete while typing is history-only.** A model call per keystroke pause is slow and costly; the
  history ghost covers repeated prompts at zero cost.
- **Reusing `SideQuestion`** rather than a new client: same tool-less, ledger-debited, guardian-free path.

## Decisions

- AFK: the model suggestion defaults to off, although the issue asks for an opt-out, because without cache
  reuse it would inflate token usage on every turn; flipping the default is a one-line change once the call
  shares the main turn's cache.
- AFK: accepting a suggestion only fills the input (Tab/Right); Enter then submits, as in Claude Code.
- AFK: model-based autocomplete of text being typed is not implemented; file a follow-up issue.
- Deviation from Claude Code: history ghost-text completion is added (Claude Code has none); recorded in the
  master plan's TUI Decisions with the feature.

## Documentation

- `docs/tui.md`: new "Prompt suggestions and history completion" section (keys, skip conditions, cost).
- `docs/configuration.md`: the two `[tui]` keys.
- `CHANGELOG.md` entry; master plan `## Decisions` (TUI subsection) lines for the deviations above.

## Testing Strategy

Unit tests (no live API; deterministic completion model from `rig_core::test_utils`):
- `should_suggest` over every skip condition, including back-off after three unused suggestions.
- Output cleaning: quotes, prefixes, multiline, empty, over-long, `/`- and `!`-leading, equal to last message.
- Transcript rendering excludes tool results for this purpose; includes them for `/btw` unchanged.
- `suggest_prompt` against a fake model: usage debited under `UsageSource::PromptSuggestion` on success and
  on a billed unusable answer; nothing written to the conversation or guardian intent; timeout and error
  return `None` without surfacing a failure.
- `History::completion`: most recent match wins, exact match yields none, slash/shell/multiline excluded.
- `App` key handling: Tab/Right accept, typing dismisses, menus take precedence, vim normal mode ignores,
  suggestion hidden with attachments or overlays.
- `view.rs` render tests with `TestBackend`: ghost text is dim, not in `InputBuffer::text()`, absent in
  screen-reader mode.
- Config: defaults, scope handling, catalog and schema stay in sync (existing tests).
- A cost-ledger test pins that the suggestion call is visible in `/cost`.

## Manual Verification

1. `fontaine` with `[tui] prompt_suggestions = true`, finish a turn; expect dim text in the empty input
   that Tab fills and typing dismisses. Not automated because the real provider response and terminal
   rendering must be seen together; the call itself is covered by the fake-model tests.
2. In a real tmux (see `tasks/design/testing.md`), type the start of an earlier prompt; expect dim
   completion that Tab accepts, with no layout shift on wrapped input.

## Open Questions

- Should accepted suggestions be recorded in the ledger as human intent? Proposed: yes, once submitted,
  because it is then an ordinary typed message the user chose to send.
