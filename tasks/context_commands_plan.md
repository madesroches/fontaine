# Context Commands Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/11

## Overview

Claude Code lets the user manage the model's context from inside a session: `/clear` starts over
with an empty context, `/compact [instructions]` summarizes the conversation on request,
`/context` shows what fills the window, and an auto-compact window decides how full the window
gets before compaction runs on its own. Fontaine already compacts automatically between turns
(rig-memory's `CompactionPolicy`) but has no way to do any of this by hand or to tune when it
happens. This plan adds `/clear` (aliases `/reset`, `/new`), `/compact [focus]`, `/context [all]`
to the terminal UI, and a `[context] autocompact_window` setting honored by both the terminal UI
and headless runs.

## Current State

- `rust/fontaine-core/src/session.rs:89` `Session` holds the rig-memory `DirStore` and a
  `CompactionPolicy::default()` (`session.rs:131,163`). `compact_if_needed` (`session.rs:208`)
  wraps `compact(model, CompactReason::Threshold, …)` in a `fontaine.compaction` span. `compact`
  (`session.rs:233`) returns early for `Threshold` when the model's window is unknown
  (`session.rs:241`), checks `policy.over_threshold(estimate_tokens(..), spec)` (rig-memory: the
  window minus `summarizer.limits.reserve`, 16 384 tokens by default), plans the summary
  (`plan_summary`), calls the model, and saves with `apply_summary`. Failures warn through the
  `Approver` and never fail the run.
- `CompactReason::Asked { focus }` already exists in rig-memory and puts the focus into the
  summarizer prompt (`Additional focus: …`). Fontaine only uses it in tests
  (`rust/fontaine-core/tests/ledger.rs:93`, `tests/permissions_auto.rs:590`).
- Threshold compaction runs before every turn: `rust/fontaine-tui/src/lib.rs:1349` and
  `rust/fontaine/src/headless.rs:610`.
- The agent is built with `.memory(session.store()).conversation(CONVERSATION_LOG)`
  (`rust/fontaine-core/src/agent.rs:1352`), so the conversation is whatever the `main` log holds;
  `rebuild` (`agent.rs:625`) keeps it.
- The system prompt is assembled in `build_agent` (`agent.rs:1297`) from `prompt::PromptParts`
  (`rust/fontaine-core/src/prompt.rs:30`): base layer or `--system-prompt`, instruction files,
  the skill index, the output style, `--append-system-prompt`. The pieces are not kept.
- Tool definitions: `ToolServerHandle::tool_defs(None)` (rig-agent) returns every advertised
  `ToolDefinition`; MCP tools are named `mcp__<server>__<tool>`
  (`rust/fontaine-mcp/src/register.rs:34`). rig-memory's `HeuristicTokenCounter` has
  `count(&Message)` and `count_tool(&ToolDefinition)`; `estimate_tokens` uses its default.
- The ledger (`rust/fontaine-core/src/ledger.rs`) is append-only; `LedgerView::apply`
  (`ledger.rs:188`) accumulates `user_messages`, which the guardian request reads both for
  `recent_user_messages` and for `task_intent` (`rust/fontaine-core/src/adjudication.rs:98`).
- Slash commands live in one table, `rust/fontaine-tui/src/commands.rs` (`BUILTINS`), dispatched
  in `lib.rs` `submit`; read-only reports are pure builders in `rust/fontaine-tui/src/reports.rs`.
  The core slash-commands plan (`tasks/completed/core_slash_commands_plan.md`) assigned `/clear`,
  `/compact` and `/context` to this issue.
- `Transcript` (`rust/fontaine-tui/src/transcript.rs:45`) keeps `entries` plus `committed` /
  `committed_chars`, the part already written to scrollback.
- Config: `[agents]` is the precedent for a key that is not allowed from shared project scope but
  is from project-local and CLI scope (`config.rs:703`, the scope table in
  `docs/configuration.md:85`).

### Claude Code semantics this follows

From code.claude.com/docs (`/en/commands`, `/en/model-config`):

- `/clear [name]`: "Start a new conversation with empty context." Aliases `/reset`, `/new`. The
  name labels the old conversation in `/resume`.
- `/compact [instructions]`: "Free up context by summarizing the conversation so far. Optionally
  pass focus instructions for the summary."
- `/context [all]`: visualizes current context usage, with `all` expanding the per-item
  breakdown, and warns when the conversation exceeds the window.
- The auto-compact window is a **token count** (`autoCompactWindow` setting, `/autocompact`,
  `--autocompact`, `CLAUDE_CODE_AUTO_COMPACT_WINDOW`), capped at the model's context window.
  The older percentage override is gone from the docs.

## Design

### `[context] autocompact_window`

```toml
[context]
autocompact_window = 150000   # tokens; compaction runs once a turn would send more
```

- New `ContextSection { autocompact_window: Option<u64> }` in `Settings` (`deny_unknown_fields`),
  merged into `Config.context: Context { autocompact_window: Option<u64> }`, higher scope wins.
- Validation: a positive integer; `0` is a load error (exit 2) naming the key.
- Scope: not allowed from shared project scope (rejected like `agents` in the project-tighten
  check), allowed from project-local (trusted), user and `--settings`. A shared repository file
  should not decide how often a user's tokens are spent on summaries.
- `Session` gains `autocompact_window: Option<u64>` with a setter
  (`Session::set_autocompact_window`), set by `AgentRuntime::new` from `config.context`. The
  trigger becomes one function, `Session::over_threshold(tokens, spec) -> bool`:
  `policy.over_threshold(tokens, spec) || autocompact_window.is_some_and(|w| tokens > w)`. So the
  effective trigger is the lower of the window minus rig's reserve and the configured window,
  which is Claude Code's "capped at the model's context window".
- The `Threshold` early return on an unknown model window is narrowed: with
  `autocompact_window` set, threshold compaction runs even when the catalog does not know the
  window (Claude Code's gateway case: "set the window … so sessions compact at that boundary").
  Without it, behavior is unchanged.

### `/compact [focus]`

- `Session::compact_if_needed` is generalized to `Session::compact_logged(model, priced, reason,
  approver) -> Option<Compacted>` (the span, the unpriced-cost drop), and `compact_if_needed`
  becomes the `Threshold` call of it. The span gains a `compaction.reason` field
  (`threshold` / `asked`).
- `compact` returns what it did instead of only the usage:

  ```rust
  pub struct CompactOutcome {
      pub usage: Option<Usage>,       // the summarizer call's, when it answered
      pub before: u64,                // estimate_tokens of the conversation before
      pub after: Option<u64>,         // after, when a summary was saved
  }
  ```

  `None` stays "nothing was done" (no window for a threshold call, under the threshold, nothing
  new to summarize). Existing callers take `.usage`.
- TUI: `Command::Compact(String)`; the arm runs `compact_logged` with
  `CompactReason::Asked { focus }` on the main model, while the turn loop keeps serving keys and
  draws a `compacting…` status (the same `running` flag a turn sets, so Enter queues text). The
  usage is added to the session totals the way a turn's is. Notices:
  - saved: `compacted the conversation: ~41.2k → ~6.3k tokens`
  - nothing to summarize: `nothing to compact`
  - a failure already warned through the `Approver`; no second notice.
- Esc interrupts it like a turn: the future is dropped, the saved conversation is untouched
  (`compact` saves only at its end).

### `/clear`

`/clear` empties the model's context in the **same session**: same id, same ledger, audit log,
grants and mode. (Claude Code starts a new session id; see Decisions.)

- `Session::clear(&self) -> Result<(), ClearError>`: loads the `main` log, removes every
  aside (`Conversation::remove_aside` over `asides()`, which takes the `rig.compaction` records
  with them), calls `set_entries(Vec::new(), Author::plugin("clear"))`, and saves. Both are
  logged changes, so the file still holds the old conversation; only the live view is empty.
  Removing the compaction asides matters: `summary_state` builds the next summary on the newest
  `Compacted` record, so leaving them would carry the cleared conversation's summary into the
  next compaction. If the conversation is halted (`halted()`), `resume()` is called so the empty
  conversation is usable. `ClearError` is a plain chain (`source()`), not a dispatch enum.
- Ledger: new `Record::ContextCleared {}`. `LedgerView::apply` clears `user_messages` on it, so
  the guardian's `task_intent` and `recent_user_messages` start again from what the human asks
  after the clear — the same view a new Claude Code session gives its classifier. Actions,
  denials, the file index and created-file facts are kept: they describe the workspace, not the
  conversation.
- `AgentRuntime::clear_conversation(&self) -> Result<(), ClearError>` runs `Session::clear`, then
  appends the record. The record is appended only after the clear succeeded, so the ledger never
  claims a clear that did not happen.
- TUI: `Command::Clear(String)` with aliases `reset` and `new`. A non-empty argument is an error
  notice: `naming the cleared conversation needs /resume, which is not available yet`. The arm
  calls `clear_conversation`; on success it calls `Transcript::clear()` (empties `entries` and
  resets `committed`/`committed_chars`, so `/copy` counts from the new conversation; what was
  already written stays in the terminal's scrollback), clears the queued harness notes
  (`pending_notices`, which described the old conversation), and adds the notice
  `cleared the conversation; the session, its permissions and its mode carry on`. On error, an
  error notice with `error_chain::chain`.
- Background subagents keep running; a report that arrives later is a turn of the new
  conversation, as it is after any turn.

### `/context [all]`

A read-only notice, built by a pure function from a core snapshot.

- Core: new module `rust/fontaine-core/src/context.rs`:

  ```rust
  pub struct ContextUsage {
      pub window: Option<u32>,          // the main model's, from the catalog
      pub autocompact_at: Option<u64>,  // the effective trigger, None when it never fires
      pub autocompact_source: TriggerSource, // Reserve | Configured
      pub system_prompt: u64,           // base layer (or --system-prompt) and --append-system-prompt
      pub output_style: u64,
      pub instructions: Vec<(PathBuf, u64)>,
      pub skills: u64,                  // the skill index in the prompt
      pub tools: Vec<(String, u64)>,    // built-in and extension tools
      pub mcp_tools: Vec<(String, u64)>,// mcp__* tools
      pub messages: u64,                // estimate_tokens of the conversation
  }
  ```

  Every count is the `HeuristicTokenCounter::default()` estimate rig-memory's threshold uses
  (text counted as `count(&Message::user(text))`, tools with `count_tool`), so `/context` and the
  compaction trigger agree.
- `build_agent` keeps what it assembled: the `Parts` → `PromptParts` texts are stored on
  `AgentRuntime` as `prompt_pieces: PromptPieces` (owned strings for each layer), refreshed by
  `rebuild`. `prompt::assemble` takes the same parts, so the pieces are exactly what is sent.
- `Session::autocompact_at(spec) -> Option<u64>` gives the effective trigger (the min of
  `window - reserve` and the configured window) from the same inputs as `over_threshold`, so the
  report cannot drift from the trigger.
- `AgentRuntime::context_usage(&self) -> ContextUsage` (async: it loads the conversation and the
  tool definitions).
- TUI: `Command::Context(String)`; `reports::context(&ContextUsage, all: bool)`:

  ```
  context ~48.6k of 200k tokens (24%), estimated · anthropic/claude-sonnet-5-5
    system prompt   2.1k
    instructions    1.3k
    skills          0.4k
    tools           6.2k  (14)
    MCP tools       2.9k  (9)
    messages       35.7k
    free          151.4k
  auto-compacts at ~183.6k (window minus a 16.4k reserve)
  ```

  - With `all`, each instruction file, tool and MCP tool gets its own indented line under its
    row, largest first. Any other argument is an error notice naming `all`.
  - Unknown window: the first line reads `~48.6k tokens, estimated (window unknown)`, no `free`
    row, and the last line `no automatic compaction: the model's window is unknown` unless
    `autocompact_window` is set (`auto-compacts at ~150.0k ([context] autocompact_window)`).
  - Over the window: a last line `over the window by ~12.0k tokens: /compact or /clear frees space`.
  - `output style` appears only when one is set.

### Headless

`autocompact_window` applies to headless runs through the same `compact_if_needed`. The slash
commands are terminal-UI only, as the other built-ins are.

## Implementation Steps

1. **Config.** Add `ContextSection` / `Context` and the `context` field in
   `rust/fontaine-core/src/config.rs`: parse, the `0` check, the project-scope rejection, merge.
   Update `docs/schemas` if the settings JSON schema lists sections (check how `[agents]` is
   represented there).
2. **Session trigger.** In `session.rs`, add `autocompact_window`, its setter,
   `over_threshold`, `autocompact_at`, the narrowed unknown-window return, `CompactOutcome`, and
   `compact_logged` (with the reason on the span). Update `compact_if_needed`'s callers
   (`lib.rs:1349`, `headless.rs:610`) and the tests that call `compact` for `.usage`.
3. **Clear in the store.** `Session::clear` and `ClearError` in `session.rs`.
4. **Ledger record.** `Record::ContextCleared` and the `LedgerView::apply` reset in
   `ledger.rs`.
5. **Runtime.** In `agent.rs`: set the session's window from `config.context`; keep
   `PromptPieces` from `build_agent`; add `clear_conversation` and `context_usage` (the latter
   via the new `context.rs`, registered in `lib.rs` of `fontaine-core`).
6. **Commands.** In `commands.rs`, rows for `clear` (aliases `reset`, `new`), `compact` (hint
   `[focus]`), `context` (hint `[all]`), and the `Command` variants. In `reports.rs`, the
   `context` builder. In `transcript.rs`, `Transcript::clear`. In `lib.rs` `submit`, the three
   arms; `/compact` runs through the same select loop a turn uses so keys and Esc are served.
7. **Docs and master plan.** See Documentation; add the Decisions lines below to
   `tasks/fontaine_harness_plan.md` `## Decisions`.

## Files to Modify

- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-core/src/session.rs`
- `rust/fontaine-core/src/ledger.rs`
- `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-core/src/context.rs` (new), `rust/fontaine-core/src/lib.rs`
- `rust/fontaine-tui/src/commands.rs`, `reports.rs`, `transcript.rs`, `lib.rs`
- `rust/fontaine/src/headless.rs` (the `compact_if_needed` return type)
- `rust/fontaine-core/tests/ledger.rs`, `rust/fontaine-core/tests/permissions_auto.rs`
- `rust/fontaine-tui/tests/tui.rs`, `rust/fontaine/tests/headless_contract.rs`
- `docs/tui.md`, `docs/configuration.md`, `docs/headless.md`, `docs/architecture.md`,
  `docs/guardian.md`, `docs/index.md`, `tasks/design/config.md`
- `CHANGELOG.md` (PR step), `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **`/clear` in place vs. a new session id.** A new id means a new ledger, audit log and
  `SessionState` mid-run (the state holds the ledger and session id as plain fields shared with
  the gate, the agent tree and the child runner), and the old conversation would be reachable
  only through `/resume`, which does not exist yet (#10). Clearing in place keeps the
  old messages in the log file, resets what the guardian treats as the human's intent, and
  touches two stores. Chosen: in place.
- **Remove compaction asides vs. a new conversation log name.** A new log name per clear would
  need the current name stored for resume and passed to every agent rebuild. Removing the asides
  is two calls on the loaded conversation. Chosen: remove the asides.
- **Token window vs. the issue's `autocompact_pct`.** Claude Code moved to a token window and
  the docs no longer describe a percentage; a token count also states the gateway case directly.
  Chosen: `autocompact_window`.
- **`/context` as text vs. a colored grid.** The other reports are read-only transcript notices
  (core slash-commands decision); the grid adds nothing a table of numbers does not say.

## Decisions

- 2026-10-24: AFK: `/clear` clears the conversation in the current session (same id, ledger,
  audit log, grants and mode) instead of starting a new session as Claude Code does. Reason:
  `/resume` (#10) is not there to reach an old session, and swapping the ledger under a live
  `SessionState` is a rework of its own; the log file keeps the old messages.
- 2026-10-24: AFK: after `/clear`, the guardian's view of the human's messages restarts at the
  `ContextCleared` ledger record; workspace facts (actions, denials, file index, created files)
  are kept. Reason: matches what a new Claude Code session's classifier sees, without dropping
  facts about the files.
- 2026-10-24: AFK: `/clear <name>` is an error until `/resume` exists. Reason: the name only
  labels the old conversation in `/resume`.
- 2026-10-24: AFK: the auto-compact setting is `[context] autocompact_window` (tokens), not the
  issue's `autocompact_pct`. Reason: Claude Code's `autoCompactWindow` is a token count.
- 2026-10-24: AFK: `autocompact_window` accepts any positive token count, where Claude Code
  accepts 100K–1M. Reason: Fontaine runs local and gateway models whose windows are below 100K.
- 2026-10-24: AFK: `[context]` is refused from shared project scope. Reason: a repository should
  not decide how often a user pays for summaries; project-local and user scope can set it.
- 2026-10-24: AFK: no `/autocompact` command, `--autocompact` flag or
  `CLAUDE_CODE_AUTO_COMPACT_WINDOW` variable; the setting is config only (`--settings` covers a
  single launch). Reason: one way to set it; `/autocompact` writes user settings, which belongs
  with `/config` (#4).
- 2026-10-24: AFK: `/context` is a text notice, not Claude Code's colored grid and suggestions.
  Reason: same as the other read-only reports.

## Documentation

- `docs/tui.md` `## Commands`: rows for `/clear` (`/reset`, `/new`), `/compact [<focus>]`,
  `/context [all]`, in sorted order; a short `## Context` section: what each does, that counts are
  estimates, that `/clear` keeps the session, its permissions and mode, and that Esc interrupts
  `/compact`.
- `docs/configuration.md`: a `### [context]` section for `autocompact_window` (default unset,
  meaning the window minus rig's reserve; capped by it; applies when the window is unknown), and a
  row in the scope table (project: no; project-local: yes; CLI: yes).
- `docs/headless.md` (~line 57): compaction honors `[context] autocompact_window`.
- `docs/architecture.md` (~line 155, the two stores): `/clear` empties the model context and adds
  a `ContextCleared` ledger record; the ledger keeps everything.
- `docs/guardian.md`: the human's messages the guardian sees start after the last `/clear`.
- `docs/index.md`: the new commands in the terminal UI command list.
- `tasks/design/config.md`: the `[context]` sample block and the scope-table row.
- `CHANGELOG.md`: `### Added` under `[Unreleased]`, Terminal UI group, citing #11 (PR step).

## Testing Strategy

Unit tests:
- Config: `[context] autocompact_window = 150000` parses; `0` is an error naming the key;
  an unknown key under `[context]` is an error; the key from project scope is refused, from
  project-local and user scope accepted; a higher scope wins.
- `Session::over_threshold` / `autocompact_at`: reserve only; configured lower than
  `window - reserve` wins; configured higher is capped; unknown window with and without a
  configured value.
- `LedgerView`: user messages before a `ContextCleared` record are gone from the view, later
  ones kept; actions, denials and `existence` are unchanged across it; a reopened ledger replays
  it the same way.
- `reports::context`: known window (percent, free, reserve line); unknown window with and
  without the configured value; over the window; `all` lists items largest first; output style
  row only when set; a bad argument is an error.
- `commands.rs`: `/clear`, `/reset`, `/new` → `Clear`; `/compact keep the API notes` →
  `Compact("keep the API notes")`; `/context all` → `Context("all")`; the table checks stay green.
- `Transcript::clear`: entries and committed counters reset; `assistant_reply(1)` is `None`.

Integration tests (offline: the deterministic test model / wiremock already used by
`tests/ledger.rs`, `tests/tui.rs`, `headless_contract.rs`):
- `Session::clear` (`tests/ledger.rs`): after a compaction then a clear, the loaded conversation
  is empty and `summary_state` has no summary, so the next compaction's request carries nothing
  of the cleared conversation — this is the silent leak the aside removal prevents. The log file
  still contains the old messages.
- Threshold with a configured window: a conversation under `window - reserve` but over
  `autocompact_window` compacts; a model with no catalog window compacts when the value is set
  and is left alone when it is not.
- Guardian request after `/clear` (`tests/permissions_auto.rs`): a human message before the
  clear is absent from `recent_user_messages` and `task_intent`, one after it is present.
- TUI (`tests/tui.rs`): `/compact focus on the parser` sends a summarizer request containing
  `Additional focus: focus on the parser` and shows the `compacted` notice; `/clear` then a
  message sends a request with no earlier user or assistant message and the ledger holds the
  record; `/context` after a reply shows the `messages` and `tools` rows.
- Headless (`headless_contract.rs`): a `--settings` file with a small `autocompact_window`
  triggers the summarizer call before the turn on a conversation the reserve alone would not
  compact.

## Open Questions

None.
