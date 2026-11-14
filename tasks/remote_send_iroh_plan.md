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
  (`MAX_FRAME`), read under a timeout. Session and message ids are plain `String`s; `Sender` is
  the authenticated iroh node id as a string.
- `ticket.rs` — `Ticket { v, node: EndpointAddr (node id + relay url + direct addrs), session:
  String, secret: [u8;32], expires_at }`, text form `fontaine-send1…` (base32 of JSON).
  Parse errors are typed (`TicketError`) because the CLI maps them to messages.
- `inbox.rs` — `RemoteInbox`, the server side state: the secret (stored only as a SHA-256 hash
  in memory), expiry, `consumed` flag, per-node bad-attempt counters, and a
  `seen: HashMap<message_id, ()>` for retry idempotency. `Delivery` trait (implemented by the
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
  binds the sender's iroh node id as the authenticated sender. Retries by the same node id with
  the same `message_id` return the original `Accepted` (lost-ack case, exactly-once queueing);
  any other request with the used secret → `Reused`. Further messages need a new ticket
  (`/remote-inbox ticket`), which is what "one-shot" means.
- `/remote-inbox ticket` mints a new ticket and revokes the previous unconsumed one (a single
  live secret). After consumption or expiry the endpoint stays up until `off` or session end and
  the status indicator shows "inbox: no live ticket".
- Checks run in this order: secret, then expiry, then reuse, so a wrong secret learns nothing
  about the ticket's state. Comparison is constant-time. After 5 bad attempts the server stops
  accepting from that remote node id for 30 s (per node, not global, so a stranger cannot lock
  out the legitimate sender).
- Expiry is checked on the server clock (injectable for tests).
- The endpoint is per session and is torn down on exit, `/clear`, `--resume` switch and
  `/remote-inbox off`, so a stale ticket cannot reach a later session (the endpoint key is minted
  fresh per enable, never persisted).
- Ticket secrecy: the ticket and the output of `/remote-inbox on|ticket` are printed once to the
  local terminal and never written to the ledger, audit log, prompt history, transcript export or
  telemetry. A remote `/remote-inbox ticket` is refused with a message, since the sender cannot
  read local output anyway. `fontaine send <ticket>` exposes the ticket to shell history and the
  process list; the CLI therefore also reads it from stdin (`--ticket-stdin`) or the
  `FONTAINE_TICKET` env var, documented.

### 3. Bounded delivery as a remote user message

Remote messages are remote-tagged entries in the existing `Steering` machinery (no separate
queue): `Queued` entries carry an origin (`Typed` or `Remote { node, message_id }`). Remote
entries go through the same hook, arrival ordering and `hold`/`announced` rules as typed ones, so
there is one queue and one arrival order, and a parked root wait wakes for them via the existing
deliverable-message check. Bounds: 16 queued remote entries and 8 KiB per message
(`MAX_REMOTE_MESSAGE_BYTES`), both checked **before** the ack. Two differences from typed
entries, both enforced in `steering.rs`: restore-to-draft on interruption/failure skips remote
entries (they stay queued or are redelivered, never inserted into the human's input), and remote
lines are never added to local prompt history.

- **Dispatch**: a remote line runs through the same dispatch as a typed line. While idle it
  arrives through a new `Input` variant for an already-submitted line, one injection design used
  by every input loop (idle prompt, approval dialogs, confirmations), so each handles it exactly
  like a typed line without touching the input buffer, draft or cursor. Mid-turn, plain messages
  ride the next tool result (wrapped with a one-line provenance header
  `[Remote message from <short node id>]`); slash, skill and `!` lines are queued as
  `Queued::Command/Skill/Shell` and execute after the turn, under the same `main_only` /
  `runs_mid_turn` rules as typed lines.
- **Ledger**: a delivered remote message becomes a `Record::UserMessage` with its existing
  origin and the new optional `remote: { node, message_id }` field (see section 6). The remote
  identity is threaded through `converse`, `Submission`, the `turn` origin parameters and
  `show_delivered` so the transcript shows the authenticated node id, never payload text.
- **Idle**: a remote line is treated exactly like a typed line: it is not held while AFK is
  blocked or stopped, so `/afk` and mode commands work from it, and the local `refuse_if_stopped`
  and guardian-disable AFK refusals apply as for typed lines. A remote prompt resets the AFK stop
  streak like any user prompt. The first remote line of a session may become its name /
  `first_prompt`, like a typed one.
- **Prompt-submit hooks** run in `converse` as for typed prompts; a hook block drops the message
  with an audit record (the sender already got `accepted`, which means queued: documented).
- **Guardian**: a remote message counts as human intent (it is a user message). Because the
  ticket is a bearer credential, that is the deliberate widening of trust this slice makes.
- **Remote is a terminal, not just a prompt**: slash commands (`/permissions`, mode changes,
  `/compact`...), skills and `!` lines work, and an open approval or question dialog is answered
  by a remote line exactly as by a typed one. There is no exception: `/guardian-disable` and
  `/guardian-enable` work from a remote line too, and remain blocked in headless runs. Authority
  of the line is full terminal input, but a multi-step interaction (a `/guardian-disable`
  confirmation, an approval answer after the model prompts) needs another ticket/line: a remote
  `/guardian-disable` leaves its confirmation dialog pending for the next input, local or a new
  remote line. This extends AGENTS.md's "after an explicit confirmation in the terminal UI" to a
  remote terminal; update both places that wording appears and the master plan's Decisions entry
  in the same change.

### 4. Enabling and disabling (local, explicit, user-only)

- `/remote-inbox [on|off|status|ticket]` slash command, interactive TUI only and `main_only`.
  Registered in `fontaine-tui/src/commands.rs` (`Command` variant, `parse_command`, `BUILTINS`,
  `main_only`, `runs_mid_turn`) with a `dispatch_command` arm in `fontaine-tui/src/ui/mod.rs` and
  an entry in the `docs/tui.md` command list and help. Never enabled by config or project scope;
  no key enables the inbox. Headless runs cannot enable it, and `Delivery` is implemented by
  the TUI only.
- `on` starts the `RemoteServer` for the current session, mints a ticket and prints it once;
  the status line shows an indicator while enabled. `off`, session end, `/clear`, resume switch
  and quit shut it down.
- Enabling is a user command, not a tool: the model cannot call it. No new hook is added, so
  `fontaine-core/src/agent/build.rs` is unchanged.

### 5. CLI: `fontaine send <ticket> <text>`

New `Command::Send(SendArgs)` in `cli.rs`, handler in `fontaine/src/send_cmd.rs`. Text may be
`-` for stdin; the ticket may come from stdin (`--ticket-stdin`) or `FONTAINE_TICKET`. Prints
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
  `record_session_event` (`gate.rs`) that writes a `DecisionRecord` (`audit.rs`) with tool
  `remote_inbox`, a distinct rule per event, and node id, message id, size and outcome in args.
  Events: accepted, rejected (with reason), enable, disable, ticket mint, and a message dropped
  by a prompt-submit hook. The ticket and secret never appear. No new `Tier` variant is needed:
  the events use `Tier::Human` like `skill_trust`, so `tier.rs` and `permissions.rs` are
  unchanged.
- **Ledger**: `MessageOrigin` gets no new variant (an older build fails to parse an unknown enum
  value in `Ledger::open`). `Record::UserMessage` keeps its origin and gains
  `#[serde(default, skip_serializing_if = "Option::is_none")] remote: Option<RemoteSender
  { node, message_id }>`; older builds ignore the unknown field, so resume of a newer ledger
  works and the remote identity is simply not shown. `set_guardian_off` and `Record::GuardianOff`
  gain an optional origin (`#[serde(default)]`) so toggles from a remote line are recorded as
  remote in both the ledger and audit log.
- Every `Record::UserMessage` struct-literal site must be swept (or a constructor added):
  `fontaine/src/headless.rs`, `fontaine-core/src/{child_runner.rs, questions.rs, steering.rs,
  goal, schedule.rs, turn.rs}` , `fontaine-tui/src/ui/{mod.rs, turn.rs}`,
  `fontaine-evals/src/none.rs` and tests.

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
   field on `Record::UserMessage` plus the struct-literal sweep, optional origin on
   `GuardianOff`, public audit entry point.
3. `fontaine-tui`: `Delivery` impl over `Steering`; new `Input` variant for an already-submitted
   line; thread remote identity through `converse`/`Submission`/`turn`/`show_delivered`; hook run.
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
- `rust/fontaine-core/src/{steering.rs,ledger.rs,audit.rs,gate.rs}`, the `Record::UserMessage` literal
  sites listed in section 6, and the guardian's intent reader for `MessageOrigin`
- `rust/fontaine-tui/src/{app.rs,commands.rs}`, `rust/fontaine-tui/src/ui/{mod.rs,turn.rs}`
- `rust/fontaine/src/cli.rs`, `send_cmd.rs` (new), `main.rs`
- `AGENTS.md` (project layout crate list; both places the terminal-confirmation wording for
  `/guardian-disable` and the floor appears), `tasks/fontaine_harness_plan.md` (workspace layout,
  `ci.yml` step set, and the Decisions entry at the end of the correct subject subsection),
  `docs/remote-inbox.md` (new), `docs/SUMMARY.md`, `docs/index.md`, `docs/agents.md`,
  `docs/tui.md` (command list and help), `CHANGELOG.md`

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
- 2026-11-14: a remote line is full terminal input (slash commands, mode changes, approval answers), because the feature is the first slice of remote control; there is no exception, `/guardian-disable` included (confirmation still required, toggles still audited with remote origin). AGENTS.md and the master plan's Decisions are updated to say the terminal may be remote.
- 2026-11-14: AFK: one ticket = one line; a multi-step interaction (a `/guardian-disable` confirmation, an approval answer) needs another ticket/line, so a remote `/guardian-disable` leaves its confirmation pending for the next input; a new ticket revokes the previous unconsumed one; lockout is per remote node id; checks run secret, expiry, reuse.
- 2026-11-14: AFK: remote messages are remote-tagged `Steering` entries (no separate queue) with the typed hook, ordering and `hold`/`announced` rules; draft restore skips them and they never enter prompt history; remote lines use the typed dispatch via a new `Input` variant for submitted lines.
- 2026-11-14: AFK: the ledger gets no `MessageOrigin::Remote` (older builds fail on unknown enum values); `Record::UserMessage` gains an optional `remote { node, message_id }` field and `Record::GuardianOff` an optional origin, both serde-defaulted.
- 2026-11-14: AFK: remote audit uses `record_session_event`-style `remote_inbox` records with `Tier::Human` (no new tier); `fontaine-remote` stays core-free and reports through `Delivery::accept`/`on_reject`.
- 2026-11-14: AFK: a remote line is treated exactly like a typed line under AFK (never held; `/afk`, mode commands and the AFK refusals apply as typed) and resets the AFK stop streak.
- 2026-11-14: AFK: the ticket and `/remote-inbox on|ticket` output are never written to the ledger, audit log, history, export or telemetry; a remote `/remote-inbox ticket` is refused; `fontaine send` also reads the ticket from stdin or `FONTAINE_TICKET`.
- 2026-11-14: AFK: `/remote-inbox` is a `main_only` slash command registered in `fontaine-tui/src/commands.rs`; no new hook is added.
- 2026-11-14: AFK: relay mode is the user-scope-only key `[remote] relay` (`default` = n0 relays, `disabled` = direct only), default `default`; it never enables the inbox.

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
remote `Steering` entries: bounds, FIFO with typed entries, skipped by draft restore, absent from
prompt history; parked root wait wakes for a remote message; tool-result delivery then ledger
`UserMessage` with `remote` in order; remote text is present in guardian human intent; `/` lines
dispatch as slash commands, a mode change from a remote line takes effect, a remote line during
an open approval dialog answers it, a remote line during a `/guardian-disable` confirmation
resolves it, a remote `/guardian-disable` leaves its confirmation pending, and the toggle is
audited and ledgered with remote origin; remote `/remote-inbox ticket` is refused; the ticket
never appears in ledger, audit or history; prompt-submit hook block is audited; remote lines are
not held while AFK is blocked and reset the stop streak; the first remote line may become the
session name/`first_prompt`; audit records for accept, reject, enable, disable and hook-dropped
message; teardown on `off`, `/clear` and resume.

Back-compat: an old `UserMessage` line parses; a new line with `remote` parses; an older-shaped
reader (struct without the field) ignores it; `GuardianOff` likewise.

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

- Which iroh version to pin? Resolve against the iroh release current at implementation.
- Should `--resume` of a session offer to re-enable? Default: no, always explicit.
