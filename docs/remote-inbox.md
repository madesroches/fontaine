# The remote inbox

The remote inbox lets you send one line of text to a live terminal session from another terminal or
device: `fontaine send <ticket> "text"`. It is the first piece of remote control, built for the
case where you walked away from a running session and want to nudge it, answer it or stop it.

Nothing enables it but you. You turn it on in the session with `/remote-inbox on`, which prints a
short-lived, one-time ticket. It is off by default, no setting turns it on, a project file cannot
turn it on, a headless run cannot turn it on, and the model has no tool for it.

## Using it

In the session:

```text
/remote-inbox on [--ttl <duration>]    start the inbox and print a ticket
/remote-inbox ticket [--ttl <duration>] mint a new ticket (revokes the previous one)
/remote-inbox status                    show whether the inbox is on and whether a ticket is live
/remote-inbox off                       take the inbox down
```

The ticket is shown once, in a dialog over the input. `c` copies it to the clipboard and Enter
dismisses it for good. A ticket lives 10 minutes unless you give `--ttl` (for example `--ttl 2h`),
and at most 24 hours. While the inbox is on, the status bar says `inbox: ticket live` or
`inbox: no live ticket`.

From the other terminal or device:

```sh
fontaine send fontaine-send1… "run the tests again and tell me what failed"
```

`fontaine send` needs no configuration, no session of its own and no provider credentials. It
prints `accepted msg-…` and exits 0 once the session has queued the message. A text of `-` reads
the message from stdin.

The ticket on the command line stays in your shell history and is visible in the process list of
the machine you run it on. To keep it out of both, pass it on stdin or in the environment:

```sh
printf '%s' "$TICKET" | fontaine send --ticket-stdin "check the build"
FONTAINE_TICKET="$TICKET" fontaine send "check the build"
```

`--ticket-stdin` and a text of `-` both read stdin, so together they are a usage error (exit 2).

## One ticket, one message

A ticket is a bearer credential with a one-time secret. The first request that presents it
consumes the secret and binds the sender's node id; the secret is consumed only when the message is
accepted, so a refused attempt (inbox full, message too large) can be retried with
the same ticket. After that, sending again needs a new ticket from `/remote-inbox ticket`, which is
what "one-shot" means. Minting any ticket revokes the previous one, used or not. A message the
client sent twice because an acknowledgement was lost is queued once; this holds only while the
ticket is live, so a late retry after expiry or revocation gets `expired` or `bad_ticket`.

The secret is checked before expiry or reuse, so a wrong secret learns nothing about the ticket's state.
After five bad attempts from one node id within 30 seconds the server answers `locked` to that node for 30 seconds; other nodes
are unaffected.

## What accepted means

`accepted` means the session put the message in its inbox: it is **queued**, not read by the model,
not run and not finished. The inbox holds at most 16 messages of at most 8 KiB each. Both limits
are checked before the acknowledgement.

A queued message is delivered like a line you typed:

- While a turn runs, a plain message rides the next tool result, marked
  `[Remote message from <node>]`, and enters the conversation as a user message.
- While the session is idle, it starts a turn, the way a line typed at the prompt does.
- A line that starts with `/` (a command or a skill) or `!` queues like the same line typed during
  a turn: it runs once the turn ends, under the same rules, so `/remote-inbox off` and
  `/remote-inbox status` work from a remote line. `/remote-inbox on` and `ticket` are refused from a
  remote line, because a ticket is shown only on the local terminal.

Remote lines never enter your input box, your draft, your queued input or your prompt history. The
transcript shows them with a `[remote <node>]` marker.

If the session cannot run a line (a prompt-submit hook blocks it, or the session is stopped by the
[AFK temperature ceiling](afk.md#the-stop)) it is dropped with an audit record and a transcript
message; it is never put back into your input. `/clear`, `/resume`, `/branch` and ending the
session take the endpoint down and drop the remote messages still queued, so a message never
crosses into another conversation; after a switch, run `/remote-inbox on` again. `--resume` never
re-enables the inbox.

## Trust: the sender is presumed to be you

A holder of a live ticket is presumed to be the user, because only your explicit local command
minted it. A remote line is therefore a user message: it counts as your intent for the
[guardian](guardian.md), it runs the prompt-submit hooks, and it is a full terminal line. Slash
commands, mode changes, skills and `!` lines work. An open approval, question,
`/guardian-disable` confirmation or `/afk review` confirmation is answered by a remote line that
equals one of its options (the label or the hotkey, ignoring case and surrounding space) and that
arrived after the dialog opened; any other text stays queued behind the dialog, which stays
pending. A line that was already queued when a dialog opened never answers it, so a `y` sent during
a turn cannot approve a later prompt.

That authority is exactly a terminal's and no more. Every tool call the line leads to still crosses
the deterministic floor, the guardian, the workspace scope and the result pipeline. `/guardian-disable` is
the one command that loosens the floor, and it works from a remote line too, with its confirmation:
the confirmation is a second line, which needs another ticket, and every toggle is recorded in the
ledger and the audit log with the remote identity. It stays blocked in headless runs.

The ticket's bearer risk is the accepted cost of this presumption. Keep it short and treat it like
a password: use `--ttl` for the shortest window that works, send it over a channel you trust, and
run `/remote-inbox off` when you are done.

The ticket is never written to the transcript, the ledger, the audit log, the prompt history, an
export or telemetry. The audit log records, as tool `remote_inbox`, each enable, disable, ticket
mint, accepted message, rejected request (with its reason), dialog answer and dropped message,
with the sender's node id, the message id and the size, never the secret. In the line-oriented
screen-reader renderer the ticket dialog is printed once like any dialog, outside the transcript;
the terminal's own scrollback keeps that one print.

## Direct and relayed connections

`fontaine send` connects over [iroh](https://www.iroh.computer/): QUIC with end-to-end encryption
to the session's endpoint, directly when the two machines can reach each other and through a relay
when they cannot (for example behind NATs). The endpoint's key is generated when you enable the
inbox and never saved, so the node id in a ticket names that one enable and nothing later.

By default the session also uses the public relays run by the iroh project, so a relayed
connection works out of the box. A relay sees connection metadata, such as the two node ids and
when they connect, never the message text. To use direct connections only, set in your user file or
`--settings`:

```toml
[remote]
relay = "disabled"
```

`relay` is `default` (the iroh project's public relays) or `disabled`. It only chooses network
paths; it never enables the inbox. The key is user-scope only, and a project file that sets it is
refused.

## Errors and exit codes

`fontaine send` prints one line and exits nonzero on failure:

| Exit | Reasons |
|---|---|
| 0 | `accepted` |
| 2 | `bad_ticket` (wrong or revoked), `expired`, `reused` (already used), `locked` (too many bad attempts), a malformed ticket, or a usage error |
| 3 | `inbox_full`, `too_large` |
| 4 | the session could not be reached, the request timed out, or the inbox was `closed` (including a session that already ended) |
| 5 | `unsupported_version`, `malformed` (a protocol mismatch) |

Sending to a session that has turned the inbox off, switched sessions or exited is a connection
failure or `closed`, exit 4.
