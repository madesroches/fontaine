# Remote Control Surface 1: `fontaine attach <ticket>` Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/664

## Overview

`fontaine attach <ticket>` opens a full TUI on another machine that drives a running host
session over iroh. The host starts with `fontaine --remote` or enables it with `/remote`; the
first connection pairs the device (one-time secret in the ticket, explicit confirmation in the
host terminal, NodeId stored in the user-scope `paired_devices.json`, revocable); later
connections rediscover the host by NodeId. One bidirectional QUIC stream per device carries
client-to-host input (prompt, approval answer, question reply, interrupt, slash command) and
host-to-client output (transcript events, `ApprovalRequest`, notices, a snapshot with
resume-from-sequence). The floor and guardian stay host-side; a dropped connection is Deny.
This supersedes the one-shot `fontaine send` / `/remote-inbox` (#627), which is folded into
attach.

## Current State

- `fontaine-remote` (core-free) holds the #627 one-shot transport: `protocol.rs` (ALPN
  `fontaine/send/1`, length-prefixed JSON frames, `Reason`), `ticket.rs` (`fontaine-send1…`),
  `inbox.rs` (`RemoteInbox`: one-time secret hash, expiry, per-node lockout, `Delivery` trait),
  `server.rs` (`RemoteServer`, per-enable endpoint key, accept loop), `client.rs` (`send`).
- `fontaine-tui` hosts it: `ui::RemoteState`, `Input::Remote` wake-up, `DriveOptions::wake`,
  remote-tagged `Steering` entries (`RemoteSender { node, message_id }` on
  `Record::UserMessage` / `GuardianOff`), dialog answering by exact option label, audit tool
  `remote_inbox`, `/remote-inbox on|off|status|ticket`. `fontaine/src/send_cmd.rs` is the CLI.
- `fontaine_tui::drive()` (`fontaine-tui/src/lib.rs`) owns the session loop: a private
  `ChannelApprover` turns approvals/questions/elicitations/notices into `Prompt` values for the
  UI loop, `Input` (keys, paste, resize, remote wake) is the input channel, and a `render`
  closure is the render sink. This is the seam a remote surface plugs into.
- No device identity is persisted: the endpoint key is minted per enable. No `fontaine --remote`
  flag, `/remote` command, `attach` subcommand or `paired_devices.json` exists.
- Decisions of #627 that attach changes: a ticket-holding line was presumed user input with no
  device confirmation; the secret was single-message.

## Design

### 1. Protocol (`fontaine-remote`, still core-free)

- New ALPN `fontaine/attach/1`; the `fontaine/send/1` ALPN, `Request/Response` one-shot types
  and `client::send` are removed (see Decisions). Framing (`read_frame`/`write_frame`, `MAX_FRAME`,
  timeouts) is reused.
- One bidirectional stream per device connection, carrying a sequence of length-prefixed JSON
  messages. First client message is `Hello { v, auth }` with `auth = Pair { secret } |
  Resume { last_seq: Option<u64> }`. Authentication of a device is the iroh connection's
  authenticated remote NodeId, never a field the client writes.
- Client → host: `Prompt{text}`, `ApprovalAnswer{id, answer}`, `QuestionReply{id, reply}`,
  `Interrupt`, `Slash{line}`, `Ping`. Host → client: `Snapshot{seq, state}`, `Event{seq, event}`,
  `ApprovalRequest{id, …}`, `Question{id, …}`, `Notice{severity, text}`, `Paired`,
  `Rejected{reason}`, `Bye`.
- `Reason` gains `NotPaired`, `PairingDeclined`, `PairingTimeout`, `Revoked`, `Superseded` to
  the existing ticket reasons. Version mismatch is `UnsupportedVersion`.
- Event payloads (`RemoteEvent`) are a serde mirror of what the host UI loop already feeds its
  `App`: transcript entries, status-bar state, prompt/dialog open and close, mode and model.
  The mirror type lives in `fontaine-tui` (it needs `App`/`Transcript` types) and is carried by
  `fontaine-remote` as opaque `serde_json::Value`, keeping the crate core-free. Every event gets
  a host-assigned monotonic `seq`; the host keeps a bounded ring of recent events
  (default 4096) so `Resume{last_seq}` replays the gap, and falls back to a full `Snapshot`
  when the gap fell out of the ring.

### 2. Device identity, pairing and revoke

