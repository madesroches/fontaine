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
  throughout. Every overlay with a selection marks it with `REVERSED`, except the `@` list, which
  uses a `> ` prefix plus `BOLD` (`view.rs:216`).
- **Rendering.** `lib.rs::drive` (`lib.rs:1487`) runs the session loop with a
  `render: FnMut(&mut App)` callback.
  - `lib.rs::run` (`lib.rs:1612`) enables raw mode and bracketed paste, then builds a
    `screen::Screen`, an inline viewport. `Screen::render` commits settled transcript lines into
    scrollback and redraws the live area through `view::draw`.
  - `view::overlay(app, width, height)` (`view.rs:159`) is the single place that decides which
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
  - `user_config::set_string` (`user_config.rs:67`) writes one key with `toml_edit`, keeping
    comments. `set_model` and `set_guardian_model` wrap it.
- **CLI.** `fontaine/src/cli.rs` (clap derive). The bin calls `trust_prompt::ask` at
  `fontaine/src/lib.rs:181`, `setup::run` at `:655` and `fontaine_tui::run` at `:685`.
- **Slash commands.** `commands.rs::BUILTINS` is one table that the parser and the `/` menu both
  read. `/output-style` (handler `fn output_style`, `lib.rs:669`; the dispatch arm is `lib.rs:487`) is the precedent for "no argument lists, an argument
  switches".
