# Session UX Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/10

## Overview

Fontaine can already reopen a session (`--continue`, `--resume <id>`, `--fork-session`), but only
by UUID, only at launch, and with no way to tell sessions apart or take a conversation out of the
store. This adds Claude Code's session UX: session names (`--name`, `/rename`), a resume picker
(`/resume`), `--resume <name>`, `/branch` (fork the live session and carry on in
the copy), and `/export` (the conversation as a text file). Issue #10.

## Current State

- Sessions are directories under `<data dir>/sessions/<uuid>/` holding rig-memory's conversation
  log, `ledger/ledger.jsonl`, `todos.json` and `schedule.json` (`tasks/design/sessions.md`).
  `Session::{create,open,fork}` and `latest_for_root` live in `fontaine-core/src/session.rs`.
- `latest_for_root` (`session.rs`, near the end) already scans every session's ledger first line
  (`Record::SessionStart`) for its primary root and takes the newest by ledger mtime. There is no
  general listing.
- `open_session` (`fontaine/src/lib.rs`) maps `--resume <id>` / `--continue` / `--fork-session` to
  one of those calls. `--resume` takes a required id (`fontaine/src/cli.rs`) and stays that way: the picker is a TUI command only. Flag combinations are
  checked in `check_session_flags`; `fontaine/tests/resume.rs` covers them headlessly.
- `Record::Resumed { source, how }` records continuation in the ledger. `LedgerView` ignores
  records that are not the human's words (`Scheduled`), the pattern for a record the guardian must
  not see.
- The TUI runs one session for the life of `fontaine_tui::drive` (`fontaine-tui/src/lib.rs`):
  `SessionState` holds the ledger and session id as plain fields shared with the gate, agent tree
  and child runner, so swapping the session in place is a rework (the master plan's 2026-10-24
  `/clear` decision). On a resumed session the transcript starts empty with a one-line note; the
  model has the history.
- Slash commands are one table (`fontaine-tui/src/commands.rs`, `BUILTINS`) that the parser and
  the `/` menu both read; handlers are dispatched from `fontaine-tui/src/ui/mod.rs`. The model
  picker (`model_picker.rs`) is the pattern for a filterable overlay.
- `docs/tui.md` says of `/clear` that naming the old conversation "needs `/resume`, which is not
  available yet".

## Design

### Names and metadata (core)

- New ledger record `SessionNamed { name }`, appended by rename. The latest one wins. Like
  `Scheduled`, `LedgerView` ignores it, so the guardian's view of intent is untouched; the ledger
  stays append-only. `--name` writes one at session start.
- `session::validate_name(&str) -> Result<String, NameError>`: trimmed, 1–80 chars, no control
  characters. One validator for the flag and `/rename`.
- `session::list(data_dir, root: Option<&Path>) -> Vec<SessionInfo>`; `SessionInfo` holds `id`,
  `name: Option<String>`, `first_prompt: Option<String>` (the first entry of `ledger.view().user_messages`, the
  same source `/rename` derives from, so it is the first message since the last clear; truncated
  for display), `updated` (ledger mtime), `primary_root`, `branched_from: Option<String>` (the `source` of the last
  `Resumed` record whose `how` starts with `fork`; the copy's id is its directory name, not the
  `SessionStart` id copied from the source). `Session::fork` copies the whole tree, so `list` takes
  the id from the directory name and reads the last `SessionNamed` and `Resumed` records as the
  copy's own. The TUI fork step appends the copy's own `Resumed` (`how` `fork (branch)`) and, when
  named, `SessionNamed` at fork time, so neither `list` nor `resolve` ever sees an inherited record
  as the copy's. It reads each ledger once, tolerating a torn
  or unreadable one by skipping it. `latest_for_root` becomes `list(.., Some(root))` first-by-
  recency, so there is one scan, not two.
- `session::resolve(data_dir, root, text) -> Result<String, ResolveError>` for `--resume` and
  `/resume <text>`: `is_session_id` is applied to `text` before any path join or directory check;
  a session directory named `text` wins; otherwise exact, case-insensitive name
  match among sessions started in `root`. Zero matches is `Missing`; several is `Ambiguous(ids)`
  (typed because the caller prints the candidates and exits 2).
