# Android Agent Switching and TUI Parity Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/746

## Overview

The Android app mirrors only the main conversation, so a session that spawns subagents is half
usable from a phone. This plan (1) publishes the agent tree and every agent's transcript over the
remote protocol and lets the app list agents, switch between them, message them and stop them, and
(2) audits the TUI's features against the app and closes the gaps that are safe for a device, with
each deliberately kept gap recorded in `## Decisions`. Authority does not change: devices still send
only prompts, slash lines, answers and interrupts, and the host floor, guardian and device-command
list apply unchanged.

## Current State

- Host mirror: `rust/fontaine-tui/src/remote/event.rs`. `MirrorTracker::diff` compares the host
  `App` with what was last published and emits `RemoteEvent::{Set, Append, Truncate, Status,
  Dialog}`; `Snapshot { entries, status, dialog }` is cut into parts by `fontaine-remote`
  (`protocol.rs`, `SNAPSHOT_PART_CAP`). Entries come from `app.transcript` only
  (`Transcript::mirror_entries`), i.e. the main conversation. Subagent transcripts live in
  `App::agent_views: HashMap<String, AgentView>` (`app.rs`) and are never mirrored.
- `Status` carries `running_agents` (a count) and `todos`, nothing per agent. The tree is
  `fontaine_sdk::agents::tree::AgentRow` (id, name, parent, type, model, effort, status, depth, spend,
  idle, writing) rendered for the TUI by `agent_list.rs::rows` / `ListRow`.
- `Dialog::Approval`/`Question` already carry `agent` and `agent_name`; the app shows `agent_name`
  in the approval title. Answers are by dialog id, so approvals from any agent already work.
- Wire (`protocol.rs`): `ClientMsg::{Hello, Prompt, ApprovalAnswer, QuestionReply, Interrupt, Slash,
  Ping}`. Nothing addresses an agent. TUI-side an agent view sends via `Action::MessageAgent`
  -> `AgentRuntime::message_agent` (`ui/mod.rs`) and stops via `kill_agent`.
- Device slash list: `Command::device_allowed` (`commands.rs`), an exhaustive match. Allowed:
  `/btw /cost /recap /compact /context /goal /init /rename /help /status` and skills. `/agents`,
  `/todos`, `/transcript`, `/denied`, `/resume`, `/model` etc. are host-only.
- Android app: `rust/fontaine-android` portable `model.rs` (lenient mirror), `app.rs` (state machine,
  `Action`), `android/ui.rs` (ImGui). It renders transcript, status line, dialog, prompt field and
  interrupt; it ignores `Status.todos`, `queued`, `goal`, background counts and the entry kinds
  `Reasoning`, `Shell`, `Attachment`, `Expanded` (shown as placeholders). A contract test in
  `fontaine-tui/src/remote/event.rs` decodes real host values with `fontaine_android::model`.
- `docs/remote.md` ("What a device sees and can do") and `docs/android.md` describe the device surface.

## Design

### 1. Agent tree and per-agent transcripts on the wire

Mirror every agent through the existing broadcast mirror (one publish for all devices, no per-device
host state):

- `Status` gains `agents: Vec<AgentSummary>` (`#[serde(default)]`): `id`, `name`, `parent`,
  `agent_type`, `model`, `status` (a string label from `agents_panel::status_label`/`row_label`),
  `depth`, `elapsed_secs`, `spend` (the text `spend_text` already builds), `idle`, `writing`. Built
  from the same `agent_list::rows` output the TUI lists, so listing rules (running, viewed, task
  owner, recently failed) are shared (DRY) and the main agent is row 0 with `id = None`/"main".
- `Snapshot` gains `agent_entries: BTreeMap<String, Vec<Entry>>` (`#[serde(default)]`), keyed by
  agent id, from `App::agent_views`. Each agent's list is capped to its last N entries
  (`AGENT_ENTRIES_CAP`, 200) and the snapshot builder keeps newest agents first so the total stays
  inside the existing multi-part snapshot limits; an agent trimmed this way is flagged
  (`AgentSummary.clipped`).
- `RemoteEvent::{Set, Append, Truncate}` gain `agent: Option<String>` (`#[serde(default,
  skip_serializing_if = "Option::is_none")]`; `None` is main, so the existing encoding is
  byte-identical). `MirrorTracker` holds one `Vec<Entry>` plus change counter per agent and runs the
  existing `diff_entries` per transcript (parameterised by transcript, not copied). A removed agent
  view (`/clear` drops them) emits `RemoteEvent::AgentGone { agent }`.
- Dialogs need no change; the app uses `Dialog.agent` to badge the agent row that is waiting.

### 2. Addressing an agent

Two new `ClientMsg` variants, both audited and routed through the same choke point as `Prompt`:

- `AgentMessage { message_id, agent, text }`: the surface calls the existing
  `AgentRuntime::message_agent` path via a new `DeviceInput::AgentMessage` handled by the interface
  loop (the runtime is not reachable from the network task), tagged with the sender like remote
  prompts. Same size cap as `Prompt`; replies `Accepted`/`PromptRejected` (an unknown or finished
  agent that cannot be resumed is `Reason::Malformed`; the refusal text goes to the device as a
  notice).