- **Keys.** Alt+Enter, Shift+Enter (when the terminal reports it), Ctrl+J, or a trailing `\`
  insert a newline (`app.rs:540-590,633`). Fontaine does not enable the kitty keyboard protocol.
- **Colour suppression.** crossterm 0.29 honours `NO_COLOR` (`style/types/colored.rs:73`): colour is dropped, and emphasis (`BOLD`/`REVERSED`) may be dropped with it, since ratatui sends a bare SGR reset after modifiers.
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

#[derive(Clone, Debug, PartialEq)]   // as Config's other fields (config.rs:1112)
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
    pub error: Style,       // failed/denied tool, error entries, guardian-unavailable heading
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
    colours anything: approval, server prompt, agents panel, denials, `UnavailablePrompt`, model picker and
    provider dialog.
  - `statusbar::render` reads `app.theme`.
  - `trust_prompt::ask` takes a `Theme`; setup reads `params.config.tui.theme` (`SetupParams`
    already carries the config), with no new theme field.
  - Overlays that only use `DIM`/`BOLD` keep their signatures.
- **Scrollback.** Lines already written to scrollback keep the colours they were written in.
  Changing the theme affects new lines and the live area.
- **`/theme [<name>]`.** This is `Command::Theme(String)` in `BUILTINS` (hint `[<name>]`,
  description "choose the colour theme").
  - With no argument, a notice lists the six names, marks the current one `(current)` and names
    the file the choice is saved to. With no user settings file (`Ui.user_config` is `None`), it
    says the choice is not saved.
  - With a name, it sets `app.theme`, saves `[tui] theme` through `user_config::set_theme`, and
    confirms. A save failure is reported, and the theme still applies for the session. With no
    user settings file it applies the theme and reports "for this session only: …", as
    `/model` does (`AgentRuntime::persist`, `Ui::finish_switch`).
  - An unknown name is an error notice that lists the valid names.

### 3. Screen-reader mode (fontaine-tui `linear.rs`, `accessibility.rs`)

**Turning it on.** `accessibility::resolve(flag: bool, env: Option<&str>, setting: Option<bool>)
-> Option<Source>`, where `Source` is `Flag | Env | Settings`. Precedence follows Claude Code:

1. `--ax-screen-reader` turns it on.
2. `FONTAINE_AX_SCREEN_READER=1` turns it on, and `=0` keeps it off even when the setting is
   `true`. Any other value is ignored, and a startup warning names it.
3. `[tui] screen_reader = true` turns it on.

The bin resolves it once, after config load and before the trust prompt. When the mode is on it
prints `[Screen Reader Mode: on via flag|env|settings]` as plain text before any raw mode, then
passes the `Option<Source>` to `trust_prompt::ask`, `setup::run` (a new `screen_reader` field on
`SetupParams`) and `fontaine_tui::run`. The env value is read with
`(host.env)("FONTAINE_AX_SCREEN_READER")`; an invalid value warns with the existing
`eprintln!("fontaine: warning: …")` form, only in interactive runs. Headless
`-p` ignores the mode.

**The linear renderer.** `Linear<W: Write>` implements the render callback `drive` already takes,
so `run` picks `Linear::new(stdout)` or `Screen`. Raw mode, bracketed paste and the key reader are
unchanged, and the cursor stays visible. Each `render(&mut App)` builds its output in this order:

1. **Transcript.** A new `Transcript::commit_entries(cut)` takes the streaming cut rule (wrapped
   rows at a width, or up to the last `\n`), advances `committed_chars` by it, and returns the
   newly settled pieces (`entry`, chars already written, whether this continues an entry). Both
   the styled `commit(width, theme)` and the linear labeller use it, so settling rules live in
   one place.
   - In linear mode a streaming reply is cut at its last `\n`, not by width. The terminal
     wraps lines.
   - The labels are:

     | Entry | Label |
     |---|---|
     | User | `you:` |
     | Assistant | `assistant:` (on the first piece only) |
     | Tool done or interrupted | `tool: <tool> <summary> — done` / `— interrupted` |
     | Tool failed or denied | `tool error: <tool> <summary> — failed: …` / `— denied: …` |
     | Error | `error:` |
     | Notice | `notice:` |
     | Shell | `shell: <command>`, followed by its output lines |

     A pending call holds back the commit, so `Permission Required:` is heard before the
     `tool:` line.

2. **Overlay.** `view::Overlay` and `view::overlay` become `pub(crate)`. It is called with width
   1000 and height `u16::MAX`, so nothing is clipped, and every `Line` is flattened to text: span
   contents are joined and trailing whitespace is trimmed. `/model` and `/guardian-model` still
   show a 14-row window that follows the selection, and `OverlayEcho`'s changed-line printing
   announces rows as they scroll in. A line with a `REVERSED` span, or the `@` list's `> ` row,
   becomes `selected: <text>`; `flatten` strips the leading `> ` marker when it adds `selected:`. The overlay is compared with the snapshot
   printed last:
   - **Opened** (the title differs, or nothing was printed before): print a header, then every
     body and footer line. The header is `Permission Required:` for the approval overlay and
     `<title>:` for the others.
   - **Same title:** print only lines that are new or changed. A line is new or changed when its
     text, after the `selected:` normalisation, is not in the set of lines of the last printed
     snapshot; position does not count. A line whose only change is that it lost its `selected:`
     marker is not printed, so moving the selection announces the newly selected row, and a
     scroll step of a windowed list prints only the row that came in.
   - **Closed:** print nothing.
3. **Mode change.** When `app.mode` differs from the last render, print `[manual mode on]`,
   `[accept edits on]`, `[plan mode on]`, `[auto mode on]` or `[dontAsk mode on]`. The first
   render records the mode without announcing it. Mode changes are announced however they
   happen: Shift+Tab, `/plan` or choosing manual at the guardian-unavailable prompt. The pause
   itself is heard through its notice.
4. **Bell.** Write BEL (`\x07`) when `app.running` goes from true to false, or when an overlay
   that needs an answer opens: approval, MCP server, guardian unavailable.
5. **Input echo.** `echo_diff(old, new) -> String` is a pure function. When the visible input is
   unchanged it writes nothing.
   - If `new` extends `old`, it writes the suffix, with `\n` written as `\r\n`.
   - If `old` minus a suffix that has no `\n` is `new`, it writes `\x08 \x08` per removed char.
   - For any other change (a mid-line edit, history recall, a deleted newline, clear after
     submit) it starts a fresh line: `\r\n> ` followed by the whole input.

**Interleaving.** When steps 1–4 produce output and the echoed input is non-empty, the renderer
first moves off the echo line with `\r\n`. Then it
writes the output and re-echoes `> <input>`. When the session is idle and nothing is typed, it
shows a bare `> ` prompt after output. The screen reader's cursor therefore always ends on the
input line.

**What is dropped.** The status bar is not drawn. `/status` reports the mode and model but not the status bar's
working indicator or agent count, and the queued-messages lines (`view.rs:104-120`) are not
announced. Resize events are ignored, and cursor movement inside the input is not announced.
`docs/terminal.md` lists all of these as known limitations.

**Startup screens in linear mode.** Both run with raw mode on and no `ratatui::try_init`; the
linear `setup::run` enables raw mode and restores it itself.

- **Trust prompt.** `trust_prompt::ask` prints `workspace trust:` followed by the prompt's
  flattened `lines()`, then keeps reading keys through `handle_key_at(key, clock())` (grace period
  included). `ask` calls `enable_raw_mode()` and restores it with `disable_raw_mode()` on every
  exit path, panic hook included, as `setup::run` does (`setup.rs:87-93`). Selection changes are announced the same way as overlays.
- **Setup dialog.** Its view moves into `Setup::view() -> (banner, title, body, footer)`. The
  graphical path still draws it through `draw_overlay`. The linear path, `setup::drive_linear(params,
  writer, input)`, prints the dialog through the overlay snapshot printer. The banner lines
  (built from `self.error` and `self.status`, which change inside the loop) go through an
  `OverlayEcho` too, under the fixed title "setup", so changed lines print. Setup's `Io` and `redraw`, which hold a `Terminal<B>` today, are generalised
  over the renderer (terminal draw or `OverlayEcho` print), so there is one setup event loop.

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
- **Colours.** Themes, `/theme`, `NO_COLOR` (colour is dropped, and emphasis may be dropped with it), and the fact that scrollback keeps old colours.
- **Screen-reader mode.** How to turn it on, the labels, the bell, menus, and the known
  limitations (no working indicator, agent count or queued-messages lines; no resize handling; no
  cursor-movement announcements).

## Implementation Steps

1. **Config (fontaine-core).** In `config.rs`, add `ThemeName`, `TuiSection`, `Tui`,
   `Settings.tui`, `Config.tui` (defaults: `DarkAnsi`, `None`), the merge, and the `check_scope`
   rejection for the project and project-local columns. In `user_config.rs`, add `set_theme`.
   Unit tests: parse, CLI-over-user merge, unknown
   theme name rejected by serde, and `set_theme` keeping comments. Add the `[tui]` cases to the
   table-driven scope tests `NO_FOR_PROJECT` and `NO_FOR_LOCAL` (`fontaine-core/tests/config.rs:543`,
   `:607`) and to the CLI-accepts test (`:784`).
2. **Theme (fontaine-tui).** Add `theme.rs` with `Theme` and `preset`, and replace every
   `Color::*` call site listed in Current State with a token. Thread `&Theme` through
   `Transcript::{commit,live_lines,lines}`, `view`, `statusbar` and the overlays that colour
   anything; `Screen::render` (`screen.rs:156` calls `transcript.commit(width)`) passes `&app.theme`. Add `App.theme`, set from config in `drive`. Give `trust_prompt::ask` a theme
   and update the bin's call (`fontaine/src/lib.rs:181`) to pass `Theme::preset(config.tui.theme)`;
   setup reads `params.config.tui.theme`. Unit tests: every preset is distinct; daltonized
   success and error differ in hue family (success has b > r, error has r > b); `dark-ansi`
   equals today's colours (assert span fg on `Transcript::lines(width, theme)` for a done and a
   failed tool line).
3. **`/theme`.** Add a `commands.rs` entry (the `theme` entry goes after `status` in `BUILTINS`, which must stay sorted) and a `Ui::theme` handler in `lib.rs`, writing
   through `user_config::set_theme`. Unit/integration tests (`tests/tui.rs` through `drive`):
   the list marks the current theme; switching updates `app.theme` and writes `[tui] theme`;
   an unknown name is an error and changes nothing; with `user_config: None` the theme applies and
   the notice says "for this session only". The tests use a temp user config file.
4. **Commit refactor.** Add `Transcript::commit_entries(cut)` (the streaming cut rule: width rows or last `\n`); rebuild `commit(width, theme)` on it.
   The existing transcript tests must stay green unchanged, apart from the theme argument.
5. **Linear renderer.** Add `accessibility.rs` (`Source`, `resolve`) and `linear.rs`
   (`Linear<W>`, `OverlayEcho`, `echo_diff`, `flatten(&Line) -> String`, transcript labeller).
   Make `view::Overlay` and `view::overlay` `pub(crate)`. `run(setup, launch_dir, screen_reader: Option<Source>)`
   picks the renderer. Unit tests:
   - `resolve`: every precedence combination, including `env=0` over `setting=true`, and an
     invalid env value.
   - `echo_diff`: append, append with newline, trailing backspace, mid-edit, recall, clear.
   - `OverlayEcho`: open, a selection move announcing only the new row, a changed body line,
     reopening after a close, and a scroll step of a windowed list printing only the row that
     came in plus the newly selected row.
   - `flatten`: a pure-function test, including selection detection for each overlay with a
     selection (the `@` list's `> ` row too).
   - The labeller: each `Entry` kind; a streaming reply committed per `\n`, labelled once.
   - Interleaving: output while text is typed moves to a new line and re-echoes the input.
   - Mode announcement, once per change.
   - Bell on turn end and on approval open, not on other overlays.
   - `TerminalGuard`'s screen becomes optional and is cleared only on the Screen path; a failed
     Linear write sets `on_error` like a failed `screen.render`.
   - Integration in `tests/tui.rs`: a `World` mock-model turn (`tests/common/mod.rs:336-373`)
     that calls a tool needing approval, through a new `World::run` variant that takes the
     renderer and shares an `Rc<RefCell<Vec<u8>>>` buffer with a `Linear` writer. Assert the bytes contain `you:`,
     `Permission Required:`, the `selected:` row, `tool: … — done`, `assistant:`, a BEL, and no
     box-drawing characters (U+2500–U+257F).
6. **Startup screens.** Linear `trust_prompt::ask` and `setup::drive_linear` built on
   `OverlayEcho`, with `Setup::view()` extracted and setup's `Io`/`redraw` generalised over the
   renderer. Tests: the trust prompt in linear mode prints
   the lines and answers on `y` after the grace period, through a key-source seam
   (`ask_with(keys, writer, clock, …)`, using the injectable clock of `handle_key_at`, so the
   test does not sleep); `setup::drive_linear` on the existing setup test fixtures
   prints the banner and the dialog without box characters; a failed retry prints the new error,
   and a listing prints the "listing the models of …" line.
7. **CLI wiring (fontaine bin).** Add `--ax-screen-reader` to `cli.rs`. In `lib.rs`, `run_interactive` (`fontaine/src/lib.rs:627`) gains a
   `screen_reader: Option<Source>` parameter; resolve the
   mode with `(host.env)("FONTAINE_AX_SCREEN_READER")` and `config.tui.screen_reader`, print the
   confirmation line and any env warning, and pass the mode to the trust prompt,
   setup and `run`. A test in `rust/fontaine/tests/exit_code.rs`: `-p` with `--ax-screen-reader` is accepted
   and its output has no confirmation line.
8. **Docs.** Add `docs/terminal.md`. Update `docs/tui.md` (the `/theme` command row, a pointer
   to terminal.md from the newline key row, a link from the intro, and a link from the
   `/copy`/tmux `set-clipboard` note), `docs/configuration.md` (a `[tui]` key reference,
   a scope-table row `[tui]` = no / no / yes, and the flag/env table rows for
   `--ax-screen-reader` and `FONTAINE_AX_SCREEN_READER`), and `docs/index.md` (a terminal page
   entry and `/theme` in the TUI line). Update `tasks/design/config.md`
   (copy the scope-table row), `tasks/design/documentation.md` (the docs page list) and
   `research/research-claude-code-parity.md` (the feature row, ~line 384). Copy this plan's
   Decisions entries (default theme, no custom themes/auto, `/theme` list-and-switch, `[tui]`
   scope, screen-reader menus, `assistant:`/`notice:` labels, mode announcements, no
   `/terminal-setup`) into the master plan's `## Decisions`, each tagged `(#50)`.