- `Session::rename(&self, name)` appends `SessionNamed` through the ledger; `Session::name()`
  reads the latest. A ledger append failure reaches the user through `Approver::warn`, per the
  error-handling rule.

### CLI

- `--name`/`-n <name>`: names the session being started, including the copy made by
  `--fork-session`; `open_session` applies it on every path that returns a session (fresh,
  `--session-id`, and `--continue` falling through to a fresh session). With plain `--resume`/`--continue` (no fork) it renames the reopened session
  (appends `SessionNamed`); `check_session_flags` needs no new rule for it. Invalid names are a
  usage error. Every fork writes its own `SessionNamed` for the copy, since the copied ledger
  carries the source's: the given name, else the source's name plus ` (branch)` when the source has
  one, else none. Without this a `--fork-session` copy would share the source's name and `resolve`
  would return `Ambiguous`.
- `--resume`/`-r` keeps its required value, now an id or name (`resolve`); there is no bare
  `--resume` picker at launch (the picker is an overlay inside `drive`, which needs a live session,
  and an optional value would swallow the positional PROMPT). `/resume` in the TUI is the picker.
  `conflicts_with = "continue_session"` stays.

### TUI

- `ResumePicker` (new `fontaine-tui/src/resume_picker.rs`, modelled on `ModelPicker`): rows of
  name-or-first-prompt, relative age, and a `⎇` marker for a branch; typed text filters by
  substring over name, first prompt and id; Up/Down/Enter/Esc. It lists sessions started in the
  current workspace root, like `--continue`; the current session is omitted.
- `/resume [name-or-id]`: with an argument resolves it as above and switches; without opens the
  picker. Only between turns.
- `/branch [name]` (alias `/fork`): `Session::fork` of the live session under a fresh id (named as
  for `--fork-session`), then switches into the copy. The original is left as it was. Only between
  turns, like `/resume`; the fork runs synchronously on the TUI task. Refused under the same two
  conditions as `/clear` (`agents.any_running() || agents.has_notifications(ROOT_AGENT)`), because the copy would not carry the subagent's in-flight state.
- `/rename [name]`: appends `SessionNamed`. Without an argument the name is derived
  deterministically from the first entry of `ledger.view().user_messages` (first line, truncated
  to 60 chars); with none to
  derive from it is an error. No model call.
- `/export [path]`: writes the conversation as Markdown. The text comes from the rig-memory
  conversation log (the same load `Session::clear` does), so compaction summaries appear as the
  summaries they are: the exporter reads `conversation.entries()` (author and asides) and labels
  a user entry authored by `rig_memory::summary_author` with a COMPACTED aside as a compaction
  summary, and uses a wildcard arm for the `#[non_exhaustive]` `AssistantContent`. After `/clear`
  the export covers only the post-clear conversation. Tool calls, read from the conversation log, are one line each
  (`adjudication::action_summary(tool, &Value)` supplies the text);
  tool results are omitted except their length. Default path is `fontaine-<id8>.md` in the launch
  directory; a relative path resolves against it. The write is `create_new` and owner-only, since
  a transcript may hold secrets: an existing file is an error rather than an overwrite. The path is
  user-typed, so it is not a model tool call and passes no scope validator; the command is not
  reachable by the model.
