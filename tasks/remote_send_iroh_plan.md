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
- Human input while a turn runs goes through `Steering` (`fontaine-core/src/steering.rs`): queued
  entries ride the root's next tool result and only then enter the ledger as
  `Record::UserMessage`, so the guardian never judges by words the model has not read. Undelivered
  entries can be restored into the human's input on interruption, which is why remote text must
  **not** use this queue (it could prepend to the draft).
- `Record::UserMessage { text, attachments, agent, origin }` (`ledger.rs:93`) carries a
  `MessageOrigin` (`Other`, `Dialog`, `Prompt`, `ledger.rs:38`) that decides what the guardian may
  read into it. Typed prompts are recorded in `fontaine-tui/src/ui/mod.rs` / `turn.rs` before the
  turn starts.
- The root's agent-mail inbox (`AgentTree::post_to_root`, `tree.rs:1783`; `Notification::render`
  wraps text as "not the user") is the wrong channel here: it is the untrusted-agent contract.
- The TUI loop starts a turn on idle work (`has_notifications` at `fontaine-tui/src/ui/mod.rs:755`,
  scheduled prompts, goal steps), all suppressed while AFK is blocked; the draft is never involved.
- CLI subcommands live in `fontaine/src/cli.rs` (`Command` enum, L157). Slash commands live in
  `fontaine-tui/src/ui/commands/`.
- Audit log and ledger: `fontaine-core/src/audit.rs`, `ledger.rs`.

## Design

### 1. New crate `fontaine-remote` (under `rust/`)

Keeps the iroh dependency tree out of `fontaine-guardian` and `fontaine-core`'s hot path. Depends
on `iroh` (exact pin, one version for the workspace; verify license/advisories with `cargo deny`),
`tokio`, `serde`, `serde_json`, `postcard`-free: the wire format is JSON for debuggability.
Added as a workspace member and dependency of `fontaine` (bin) and `fontaine-tui`; the core
stays iroh-free by receiving messages through a trait.

Modules:

- `protocol.rs` — ALPN `fontaine/send/1`. One bidirectional stream per request.
  `Request { v: u16, secret: String, message_id: String, text: String }`;
  `Response { v, status: Accepted{message_id} | Rejected{reason} }` with
  `Reason = BadTicket | Expired | Reused | UnknownSession | TooLarge | InboxFull | Closed |
  UnsupportedVersion | Malformed`. Frames are length-prefixed JSON with a hard read cap
  (`MAX_FRAME`), read under a timeout.
- `ticket.rs` — `Ticket { v, node: EndpointAddr (node id + relay url + direct addrs), session:
  SessionId, secret: [u8;32], expires_at }`, text form `fontaine-send1…` (base32 of JSON).
  Parse errors are typed (`TicketError`) because the CLI maps them to messages.
- `inbox.rs` — `RemoteInbox`, the server side state: the secret (stored only as a SHA-256 hash
  in memory), expiry, `consumed` flag, and a `seen: HashMap<message_id, MessageId>` for retry
  idempotency. `Delivery` trait (implemented by the TUI/headless host over `AgentTree`):
  `fn accept(&self, from: Sender, text: String) -> Result<(), DeliveryError>` where
  `DeliveryError = Full | Closed`.
- `server.rs` — `RemoteServer::start(inbox, delivery, clock, relay_mode) -> (Ticket source,
  handle)`; accept loop; one task per connection; `shutdown()` closes the endpoint with an
  application error code so in-flight clients see `Closed`.
- `client.rs` — `send(ticket, text, opts) -> Result<Accepted, SendError>`; connects, writes the
  request, waits for the response with a timeout.

### 2. Auth, one-time secret, idempotency

- A ticket is minted by `/remote-inbox on` (lifetime default 10 minutes, `--ttl`, max 24 h). The
  secret is **one-time per sender session**: the first successful request *consumes* the secret
  and binds the sender's iroh node id as the authenticated sender identity. Retries by the same
  node id with the same `message_id` return the original `Accepted` (lost acknowledgement
  case, exactly-once queueing); any other request with the used secret → `Reused`.
  Further messages need a new ticket (`/remote-inbox ticket`), which is what "one-shot" means.
- Secret comparison is constant-time; a wrong secret never reveals whether the session exists
  (`BadTicket`), and after 5 bad attempts the server stops accepting for 30 s.
- Expiry is checked on the server clock (injectable for tests).
- A ticket for another session id gets `UnknownSession`; the endpoint is per session and is
  torn down on exit, `/clear`, `--resume` switch and `/remote-inbox off`, so a stale ticket
  cannot reach a later session (the endpoint key is minted fresh per enable, never persisted).

### 3. Bounded delivery as a remote user message

