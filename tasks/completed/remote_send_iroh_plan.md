# One-Shot Remote Agent Message over iroh (`fontaine send`) Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/627

## Overview

Let a user enable a remote inbox on one live session (`/remote-inbox on`), receive a short-lived
ticket, and from another terminal or device run `fontaine send <ticket> "text"`. The client
connects over iroh (direct or relayed QUIC), authenticates with the ticket's one-time secret, and
gets `accepted` only after the live session has put the message into a bounded inbox. The message
is **presumed to come from the user**: holding a live ticket, minted by the user's explicit local
enable, is the proof. It enters the conversation as a user message (marked remote), delivered with
a later tool result while running and as a prompt turn while idle, never touching the human's
draft or queued input. This is the first slice of #583; persistent pairing, status and
orchestration stay there.

## Current State

- No iroh dependency and no cross-process ingress exist (`grep iroh` is empty). #580 (the
  external-message inbox) is **open**, so this slice cannot lean on it; it ships the minimal
  ingress it needs and #580 can later generalize it.
- Human input while a turn runs goes through `Steering` (`fontaine-core/src/steering.rs`):
  `Queued` entries (`Message`, `Skill`, `Command`, `Shell`) ride the root's next tool result and
  only then enter the ledger as `Record::UserMessage`, so the guardian never judges by words the
  model has not read. Undelivered entries are restored into the human's input on interruption;
  remote entries must be exempt from that restore.
- `Record::UserMessage { text, attachments, agent, origin }` (`ledger.rs:93`) carries a
  `MessageOrigin` (`Other`, `Dialog`, `Prompt`, `ledger.rs:38`) that decides what the guardian may
  read into it. Typed prompts are recorded in `fontaine-tui/src/ui/mod.rs` / `turn.rs` before the
  turn starts.
- The root's agent-mail inbox (`AgentTree::post_to_root`, `tree.rs:1783`; `Notification::render`
  wraps text as "not the user") is the wrong channel here: it is the untrusted-agent contract.
- The TUI loop starts a turn on idle work (`has_notifications` at `fontaine-tui/src/ui/mod.rs:755`,
  scheduled prompts, goal steps), all suppressed while AFK is blocked; the draft is never involved.
- CLI subcommands live in `fontaine/src/cli.rs` (`Command` enum, L157). Slash commands live in
  `fontaine-tui/src/commands.rs` (`Command`, `parse_command`, `main_only`, `runs_mid_turn`),
  dispatched by `dispatch_command` in `fontaine-tui/src/ui/mod.rs`. The TUI's input channel is
  `Input` in `fontaine-tui/src/app.rs`.
- Audit log and ledger: `fontaine-core/src/audit.rs` (`DecisionRecord`), `ledger.rs`. Human-driven
  session events are audited by `SessionState::record_session_event` (`gate.rs`, private today).

## Design

### 1. New crate `fontaine-remote` (under `rust/`)

Keeps the iroh dependency tree out of `fontaine-guardian` and `fontaine-core`'s hot path. Depends
on `iroh` (exact pin, one version for the workspace; verify license/advisories with `cargo deny`),
`tokio`, `serde`, `serde_json`; the wire format is JSON for debuggability. Added as a workspace
member and dependency of `fontaine` (bin) and `fontaine-tui`; it is core-free: the core and the
crate meet only through the `Delivery` trait implemented by the TUI host.

Modules:

- `protocol.rs` — ALPN `fontaine/send/1`. One bidirectional stream per request.
  `Request { v: u16, secret: String, message_id: String, text: String }`;
  `Response { v, status: Accepted{message_id} | Rejected{reason} }` with
  `Reason = BadTicket | Expired | Reused | Locked | TooLarge | InboxFull | Closed |
  UnsupportedVersion | Malformed`. Frames are length-prefixed JSON with a hard read cap
  (`MAX_FRAME`, at least the 8 KiB text limit plus JSON overhead), read under a timeout; a frame whose length prefix exceeds the cap is rejected as `TooLarge`, so the retry-with-same-ticket path for `TooLarge` is reachable. Session and message ids are plain `String`s; `Sender` is
  the authenticated iroh node id as a string.
- `ticket.rs` — `Ticket { v, node: EndpointAddr (node id + relay url + direct addrs),
  secret: [u8;32], expires_at }` (no session id: the endpoint key is per enable, so the node id already names the session), text form `fontaine-send1…` (base32 of JSON).
  Parse errors are typed (`TicketError`) because the CLI maps them to messages.
- `inbox.rs` — `RemoteInbox`, the server side state: the secret (stored only as a SHA-256 hash
  in memory), expiry, `consumed` flag, per-node bad-attempt counters, and a
  `seen: HashMap<(node, message_id), Response>` holding the stored original `Accepted` response for retry idempotency, cleared, together with the old secret hash (consumed or not), whenever any new ticket is minted, so a late retry gets `BadTicket`. `Delivery` trait (implemented by the
  TUI host): `fn accept(&self, from: Sender, message_id: String, text: String) -> Result<(),
  DeliveryError>` with `DeliveryError = Full | Closed`, and `fn on_reject(&self, reason: Reason,
  node: Sender)` so rejected requests reach the host's audit.
