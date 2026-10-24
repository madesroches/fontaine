# Terminal setup, colours and screen readers

This page covers the terminal side of the [terminal UI](tui.md): getting a newline key to work,
tmux and macOS settings, colours and themes, the clipboard, and screen-reader mode. Fontaine has no
`/terminal-setup` command: it never edits another program's configuration file, so the settings
below are yours to make.

## New lines in the message box

Enter sends. These insert a newline instead:

| Key | Works in |
|---|---|
| Alt+Enter | Every terminal where Alt (Option on macOS) is sent as Meta; see [Option as Meta](#option-as-meta-on-macos). |
| Ctrl+J | Every terminal. |
| `\` then Enter | Every terminal. The backslash is removed and the line continues. |
| Shift+Enter | Terminals that report the Shift modifier on Enter. |

Fontaine does not turn on the kitty keyboard protocol, so Shift+Enter only works where the terminal
already reports it. In a terminal that cannot, bind Shift+Enter to send the Alt+Enter sequence,
`ESC` followed by a carriage return. GNOME Terminal and other VTE-based terminals send the same
bytes for Shift+Enter as for Enter and have no setting to change that; use Alt+Enter or Ctrl+J
there.

**VS Code and Cursor** (integrated terminal). Add to `keybindings.json`:

```json
{
  "key": "shift+enter",
  "command": "workbench.action.terminal.sendSequence",
  "args": { "text": "\u001b\r" },
  "when": "terminalFocus"
}
```

**Alacritty** (`alacritty.toml`):

```text
[[keyboard.bindings]]
key = "Return"
mods = "Shift"
chars = "\u001b\r"
```

**WezTerm** (`wezterm.lua`):

```lua
config.keys = {
  { key = 'Enter', mods = 'SHIFT', action = wezterm.action.SendString '\x1b\r' },
}
```

**Zed** (`keymap.json`):

```json
[
  {
    "context": "Terminal",
    "bindings": { "shift-enter": ["terminal::SendText", "\u001b\r"] }
  }
]
```

## tmux

tmux swallows modifiers on keys such as Shift+Enter unless extended keys are on. Add to
`~/.tmux.conf`:

```text
set -s extended-keys on
set -as terminal-features 'xterm*:extkeys'
```

and restart the tmux server. Alt+Enter, Ctrl+J and `\` then Enter need no setting.

`/copy` sends the reply with an OSC 52 escape. tmux forwards it to the outer terminal under its
default `set-clipboard external`; if you changed that to `off`, set it back:

```text
set -g set-clipboard external
```

The outer terminal must itself accept OSC 52 clipboard writes. See
[the commands table](tui.md#commands) for `/copy`.

## Option as Meta on macOS

Alt+Enter arrives as `ESC` plus Enter only when the terminal sends Option as Meta:

- **Terminal.app**: Settings, Profiles, Keyboard, tick "Use Option as Meta key".
- **iTerm2**: Settings, Profiles, Keys, General, set "Left Option key" (and "Right Option key" if
  you use it) to "Esc+".
- **VS Code and Cursor**: set `"terminal.integrated.macOptionIsMeta": true` in the settings.

## Colours

Every colour in the interface comes from a theme. Six are built in:

| Theme | For |
|---|---|
| `dark-ansi` (default) | A dark background, using the terminal's own 16 colours. |
| `light-ansi` | A light background, using the terminal's own 16 colours. |
| `dark` | A dark background, with fixed RGB colours. |
| `light` | A light background, with fixed RGB colours. |
| `dark-daltonized` | A dark background, with blue and orange in place of green and red. |
| `light-daltonized` | A light background, with blue and orange in place of green and red. |

The `-ansi` themes follow whatever palette the terminal is set to and need no truecolor support.
The others use fixed RGB values, which a terminal without truecolor approximates badly or not at
all. The daltonized themes show a finished tool call in blue and a failed or denied one in orange,
which readers with red-green colour blindness can tell apart.

Choose one with `/theme <name>`, which applies at once and saves `[tui] theme` to your user
settings file, or by writing the key yourself:

```toml
[tui]
theme = "light-daltonized"
```

`/theme` with no name lists the themes and marks the current one. Lines already written to the
terminal's scrollback keep the colours they were written in; a new theme applies to new lines and
to the live area. `[tui]` is read from your user file and `--settings` only. There are no custom
theme files and no automatic light or dark detection.

Fontaine also uses bold, dim and reversed text, for example to mark the selected row. When the
`NO_COLOR` environment variable is set, colour is dropped, and the terminal library may drop that
emphasis with it, so a selected row can lose its highlight. Most selected rows also carry a `>` marker, which stays.

## Screen-reader mode

Screen-reader mode replaces the live area with plain, labelled lines appended in order, which a
screen reader can follow. Turn it on with any of:

1. `--ax-screen-reader`.
2. `FONTAINE_AX_SCREEN_READER=1` in the environment. `FONTAINE_AX_SCREEN_READER=0` keeps it off
   even when the setting is `true`. Any other value is ignored, with a warning at startup.
3. `screen_reader = true` under `[tui]` in your user settings file:

```toml
[tui]
screen_reader = true
```

The first line printed is `[Screen Reader Mode: on via flag]` (or `env`, or `settings`). A headless
`-p` run ignores the mode. Keys do not change: Enter sends, and the dialogs keep their letter keys
and arrows.

What is printed:

| Line | Meaning |
|---|---|
| `you: ...` | A message you sent. |
| `assistant: ...` | The model's reply, printed a line at a time as it streams. The label appears once. |
| `tool: <tool> <summary> — done` | A tool call that finished; `— interrupted` for one that did not. |
| `tool error: <tool> <summary> — failed: ...` | A tool call that failed, or `— denied: ...` for one that was refused. |
| `error: ...` and `notice: ...` | Errors and notices. |
| `shell: <command>` | A `!` command you ran, followed by its output. |
| `Permission Required:` | An approval prompt opened. Its question, the call, the choices and the rule the lasting answers would add follow. |
| `<name>:` | Another dialog opened, for example `model:`, `providers:`, `denied:` or `MCP server:`. |
| `selected: ...` | The highlighted row of the open dialog. |
| `[plan mode on]` | The mode changed: `[manual mode on]`, `[accept edits on]`, `[plan mode on]`, `[auto mode on]` or `[dontAsk mode on]`, however it changed. |

A dialog is printed in full when it opens and afterwards only the lines that changed: moving the
selection prints the newly selected row as `selected: ...`, and scrolling a long list prints the
row that came into view. While you type a reason for denying a call, each character you type is
echoed as you type it and the prompt is not printed again.

What you type is echoed after a `> ` prompt, one character at a time. Editing anywhere except the
end of the line prints the whole input again on a fresh line. When output arrives while you are
typing, it is printed on the lines above and your input is echoed again below it, so the cursor
always ends on the input line.

A bell (BEL) sounds when a reply finishes and when a prompt that needs an answer opens: an
approval, an MCP server question or the unavailable-guardian prompt.

Known limitations:

- There is no status bar, so no working indicator and no running-agent count. `/status` reports the
  mode and the model.
- Messages queued while a turn runs are not announced; each is heard as `you: ...` when it is sent.
- A running tool call is not announced until it ends. The absent bell tells you the turn is still
  going.
- Terminal resizes are ignored, and cursor movement inside the input is not announced.
- Dialogs are not numbered lists; use their letter keys and arrows.