- `StopAgent { agent }`: queued as `DeviceInput::StopAgent`, applied with `kill_agent`, audited
  (`agent_stop`). Stopping `main` is not offered: main is interrupted with `Interrupt`.

`Interrupt` stays main-only. The protocol `VERSION` stays 1: all additions are `serde(default)`
fields or new message variants; an older host answers a new variant as `Malformed`, and the app
shows "this host does not support agent control; update it" instead of failing silently.

### 3. App side

- `model.rs`: `AgentSummary`, `Status.agents`, `Snapshot.agent_entries`, `agent` on the event kinds,
  `RemoteEvent::AgentGone`; all lenient (unknown fields ignored).
- `app.rs`: `viewing: Option<String>` plus `entries_by_agent`; the transcript renderer and the
  prompt send read the viewed agent. Sending while viewing a subagent emits `ClientMsg::AgentMessage`;
  while viewing main it emits `Prompt`/`Slash` as today (slash lines are main-only: while a subagent is
  viewed the field says "switch to main to run commands", mirroring `app::MAIN_ONLY`). A `Stop agent`
  action emits `StopAgent`. If the viewed agent disappears the view falls back to main.
- `android/ui.rs`: an agents bar below the status line (hidden while only main exists) listing rows as
  chips/a collapsible list with name, status, model, spend; the row with a pending dialog is badged;
  tap switches; the viewed agent's header shows status and a Stop button. Interrupt button stays for
  main. Layout logic lives in `layout.rs` (portable, unit-tested) like the existing layout.

### 4. Parity audit

Each TUI feature against the app; "close" means this plan implements it, "keep" is recorded in
Decisions.

| TUI feature | Device today | Action |
|---|---|---|
| Transcript: user, assistant, tool, notice, error | rendered | none |
| Reasoning, Shell, Attachment, Expanded entries | placeholder | close: render reasoning (collapsed, tap to expand), shell rows (command + output), attachment as a labelled line |
| Approvals and questions (single-select) | works | none |
| Multi-select question | refused by host | keep (decided in `android_app_plan`): answered at the host |
| Agent list, view, message, stop | missing | close (sections 1-3) |
| Todo list / task board (`/todos`, status `todos`) | not shown | close: read-only todo panel from `Status.todos` |
| Queued messages (`Status.queued`) | not shown | close: a "queued" line list above the field |
| Goal, scheduled, background shells/monitors | not shown | close: status chips from existing `Status` fields |
| Interrupt | works (main) | none |
| Slash commands on the device list | sent as `Slash` | close: show the host's reply notices; add a command picker listing exactly the device-allowed names (`/help` text) so the user need not remember them |
| `/agents` panel (jobs, board, kill job) | host-only | closed in effect by the agents bar for agents; background job kill stays host-only (keep) |
| Session picker `/resume`, `/branch`, `/rewind`, `/clear` | host-only | keep: they replace or rewrite the session the device is attached to and the host list is not device-allowed |
| `/model`, `/effort`, `/config`, `/permissions`, `/mcp`, `/hooks`, `/plugin*`, `/login`, `/theme`, `/output-style`, `/guardian-model`, `/add-dir`, `/export`, `/copy`, `/memory`, `/vim`, `/keybindings`, `/bug`, `/exit`, `/loop` | host-only | keep: they change permissions, credentials, host files or host UI; unchanged `device_allowed` |
| Permission mode changes (Shift+Tab), `/afk` | host-only | keep: a phone cannot loosen the mode of an unattended host |
| `/guardian-disable`, `/guardian-enable`, `/remote*` | refused | keep (security invariant) |
| Elicitations, trust prompts, usage pause, pairing | "waiting on the host" | keep |
| Image paste, `@` file mention, editor handoff | host-side | keep: no host filesystem or image path from a device |

A test pins the table: the exhaustive `device_allowed` match is unchanged (its existing test stays),
and a new test asserts the app's slash picker list equals the commands for which `device_allowed`
is true.

## Implementation Steps

1. **Wire types** (`rust/fontaine-remote/src/protocol.rs`): `ClientMsg::AgentMessage`,
   `ClientMsg::StopAgent`; round-trip tests; docs constants reuse `MAX_MESSAGE_BYTES`.
2. **Host mirror** (`rust/fontaine-tui/src/remote/event.rs`): `AgentSummary`, `Status.agents`,
   `Snapshot.agent_entries`, `agent` on events, `AgentGone`; `MirrorTracker` per-agent diff
   (`diff_entries` generalised over a transcript). Build summaries from `agent_list::rows`.
