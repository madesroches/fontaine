# Remote control

Remote control lets a second machine drive a live terminal session. `fontaine attach <ticket>` opens
a full terminal UI on that machine that mirrors the session and lets you prompt it, answer its
approvals and questions, interrupt it and run a few commands, while the session itself keeps running
where it started.

Nothing enables it but you. You turn it on with `fontaine --remote` or, in a running session,
`/remote on`. It is off by default, no setting turns it on, a project file cannot turn it on, a
headless run cannot turn it on, `--resume` never turns it back on, and the model has no tool for it.

## Enabling it and pairing a device

```text
fontaine --remote                       start with remote control on and a ticket shown
/remote on [--ttl <duration>]           turn it on and show a ticket
/remote ticket [--ttl <duration>]       mint a new ticket (revokes the previous one)
/remote status                          whether it is on, how many devices are attached, whether a ticket is live
/remote devices                         list the paired devices
/remote revoke <label|node-prefix>      remove a paired device and close its connection
/remote off                             turn it off
```

The ticket is shown once, in a dialog over the input. `c` copies it and Enter dismisses it for good.
A ticket lives 10 minutes unless you give `--ttl` (for example `--ttl 2h`), and at most 24 hours. The
status bar says `remote: ticket live`, `remote: no live ticket` or `remote: N device(s)`.

On the other machine:

```sh
fontaine attach fontaine-attach1…
```

The first connection of a device pairs it. The ticket's one-time secret only lets the device ask: the
session then opens a **confirmation dialog in the host terminal** showing the device's node id and the
label it gave itself, and only a local `y` stores the device. A device cannot confirm its own pairing,
and the dialog cannot be answered from any attached device. An explicit `n` refuses the device
(`pairing_declined`); no answer within 60 seconds is `pairing_timeout`; a device that disconnects
while the dialog is open closes it with no reply. In every case the secret is consumed.

`fontaine attach --name <NAME>` sets the label the host's confirmation dialog shows; without it the
device uses `COMPUTERNAME` or `HOSTNAME`, else `device`. `--host-name <LABEL>` sets the label the
device stores the host under when pairing, so a later `fontaine attach <LABEL>` finds it; without it
select the host by a node-id prefix.

A paired device is stored by node id in `paired_devices.json` in your user-scope data directory.
After that it reattaches with no ticket:

```sh
fontaine attach work-laptop        # the label or a node-id prefix of at least 4 characters of a paired host
```

The device keeps its own key (`attach_device.key`) and the hosts it paired with
(`attach_hosts.json`) in its data directory. Both files, and the host's key (`remote_host.key`) and
`paired_devices.json`, are owner-only; project scope can never read or write them.

The ticket on the command line stays in your shell history and is visible in the process list. To
keep it out of both, pass it in the environment for an interactive attach, or on stdin for a
one-shot `--message` (an interactive attach needs stdin for keys, so `--ticket-stdin` without
`--message` is refused):

```sh
FONTAINE_TICKET="$TICKET" fontaine attach
printf '%s' "$TICKET" | fontaine attach --ticket-stdin --message "hello"
```

## Revoking a device

`/remote revoke <label|node-prefix>` removes the entry and closes that device's live connection with
`revoked`. `fontaine remote devices` and `fontaine remote revoke <device>` do the same offline against
the file; a running host re-reads the file on every device message and every keepalive and closes a
now-revoked connection. A revoked node id needs a new pairing: its `Resume` gets `not_paired`.

## What a device sees and can do

The device runs a terminal UI with no agent, no provider credentials and no project config. It paints
the host's transcript, status bar, todo list and the one dialog that wants an answer, with the same
renderers the host uses. Local-only features (editor handoff, clipboard images, `/config`, shell
lines) say they are host-side.

A device sends only a prompt or a slash line:

- A **prompt** is plain text for the model. A line that starts with `!` or `/` is still plain text,
  never a shell line or a command. It queues like a line you typed during a turn, but it never enters
  your draft, your queued input or your prompt history; the transcript marks it `[remote <node>]` and
  the ledger records `remote: { node, message_id }`.
- A **slash line** runs only if the command is on the device list: `/btw`, `/cost`, `/recap`,
  `/compact`, `/context`, `/goal`, `/init`, `/rename`, `/help`, `/status` and skills. Every
  other command is refused with a notice to the device, an audit record, and never reaches the
  dispatcher. That includes `/loop`, `/guardian-disable` and `/guardian-enable`, every `/remote` subcommand,
  `/afk`, `/theme`, `/export`, `/config`, `/clear`, `/resume`, `/branch`, `/rewind`, `/model`,
  `/effort`, `/login`, `/exit`, `/add-dir`, `/plugin`, `/mcp` and every panel, toggle or picker of the
  host.
- **Interrupt** (Esc on the device) interrupts the main turn when one is running, and does nothing
  otherwise.
- **Approvals and questions** can be answered from the device. An approval takes one of the choices the
  dialog offered: allow once, allow for the session, or deny with an optional reason of at most 1 KiB.
  A device cannot choose "always allow", which writes a permanent rule to your user settings: it is
  not shown and an answer naming it is refused. A question reply names the sub-question and the options
  it saw; a reply for a question the panel has moved past is refused. A device gives one option per
  question: a multi-select answer with several options is refused with a notice, and is answered at the host. The first answer wins, whether
  it comes from the host or any device; the others see the dialog close. A device answer is refused,
  with a notice to that device, while you are typing a denial reason or have a question panel
  half-answered, so it never discards your input.
- Elicitations, the guardian-unavailable dialog, the usage pause, MCP server and skill trust prompts,
  `/afk review`, the pairing confirmation and `/guardian-disable` are the host's alone. A device sees
  "the host is waiting on …" and an answer naming one is refused, audited, and leaves the dialog
  pending.

