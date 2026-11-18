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

`/remote on`, `/remote ticket` and `fontaine --remote` (which reaches the same `begin_ticket` path
through `start_remote`) open the same dialog, so they show the QR too. `/remote-control`
differs only in always ending at a QR dialog, which makes it the discoverable entry point.

### QR payload

The raw ticket uppercased: `FONTAINE-ATTACH1<BASE32>`. Uppercase letters, digits `2-7` and `-` are
all in the QR alphanumeric set, which holds about 1.45x more than byte mode and so yields a smaller
code than the lowercase ticket would. `Ticket::parse` matches the prefix case-insensitively
(base32 decoding already is), so the uppercase form parses with no second code path, and the
Android app can pass the scanned text straight to the same parser. No URI scheme: the app is the one
reader, and a scheme can be added later behind a new ticket prefix without breaking this one.

`Ticket::qr_payload(&self) -> String` (in `fontaine-remote`) returns `encode().to_uppercase()`, so
the format has one owner. `TicketPrompt::new` takes `ticket.qr_payload()` next to `ticket.encode()`
at both call sites (`ui/remote.rs`), and the prompt keeps it and clears it on `Drop` like the text
ticket, since `body` cannot reach the `Ticket`.

### Rendering

New `rust/fontaine-tui/src/qr.rs`, dependency `qrcode = { version = "=0.14.1", default-features = false }` (pure Rust,
MIT/Apache-2.0, no image stack) declared in `rust/Cargo.toml` `[workspace.dependencies]` and used
in `fontaine-tui` with `.workspace = true`.

- `qr::render(payload, max_width, max_height) -> Option<Vec<Line<'static>>>` encodes at error
  correction level L (a screen, not print, so no damage to tolerate; smaller code), pads a
  quiet zone of 2 modules (the spec asks 4; 2 scans reliably on a light frame and halves the cost in
  rows). It builds the text rows with `code.render::<unicode::Dense1x2>().quiet_zone(false)` (one
  text row per two module rows, `▀`, `▄`, `█` and space), pads the 2-module quiet zone by hand, and
  styles each row black on white.
- Every cell is drawn as fixed ANSI black modules on a bright-white background
  (`Color::Black`/`Color::White`), never theme roles, because a theme-inverted code fails on
  scanners that do not handle inverted modules.
- Returns `None` when the code's `modules + 4` columns exceed `max_width`, or its
  `ceil((modules + 4) / 2)` text rows exceed `max_height`. Rows, not width, are the constraint: a ticket's payload grows about 35-40 characters per
  direct address, so the code needs a terminal of about 80×44 (relay disabled) to about 80×50
  (relay default with IPv6 addresses). The dialog redraws on every resize, so it switches between QR and text with no state.
- Nothing is cached: the lines are rebuilt each draw from the ticket the dialog holds, which
  `Drop` already clears, so the code is no longer reachable than the text was.

`TicketPrompt::body` takes `(width, height, linear, theme)`:

- `linear` (screen reader): text ticket only, as today; the QR is never printed.
- `NO_COLOR` set: crossterm drops the fixed black-on-white colours, so the half-block code could
  render inverted; show the text ticket with one line saying the QR is off because `NO_COLOR` is
  set.
- Otherwise: heading, validity line, the QR when it fits, then "Waiting for a device to connect;
  this closes when one does." The wrapped text ticket is shown **instead of** the QR when it does
  not fit, with one line saying the terminal is too small for the QR. `c` copies the text ticket in
  both cases. QR mode drops the existing hint line ("On the other device: FONTAINE_TICKET=<ticket>
  fontaine attach") and the blank line before it; the text fallback keeps both.
- The footer is unchanged.

`view::overlay` passes `linear` and a `height` computed with `body_rows(app, width, height,
footer_len)`, the body's share of the viewport (the overlay's own `height` is the whole viewport,
and `draw_overlay` scrolls to the tail, which would cut off the top of the QR). `body` passes `qr::render` that budget
minus the dialog's own text lines (heading, validity, waiting lines) as `max_height`.

`chrome()` and `view::height` call `overlay` with the terminal height, while `draw_widgets` calls it
with the inline viewport height, which `body_rows` reduces by one more row. So the QR/text choice
is made from the terminal height both paths share: `overlay` takes the terminal height for the
ticket dialog's decision, and `body` follows it, so the dialog does not flip between QR and text
across the two paths. `draw_widgets` sees only the inline viewport, so `view::draw` and
`draw_widgets` take a `terminal_height` parameter, which `Screen::render` already reads and passes
down; `draw_widgets` hands it to `overlay`.