- `server.rs` — `RemoteServer::start(inbox, delivery, clock, relay_mode) -> (Ticket source,
  handle)`; accept loop; one task per connection; `shutdown()` closes the endpoint with an
  application error code so in-flight clients see `Closed`.
- `client.rs` — `send(ticket, text, opts) -> Result<Accepted, SendError>`; connects, writes the
  request, waits for the response with a timeout.

Relay mode is the user-scope-only config key `[remote] relay`: `default` (iroh's n0 public
relays; documented metadata exposure) or `disabled` (direct connections only). The default is
`default`. It does not enable anything; no key enables the inbox.

### 2. Auth, one-time secret, idempotency

- A ticket is minted by `/remote-inbox on` (lifetime default 10 minutes, `--ttl`, max 24 h). One
  ticket = one line. The secret is one-time: the first successful request *consumes* it and
  binds the sender's iroh node id as the authenticated sender. Consumption, binding and the `seen` entry happen only on `Accepted`; `InboxFull`, `TooLarge` and `Closed` do not consume the secret, so the sender may retry with the same ticket. The sequence check, `Delivery::accept`, then consume/bind/`seen` runs under one `RemoteInbox` lock (`accept` is synchronous and non-blocking), so two concurrent first requests cannot both be `Accepted`. Retries by the same node id with
  the same `message_id` return the original `Accepted` (lost-ack case, exactly-once queueing);
  this is only idempotent while the ticket is live: after expiry, or revocation by a newer ticket,
  a late retry gets `Expired`/`BadTicket` (documented). `Reused` applies only to a different
  `message_id` on a consumed, still-live secret. Further messages need a new ticket
  (`/remote-inbox ticket`), which is what "one-shot" means.
- `/remote-inbox on [--ttl <dur>] | off | status | ticket [--ttl <dur>]`; `ticket` mints a new ticket and revokes the previous one, consumed or not (a single
  live secret); minting any ticket clears `seen` and the old secret hash. After consumption or expiry the endpoint stays up until `off` or session end and
  the status indicator shows "inbox: no live ticket".
- Checks run in this order: secret, then expiry, then reuse, so a wrong secret learns nothing
  about the ticket's state. Comparison is constant-time. After 5 bad attempts the server stops
  accepting from that remote node id for 30 s (per node, not global, so a stranger cannot lock
  out the legitimate sender). The per-node map is bounded: entries expire after the 30 s window and the map is capped.
- Expiry is checked on the server clock (injectable for tests).
- The endpoint is per session and is fully torn down (not a closed flag) on exit, at the common session-switch point (`/clear`, `/resume`, `/branch`: every switch via `self.switch`/`Break`) and on `/remote-inbox off`, so a stale ticket cannot reach a later session (the endpoint key is minted
  fresh per enable, never persisted). After a switch the user must run `/remote-inbox on` again.
- Ticket secrecy: the ticket and the output of `/remote-inbox on|ticket` are shown once, never through `transcript.notice` (which reaches `/export`, `/transcript` and scrollback). In the TUI the ticket is shown in a dismissible modal that is cleared on dismiss; in the linear renderer it is written once directly to the terminal, outside the transcript model, and scrollback retention of that one print is accepted and documented. Either way it is never written to the ledger, audit log, prompt history, transcript export or
  telemetry. A remote `/remote-inbox ticket` or `on` is refused with a message, since the sender cannot
  read local output anyway. `fontaine send <ticket>` exposes the ticket to shell history and the
  process list; the CLI therefore also reads it from stdin (`--ticket-stdin`) or the
  `FONTAINE_TICKET` env var, documented.

### 3. Bounded delivery as a remote user message

