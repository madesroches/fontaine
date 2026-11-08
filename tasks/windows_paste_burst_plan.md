# Windows Multi-Line Paste Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/514

## Overview

On Windows, crossterm 0.29 reads the console API and never produces `Event::Paste`, so a pasted
multi-line block arrives as `Char` and `Enter` key events. Each `Enter` submits, so every line
becomes its own message. The terminal reader will recognise such a burst of key events and turn it
into one `Input::Paste` whenever it contains a newline, so the existing paste path handles it.

## Current State

- `fontaine-tui/src/lib.rs` `read_terminal` (line ~562) maps `Event::Key` → `Input::Key`,
  `Event::Paste` → `Input::Paste`, `Event::Resize` → `Input::Resize`, one event at a time. It is
  shared by `run` and `setup::run` (`setup.rs:196`), so the linear (screen reader) interface and
  the setup prompts read through it too.
- `EnableBracketedPaste` is sent with `let _ =` in `lib.rs` (`run`, after a suspend) and
  `setup.rs:185`; on Windows it has no effect.
- `app.rs` `dispatch_input` (line ~926) routes `Input::Paste` to the overlay that owns it, the
  search box or the message box; `input.rs:146` already normalises `\r\n` and `\r` to `\n`.
- A plain `Enter` key event is submit in the message box, so unbracketed pasted text is submitted.
- `docs/tui.md` describes paste around lines 356, 434 and 889.

## Design

A new module `fontaine-tui/src/paste_burst.rs` holds the burst logic as a pure function over an
event source, so it is unit-testable without a terminal.

```text
pub(crate) fn coalesce(
    first: KeyEvent,
    next: impl FnMut(Duration) -> io::Result<Option<Event>>,   // None = nothing within the gap,
                                                              // or stop/pause.requested is set
) -> io::Result<Vec<Input>>
```

`coalesce` returns the inputs to send, in order, including a trailing non-burst event that ended
the burst, mapped to `Input` (`Resize` → `Input::Resize`, `Paste` → `Input::Paste`, other events
discarded). On `Err` from `next` it returns the inputs collected so far followed by the error, so
they are flushed before the error is reported. The closure returns `None` when `stop` or
`pause.requested` is set, so the burst ends and is flushed.

Rules:

- A key is a *burst key* when it is a `Char` with no CONTROL or ALT modifier (SHIFT allowed), a
  `Char` with exactly CONTROL|ALT (crossterm's Windows reader reports AltGr that way; it maps to
  its char), `Enter` with no modifiers, or `Tab`. Pure Ctrl+char and pure Alt+char are not burst
  keys and end the burst. Ctrl+J is not special: crossterm delivers it as `Char('j')` with CONTROL.
- A Release is dropped: it does not start, end or count toward a burst, but resets the gap timer.
  Releases are dropped from a replayed (non-paste) burst too.
- Only a burst key starts a burst. After it, `next` is asked for another event with a gap of
  `BURST_GAP` (15 ms), measured per event, not over the whole burst. Burst keys keep extending it;
  the first gap, or the first non-burst event, ends it. A non-burst event that ended the burst is
  kept and delivered right after the burst, never dropped.
- A burst is a paste iff it holds at least two burst keys (Releases dropped) and at least one is
  `Enter`. The keys become text (`Enter` → `\n`, `Tab` → `\t`, others → their char) and are sent as
  one `Input::Paste`. A lone `Enter` stays a key, so `Enter` typed by hand submits; `Enter`,
  `Enter` gives `Paste("\n\n")`; `a`, `Enter` gives `Paste("a\n")`.
- Any other burst (no newline) is replayed as the original key events, in order, so typing,
  key repeat and vim-mode keys behave exactly as before. Replaying a CONTROL|ALT key leaves app
  key handling unchanged.

`read_terminal` calls `coalesce` after reading a burst key, only when `paste_burst::enabled()` is
true. `enabled()` is `cfg!(windows)`; a `FONTAINE_PASTE_BURST` override is deliberately not added.
Events are read with `event::poll(gap)` + `event::read()`; the existing pause and stop checks
stay at the top of the loop.

Why a time gap and not a pre-submit buffer: a pasted block lands in the console input queue in
one write, so successive reads are well under 1 ms apart, while a human cannot type two keys 15 ms
apart.