### Accepting the connection

The endpoint already accepts after the dialog closes; two gaps make "scan, then confirm" fail
today:

1. A pairing request that arrives while the ticket dialog is open is held back by `covered()`, so
   the person sees a QR for a device that is already waiting, and the device's 60-second timer
   runs. In `sync_remote`, when only the ticket dialog covers the screen
   (`App::covered()` is split into `ticket_prompt.is_some() || self.covered_by_other_than_ticket()`,
   the latter owning the rest of the field list, and `sync_remote` tests that method; the dialog is
   `App.ticket_prompt`; `/remote` runs mid-turn, so it can sit over an approval or question, and
   then nothing changes), call `take_pairing()` only when `!running.server.has_live_ticket()` (the
   current secret was consumed, so the dialog shows a dead code) and, on `Some`, clear
   `ticket_prompt` and open the pairing confirmation (`begin_pairing`). When a live ticket's dialog
   stays up, do not call `take_pairing()`: the request stays queued, as today, until the dialog is
   dismissed and then opens, because `view::overlay` draws `ticket_prompt` first and
   `App::handle_key` returns early on it, so a confirmation opened under the dialog could not be
   answered before the device's 60-second timer runs out.
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
   default-features = false }`; `rust/fontaine-tui/Cargo.toml` uses it with `.workspace = true`.
   Run `cargo deny check`.
3. `rust/fontaine-tui/src/qr.rs` (+ `lib.rs` module): `render`, on qrcode's `Dense1x2` renderer.
4. `rust/fontaine-tui/src/ticket_prompt.rs`: new `body` signature, QR/text selection, waiting line,
   `TicketPrompt::new` taking the QR payload; update `view.rs` call. `view::draw` and `draw_widgets`
   take `terminal_height` and pass it to `overlay`; `rust/fontaine-tui/src/screen.rs`:
   `Screen::render` passes its terminal height to `view::draw`, and the test caller of `view::draw`
   is updated.
5. `rust/fontaine-tui/src/remote/mod.rs`: `RemoteCommand::Control`, parse, and `control [--ttl
   <duration>]` added to `USAGE`.
6. `rust/fontaine-tui/src/commands.rs`: add a `remote-control` entry to `BUILTINS` (sorted by name,
   between `remote` and `rename`, hint `[--ttl <duration>]`), add `control` to the `remote`
   builtin's hint, and add `/remote-control` to the list in
   `runs_mid_turn_marks_the_immediate_ui_set_and_nothing_else`.
7. `rust/fontaine-tui/src/app.rs`: split `covered()` into `ticket_prompt.is_some() ||
   covered_by_other_than_ticket()`. `rust/fontaine-tui/src/ui/remote.rs`: `Control` arm; in `sync_remote`, the ticket-dialog drop
   before a pairing confirmation; both `TicketPrompt::new` call sites pass `ticket.qr_payload()`.
8. Docs, changelog, plan Decisions (below).
9. Post the payload contract (see Decisions) as a comment on #288: `gh issue comment 288`.

## Files to Modify

- `rust/fontaine-remote/src/ticket.rs`
- `rust/Cargo.toml`, `rust/fontaine-tui/Cargo.toml`, `rust/Cargo.lock`
- `rust/fontaine-tui/src/{qr.rs (new), lib.rs, app.rs, ticket_prompt.rs, view.rs, screen.rs, commands.rs, remote/mod.rs, ui/remote.rs}`
- `docs/remote.md`, `docs/tui.md`, `docs/terminal.md`, `docs/configuration.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **QR in the existing dialog vs. a separate QR dialog.** One dialog keeps one owner of the
  ticket's lifetime, Drop-clearing and `c` copy; a second would duplicate all three.
- **Accept the measured QR minimums vs. shrink the payload.** The large minimum terminal size and
  its text fallback stay; capping addresses or a shorter encoding is a wire-format change for its
  own issue.
- **A shorter ticket encoding** would shrink the code further but changes the wire format #664
  shipped; out of scope here.

## Decisions

