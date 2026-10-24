# Terminal Polish: Themes, Screen-Reader Mode, Terminal Setup Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/50

## Overview

Claude Code offers colour themes (`/theme`, the `theme` setting, colourblind-friendly
"daltonized" presets), a screen-reader mode that swaps the visual interface for plain, labelled,
linear text (`--ax-screen-reader`, `CLAUDE_AX_SCREEN_READER`, `axScreenReader`), and
`/terminal-setup` plus a terminal-configuration page for Shift+Enter, Option-as-Meta and tmux.
Fontaine adds the same three things to its ratatui terminal UI:

1. **Theme tokens.** Every colour in the TUI comes from a `Theme` value with semantic tokens
   (`success`, `error`, `warning`, …). Six presets, named like Claude Code's. You choose one with
   `[tui] theme` or `/theme`.
2. **Screen-reader mode.** A linear renderer replaces the inline live area. It prints settled
   transcript entries as labelled lines (`you:`, `assistant:`, `tool:`, …) and dialogs as plain
   text, echoes what you type, announces mode changes, and rings the bell when a reply finishes
   or a prompt opens. Key handling does not change.
3. **Terminal setup docs.** A new `docs/terminal.md` page covers the newline keys in each
   terminal, tmux `extended-keys`, Option as Meta, `NO_COLOR`, OSC 52, themes and screen-reader
   mode. There is no `/terminal-setup` command.

## Current State

- **Colours are hard-coded.** About 20 `Color::*` uses are spread over the TUI, with no
  indirection:
  - `transcript.rs:351`: user message, cyan + bold.
  - `transcript.rs:368-376,399`: tool done (green), and failed/denied/error (red).
  - `statusbar.rs:41-44`: mode colours (manual/dontAsk gray, accept edits green, plan cyan,
    auto yellow).
  - `view.rs:346`: the `!` shell input border, magenta.
  - Dialog headings, bold yellow: `approval.rs:221`, `server_prompt.rs:54`, `setup.rs:165`,
    `trust_prompt.rs:105`, `agents_panel.rs:154,190`, `denials.rs:87`, `model_picker.rs:305`.
  - `denials.rs:233`: bold red.
  - `provider_dialog.rs:631,765`: yellow warnings.

  Emphasis that is not colour (`DIM`, `BOLD`, `REVERSED` for the selected row) is used
  throughout. Every overlay marks its selected row with `REVERSED`, except the `@` list, which
  uses a `> ` prefix plus `BOLD` (`view.rs:213`).
- **Rendering.** `lib.rs::drive` (`lib.rs:1487`) runs the session loop with a
  `render: FnMut(&mut App)` callback.
  - `lib.rs::run` (`lib.rs:1612`) enables raw mode and bracketed paste, then builds a
    `screen::Screen`, an inline viewport. `Screen::render` commits settled transcript lines into
    scrollback and redraws the live area through `view::draw`.
  - `view::overlay(app, width, height)` (`view.rs:157`) is the single place that decides which
    dialog sits above the input and yields `Overlay { title, body, footer }` as styled `Line`s.
  - `Transcript::commit(width)` (`transcript.rs:241`) advances a commit cursor over settled
    entries and streams a reply row by row.
- **Startup screens run before `drive`, each on its own `ratatui::try_init()` terminal.**
  - The trust prompt is `trust_prompt::ask` (`trust_prompt.rs:180`).
  - The no-model setup dialog is `setup::run`/`setup::drive` (`setup.rs:86,111`). It draws its
    banner plus a `(title, body, footer)` through `view::draw_overlay`.
- **Config.**
  - `fontaine-core/src/config.rs::Settings` (`config.rs:586`) is `deny_unknown_fields`, with
    per-column scope checks in `check_scope` (`config.rs:636`). `accounts` is the precedent for
    a section that is user/CLI-only.
  - `user_config::set_string` (`user_config.rs:68`) writes one key with `toml_edit`, keeping
    comments. `set_model` and `set_guardian_model` wrap it.
