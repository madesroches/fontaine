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
- `docs/tui.md` mentions terminals that deliver paste as text, assuming the bracketed path works.

## Design

A new module `fontaine-tui/src/paste_burst.rs` holds the burst logic as a pure function over an
event source, so it is unit-testable without a terminal.

```text
pub(crate) fn coalesce(
    first: KeyEvent,
    next: impl FnMut(Duration) -> Option<Event>,   // None = nothing within the gap
) -> Burst
pub(crate) enum Burst { Paste(String), Keys(Vec<KeyEvent>), /* + a trailing non-burst event */ }
```

Rules:

- A key is a *burst key* when its kind is `Press` or `Repeat`, and it is a `Char` with no CONTROL or
  ALT modifier (SHIFT allowed), `Enter` with no modifiers, or `Ctrl+J` (how some consoles deliver
  `\n`), or `Tab`.
- Only a burst key starts a burst. After it, `next` is asked for another event with a gap of
  `BURST_GAP` (5 ms). Burst keys keep extending it; the first gap, or the first non-burst event,
  ends it. A non-burst event that ended the burst is kept and delivered right after the burst,
  never dropped (`Release` events are skipped inside the burst: Windows reports them).
- If the collected keys include an `Enter`/`Ctrl+J` **and** at least one burst key after the first
  `Enter`, the burst is a paste: the keys become text (`Enter`/`Ctrl+J` → `\n`, `Tab` → `\t`) and
  are sent as one `Input::Paste`. A burst ending with the newline (`"a\n"`) counts. A lone `Enter`
  is not a burst, so `Enter` typed by hand submits.
- Any other burst (no newline) is replayed as the original key events, in order, so typing,
  key repeat and vim-mode keys behave exactly as before.

`read_terminal` calls `coalesce` after reading a burst key, only when `paste_burst::enabled()` is
true. `enabled()` is `cfg!(windows)`; a `FONTAINE_PASTE_BURST` override is deliberately not added.
Events are read with `event::poll(gap)` + `event::read()`; the existing pause and stop checks
stay at the top of the loop.

Why a time gap and not a pre-submit buffer: a pasted block lands in the console input queue in
one write, so successive reads are well under 1 ms apart, while a human cannot type two keys 5 ms
apart.

## Implementation Steps

1. Add `paste_burst.rs` with `coalesce`, the burst-key predicate, the key-to-text mapping and
   `enabled()`; register it in `lib.rs`.
2. Change `read_terminal` to route key events through `coalesce` when enabled, sending the
   resulting inputs in order.
3. Make the event read testable: `read_terminal` passes a closure over `event::poll`/`read`;
   `coalesce` itself takes the closure.
4. Unit tests (below).
5. Update `docs/tui.md` and `CHANGELOG.md`; add the deviation line to the plan's `## Decisions`
   (TUI subsection).

## Files to Modify

- `rust/fontaine-tui/src/paste_burst.rs` (new)
- `rust/fontaine-tui/src/lib.rs`
- `docs/tui.md`
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Enable VT input and parse `ESC[200~`/`ESC[201~` ourselves**: exact, but means leaving
  crossterm's Windows reader or parsing raw console input ourselves, and only works where the host
  supports VT input. The burst heuristic works on every Windows console and stays in the reader.
- **Treat only Shift/Alt+Enter and Ctrl+J as newline**: doesn't fix paste.
- **Run the heuristic on all platforms**: unneeded where bracketed paste works, and risks
  misclassifying fast key sequences from remote terminals; `enabled()` limits it to Windows.
- **Buffer chars-only bursts into a paste too**: no newline means no harm from keys, and replaying
  keys keeps key bindings working.

## Decisions

## Documentation

`docs/tui.md`: say that on Windows consoles pasted text arrives as keys and is detected as a
burst, so a multi-line paste is inserted rather than submitted.

## Testing Strategy

All behavior is reachable with a scripted event source:

- Burst of `Char`, `Enter`, `Char` yields one `Paste("ab\ncd")`-style result and no `Enter` key,
  so no submit (the bug in the issue).
- CRLF delivered as two Enter events yields `"\n\n"` (the existing `input.rs` normaliser collapses
  `\r\n` text only; blank-line fidelity is accepted, see Open Questions).
- A lone `Enter` after a gap yields the key (submit still works by hand).
- `Char`, then gap, then `Enter` yields two separate key results.
- A chars-only burst is replayed as the original keys.
- Ctrl/Alt-modified chars and Release events don't start or join a burst; a non-burst event that
  ends a burst is delivered after it, in order.
- Trailing newline: `"a"`, `Enter` yields `Paste("a\n")`.
- A test through the existing `common` harness that `Input::Paste` with newlines inserts without
  `Action::Submit` already holds via `app.rs:1947`; no new app test is needed.
- Setup prompts and the linear interface read through the same `read_terminal`, so they are covered
  by the unit tests; their paste handlers already exist (`setup.rs:369`).

## Manual Verification

1. On a Windows terminal, `cargo run -p fontaine`, paste a three-line block into the prompt.
   Expected: one input box containing three lines, nothing submitted. Not automated because the
   crossterm Windows console reader cannot be driven from a test.
2. Type `hi`, pause, press Enter. Expected: submits.
3. Repeat with `FONTAINE`'s screen reader mode (linear interface).

## Open Questions

- Does Windows Terminal deliver a pasted blank line as two Enter events (`\r\n` → `Enter`,
  `Enter`)? If it does, each pasted line break doubles. To be settled by manual verification on
  a real console; if it does, collapse a `\r`-then-`\n` pair in the key-to-text mapping.