- Switching: `fontaine_tui::drive` returns `Result<DriveOutcome, String>` instead of `()`, where
  `DriveOutcome` is `Exit` or `Switch(SessionTarget { source, record_resume })`; the result is
  propagated through `fontaine_tui::run` and `run_interactive`. `record_resume` is true for
  `/resume` and false for `/branch`. The TUI resolves and validates the target before returning
  `Switch`, and for `/branch` performs the `Session::fork` itself (`source` is then the copy's id)
  and appends the copy's `Resumed` and `SessionNamed` there, so a failure is shown in the live
  session, which is untouched. After `Switch` the old run is torn down (agents killed, terminal
  restored, grants, mode and transcript gone), so the loop only opens an already-validated session
  with `Session::open`; `record_session_start` writes `Resumed` (`how` = `resume`) only when
  `record_resume` is true, so the copy never gets a second one. Each iteration re-runs
  `prepare_session` with the derived `Cli`, rebuilds the `fontaine.session` span, and redoes
  `models::resolve`, the scanner start and `agent_setup`; the telemetry guard, config load and
  trust decision stay outside the loop. The loop sits in `run` (`fontaine/src/lib.rs`) around the runtime block, because `run`
  owns the telemetry guard, the config load and the trust prompt, and `export.finish` is called
  once; the trust decision and telemetry carry across iterations and each iteration enters a new
  `fontaine.session` span with `start_type` `resume`. `prepare_session`/`open_session` read
  `cli.resume`, `cli.continue_session`, `cli.fork_session` and `cli.session_id`, so each iteration
  builds a derived session selection (a cloned `Cli` with those four overwritten) rather than
  reusing the launch flags: a stale `session_id` would be reused as the fork id (`InUse`) and the
  original `--continue` would be re-applied. Resume semantics are unchanged: default mode, no
  grants, MCP servers and scanner restarted.

```
/resume ─▶ picker ─▶ Enter ─▶ drive returns Switch(src) ─▶ run loop ─▶ Session::open(src) ─▶ drive
/branch ───────────────────▶ Session::fork(src,new) ─▶ drive returns Switch(new) ─▶ Session::open(new) ─▶ drive
```

## Implementation Steps

1. **Core metadata** (`fontaine-core/src/ledger.rs`, `session.rs`): `SessionNamed` record,
   `validate_name`, `SessionInfo`, `list`, `resolve`, `Session::{rename,name}`; rebuild
   `latest_for_root` on `list`. Make `LedgerView` ignore the new record.
2. **CLI** (`fontaine/src/cli.rs`, `lib.rs`): `--name`, `--resume` by id or name, name resolution
   in `open_session`, write the name on create/fork.
3. **Export** (new `fontaine-core/src/export.rs`): conversation → Markdown as a pure function over
   loaded messages, plus the file write.
4. **Picker and commands** (`fontaine-tui`): `resume_picker.rs`, `Command::{Resume,Branch,Rename,
   Export}` rows in `BUILTINS`, handlers under `ui/commands/*.rs` (dispatch at `ui/mod.rs` ~273-317), `pub mod resume_picker;`
   in `lib.rs`, `app.rs` `Action` variants, overlay drawing in `view.rs`, and `screen.rs`/`linear.rs`
   wiring (screen-reader linear mode lists the rows as plain lines, as the model picker does).
   `BUILTINS` must stay sorted by name with unique names and aliases. Replace the "/resume not
   available yet" message in `ui/commands/context.rs`.
5. **Switch loop**: `drive` returns the outcome (callers in `fontaine-tui/tests/common/mod.rs`
   change with it); `run` in `lib.rs` loops around the runtime block with a derived session
   selection per iteration, as in Design. `World::run` and `run_linear` return `drive`'s outcome.
6. **Docs and decisions** (below).

## Files to Modify

- `rust/fontaine-core/src/{ledger.rs,session.rs,lib.rs}`, new `export.rs`
- `rust/fontaine/src/{cli.rs,lib.rs}`
- `rust/fontaine-tui/src/{commands.rs,lib.rs,app.rs,view.rs,screen.rs,linear.rs,ui/mod.rs,
  ui/commands/*.rs}`, new `resume_picker.rs`
- `rust/fontaine/tests/resume.rs`, `rust/fontaine-core` unit tests, `rust/fontaine-tui` tests
  including the shared rig `rust/fontaine-tui/tests/common/mod.rs`
- `docs/tui.md` (~91, ~485, ~507-526), `docs/headless.md` (~36-39, ~75), `docs/index.md` (~29),
  `docs/configuration.md`, `docs/telemetry.md`, `docs/architecture.md`,
  `tasks/design/sessions.md`, `tasks/design/observability.md` (~56),
  `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Switch by relaunching the run loop vs. swapping the session in place.** In-place swap means
  reworking every holder of the ledger and session id. A loop around the existing startup path
  reuses resume exactly as `--resume` does and costs only an MCP/scanner restart.
- **Name in the ledger vs. a sidecar `meta.json`.** The ledger is already append-only and scanned
  for the list; a sidecar would be a second durable record to keep consistent and copy on fork.
- **Deterministic `/rename` default vs. Claude Code's model-generated name.** No extra model call,
  no spend, no prompt-injection surface.
- **Markdown export vs. also JSON.** The raw log is already JSONL on disk; export is for reading.

## Decisions

- 2026-10-26: The `/resume` picker leaves out sessions with no name and no first prompt (every launch writes a start, so they are noise); `--continue` and resolve by id still reach them.
- Picker and `--continue` scope: the current workspace root only, because that is the existing
  `--continue` scope and avoids showing sessions from unrelated projects. Claude Code's all-projects
  toggle is not added.
- 2026-10-26: `/branch` forks in the TUI, which also writes the copy's `Resumed` (`fork (branch)`) and `SessionNamed` at fork time; the loop's `record_session_start` skips `Resumed` for it (`SessionTarget.record_resume` is false).
- `/branch` carries no grants and starts in the default mode, like every resume/fork.
- `/clear` still refuses a name for the old conversation; with `/resume` available its docs point
  there instead of saying "not available yet". This plan supersedes the master plan's 2026-10-24
  `/clear <name>` decision ("until `/resume` exists").
- 2026-10-26: A switch restarts MCP servers and the scanner, like any resume.
- 2026-10-26: `/resume` and `/branch` run only between turns; `/branch` is refused while a subagent runs.
- 2026-10-26: `/export` is owner-only and never overwrites an existing file.
- AFK 2026-10-26: No bare `--resume` picker at launch; the picker is `/resume` in the TUI only (the overlay lives in `drive`, which needs a live session, and an optional `--resume` value would swallow the positional PROMPT). Deviation from Claude Code; issue #10 asks for a TUI picker.
- AFK 2026-10-26: `--resume` keeps its required value, so `fontaine -p --resume <id> "task"` parses unchanged.
- 2026-10-26: `/rename` without an argument is deterministic (no model call), a deviation from Claude Code.

## Documentation

- `docs/tui.md`: new rows for `/resume`, `/branch`, `/rename`, `/export`; rewrite the Resuming
  section and the `/clear` sentence.
- `docs/headless.md`: `--name`, `--resume` by name.
- `docs/configuration.md` (~876): the `--continue`/`--resume`/`--fork-session` row gains `--name`
  (`--resume` takes an id or name). `docs/telemetry.md` (~118): a switch uses `start_type` `resume`.
  `docs/architecture.md` (~184-187): resuming and forking. `docs/tui.md` ~240 and ~278: the
  `/clear` text.
- `tasks/design/sessions.md`: `SessionNamed`, listing and the switch loop.
- Master plan `## Decisions`: dated lines for the deviations above (no all-projects picker,
  deterministic `/rename`).