## Implementation Steps

1. Add `paste_burst.rs` with `coalesce`, the burst-key predicate, the key-to-text mapping and
   `enabled()`; register it in `lib.rs`.
2. Change `read_terminal` to route a burst key through `coalesce` when enabled, sending the
   returned inputs in order and then the error, if any.
3. Make the event read testable: `read_terminal` passes a closure over `event::poll`/`read` that
   returns `None` when `stop` or `pause.requested` is set; `coalesce` itself takes the closure.
4. Unit tests (below).
5. Update `docs/tui.md` and add a `CHANGELOG.md` entry under `## [Unreleased]` → `### Fixed`,
   ending with `(#514)`; add the deviation line to the plan's `## Decisions`
   (TUI subsection).

## Files to Modify

- `rust/fontaine-tui/src/paste_burst.rs` (new)
- `rust/fontaine-tui/src/lib.rs`
- `docs/tui.md`
- `CHANGELOG.md` (`## [Unreleased]` → `### Fixed`, ending with `(#514)`)
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Enable VT input and parse `ESC[200~`/`ESC[201~` ourselves**: exact, but means leaving
  crossterm's Windows reader or parsing raw console input ourselves, and only works where the host
  supports VT input. The burst heuristic works on every Windows console and stays in the reader.
- **Run the heuristic on all platforms**: unneeded where bracketed paste works, and risks
  misclassifying fast key sequences from remote terminals; `enabled()` limits it to Windows.
- **Buffer chars-only bursts into a paste too**: no newline means no harm from keys, and replaying
  keys keeps key bindings working. Consequence: a dropped file path (single-line, chars-only) is
  replayed as keys, so PDF attachment by path (`dropped_pdf`) is not detected on Windows unless
  the path arrives bracketed.

## Decisions

- AFK: The LF of a pasted CRLF on Windows is an unverified console behavior; no CRLF collapsing or Ctrl+J special case is added (key-level collapsing is impossible and contradicts `Enter`, `Enter` => `Paste("\n\n")`); it is settled by manual verification.
- AFK: Dropping a file path onto the window is not detected as a PDF attachment on Windows unless it arrives bracketed, since it is a single-line chars-only burst replayed as keys; no length threshold is added.

## Documentation

`docs/tui.md` (the paste text around lines 356, 434 and 889): say that on Windows consoles
pasted text arrives as keys and is detected as a burst, so a multi-line paste is inserted rather
than submitted. Also say that attaching a PDF by dropping its path (line ~356) is not detected on
Windows unless the path arrives as a bracketed paste.

## Testing Strategy

All behavior is reachable with a scripted event source:

- Burst of `Char`, `Enter`, `Char` yields one `Paste("ab\ncd")`-style result and no `Enter` key,
  so no submit (the bug in the issue).
- `Enter`, `Enter` yields `Paste("\n\n")`.
- `Enter` alone (after a gap) yields the key (submit still works by hand).
- `Char`, then gap, then `Enter` yields two separate key results.
- A chars-only burst of Press events is replayed as the original keys.
- Pure Ctrl/Alt-modified chars don't start or join a burst; a Release is dropped and resets the gap
  timer; a non-burst event that ends a burst is delivered after it, in order.
- A `Char` with CONTROL|ALT (AltGr) joins a burst and maps to its char.
- Trailing newline: `"a"`, `Enter` yields `Paste("a\n")`.
- An error from `next` flushes the collected inputs, then returns the error.
- A test in `app.rs` that pasting `"a\nb"` into the message box inserts it without `Action::Submit`.

## Manual Verification

1. On a Windows terminal, `cargo run -p fontaine`, paste a three-line block into the prompt.
   Expected: one input box containing three lines, nothing submitted. Not automated because the
   crossterm Windows console reader cannot be driven from a test.
2. Type `hi`, pause, press Enter. Expected: submits.
3. Repeat with `fontaine --ax-screen-reader` (linear interface).
4. Paste a two-line block and check what the console delivers for the LF of a pasted CRLF
   (Enter twice, `Char('j')`+CONTROL, or nothing). Expected: one line break inserted. If it
   differs, record what was seen; the burst rules are not changed speculatively.