Remote messages are remote-tagged entries in the existing `Steering` machinery (no separate
queue): `Queued` entries carry an origin (`Typed` or `Remote { node, message_id }`). Remote
entries go through the same hook, arrival ordering and `hold`/`announced` rules as typed ones, so
there is one queue and one arrival order, and a parked root wait wakes for them via the existing
deliverable-message check. Bounds: 16 queued remote entries and 8 KiB per message
(`MAX_REMOTE_MESSAGE_BYTES`), both checked **before** the ack. `Delivery::accept` always pushes into
`Steering`, which enforces the cap of 16 under its lock (today `Steering::queue` returns `()`; the remote enqueue returns a `Result` with a `Full` error that `accept` maps to `DeliveryError::Full`). `Steering::queued()` returns `Vec<String>`; a richer `queued_entries()` returns text plus the remote marker, and every `queued()` caller (`claude_compat/router.rs`, tests) is updated; the idle `Input` variant below is only a
wake-up that makes the loop drain Steering's remote entries through typed dispatch, so ordering and
bound have one owner. Remote entries are never merged by `group()` (`ui/turn.rs`) with typed ones:
each is its own submission carrying its remote identity. `Steering::retract()` returns every entry with its origin and sequence number, `Submission` carries that sequence number, and a new `Steering::requeue(entry, seq)` inserts in sequence order ("position" means sequence order), adjusting `announced` per the rule below. The submit paths (`end_of_turn` and
`group` in `ui/turn.rs`, reached from `converse`, `harness_turn_tail` and `goal.rs`) submit remote
entries in arrival order as separate submissions (`end_of_turn` today returns a single `EndOfTurn`; its return type changes to carry both the restore text and the remote submissions, e.g. a pair or a new variant), including after an interrupted or failed turn:
the post-interrupt drain point is the `end_of_turn` call in `converse`/`harness_turn_tail`, which
submits the remote entries even when the typed ones go to `EndOfTurn::RestoreToInput`. Only the
restore-to-draft paths (`EndOfTurn::RestoreToInput`, `restore_block` in `ui/turn.rs`, `edit_queued` in `ui/mod.rs`, `serve_queued`
in `ui/commands/context.rs`) filter remote entries out and re-queue them in `Steering` instead of
restoring them, so remote text never enters the human's input. `restore_block` restores the raws of all `pending` submissions, so remote submissions in `pending` are never restored to the draft: on interruption or failure they are re-queued into `Steering`, and the `Break` paths that drop `pending` re-queue them too, or audit-drop them if the session is ending. `queued_entries()`/display mark remote
entries as remote, and remote lines are never added to local prompt history.