The device that answered an approval or question is recorded: its node id is in the ledger
(`Approval`, `Denial` and the dialog's user message carry `remote`, with the dialog id as
`message_id`) and in the audit explanation. A dropped connection never approves anything.

### AFK

An attached device is not human presence. While [AFK mode](afk.md) is on, every approval and question
is still answered unattended on the host (denied or unavailable) and no device is asked. Devices get a
notice when AFK turns on or off, and on attach while it is on.

### What the floor still does

Every tool call a device induces crosses the same deterministic floor, guardian, workspace scope and
result pipeline as a call you typed. The guardian and the floor stay host-side, and no device can turn
either off.

## Reconnecting and leaving

If the connection drops, the device shows `reconnecting…` and retries with backoff. It resumes from
the last event it applied: the host keeps a ring of recent events (4096 events or 16 MiB, whichever
comes first) under a stream epoch, replays the gap, and falls back to a full snapshot when the gap fell
out of the ring or the epoch changed (after `/clear`, `/resume`, `/branch` or a re-enable). Each
device has a bounded outbound queue; a device that falls too far behind is disconnected with a notice
and reconnects. Both sides ping every 15 seconds and call the connection lost after 45 seconds of
silence.

Leave with Ctrl+D at an empty idle prompt or `/detach`; the session continues. A second connection
from the same device supersedes the first (`superseded`); several different paired devices can be
attached at once.

`/clear`, `/resume`, `/branch` and quitting say goodbye to every device and take the endpoint down.
Paired devices survive in `paired_devices.json` and reattach after the next `/remote on` or
`fontaine --remote`. One session at a time is the remote host: a second session's `/remote on` is
refused while another holds `remote_host.lock`.

## One message, no screen: `attach --message`

```sh
fontaine attach <ticket|host> --message "run the tests again and tell me what failed"
echo "check the build" | fontaine attach work-laptop --message -
```

This attaches, sends one prompt, waits until the host has queued it, prints `accepted msg-…` and
exits. `--message` waits long enough for a pairing confirmation before it waits for the
acknowledgement. `accepted` means **queued**: not read by the model, not run and not finished. The
queue holds at most 16 remote messages of at most 8 KiB each.

`fontaine send` and `/remote-inbox` are removed; this replaces them, and a `fontaine-send1…` ticket
fails to parse with an error saying so.

## Trust model

A ticket is a bearer secret that only lets a device **ask to pair**; you decide, at the host
terminal, whether to pair it. Authentication of a paired device is the node id of its iroh
connection, never something the device writes. Pairing is the real authorization, and it is
revocable. The secret is one-time, expires, is compared in constant time, and is checked before
expiry or reuse, so a wrong secret learns nothing. After five bad attempts from one node id within 30
seconds, that node gets `locked` for 30 seconds.

Keep a ticket short-lived and treat it like a password. It is never written to the transcript, the
ledger, the audit log, the prompt history, an export or telemetry. The audit log records, as tool
`remote`, each enable, disable, ticket mint, pairing confirmation or refusal, attach, detach, refused
connection, message, refused command, interrupt, device answer and revoke, with the node id and ids
but never the secret. In the line-oriented screen-reader renderer the ticket dialog is printed once
like any dialog, outside the transcript; the terminal's scrollback keeps that one print.

## Direct and relayed connections, and rediscovery

Devices connect over [iroh](https://www.iroh.computer/): QUIC with end-to-end encryption to the host's
endpoint, directly when the two machines can reach each other and through a relay when they cannot.
The host's endpoint key is persistent, so a paired device finds it again by node id.

By default (`[remote] relay = "default"`) the host uses the iroh project's public relays **and
publishes its node id and home relay URL to iroh's address-lookup service** under that persistent
key, so a paired device can find it even when its addresses changed. The attaching device only
resolves; it never publishes its own node id or relay. That publication is metadata exposure beyond
relay use: anyone who knows your host's node id can look up where to reach it, though not read
anything. A relay sees connection metadata such as the two node ids and when they connect, never
content.

```toml
[remote]
relay = "disabled"
```

With `disabled`, nothing is published, the host listens for direct connections only, and a device
dials the addresses it stored at its last attach and queries no lookup service. Direct-only users
reattach through those stored addresses; a self-hosted lookup service is not supported. `relay` is
user-scope only, and a project file that sets it is refused. The first pairing needs no lookup: the
ticket carries the host's addresses. `fontaine attach` reads only the user-scope `[remote] relay`.

## Errors and exit codes

`fontaine attach --message` prints one line and exits nonzero on failure:

| Exit | Reasons |
|---|---|
| 0 | `accepted` |
| 2 | `bad_ticket`, `expired`, `reused`, `locked`, `not_paired`, `pairing_declined`, `pairing_timeout`, `revoked`, a malformed or retired ticket, or a usage error |
| 3 | `inbox_full`, `too_large` |
| 4 | the session could not be reached, the request timed out, `closed`, or `superseded` |
| 5 | `unsupported_version`, `malformed` (a protocol mismatch) |

An interactive `fontaine attach` that ends because the host turned remote control off or exited prints
a line and exits 0.

## Limits

- Real network behavior (NAT traversal, relay fallback, address-lookup rediscovery) is verified by
  hand on two machines, not in the automated tests, which use loopback and a local relay.
- Typed answers to a question's "Other" row, attachments and clipboard images are host-side.
- A session whose transcript is too large to send as one snapshot (over 32 MiB) cannot be attached
  to: the device is refused with `too_large` (exit code 3). Starting a new session or `/clear`
  makes it attachable again.
- The device paints from the host's events, so a surface the host adds without a mirror event does
  not appear on a device until the mirror covers it.
