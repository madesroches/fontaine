# The terminal UI

`fontaine` without `-p` starts the terminal UI. It needs a terminal: with piped standard input or
output it exits with code 2 and points at `fontaine -p`. Configuration, accounts and permission
rules work as in headless runs; the mode is the one from `[permissions] default_mode` or
`--permission-mode` (`auto` is refused).

The screen has a transcript, an input box and a status bar. The transcript shows your messages
(`>`), the streaming answer, and one line per tool call with its status: pending, done, failed,
denied with the reason, or interrupted.

## Status bar

The bar shows the permission mode, the model and a key hint. The mode labels are `manual`,
`⏵⏵ accept edits on`, `⏸ plan mode on` and `dontAsk`; `⏵⏵ auto mode on` is reserved for `auto`.

## Keys

| Key | Action |
|---|---|
| Enter | Submit. Alt+Enter, Shift+Enter or Ctrl+J insert a newline; a trailing `\` before Enter also continues the line. |
| Shift+Tab | Cycle the mode: manual, accept edits, plan, and back to manual (`auto` joins the cycle when it ships). It takes effect on the next tool call, even mid-turn. |
| Esc, Ctrl-C while a turn runs | Interrupt the turn. The conversation is kept and an open prompt is answered with deny. |
| Ctrl-C, or Ctrl-D on an empty input, when idle | Quit. |
| PageUp, PageDown | Scroll the transcript. |
| Arrows, Home, End, Backspace, Delete, Ctrl-A/E/U/K/W/B/F | Edit the input. |

## Approval prompts

When a call needs approval, a prompt lists the call and why it asked:

- `y` allow once.
- `s` allow session, when the prompt offers it.
- `a` always allow, when the prompt offers it. The prompt shows the exact rule that will be added
  to `[permissions] allow` in your user settings file.
- `n` deny. Enter on the deny row opens an optional reason, which is sent to the model with the
  denial.

Up, Down and Tab move the highlight, Enter confirms. Prompts forced by the floor (an `ask` rule, a
critical path, a protected path) and shell calls that are not one literal simple command offer
only allow once and deny. See [Permissions](permissions.md) for what each answer does.

## Commands

| Command | Action |
|---|---|
| `/add-dir <path>` | Adds a workspace root. The path is validated and canonicalized like `--add-dir`; file tools may reach it from the next turn, with the conversation kept. Only between turns. |
| `/exit`, `/quit` | Leave. |
