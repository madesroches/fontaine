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
  and the device slash allow-list in `rust/fontaine-tui/src/remote/surface.rs`).
- No QR dependency exists in `rust/Cargo.lock`.

## Design

### Command

`/remote-control [--ttl <duration>]`, interactive and main conversation only, in the same
classification sets as `Command::Remote` (immediate-UI, not shell-limited, refused for devices). It
maps onto the existing parser so TTL rules are shared: the builtin's `command` closure returns
`Command::Remote(format!("control {argument}"))` and `parse_remote_command` gains a verb
`control` → `RemoteCommand::Control(Duration)` (`/remote control` works too, a harmless alias).

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

New `rust/fontaine-tui/src/qr.rs`, dependency `qrcode` (pure Rust, MIT/Apache-2.0, with
`default-features = false` so no image stack) in `fontaine-tui`, pinned like the other direct
dependencies.

- `qr::render(payload, max_width, max_height) -> Option<Vec<Line<'static>>>` encodes at error
  correction level L (a screen, not print, so no damage to tolerate; smaller code), pads a
  quiet zone of 2 modules (the spec asks 4; 2 scans reliably on a light frame and halves the cost in
  rows), and packs two module rows per text row with `▀`, `▄`, `█` and space.
- Every cell is drawn dark-on-light with explicit colours (`Color::Rgb` black on white), never the
  theme's, because a theme-inverted code fails on scanners that do not handle inverted modules.
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

`view::overlay` passes its `height` and `linear` to `body`.

### Accepting the connection

The endpoint already accepts after the dialog closes; two gaps make "scan, then confirm" fail
today:

1. A pairing request that arrives while the ticket dialog is open is held back by `covered()`, so
   the person sees a QR for a device that is already waiting, and the device's 60-second timer
   runs. In `sync_remote`, when a pairing request is waiting and the open dialog is the ticket
   dialog, drop the ticket dialog first (its secret was consumed by that first valid use, so it
   shows a dead code) and then open the confirmation. `RemoteSurface` gains a non-consuming
   `has_pairing()` for the check.
2. The dialog text says it is waiting and closes itself on connect, instead of leaving a stale
   code up. The status bar keeps `remote: ticket live`.

A ticket that expires with the dialog open is not auto-closed; the dialog states its lifetime, and
the status bar changes to `remote: no live ticket`.

### Security

No change to authority or to the ticket's secrecy class. The QR is another rendering of the same
bearer ticket: shown once in the dialog, never in the transcript, ledger, audit log, history,
export or telemetry; one-time secret, 10-minute default TTL, 24-hour maximum; the paired NodeId is
the authorization after a local confirmation. `/remote-control` is not enableable from config,
project scope, headless runs or a model, and a device cannot run it. No new audit event: `enabled`
and `ticket_minted` already record the mint, and showing the QR adds no new capability.

## Implementation Steps

1. `rust/fontaine-remote/src/ticket.rs`: case-insensitive prefix match in `parse`; add
   `Ticket::qr_payload`.
2. `rust/fontaine-tui/Cargo.toml` (+ `rust/Cargo.toml` if dependencies are centralised): add
   `qrcode`; dev-dependency `rqrr` for the decode test. Run `cargo deny check`.
3. `rust/fontaine-tui/src/qr.rs` (+ `lib.rs` module): `render`.
4. `rust/fontaine-tui/src/ticket_prompt.rs`: new `body` signature, QR/text selection, waiting line;
   update `view.rs` call.
5. `rust/fontaine-tui/src/remote/mod.rs`: `RemoteCommand::Control`, parse, `USAGE`.
6. `rust/fontaine-tui/src/commands.rs`: `remote-control` builtin; add to the three classification
   sets beside `Command::Remote`.
7. `rust/fontaine-tui/src/ui/remote.rs`: `Control` arm; the ticket-dialog drop before a pairing
   confirmation; `rust/fontaine-tui/src/remote/surface.rs`: `has_pairing()`.
8. Docs, changelog, plan Decisions (below).

## Files to Modify

