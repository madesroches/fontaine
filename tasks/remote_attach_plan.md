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
  and `client::send` are removed (see Decisions). Framing (`read_frame`/`write_frame`) is reused
  with attach-specific limits: on the attach stream `FRAME_TIMEOUT` (10 s) covers only the body
  read once a 4-byte prefix has arrived, never the wait for the prefix. Both sides send `Ping`
  every 15 s; a side that has received nothing for 45 s closes the connection as lost. The attach
  ALPN has its own frame caps: 4 MiB host → client, 64 KiB client → host. A `Snapshot` larger
  than the host → client cap is sent as consecutive `Snapshot{seq, part, of, state}` chunks each
  within the cap, applied by the client only when all parts have arrived; an `Event` over the cap
  has its oversized text fields truncated (marked) by the `RemoteEvent` mirror.
- One bidirectional stream per device connection, carrying a sequence of length-prefixed JSON
  messages. First client message is `Hello { v, label, auth }` with `auth = Pair { secret } |
  Resume { last_seq: Option<u64> }`. Authentication of a device is the iroh connection's
  authenticated remote NodeId, never a field the client writes.
- Client → host: `Prompt{message_id, text}`, `ApprovalAnswer{id, answer}`, `QuestionReply{id, reply}`,
  `Interrupt`, `Slash{line}`, `Ping`. Host → client: `Snapshot{seq, part, of, state}`, `Event{seq, event}`,
  `ApprovalRequest{id, …}`, `Question{id, …}`, `Notice{severity, text}`, `Resolved{id}`,
  `Accepted{message_id}`, `PromptRejected{message_id, reason}`, `Paired`, `Rejected{reason}`,
  `Bye`.
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
- `fontaine-remote` does no file I/O for the host key, the device key or `paired_devices.json`:
  it takes key bytes and the device list through an injected store trait. The TUI/bin side reads
  and writes them with `fontaine_core::owner_only::{write_atomic,create_new}`.
- One session is the remote host at a time, enforced by `fontaine_core::file_lock::try_lock` on a
  `remote_host.lock` file in the data dir, held while remote is enabled. A second session's
  `--remote` / `/remote on` refuses, saying another session is the remote host.
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
  paired_at, last_seen }]`. `fontaine-remote` holds the `PairedDevices` data type and the store
  trait; the file is read and written host-side. Every update runs under `file_lock`. Project scope can never write or read it.
- Revoke: `/remote devices` lists, `/remote revoke <node-prefix|label>` removes the entry and
  closes that device's live connection (`Revoked`); `fontaine remote devices|revoke` does the
  same offline against the file; the running host re-checks the file on every client
  message and every keepalive and closes a now-revoked live connection (`Revoked`). A revoked
  NodeId needs a new pairing.
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
- Approvals, questions and elicitations keep going through `ChannelApprover` and the UI loop;
  devices see dialogs only through `RemoteEvent`. Typed device answers (`ApprovalAnswer`,
  `QuestionReply`) are a loop input applied to the open `App` dialog by `id` (as
  `take_remote_answer`/`answer_remote_dialog` do today), replacing #627's exact-label text
  mapping. A typed answer is accepted only for a dialog of kind approval or question; the
  device-answerable dialogs are exactly those two. Elicitation, guardian-unavailable,
  usage-pause, server prompts, the `/afk review` confirmation, the pairing confirmation and the
  `/guardian-disable` confirmation are local-only: a device sees them as a "waiting on the host"
  notice via `RemoteEvent` and cannot answer them; an answer naming one is refused, audited, and
  leaves the dialog pending. Once typed answers exist, `DialogHold`,
  `Steering::open_dialog`/`take_dialog_answer` and the `held` filter no longer apply to device
  entries: a device `Prompt` sent while a dialog is open is queued like any other message and
  never answers the dialog. The first answer wins (local or any device) because the loop sends the oneshot once;
  the other devices get a `Resolved{id}` event that closes their dialog. This meets the issue's
  "same shape as `ChannelApprover`". The answering device's NodeId goes in the ledger and audit
  record (`remote` field, audit `explanation`). A device disconnect closes only that device's
  dialog; the local approval stays pending (if the host UI is quit, the session ends and the
  call is denied as today). A dropped connection never approves. `Prompt` lines that happen to
  read `y` stay ordinary messages.
- Slash commands from a device are gated by an explicit exhaustive `Command::device_allowed()`
  match with no wildcard arm (precedent: `main_only()`/`runs_mid_turn()` in
  `fontaine-tui/src/commands.rs`), so a new variant fails to compile until classified; they also
  run under the `main_only` rules. Allowed (with or without argument): `Btw`, `Cost`, `Recap`,
  `Compact`, `Context`, `Loop`, `Goal`, `Init`, `Rename`, `Help`, `Status`, `Other` (skills).
  Refused: `GuardianDisable` (including its confirmation dialog), `GuardianEnable`, `RemoteInbox`
  (replaced by `/remote`, all subcommands), `Afk` (the review form and its confirmation),
  `Theme` (persists config), `Export` (writes a host file at a device-chosen path), `Config`,
  `Clear`, `Resume`, `Branch`, `Rewind`, `Model`, `Effort`, `GuardianModel`, `OutputStyle`,
  `Login`, `Exit`, `AddDir`, `Bug`, `Plugin`, `Plugins`, `ReloadPlugins`, `Skills` (trust form),
  `Mcp`, `Hooks`, `Permissions`, `Agents`, `Transcript`, `Todos`, `Expand`, `Denied`,
  `AutoModeSetup`, `Memory`, `Copy`, `Vim`, `Keybindings` (host panels, toggles, pickers,
  persisted host state or the local terminal). A device sends only `Prompt` or `Slash`; a `Prompt` beginning with `!` or `/`
  is plain text for the model, never a shell line or a command. A refused command returns a
  `Notice`, writes an audit record and never reaches the dispatcher.
- Interrupt from a device behaves as Esc locally.

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
  <text|->` attaches, sends one `Prompt`, waits for the host's `Accepted{message_id}` (or `PromptRejected`), prints `accepted <message_id>` and exits with the existing exit-code table (2 ticket/
  pairing/auth, 3 full/too large, 4 connection, 5 protocol). `fontaine send` is removed.