- Host key: a persistent iroh secret key in the user-scope data dir (`remote_host.key`, 0600 on
  Unix, user-only ACL on Windows), created at the first `--remote`/`/remote`. It replaces the
  per-enable key so a paired device can find the host again by NodeId (see rediscovery).
- Client key: a persistent device key in the data dir (`attach_device.key`), same protection.
- Pairing: `/remote` / `--remote` mints a ticket (existing `Ticket` + `RemoteInbox` secret
  rules: one-time, TTL default 10 min, max 24 h, per-node lockout, constant-time compare,
  check order secret → expiry → reuse). The ticket is shown once under the #627 secrecy rules
  (dismissible modal / one direct linear print; never in ledger, audit, history, export,
  telemetry). A valid `Pair` secret does not authorize anything by itself: the host opens a
  confirmation dialog showing the device NodeId (short form) and the client's self-reported
  label, and only an explicit local-terminal "yes" stores the NodeId. The dialog is **not**
  remote-answerable (a remote device cannot confirm its own pairing); no answer in 60 s or a
  disconnect is `PairingDeclined`/`PairingTimeout`, and the secret is consumed on first valid
  use either way.
- `paired_devices.json` (user-scope data dir, atomic write, serde-versioned): `[{ node, label,
  paired_at, last_seen }]`. Loaded by a `PairedDevices` store in `fontaine-remote` (file
  path injected). Project scope can never write or read it.
- Revoke: `/remote devices` lists, `/remote revoke <node-prefix|label>` removes the entry and
  closes that device's live connection (`Revoked`); `fontaine remote devices|revoke` does the
  same offline against the file. A revoked NodeId needs a new pairing.
- Reconnect of a paired NodeId needs no ticket: `Hello{Resume}` is accepted when
  `remote_id` is in `paired_devices.json`. An unpaired NodeId without a valid `Pair` secret
  gets `NotPaired` and counts toward the per-node lockout.
- One live device connection per NodeId; a second connection from the same NodeId supersedes
  the first (`Superseded`). Several different paired devices may be attached at once.

### 3. Host surface over the `drive()` seam

- Add a second input channel and a render sink to `fontaine_tui::drive()` via a new
  `RemoteSurface` (in `fontaine-tui`, next to `ui::RemoteState`):
  - **Output**: the UI loop already produces every state change for its `App`; each is also
    turned into a `RemoteEvent`, stamped with `seq`, appended to the ring and fanned out to
    every attached device. No second render path: devices render from events.
  - **Input**: device messages become host input through the existing remote-tagged `Steering`
    entries (`Prompt`, `Slash`) and a new `Input::RemoteControl` wake-up for `Interrupt`.
    Remote entries keep the #627 rules (own submission, never in the draft or history, ledger
    `remote: { node, message_id }`).
- `RemoteApprover`: same shape as `ChannelApprover`; `approve`, `ask_user`, `elicit*` forward
  the request to every attached device **and** to the local UI. The first answer wins (local or
  any device); the others get a `Resolved{id}` event that closes their dialog. The answering
  device's NodeId goes in the ledger and audit record (`remote` field, audit `explanation`).
  With no local UI and no device attached, or when the last attached device disconnects while
  an approval is open and no local dialog can answer, the answer is Deny. A disconnect never
  silently approves, never leaves a call hanging.
- Dialog answers from a device are typed (`ApprovalAnswer`, `QuestionReply`) and applied to the
  open request by `id`, replacing #627's exact-label text mapping for attached devices.
  `Prompt` lines that happen to read `y` stay ordinary messages.
- Slash commands from a device run under the `main_only` rules and a device allow-list: the
  same set a local user can run, **except** `/guardian-disable`, `/guardian-enable`, `/remote*`
  (minting or pairing), `/config`, and any command that opens a host-only picker. A refused
  command returns a `Notice`, writes an audit record and never reaches the dispatcher.
- Interrupt from a device behaves as Esc locally.
- `/guardian-disable` is never available from a remote device (both the slash path and any
  confirmation dialog). This supersedes the #627 Decision that allowed it from a remote
  terminal; AGENTS.md, SECURITY.md, the master plan Decisions and the docs are amended.

### 4. Client: `fontaine attach <ticket>`

