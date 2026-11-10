# AltGr Character Input Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/556

## Overview

On Windows, a character composed with AltGr (French Canadian AltGr+2 = `@`, plus `€ { } [ ] ~`,
and every AltGr character on every such layout) never reaches the message box or the TUI's
filter boxes. The Windows console reports AltGr as Ctrl+Alt held together, with the composed
character already in the event (`KeyEvent { code: Char('@'), modifiers: CONTROL | ALT }`), and
every text-entry path in `fontaine-tui` treats any Ctrl or Alt `Char` as a shortcut. One shared
predicate decides "what text does this key type", recognising the AltGr signature on Windows, and
every text-entry path uses it.

## Current State

- `rust/fontaine-tui/src/input.rs`
  - `emacs_key` (line ~344): `Char(c) if control` matches the readline shortcuts, `_ => return
    false`; `Char(c) if alt` likewise. A `CONTROL | ALT` `@` hits the control arm, matches no
    shortcut and returns false — the key is dropped.
  - `vim_insert_key` (line ~391): inserts only `Char(c) if !control && !alt`.
  - `vim_normal_key` (line ~425): `Char(_)` with Ctrl/Alt returns false, so an AltGr char cannot
    be the target of `r` (replace) and is not consumed as a Normal-mode key.
- `rust/fontaine-tui/src/app.rs` `handle_key` (line ~1100) looks the key up in the keymap
  (`self.keymap.lookup(key)`) before the editor sees it (line ~1755), so a bound Ctrl+Alt key
  still wins. `search_key` (line ~672, Ctrl+R history search) pushes only chars with neither
  Ctrl nor Alt.
- Filter/text boxes with the same `!intersects(CONTROL | ALT)` guard:
  - `picker.rs` `edit_query` (line 22) — shared by `model_picker.rs` and `resume_picker.rs`.
  - `config_panel.rs` filter (line ~197) and numeric field (line ~275).
  - `provider_dialog.rs` `is_text` (line 164), used by its three text fields.
  - `question.rs` (line ~149): a non-shortcut char starts the "Other" answer.