- **CLI.** `fontaine/src/cli.rs` (clap derive). The bin calls `trust_prompt::ask` at
  `fontaine/src/lib.rs:181`, `setup::run` at `:655` and `fontaine_tui::run` at `:685`.
- **Slash commands.** `commands.rs::BUILTINS` is one table that the parser and the `/` menu both
  read. `/output-style` (`lib.rs:487`) is the precedent for "no argument lists, an argument
  switches".
- **Keys.** Alt+Enter, Shift+Enter (when the terminal reports it), Ctrl+J, or a trailing `\`
  insert a newline (`app.rs:540-590,633`). Fontaine does not enable the kitty keyboard protocol.
- **Colour suppression.** crossterm 0.29 already honours `NO_COLOR` (`style/types/colored.rs:73`).
- **Docs.** `docs/tui.md` documents keys and commands. `docs/configuration.md` has the per-key
  scope table, the key reference and the flag/env table. There is no terminal-setup or
  accessibility page.

## Design

### 1. Config: `[tui]` section (fontaine-core)

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    Dark, Light, DarkDaltonized, LightDaltonized,
    #[default] DarkAnsi, LightAnsi,
}
impl ThemeName {
    pub const ALL: [ThemeName; 6];
    pub fn as_str(self) -> &'static str;          // "dark-ansi" …
    pub fn parse(name: &str) -> Option<ThemeName>; // for `/theme <name>`
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TuiSection {
    pub theme: Option<ThemeName>,
    pub screen_reader: Option<bool>,
}

pub struct Tui { pub theme: ThemeName, pub screen_reader: Option<bool> }   // in Config
```

- **Settings and merge.** `Settings.tui: Option<TuiSection>`; `Config.tui: Tui`. Each key is
  merged with the usual highest-scope-wins rule (CLI over user).
- **Scope.** `check_scope` rejects `tui` in the `ProjectLocal` and `Project` columns, in the same
  block as `accounts`, with the reason "terminal preferences come only from the user file or
  --settings". The user file and `--settings` may set it.
- **Writer.** `user_config::set_theme(path, name)` wraps `set_string(path, "tui", "theme", name)`.