## Files to Modify

- `rust/fontaine-core/src/config.rs`, `rust/fontaine-core/src/user_config.rs`
- `rust/fontaine-tui/src/theme.rs` (new), `linear.rs` (new), `accessibility.rs` (new), `lib.rs`
- `rust/fontaine-tui/src/transcript.rs`, `view.rs`, `statusbar.rs`, `app.rs`, `commands.rs`
- `rust/fontaine-tui/src/approval.rs`, `server_prompt.rs`, `setup.rs`, `trust_prompt.rs`,
  `agents_panel.rs`, `denials.rs`, `model_picker.rs`, `provider_dialog.rs`, `screen.rs`
- `rust/fontaine-tui/tests/tui.rs`, `tests/setup.rs`
- `rust/fontaine/src/cli.rs`, `rust/fontaine/src/lib.rs`
- `docs/terminal.md` (new), `docs/tui.md`, `docs/configuration.md`, `docs/index.md`
- `tasks/fontaine_harness_plan.md` (`## Decisions`), `tasks/design/config.md`,
  `tasks/design/documentation.md`, `research/research-claude-code-parity.md`

## Trade-offs

- **The linear renderer reuses `view::overlay`, not a hand-written plain view per dialog.**
  Twelve overlays already produce their content as `Line`s, and flattening them keeps a single
  source for each dialog's text, so a new dialog works in screen-reader mode without extra code.
  The cost is that selection is detected from the `REVERSED` modifier. That detection lives in
  one function with a test per overlay type.