- `elicitation.rs` text fields are `InputBuffer`s, so they follow `input.rs`.
- `paste_burst.rs` `burst_text` (line 22) already accepts `CONTROL | ALT` chars ("Windows reports
  AltGr as CONTROL|ALT"); the burst reader only runs on Windows (`enabled()`).
- `app.rs` (line ~1360) sends a key to `list_key` only when `!control`, so under the plan an AltGr
  char would skip the focused agent list and be typed into the prompt while the list kept focus.
  The blur lives in `agent_list.rs` (line ~348), whose `plain` (line ~289) excludes CONTROL | ALT.
- Not text entry, left alone: `transcript_view.rs`, `elicitation.rs`'s
  `plain` (bool/select toggles), `setup_prompt.rs` (digit/`y`/`n` choices).

## Design

One predicate in `input.rs`, next to the editor that owns text entry:

```rust
/// Whether the platform's terminal reports AltGr as Ctrl+Alt held together.
const ALTGR_IS_CTRL_ALT: bool = cfg!(windows);

/// The character a key types into a text field, or `None` when it is not text: a `Char` with
/// neither Ctrl nor Alt (Shift is fine), or an AltGr-composed `Char`.
pub(crate) fn typed_char(key: &KeyEvent) -> Option<char> {
    // #[cfg(test)]: reads a thread-local override of the flag so tests exercise the AltGr
    // path on every OS; production code has no runtime switch.
    typed_char_on(key, ALTGR_IS_CTRL_ALT)
}

pub(crate) fn typed_char_on(key: &KeyEvent, altgr_is_ctrl_alt: bool) -> Option<char>

/// #[cfg(test)] helper: runs the closure with `typed_char` seeing the given flag.
fn with_altgr<R>(on: bool, f: impl FnOnce() -> R) -> R
```

`typed_char_on` returns `Some(c)` for `KeyCode::Char(c)` when:

- the modifiers intersect neither CONTROL nor ALT, or
- `altgr_is_ctrl_alt` and the modifiers contain both CONTROL and ALT and `c` is not an ASCII
  letter or digit and not a control character.

Uses:

- `emacs_key`: a first arm `KeyCode::Char(_) if let Some(c) = typed_char(&key) =>
  self.insert(c)`, ahead of the Ctrl/Alt shortcut arms; the old bare `Char(c) => insert` arm
  becomes unreachable for text and is removed (a `Char` that is not typed text and matches no
  shortcut returns false as today).
- `vim_insert_key`: `Char(c) if !control && !alt` becomes the same `typed_char` arm.
- `vim_normal_key`: `Char(c) if !control && !alt` becomes `typed_char`, so `r@` and an unknown
  AltGr char behave like any other printable key in Normal mode.
- `app.rs` `search_key`, `picker::edit_query`, `config_panel.rs` (both sites),
  `provider_dialog::is_text` (body becomes `typed_char(key)`; keep or inline the wrapper,
  implementer's call), `question.rs`: replace the hand-written guard with `typed_char`.
- `app.rs` list-focus gate (line ~1360): `!control` becomes
  `(!control || (typed_char(&key).is_some() && mapped.is_none()))` (the gate runs before the
  bound-action checks, so a bound Ctrl+Alt chord must not be diverted to the list), and `agent_list.rs`'s blur arm (line ~348) becomes `KeyCode::Char(_) if typed_char(&key).is_some()`,
  so an AltGr char blurs the list and is then typed into the prompt.
- `paste_burst::burst_text`: the `Char` arm becomes `typed_char_on(key, true)` — the burst reader
  is Windows-only, so it always reads Ctrl+Alt as AltGr. Its existing comment moves to the call.

## Implementation Steps

1. `input.rs`: add `ALTGR_IS_CTRL_ALT`, `typed_char` (with the `#[cfg(test)]` thread-local
   override), `typed_char_on`, the `with_altgr` test helper; rewrite the `Char` arms of
   `emacs_key`, `vim_insert_key`, `vim_normal_key` to use `typed_char`.
2. Replace the guards in `app.rs` (`search_key` and the list-focus gate), `agent_list.rs`
   (blur arm), `picker.rs`, `config_panel.rs`, `provider_dialog.rs`, `question.rs`.
3. `paste_burst.rs`: `burst_text` uses `typed_char_on(key, true)`.
4. Tests (below).
5. Docs: `docs/tui.md` `## Keys`, next to `### Pasting`: one sentence that AltGr characters type
   normally on Windows, and that a Ctrl+Alt chord bound in the keymap still takes precedence.
   `docs/configuration.md` `### [keybindings]`: one sentence that on Windows a `ctrl+alt+<char>`
   binding hides that AltGr character.
6. `CHANGELOG.md` entry under Fixed (the `pr` step).

## Files to Modify

- `rust/fontaine-tui/src/input.rs`
- `rust/fontaine-tui/src/app.rs`
- `rust/fontaine-tui/src/agent_list.rs`
- `rust/fontaine-tui/src/picker.rs`
- `rust/fontaine-tui/src/config_panel.rs`
- `rust/fontaine-tui/src/provider_dialog.rs`
- `rust/fontaine-tui/src/question.rs`
- `rust/fontaine-tui/src/paste_burst.rs`
- `docs/tui.md`
- `docs/configuration.md`
- `CHANGELOG.md`

## Decisions

- 2026-11-09: AltGr-as-Ctrl+Alt is recognised only on Windows (a `const` gate), as only that console reports it so.
- 2026-11-09: Ctrl+Alt+ASCII-letter is never treated as AltGr text, so those readline chords keep working.
- 2026-11-09: The predicate lives in `input.rs`, not a new `keys.rs` module.
- 2026-11-09: AFK: Ctrl+Alt+ASCII-digit is also never AltGr text (crossterm reports a non-AltGr Ctrl+Alt+2 as `Char('2')` with CONTROL|ALT, and no mainstream layout's AltGr composes a digit); a non-AltGr Ctrl+Alt+punctuation chord on Windows now types its base character, accepted because crossterm cannot tell it from AltGr `@ { } [ ] \ | ~`.

## Documentation

- `docs/tui.md`: AltGr note under `## Keys`, next to `### Pasting`.
- `docs/configuration.md`: `### [keybindings]` note that a Windows `ctrl+alt+<char>` binding hides that AltGr character.

## Testing Strategy

This is a bug seen in the wild; it is pinned at the unit tier, which reproduces it exactly
(crossterm hands the app a constructed `KeyEvent`, nothing below that is involved).

- `input.rs` tests:
  - `typed_char_on` table: plain/shifted char → Some; Ctrl+char, Alt+char → None; Ctrl+Alt `@`,
    `€`, `{`, `ł` with the flag → Some, without → None; Ctrl+Alt `a`/`A`/`2` → None; Ctrl+Alt control char (flag on) →
    None; unmodified control char → Some (as today); non-`Char` → None.
  - Regression for #556: an `InputBuffer` in Emacs mode given `Char('@')` with `CONTROL | ALT`
    gets `@` inserted, run under `with_altgr(true, || ...)` so it holds on every CI OS.
  - Emacs Ctrl+Alt+`a` still moves home (with the flag on); vim Insert inserts an AltGr `{`; vim
    Normal `r` then AltGr `@` replaces the char.
- `agent_list.rs`/`app.rs`: with the list focused, an AltGr char (flag on) blurs the list and is
  typed into the prompt; a bound Ctrl+Alt+`@` chord (flag on) still fires its action while the
  list is focused.
- `picker.rs`, `provider_dialog.rs`, `config_panel.rs`, `question.rs`: one test each that an
  AltGr char (flag on) reaches the query/field.
- `paste_burst.rs`: existing CONTROL|ALT test stays green.

## Manual Verification

On Windows with the French Canadian layout: `cargo run -p fontaine`, press AltGr+2 in the prompt
→ `@` appears; repeat in `/model` filter. Not automated because it needs the real console
keyboard layout translation.
