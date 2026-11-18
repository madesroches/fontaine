# Remote Control QR Code Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/706

## Overview

`/remote-control [--ttl <duration>]` turns remote control on (or mints a fresh ticket when it is
already on) and shows the attach ticket as a **QR code** in the existing ticket dialog, so the
Android app (#288) can scan it instead of a person transcribing a `fontaine-attach1…` string. The
endpoint stays listening and the device pairs through the unchanged host-terminal confirmation. The
QR carries the existing ticket, not a new credential, so every pairing and authority rule of
`/remote` holds as it is.

## Current State

- `/remote on|off|status|ticket|devices|revoke` is `Command::Remote(String)`
  (`rust/fontaine-tui/src/commands.rs:46`, builtin at `:496`), parsed by `parse_remote_command` in
  `rust/fontaine-tui/src/remote/mod.rs` and executed in `rust/fontaine-tui/src/ui/remote.rs`
  (`remote_command`, `start_remote`, `mint_remote_ticket`, `finish_remote_start`).
- Both `on` (once the endpoint is up) and `ticket` end in `App::begin_ticket(TicketPrompt::new(
  ticket.encode(), lifetime_text(ttl)))`. `TicketPrompt` (`rust/fontaine-tui/src/ticket_prompt.rs`)
  renders the text ticket wrapped to the dialog width; `view::overlay` builds it
  (`rust/fontaine-tui/src/view.rs:399`) and receives `width`, `height` and `linear` (the
  line-oriented screen-reader renderer), but `TicketPrompt::body` only takes `width`.
- A ticket is `Ticket { v, node: EndpointAddr, secret, expires_at }`, encoded as `fontaine-attach1`
  plus lowercase base32 of its JSON (`rust/fontaine-remote/src/ticket.rs`). `parse` uppercases the
  base32 body but matches the prefix case-sensitively.
- The endpoint keeps accepting after the dialog closes. A device that presents a valid ticket makes
  the host open a local pairing confirmation (`sync_remote`, `ui/remote.rs`), which is opened only
  when `!self.app.covered()`, so any open dialog, including the ticket dialog, holds it back while
  the device's 60-second pairing timer runs.
- Every `/remote` subcommand is refused for a device (`current_remote.is_some()` in `remote_command`,
  and `device_allowed()` on the parsed command).
- No QR dependency exists in `rust/Cargo.lock`.

## Design

### Command

`/remote-control [--ttl <duration>]`, interactive and main conversation only. It maps onto the
existing parser so TTL rules are shared: the builtin's `command` closure returns
`Command::Remote(format!("control {argument}"))`, so it inherits the variant's classification
(`main_only`, `device_allowed`, `runs_mid_turn`), and `parse_remote_command` gains a verb
`control` → `RemoteCommand::Control(Duration)` (`/remote control` works too, a harmless alias).
No new audit event: `enabled` and `ticket_minted` already record the mint.

`Control(ttl)` in `remote_command`:

| State | Effect |
| --- | --- |
| `Off` | `start_remote(runtime, ttl)`: on start, `finish_remote_start` mints the ticket and opens the dialog, exactly as `/remote on` |
| `Running` | `mint_remote_ticket(runtime, ttl)`: a new ticket, the previous one revoked, dialog opened |
| `Starting` | the existing "still starting; try again in a moment" notice |

`/remote on` and `/remote ticket` open the same dialog, so they show the QR too. `/remote-control`
differs only in always ending at a QR dialog, which makes it the discoverable entry point.

### QR payload

The raw ticket uppercased: `FONTAINE-ATTACH1<BASE32>`. Uppercase letters, digits `2-7` and `-` are
all in the QR alphanumeric set, which holds about 1.45x more than byte mode and so yields a smaller
code than the lowercase ticket would. `Ticket::parse` matches the prefix case-insensitively
(base32 decoding already is), so the uppercase form parses with no second code path, and the
Android app can pass the scanned text straight to the same parser. No URI scheme: the app is the one
reader, and a scheme can be added later behind a new ticket prefix without breaking this one.

`Ticket::qr_payload(&self) -> String` (in `fontaine-remote`) returns `encode().to_uppercase()`, so
the format has one owner.

### Rendering

New `rust/fontaine-tui/src/qr.rs`, dependency `qrcode = { version = "=0.14.1", default-features = false }` (pure Rust,
MIT/Apache-2.0, no image stack) declared in `rust/Cargo.toml` `[workspace.dependencies]` and used
in `fontaine-tui` with `.workspace = true`.

- `qr::render(payload, max_width, max_height) -> Option<Vec<Line<'static>>>` encodes at error
  correction level L (a screen, not print, so no damage to tolerate; smaller code), pads a
  quiet zone of 2 modules (the spec asks 4; 2 scans reliably on a light frame and halves the cost in
  rows), and packs two module rows per text row with `▀`, `▄`, `█` and space.
- Every cell is drawn as fixed ANSI black modules on a bright-white background
  (`Color::Black`/`Color::White`), never theme roles, because a theme-inverted code fails on
  scanners that do not handle inverted modules.
- Returns `None` when `modules + quiet` does not fit `max_width` columns or `ceil(rows / 2)` text
  rows. The dialog redraws on every resize, so it switches between QR and text with no state.
- Nothing is cached: the lines are rebuilt each draw from the ticket the dialog holds, which
  `Drop` already clears, so the code is no longer reachable than the text was.

`TicketPrompt::body` takes `(width, height, linear, theme)`:

- `linear` (screen reader): text ticket only, as today; the QR is never printed.
- Otherwise: heading, validity line, the QR when it fits, then "Waiting for a device to connect;
  this closes when one does." The wrapped text ticket is shown **instead of** the QR when it does
  not fit, with one line saying the terminal is too small for the QR. `c` copies the text ticket in
  both cases.
- The footer is unchanged.

`view::overlay` passes `linear` and a `height` computed with `body_rows(app, width, height,
footer_len)`, the body's share of the viewport (the overlay's own `height` is the whole viewport,
and `draw_overlay` scrolls to the tail, which would cut off the top of the QR). `body` passes `qr::render` that budget
minus the dialog's own text lines (heading, validity, waiting lines) as `max_height`.