- `rust/fontaine-remote/src/ticket.rs`
- `rust/fontaine-tui/Cargo.toml`, `rust/Cargo.lock`
- `rust/fontaine-tui/src/{qr.rs (new), lib.rs, ticket_prompt.rs, view.rs, commands.rs, remote/mod.rs, remote/surface.rs, ui/remote.rs}`
- `docs/remote.md`, `docs/tui.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **QR in the existing dialog vs. a separate QR dialog.** One dialog keeps one owner of the
  ticket's lifetime, Drop-clearing and `c` copy; a second would duplicate all three.
- **Raw ticket vs. URI payload.** Raw needs no scheme registration and no second parser; the
  versioned prefix leaves room for a URI form later.
- **Uppercase alphanumeric payload vs. the lowercase ticket.** A smaller code (fewer rows) at the
  cost of one case-insensitive prefix match.
- **Fixed colours vs. theme colours.** Theme colours are wrong on some themes in a way scanners
  punish silently; fixed colours cost a hard-coded pair in one function.
- **A shorter ticket encoding** would shrink the code further but changes the wire format #664
  shipped; out of scope here.
- **Always-on `/remote on` QR vs. only `/remote-control`.** Showing it from all three paths costs
  nothing (one dialog) and avoids a ticket dialog that shows less depending on how it was reached.

## Decisions

(none yet)

## Documentation

- `docs/remote.md`: a "Pair a phone with a QR code" section under the ticket text: the command, that
  `/remote on` and `/remote ticket` show the code too, the minimum terminal size, the text fallback,
  that the screen-reader renderer prints the text ticket only, and that the confirmation and TTL
  rules are unchanged.
- `docs/tui.md`: a `/remote-control [--ttl <duration>]` row beside `/remote`.
- `CHANGELOG.md`: `Added` entry.
- Plan `## Decisions`, in the TUI subsection beside the #664 remote lines: dated line for the
  command name, the raw uppercase payload, fixed scanner colours, and the dialog closing on a
  pairing request.

## Testing Strategy

Unit tests (anything reachable with constructed inputs):

- `parse_remote_command`: `control`, `control --ttl 2h`, bad TTL and stray arguments.
- `Ticket`: uppercase and mixed-case prefix parse to the same ticket; `qr_payload` parses back;
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
  not, text only when `linear`; the drop clears the ticket.

Integration (`rust/fontaine-tui/tests/remote.rs`, which drives a real `/remote on` and a client):

- `/remote-control` from `Off` opens the dialog with a QR and a live endpoint; run again while on,
  it mints a new ticket and the earlier one is refused (`bad_ticket`-class refusal).
- A client pairs while the QR dialog is open: the dialog closes and the confirmation opens at once,
  and answering `y` stores the device. This pins the gap that held the confirmation behind the
  dialog until the device timed out.
- A device that sends `/remote-control` is refused, and it is absent from the device allow-list.
- The ticket and QR never appear in the transcript, ledger or audit log (extends the existing
  secrecy assertions).

## Manual Verification

Only the optics, which no test reaches:

1. `cargo run -p fontaine`, then `/remote-control` in a tmux pane at 80×40, then at 80×24.
   Expected: a QR at 80×40; the text ticket and "terminal is too small" line at 80×24.
2. In the 80×40 pane, scan the code with a phone camera. Expected: the text decodes to the
   `FONTAINE-ATTACH1…` ticket. Run it in a light and a dark terminal theme: both scan.

Why not automated: whether a phone camera resolves terminal half-blocks, under real fonts, line
spacing and colour profiles, is a property of the terminal and optics. The `rqrr` round trip covers
the encoding and mapping.

## Open Questions

- **The Android app's parser.** The plan assumes it feeds the scanned text to the ticket parser and
  accepts the uppercase form. Confirm with the app work before it ships.
- **Fixed colours in 16-colour terminals.** `Color::Rgb` is not downsampled on a 256-colour
  terminal (see the 2026-10-24 theme decision); whether to use indexed 16/231 instead should be
  settled by the tmux check above.