A new `RemoteQueue` in `fontaine-core` (beside `steering.rs`, same `Arc<Mutex>` shape, poison
panics because it feeds guardian intent) holds accepted messages: FIFO, capped at 16 entries and
8 KiB per message (`MAX_REMOTE_MESSAGE_BYTES`), both checked **before** the ack. It is separate
from `Steering`, so nothing here is ever restored into the human's input.

- **Running**: the root's next tool result carries the queued texts (same hook point as steered
  messages), wrapped with a one-line provenance header (`[Remote message from <short node id>]`),
  and only then does each enter the ledger as `Record::UserMessage` with
  `origin: MessageOrigin::Remote` and `remote: { node, message_id }` (new optional field).
  Ordering relative to human steered entries is arrival order at delivery time.
- **Idle**: the TUI loop treats a non-empty `RemoteQueue` like a due scheduled prompt: it starts a
  normal user turn from the message text, with the `UserMessage` recorded before the turn so the
  guardian reads it as intent. Suppressed while AFK is blocked (the message stays queued and is
  delivered when AFK resumes; the sender already got `accepted`, which means queued).
- **Prompt-submit hooks** run on it as on a typed prompt; a hook block drops the message with an
  audit record (the sender was already acked: documented).
- **Guardian**: `MessageOrigin::Remote` counts as human intent (it is a user message). Because the
  ticket is a bearer credential, that is the deliberate widening of trust this slice makes; the
  sender label is the authenticated node id, never payload text, and is shown in the transcript.
- **Remote is a terminal, not just a prompt**: the line is submitted through the same dispatch
  as a line typed at the prompt (`submit`, `fontaine-tui/src/ui/mod.rs:1026`), so slash commands
  (`/permissions`, mode changes, `/compact`...), skills and `!` lines work, and a pending
  approval or question dialog can be answered by a remote line. It never goes through the
  `Input` key/paste events or the human's input buffer, so the draft and its cursor are untouched.
  There is no exception: `/guardian-disable` and `/guardian-enable` work from a remote line too,
  including their explicit confirmation step, since the remote line is the user's terminal. They
  remain blocked in headless runs (which cannot enable the inbox anyway) and are still recorded in
  the ledger and audit log, with the remote origin noted. This extends AGENTS.md's
  "after an explicit confirmation in the terminal UI" to a remote terminal; update that wording
  and the master plan's Decisions entry for `/guardian-disable` in the same change.

### 4. Enabling and disabling (local, explicit, user-only)

- `/remote-inbox [on|off|status|ticket]` slash command (`commands/remote.rs`), interactive TUI
  only. Never enabled by config or project scope; no config key exists. In headless runs
  there is no way to enable it (documented).
- `on` starts the `RemoteServer` for the current session, mints a ticket and prints it once;
  the status line shows an indicator while enabled. `off`, session end, `/clear`, resume switch
  and quit shut it down.
- Enabling is a user command, not a tool: the model cannot call it.

### 5. CLI: `fontaine send <ticket> <text>`

New `Command::Send(SendArgs)` in `cli.rs`, handler in `fontaine/src/send_cmd.rs`. Text may be
`-` for stdin. Prints `accepted <message_id>` and exits 0; failures print one line from the
typed reason and exit nonzero with distinct codes for `Expired`/`Reused`/`BadTicket` (2),
`InboxFull`/`TooLarge` (3), connection/timeout (4). `message_id` is generated client-side per
invocation (`msg-<uuid>`), and the client retries once on a lost ack with the same id. It
needs no config, session or provider credentials.

### 6. Audit and ledger

Each accepted or rejected request writes an audit record (sender node id, message id, size,
outcome) via `audit.rs`. Delivery writes the `UserMessage` ledger record with
`origin: Remote` and the sender/message identity (see section 3); older readers that predate the
field ignore it via serde defaults, and `MessageOrigin::Remote` is added so resume replays it.

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
2. `fontaine-core`: `RemoteQueue`, `MessageOrigin::Remote` + remote identity on `UserMessage`, tool-result delivery hook.
3. Wire `Delivery` for the TUI host over `RemoteQueue`; idle-turn start in the TUI loop; hook run; audit records.
4. `/remote-inbox` command and status indicator; teardown on clear/switch/quit.
5. `fontaine send` CLI.
6. Docs, changelog, `cargo deny` config for iroh's licenses.

## Files to Modify