`/clear`, `/resume` and `/branch` first tear the endpoint down (so `accept` returns `Closed`), then call a new `Steering::drain_remote` (not `retract`) at the common session-switch point (every switch via `self.switch`/`Break`), before the idle drain: it DROPs already-accepted remote
entries still queued, so they are never delivered across a context boundary; each gets an audit
drop record and a transcript notice (the sender's `accepted` meant queued, documented). Remote entries still queued at session end or `TurnEnd::InputClosed` are audit-dropped the same way.

Re-queued remote entries (`restore_block`, `pending` on interruption or failure, the `Break` paths, the `RestoreToInput` filters) go back through `Steering::requeue(entry, seq)` with their original sequence number (or are marked dialog-ineligible), so an old mid-turn `y` can never answer a later dialog. Every mid-queue removal or insert (a dialog taking an entry, `drain_remote`, a re-queue) adjusts `Steering.announced`.

- **Wake-up**: the idle `Input` wake-up is edge-triggered, so the idle loop also checks `Steering` for deliverable remote entries before awaiting input and after each modal/dialog/`serve_queued` exit (level-triggered, like the `has_notifications` check); modal and dialog loops that decline an entry leave it queued for that check.
- **Dispatch**: a remote line runs through the same dispatch as a typed line. While idle it
  arrives through a new `Input` variant (wake-up only, carries no text, see above) with a single injection point that
  delivers the line to whichever loop is awaiting input, without touching the input buffer, draft or
  cursor. In scope: the idle prompt and every input loop in `ui/turn.rs` and `ui/mod.rs` that awaits
  input: the approval selector, `ask_user_question`, the `/guardian-disable` confirmation and the AFK
  review dialog.
- **Dialog mapping**: a remote line answers an open approval, question or confirmation dialog only
  when its trimmed, case-insensitive text exactly equals one of that dialog's option labels or hotkey
  letters (the same options a local user could pick). `accept` always enqueues and acks "queued"; the cap applies at enqueue only. When the dialog
  consumes a matching line, the injection point takes that entry out of `Steering`.
  Only remote lines accepted after the dialog opened can answer it: `Steering` stamps each entry with a sequence number and a dialog registers through `Steering::open_dialog(options) -> guard` (the guard releases the hold on drop; each dialog has its own watermark); entries at or below it stay ordinary queued messages. The dialog guard uses the same global `hold` that `has_deliverable_message`, `has_unannounced`, `take_announcement` and `unannounced_from` already check, so dialog-held entries are non-deliverable and plain messages behind them wait like typed ones. While a dialog is open, steer does not deliver dialog-eligible entries into a tool result (they are held for the dialog); post-open entries that do not match stay queued as normal messages and are delivered after the dialog closes, not held forever. When a dialog consumes a line, the host writes a `remote_inbox` audit record (rule `dialog_answer`) with node id, message id and dialog kind. Anything else stays queued as a normal
  message behind the dialog, and the dialog stays pending. Remote-answerable dialogs are exactly these four kinds (approval selector, `ask_user_question`, `/guardian-disable` confirmation, AFK review); every other awaiting-input loop (Guardian unavailable prompt, UsagePause, RetryDenied, pickers, rewind confirm) declines and leaves the entry queued. Mid-turn, plain messages
  ride the next tool result (wrapped with a one-line provenance header
  `[Remote message from <short node id>]`); remote slash, skill and `!` lines are always queued as
  `Queued::Command/Skill/Shell` and execute after the turn (including `/remote-inbox off` and `status`), under the `main_only` rules; typed lines keep their `runs_mid_turn` behaviour.
- **Ledger**: a delivered remote message becomes a `Record::UserMessage` with its existing
  origin (`MessageOrigin::Other` for an idle remote line, never `Prompt`, so `reply_before_last`
  (`ledger.rs`) does not bind it to an agent reply the sender never saw) and the new optional `remote: { node, message_id }` field (see section 6). The remote
  identity is threaded through `converse`, `Submission`, the `turn` origin parameters and
  `show_delivered` so the transcript shows the remote text with a remote marker and the authenticated node id.
- **Idle**: a remote line is treated exactly like a typed line: it is not held while AFK is
  blocked or stopped, so mode commands and `/afk review` work from it, and the local `refuse_if_stopped`
  and guardian-disable AFK refusals apply as for typed lines, except that a remote line refused by
  `refuse_if_stopped`/`refuse_stopped` (`ui/turn.rs`) is never restored to the human's input: it
  shows an error in the transcript and writes an audit drop record (the sender already got
  `accepted`, documented as "queued"). A remote prompt resets the AFK stop
  streak like any user prompt. The first remote line of a session may become its name /
  `first_prompt`, like a typed one.
- **Prompt-submit hooks** run in `converse` as for typed prompts; a hook block drops the message
  with an audit record only: `accepted` means queued, and the secret was already consumed at `Accepted` (documented).
- **Guardian**: a remote message counts as human intent (it is a user message). Because the
  ticket is a bearer credential, that is the deliberate widening of trust this slice makes.
- **Remote is a terminal, not just a prompt**: slash commands (`/permissions`, mode changes,
  `/compact`...), skills and `!` lines work, and an open approval or question dialog is answered
  by a remote line under the dialog mapping above. There is no exception: `/guardian-disable` and
  `/guardian-enable` work from a remote line too, and remain blocked in headless runs. Authority
  of the line is full terminal input, but a multi-step interaction (a `/guardian-disable`
  confirmation, an approval answer after the model prompts) needs another ticket/line: a remote
  `/guardian-disable` leaves its confirmation dialog pending for the next input, local or a new
  remote line. This extends AGENTS.md's "after an explicit confirmation in the terminal UI" to a
  remote terminal; update that wording in AGENTS.md (line ~50) and the master plan's Decisions entry
  in the same change.

### 4. Enabling and disabling (local, explicit, user-only)

- `[remote]` is user-scope-only: `fontaine-core/src/config.rs` rejects it in project scope with a config error/warning (project scope can only tighten); documented in `docs/configuration.md`.
- `/remote-inbox on [--ttl <dur>] | off | status | ticket [--ttl <dur>]` slash command, interactive TUI only and `main_only`.
  Registered in `fontaine-tui/src/commands.rs` (`Command` variant, `parse_command`, `BUILTINS`,
  `main_only`, `runs_mid_turn`: it is in the immediate-UI set; its handler is synchronous and spawns the async endpoint start as a task whose result returns through a new `Action` variant carrying the started ticket (update the `runs_mid_turn` doc comment in `commands.rs`, which says handlers touch only UI state); remote slash lines are always queued and run after the turn, never immediately (typed lines keep `runs_mid_turn`); the `commands.rs` test
  `runs_mid_turn_marks_the_immediate_ui_set_and_nothing_else` needs updating; a remote
  `/remote-inbox off` and `status` are allowed, `ticket` and `on` are refused; `status` has a "starting" state; `on` while already enabled or starting is a no-op that shows status) with a `dispatch_command` arm in `fontaine-tui/src/ui/mod.rs` and
  an entry in the `docs/tui.md` command list and help. Never enabled by config or project scope;
  no key enables the inbox. Headless runs cannot enable it, and `Delivery` is implemented by
  the TUI only.
- `on` starts the `RemoteServer` for the current session, mints a ticket and prints it once;
  the status line shows an indicator while enabled. `off`, session end, any session switch (`/clear`, `/resume`, `/branch`) and quit fully tear it down; the user must run `/remote-inbox on` again after a switch.
- Enabling is a user command, not a tool: the model cannot call it. No new hook is added, so
  `fontaine-core/src/agent/build.rs` is unchanged.

### 5. CLI: `fontaine send <ticket> <text>`

New `Command::Send(SendArgs)` in `cli.rs`, handler in `fontaine/src/send_cmd.rs`. Text may be
`-` for stdin; the ticket may come from stdin (`--ticket-stdin`) or `FONTAINE_TICKET`; `--ticket-stdin` with text `-` is a usage error (exit 2). Prints
`accepted <message_id>` and exits 0; failures print one line from the typed reason and exit
nonzero:

| Exit | Reasons |
|---|---|
| 2 | `BadTicket`, `Expired`, `Reused`, `Locked`, malformed ticket |
| 3 | `InboxFull`, `TooLarge` |
| 4 | connection failure, timeout, `Closed` |
| 5 | `UnsupportedVersion`, `Malformed` |

`message_id` is generated client-side per invocation (`msg-<uuid>`), and the client retries once
on a lost ack with the same id. It needs no config, session or provider credentials.

### 6. Audit and ledger

- **Audit**: `fontaine-remote` reports through the host callbacks (`Delivery::accept` /
  `on_reject`); the TUI host calls a new public `SessionState` entry point built on
  `record_session_event_as` (`gate.rs`), taking a `Decision` (rejects and drops are `Deny`), that writes a `DecisionRecord` (`audit.rs`) with tool
  `remote_inbox`, a distinct rule per event, and node id, message id, size and outcome in the record's `explanation` text:
  `record_session_event_as` stores only `args_digest` of its args and hardcodes `ROOT_AGENT`, which
  is the intended agent id here; there is no audit schema change.
  Events: accepted, rejected (with reason), enable, disable, ticket mint, and a message dropped
  by a prompt-submit hook. The ticket and secret never appear. No new `Tier` variant is needed:
  the events use `Tier::Human` like `skill_trust`, so `tier.rs` and `permissions.rs` are
  unchanged.
- **Ledger**: `Record::UserMessage.text` of a remote message stores the verbatim text; the provenance header is added only in the tool result. `MessageOrigin` gets no new variant (an older build fails to parse an unknown enum
  value in `Ledger::open`). `Record::UserMessage` keeps its origin and gains
  `#[serde(default, skip_serializing_if = "Option::is_none")] remote: Option<RemoteSender
  { node, message_id }>`; older builds ignore the unknown field, so resume of a newer ledger
  works and the remote identity is simply not shown. `set_guardian_off` and `Record::GuardianOff`
  (`ledger.rs` ~114; the `..` match at ~764 is unaffected) gain `#[serde(default, skip_serializing_if = "Option::is_none")] remote: Option<RemoteSender>`, the same type as on `Record::UserMessage`, so the `remote` on `GuardianOff` and in the audit `explanation` is the identity of the CONFIRMING line (a local confirmation records none); the requesting line's identity is also written into the audit `explanation`.
- Every `Record::UserMessage {` literal must be updated (find them with `rg`), or add a
  constructor (preferred). Likewise every `Record::GuardianOff` literal and pattern
  (`gate.rs` ~747, `tests/guardian_off.rs` ~52; the exact-shape `matches!` there needs `remote: None`).

### 7. Invariants kept

The remote line is presumed-user terminal input: it can answer approvals, change modes and run
slash commands, and run `/guardian-disable` after its confirmation, exactly as a local line can.
Every tool call it induces crosses the floor, guardian, workspace scope and result pipeline unchanged. The
guardian is not optional, no regex is applied to arguments, no new tool is registered (the
risk-class choke-point is untouched), and enabling is user-only, per session, never via config,
project scope or the model. The ticket's bearer risk is the accepted cost of the presumption:
short TTL, one-time secret, shown once, torn down with the session. Record the deviation from
the issue's earlier untrusted-mail wording in the plan's Decisions.

## Implementation Steps

1. Add `fontaine-remote` crate with `protocol`, `ticket`, `inbox`, `client`, `server`.
2. `fontaine-core`: origin on `Queued` entries (skip on draft restore and history), `remote`
   field on `Record::UserMessage` plus the struct-literal sweep, optional `remote` on
   `GuardianOff`, public audit entry point.
3. `fontaine-tui`: `Delivery` impl over `Steering`; new `Input` variant that is wake-up only and carries no text
   (the line stays in `Steering`); thread remote identity through `converse`/`Submission`/`turn`/`show_delivered`; hook run.
4. `/remote-inbox` command (`commands.rs`, `dispatch_command`), status indicator, teardown on
   clear/switch/quit.
5. `fontaine send` CLI.
6. Docs, changelog, `cargo deny` config for iroh's licenses; governance edits (AGENTS.md, master
   plan); iroh build check on macOS/Windows: run `cargo check` on the Windows target
   (`--target x86_64-pc-windows-gnu`) and confirm the `dist` build matrix still builds with the
   new dependency tree.