- `fontaine attach <ticket-or-node>`: with a ticket, connect, `Pair`, wait for host
  confirmation, store the host (`attach_hosts.json`: host NodeId, label, last addresses) and
  attach. With a stored host label or NodeId and no ticket, `Resume`. Ticket sources match
  `send`: argument, `--ticket-stdin`, `FONTAINE_TICKET`.
- The client runs the TUI in remote mode: terminal reader → `Input` keys → a local
  `RemoteClientUi` that applies `Event`s to an `App` and renders with the existing renderers,
  and translates submit/Esc/dialog choices into client messages. It runs no agent, loads no
  provider credentials and no project config. Local-only features (editor handoff, clipboard
  images, `/config`) say they are host-side.
- Reconnect: on connection loss the client shows "reconnecting" and retries with backoff,
  `Resume{last_seq}`; the host replays from the ring or sends a `Snapshot`. Exit with Ctrl+D
  or `/detach`; the host session continues.
- Non-interactive mode folds in `fontaine send`: `fontaine attach <ticket|host> --message
  <text|->` attaches, sends one `Prompt`, waits for the host's `Event` acknowledging it was
  queued, prints `accepted <message_id>` and exits with the existing exit-code table (2 ticket/
  pairing/auth, 3 full/too large, 4 connection, 5 protocol). `fontaine send` is removed.

### 5. Rediscovery

The host publishes its address via iroh address lookup under the persistent key only when
`[remote] relay = "default"`; the client dials the stored NodeId with lookup enabled, falling
back to the stored last-known addresses. `relay = "disabled"` stays direct-only and publishes
nothing (documented: rediscovery then works only on a reachable address). Tickets still carry
addresses for the first pairing so no lookup is needed to pair.

### 6. Enabling

- `fontaine --remote` starts the TUI with the endpoint live and prints the ticket through the
  same one-shot secret display; `/remote on|off|status|ticket|devices|revoke` is the slash
  command (`/remote-inbox` is removed). Enabling stays user-only: not config, not project
  scope, not a tool, not headless, never re-enabled by `--resume`. `/clear`, `/resume`,
  `/branch` and quit disconnect devices (`Bye`) and tear the endpoint down; paired devices
  survive in `paired_devices.json` and reattach after `/remote on` (or the next `--remote`).
- `[remote] relay` stays user-scope-only.

### 7. Invariants kept

Floor, guardian and workspace scope run host-side on every device-induced call; no regex on
arguments; no new tool (choke-point test unchanged); a device never confirms its own pairing;
`/guardian-disable` is unreachable from a device; a lost connection is Deny; project scope can
only tighten and cannot touch `[remote]` or `paired_devices.json`.

## Implementation Steps

1. **Spike (gates the rest)**: enumerate every value the UI loop applies to `App` and every
   `Prompt`/`Input` it consumes; define `RemoteEvent` and prove a client `App` built purely from
   events renders an identical transcript to the host in a `TestBackend` test. If events cannot
   cover a surface, record it as a limitation in docs and drop it from the snapshot.
2. `fontaine-remote`: new `protocol` messages and ALPN; `paired` store (`PairedDevices`);
   persistent key helpers; `server` rewritten around per-device sessions with a `HostSession`
   trait implemented by the TUI; `client` attach/reconnect; remove one-shot `send`.
3. `fontaine-core`: audit/ledger helpers for the answering device (`remote` on approval
   outcome records if absent); no schema break (serde-defaulted optional fields).
4. `fontaine-tui`: `RemoteEvent` mirror and event ring, `RemoteSurface`, `RemoteApprover`
   (first answer wins, disconnect = Deny), pairing confirmation dialog (local only), device
   slash allow-list, `/remote` command family replacing `/remote-inbox`, status indicator
   with device count, `RemoteClientUi` for the client.
5. `fontaine`: `--remote` flag, `attach` and `remote devices|revoke` subcommands, remove
   `send_cmd.rs`, wire client key/hosts files.
6. Docs, changelog, AGENTS.md / SECURITY.md / master plan edits, `cargo deny`, Windows
   `cargo check --target x86_64-pc-windows-gnu`.

## Files to Modify

- `rust/fontaine-remote/src/{lib,protocol,ticket,inbox,server,client}.rs`, new
  `src/{paired,keys,session}.rs`, `rust/fontaine-remote/tests/*`