### 5. Rediscovery

The host publishes its NodeId and addresses via iroh address lookup under the persistent key
only when `[remote] relay = "default"`; the client dials the stored NodeId with lookup enabled, falling
back to the stored last-known addresses. A self-hosted address lookup service is out of scope:
direct-only users reattach via stored addresses. `relay = "disabled"` stays direct-only and publishes
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
`/guardian-disable` is unreachable from a device; a lost connection never approves; project scope can
only tighten and cannot touch `[remote]` or `paired_devices.json`.

## Implementation Steps

1. **Spike (gates the rest)**: enumerate every value the UI loop applies to `App` and every
   `Prompt`/`Input` it consumes; define `RemoteEvent` and prove a client `App` built purely from
   events renders an identical transcript to the host in a `TestBackend` test. If events cannot
   cover a surface, record it as a limitation in docs and drop it from the snapshot.
2. `fontaine-remote`: for `relay = "default"`, publish to address lookup (the server today uses
   `presets::Minimal`, which publishes nothing); the pin is `iroh =1.4.1` with
   `default-features = false, features = ["tls-ring"]`, which lists no lookup-specific feature,
   so the implementer confirms in the spike whether publishing works with it or needs another
   feature enabled in `rust/Cargo.toml`; new `protocol` messages and ALPN; `paired` data type (`PairedDevices`);
   store trait for key bytes and the paired list (no file I/O in the crate); `server` rewritten around per-device sessions with a `HostSession`
   trait implemented by the TUI; `client` attach/reconnect; remove one-shot `send`.
3. `fontaine-core`: audit/ledger helpers for the answering device (`remote` on approval
   outcome records if absent); no schema break (serde-defaulted optional fields).
4. `fontaine-tui`: `RemoteEvent` mirror and event ring, `RemoteSurface`, typed device
   answers as a loop input (first answer wins), the key/paired-file store implementation over
   `fontaine_core::owner_only` and `file_lock`, pairing confirmation dialog (local only), device
   slash allow-list (`Command::device_allowed()`), removal of `DialogHold`/`open_dialog`/
   `take_dialog_answer` for device entries, `/remote` command family replacing `/remote-inbox`, status indicator
   with device count, `RemoteClientUi` for the client; rewrite `tests/remote.rs` and
   `ticket_prompt.rs` for attach.
5. `fontaine`: `--remote` flag, `attach` and `remote devices|revoke` subcommands, remove
   `send_cmd.rs`, wire client key/hosts files.
6. Docs, changelog, AGENTS.md / SECURITY.md / master plan edits, `cargo deny`, Windows
   `cargo check --target x86_64-pc-windows-gnu`.

## Files to Modify

- `rust/fontaine-remote/src/{lib,protocol,ticket,inbox,server,client}.rs`, new
  `src/{paired,session}.rs`, `rust/fontaine-remote/tests/*`