## Files to Modify

- `rust/Cargo.toml`, `rust/deny.toml`, `rust/fontaine/Cargo.toml`, `rust/fontaine-tui/Cargo.toml`
- `rust/fontaine-remote/**` (new)
- `rust/fontaine-core/src/{steering.rs,ledger.rs,gate.rs,config.rs,questions.rs,session.rs}`, `rust/fontaine-core/src/child_runner.rs` (production `Record::UserMessage` literal in `record_human_prompt` ~566, and tests ~1634, ~1643), `rust/fontaine/src/headless.rs`, `rust/fontaine/src/claude_compat/router.rs` (`steering.queue`/`retract` ~292, ~336), `rust/Cargo.lock`, the `Record::UserMessage` and
  `Record::GuardianOff` literal sites (section 6)
- `rust/fontaine-tui/src/{app.rs,commands.rs,linear.rs,statusbar.rs,view.rs}`, `rust/fontaine-tui/src/ui/{mod.rs,turn.rs,goal.rs,providers.rs,tests.rs}` (`providers.rs`: exhaustive `Action`/`Input` matches), `rust/fontaine-tui/src/ui/commands/{context.rs,guardian.rs}`, and the dialog input loops (`turn.rs` and the others that await input)
- `rust/fontaine/src/cli.rs`, `send_cmd.rs` (new), `main.rs`
- `AGENTS.md` (project layout crate list; the terminal-confirmation wording for
  `/guardian-disable` and the floor, line ~50), `tasks/fontaine_harness_plan.md` (workspace layout,
  `ci.yml` step set, and the Decisions entry at the end of the correct subject subsection),
  `docs/remote-inbox.md` (new), `docs/SUMMARY.md`, `docs/index.md`, `docs/agents.md`,
  `docs/tui.md` (command list and help), `docs/guardian.md`, `docs/afk.md`, `docs/terminal.md`,
  `docs/configuration.md` (`[remote] relay`, user-scope-only); the `/guardian-disable` docs
  (`tui.md`, `guardian.md`, `afk.md`, `terminal.md`) say the confirmation may be remote, `tasks/fontaine_harness_plan.md` Decisions line ~744-745 (the `/guardian-disable` deviation), `SECURITY.md` (describes the guardian-disable
  confirmation), `CHANGELOG.md`

