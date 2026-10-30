# Slash Completion Mid-Prompt and Common-Prefix Tab Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/294

## Overview

The TUI's `/` menu only opens while the whole input is a bare `/name`, and Tab always writes the
selected row's full name. This plan makes a `/token` under the cursor anywhere in the prompt (at
the start of the input or after whitespace, like `@` mentions) open the same menu. Accepting a
match replaces only that token. It also makes Tab extend the typed name to the longest common
prefix of the matches before it accepts a row. Completion only edits the input; what is sent is
still what was typed.

## Current State

- `App::slash_menu()` (`rust/fontaine-tui/src/app.rs:801`) returns `None` unless
  `text.strip_prefix('/')` is a name with no whitespace. Its guards keep the menu closed when an
  overlay is open (`covered()`), during a history search, beside a `@` list, while history is
  being navigated, or after Esc dismissed it for this exact text (`slash_dismissed`).
- `slash_selected: (String, usize)` (`app.rs:185`) keys the selected row on the typed name.
  `refresh_slash()` (`app.rs:772`) resets it once `text.strip_prefix('/')` no longer equals that
  name.
- `slash_key()` (`app.rs:831`) handles Up and Down. Tab calls `complete_slash(name, has_hint)`,
  and Enter runs the selected command, or completes it when the command takes a required
  argument. `complete_slash()` (`app.rs:862`) replaces the **whole input** with `/name` plus a
  space when the entry has a hint.
- `@` mentions already solve the token problem. `complete::mention_at(text, cursor)`
  (`rust/fontaine-tui/src/complete.rs:19`) finds the `@` token the cursor is in, at the start of
  the input or after whitespace, as char offsets `start..end` plus a `query`.
  `accept_completion()` (`app.rs:318`) replaces that range with `@path ` through
  `InputBuffer::replace_range` (`input.rs:36`).
- `commands::match_menu(menu, typed)` (`commands.rs:446`) ranks entries: an exact name or alias
  first, then prefix matches, then contains matches. The comparison ignores case.
- `view::menu_lines` (`view.rs:383`) draws the menu from `slash_menu()`, and its doc comment says
  it is shown "while the first word of the message is a bare `/name`".
- `docs/tui.md` ("Slash-command menu", the key table rows for Tab and `/`) describes the
  bare-`/name`-only behavior.

### Claude Code's behavior (observed with `claude` 2.1.314 in tmux)

- Typing `please run /zork` opens the menu mid-prompt. Built-in commands are listed there too
  (`go /mode` lists `/model`).
- Tab accepts the **selected row** straight away. There is no common-prefix step: `/zork-` with
  matches `zork-alpha`, `zork-alphabet` and `zork-beta` became `/zork-beta`, and `/zork-al`
  followed by Down and Tab became `/zork-alphabet`. Mid-prompt, a space follows the name
  (`go /zork-beta X` after typing `X`).
- Mid-prompt, Enter submits the message as typed (`go /zork-al` was sent unchanged). It does not
  complete or run anything.

## Design

### Finding the token

Generalize `mention_at` in `complete.rs` into a sigil-parameterized finder, so `@` and `/` share
one implementation:

```rust
/// A sigil token in the input, as char offsets (`end` exclusive) and the text after the sigil.
pub struct Token { pub start: usize, pub end: usize, pub query: String }   // was `Mention`

pub fn token_at(text: &str, sigil: char, cursor: usize) -> Option<Token>;
pub fn mention_at(text: &str, cursor: usize) -> Option<Token> { token_at(text, '@', cursor) }
```

`Mention` is renamed to `Token` outright. The rules stay those of `mention_at`: the token
starts the input or follows whitespace, and the cursor is after the sigil and inside the token or
at its end.

### The menu state

