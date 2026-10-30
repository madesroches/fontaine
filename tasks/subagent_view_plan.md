# Subagent View Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/321

## Overview

Make subagents work the way they do in Claude Code. A list of agents sits below the status line.
Selecting a subagent there switches the whole screen to that agent's transcript, drawn with the
same components as the main chat. Whatever the human types then goes to that agent: a running
agent reads it at its next tool result, and an agent that has ended is resumed with it. Selecting
`main` switches back, and the main chat is reprinted intact. This replaces the agents panel's flat,
unscrollable detail view (#299). It also fixes the blank main chat after leaving the panel (#296).

## Current State

- **Agents panel.** Ctrl-T, `/agents` and `/tasks` open `AgentsPanel`
  (`rust/fontaine-tui/src/agents_panel.rs`), an overlay above the input with one row per subagent
  and one per background job. Enter opens a `Detail` with three sections built from strings: the
  pending approvals, the gate decisions and a transcript log (`detail_lines`, `:279`). The log
  comes from `SessionState::log_agent_line` (`rust/fontaine-core/src/gate.rs:813`). `run_child`
  (`rust/fontaine-core/src/child_runner.rs:382`) writes it as 200-char summaries of `Assistant`,
  `ToolCall` and `ToolResult` events, capped at `AGENT_LOG_LINES`. The decisions come from
  `decisions_by_agent` (`gate.rs:772`). The driver fills the detail on every draw
  (`rust/fontaine-tui/src/ui/mod.rs:58`, `refresh_panel`). Nothing in the detail scrolls, and the
  panel holds the keyboard while it is open (`app.rs:693`).
- **Child runs.** `CoreChildRunner::spawn` (`child_runner.rs:272`) builds an `Agent` with its own
  `MemoryStore` and moves it into a `tokio::spawn`ed `run_child`. That function calls
  `turn::drive` (with `feed: None`) and an `on_event` closure, which sees every `SessionEvent`
  except decisions and previews (deltas, calls, results, usage) but keeps only the log strings.
  A child's decisions go through `SessionState::publish` (`gate.rs:769`) into the session-wide
  `DecisionFeed`, which only the root turn loop drains, so they appear in the root transcript. When the run ends, the `Agent` and its
  store are dropped. `ChildRun` (`:216`) holds status, a kill switch and a one-shot report
  `watch`. Nothing can continue a child after its report.
- **Report routing.** `AgentTool::call` (`rust/fontaine-agents/src/spawn.rs:173`) awaits the
  report of a foreground child inline. For a background child it spawns a task that queues
  `notify_report` to the parent, then calls `conclude` → `AgentTree::finish`. That function marks
  the node ended and kills its running descendants. `AgentTree::deliver` (`tree.rs:613`) refuses a
  target that is not running.
- **Human input mid-turn.** `Steering` (`rust/fontaine-core/src/steering.rs`) queues what the human
  types while the root works. `SteeringHook` appends the leading plain messages to the root's next
  tool result with the "[The user sent this message while you were working…]" wording. Only then
  does it append `Record::UserMessage` to the ledger, so the guardian judges calls only by words
  the model has read. The hook is installed on the root agent only (`agent/build.rs:266`), and
  `steer` returns `Proceed` for any other agent (`steering.rs:121`).
- **Messages from agents.** `send_message` and background reports reach a child through its tree
  inbox. `NotificationStage` (`child_runner.rs:47`) appends them to its next tool result before
  the scanner runs, rendered as untrusted data.
- **Rendering.** The TUI draws inline (`rust/fontaine-tui/src/screen.rs`). Settled transcript
  rows are written once into terminal scrollback, and only the live area is redrawn. `Transcript`
  (`rust/fontaine-tui/src/transcript.rs`) keeps every entry plus a commit cursor
  (`committed`, `committed_chars`). `Transcript::on_event` turns `SessionEvent`s into entries.
  `Ui::on_event` (`ui/turn.rs:691`) adds the guardian-denial line and the task list refresh.
  `linear.rs` (the screen-reader renderer) settles through the same `commit_entries`.
- **Why the main chat goes blank (#296).** `Screen::draw_frame` (`screen.rs:358`) commits enough
  transcript rows that `held + chrome` fits the terminal. `chrome` includes the overlay
  (`view.rs:47`), so a tall agents panel pushes the live transcript into scrollback and grows the
  viewport to the whole screen. When the panel closes, `wanted = content.max(free)` keeps the
  viewport at full height. The rows the panel took become padding, and the last messages are only
  in scrollback above it.
- **Claude Code 2.1.314** (observed in a 120×40 tmux pane, plus the sub-agents, keybindings and
  interactive-mode docs):
  - A panel below the status line lists `● main` and one row per subagent (type, description,
    elapsed time, tokens). Down on the prompt moves focus into it. ↑/↓ select a row, and the hint
    reads `Enter to view · x to stop`. Esc returns focus to the prompt and does not leave the view.
  - Enter on a subagent clears the screen **and the scrollback** and reprints the agent's
    transcript: its task prompt as the first user message, then its replies and tool calls. The
    input box's top border carries the agent's description on the right, and the placeholder reads
    `Message @general-purpose…`. The panel marks the viewed row `●`.
  - Text typed there is added to the agent's transcript as a user message. An agent that had
    ended its turn is resumed with it. The main transcript gets `Agent "…" was resumed by the
    user`, and the main model later receives the report as a notification.
  - Selecting `main` + Enter clears again and reprints the main conversation, which then holds no
    trace of the agent's lines.
  - Docs: in an agent's view, follow-up messages and skills go to that agent and built-in commands
    go to the main conversation. A done agent's row leaves the panel at once, and a failed or
    stopped row leaves after 30 s. Ended agents stay listed in `/tasks`. x stops the selected agent.

## Design

### Overview

```
                        ┌──────────── fontaine-core ─────────────┐
 child drive() ──event──▶ AgentEventSink (installed by the TUI)  │──▶ App.agent_views[id].transcript
                        │                                        │
 TUI Enter on agent ──▶ App.viewing = Some(id) ──▶ Screen::reprint (clear screen + scrollback,
                        │                            recommit shown transcript from row 0)
 TUI typed text ──────▶ AgentRuntime::message_agent(id, text)
                        │   running  → Steering::queue_for(id)  → SteeringHook on child's next result
                        │   ended    → CoreChildRunner::resume(id, text) → new run, same store
                        └────────────────────────────────────────┘
```

### 1. Every child event reaches the TUI (core)

- Add `SessionState::set_agent_event_sink(UnboundedSender<(String, SessionEvent)>)` and a
  private `emit_agent_event(id, event)`. With no sink installed (headless, evals), events are
  dropped. The TUI installs the sink where it builds or receives the runtime (`lib.rs`).
- `run_child`'s `on_event` sends the events it sees to the sink, deltas included, so the view
  streams. It keeps its last-text and budget logic.
- Decisions and previews are emitted where they are published: `SessionState::publish` routes
  records with `agent_id != ROOT_AGENT` to `emit_agent_event` as `Decision` events and keeps them
  off the root feed. `publish_preview` (`gate.rs:785`) gains an `agent` argument (in scope at
  `permissions.rs:1884`) and routes the same way. When no sink is installed (headless), child
  records stay on the root feed as today.
- Delete `log_agent_line`, `agent_log`, `agent_logs`, `AGENT_LOG_LINES`, `decisions_by_agent` and
  `decisions_of`. The transcript shows decisions as verdict lines, so nothing reads them any more.
  Port the `agent_pipeline.rs:207` assertion to the sink: each child's `Decision` events arrive
  under its own id, and none reaches the root feed.

### 2. Per-agent transcripts in the TUI

- `App` gains `agent_views: HashMap<String, AgentView>`, where `AgentView { transcript: Transcript }`,
  and `viewing: Option<String>` (`None` is the main conversation).
- `App::shown()` / `shown_mut()` return the transcript on screen. `Screen::draw_frame`,
  `view::height`, `view::draw` (via `draw_transcript`) and `linear.rs` read the transcript through
  these instead of `app.transcript`. Only the shown transcript commits rows. A hidden transcript
  keeps its entries and its commit cursor.
- The sink receiver is polled by a new arm in the idle loop's `select!` (`ui/mod.rs`) and in the
  turn loop's `select!` (`ui/turn.rs`). The driver feeds each event into that agent's transcript. Factor
  `Ui::on_event`'s guardian-denial and todo handling into
  `fn apply_event(&mut Transcript, runtime, event)` so both paths share it. The task-list refresh
  stays root-only.
- The view's first entry is the agent's task prompt as a `User` entry, as in Claude Code. `run_child`
  sends the task prompt to the sink itself, as the first message of the view (a dedicated sink
  message, not a `SessionEvent`), before calling `drive`. `drive`, `SessionEvent` and the root
  paths are unchanged.

### 3. Switching the screen

- `Transcript::recommit()` sets `committed = 0`, `committed_chars = 0` and drops `retired`, so the
  next render writes the whole transcript into scrollback again.
- `Screen::reprint()` erases the visible screen and the scrollback (`CSI 2J`, `CSI 3J`, cursor
  home), then rebuilds the inline viewport at full height with `repaint = true`. Erase in this
  order: `CSI 3J` after `CSI 2J` drops whatever tmux moved into history when the screen was cleared
  (the gotcha `erase_rows` documents).
- `App::view(target: Option<String>)` only sets `viewing` and `app.reprint = true`.
  `Screen::render` consumes the flag before it commits: `recommit()` on the shown transcript,
  `reprint()`, then the normal frame. `linear.rs` ignores the flag, so only the inline renderer
  resets the cursor.
  `Linear` gets a last-printed-view field compared to `app.viewing` on each render (like its todos
  tracking); on a change, `linear.rs` prints `viewing agent-3 (worker)` or `back to the main
  conversation`, then only the entries of the shown transcript it has not printed yet. It takes
  and discards `app.reprint` without recommitting. A screen reader gets no duplicate history.
- Like Claude Code, a reprint drops what was in the terminal's history before the session started,
  plus the main chat's rows from before a `/clear`.
- **#296.** Closing the agents panel calls `App::view(None)` when the screen committed rows while
  the panel was open. The `committed_under_overlay` flag lives on `App`: `Screen::draw_frame`, which already
  gets `&mut App`, sets it when `commit_within` releases rows while `app.agents` is open, and the
  `PanelKey::Close` arm reads and clears it before calling `App::view(None)`. The main chat is then reprinted in full, so nothing hides above
  the padding. Approvals and the other dialogs keep today's behavior. They are not browse views,
  and a reprint after every answer would clear history for routine prompts.

### 4. The agent list below the status line

- New `rust/fontaine-tui/src/agent_list.rs`: `AgentList { focused: bool, selected: usize, dismissed: HashSet<String> }` plus
  `fn rows(tree rows, viewing, now) -> Vec<ListRow>`. It shows nothing when the tree has no
  children. Otherwise it shows `main` first, then each child as indent + id + type + status +
  elapsed + `n tok` (the same spend text as the panel: `spend_text`, `agents_panel.rs:85`, becomes `pub(crate)` over an
  input both callers can build instead of `&PanelRow`). The viewed row
  is marked `●` and the rest `◯`; the focused selection is reversed. Rows shown:
  - every running child;
  - the viewed child;
  - a child that ended `failed`, `killed` or `budget-exhausted`, until 30 s after it ended or until
    x dismisses it (Claude Code: select it and press x);
  - a `done` child, which drops off at once (Claude Code).
  `AgentRow` gains `started` (from `Node.started`) and `ended: Option<Instant>` for the 30 s window.
  `Node` gains `ended: Option<Instant>`, set in `finish` (including its orphan-kill loop) and in
  `kill` for the target and its descendants.
  Elapsed counts from the latest run's start and resets on resume.
- Keys:
  - Down on the last line of the prompt, when not navigating history, no slash or `@` completion menu is open and the list is non-empty,
    focuses the list.
  - With the list focused, ↑/↓ move, and ↑ on the first row returns focus to the prompt.
  - Enter views the selected row (`main` or an agent).
  - x stops a running agent through the existing `Action::KillAgent`. On an ended row it adds the
    id to `dismissed`, which `rows()` filters. It does nothing on `main`
    or on the viewed agent's row, where it types into the prompt (Claude Code).
  - Esc returns focus to the prompt.
  - Any printable key returns focus to the prompt and types there.
- `view::chrome` and `view::draw` add the list's rows below the status line. The agent list takes its rows first from the cap `todo_lines` computes, and
  the task list gets the remainder; the list's rows count in `chrome` and `body_rows`. When the list
  shrinks it leaves a small padding gap, as the task list does. The hint line above the rows reads `↑/↓ select · Enter view ·
  x stop · Esc back`.
- The agents panel stays as the full overview: every agent, ended ones included, plus the
  background jobs and budgets. Enter on an agent row closes the panel and views that agent, which
  replaces `Detail`. Delete `Detail`, `detail_lines`, `is_inspecting`, the `open` state and the
  detail half of `refresh_panel`. This is how an ended agent that has left the list is reached
  (Claude Code's `/tasks`).

### 5. The agent view

- **Header.** The input box's top border shows `agent-3 · worker · running` on the right
  (`draw_input`). The placeholder reads `Message agent-3…`. The status line's spinner and state
  show the viewed agent's status, not the main turn's.
- **Approvals** stay global. Any agent's prompt opens over whichever view is shown, and it already
  names the agent (`approval.rs:225`). Nothing can hide behind a view.
- **Scrolling.** The view lives in terminal scrollback like the main chat, so the wheel and tmux
  copy mode scroll it (#299).
- **Esc** closes palettes as usual. It never interrupts the main turn while an agent is viewed,
  because the human cannot see that turn, and the double-Esc rewind is off.
- **Input routing** (`App::enter` when `viewing.is_some()`):
  - plain text → `Action::MessageAgent { id, text }`;
  - a built-in command runs as today, against the main conversation, unless it is in the refused
    list below; `/expand` and `/copy` act on the shown transcript, while `/export` stays the main
    session export;
  - commands that change the main conversation (`/clear`, `/compact`, `/rewind`, `/branch`,
    `/resume`, `/model`, `/goal`, `/loop`), skills and `!` lines are refused with a notice in the
    view: `switch back to main to run this (↓ then Enter on main)`. The text stays in the input.

### 6. Messaging a subagent (core)

`AgentRuntime::message_agent(&self, id, text) -> Result<Delivered, MessageRefusal>`:

- **Running** (`tree.is_running(id)`) → `steering.queue_for(id, text)` → `Delivered::Queued`.
  - `Steering` gains a per-agent map `agents: HashMap<String, AgentQueue { queued, delivered }>`.
  - Install `SteeringHook` on every child too, in `CoreChildRunner::spawn` after `apply_common`.
    `ChildContext` (built in `agent/setup.rs`) gains a `steering` field to carry the `Steering`.
    `Steering` is created before `setup::subagent_support(...)` (`agent.rs:262` today, after it)
    and passed in.
  - `steer` drains the child's queue for that agent with the same wording and
    `Record::UserMessage` as the root's. Only the root path keeps the `Queued` variants and
    `hold`.
  - `CoreWake::pending` (`child_runner.rs:80`) checks steering only for `ROOT_AGENT`, so a child
    parked in a long wait (`job_output`, agent wait) would not wake. `Steering` gains a per-agent
    `has_deliverable_message_for(agent)`, and `CoreWake::pending` checks it for every agent.
  - The view shows a queued message dimmed under the transcript until delivery. The driver then
    moves it into the agent's transcript as a `User` entry (`take_delivered_for(id)`, drained
    before that agent's next sink event).
- **Ended `done` or `failed`** → `CoreChildRunner::resume(id, text)` → `Delivered::Resumed`.
  Refused with the budget-spent notice whenever `exhausted_of(id).is_some()`, whatever the status.
- **Ended `killed` or `budget-exhausted`** → `MessageRefusal::Ended(status)`. The notice reads
  `agent-3 was killed; it cannot be resumed`, or `…its budget is spent…`.
- **Unknown id** → `MessageRefusal::Unknown`.

**Resume** (`fontaine-agents` + `fontaine-core`):

- `ChildRun` keeps the built `Agent`, its `ChildModel`, its kill channel and a `watch` of
  `(run: u64, Option<ChildReport>)`. `report()` resolves with the report of the run that is
  current when it is called.
- `AgentTree::resume(id) -> Result<(), Refusal>` re-checks `max_parallel` and the session budget,
  sets the node back to `Running`, clears `report` and `ended`, resets `Node.started =
  Instant::now()`, and pokes `changed`. A refusal
  becomes `MessageRefusal::Limit(…)`, shown in the TUI as a human-facing notice, not the spawn
  tool's model-facing refusal text.
- `ChildHandle::resume(&self, prompt: String)` first sets `ChildRun.status` back to `Running`
  (`Node::live_status` reads it while the node runs, and `run_child` writes it only at run end),
  then starts a new `run_child` on the same `Agent`, so
  the same `MemoryStore` and conversation continue. The new run's report replaces the old one.
- The resumed run always reports back as a background child does. Factor the background branch
  of `AgentTool::call` (`spawn.rs:277`) into `pub fn watch_report(tree, ledger, parent, id,
  type_name, handle)`, which both paths call. The report goes to the parent's inbox. When the
  parent has ended, it goes to the root's (`post_to_root` with `NotificationKind::Report`). An
  idle root then gets a turn of its own, as for any background report.
- The ledger gets `AgentEvent::Resumed { agent }` → `Record::Agent { event: "resumed" }`, with an
  arm in `LedgerWindow`'s mapping (`child_runner.rs:95-110`). Then the
  human's text is appended as `Record::UserMessage` and becomes the prompt of the new run.
- The main transcript gets the notice `you resumed agent-3`. The resumed child keeps its
  `own_mode` and its tool set. Every call still goes through the floor and the gate, and
  `posture` is computed per call against its parent. The resumed run inherits nothing from
  `/guardian-disable`, since that state is root-only and unchanged.
- **A message that misses its run.** When a run ends with that agent's queue still holding human
  messages (it answered without another tool call), `run_child`'s caller takes them and resumes
  the agent with them joined. This is Claude Code's "reads your message once that finishes". It
  runs only after the old run's conclude/`finish` has completed, in the shared `watch_report`
  path, so a late `finish` cannot mark the new run ended, kill its children or overwrite its
  report, and `resume`'s `max_parallel` check does not count the old run. `report()` for a
  foreground parent resolves with the first run's report.

### Security

- A message typed into an agent view is the human's own words. It enters the ledger as
  `UserMessage` only when the model reads it, the same as steering. It never goes through the tree
  inbox, so it is never framed as untrusted agent data, and the injection scanner never scans it.
- Resume never widens anything. The tool set, `own_mode`, budget caps (cumulative across runs),
  `max_parallel` and the session budget all hold. A killed or budget-exhausted agent, or one whose budget is spent whatever its status, cannot be
  resumed. Headless runs have no view, and `send_message` keeps its "target not running" refusal.
- `/rewind` drops later `UserMessage` records from the guardian's intent, those typed to agents
  included. That direction is stricter, never looser.

## Mockups

- `tasks/subagent_view_mockups/states.html` shows three terminal states side by side: the main
  chat with the agent list focused, an agent's view mid-run with a queued message, and the refusal
  notice for a main-only command. There is one design direction, the one Claude Code ships.

## Implementation Steps

1. **Core event sink.** Add the sink on `SessionState`, send from `run_child` (prompt first), and
   route child decisions and previews to it in `publish` / `publish_preview`. Update `agent_pipeline.rs`.
2. **Per-agent steering.** Add the `Steering` per-agent queues and install `SteeringHook` on
   children. Add `queue_for`, `take_delivered_for` and `take_pending_for` (for the
   missed-run case).
3. **Resume.**
   - `fontaine-agents`: `ChildHandle::resume`, `AgentTree::resume`, `ended` on rows, and
     `watch_report` factored out of `spawn.rs`.
   - `root_ledger`: `AgentEvent::Resumed`.
   - `fontaine-core`: `ChildContext` gains a `steering` field set in `agent/setup.rs`, with `Steering` created before `subagent_support`; `ChildRun` keeps its agent and run generation, `CoreChildRunner` resume, and
     the missed-message resume.
   - Add `AgentRuntime::message_agent` with `Delivered` / `MessageRefusal`.
4. **TUI transcripts.**
   - Add `AgentView`, `agent_views`, `viewing`, `shown()`/`shown_mut()` and `apply_event`.
   - Drain the sink in `ui/mod.rs` and `ui/turn.rs`.
   - Point `screen.rs`, `view.rs` and `linear.rs` at `shown()`.
5. **Reprint.** Add `Transcript::recommit`, `Screen::reprint`, `App::view` + the `reprint` flag,
   and the linear header lines. Add `committed_under_overlay` for the #296 path.
6. **Agent list.** Add `agent_list.rs`, the keys in `App::handle_key` / `message_key`, layout in
   `view.rs`, and `screen.rs`'s chrome.
7. **Agent view.** Add the header and placeholder in `draw_input`, the status line, the input
   routing and refusal notices in `App::enter`, and the Esc rules. Add `Action::MessageAgent`,
   handled in both driver loops, which shows `Delivered` / `MessageRefusal` notices.
8. **Agents panel.** Enter → `PanelKey::View(id)`. Delete the `Detail` path, then remove
   `agent_logs` / `decisions_by_agent` / `decisions_of` / `log_agent_line` and their accessors,
   which `refresh_panel` used until now.
9. **Docs and Decisions** (below).

## Files to Modify

- `rust/fontaine-core/src/gate.rs` — event sink; remove agent logs and per-agent decisions
- `rust/fontaine-core/src/child_runner.rs` — sink, steering hook on children, `ChildContext.steering`, `CoreWake`, `LedgerWindow` arm for `Resumed`, resume, missed-message resume
- `rust/fontaine-core/src/agent/setup.rs` — set `ChildContext.steering`
- `rust/fontaine-core/src/steering.rs` — per-agent queues
- `rust/fontaine-core/src/agent.rs` (or `agent/` module owning `AgentRuntime`) — `message_agent`
- `rust/fontaine-core/tests/agent_pipeline.rs` — sink assertion, resume and steering tests
- `rust/fontaine-agents/src/runner.rs` — `ChildHandle::resume`
- `rust/fontaine-agents/src/tree.rs` — `resume`, `ended`
- `rust/fontaine-agents/src/spawn.rs` — `watch_report`
- `rust/fontaine-agents/src/root_ledger.rs` — `AgentEvent::Resumed`
- `rust/fontaine-tui/src/agent_list.rs` (new), `lib.rs` — module, sink install
- `rust/fontaine-tui/src/app.rs` — views, list focus, routing, `Action::MessageAgent`
- `rust/fontaine-tui/src/transcript.rs` — `recommit`
- `rust/fontaine-tui/src/screen.rs` — `reprint`, overlay-commit flag, shown transcript
- `rust/fontaine-tui/src/view.rs` — agent list rows, input header, shown transcript
- `rust/fontaine-tui/src/linear.rs` — shown transcript, view headers
- `rust/fontaine-tui/src/statusbar.rs` — viewed agent's state
- `rust/fontaine-tui/src/agents_panel.rs` — Enter views; detail removed
- `rust/fontaine-tui/src/ui/mod.rs`, `ui/turn.rs` — sink drain, `apply_event`, `MessageAgent`
- `docs/tui.md`, `docs/terminal.md`, `docs/agents.md`, `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **Alternate screen for the view** instead of a reprint: switching back would restore the main
  screen for free, but #70 rejected the alternate screen because it breaks tmux selection and wheel
  scrolling, which the view needs most. Claude Code reprints.
- **Fixing every overlay's commit-into-scrollback** (draw overlays over the transcript instead of
  committing): this breaks the invariant that the visible tail never hides rows that are not in
  scrollback, which the approval prompt relies on. Reprinting only after the browse panel is
  narrower.
- **Human message via the tree inbox** (as `send_message` does): reusing it would frame the human's
  words as untrusted agent data and run them through the scanner. Per-agent steering keeps the
  root's semantics.

## Decisions

- 2026-10-30: This plan is drafted with Claude Code 2.1.314's observed behavior as the reference.
  The deviations below are recorded in the master plan's Decisions when the change ships.
- 2026-10-30: Deviation — skills, `!` lines and conversation-changing commands typed in an agent
  view are refused with a pointer back to main (Claude Code sends skills to the agent and asks to
  confirm `/clear`/`/compact`/`/rewind`); reason: no child-side skill path exists, and a refusal
  cannot act on a conversation the human is not looking at.
- 2026-10-30: Deviation — Esc never interrupts the main turn while an agent is viewed; reason: the
  human cannot see what it would stop.
- 2026-10-30: Deviation — a killed or budget-exhausted agent cannot be resumed by typing into its
  view (Claude Code lets the user resume a stopped row within 30 s); reason: the human's kill and
  the budget cap stay final.
- 2026-10-30: The mockup in `tasks/subagent_view_mockups/states.html` was approved by the maintainer.
- 2026-10-30: Ended agents leave the list as in Claude Code (done at once, others after 30 s) and
  stay reachable from the agents panel.

## Documentation

- `docs/tui.md`:
  - rewrite "The agents panel" around the agent list and the agent view: selecting, viewing,
    messaging, resuming, switching back, what a reprint drops, and refused commands;
  - update the Keys table rows for Down (`tui.md:94`), Enter, x, Esc and Ctrl-T, and the
    `/agents` row in the Commands table (`tui.md:322`).
- `docs/terminal.md`: in "## Screen-reader mode", describe the `viewing …` / `back to …` lines.
- `docs/agents.md`: rewrite "Watching and stopping subagents" (~`:192-195`) around the agent list
  and view; messaging a subagent from its view, resume rules (which statuses, budgets
  carry over, report goes to the parent or main), and the "resumed" ledger event.
- `tasks/fontaine_harness_plan.md` `## Decisions`: the dated deviation lines above.
- `CHANGELOG.md`: entry under the unreleased section.

## Testing Strategy

Unit (TUI, `TestBackend` and the `screen.rs` fake terminal with scrollback):

- `agent_list`:
  - row selection (`main` first, nested indent);
  - done rows drop at once, while failed/killed/budget rows stay 30 s (inject `now`);
  - x on a running row vs on `main` or the viewed row;
  - Down focuses only on the prompt's last line when not navigating history;
  - printable keys return focus.
- `App::enter` routing in a view: plain text → `MessageAgent`; the refused commands, skills and `!`
  lines leave the text in the input and push the notice; UI commands still run. Esc in a view
  does not return `Interrupt`.
- `Transcript::recommit` re-emits every entry, and a hidden transcript commits nothing.
- **Regression for #296 (pins the reported bug):** in the `screen.rs` fake terminal,
  1. fill the main transcript;
  2. open the agents panel with a body tall enough to force commits;
  3. close it;
  4. assert the last main message is on the visible rows, not only in scrollback.
- `Screen::reprint`: after viewing an agent, the fake terminal's scrollback holds only the agent's
  rows. After viewing main again, it holds only main's rows, including entries that arrived while
  hidden. Assert the `CSI 2J` + `CSI 3J` bytes.
- `linear.rs`: switching prints the header and only unprinted entries; returning to main prints
  no duplicates.
- Agents panel: Enter on an agent row yields `PanelKey::View(id)`.
- A queued message to the viewed agent renders dimmed, then as a user line once delivered.

Unit / integration (core, `rig_core::test_utils` deterministic models, no live APIs):

- Sink: a child's task prompt, `ToolCall`, `Decision` and `Assistant` events arrive under its id
  (run a real child, or call `publish` with a child agent id; not `decisions_of`). With no sink
  installed, nothing blocks or grows and child records stay on the root feed.
- Per-agent steering:
  - a message queued for a running child is appended to its next tool result with the user
    wording;
  - `Record::UserMessage` is appended only then;
  - the root's queue is untouched, and a child's queue never steers the root.
- Resume:
  - a done child resumed with text continues the same conversation (the scripted model sees the
    earlier turns);
  - status goes back to `running`;
  - the new report reaches the parent's inbox, or the root's when the parent has ended;
  - `Resumed` and `UserMessage` land in the ledger in that order;
  - spend accumulates against the same cap;
  - killed and budget-exhausted children are refused, and so is a `done` or `failed` child whose
    budget is spent;
  - a resume at `max_parallel` is refused with `MessageRefusal::Limit`.
- A message queued for a child parked in a long wait wakes it (`CoreWake::pending`).
- A message queued for a child that answers without another tool call resumes it with that
  message.
- Mode invariant: a resumed child's posture is never looser than its parent's. Reuse the
  existing posture test with a resumed child.

The choke-point test stays green (no new tool).

## Manual Verification

Terminal layout bugs must be reproduced in a real tmux (`tasks/design/testing.md`, "Reproducing
terminal layout bugs"). `TestBackend` does not model tmux's history handling of `CSI 2J`/`3J`.

1. `tmux new -s fv -x 120 -y 40 'cargo run -p fontaine'`. Ask for a background `worker` that runs
   `sleep 60` then reads a file.
   - Expected: the agent list appears below the status line with `● main` and the agent row.
2. Down, Down, Enter.
   - Expected: the screen shows only the agent's transcript, starting with its task prompt, and
     the input border names the agent.
   - `tmux capture-pane -p -S -500` holds none of the main chat's lines.
3. Type `also count the lines` and press Enter while the agent runs.
   - Expected: the agent reads it and answers.
4. Wait for the agent to finish, then type `and the words` and press Enter.
   - Expected: the agent resumes, and the main chat later shows `you resumed agent-…` and the
     report turn.
5. Scroll with the mouse wheel and tmux copy mode.
   - Expected: the agent's full transcript is reachable.
6. Down to `main`, Enter.
   - Expected: the main chat is reprinted with its last message on screen, and the pane history
     holds no agent lines.

## Open Questions

- None blocking. Row descriptions wait on #114/#290. Until then a row shows id + type, and the
  header uses the same.