## Trade-offs

- **New crate vs. inside `fontaine-agents`**: iroh pulls a large tree (QUIC, TLS, relay
  client); isolating it keeps compile time and the audit surface off the guardian-adjacent crates.
- **Secret consumed on first use vs. valid for N messages**: matches the issue's "one-time
  secret" and "one-shot"; lost acks are handled by `message_id` idempotency rather than by
  keeping the secret alive.
- **Remote-tagged `Steering` entries vs. a separate queue or root agent mail**: one queue keeps
  one arrival order and one delivery path; the origin tag lets draft restore skip remote text;
  agent mail carries the untrusted contract the user rejected for this slice.
- **Presumed user vs. untrusted mail**: the user chose the presumption; a ticket is a bearer
  credential for their own devices, so the mitigation is a short, one-time ticket, not a weaker
  message.
- **Minimal ingress now vs. waiting for #580**: this slice must ship alone; the ingress is
  deliberately small so #580 can generalize it.
- **n0 relays by default**: keeps the relayed acceptance criterion working out of the box, at the
  cost of documented metadata exposure; `[remote] relay = "disabled"` opts out.

## Decisions

- 2026-11-14: remote messages are presumed to come from the user (user message, guardian intent, prompt hooks), superseding the issue's original untrusted-agent-mail wording; the issue body was updated to match.
- 2026-11-14: a remote line is full terminal input (slash commands, mode changes, approval answers), because the feature is the first slice of remote control; there is no exception, `/guardian-disable` included (confirmation still required, toggles still audited with remote identity). AGENTS.md and the master plan's Decisions are updated to say the terminal may be remote.
- 2026-11-14: AFK: one ticket = one line; a multi-step interaction (a `/guardian-disable` confirmation, an approval answer) needs another ticket/line, so a remote `/guardian-disable` leaves its confirmation pending for the next input; a new ticket revokes the previous ticket, consumed or not; lockout is per remote node id; checks run secret, expiry, reuse.
- 2026-11-14: AFK: `retract()` returns everything with its origin; `end_of_turn`/`group` submit remote entries as separate submissions in arrival order (after an interrupted or failed turn too, at the `end_of_turn` drain in `converse`/`harness_turn_tail`); only the restore-to-draft paths (`EndOfTurn::RestoreToInput`, `edit_queued`, `serve_queued`) filter remote entries out and re-queue them.
- 2026-11-14: AFK: remote messages are remote-tagged `Steering` entries (no separate queue) with the typed hook, ordering and `hold`/`announced` rules; draft restore skips them and they never enter prompt history; remote lines use the typed dispatch through `Steering`; an `Input` variant only wakes the idle loop to drain them.
- 2026-11-14: AFK: the ledger gets no `MessageOrigin::Remote` (older builds fail on unknown enum values); `Record::UserMessage` gains an optional `remote { node, message_id }` field and `Record::GuardianOff` an optional `remote` of the same type (amended: not an origin value), both serde-defaulted.
- 2026-11-14: AFK: remote audit uses `record_session_event`-style `remote_inbox` records with `Tier::Human` (no new tier); `fontaine-remote` stays core-free and reports through `Delivery::accept`/`on_reject`.
- 2026-11-14: AFK: a remote line is treated exactly like a typed line under AFK (never held; mode commands, `/afk review` and the AFK refusals apply as typed) and resets the AFK stop streak.
- 2026-11-14: AFK: the ticket and `/remote-inbox on|ticket` output are never written to the ledger, audit log, history, export or telemetry; a remote `/remote-inbox ticket` is refused; `fontaine send` also reads the ticket from stdin or `FONTAINE_TICKET`.
- 2026-11-14: AFK: `/remote-inbox` is a `main_only` slash command registered in `fontaine-tui/src/commands.rs`; no new hook is added.
- 2026-11-14: AFK: remote audit identity (sender node id, message id, size, outcome) goes in the record's `explanation` with agent `ROOT_AGENT`, no audit schema change; a remote `GuardianOff` is identified by the ledger `remote` field and the audit `explanation`.
- 2026-11-14: AFK: a remote line refused by `refuse_if_stopped`/`refuse_stopped` is never restored to the human's input; it shows a transcript error and writes an audit drop record.
- 2026-11-14: AFK: an idle remote line is recorded with `MessageOrigin::Other`, never `Prompt`, so `reply_before_last` does not bind it to a reply the sender never saw.
- 2026-11-14: AFK: a remote line answers an open dialog only when its trimmed, case-insensitive text equals an option label or hotkey letter; otherwise it is queued behind the dialog, which stays pending; one injection point serves every awaiting input loop.
- 2026-11-14: AFK: on `/clear` and resume switch, already-accepted remote entries still queued in `Steering` are dropped, never delivered across a context boundary, each with an audit drop record and a transcript notice (the sender's `accepted` meant queued); this is a separate drop (`Steering::drain_remote`), not a `retract()` caller.
- 2026-11-14: AFK: only remote lines accepted after a dialog opened can answer it; entries already queued in `Steering` when it opens stay ordinary queued messages (a mid-turn `y` never approves a later dialog).
- 2026-11-14: AFK: `restore_block` never restores remote submissions in `pending` to the draft; on interruption/failure they are re-queued into `Steering`, and `Break` paths that drop `pending` re-queue them, or audit-drop them if the session is ending.
- 2026-11-14: AFK: the idle wake-up is level-triggered: the idle loop checks `Steering` for deliverable remote entries before awaiting input and after every modal/dialog/`serve_queued` exit; loops that decline an entry leave it queued for that check.
- 2026-11-14: AFK: remote `/remote-inbox on` is refused like `ticket`; any new ticket clears `seen` and the old secret hash; the per-node lockout map expires entries after 30 s and is capped; a dialog-consumed remote line is audited as `remote_inbox` rule `dialog_answer`; post-open eligibility is a `Steering` sequence watermark and dialog-eligible entries are held while a dialog is open.
- 2026-11-14: AFK: re-queued remote entries (`restore_block`, `pending`, `Break` paths, `RestoreToInput` filters) keep their original `Steering` sequence number and position or are marked dialog-ineligible, so an old mid-turn `y` never answers a later dialog; dialogs register via `open_dialog` and non-matching post-open entries are delivered after it closes.
- 2026-11-14: AFK: the ticket is shown in a dismissible TUI modal cleared on dismiss, or in the linear renderer printed once directly to the terminal outside the transcript model (scrollback retention of that print accepted, documented); never in transcript, ledger, audit, history, export or telemetry.
- 2026-11-14: AFK: `/clear`, `/resume` and `/branch` (the common session-switch point) tear the endpoint down first (`accept` returns `Closed`; the user re-runs `/remote-inbox on`), then `drain_remote`; the secret is consumed only on `Accepted`; remaining remote entries are audit-dropped at session end.
- 2026-11-14: AFK: a hook-dropped remote message is audit-only: `accepted` means queued and the secret is consumed at `Accepted`, so a hook drop never un-consumes it.
- 2026-11-14: AFK: relay mode is the user-scope-only key `[remote] relay` (`default` = n0 relays, `disabled` = direct only), default `default`; it never enables the inbox.
- 2026-11-14: AFK: pin the newest stable iroh release that passes `cargo deny` and builds on Linux, macOS and Windows at implementation time, exact-pinned like the other pre-1.0 families.
- 2026-11-14: AFK: `--resume` never re-enables the inbox; it is always an explicit `/remote-inbox on`.

## Documentation

- New `docs/remote-inbox.md`: enabling/disabling, ticket lifetime, presumed-user trust semantics and the ticket's bearer-credential risk (including shell-history/process-list exposure and the stdin/env alternatives), delivery
  guarantees, what "accepted" means (queued, not model-delivered or completed), error table and exit codes,
  direct vs relayed, `[remote] relay` and privacy of relay use. Linked from `SUMMARY.md`, `index.md`; cross-link
  from `docs/agents.md` ("Messages between agents") and `docs/tui.md` (slash commands).

## Testing Strategy

Unit (no network): ticket encode/parse round trip and malformed/expired/version cases; check
order (bad secret before expiry before reuse); secret consume and reuse; idempotent retry
returns the same `message_id` without a second enqueue; frame cap and oversized text; per-node
lock after bad attempts (another node unaffected); inbox cap → `InboxFull`; 24 h TTL cap; a new
ticket revokes the previous one;
remote `Steering` entries: bounds (enforced in `Steering`, `accept` always pushes), FIFO with typed entries, never grouped with typed entries, returned by `retract()` with origin, submitted in order as separate submissions after an interrupted/failed turn, filtered out and re-queued by `RestoreToInput`, `edit_queued` and `serve_queued`, marked remote in `queued_entries()`, absent from
prompt history; parked root wait wakes for a remote message; tool-result delivery then ledger
`UserMessage` with `remote` in order; remote text is present in guardian human intent; `/` lines
dispatch as slash commands, a mode change from a remote line takes effect, per dialog kind
(approval selector, `ask_user_question`, `/guardian-disable` confirmation, AFK review): a remote line equal to an option label or hotkey (case-insensitive, trimmed) answers it and any other text is queued with the dialog still pending; an idle remote line is recorded as `MessageOrigin::Other` and `reply_before_last` does not bind it; a remote line refused while stopped is not restored to the input and writes an audit drop record; a remote `/guardian-disable` leaves its confirmation pending, and the toggle is
audited and ledgered with `remote`; remote `/remote-inbox ticket` is refused; the ticket
never appears in ledger, audit, history or `/export`; a project-scope `[remote]` key is rejected with a config error/warning; a `y` queued mid-turn does not approve a later dialog, including after it was re-queued by `restore_block`/`pending`/`Break`/`RestoreToInput` (sequence number and position preserved); a non-matching post-open entry is delivered after the dialog closes; `open_dialog` guard drop releases the hold; `Steering.announced` stays consistent after a dialog takes an entry, `drain_remote` and a re-queue (beside `retract_delivery_and_hold_keep_the_announcement_consistent`); remaining remote entries are audit-dropped at session end; `InboxFull`/`TooLarge`/`Closed` do not consume the secret and the same ticket can retry; two concurrent first requests give exactly one `Accepted` and the other `Reused`; a remote message's ledger text is verbatim without the provenance header; dialogs outside the four remote-answerable kinds decline and leave the entry queued; a remote slash line is queued and runs after the turn, `/remote-inbox off` included; `--ticket-stdin` with text `-` exits 2; prompt-submit hook block is audited; remote lines are
not held while AFK is blocked and reset the stop streak; the first remote line may become the
session name/`first_prompt`; audit records for accept, reject, enable, disable and hook-dropped
message; full teardown on `off`, `/clear`, `/resume` and `/branch`; a second `/remote-inbox on` while starting is a no-op; a frame length prefix over `MAX_FRAME` gives `TooLarge`; accepted remote entries still queued are dropped on `/clear` and resume switch with an audit drop record and transcript notice, via `drain_remote` after accepting stops (`accept` returns `Closed`); a lost-ack retry within the live ticket window returns the original `Accepted` once, a late retry after expiry or revocation gets `Expired`/`BadTicket`; sending to a torn-down endpoint (the issue's "unknown sessions" case) is the connection-failure/`Closed` path, exit 4.

Back-compat: an old `UserMessage` line parses; a new line with `remote` parses; an older-shaped
reader (struct without the field) ignores it; `GuardianOff` likewise (with and without `remote`).

Integration (`fontaine-remote/tests`): server and client in one process on local iroh endpoints
with relay disabled (direct on loopback) and with a local test relay in-process (forced-relay
path); no external relay or live provider. Cases: accepted, expired, reused, invalid, oversized,
full, shutdown mid-flight, dropped ack then retry (exactly once). TUI test
(`TestBackend`) with a scripted model: delivery while running (arrives with next tool result),
while idle (a user turn), and a multiline draft plus queued input preserved byte for byte.
Real-network qualification catches NAT/relay behavior a loopback test cannot; it is manual.

## Manual Verification

1. Two machines on one LAN: `/remote-inbox on`, run `fontaine send <ticket> "hi"`; expect
   `accepted msg-…` and the message in the session. Why manual: needs real network paths.
2. Block direct UDP (firewall) between them and repeat; expect success via relay.
3. Type a draft in the target, send, confirm the draft is untouched.

## Open Questions

None.