### 2. Themes (fontaine-tui `theme.rs`)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub user: Style,        // your `>` messages
    pub success: Style,     // tool done
    pub error: Style,       // failed/denied tool, error entries, denied-list heading
    pub warning: Style,     // provider-dialog warnings
    pub heading: Style,     // dialog headings (approval, trust, server, setup, agents, denied, picker groups)
    pub mode_manual: Style, // manual, dontAsk
    pub mode_accept: Style, // accept edits
    pub mode_plan: Style,
    pub mode_auto: Style,
    pub shell: Style,       // `!` input border
}
impl Theme { pub fn preset(name: ThemeName) -> Theme; }
```

- **What a token holds.** Tokens hold colour only. Call sites keep adding `BOLD`/`DIM`, as they
  do today; `heading` is bold yellow today, so call sites write `theme.heading.add_modifier(BOLD)`.
  Selection stays `REVERSED` and is not a token.
- **Presets.**

  | Token | `dark-ansi` (default, today's palette) | `light-ansi` | `dark` | `light` | `dark-daltonized` | `light-daltonized` |
  |---|---|---|---|---|---|---|
  | user | Cyan | Blue | Rgb(122,180,232) | Rgb(0,95,175) | Rgb(122,180,232) | Rgb(0,95,175) |
  | success | Green | Green | Rgb(78,186,101) | Rgb(44,122,57) | Rgb(51,153,255) | Rgb(0,102,153) |
  | error | Red | Red | Rgb(255,107,128) | Rgb(171,43,63) | Rgb(255,153,51) | Rgb(204,102,0) |
  | warning, heading, mode_auto | Yellow | Magenta | Rgb(255,193,7) | Rgb(150,108,30) | Rgb(255,204,0) | Rgb(150,108,30) |
  | mode_manual | Gray | DarkGray | Rgb(153,153,153) | Rgb(102,102,102) | Rgb(153,153,153) | Rgb(102,102,102) |
  | mode_accept | Green | Green | Rgb(175,135,255) | Rgb(135,0,255) | Rgb(175,135,255) | Rgb(135,0,255) |
  | mode_plan | Cyan | Blue | Rgb(72,150,140) | Rgb(0,102,102) | Rgb(72,150,140) | Rgb(0,102,102) |
  | shell | Magenta | Magenta | Rgb(253,93,177) | Rgb(255,0,135) | Rgb(253,93,177) | Rgb(255,0,135) |

  - The daltonized presets put success and error on the blue/orange axis, which red–green
    colourblind readers can tell apart.
  - The `-ansi` presets use the 16 named colours, so they follow the terminal's own scheme and
    need no truecolor support.
- **Plumbing.**
  - `App` gets `pub theme: Theme`. `drive` sets it from `Theme::preset(setup.config.tui.theme)`.
  - `Transcript::commit`, `live_lines` and `lines` take `&Theme`, and `entry_lines` uses it.
  - `view::overlay`/`draw` pass `&app.theme` into each overlay's `body`/`footer`/`lines` that
    colours anything: approval, server prompt, agents panel, denials, model picker and provider
    dialog.
  - `statusbar::render` reads `app.theme`.
  - `trust_prompt::ask` and `setup::SetupParams` take a `Theme`.
  - Overlays that only use `DIM`/`BOLD` keep their signatures.
- **Scrollback.** Lines already written to scrollback keep the colours they were written in.
  Changing the theme affects new lines and the live area.
- **`/theme [<name>]`.** This is `Command::Theme(String)` in `BUILTINS` (hint `[<name>]`,
  description "choose the colour theme").
  - With no argument, a notice lists the six names, marks the current one `(current)` and names
    the file the choice is saved to.
  - With a name, it sets `app.theme`, saves `[tui] theme` through `user_config::set_theme`, and
    confirms. A save failure is reported, and the theme still applies for the session.
  - An unknown name is an error notice that lists the valid names.
  - It works during a turn: it touches only the interface.

### 3. Screen-reader mode (fontaine-tui `linear.rs`, `accessibility.rs`)

**Turning it on.** `accessibility::resolve(flag: bool, env: Option<&str>, setting: Option<bool>)
-> Option<Source>`, where `Source` is `Flag | Env | Settings`. Precedence follows Claude Code:

1. `--ax-screen-reader` turns it on.
2. `FONTAINE_AX_SCREEN_READER=1` turns it on, and `=0` keeps it off even when the setting is
   `true`. Any other value is ignored, and a startup warning names it.
3. `[tui] screen_reader = true` turns it on.

The bin resolves it once, after config load and before the trust prompt. When the mode is on it
prints `[Screen Reader Mode: on via flag|env|settings]` as plain text before any raw mode, then
passes the `Option<Source>` to `trust_prompt::ask`, `setup::run` and `fontaine_tui::run`. Headless
`-p` ignores the mode.

**The linear renderer.** `Linear<W: Write>` implements the render callback `drive` already takes,
so `run` picks `Linear::new(stdout)` or `Screen`. Raw mode, bracketed paste and the key reader are
unchanged, and the cursor stays visible. Each `render(&mut App)` builds its output in this order:

1. **Transcript.** A new `Transcript::commit_entries()` advances the same commit cursor and
   returns the newly settled pieces (`entry`, chars already written, whether this continues an
   entry). Both the styled `commit(width, theme)` and the linear labeller use it, so settling
   rules live in one place.
   - In linear mode a streaming reply commits up to its last `\n`, not by width. The terminal
     wraps lines.
   - The labels are:

     | Entry | Label |
     |---|---|
     | User | `you:` |
     | Assistant | `assistant:` (on the first piece only) |
     | Tool done, pending or interrupted | `tool: <tool> <summary> — done` / `— interrupted` |
     | Tool failed or denied | `tool error: <tool> <summary> — failed: …` / `— denied: …` |
     | Error | `error:` |
     | Notice | `notice:` |
     | Shell | `shell: <command>`, followed by its output lines |

2. **Overlay.** `view::overlay` becomes `pub(crate)`. It is called with width 1000 and height
   `u16::MAX`, so nothing is clipped or windowed, and every `Line` is flattened to text: span
   contents are joined and trailing whitespace is trimmed. A line with a `REVERSED` span, or the
   `@` list's `> ` row, becomes `selected: <text>`. The overlay is compared with the snapshot
   printed last:
   - **Opened** (the title differs, or nothing was printed before): print a header, then every
     body and footer line. The header is `Permission Required:` for the approval overlay and
     `<title>:` for the others.
   - **Same title:** print only lines that are new or changed. A line whose only change is that
     it lost its `selected:` marker is not printed, so moving the selection announces the newly
     selected row.
   - **Closed:** print nothing.
3. **Mode change.** When `app.mode` differs from the last render, print `[manual mode on]`,
   `[accept edits on]`, `[plan mode on]`, `[auto mode on]` or `[dontAsk mode on]`. Mode changes
   are announced however they happen: Shift+Tab, `/plan` or the guardian pause.
4. **Bell.** Write BEL (`\x07`) when `app.running` goes from true to false, or when an overlay
   that needs an answer opens: approval, MCP server, guardian unavailable.
5. **Input echo.** `echo_diff(old, new) -> String` is a pure function. When the visible input is
   unchanged it writes nothing.
   - If `new` extends `old`, it writes the suffix, with `\n` written as `\r\n`.
   - If `old` minus a suffix that has no `\n` is `new`, it writes `\x08 \x08` per removed char.
   - For any other change (a mid-line edit, history recall, a deleted newline, clear after
     submit) it starts a fresh line: `\r\n> ` followed by the whole input.

**Interleaving.** When steps 1–4 produce output and the echoed input is non-empty, the renderer
first moves off the echo line: `\r\x1b[2K` when the input has no newline, otherwise `\r\n`. Then it
writes the output and re-echoes `> <input>`. When the session is idle and nothing is typed, it
shows a bare `> ` prompt after output. The screen reader's cursor therefore always ends on the
input line.

**What is dropped.** The status bar is not drawn (`/status` reports the same facts), and resize
events are ignored. Cursor movement inside the input is not announced. `docs/terminal.md` lists
both as known limitations.

**Startup screens in linear mode.** Both run with raw mode on and no `ratatui::try_init`.

- **Trust prompt.** `trust_prompt::ask` prints `workspace trust:` followed by the prompt's
  flattened `lines()`, then keeps reading keys through the same `handle_key` (grace period
  included). Selection changes are announced the same way as overlays.
- **Setup dialog.** Its view moves into `Setup::view() -> (banner, title, body, footer)`. The
  graphical path still draws it through `draw_overlay`. The linear path prints the banner once
  and the dialog through the same overlay snapshot printer, via `setup::drive_linear(params,
  writer, input)`.

To share code, the snapshot printer is `linear::OverlayEcho { last: Option<(String, Vec<String>)> }`
with `fn update(&mut self, title, lines) -> String`. The TUI, the trust prompt and the setup
dialog all use it.

### 4. Terminal setup (docs only)

`docs/terminal.md` holds Fontaine's version of Claude Code's terminal-config page:

- **New lines.** Alt+Enter, Ctrl+J and `\`+Enter work everywhere; Shift+Enter works where the
  terminal reports the modifier. The page gives per-terminal notes: VS Code/Cursor keybinding
  JSON that sends `\u001b\r` for Shift+Enter, the Alacritty/WezTerm/Zed equivalents, and the
  fact that gnome-terminal cannot report it.
- **tmux.** `set -s extended-keys on` and `set -as terminal-features 'xterm*:extkeys'`, plus the
  existing `set-clipboard` note for `/copy`.
- **Option as Meta on macOS.** Apple Terminal, iTerm2 and VS Code settings, so that Alt+Enter
  works.
- **Colours.** Themes, `/theme`, `NO_COLOR`, and the fact that scrollback keeps old colours.
- **Screen-reader mode.** How to turn it on, the labels, the bell, menus, and the known
  limitations.

## Implementation Steps

1. **Config (fontaine-core).** In `config.rs`, add `ThemeName`, `TuiSection`, `Tui`,
   `Settings.tui`, `Config.tui` (defaults: `DarkAnsi`, `None`), the merge, and the `check_scope`
   rejection for the project and project-local columns. In `user_config.rs`, add `set_theme`.
   Unit tests: parse, scope rejection in both project columns, CLI-over-user merge, unknown
   theme name rejected by serde, and `set_theme` keeping comments.
2. **Theme (fontaine-tui).** Add `theme.rs` with `Theme` and `preset`, and replace every
   `Color::*` call site listed in Current State with a token. Thread `&Theme` through
   `Transcript::{commit,live_lines,lines}`, `view`, `statusbar` and the overlays that colour
   anything. Add `App.theme`, set from config in `drive`. Give `trust_prompt::ask` and
   `SetupParams` a theme. Unit tests: every preset is distinct; daltonized success ≠ error hue
   family; `dark-ansi` equals today's colours (pin with a `TestBackend` render of a done and a
   failed tool line, asserting the cell fg).
3. **`/theme`.** Add a `commands.rs` entry and a `Ui::theme` handler in `lib.rs`, writing
   through `user_config::set_theme`. Unit/integration tests (`tests/tui.rs` through `drive`):
   the list marks the current theme; switching updates `app.theme` and writes `[tui] theme`;
   an unknown name is an error and changes nothing.
4. **Commit refactor.** Add `Transcript::commit_entries()`; rebuild `commit(width, theme)` on it.
   The existing transcript tests must stay green unchanged, apart from the theme argument.
5. **Linear renderer.** Add `accessibility.rs` (`Source`, `resolve`) and `linear.rs`
   (`Linear<W>`, `OverlayEcho`, `echo_diff`, `flatten(&Line) -> String`, transcript labeller).
   Make `view::overlay` `pub(crate)`. `run(setup, launch_dir, screen_reader: Option<Source>)`
   picks the renderer. Unit tests:
   - `resolve`: every precedence combination, including `env=0` over `setting=true`, and an
     invalid env value.
   - `echo_diff`: append, append with newline, trailing backspace, mid-edit, recall, clear.
   - `OverlayEcho`: open, a selection move announcing only the new row, a changed body line,
     reopening after a close.
   - The labeller: each `Entry` kind; a streaming reply committed per `\n`, labelled once.
   - Interleaving: output while text is typed clears the line and re-echoes it.
   - Mode announcement, once per change.
   - Bell on turn end and on approval open, not on other overlays.
   - Integration in `tests/tui.rs`: `drive` with a `Linear<Vec<u8>>` render on a cassette or
     test-model turn that calls a tool needing approval. Assert the bytes contain `you:`,
     `Permission Required:`, the `selected:` row, `tool: … — done`, `assistant:`, a BEL, and no
     box-drawing characters (U+2500–U+257F).
6. **Startup screens.** Linear `trust_prompt::ask` and `setup::drive_linear` built on
   `OverlayEcho`, with `Setup::view()` extracted. Tests: the trust prompt in linear mode prints
   the lines and answers on `y` after the grace period, through a key-source seam
   (`ask_with(keys, writer, …)`); `setup::drive_linear` on the existing setup test fixtures
   prints the banner and the dialog without box characters.
7. **CLI wiring (fontaine bin).** Add `--ax-screen-reader` to `cli.rs`. In `lib.rs`, resolve the
   mode with `env("FONTAINE_AX_SCREEN_READER")` and `config.tui.screen_reader`, print the
   confirmation line and any env warning, and pass the mode and the theme to the trust prompt,
   setup and `run`. A test in the bin's CLI tests: `-p` with `--ax-screen-reader` is accepted
   and its output has no confirmation line.
8. **Docs.** Add `docs/terminal.md`. Update `docs/tui.md` (the `/theme` command row, a pointer
   to terminal.md from the newline key row), `docs/configuration.md` (a `[tui]` key reference,
   a scope-table row `[tui]` = no / no / yes, and the flag/env table rows for
   `--ax-screen-reader` and `FONTAINE_AX_SCREEN_READER`), and `docs/index.md` (a terminal page
   entry and `/theme` in the TUI line). Record the deviations in the master plan's
   `## Decisions`.

## Files to Modify

- `rust/fontaine-core/src/config.rs`, `rust/fontaine-core/src/user_config.rs`
- `rust/fontaine-tui/src/theme.rs` (new), `linear.rs` (new), `accessibility.rs` (new), `lib.rs`
- `rust/fontaine-tui/src/transcript.rs`, `view.rs`, `statusbar.rs`, `app.rs`, `commands.rs`
- `rust/fontaine-tui/src/approval.rs`, `server_prompt.rs`, `setup.rs`, `trust_prompt.rs`,
  `agents_panel.rs`, `denials.rs`, `model_picker.rs`, `provider_dialog.rs`
- `rust/fontaine-tui/tests/tui.rs`, `tests/setup.rs`
- `rust/fontaine/src/cli.rs`, `rust/fontaine/src/lib.rs`
- `docs/terminal.md` (new), `docs/tui.md`, `docs/configuration.md`, `docs/index.md`
- `tasks/fontaine_harness_plan.md` (`## Decisions`)

## Trade-offs

- **The linear renderer reuses `view::overlay`, not a hand-written plain view per dialog.**
  Twelve overlays already produce their content as `Line`s, and flattening them keeps a single
  source for each dialog's text, so a new dialog works in screen-reader mode without extra code.
  The cost is that selection is detected from the `REVERSED` modifier. That detection lives in
  one function with a test per overlay type.
- **Keys stay the same; menus are not turned into numbered lists.** Claude Code turns menus into
  numbered "Select with numbers" lists. Fontaine's dialogs already answer to single letters
  (`y`/`n`/`a`/`s`, `r`/`p`/`m`), with arrows to move, and the flattened footer reads those keys
  out. Numbered lists would need a second input path in every dialog, including the approval
  grace logic, which is a security surface.
- **Partial-match echo, not a full line editor.** `echo_diff` covers typing and backspacing at
  the end, which is what Claude Code optimises. Anything else reprints the input on a fresh line,
  which is correct, if noisier, without tracking a cursor.
- **Theme tokens are colours, not whole styles.** Call sites keep their own `BOLD`/`DIM`, so
  themes cannot accidentally drop emphasis. A theme cannot restyle emphasis either; custom themes
  would add that.
- **Docs, not `/terminal-setup`.** Writing other programs' config files (VS Code `keybindings.json`,
  Zed's `keymap.json`, Apple Terminal plists) from inside an agent harness is a write outside the
  workspace with no undo, and the issue asks for documented setup.

## Decisions

- 2026-10-24: The default theme is `dark-ansi` (today's 16-colour palette), not Claude Code's
  truecolor `dark`. ratatui does not downsample truecolor on 256-colour terminals, and the ANSI
  palette follows the terminal's own scheme.
- 2026-10-24: There are no custom theme files (`themes/*.json`) and no `auto` light/dark
  detection. Detection needs an OSC 11 round trip, which times out on terminals that do not
  answer (the same problem the inline viewport avoids with cursor queries); custom themes can
  follow as their own issue.
- 2026-10-24: `/theme` is "no argument lists, a name switches and saves", like `/output-style`,
  not Claude Code's picker with a preview.
- 2026-10-24: `[tui]` is a scope error in project and project-local files (like `accounts`). In
  Claude Code a project's `axScreenReader` is ignored, with a startup line. Fontaine's settings
  are `deny_unknown_fields` with a per-column scope table, and terminal preferences are personal.
- 2026-10-24: In screen-reader mode, menus keep their letter keys and arrows, and the selected row
  is announced. They are not numbered lists. Claude Code's 3 s startup quiet wait,
  `CLAUDE_AX_PREPARK_MS`, the bell for tools running longer than 5 s, OSC 133 turn markers and
  word-deletion announcements are not implemented.
- 2026-10-24: Assistant text is labelled `assistant:`, not Claude Code's `claude:`, because
  Fontaine runs any model. Notices are labelled `notice:`.
- 2026-10-24: Mode changes are announced however they happen, including `/plan`, where Claude
  Code announces only Shift+Tab.
- 2026-10-24: Fontaine has no `/terminal-setup` command and does not enable the kitty keyboard
  protocol. Terminal setup is documented in `docs/terminal.md`, as the issue specifies, because
  writing other programs' config files is an unrecoverable write outside the workspace, and the
  keyboard protocol changes how several key chords are reported.

## Documentation

- **New `docs/terminal.md`**: newline keys per terminal, tmux, Option as Meta, colours and
  themes (`/theme`, `[tui] theme`, `NO_COLOR`), screen-reader mode (turning it on and off,
  labels, the bell, dialogs, known limitations).
- **`docs/tui.md`**: a `/theme` row in Commands, and links to terminal.md from the Keys table
  and the intro.
- **`docs/configuration.md`**: a `[tui]` key reference, a scope-table row, and flag/env rows for
  `--ax-screen-reader` and `FONTAINE_AX_SCREEN_READER`.
- **`docs/index.md`**: an entry for the terminal page.

## Testing Strategy

All of this is reachable with constructed inputs, so it is unit- and integration-tested, with no
live APIs:

- **Config:** parse/scope/merge tests in `config.rs`, and the `set_theme` round-trip in
  `user_config.rs`.
- **Themes:** preset table tests, plus a `TestBackend` render pinning that `dark-ansi` keeps
  today's cell colours (a regression guard for the token refactor).
- **`/theme`:** `drive`-level tests in `tests/tui.rs` with a temp user config file.
- **Screen-reader mode:**
  - Pure-function tests for `resolve`, `echo_diff`, `flatten`, `OverlayEcho` and the labeller.
  - A `Linear<Vec<u8>>` renderer test over `App` states: interleaving, mode announcement and the
    bell.
  - One `drive` integration test that asserts the byte stream of a whole approved-tool turn,
    including the absence of box-drawing characters.
- **Startup screens:** a linear trust prompt through a key/writer seam, and
  `setup::drive_linear` on the existing setup fixtures.
- **Bin:** a CLI parse test for `--ax-screen-reader`, and a headless run that does not print the
  confirmation line.

## Manual Verification

Unit tests cannot hear what a real screen reader announces, and the mode exists for that use.

1. `cargo run -p fontaine -- --ax-screen-reader` in a terminal under Orca (Linux) or NVDA
   (Windows Terminal). Expected: the first line is `[Screen Reader Mode: on via flag]`. Typing
   echoes each character. A turn reads as `you:` … `assistant:` …, and an approval reads
   `Permission Required:` with the key hints, then `selected: …` as arrows move. A bell sounds
   when the reply ends.
2. `cargo run -p fontaine`, then `/theme light-daltonized` in a light-background terminal.
   Expected: new tool lines show blue for done and orange for failed, and
   `~/.config/fontaine/settings.toml` gains `[tui] theme = "light-daltonized"`.

## Open Questions

None. The design resolves the open points above as Decisions.