- `rust/fontaine-tui/src/{lib,app,commands,shutdown,statusbar,view}.rs`,
  `rust/fontaine-tui/src/ui/{mod,turn,commands/*}.rs`, new `src/remote/{event,surface,approver,client_ui}.rs`
- `rust/fontaine-core/src/{audit,gate,ledger}.rs` (answering-device identity)
- `rust/fontaine/src/{cli,lib,main}.rs`, new `attach_cmd.rs`, delete `send_cmd.rs`
- `rust/Cargo.toml`, crate manifests, `rust/deny.toml` if licenses change
- `AGENTS.md`, `SECURITY.md`, `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`,
  `docs/{remote-inbox→remote,SUMMARY,index,tui,guardian,afk,terminal,configuration,agents}.md`

## Trade-offs

- **Event mirror vs. streaming rendered terminal frames**: frames are simpler but tie the
  client to the host's terminal size, cannot reflow or scroll natively, and ship every redraw;
  events give a native client TUI and resume-from-sequence. Cost: a serde mirror that must track
  `App` changes, mitigated by the step-1 equivalence test.
- **Persistent host key vs. per-enable key**: rediscovery by NodeId requires a stable identity;
  the cost is a long-lived key file, mitigated by the pairing list being the real authorization.
- **Fold `send` into `attach --message` vs. keep both**: one protocol and one trust model
  instead of two; scripted use now needs a one-time pairing.
- **Typed dialog answers vs. text mapping**: removes the exact-label heuristic for devices.

## Decisions

- AFK: `fontaine send`, the `fontaine/send/1` ALPN and `/remote-inbox` are removed, not kept as
  aliases; `attach --message` is the one-shot path. Reason: the issue says attach supersedes it,
  and a second trust model (unpaired bearer ticket = user) contradicts device pairing.
- AFK: `/guardian-disable` and `/guardian-enable` are refused from any device, reversing the
  #627 allowance; the master plan Decision, AGENTS.md and SECURITY.md wording are updated.
- AFK: a pairing confirmation is local-terminal only and not remote-answerable; a secret is
  consumed on first valid use even if the host declines.
- AFK: with no local UI able to answer and no device attached, an open approval is Deny.
- AFK: the event ring defaults to 4096 events; a gap beyond it sends a full snapshot.
- AFK: `paired_devices.json` and the host key are user-scope data-dir files; project scope
  cannot read, write or influence them.

## Documentation

`docs/remote-inbox.md` becomes `docs/remote.md` (enabling, pairing, devices and revoke,
reconnect, trust model, relay privacy, rediscovery limits, `attach --message` and exit codes);
update `SUMMARY.md`, `index.md`, `tui.md`, `guardian.md`, `afk.md`, `terminal.md`,
`configuration.md`, `agents.md`; CHANGELOG entry noting the removal of `fontaine send`.

## Testing Strategy

Unit (no network): pairing store round trip and atomic write; unpaired NodeId gets `NotPaired`
and counts toward lockout; replayed pairing secret is `Reused`; secret consumed on decline;
revoke closes the live connection; device slash allow-list refuses `/guardian-disable`,
`/guardian-enable`, `/remote*` and never reaches dispatch; first-answer-wins with local and
device answers; mid-approval disconnect is Deny and a second device can still answer;
last-device disconnect with no local UI is Deny; the answering NodeId is in ledger and audit;
event ring resume from sequence and snapshot fallback; `RemoteEvent` serde round trip;
host/client `TestBackend` transcript equivalence; ticket never in ledger/audit/history/export.

Integration (`fontaine-remote/tests`, in-process iroh endpoints, relay disabled and the local
test relay): pair → confirm → attach → reattach by NodeId with no ticket; declined pairing;
reconnect with `Resume`; superseded connection; `attach --message` exit codes. A real network
catches NAT/relay behaviour and address-lookup rediscovery a loopback test cannot; manual.

## Manual Verification

1. Two machines: `fontaine --remote`, `fontaine attach <ticket>`, confirm on the host, drive a
   prompt and an approval from the client. Why manual: real network paths and a real terminal.
2. Kill the client's network mid-approval; the host call is denied. Reattach; transcript resumes.
3. `/remote revoke`, then attach again; expect `Revoked`/`NotPaired`.

## Open Questions

- Does address lookup (rediscovery) need a self-hosted service for users who disable the n0
  relays? Out of scope here; direct-only users reattach via stored addresses.