- `rust/fontaine-tui/src/{lib,app,commands,shutdown,statusbar,view,setup,confirm_prompt,approval,question}.rs`,
  `rust/fontaine-tui/src/ui/{mod,turn,remote,commands/*}.rs`, `src/remote.rs` becomes
  `src/remote/mod.rs` with new `src/remote/{event,surface,client_ui}.rs`,
  `rust/fontaine-tui/tests/remote.rs` (rewrite for attach: it drives #627 via
  `fontaine_remote::send` and pins the exact-label dialog answer and remote guardian-disable
  behaviour, both removed), `rust/fontaine-tui/src/ticket_prompt.rs` (mentions `fontaine send`)
- `rust/fontaine-core/src/{audit,gate,ledger,steering}.rs` (answering-device identity; dialog hold)
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

## Decisions

- AFK: `fontaine send`, the `fontaine/send/1` ALPN and `/remote-inbox` are removed, not kept as
  aliases; `attach --message` is the one-shot path. Reason: the issue says attach supersedes it,
  and a second trust model (unpaired bearer ticket = user) contradicts device pairing.
- AFK: `/guardian-disable` and `/guardian-enable` are refused from any device, reversing the
  #627 allowance; the master plan Decision, AGENTS.md, SECURITY.md and the docs wording are
  updated.
- AFK: a pairing confirmation is local-terminal only and not remote-answerable; a secret is
  consumed on first valid use even if the host declines.
- AFK: a device disconnect closes only that device's dialog; the local approval stays pending,
  and quitting the host UI ends the session and denies the call as today.
- AFK: the event ring defaults to 4096 events; a gap beyond it sends a full snapshot.
- AFK: `[remote] relay = "default"` also publishes the host NodeId and addresses to iroh's
  address-lookup service, a metadata exposure beyond relay use; `relay = "disabled"` publishes
  nothing. The master plan Decision (`tasks/fontaine_harness_plan.md`, remote relay) is updated,
  and `docs/remote.md` and `docs/configuration.md` cover the exposure.
- AFK: `paired_devices.json` and the host key are user-scope data-dir files; project scope
  cannot read, write or influence them.

## Documentation

`docs/remote-inbox.md` becomes `docs/remote.md` (enabling, pairing, devices and revoke,
reconnect, trust model, relay privacy, rediscovery limits, `attach --message` and exit codes);
update `SUMMARY.md`, `index.md`, `tui.md`, `guardian.md`, `afk.md`, `terminal.md`,
`configuration.md` (relay = default also publishes to address lookup), `agents.md`; update the
master plan Decision on relay (`tasks/fontaine_harness_plan.md`); CHANGELOG entry noting the removal of `fontaine send`.

## Testing Strategy

Unit (no network): pairing store round trip and atomic write; unpaired NodeId gets `NotPaired`
and counts toward lockout; replayed pairing secret is `Reused`; secret consumed on decline;
revoke closes the live connection; device slash allow-list refuses `/guardian-disable`,
`/guardian-enable`, `/remote*` and never reaches dispatch; first-answer-wins with local and
device answers; a device disconnect mid-approval closes only its dialog, leaves the local approval pending
and never approves, and a second device can still answer; the answering NodeId is in ledger and audit;
event ring resume from sequence and snapshot fallback; `RemoteEvent` serde round trip;
host/client `TestBackend` transcript equivalence; ticket never in ledger/audit/history/export; a device `Prompt` sent while a dialog is open is
queued normally and never answers it; an `ApprovalAnswer`/`QuestionReply` naming the pairing
confirmation dialog's id, or a locally opened `/guardian-disable` confirmation's id, is refused,
audited and leaves the dialog pending; a test walks `BUILTINS` and asserts `device_allowed()`
classifies every command.

Integration (`fontaine-remote/tests`, in-process iroh endpoints, relay disabled and the local
test relay): pair → confirm → attach → reattach by NodeId with no ticket; declined pairing;
reconnect with `Resume`; superseded connection; a revoked NodeId's `Resume` gets `NotPaired` and its live connection
gets `Revoked`. Unit (`attach_cmd.rs`): every `Reason` (incl. `NotPaired`, `PairingDeclined`,
`PairingTimeout`, `Revoked`, `Superseded`) maps to its `attach --message` exit code. A real network
catches NAT/relay behaviour and address-lookup rediscovery a loopback test cannot; manual.

## Manual Verification

1. Two machines: `fontaine --remote`, `fontaine attach <ticket>`, confirm on the host, drive a
   prompt and an approval from the client. Why manual: real network paths and a real terminal.

## Open Questions

None.