- **Partial-match echo, not a full line editor.** `echo_diff` covers typing and backspacing at
  the end, which is what Claude Code optimises. Anything else reprints the input on a fresh line,
  which is correct, if noisier, without tracking a cursor.
- **Theme tokens are colours, not whole styles.** Call sites keep their own `BOLD`/`DIM`, so
  themes cannot accidentally drop emphasis. A theme cannot restyle emphasis either; custom themes
  would add that.

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
  is announced. They are not numbered lists: those would need a second input path in every dialog, including the approval grace logic. Claude Code's 3 s startup quiet wait,
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

## Manual Verification

Unit tests cannot hear what a real screen reader announces, and the mode exists for that use.

1. `cargo run -p fontaine -- --ax-screen-reader` in a terminal under Orca (Linux) or NVDA
   (Windows Terminal). Expected: the first line is `[Screen Reader Mode: on via flag]`. Typing
   echoes each character. A turn reads as `you:` … `assistant:` …, and an approval reads
   `Permission Required:` with the key hints, then `selected: …` as arrows move. A bell sounds
   when the reply ends.
2. `cargo run -p fontaine`, then `/theme light-daltonized` in a light-background terminal.
   Expected: new tool lines show blue for done and orange for failed, and
   `~/.config/fontaine/config.toml` gains `[tui] theme = "light-daltonized"`.

## Open Questions

None. The design resolves the open points above as Decisions.