`SlashMenu` gains the token and whether it is **bare**, meaning the token is the whole input
(`start == 0 && end == len`, which is today's only case):

```rust
pub struct SlashMenu<'a> {
    pub token: Token,           // replaces `typed`; `token.query` is the typed name
    pub bare: bool,
    pub matches: Vec<&'a MenuEntry>,
    pub selected: usize,
}
```

`slash_menu()` keeps every guard and adds `!self.shell_mode()`, so `!ls /u` or `!cd /p` opens no
menu (today `strip_prefix('/')` excludes `!` input implicitly). It finds the token with `token_at(text, '/', cursor)` in place
of `strip_prefix('/')`, and the rest is unchanged: `match_menu` on `token.query`, `None` on no
matches, and the `slash_dismissed` check. The mid-prompt menu lists the same entries, built-ins
included, as Claude Code does.

`slash_selected` is keyed on `(token.start, token.query)` rather than on the name alone, so moving
to another `/token` with the same text starts again at row 0. `refresh_slash()` compares against
the token at the cursor instead of `strip_prefix('/')`.

### Keys

| Key | Bare (`/name` is the whole input) | Inline (anywhere else) |
|---|---|---|
| Up / Down | Move the selection (unchanged). | Same. |
| Tab | Common-prefix step, then accept (below). | Same, with inline accepting. |
| Enter | Run the selected row, or complete one that requires an argument (unchanged). | Not taken by the menu: it falls through and the message is submitted as typed, as Claude Code does. |
| Esc / Ctrl-C | Dismiss until the text changes (unchanged). | Same. |

**Tab.** Compute `common_prefix(&matches)` over the matches' names. A new pure function in
`commands.rs` returns the longest prefix shared by every name, compared case-insensitively and
spelled as in the first name. When there is more than one match, and that prefix is longer than
the typed name and starts with it (ignoring case), Tab replaces the token with `/<prefix>`. It
adds no space, the menu stays open, and the selection resets to row 0 because the typed name
changed. Otherwise Tab accepts the selected row. The otherwise case covers a single match, a
prefix that does not narrow (a second Tab), and matches found through an alias or a contains match,
whose names do not start with the typed text. So `/zork-al` + Tab gives `/zork-alpha` (both
`zork-alpha*` still listed), and a second Tab accepts the selected `zork-alpha`.

**Accepting.** `complete_slash(name, space)` becomes `accept_slash(&menu, entry)`:
- Bare: the current behavior. The whole input becomes `/name`, with a space only when the entry
  has a hint.
- Inline: `replace_range(token.start, token.end, "/name")`, followed by a space unless the char
  after `token.end` is already whitespace. This mirrors `@` acceptance and Claude Code's inline
  space, without doubling an existing one. Text before and after the token is untouched. When no space is added, the cursor sits right
after the name, so the menu stays open on the exact match, as in the bare case.

Both reset history navigation as today.

### Behavior change at the bare position

The menu now needs the cursor inside the `/token` (after the `/`), as `@` does. A bare `/mod`
with the cursor moved to column 0 no longer shows the menu. Typing, history and `set_text` all
leave the cursor at the end, so this only shows after the cursor has been moved deliberately.

## Implementation Steps

1. `complete.rs`: add `Token` and `token_at(text, sigil, cursor)`. Re-express `mention_at` through
   it, rename `Mention` to `Token` at its uses in `app.rs` (`CompletionList`,
   `refresh_completion`, `accept_completion`, `dismiss_completion`) and in `linear.rs` (the
   `crate::complete::Mention { start, end, query }` literal in its test), and update the module doc
   to cover both sigils.
2. `commands.rs`: add `pub fn common_prefix(names: &[&str]) -> String`, with unit tests.
3. `app.rs`:
   - `SlashMenu`: replace `typed` with `token` and add `bare`.
   - `slash_menu()`: find the token at the cursor; compute `bare`; add the `!self.shell_mode()`
     guard.
   - `slash_selected` key `(usize, String)`; update `refresh_slash()` to match.
   - `slash_key()`: add the Tab prefix step; Enter only when `bare`; inline accept.
   - Replace `complete_slash` with the bare/inline accept described above.
4. `view.rs`: `menu_lines` reads `menu.token.query` in place of `menu.typed`. Update its doc
   comment. The `commands` footer keeps `Enter run` only when `menu.bare`; inline it reads
   `↑/↓ select · Tab complete · Esc close`, since Enter is not a menu key there.
5. Docs: `docs/tui.md`.
6. Master plan `## Decisions`: add the common-prefix Tab deviation from Claude Code (the first
   Decisions bullet below) as a `YYYY-MM-DD: ... (#294)` line; the date is the day it lands.

## Files to Modify

- `rust/fontaine-tui/src/complete.rs`
- `rust/fontaine-tui/src/commands.rs`
- `rust/fontaine-tui/src/app.rs`
- `rust/fontaine-tui/src/view.rs`
- `rust/fontaine-tui/src/linear.rs` (its test's `Mention` literal)
- `docs/tui.md`
- `tasks/fontaine_harness_plan.md` (`## Decisions`)
- `CHANGELOG.md` (by the PR step)

## Trade-offs

- **Shared `token_at` vs. a second `slash_at` copy.** The rule is identical apart from the sigil,
  so a single function keeps the two completions from drifting apart.
- **Common prefix over all matches vs. only the prefix-class ones.** With all matches, a contains
  or alias match leaves the prefix no longer than the typed text, and Tab simply accepts the
  selected row. That is predictable and needs no ranking knowledge in the prefix logic.
  Restricting the prefix to name-prefix matches would extend text that other listed rows do not
  share.

## Decisions

- Tab extends to the matches' longest common prefix before accepting a row, as the issue asks;
  Claude Code (2.1.314) accepts the selected row on the first Tab. The second Tab, once the prefix
  no longer narrows, accepts the selected row, as Claude Code's Tab does. Record this deviation in
  the master plan's Decisions.
- The inline menu lists built-in commands as well as skills, as Claude Code does. Accepting one
  only edits the text; a built-in named mid-prompt is not run.
- Enter with an inline menu open submits the message as typed (Claude Code's behavior); only a bare
  `/name` runs on Enter, because running a built-in from mid-prompt would discard the surrounding
  text.
- AFK: The inline menu's footer drops the `Enter run` hint (`↑/↓ select · Tab complete · Esc close`) rather than relabeling it, since Enter is not a menu key inline; the bare footer is unchanged.

## Documentation

`docs/tui.md`:
- Key table: the Tab row covers the `/` menu's common-prefix step. Replace the
  "`/` as the first character" row with "`/` at the start of the input or after a space".
- "Slash-command menu" section: describe the inline menu. Tab first extends to the common prefix,
  then accepts; inline acceptance replaces only the token and adds a space. Enter runs only a bare
  `/name`, and inline it sends the message. Replace "The menu only opens while the input is a bare
  `/name`" with the cursor-in-token rule, keeping the existing guard list (panels, history search,
  `@` list, recalled history).
- Line 110 ("Typing `/` as the first character lists the commands and skills…"): rewrite to say
  `/` at the start of the input or after a space.
- Main key table, Enter row (`docs/tui.md:93`): with the slash menu open, Enter runs the selected
  command only for a bare `/name`; inline it sends the message.

## Testing Strategy

All of this is reachable with unit tests in `app.rs` (existing `type_text`/`press` helpers),
`complete.rs` and `commands.rs`:

- `complete.rs`: `token_at` with `/`, at the start of the input, after a space and after a
  newline. A `/` inside a word (`a/b`, a path like `src/main`) is not a token. Existing `@` tests
  stay green through `mention_at`.
- `commands.rs`: `common_prefix`: a single name, a shared prefix, no shared prefix,
  case-insensitive comparison spelled as in the first name, and multi-byte chars.
- `app.rs`:
  - `hello /mod` opens the menu with `model` and `guardian-model`; `a/mod` does not.
    `hello /mod x` with the cursor at the end does not.
  - Inline Tab on a single match: `please run /add` + Tab gives `please run /add-dir ` and the
    menu closes. With text after the token (`run /cle now`, cursor after `cle`), the text after it
    is kept, no double space appears, and `slash_menu().is_some()` (the exact match).
  - Common prefix: a menu with entries `zork-alpha`, `zork-alphabet` and `zork-beta`. `/zork-al`
    + Tab gives `/zork-alpha` with the menu still open. A second Tab accepts `zork-alpha`, keeping
    the bare space rule. `/zork-` + Tab accepts the selected row, because the prefix does not
    narrow.
  - An alias or contains match: `/reset` + Tab still gives `/clear`, and `/mod` + Tab still gives
    `/model ` with the trailing space, since `model` has a hint (contains-match `guardian-model` blocks the prefix step).
  - Inline Enter returns `Action::Submit` with the text as typed. Bare Enter keeps its existing
    tests.
  - Selection keyed on the token: moving the selection, then editing another `/token` with the
    same name, starts at row 0.
  - Guards: no inline menu while a `@` list, history search or panel is open (extend the existing
    precedence tests).
  - Esc inline dismisses until the text changes.
  - Shell mode: `!ls /u` opens no menu.
- `view.rs`: for an inline menu (`hello /mod`) the footer is `↑/↓ select · Tab complete · Esc
  close`; the existing bare-footer test stays as is.
- Update `the_slash_menu_opens_only_for_a_bare_name`, which asserts `hello /mod` has no menu, to
  the new rule.

## Manual Verification

1. `cd rust && cargo run -p fontaine`, type `please run /mod`, and check that the menu appears
   under the input. Press Tab and confirm that `please run /model ` stays on one line with the
   cursor after the space. This checks the live-area rendering of an inline menu, which
   `TestBackend` unit tests don't exercise against a real terminal.