## Testing Strategy

Unit tests (fakes suffice, no live APIs):
- `validate_name` bounds and control characters; `SessionNamed` last-wins; `LedgerView` ignores it
  (guardian intent unchanged — a security invariant).
- `list`: skips torn/foreign ledgers, filters by root, orders by recency, reports branch parent;
  `latest_for_root` behaves as before; `resolve` id-before-name, case-insensitive, `Missing`,
  `Ambiguous`.
- Export rendering: roles, compaction summary, tool call lines, tool results omitted; refusal to
  overwrite; owner-only mode on Unix.
- Command table: new names/aliases parse, appear in the menu, and the choke-point/menu tests stay
  green.
- Picker: filtering, selection, current session omitted, empty list message, linear rendering.

Integration (`fontaine -p`, `fontaine/tests/resume.rs`): `--name` then `--resume <name>`
reopens it; `--fork-session --name` names the copy only; ambiguous name exits 2 and lists ids;
`fontaine -p --resume <id> "task"` still parses the prompt as the prompt. The TUI switch loop is tested with the existing
`fontaine-tui` test rig by driving `/branch` and asserting the returned `Switch` outcome and the
copy on disk; the rig (`World::run`, `run_linear` in `fontaine-tui/tests/common/mod.rs`) returns
`drive`'s outcome and the test reads the copy's id from it.

## Manual Verification

1. `cargo run -p fontaine`, send a message, `/rename demo`, quit; `cargo run -p fontaine -- -r demo`
   reopens it. Expected: the note names the session, the model recalls the message. Not automated
   because the real terminal draw is the part under test.
2. In a live session run `/branch`, then `/resume`; the picker shows both with the branch marker.
   Expected: choosing the original reopens it unchanged. Not automated: overlay look-and-feel.

## Open Questions

- None.