- `rust/Cargo.toml`, `rust/deny.toml`, `rust/fontaine/Cargo.toml`, `rust/fontaine-tui/Cargo.toml`
- `rust/fontaine-remote/**` (new)
- `rust/fontaine-core/src/{remote_queue.rs (new),steering.rs,ledger.rs,audit.rs}` and the guardian's intent reader for `MessageOrigin`
- `rust/fontaine-tui/src/ui/{mod.rs,turn.rs}`
- `rust/fontaine/src/cli.rs`, `send_cmd.rs` (new), `main.rs`
- `rust/fontaine-tui/src/ui/commands/{mod.rs,remote.rs}`
- `AGENTS.md` (floor invariant wording), `tasks/fontaine_harness_plan.md` (Decisions, layout),
  `docs/remote-inbox.md` (new), `docs/SUMMARY.md`, `docs/index.md`, `docs/agents.md`,
  `docs/tui.md`, `CHANGELOG.md`

## Trade-offs

- **New crate vs. inside `fontaine-agents`**: iroh pulls a large tree (QUIC, TLS, relay
  client); isolating it keeps compile time and the audit surface off the guardian-adjacent crates.
- **Secret consumed on first use vs. valid for N messages**: matches the issue's "one-time
  secret" and "one-shot"; lost acks are handled by `message_id` idempotency rather than by
  keeping the secret alive.
- **Separate `RemoteQueue` vs. the steering queue or root agent mail**: steering can restore text
  into the human's draft; agent mail carries the untrusted contract the user rejected for this
  slice.
- **Presumed user vs. untrusted mail**: the user chose the presumption; a ticket is a bearer
  credential for their own devices, so the mitigation is a short, one-time ticket, not a weaker
  message.
- **Minimal ingress now vs. waiting for #580**: this slice must ship alone; `RemoteQueue` is
  deliberately small so #580 can replace it.

## Decisions

- 2026-11-14: remote messages are presumed to come from the user (user message, guardian intent, prompt hooks), superseding the issue's original untrusted-agent-mail wording; the issue body was updated to match.
- 2026-11-14: a remote line is full terminal input (slash commands, mode changes, approval answers), because the feature is the first slice of remote control; there is no exception, `/guardian-disable` included (confirmation still required, toggles still audited with remote origin). AGENTS.md and the master plan's Decisions are updated to say the terminal may be remote.

## Documentation

- New `docs/remote-inbox.md`: enabling/disabling, ticket lifetime, presumed-user trust semantics and the ticket's bearer-credential risk, delivery
  guarantees, what "accepted" means (queued, not model-delivered or completed), error table,
  direct vs relayed, privacy of relay use. Linked from `SUMMARY.md`, `index.md`; cross-link
  from `docs/agents.md` ("Messages between agents") and `docs/tui.md` (slash commands).

## Testing Strategy

Unit (no network): ticket encode/parse round trip and malformed/expired/version cases; secret
consume and reuse; idempotent retry returns the same `message_id` without a second enqueue;
frame cap and oversized text; rate lock after bad attempts; inbox cap → `InboxFull`;
`RemoteQueue` bounds, FIFO and never-restored-into-draft; tool-result delivery then ledger
`UserMessage{origin: Remote}` in order; remote text is present in guardian human intent; `/` lines dispatch as slash commands, a mode
change takes effect, a pending approval is answered by a remote line, `/guardian-disable` from a
remote line requires its confirmation and is audited with remote origin; prompt-submit hook block; AFK-blocked hold; audit records;
`MessageOrigin` serde back-compat.

Integration (`fontaine-remote/tests`): server and client in one process on local iroh endpoints
with relay disabled (direct on loopback) and with a local test relay in-process (forced-relay
path); no external relay or live provider. Cases: accepted, expired, reused, invalid, unknown
session, oversized, full, shutdown mid-flight, dropped ack then retry (exactly once). TUI test
(`TestBackend`) with a scripted model: delivery while running (arrives with next tool result),
while idle (a user turn), and a multiline draft plus queued input preserved byte for byte.
Real-network qualification catches NAT/relay behavior a loopback test cannot; it is manual.

## Manual Verification

1. Two machines on one LAN: `/remote-inbox on`, run `fontaine send <ticket> "hi"`; expect
   `accepted msg-…` and the message in the session. Why manual: needs real network paths.
2. Block direct UDP (firewall) between them and repeat; expect success via relay.
3. Type a draft in the target, send, confirm the draft is untouched.

## Open Questions

- How is a remote line matched to a pending approval or question dialog (the next remote line
  answers the oldest open dialog; non-matching text is queued as a prompt)? Default: the next
  remote line answers the open dialog if it parses as an option, else it is queued.

- Which iroh version and relay default (n0 public relays vs. configurable `[remote] relay_url`
  in user-scope config)? Default to n0's and document the metadata exposure; resolve against the
  iroh release current at implementation.
- Should `--resume` of a session offer to re-enable? Default: no, always explicit.