### Accepting the connection

The endpoint already accepts after the dialog closes; two gaps make "scan, then confirm" fail
today:

1. A pairing request that arrives while the ticket dialog is open is held back by `covered()`, so
   the person sees a QR for a device that is already waiting, and the device's 60-second timer
   runs. In `sync_remote`, when `covered()` is true only because of the ticket dialog
   (`App.ticket_prompt`; `/remote` runs mid-turn, so it can sit over an approval or question, and
   then nothing changes), call `take_pairing()` and, on `Some`, clear `ticket_prompt` (its secret
   was consumed by that first valid use, so it shows a dead code) and `begin_pairing`.
   `take_pairing()` already skips and audits stale requests.
2. The dialog text says it is waiting and closes itself on connect, instead of leaving a stale
   code up. The status bar keeps `remote: ticket live`.

A ticket that expires with the dialog open is not auto-closed; the dialog states its lifetime, and
the status bar changes to `remote: no live ticket` on the next redraw.

## Implementation Steps

1. `rust/fontaine-remote/src/ticket.rs`: case-insensitive prefix match in `parse`, and make the
   retired-prefix check case-insensitive in the same edit (an uppercase retired ticket reports
   `Retired`, not `NotATicket`; unit-tested); add `Ticket::qr_payload`.
2. `rust/Cargo.toml` `[workspace.dependencies]`: `qrcode = { version = "=0.14.1",
   default-features = false }` and dev-dependency `rqrr = { version = "=0.11.0",
   default-features = false }` (decode via `PreparedImage::prepare_from_bitmap`);
   `rust/fontaine-tui/Cargo.toml` uses both with `.workspace = true`. Run `cargo deny check`.
3. `rust/fontaine-tui/src/qr.rs` (+ `lib.rs` module): `render`.
4. `rust/fontaine-tui/src/ticket_prompt.rs`: new `body` signature, QR/text selection, waiting line;
   update `view.rs` call.
5. `rust/fontaine-tui/src/remote/mod.rs`: `RemoteCommand::Control`, parse, `USAGE`.
6. `rust/fontaine-tui/src/commands.rs`: add a `remote-control` entry to `BUILTINS` (sorted by name,
   between `remote` and `rename`, hint `[--ttl <duration>]`) and add `control` to the `remote`
   builtin's hint.
7. `rust/fontaine-tui/src/ui/remote.rs`: `Control` arm; in `sync_remote`, the ticket-dialog drop
   before a pairing confirmation.
8. Docs, changelog, plan Decisions (below).
9. Post the payload contract (see Decisions) as a comment on #288: `gh issue comment 288`.

## Files to Modify