3. **Host surface** (`remote/surface.rs`, `ui/remote.rs`): handle the two new messages
   (`DeviceInput::AgentMessage`/`StopAgent`), apply through `message_agent`/`kill_agent`, audit,
   refuse with a notice to the device on failure; the host viewing state is untouched (devices do
   not move the host's `viewing`).
4. **TUI attach client** (`remote/client_ui.rs`): apply the agent-tagged events to
   `App::agent_views` so `fontaine attach` mirrors subagents too and the `agent_list`/view works
   there (it shares the renderers); lets the same host code path be exercised end to end in tests.
5. **App model and state** (`fontaine-android/src/model.rs`, `app.rs`, `layout.rs`): agent mirror,
   viewing state, new actions, the `MAIN_ONLY` rule, fallback to main.
6. **App UI** (`android/ui.rs`): agents bar, per-agent header and Stop, parity renderers
   (reasoning, shell, attachment), todo panel, queued list, status chips, slash picker.
7. **Contract test**: extend `the_android_mirror_decodes_what_the_host_publishes` with a snapshot
   holding a subagent view and tagged events.
8. **Docs and changelog**.

## Files to Modify

- `rust/fontaine-remote/src/protocol.rs`, `hub.rs` (only if the hub filters message kinds)
- `rust/fontaine-tui/src/remote/{event.rs,surface.rs,client_ui.rs}`, `ui/remote.rs`, `app.rs` (read
  access to agent views for the mirror)
- `rust/fontaine-android/src/{model.rs,app.rs,layout.rs,android/ui.rs}`
- `docs/remote.md`, `docs/android.md`, `CHANGELOG.md`

## Trade-offs

- **Mirror all agents to every device** vs a per-device `View { agent }` subscription: the hub
  publishes one stream to all devices and holds no per-device state; a subscription needs per-device
  mirrors and a resume story for each. Cost: bandwidth for transcripts a device is not looking at,
  bounded by `AGENT_ENTRIES_CAP` and the snapshot caps.
- **Tag events with `agent`** vs a separate event family for subagents: a tag reuses `diff_entries`
  and the apply path.
- **Version bump** vs additive fields: additive keeps old apps working against a new host (they
  ignore the new fields) and needs no negotiation.

## Decisions

- Keep host-only: session replacement and rewriting (`/resume /branch /rewind /clear`), model and
  effort, config, permissions, MCP, hooks, plugins, login, theme, output style, `/add-dir`, `/export`,
  `/copy`, `/memory`, `/bug`, `/exit`, `/loop`, mode changes, `/afk`; each changes host state beyond
  the conversation or loosens what an unattended host will do. `device_allowed` is not widened.
- Keep `/guardian-disable`, `/guardian-enable` and every `/remote` subcommand refused to devices
  (security invariants).
- Keep multi-select questions, elicitations, trust prompts, usage pause and pairing at the host.
- A device does not move the host's viewed agent; each device keeps its own view.
- Devices stop an agent (`StopAgent`) but do not interrupt a subagent's turn separately; stopping is
  the TUI's `x` equivalent and `Interrupt` stays main-only.
- `StopAgent` and `AgentMessage` are not guardian-judged: like `Prompt` and `Interrupt` they are
  inputs a human gives, and every call the agent then makes is still gated by the floor and guardian.
- Protocol `VERSION` stays 1 (additive).

## Documentation

- `docs/remote.md`: device list gains agent view/message/stop; the table of kept gaps with reasons.
- `docs/android.md`: agents bar, per-agent view, the added panels, slash picker.
- `CHANGELOG.md`: user-visible entry.

## Testing Strategy

- `protocol.rs`: round trips of the new `ClientMsg` variants; old-shape events (no `agent`) still
  parse; an event with `agent: None` serializes without the key.
- `event.rs`: a host with two subagent views, diffed event by event into a device `App`, renders the
  same lines per agent (extends `a_device_built_only_from_events_renders_the_host_transcript`);
  snapshot with agents round-trips through `SnapshotAssembler`; `AgentGone` on `/clear`; the cap and
  `clipped` flag.
- `surface.rs`: `AgentMessage`/`StopAgent` are queued as inputs and audited; an unknown agent id is
  refused with a notice; neither reaches the steering queue as a main prompt; a closed surface
  refuses them. The existing refused-command and plain-text prompt tests stay green.
- `fontaine-android` unit tests: model decodes agent fields and ignores unknown ones; `app.rs`
  send routing (viewing a subagent -> `AgentMessage`, slash while viewing a subagent is refused
  locally, view falls back to main when the agent leaves); `layout.rs` agents bar; picker list equals
  the device-allowed commands.
- Contract test extended as in step 7.
- End to end: a headless `fontaine -p`-driven integration test is not applicable (attach is
  interactive); the `fontaine attach` client over a loopback hub is the wiring test in `client_ui.rs`.

## Manual Verification

1. Run a session with `fontaine --remote`, ask it to spawn two subagents, pair the APK, and confirm the
   agents bar lists main and both agents, a tap shows each transcript live, a message to a subagent
   shows in its view and a Stop ends it. Not automated: needs a phone and the ImGui renderer, which no
   unit test reaches; breakage is obvious on first use.

## Open Questions

- None the codebase cannot settle.