- 2026-11-18: the QR payload is the raw ticket uppercased, `FONTAINE-ATTACH1<BASE32>`, parsed
  case-insensitively by `Ticket::parse`, no URI scheme; the Android app (#288, not yet built) must
  accept it unchanged.
- 2026-11-18: AFK: accept the measured QR minimums (versions 11-14, about 80x44 to 80x50) with the
  text-ticket fallback rather than shrinking the payload, which is a wire-format change; a
  follow-up issue for a smaller payload should be filed.
- 2026-11-18: AFK: when `NO_COLOR` is set, show the text ticket with a one-line reason instead of
  the QR, since the fixed black-on-white colours are dropped and the code could render inverted;
  documented next to the screen-reader fallback in `docs/remote.md`.
- 2026-11-18: AFK: over a ticket dialog, call `take_pairing()` only when `!has_live_ticket()`, then
  clear `ticket_prompt` and open the confirmation; with a live ticket's dialog up the request stays
  queued until the dialog is dismissed, since the dialog hides and swallows keys for a confirmation.

## Documentation

- `docs/remote.md`: a "Pair a phone with a QR code" section under the ticket text: the command, that
  `/remote on`, `/remote ticket` and `fontaine --remote` show the code too, the minimum terminal
  size (measured: about 44 rows for a relay-disabled ticket to about 50 for relay default with IPv6
  addresses, at any width of 80 or more), the text fallback,
  that the screen-reader renderer and `NO_COLOR` print the text ticket only, and that the confirmation and TTL
  rules are unchanged.
- `docs/remote.md:8` and its command block (lines 14-22): name `/remote-control` (and
  `/remote control`) among the ways to turn remote control on and the subcommands.
- `docs/configuration.md:1265`: "only `fontaine --remote` or `/remote on` … enables remote control"
  also names `/remote-control`.
- `docs/remote.md:24` ("Enter dismisses it for good") and `docs/tui.md:814` ("c copies it, Enter
  dismisses it"): also say the dialog closes when a device presents the ticket.
- `docs/remote.md:90` ("every `/remote` subcommand" refused for a device): also name
  `/remote-control`; `docs/remote.md:141-142` (devices reattach after "the next `/remote on` or
  `fontaine --remote`"): `/remote-control` also starts the endpoint.
- `docs/terminal.md:238`: the `remote:` row says the ticket dialog is removed when dismissed; it
  also closes when a device pairs.
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
  match the version for a known payload; at width 78 (the overlay's inner width at 80 columns) the
  639-char ticket (relay default, 2 IPv4 and 2 IPv6 direct addresses) fits at `max_height` 39 and
  not at 38. At the `TicketPrompt`/whole-terminal level that ticket shows the QR at 80×50 and the
  text fallback at 80×40. Measured (iroh 1.4.1,
  qrcode 0.14.1, ECC L, alphanumeric): relay disabled with 2 IPv4: 400 chars, version 11, 33 text
  rows; relay default with 2 IPv4: 477 chars, version 12, 35 rows; relay default with 2 IPv4 and
  2 IPv6: 639 chars, version 14, 39 rows. The test pins these rows rather than assuming them.
- **Cells-to-modules comparison:** rebuild the module grid from the rendered cells (glyph plus
  fg/bg), offset by the quiet zone with all-light padding, and assert it equals
  `QrCode::to_colors()`. A wrong half-block mapping or inverted polarity gives an unscannable code
  with no error.
- `TicketPrompt::body`: QR present when it fits (and the hint line absent), text ticket plus the
  too-small line when it does not, text only when `linear`; the drop clears the ticket. The existing
  unit test at `ticket_prompt.rs:92`, `hint_uses_the_env_form_an_interactive_attach_accepts`, moves
  to the text-fallback case and is updated to the new `body` signature.
- View level: `chrome()` and the draw path agree on QR mode for the same app state.

Integration (`rust/fontaine-tui/tests/remote.rs`, which drives a real `/remote on` and a client):

- `/remote-control` from `Off` opens the dialog (match the "Remote ticket" heading, which both modes
  show) and a live endpoint; run again while on, it mints a new ticket and the earlier ticket's
  client fails with `BadTicket` while the host audits a `device_refused` rule.
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

1. `cargo run -p fontaine`, then `/remote-control` in a tmux pane at 80×50, then at 80×40 and 80×24.
   Expected: a QR at 80×50; the text ticket and "terminal is too small" line at 80×40 and 80×24.
2. In the 80×50 pane, scan the code with a phone camera. Expected: the text decodes to the
   `FONTAINE-ATTACH1…` ticket. Run it in a light and a dark terminal theme: both scan.

Why not automated: whether a phone camera resolves terminal half-blocks, under real fonts, line
spacing and colour profiles, is a property of the terminal and optics. The cells-to-modules test covers
the encoding and mapping.