- `rust/fontaine-remote/src/ticket.rs`
- `rust/Cargo.toml`, `rust/fontaine-tui/Cargo.toml`, `rust/Cargo.lock`
- `rust/fontaine-tui/src/{qr.rs (new), lib.rs, ticket_prompt.rs, view.rs, commands.rs, remote/mod.rs, ui/remote.rs}`
- `docs/remote.md`, `docs/tui.md`, `docs/configuration.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **QR in the existing dialog vs. a separate QR dialog.** One dialog keeps one owner of the
  ticket's lifetime, Drop-clearing and `c` copy; a second would duplicate all three.
- **A shorter ticket encoding** would shrink the code further but changes the wire format #664
  shipped; out of scope here.

## Decisions

- 2026-11-18: the QR payload is the raw ticket uppercased, `FONTAINE-ATTACH1<BASE32>`, parsed
  case-insensitively by `Ticket::parse`, no URI scheme; the Android app (#288, not yet built) must
  accept it unchanged.

## Documentation

- `docs/remote.md`: a "Pair a phone with a QR code" section under the ticket text: the command, that
  `/remote on` and `/remote ticket` show the code too, the minimum terminal size, the text fallback,
  that the screen-reader renderer prints the text ticket only, and that the confirmation and TTL
  rules are unchanged.
- `docs/remote.md:8` and its command block (lines 14-22): name `/remote-control` (and
  `/remote control`) among the ways to turn remote control on and the subcommands.
- `docs/configuration.md:1265`: "only `fontaine --remote` or `/remote on` … enables remote control"
  also names `/remote-control`.
- `docs/tui.md`: a `/remote-control [--ttl <duration>]` row beside `/remote`.
- `CHANGELOG.md`: `Added` entry.
- Plan `## Decisions`, in the TUI subsection beside the #664 remote lines: dated line for the
  command name, the raw uppercase payload, fixed scanner colours, and the dialog closing on a
  pairing request.

## Testing Strategy

Unit tests (anything reachable with constructed inputs):

- `parse_remote_command`: `control`, `control --ttl 2h`, bad TTL and stray arguments.
- `Ticket`: uppercase and mixed-case prefix parse to the same ticket, and an uppercase retired
  ticket reports `Retired`; `qr_payload` parses back;
  the payload contains only QR-alphanumeric characters.
- `qr::render`: returns `None` below the fit threshold on either axis; module count and row count
  match the version for a known payload; a payload of a relay-default ticket and of a
  `relay = "disabled"` ticket fit an 80×40 area (this pins the size estimate rather than assuming
  it).
- **Scannability round trip:** rebuild the bitmap from the rendered cells (half-block mapping and
  quiet zone) and decode it with `rqrr`; the result equals the payload. A wrong half-block mapping
  or inverted polarity gives an unscannable code with no error, so this is the test that earns its
  place.
- `TicketPrompt::body`: QR present when it fits, text ticket plus the too-small line when it does
  not, text only when `linear`; the drop clears the ticket. The existing unit test at
  `ticket_prompt.rs:92` is updated to the new `body` signature.

Integration (`rust/fontaine-tui/tests/remote.rs`, which drives a real `/remote on` and a client):

- `/remote-control` from `Off` opens the dialog with a QR and a live endpoint; run again while on,
  it mints a new ticket and the earlier one is refused (`bad_ticket`-class refusal).
- A client pairs while the QR dialog is open: the dialog closes and the confirmation opens at once,
  and answering `y` stores the device. This pins the gap that held the confirmation behind the
  dialog until the device timed out. The `enable()` helper closes the dialog before pairing
  (`tests/remote.rs:24-37`), so this needs a variant that keeps it open.
- A device that sends `/remote-control` is refused by `device_allowed`: add it to the existing
  `a_device_cannot_run_guardian_disable_and_the_refusal_is_audited` (`tests/remote.rs:355-397`)
  and bump its `command_refused` count from 4 to 5.
- The ticket and QR never appear in the transcript, ledger or audit log: extend the existing
  secrecy assertions (`tests/remote.rs:159-170`), which search only for lowercase
  `fontaine-attach1`, to be case-insensitive so they catch `FONTAINE-ATTACH1…`.

## Manual Verification

Only the optics, which no test reaches:

1. `cargo run -p fontaine`, then `/remote-control` in a tmux pane at 80×40, then at 80×24.
   Expected: a QR at 80×40; the text ticket and "terminal is too small" line at 80×24.
2. In the 80×40 pane, scan the code with a phone camera. Expected: the text decodes to the
   `FONTAINE-ATTACH1…` ticket. Run it in a light and a dark terminal theme: both scan.

Why not automated: whether a phone camera resolves terminal half-blocks, under real fonts, line
spacing and colour profiles, is a property of the terminal and optics. The `rqrr` round trip covers
the encoding and mapping.
