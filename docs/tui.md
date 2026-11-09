# The terminal UI

`fontaine` without `-p` starts the terminal UI. It needs a terminal: with piped standard input or
output it exits with code 2 and points at `fontaine -p`. Configuration, accounts and permission
rules work as in headless runs; the mode is the one from `[permissions] default_mode` or
`--permission-mode`. With neither set, the terminal UI starts in `auto`, the mode where the
[guardian](guardian.md) judges each call that nothing pre-approved. Newline keys, tmux and macOS
settings, colour themes and screen-reader mode are on [Terminal setup](terminal.md).

The UI draws inline, not on the alternate screen, but it takes the whole terminal when it starts:
the shell's earlier output scrolls into the terminal's history. The transcript reads top-down, with
the input box and status bar on the bottom rows, and this holds across resizes: a resize erases the
scrollback and prints the transcript again at the new size. A live area down to the bottom row holds
the input box, the status bar, any open prompt, the working spinner row while a turn runs and the
part of the transcript that still fits above the input. A short transcript, typically just the
startup warnings, sits on the rows directly above the input box with empty space above it. Once the
transcript no longer fits, the oldest entries move into the terminal's normal scrollback, and
leaving the UI writes whatever was still held back there too, so the conversation stays in the
terminal. Scroll back with the terminal or with tmux copy mode, and select text with
the mouse as in any shell program.
Showing a [subagent's transcript](#the-agent-list-and-the-agent-view) erases the terminal's
scrollback and writes that transcript in its place, and going back to the main conversation does
the same, so whatever the terminal held from before the session started, and the
main chat's rows from before a `/clear`, are gone after the first switch. Ctrl-L and `/clear`
erase the scrollback the same way.

The input box title says what Enter does: it sends a message, or, when the input starts with `!`,
runs it as a shell command. The transcript shows your messages (`>`), the streaming answer, and a
card per tool call with its status: pending, done, failed, denied with the reason, or interrupted. A
call the guardian refused shows its rule label and the guardian's own explanation on lines of their
own, for example `[Download+Execute] pipes a download into a shell`. A line is written to scrollback once it has
settled and no longer fits above the input; a call still pending when the screen is full is written
as pending, and its card (outcome, output, diff) follows after it once it settles.

A call that has finished shows more than its one line. A command that is multi-line or wider than
the screen is shown under the `•` row, folded to three rows with `… (+N rows)` for the rest. The
tool's output follows, dim behind a `│` gutter: the last ten rows, with `… N earlier rows` when
more were cut. URLs in tool output and in `!` shell output are drawn in the link colour,
underlined. An `edit`, `write` or `notebook_edit` call shows its diff instead (a `path  +added -removed` header,
then up to 20 lines in the same form as the approval prompt, with `… N more changed lines`), for
every allowed call, not only the ones you were asked about. A failed call keeps its `✗ failed:`
reason on the first row and shows its output. `read` and `todo_write` calls and calls with empty
output stay one line. Output is made safe before it is kept: control characters are blanked, tabs
expanded, a line redrawn with a carriage return keeps its last drawing and credential-shaped text is
redacted. At most 64 KiB of a result is kept (its start and its end), so the transcript stays small
after a huge `cat`.

When a card was cut, its last row reads `expand: /expand N`. `/expand [n]` prints call `n` in
full as new transcript lines (the whole command, all the retained output and the whole diff);
without a number it prints the latest call. Numbers count the session's calls from 1 and are not
reset by `/clear`, which empties the transcript, so a cleared call is an unknown call. It does not
touch the model's context. `/expand thinking` does the same for the model's
[reasoning](#model-reasoning). To read every call, its whole output and the whole reasoning without
adding anything to the terminal's scrollback, open the [transcript view](#the-transcript-view). In
screen-reader mode a call is still one labelled line, followed by the last ten lines of its output
as plain lines and the same `expand:` hint.

A long command under approval scrolls inside
half the screen, so the call it asks about stays visible above the input.

### Model reasoning

The model's reasoning shows as a dim, italic card behind a `thinking` header. While it streams, the
card shows the last five wrapped rows live and is not written to scrollback; once the reasoning
settles it is written folded to its first five rows, so a long trace does not push the answer off
screen; when the card was cut, `expand: /expand thinking` is its last row. `/expand thinking` prints the latest
settled card in full as new transcript lines; it is an error while that card is still streaming,
and when there is no reasoning. Like tool output, reasoning is made safe to paint (control
characters blanked, credential-shaped text redacted) and at most 64 KiB of it is kept, its start
and its end, which the expanded card notes. A model that withholds its reasoning shows no card.

A subagent's reasoning shows in that agent's own transcript when you open it in the
[agent view](#the-agent-list-and-the-agent-view), never inline in the main conversation.
`[tui] show_reasoning = false` hides the cards; it is on by default. Headless `-p` prints no
reasoning and `stream-json` carries none.

### Notices

Notices are the lines Fontaine adds itself. An informational notice is a dim line starting with
`·`. A warning starts with `⚠` and shows its headline in the theme's warning colour, with what to
do about it on dim lines indented underneath; text in backticks and `/commands` in that hint are
bold, so the actionable part stands out without relying on colour. Warnings open the conversation:
a guardian that runs on your main model, a `[guardian] profile = "minimal"` that is not in effect,
a tool-result scanner that is unavailable, an additional directory that was skipped and an ignored
`FONTAINE_AX_SCREEN_READER` value, and a [trusted skill](extensions.md#trusted-skills) whose file changed since you trusted it. A stale plugin skill's `allowed-tools` stay off until `/skills trust <name>`; a stale user skill only loses its guardian trust note. The background [plugin auto-update](plugins.md#auto-update) pass adds a notice, once, for each marketplace or plugin it staged for the next start, for a plugin revision it held until you run `fontaine plugin update <name>`, and for a failed update; it appears whenever the pass finishes, in whichever session is open. Set `FONTAINE_ASCII_GLYPHS=1` to use `*` and `-` in place of
`⚠` and `·` on terminals or fonts that lack them (see [Terminal setup](terminal.md#ascii-glyphs)).

Narrowing the terminal clears the visible screen and redraws the live area over the whole screen. Output
already scrolled off is kept. Lines still on screen at that moment are kept only where clearing the
screen saves it to history (tmux by default) and may be lost elsewhere. Lines already in scrollback
keep the width they were written at.

## Workspace trust

Before the screen opens, a folder you have not trusted gets the trust prompt. It names what the
folder defines that would run (hooks, [MCP servers](mcp.md), the commands in skills and its
settings files) and asks:

- `y` trust the folder. It is written to `trust.toml` (see
  [Workspace trust](configuration.md#workspace-trust)), the folder's untracked
  `settings.local.toml` is read with your trust, and what the repository defines starts. Each
  repository-defined MCP server is still asked about before it starts.
- `n` or Esc do not trust it. The session runs without the repository's hooks, MCP servers and
  skill commands, and the question comes back next session. Nothing is written.

Enter acts on the highlighted answer, which starts on "do not trust". Whatever you answer, nothing
the repository defines can approve a tool call: a repository's hook `allow` is ignored and its MCP
tools are checked like any `arbitrary` tool. A headless run does not ask and loads them.

## Status bar

The bar shows the permission mode, the directory the shell is in with the git branch checked out there (`· ~/work/app (main)`; a detached `HEAD` shows its short commit, the home directory is `~`, a long path is cut from the left with `…`), the model (followed by the [subscription quota](#subscription-quota) of the main and the guardian account when they have one, then `effort <level>` while a [reasoning effort](configuration.md#model) is set), how many subagents are running, `⏱ N` when `N` [scheduled tasks](#scheduled-prompts) exist, `◎ goal 12m` while a [goal](#goals) is active (`◎ goal paused` when it waits for you), and a key
hint rendered from the effective [keybindings](#remapping-the-keys). With [vim input
mode](#vim-input-mode) on, the bar also shows `vim:insert` or `vim:normal` after the mode. The
mode labels are `manual`,
`⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ auto mode on`, `⏵⏵⏵ AFK mode on` ([AFK mode](afk.md)) and
`dontAsk`. ` AFK STOPPED ` (reversed) shows while a temperature stop waits for `/afk review`, next to
the guardian-off chip.

### Subscription quota

An account with [`quota = "kimi"`](configuration.md#accountsname) adds the provider's own quota to
the bar, such as `kimi · 5h 60% (15:00) · 7d 30% (Sun) · code 5% · month 51% (Dec 1)`: every window
the provider reports, always, shortest first, each with its used share rounded to the nearest
percent (`100%` from the limit up) and, in parentheses, when it resets in local time: the clock
time (24-hour) when that is today, `tomorrow`, the weekday within the next six days, and the date
beyond. The reset is left out when the provider gives none or it has passed. From 90% on any
window, the segment takes the theme's warning colour.

A `claude-subscription` account needs no `quota` key: from the first turn on it adds
`claude · 5h 9% (15:00) · 7d 15% (Mon)`, read from the usage events the Claude CLI already sends
with its replies. With no data (an API-key CLI session, or before the first turn) there is no
segment.

The guardian's account is read the same way, and can be a different subscription. Its segment follows
the main one with a `guardian` prefix and the same run of windows, as in
`guardian kimi · 5h 82% (15:00) · … · month 16% (Nov 18)`, and shows only once any of its windows is
at 70% or more, so a guardian with room to spare adds nothing to the bar.
A guardian on the main model's account is not shown twice. The same figures reach the
[`result` event](headless.md#output) of a headless run as `account_usage`.

A `claude-subscription` account's figures appear as soon as a turn's stream reports them; every
other account's are refreshed every minute in the background. Both never block a turn or a
redraw. If a refresh fails the last figures stay for ten minutes and are then hidden; the first
failure of a session is reported once per account as a warning. A segment is also hidden when its model is on an
account without a quota source, and no figure is ever estimated: with no data from the provider nothing is
shown. On a narrow terminal the segments are kept in preference to the key hint, goal, scheduled
and agent counts, and the guardian's is dropped before the main one.

While a [subagent's transcript](#the-agent-list-and-the-agent-view) is on screen the bar's key hint
is about that agent: `viewing agent-3 · working · ↓ then Enter on main to go back · shift+tab mode`,
with the agent's own state (`working`, how it ended, or `no longer listed`) in place of the main
turn's. While the main turn runs the hint segment is empty, so the bar shows only the mode, directory,
model, quota, effort, agents, scheduled tasks and goal segments.

### Custom status line

A `[statusline]` command in your user settings replaces the built-in bar's hint area with its own
output. The command receives one JSON object on stdin and the first line of its stdout (ANSI
escape sequences stripped) is shown after the mode, directory, model, quota, effort, agents,
scheduled tasks and goal segments. Any non-zero exit, timeout after five seconds, or empty output
keeps the previous good line; with none yet, the built-in rendering is used. While a refusal, attach notice or
viewed-agent message is showing, it takes the custom line's place. The command runs through `sh -c`
(Git Bash on Windows); on Windows without Git Bash it runs in PowerShell.

```toml
[statusline]
command = "jq -r '.mode'"
refresh_interval_secs = 10
```

The JSON payload has the same field names as Claude Code's status line where Fontaine has the
data, plus Fontaine extensions:

| Field | Value |
|---|---|
| `model.id`, `model.display_name` | The main model reference |
| `cwd` | The shell's current directory |
| `workspace.current_dir` | Same as `cwd` |
| `workspace.project_dir` | The primary workspace root |
| `workspace.added_dirs` | Additional workspace roots, excluding the memory root and session scratch dir |
| `session_id` | The session id |
| `version` | The Fontaine version |
| `effort.level` | The reasoning effort, when set |
| `mode` | `manual`, `acceptEdits`, `plan`, `auto`, `afk` or `dontAsk` |
| `guardian.model` | The live guardian's label, or `null` when unavailable |
| `guardian.off` | Whether the guardian is disabled for this session |
| `afk.on`, `afk.stopped` | Whether [AFK mode](afk.md) is on, and whether the temperature ceiling has stopped the session |
| `recent_verdicts` | Last 5 gate decisions with a guardian verdict: `{tool, decision, rule, guardian_verdict}` |
| `account_usage` | Main and guardian account usage windows, in the same shape as headless output |
| `context_window.used_percentage` | Context used as a percentage of the model's window, when known |
| `cost.total_cost_usd` | Session total cost across all non-guardian sources, when any call was priced |

The `[statusline]` section is accepted only in the user file; it cannot be set from project,
project-local or `--settings` files, because the command runs arbitrary code.

While the conversation on screen is working, a spinner row sits between the task list and the
message box: a breathing-dot glyph whose colour drifts, the label `Working…` with a highlight
sweeping across it, and in parentheses the time since the turn began and `↓` the output tokens so
far, such as `● Working… (1m05s · ↓ 2.3k tokens)`. The token count is estimated from the streamed
text and replaced by the provider's figure when each model call ends, so it can step down slightly;
it is left out while it is 0. The row covers the whole turn, including tool waits, goal evaluation,
`/compact`, a `!` command and a skill expansion, and in a running agent's view it shows that agent's
elapsed time. It is hidden while an approval, a question or the guardian-unavailable prompt waits
for you, and it never takes input: Esc works as before. With `FONTAINE_ASCII_GLYPHS=1` the glyph is
`.oO0Oo`. The default 16-colour themes cannot blend colours, so there the highlight steps between
dim, normal and bold and the glyph cycles the theme's accent colours; the RGB themes
(`dark`, `light` and the daltonized pair) shift smoothly. Screen-reader mode shows no spinner. When
any subagent is running or recently ended, the [agent list](#the-agent-list-and-the-agent-view)
sits on the rows below the bar.

## Keys

The global keys below are remappable: each row names the action it triggers, and
`[keybindings]` in the settings maps keystrokes onto those actions (see
[Remapping the keys](#remapping-the-keys)). Keys that belong to an open overlay — approvals, the
`/config` panel, pickers, the slash-command menu — are not remapped, and neither are the vim
cursor keys (see [Vim input mode](#vim-input-mode)).

| Key | Action |
|---|---|
| Enter (`submit`) | Submit. Alt+Enter, Shift+Enter or Ctrl+J (`newline`) insert a newline; a trailing `\` before Enter also continues the line. A binding matches its exact modifiers, so Ctrl+Enter and Shift+Alt+Enter do nothing unless bound. Which of these your terminal sends, and how to bind Shift+Enter, is in [Terminal setup](terminal.md#new-lines-in-the-message-box). While a turn runs, a message is queued (see [Messages during a turn](#messages-during-a-turn)). With a `@` list open, Enter completes instead; with the [slash-command menu](#slash-command-menu) open on a bare `/name`, it runs the selected command; after other text, it sends the message. |
| Up, Down (`history_previous`, `history_next`), Ctrl-P, Ctrl-N | With the `@` list or the slash-command menu open, move the selection. Otherwise, on the first or last line of the input, recall older or newer submitted inputs; elsewhere they move the cursor. With messages queued, Up takes them back into the input. Down on the last line, when the input is not walking the history and the [agent list](#the-agent-list-and-the-agent-view) shows, moves the keyboard into the list: Up and Down select a row, Up on the first row gives the keyboard back. Ctrl-P and Ctrl-N are hardcoded aliases of Up and Down. |
| Enter, x, Esc in the agent list | Enter shows the selected row's transcript (`main` or a subagent). `x` stops a running subagent, or hides an ended one; on `main` or the agent on screen it is typed into the input. Esc gives the keyboard back to the input, and any printable key does too and is typed there. |
| Ctrl-R (`history_search`) | Search the prompt history; see [History](#history). |
| Ctrl-V (`attach_clipboard`) | Attach the clipboard's image to the message; see [Attaching files](#attaching-files). Terminals that turn Ctrl+V into a bracketed paste never deliver the key event; their paste arrives as text, and a dropped PDF path becomes a PDF attachment. |
| Tab | Completes the `@` file under the cursor, or the command name in the slash-command menu, when a list is open. In the `/` menu and the `@` list it first extends what you typed to what all the listed commands, files or directories share, and accepts the selected row only when nothing more can be extended. |
| Shift+Tab (`cycle_mode`) | Cycle the mode by increasing autonomy: manual, plan, accept edits, auto, [AFK](afk.md), and back to manual. It takes effect on the next tool call, even mid-turn. Entering `auto` or AFK lists the broad allow rules it sets aside and says so when the guardian cannot be asked. Entering AFK answers every open or queued approval, question and guardian dialog as a denial or no-answer; leaving it reviews the stretch, when the turn ends if one is running. A rebound key runs the same cycle — no binding can jump to a mode outside it. |
| Esc, Ctrl-C while a turn or a `!` command runs | Interrupt it. The interrupted turn is kept in the model's conversation up to its last finished tool round, followed by an interruption marker, and the next prompt continues from there; text the model was still streaming is not kept. Earlier turns are kept, and an open prompt is answered with deny. An open history search, `@` list or slash-command menu is closed first. Interrupting never stops a subagent: it keeps running and reports later, and the agents panel stops one (the model can also stop the subagents it started). While a subagent's transcript is on screen only Ctrl-C interrupts the main turn: Esc does not, because the turn is out of sight. |
| Esc when idle and a self-paced `/loop` waits for its next iteration | Ends the loop; see [Scheduled prompts](#scheduled-prompts). |
| Esc twice in quick succession when idle with an empty input | Opens the `/rewind` picker; see [Checkpoints and /rewind](checkpoints.md). Any other meaning of Esc above wins first. Off while a subagent's transcript is on screen. |
| Ctrl-C (`interrupt`), or Ctrl-D on an empty input (`quit`), when idle | Quit. Both keys are reserved: they cannot be rebound or unbound. With text in the input, Ctrl-D deletes the character under the cursor in Emacs mode and in vim's Insert state. |
| PageUp, PageDown | Scroll the command or diff in an open approval prompt. |
| Arrows, Home, End, Backspace, Delete, Ctrl-A/E/B/F/H/D | Move by character or line and delete a character. Ctrl-H is Backspace, Ctrl-D is Delete. |
| Ctrl-Left, Ctrl-Right, Alt-Left, Alt-Right, Alt-B, Alt-F | Move by word. A word is a run of letters and digits, so `/`, `.` and `_` separate words. |
| Alt-Backspace, Ctrl-Backspace, Alt-D, Ctrl-Delete, Ctrl-W | Delete the previous word, or with Alt-D and Ctrl-Delete the next one. Ctrl-W deletes back to the previous whitespace, so a path goes in one piece. See [Terminal setup](terminal.md#word-movement-and-deletion) for Ctrl-Backspace. |
| Ctrl-Home, Ctrl-End | Jump to the start or end of the whole input; Home and End stay on the current line. |
| Ctrl-U, Ctrl-K, Ctrl-Y | Delete to the start or end of the line, and paste the last deleted text back. One entry is kept, and it survives submitting. |
| Ctrl-L (`reprint`) | Redraw the screen from the transcript, keeping the conversation. Like switching to a subagent, it erases the terminal's scrollback first. Does nothing in screen-reader mode. |
| Ctrl-O (`transcript`) | Open or close the [transcript view](#the-transcript-view): every call, output, reasoning and denial reason in full. Ignored while the agents panel is open. |
| Ctrl-T (`todos`) | Expand or collapse the [task list](#the-task-panel); with no tasks it prints `no tasks`. Ignored while the agents panel is open. |
| (`agents_panel`, no default key) | Open or close the [agents panel](#the-agents-panel), even over an approval prompt, once you [bind a key](#remapping-the-keys) to it; `/agents` always opens it. Enter on an agent row there shows its transcript. |
| `/` at the start of the input or after a space | Opens the [slash-command menu](#slash-command-menu). |

### Pasting

Terminals that support bracketed paste hand the whole pasted text to the input box at once, so a
multi-line paste is inserted rather than submitted. Windows consoles have no bracketed paste: the
pasted text arrives as keystrokes. Fontaine treats a burst of keys, each less than 15 ms after the
last, that holds a line break as a paste, so a multi-line paste is inserted there too. A single-line
paste is replayed as the keys it arrived as, which is why a dropped file path is not attached as a
PDF on Windows. Pressing Enter by hand still submits.

### Remapping the keys

The global actions above live in a `[keybindings]` table of the settings: each entry maps one
keystroke onto an action, and the empty string unbinds the keystroke:

```toml
[keybindings]
"ctrl+g" = "todos"   # this key now opens the task list…
"ctrl+t" = ""       # …and this one does nothing
"f2" = "cycle_mode"  # extra keys can trigger any action
```

`/keybindings` prints the effective table — every action, the keystrokes it answers to, and
whether each binding is yours or a default — and the path of the user settings file to edit.
Changes apply on the next launch. A keystroke bound to a new action stops triggering the action it
had; binding one action to several keys is fine. `ctrl+c` and `ctrl+d` are reserved and always
keep their roles. The full syntax — modifiers, key names, the action list, scope rules — is in
[Configuration](configuration.md#keybindings).

### Vim input mode

`[tui] editor_mode = "vim"` (or `/vim`, which also saves the choice) makes the input box modal
instead of readline-style. The status bar shows `vim:insert` or `vim:normal` while it is on;
`/vim` toggles back to emacs editing. Approval prompts, questions and other overlay input fields
stay emacs-style.

- **Insert** is the start state and types as usual. `Esc` switches to Normal — it never
  interrupts or cancels from Insert, and with a history search, `@` list or `/` menu open it
  closes that first, exactly as in emacs mode. Ctrl+D (delete character), Ctrl+H (backspace)
  and Ctrl+W (delete word back) stay editing keys; every other Ctrl+key passes through to the
  remappable global keys above, so Ctrl+R still searches history.
- **Normal** takes text keys as commands: `h l 0 $` and the arrow keys move (by character, to
the line's ends), `w b e` move by word, `j`/`k` move between lines, `gg`/`G` jump to the start or
  end of the input. `d` and `c` wait for a motion (`w b e $ 0`) or double for the whole line
  (`dd`, `cc`, the latter into Insert); `D` and `C` take the rest of the line; `x` deletes a
  character, `r` waits for the character to replace it with; deleted text goes to the same
  single-entry yank slot as Ctrl-U/Ctrl-K, and `p` pastes it after the cursor. `i a A I o O`
  enter Insert at the cursor, after it, at the line's ends or on a new line below or above.
  `u` undoes the last Normal-mode edit (snapshots, capped at 100); there is no redo — Ctrl+R is
  history search. `Enter` submits from either state, and an unrecognized key is ignored.
- **Esc in Normal** is not consumed by the input: it reaches the usual arms — interrupting a
  running turn, cancelling a waiting `/loop`, or a double press opening `/rewind` — as it does in
  emacs mode.

## Slash-command menu

Typing `/` at the start of the input or after a space lists the commands and skills, and narrows
the list as you type the name. Names and aliases that start with what you typed come first, then those that merely
contain it; the match ignores case. A row found through an alias shows it in parentheses, as in
`/clear (reset)`. Up to eight rows are shown, scrolling to keep the selection visible.

| Key | Action |
|---|---|
| Up, Down | Move the selection. |
| Tab | When several commands match and they share a longer name than you typed, extend to that shared prefix. Otherwise complete the selected name. A bare `/name` gets a trailing space when the command takes an argument; after other text only that `/name` is replaced and a space follows it unless one is already there. An alias becomes the command's own name. |
| Enter | On a bare `/name`, run the selected command. A command with a required argument, shown as `<arg>` in its hint, is completed instead, since it cannot run without one. After other text, Enter is not a menu key: the message is sent as typed. |
| Esc, Ctrl-C | Close the menu until the text changes. |

The menu opens while the cursor is inside a `/name` that starts the input or follows a space, so
`hello /mod` opens it and `src/main` does not. It closes once you type a space, and it
never opens in a `!` shell command, over a panel or prompt, during a history search or beside a `@` list. A recalled
history entry that starts with `/` does not open it, so Up keeps walking the history.

## Shell commands

An input that starts with `!` is a shell command. The input box title changes and its border is
coloured; Enter runs the command instead of sending a message, and the `!` is not part of it.

- It runs as you, in the primary workspace root, with no approval prompt: it is your own action, so
  the [permission pipeline](permissions.md) and the [guardian](guardian.md) do not see it.
- It runs with the same environment (scrubbed, plus [`[env]`](configuration.md#env)) and the same
  output cut as the agent's `shell` tool, because the output goes to the agent. On Windows without
  Git Bash it runs in PowerShell, like the agent's `powershell` tool. It is killed after 120 seconds, or after 60 seconds without
  output, and has no standard input.
- A `!` input typed while a turn runs queues like a message and runs after the turn, in the order
  typed with everything else queued.
  While a command runs nothing else can be sent or queued; Esc or Ctrl-C interrupts it and kills
  its process group.
- The output is shown in the transcript and handed to the agent with your next message, marked as
  output you produced and as data, not instructions. An interrupted command tells the agent nothing.
- Each command is written to the session ledger and to the audit log, an interrupted one included,
  since it may already have had effects. It never counts as one of your messages to the agent.

## File mentions

Typing `@` after whitespace, or at the start of the input, opens a list of every workspace file
or directory that matches what follows it, best matches first (file or directory name prefix, then
name substring, then shorter paths). Directories are marked with a trailing `/`. The list shows up
to eight rows at a time; Up and Down scroll through all matches and keep the selection visible.
Tab first extends the token to what all the matches share, and accepts the selected row once
nothing more can be extended; Enter accepts the selected row directly. A file puts `@path ` in the
input; a directory puts `@dir/` with no space, so the list reopens on its contents and you can keep
completing into it. Esc closes the list until you edit the token. Files and directories come from
the same index the session starts with, which respects `.gitignore`, and is refreshed when a turn
ends and after `/add-dir`. Paths under the primary root are shown relative to it.

Completion only edits your input. The message is sent as you typed it, and the agent reads the
file through its `read` tool, so the content passes the same scope checks, deterministic floor and
guardian as any other read. An image file read that way is returned to the model as an image part
when the model accepts images.

## Attaching files

An inline file rides your message to the model as a content part, the way a typed sentence does.
Two kinds exist: a clipboard image (Ctrl+V) and a dropped PDF (drop the file on the terminal, which
pastes its path).

- Ctrl+V reads the clipboard image with the desktop's own tool (`wl-paste`, `xclip` or
  `pngpaste`) and saves it in the session's private scratch directory. No image on the clipboard
  is a one-line notice, nothing more. Terminals that turn Ctrl+V into a bracketed paste never
  deliver the key event; the paste path already handles their text.
- A dropped file whose path lands in the input as a paste becomes a PDF attachment when the file
  sniffs as `%PDF-`; any other path stays typed text.
  On Windows a dropped path is not detected as a PDF attachment; see [Pasting](#pasting).

Each attachment shows as a removable chip above the typed text, with its kind and size. Backspace
on an empty input removes the last chip. On Enter the chips become parts of the message: an image
reaches a model that accepts images as an image, a PDF as a document, and the transcript shows a
one-line marker — `[image attached: shot.png · 226 KB]` — under your line. What the current model
does not accept is not sent: the turn notes each such attachment in text instead, and the status
bar warns "model does not accept images" when you attach one to such a model. Attachments wait out
a running turn and are refused in an agent's view: both keep the chips in the input.

Attachments are base64, capped at 5 MB; the image is sniffed when it is read. The
[context gauge](configuration.md) counts text tokens only and does not subtract what an image
costs. A `read` of a PDF never returns the file — the result is a stub pointing here.

## History

Every input you submit, `/` commands and `!` commands included, is kept in `history.jsonl` in the
[data directory](configuration.md#where-files-live), readable by the current user only.
It holds what you typed, so treat it like your shell history. The file is shared by all projects;
each line records the primary root it was typed in, and only this project's lines are recalled. It
keeps the newest 5,000 lines. A file that cannot be read or written is reported once and the
history then lasts for the session only.

- Up on the first line of the input shows older entries, Down on the last line shows newer ones,
  and going past the newest restores what you had typed. Editing a recalled entry stops the walk.
  Ctrl-P and Ctrl-N do the same as Up and Down.
- Ctrl-R opens a search above the input. Typing narrows the match (case-insensitive), Ctrl-R again
  moves to the next older match, Enter or Tab put the match in the input without sending it, and Esc
  cancels.

## Markdown in replies

The transcript renders the markdown in the assistant's replies:

- `**bold**`, `*italic*` and `` `code` `` (code is shown bold), and links as their text, coloured and underlined, as are bare
  `http://` and `https://` URLs;
- headings, bold, with a first-level heading also underlined;
- bullet and numbered lists, nested by indent, wrapped under their text;
- fenced code blocks, dimmed and indented two spaces, kept verbatim;
- tables, with aligned columns (`:--`, `--:`, `:-:`), a rule under the header and long cells wrapped
  inside their column. A table too narrow for its columns (4 characters wide each) shows as
  its source lines.

Everything else shows as the source text, wrapped: block quotes, HTML, horizontal rules, indented
code and images, and a whole list with an item that holds anything other than text and nested lists (a heading,
a code block or a table included). A line break
in a paragraph stays a line break. Set `FONTAINE_ASCII_GLYPHS=1` for `-` bullets and `|`/`-`
table lines.

While a reply streams, the rows that can no longer change are written to scrollback a block at a
time, and the paragraph being written stays in the live area as source until it ends, so a long
reply shows its formatting as each block completes. The paragraph being written, an open fenced
block, or a list taller than the live area can reach scrollback partly as source. Only the reply as it is shown changes: `/copy`
and `/export` give the exact text the model wrote, and the line-mode renderer prints it as is.

## Messages during a turn

Enter on an ordinary message while a turn runs queues it. Queued messages show above the input as
`queued: <first line>`. They are not sent at once: the agent receives them with its next tool
result, appended to that result after a line saying the user sent the message while it was working,
and the transcript then shows the message as a `>` line at that point. Only then do the words count
as yours for the [guardian](guardian.md).

- Up on the first line takes the queued messages back into the input, for editing.
- If the agent answers without another tool result, the queued messages are sent as the next turn.
- If the turn is interrupted, fails or hits its turn limit, undelivered messages return to the
  input, in front of whatever is there, and nothing is sent. A turn the interrupt cut short stays
  in the model's conversation, so a message the agent received with a tool round that finished
  before the interrupt is kept; a message delivered into the tool batch still running at the
  interrupt is lost and must be sent again.
- A built-in command other than `/loop`, or a known skill invocation, queues like a message and
  runs when the turn ends, in the order typed: a skill runs as its expanded prompt, a built-in
  command does exactly what it would have done had it been submitted at that moment. Neither is
  steered into the running turn, and a message queued behind one waits with it rather than
  overtaking it. If the model is parked in a blocking wait, a queued command, skill or `!` line ends it and the
  wait's result tells the model the entry is pending (its first line, quoted); the entry itself still
  runs after the turn. Unknown slash names and `/loop` are refused with a one-line hint that replaces
  the status bar's key hints until the next input event (key, paste or resize), or the end of the
  turn; it adds nothing to the transcript, and the input stays. The line renderer has no status
  bar, so there the refusal is a transcript notice. `/loop` is refused until #298.
- A `!` shell command queues the same way and executes after the turn, one at a time and in the
  order typed; while one executes, nothing else can be sent or queued.
- The commands that act on the UI alone (`/status`, `/theme`, the panels, `/help`, `/recap`, `/cost`, …) run
  immediately, turn or no turn.
- A queued command that opens a picker or dialog (`/model`, bare `/resume`, `/rewind`, `/login`)
  holds the later queued entries until it closes. `/resume <name>` and `/branch` are refused while
  later entries remain, so a session switch cannot silently discard them.

## Approval prompts

When a call needs approval, a prompt lists the call and why it asked. A bell or desktop
notification tells you when it opens; see [Notifications](terminal.md#notifications). A
[`notification` hook](hooks.md#lifecycle-events) runs in the background at the same moment, for an
approval (`permission_prompt`) or a question (`elicitation_dialog`), whatever `[tui] notify` is.

- `y` allow once.
- `s` allow session, when the prompt offers it.
- `a` always allow, when the prompt offers it. The prompt shows the exact rule that will be added
  to `[permissions] allow` in your user settings file, with newlines and tabs in the rule shown as `\n` and `\t`, and other control characters as `\u{..}`.
- `n` deny. Enter on the deny row opens an optional reason, which is sent to the model with the
  denial.

For an `edit`, `write` or `notebook_edit` call the prompt also shows the change as a diff, above the reason: a
`path  +added -removed` header, then the changed lines with three lines of context, each behind
its line number, added lines marked `+` and removed lines `-` (in green and red). Long lines wrap
under a blank gutter, and PageUp and PageDown scroll the diff like any long call. At most 200 rows
are shown; the rest is summarized as `… N more changed lines (not shown)`. The diff is computed
from the file as it is when the prompt opens, so it is advisory: the tool applies the edit to the
file as it is when it runs. The transcript card of an allowed call shows the same diff. No diff appears, and the prompt shows only the path, for other tools,
for a file that is missing (`edit`, `notebook_edit`), not UTF-8 or larger
than 1 MiB, and for an edit that matches nowhere or more than once. A path outside the workspace
roots resolves to its real target, so an approved external write shows that file's diff. A `notebook_edit` diff is the cell's source before and after, not the notebook's JSON. Control characters in file
content are blanked and credential-shaped text is redacted before it is shown.

When several agents ask at once their prompts queue and are shown one at a time, and a prompt that
belongs to a subagent names it: `Allow shell (scout, agent-2)?`. A prompt whose agent was killed goes away.

Up, Down and Tab move the highlight, Enter confirms. Prompts forced by the floor (an `ask` rule, a
critical path, a protected path) and shell calls that are not one literal simple command offer
only allow once and deny. Enter and the letter keys are ignored for the first 400 ms after a prompt
opens, and key repeats never answer, so text typed ahead during a turn cannot approve a call. See [Permissions](permissions.md) for what each answer does.

## Questions from the model

The model can stop and ask you 1 to 4 multiple-choice questions with `ask_user_question`. A dialog
shows one question at a time: a header chip, the question and numbered options with their
descriptions, then an "Other" row holding a text field for your own words. The field is always
visible; the highlight marks where your keys go. On an option row the keys choose, on the "Other"
row they type.

| Key | Action |
|---|---|
| Up, Down, Tab, `j`, `k` | Move the highlight. |
| `1`-`9` | Jump to that row. |
| Space | Toggle the highlighted option, when the question allows several. |
| Enter | Choose the option (several: confirm the toggled ones) and go to the next question, together with any text in the field as a note. On "Other" with text, submit the text (several: combined with the toggled options). |
| Esc | Decline the whole call and discard the answers already given. On "Other" it only moves the highlight back to the options. |

Typing a character while the highlight is on an option row drops straight into the field: the
highlight jumps to the "Other" row and the character starts your answer. Once the highlight is
there, every printable character — digits, `j`, `k` and Space included — goes into the field;
arrows and Tab move the highlight off it, and the text stays when you leave. Leaving and coming
back works: text typed alongside a chosen option is submitted with it as a note. An answer needs
at least one selection or some text, so Enter on the "Other" row with an empty field does
nothing — with several selections allowed, confirm the toggled options with Enter on an option
row. Enter, Esc and the letter and number keys are ignored for the first 400 ms after a dialog
opens, and key repeats never answer, so text typed ahead during a turn cannot answer for you. Esc
here declines the question rather than interrupting the turn; Ctrl-C still interrupts, and
answers the question as declined. Only the main conversation asks: questions queue behind each
other, and behind an open approval prompt, which takes the keys first. Declining tells the model
so, and it carries on with its best judgment or asks in plain text. The words you type under
"Other" are recorded as your message, so the [guardian](guardian.md#what-the-guardian-sees) reads
them as intent — a note on a choice included; the options you pick are not.

## When the guardian asks

An `escalate` verdict opens the usual approval prompt with the guardian's `[Rule] explanation`
as the reason. These prompts offer only allow once and deny, because a lasting grant would take
the question away from the guardian.

After three denials in a row, or twenty in a session, `auto` pauses: a notice says so and the
next call that needs the guardian asks you instead. Approving a call resumes `auto`.

If the guardian model named by `[guardian] model` cannot be used at all (no credential, an
unknown account), nothing is substituted silently. A prompt offers:

- `r` retry: look the pinned model up again, for when its credential has appeared.
- `p` pick another validated guardian: the models of `guardian_models.toml` and the models your
  accounts list explicitly. The pick is saved as `[guardian] model`, so later sessions use it too.
- `m` switch the session to `manual` and ask you about this call.
- `n` leave the call denied.

## Resuming

`fontaine --continue` reopens the most recently updated session that was started in this folder,
`--resume <id-or-name>` a session by id or by name, and `--fork-session` with either starts a copy
under a new id. `--name <name>` names the session you start (the copy, with `--fork-session`) so
`--resume` can find it later. The transcript starts empty with a note saying which session
continues; the model has the earlier conversation, including a turn that was still running when
the session's process died, kept and marked as unfinished, so the model continues the interrupted
task. The session starts in the mode your settings
give, with no grants from before. See [Headless runs](headless.md#continuing-a-session) for the
details.

Inside a running session, `/rename` names it, `/resume` switches to another session of this folder
and `/branch` carries on in a copy; see [Sessions](#sessions). A switch is a fresh start of the
chosen session, just as `--resume` would give: the default permission mode, no grants, and the MCP
servers and the tool-result scanner started again. What the old session was doing is stopped, so a
switch happens only between turns.

### Sessions

`/resume` with no argument opens a picker of the sessions that were started in this folder, newest
first, without the one you are in. Each row shows the session's name, or its first message when it
has no name, and how long ago it was last used; `⎇` marks a session that is a branch of another.
Type to narrow the list by name, first message or id, Up and Down select, Enter switches and Esc
closes the picker. In screen-reader mode the rows are plain lines that say `branch` instead of
showing the mark and end with the first eight characters of the id. `/resume <name-or-id>` skips the
picker: an id is taken whatever folder it was started in, a name is matched without regard to case
among this folder's sessions, and two sessions with one name are an error that lists their ids.

A name is 1 to 80 characters with no control characters. `/rename <name>` records it in the
session's ledger; the latest name wins. `/rename` alone draws a name from the first line of your
first message since the last `/clear`, cut to 60 characters (a conversation `/rewind` to that first message also takes it away, so the name is drawn from the next one you send); it makes no model call, and is an error
when there is no message yet.

`/branch [<name>]` (also `/fork`) copies the session under a new id and carries on in the copy,
leaving the original as it was. The copy is named `<name>`, or the original's name followed by
` (branch)`, or nothing when the original has none. `/branch` is refused while a subagent is
running, or a report or background-job notice is waiting to be delivered, since the copy would
not carry that state.

`/export [<path>]` writes the conversation as Markdown. The default is `fontaine-<first 8 characters
of the session id>.md` in the project directory (the launch directory, or `--cd`); a relative path is relative to it.
When the session has subagents — live, or recorded in its ledger before a `--resume` — the target
is a folder, `fontaine-<first 8 characters of the session id>/`, instead. The folder holds
`main.md`, the root conversation with a `## Subagents` index after the session header, and one
`<agent-id>.md` per subagent. The index lists every subagent the ledger recorded, oldest first:
its id, type, parent and status, with a link to its transcript file. Each `<agent-id>.md` opens
with the same identification and renders the subagent's conversation exactly like the root's. A
subagent that is still running is exported rather than refused: its file holds its completed runs
and, after a marker, the in-flight run's messages from its journal, and its index row says so. A
subagent whose process was killed mid-run is exported the same way, with the run marked as
interrupted. Subagents whose spawn turn was
later rewound away are still listed: their transcripts stand as per-agent evidence, the way the
ledger keeps the fact of their work.
The text is the model's conversation, so after a compaction the summary stands where the older
messages were (labelled as one), and after `/clear` or a `/rewind` only the conversation that remains is in it. A tool
call is one line, and a tool result appears only as its length. Everything the export writes is
readable by you alone, since a transcript can hold secrets, and an existing file or folder is an
error: an export never overwrites.

## Commands

| Command | Action |
|---|---|
| `/add-dir <path>` | Adds a workspace root. The path is validated and canonicalized like `--add-dir`; file tools may reach it from the next turn, with the conversation kept. Until then a file-tool call naming a path outside the roots is adjudicated like any unapproved call: the write tools (`write`, `edit`, `notebook_edit`) ask here, are judged by the guardian in `auto` and are denied in `dontAsk` and headless runs, while `lsp` is refused outright, with a hint naming this command and the session's scratch directory, which is always a root. The read-only tools (`read`, `ls`, `glob`, `grep`) ask or go to the guardian like any unapproved call. See [Permissions](permissions.md#the-pipeline). Only between turns. |
| `/afk report [all]`, `/afk review` | [AFK mode](afk.md), the last stop of the Shift+Tab cycle, is for working while you are away: nothing is asked of you, each call that would have needed approval is denied with guidance to finish the allowed work or stop and report. `/afk report` shows the [report](afk.md#the-report) in the transcript (`all` lists every stretch), and `/afk review` shows it and, after a confirmation, lets a session [stopped by the temperature ceiling](afk.md#the-stop) run again, in `manual` if AFK was on. Both only between turns and only in the main conversation. `--permission-mode afk`, `--afk`, `default_mode = "afk"` and the cycle key enter AFK; no tool does. Entering it is refused while the guardian is disabled, and `/guardian-disable` while AFK is on or stopped. |
| `/agents`, `/tasks` | Opens the [agents panel](#the-agents-panel): every subagent, ended ones included until a `/clear` prunes them, and the background jobs. |
| `/auto-mode-setup` | Collects this project's [environment facts](guardian.md#project-environment) (GitHub API through `gh`, repository files and, in a trusted workspace, a scan by the guardian's model) while the screen stays live; Esc cancels. The scan's proposals are shown in the transcript and in a review prompt (`y` saves them, `n` discards them), entries that relax the guardian first. The facts are stored as `fontaine auto-mode setup` stores them and apply to this session from the next guardian call; the report ends with `applied to this session`. A last prompt offers the guardian's [allow screen](guardian.md#setting-up-the-composition): the hosted screen on an `openrouter` account whose key resolves (one confirmation that states guardian state is sent to OpenRouter and Typesafe), or a local one (safe or fast, then a confirmation); it writes the user settings file and applies from the next session. The startup warning in `auto` points here. |
| `/branch [<name>]`, `/fork` | Copies the session under a new id and carries on in the copy; the original is left as it was. Only between turns, and refused while a subagent runs. See [Sessions](#sessions). |
| `/btw <question>` | Asks a side question about the conversation without adding to it; see [Side questions](#side-questions-and-recaps). |
| `/clear`, `/reset`, `/new` | Starts over with an empty context. See [Context](#context). It takes no name for the old conversation; `/rename` names a session, and `/resume` returns to one later. |
| `/compact [<focus>]` | Summarizes the conversation so far to free context; the optional focus tells the summary what to keep above all. See [Context](#context). |
| `/config`, `/settings` | Opens the [settings panel](#settings-panel): every settings key with its effective value and the scope that supplied it, and a few preferences you can change. |
| `/context [all]` | Shows what fills the context window. See [Context](#context). |
| `/copy [<n>]` | Copies the latest assistant reply, or the `n`th latest, to the system clipboard. A reply is the text between tool calls, so a turn that calls a tool and then answers has two; `/copy` takes the last one. It is an error when there is no such reply. |
| `/cost`, `/usage` | Shows the session's token usage and spend by source and by model, guardian included; see [Usage and cost](#usage-and-cost). Runs immediately, turn or no turn. |
| `/denied` | Lists the calls the guardian denied this session, with their rule label and explanation. Up and Down select; `r` approves the selected call once, as if you had approved it at a prompt, and tells the agent on its next turn that it may retry exactly that call. The retry runs without asking the guardian, never enters the verdict cache and does not count toward the denial limits. The deterministic floor still runs first, so an `ask` rule or a critical path still prompts. Esc closes the list. |
| `/bug [<description> \| <draft id> \| discard <draft id>]` | Files a Fontaine bug report as a GitHub issue on `madesroches/fontaine`. It first shows the exact text that will be sent; nothing is written or sent until you confirm, and Esc cancels. The default `minimal` level holds the environment, a redacted settings snapshot, recent warnings and guardian verdict summaries; `full` adds the last turns of the transcript, and you toggle it in the preview. Known secret values (stored credentials, API-key environment values, MCP environment values) are scrubbed, plus a token-pattern pass. With `gh` installed and authenticated it runs `gh issue create`; otherwise it prints a prefilled new-issue URL and keeps the full report in an owner-only file under the data directory's `bug-reports/` folder. The transcript never goes in the URL. The submission is recorded in the ledger and audit log. The URL carries the report without the transcript, cut so the encoded URL stays under 8000 bytes, and names the bundle file rather than its path. Drafts the model queued with `draft_bug_report` are listed by a bare `/bug` and can be loaded as the description with `/bug <draft id>`; a draft is deleted once the report built from it is filed, and `/bug discard <draft id>` deletes one without filing. The footer counts the drafts still pending. Main conversation only. |
| `/exit`, `/quit` | Leave. |
| `/expand [<n> \| thinking]` | Prints tool call `n` in full (command, retained output, diff) as new transcript lines; the latest call without a number. `n` is the number in the card's `expand:` hint, counted from the start of the session. It is an error for a call that does not exist or that `/clear` removed. `/expand thinking` prints the latest [reasoning card](#model-reasoning) in full instead. |
| `/export [<path>]` | Writes the conversation to a Markdown file, owner-only, never over an existing file; with subagents it is a folder — `main.md` plus one `<agent-id>.md` per subagent. See [Sessions](#sessions). |
| `/goal [<condition> \| clear]` | Keeps working until the condition is met; see [Goals](#goals). Bare `/goal` shows the goal's status. |
| `/effort [off\|low\|medium\|high\|max\|unset]` | Without an argument, shows the session's reasoning effort and the levels the current model takes. With a level, sets it for the rest of this session; `unset` goes back to the model's own default. It is not saved: `[model] effort` is the standing choice. The next turn sends it with no rebuild, and a subagent started afterwards inherits it. A level the model cannot take is left out with a one-time warning. The status bar shows `effort <level>` beside the model while one is set. |
| `/guardian-disable` | Turns the guardian and the permission floor off for the rest of this session, after a confirmation. A warning chip stays in the status bar until `/guardian-enable`. Session-scoped only: it is never saved to settings, a project cannot turn it on, and subagents are never loosened by it. Intended as a development escape hatch. |
| `/guardian-enable` | Turns the guardian and the permission floor back on for this session. |
| `/guardian-model [<account>/<model>]` | Without an argument, opens the [model picker](#choosing-a-model) over a live listing. With one, pins the guardian to that model for the live session and saves it as `[guardian] model`. A model whose account has no credential becomes an unavailable guardian, which the next adjudication reports. When a higher scope (`--settings` or a project-local file) sets `[guardian] model`, the notice adds that the scope wins from the next launch. |
| `/help` | Lists the built-in commands, your user-invocable skills and the main keys. |
| `/hooks` | Lists the [hooks](hooks.md) by event with their matcher, type, command, URL or prompt and scope, marks the ones that do not run (a repository's hooks before workspace trust, or the kinds a repository may not define) and names the events with none. |
| `/init` | Has the agent write the project instruction file, or improve the one that exists. See [Instruction files](instructions.md#init). Takes no argument. |
| `/login [<account>]` | Opens the [provider dialog](#managing-providers) to add an account, log in, log out or remove one. With an account name it opens straight on that account's log-in: a hidden key prompt, the device sign-in for `opencode-go`, or, for a `claude-subscription` account, the check of the external Claude CLI. Only between turns. |
| `/loop [<interval>] <prompt>`, `/loop list`, `/loop stop [<id>]` | Runs a prompt again and again while the session is idle; see [Scheduled prompts](#scheduled-prompts). |
| `/mcp [reconnect <name\|all>]` | Lists each [MCP server](mcp.md) as `connected`, `pending (reconnecting, attempt N of 5)` while a dropped remote server is being retried, `failed` with the error, `left off` (you declined it) or `not loaded` (the workspace is not trusted), each with the scope that contributed it — a plugin's servers read `[plugin]`. `/mcp reconnect <name>` restarts one server and `/mcp reconnect all` every failed one, with no waiting; only servers tried this session can be restarted, never one you left off or that is not loaded. It runs between turns only. Any other argument shows the usage line. |
| `/memory [<n>]` | Lists the instruction files, numbered, with where each came from and the `FONTAINE.md` files that do not exist yet. With a number it opens that file in `$VISUAL` or `$EDITOR`, creating it first when needed. See [Instruction files](instructions.md#memory). |
| `/model [<account>/<model>\|<role>]` | Without an argument, opens the [model picker](#choosing-a-model). With one, which may also be a [role](configuration.md#modelsroles) name or a comma-separated list, switches the live session to that model (the role's first entry) and makes the list the session's, rebuilds the agent on it with the conversation kept, and saves what you typed as `[model] model` in your user settings file; when a higher scope sets `[model] model`, the notice adds that the scope wins from the next launch. The switch is for this session only, and the notice names the key to edit, when you type a list, `$main`, or a role that reaches `main`, and when `main` under `[models.roles]` or a list in `[model] model` is set in any settings scope. The guardian moves with it only when it is on the main-model fallback (no `[guardian] model` and no validated model available); a pinned or validated guardian stays. The session's [`/effort`](#commands) level is kept and checked against the new model; one it cannot take is dropped with a warning. |
| `/output-style [<name>\|none]` | Without an argument, lists the [output styles](extensions.md#output-styles). With one, switches to it for the next turn; `none` removes it. |
| `/permissions` | Shows the permission mode and the `deny`, `ask` and `allow` rules in force, each with the scope that contributed it (session and always grants included), the workspace roots and the settings file where rules are edited. It is read-only; see [Permissions](permissions.md). While the guardian is off it adds that these rules are not being enforced. |
| `/plan [<description>]` | Switches to plan mode, as Shift+Tab does (out of AFK it reviews the stretch). With a description it also sends it as your next message, so `/plan fix the auth bug` plans the fix. |
| `/plugins` | Lists the installed [plugins](plugins.md): name, marketplace, pinned short sha, enabled or disabled, and their loaded component counts. Installs and marketplace changes stay on the CLI; the only background change is the [auto-update](plugins.md#auto-update) pass, whose held revisions `fontaine plugin list` marks. |
| `/recap` | Reports what the session has done so far, from the ledger and without calling a model; see [Side questions](#side-questions-and-recaps). Runs immediately, turn or no turn. |
| `/rename [<name>]` | Names the session. Without a name, draws one from the first line of your first message. See [Sessions](#sessions). |
| `/resume [<name-or-id>]` | Switches to another session started in this folder: the picker without an argument, that session with one. Sessions where nothing was said and that have no name are not listed. Only between turns. See [Sessions](#sessions). |
| `/rewind`, `/checkpoint` | Opens a list of this conversation's prompts to restore the code, the conversation or both to the start of one. Esc twice does the same. See [Checkpoints and /rewind](checkpoints.md). Refused while a subagent runs or a report, message or board reminder waits to be delivered; a conversation restore retires the idle [subagents](agents.md#agent-lifecycle). |
| `/skills [trust <name>]` | Without an argument, lists every loaded [skill](extensions.md#skills) with its scope, marking those only the model can use, those only you can run, those a built-in command hides, and those you have trusted (`trusted`) or trusted before the file changed (`changed (re-trust)`). `trust <name>` opens a dialog that lists the skill's `allowed-tools` (or, for a skill you trusted before, what changed in them and in its `!` commands) and records your trust on `y`, in the session ledger and audit log too. The trust applies from the next session start; the running session's grants and this list do not change. The trust form waits for the turn to end; a [project skill](extensions.md#allowed-tools) cannot be trusted. See [Trusted skills](extensions.md#trusted-skills). |
| `/status` | Shows the version, the session id, the workspace roots (the scratch directory labelled separately), the permission mode, the main model, the guardian (or why it is unavailable) with its profile, the output style, and how many MCP servers are connected. While the guardian is off it reports that instead of the guardian's model. |
| `/theme [<name>]` | Without an argument, lists the colour themes, marks the current one and names the file the choice is saved to. With a name, switches to that theme at once and saves it as `[tui] theme` in your user settings file. An unknown name is an error that lists the themes. See [Terminal setup](terminal.md#colours). |
| `/todos` | Expands or collapses the [task list](#the-task-panel), as Ctrl-T does; with no tasks it prints `no tasks`. |
| `/transcript` | Opens or closes the [transcript view](#the-transcript-view), as Ctrl-O does. Runs immediately, turn or no turn. |
| `/<skill> [arguments]` | Runs a [skill](extensions.md#slash-commands-and-arguments). |

Typing `/` lists the built-in commands and your user-invocable skills with their argument hints,
narrowing as you type; a skill marked `user-invocable: false` does not appear.

`/copy` writes the reply with an OSC 52 escape, so it needs a terminal that supports OSC 52
clipboard access. Inside tmux it first loads the reply with `tmux load-buffer -w -`, which tmux
forwards to the outer terminal under its default `set-clipboard external` (see
[tmux](terminal.md#tmux)); if that fails the reply still lands in a tmux buffer. A terminal that
ignores the escape leaves the clipboard unchanged without reporting an error.

## Context

`/clear` empties the model's context. The session carries on: the same id, ledger, audit log,
grants and permission mode. The transcript is emptied and the screen and the terminal's
scrollback are erased, as Ctrl-L does (screen-reader mode keeps its history), and so is the
guardian's view of what you have asked, which starts again from the next message you send; the
files, actions and denials it knows about are kept.
[`/rewind`](checkpoints.md) trims the guardian's view the same way for the prompts it takes back. The old
conversation cannot be resumed: `--resume` reopens the empty one. `/clear` is refused while a
subagent is running or a report, message or board reminder waits to be delivered: a running subagent's calls are judged against what you asked, and a notice or reminder would reach a conversation you just emptied. Wait for it
or kill it from the [agents panel](#the-agents-panel). Idle subagents are retired and pruned: they
could not be woken against intent the clear drops, and the subagents the cleared
conversation spawned leave the [agent list](#the-agent-list-and-the-agent-view) and the
[agents panel](#the-agents-panel); their persisted transcripts stay on disk, so `/export` still
renders them.

`/compact [<focus>]` asks the main model to summarize the conversation, keeping only the newest
reply as it is, and replaces the older part with the summary. It shows
`compacted the conversation: ~41.2k → ~6.3k tokens`, or `nothing to compact` when there is nothing
older to summarize. Esc interrupts it and leaves the conversation as it was. Messages you type
meanwhile are put back in the input box when it ends instead of being sent. The model must be one
the catalog lists (a derived model counts as listed). Compaction also runs on its own before a turn once the conversation outgrows
the model's window less a reserve, or earlier if you set
[`[context] autocompact_window`](configuration.md#context).

`/context` shows what the next request would carry: the system prompt, instruction files, the
skill index, the tool definitions (MCP tools apart), the messages and what is free of the window,
with the point where compaction runs on its own. `/context all` also lists each instruction file (imported files and rules included)
and tool, largest first. The numbers are estimates from a character-based counter, not the
provider's tokenizer, and they count the same way the compaction trigger does. When the model is
not in the catalog (a derived model counts as listed) there is no `free` row and it never compacts automatically; a catalogued model
whose window is not known has no `free` row either and compacts automatically only when
`autocompact_window` is set. When the total is over the window it says so.

## Side questions and recaps

`/btw <question>` asks the main model one question about the conversation so far and prints the
answer as a transcript notice, `btw: <question>` followed by the answer. The model sees the
conversation rendered as text, newest part kept if it is long, has no tools and cannot act. Neither
the question nor the answer is saved: the next request does not carry them, the ledger does not
record them as something you said, and the guardian's view of what you have asked is unchanged.
`/clear` removes the notice with the rest of the screen. Each question stands alone; a later one
does not see an earlier one. It costs tokens on the main model, counted in the session's spend, and
waits 60 seconds at most for an answer, which is cut at 4,096 tokens. Like `/compact` it waits
behind a running turn, because the conversation is being written then; Esc interrupts it, and text
typed meanwhile is put back in the input box. A bare `/btw` is a usage error, and a subagent's view
refuses it with `switch back to main to run this`.

`/recap` is a report built from the session ledger, so it costs nothing and works when no model is
reachable. Each line is left out when it has nothing to say:

- what you asked since the last `/clear` or `/rewind`: the count, the first message and the latest
  few, each cut to one line;
- the active goal;
- the files the session was allowed to write or edit, counted, with the first few named. These are
  the files of allowed `write`, `edit` and `notebook_edit` calls over the whole session, whether or
  not the call then succeeded and whether or not a `/rewind` reverted it;
- the tool calls by tool, most used first;
- the calls denied, with the latest one's tool, rule and summary;
- the subagents spawned, and how many are running now;
- once AFK mode has run, how the latest stretch stands (`afk: blocked — /afk report`);
- the permission mode.

Everything except the first line covers the whole session, earlier runs of a resumed session
included.

## Usage and cost

`/cost` (alias `/usage`) prints what the session's model calls have used, one row per source that
made a call, then a total, then a second table by model:

```
Session usage
  source        calls   input   output  cache read  cache write  cost
  main             12   84.2k    9.1k       61.0k       3.2k      $0.4120
  guardian         31   22.0k    1.4k        9.8k          0      $0.0150
  subagents         8   40.1k    5.0k          0           0      $0.1900 · 5.0k unpriced tok
  total            51  146.3k   15.5k       70.8k       3.2k      $0.6170 · 5.0k unpriced tok
By model
  model                                 calls   input   output  cache read  cache write  cost
  main · anthropic/claude-opus-5-5         12   84.2k    9.1k       61.0k       3.2k      $0.4120
  coding · anthropic/claude-sonnet-5-5      5   30.0k    4.0k          0           0      $0.1000
  coding · kimi/kimi-for-coding             3   10.1k    1.0k          0           0      $0.0900 · 5.0k unpriced tok
  anthropic/claude-haiku-5-5               31   22.0k    1.4k        9.8k          0      $0.0150
```

The sources are `main`, `guardian`, `subagents`, `goal` (the goal evaluator), `compaction` and
`side questions` (`/btw`), in that order. `calls` counts model requests, with a guardian
adjudication counting once however many requests it took to get a verdict; a guardian answer served
from its verdict cache is not a call, but one inside a guardian composition cannot be told apart
and does count. The `cache read` and `cache write` columns are left out when the provider reported
no cache tokens.

The by-model table has one row per role and model that served a call, with the same columns. A
call asked for through a [role](configuration.md#modelsroles) shows the role first
(`coding · anthropic/claude-sonnet-5-5`), so a list that fell through shows the spend on each entry;
a model named directly, and the guardian, show the reference alone. Compaction counts under the
`compaction` role, or under the main model when you defined none.

A dollar figure covers only the calls that had a price. A source with none shows `n/a`, and when
some calls had no price (an account whose models the catalog does not price) the figure is a lower
bound and ends with ` · N unpriced tok`, the input and output tokens of those calls. A session
cost budget set under `[agents]` adds a last line with the spend against it.

The report does not count the estimate for a guardian request a composition dropped before its
answer came back, the LLM tool-result scanner's calls, the local screen calls of a guardian
composition, the repository scan of `/auto-mode-setup`, or calls made outside a session such
as `fontaine auto-mode critique`. It reports the current process's session: `/clear` keeps
counting, while `/resume`, `/branch` and `--resume` start from zero, because the figures are not
saved with the session.

## Choosing a model

`/effort` is the reasoning-level counterpart: it shows or sets the level for the session, and a model switch keeps the level and checks it against the new model. `/model` with no argument opens an overlay above the message box. The first line shows the current
model and the guardian. Below it, the models of every configured account sit under a heading per
account, sorted by account name, and the current model is marked `(current)` and selected. A
`Recent` group comes first with the models you switched to lately, up to five. They are kept in
`recent_models.json` in the data directory, separately for `/model` and `/guardian-model`, and an
entry drops out when its account no longer offers the model.

Typing filters the list: the text matches `<account>/<model>` as a case-insensitive subsequence, and
better matches (contiguous runs, a match at the start of a model id or after `/ - _ .`) come first.
Every printable key is text, including `j`, `k` and `q`. Backspace deletes. Up and Down move between
models, skipping headings and wrapping around. Enter switches exactly as `/model <account>/<model>`
does, so the choice is saved, the guardian follows only when it is on the main-model fallback, and
the model joins Recent. That holds for the current model too: Enter on it saves it as the
default, for when it came from `--model` or the settings file no longer names it. Esc closes the
overlay without switching.

An account lists its models from its `models` setting when it has one, and from the provider
otherwise. The first time the overlay opens in a session it asks the accounts that have to be
asked, all at once and for at most 10 seconds each, and says `listing models…` while it waits.
Successful listings are kept for the session, until a credential change for that account drops its
entry; an account that failed, or was added since, is asked again the next time the overlay opens,
so a credential added in the meantime shows up. An account that cannot be listed
keeps its heading and shows why (`could not list: ...`), for example a missing credential, a
provider error or the timeout. A row an account lists but refuses, such as a `claude-subscription`
model with no listed model of its name pattern, is drawn dim under the account's models as
`<id> — unavailable: <reason>`. The same filter matches it, and Up, Down and Enter skip it, so it
can never be picked. A group keeps its heading when only its unavailable rows match the filter.
`/guardian-model`'s overlay and the provider dialog's model picker show these rows too. Switching
to a model whose facts are derived from a listed model of the same name pattern shows a one-time
warning in the conversation (see [model discovery](configuration.md#model-discovery)).

`/guardian-model` with no argument opens the same overlay over a live listing of every account, as
`/model` does, failures included. A pick pins the guardian and saves it as `[guardian] model`; a pick
the account cannot serve (no credential, say) is still installed and saved, and reported as an
unavailable guardian.

## Settings panel

`/config` (alias `/settings`) opens an overlay above the message box that lists every settings key,
one row each: the key, its effective value with defaults filled in, and the scope that supplied it.

```
 config ────────────────────────────────────────────────────────────────
  Settings · key, effective value, source
 > tui.theme                    dark-ansi                      default   ✎
   tui.screen_reader            unset                          default   ✎
   tui.show_reasoning           true                           default   ✎
   tui.notify                   bell                           default   ✎
   context.autocompact_window   150000                         project-local ✎
   permissions.default_mode     unset (interactive: auto, ...  default
   permissions.deny             3 entries                      user, project
   accounts                     work, local                    user
 ───────────────────────────────────────────────────────────────────────
  filter: ▏
  type to filter · ↑/↓ select · Enter change · Del reset · Esc close
```

The source column is `default`, `user`, `project`, `project-local` or `cli`. A scalar shows the
scope that wins. A list key (`permissions.deny`, `hooks.pre_tool_use`, ...) shows every scope that
contributed, highest first, and its value is the entry count. A named table (`accounts`,
`mcp.servers`, `agents.types`, `env`, `telemetry.resource_attributes`) is one row listing the entry
names. `[env]` shows names only, never values; `statusline.command` and `guardian.scanner_command` show
as `<program> (+N args)`, `guardian.composition.script` as `set` and `guardian.composition.params`
as `<hidden>` (keys only); and a proxy or endpoint URL is shown without its
credentials. The column names config scopes only: a value set by a command-line flag or an
environment variable (`--model`, screen-reader mode) is not labelled as such.

Typing filters the rows by key, space included. Up and Down select and wrap around, and Esc closes.

Eleven rows can be changed, marked `✎`. The rest are read-only: guardian, permission, account,
network, telemetry, `env`, hook and MCP keys change what the guardian and the permission pipeline
allow or send data off the machine, so they take a deliberate edit of the settings file. Rule
editing stays in the files too; see [Permissions](permissions.md).

| Key | Enter | Takes effect |
|---|---|---|
| `tui.theme` | Cycles the themes | At once, as `/theme` |
| `tui.screen_reader` | Cycles `true`, `false`, unset | From the next launch |
| `tui.show_reasoning` | Cycles `true`, `false` | From the next launch |
| `tui.editor_mode` | Cycles `emacs`, `vim`, unset | From the next launch |
| `tui.notify` | Cycles `bell`, `osc9`, `osc777`, `off`, unset | From the next launch |
| `context.autocompact_window` | Opens a field for a positive whole number; empty clears the key | From the next launch |
| `schedule.enabled` | Cycles `true`, `false` | From the next launch |
| `memory.auto` | Cycles `true`, `false` | From the next launch |
| `shell.max_timeout_secs` | Opens a field for a positive whole number; empty clears the key | From the next launch |
| `model.model` | Closes the panel and opens the [model picker](#choosing-a-model) | At once, as `/model` |
| `model.effort` | Read-only; shows the level the session runs at, which `--effort` and `/effort` can move off the setting | From the next launch |
| `guardian.model` | Closes the panel and opens the guardian picker | At once, as `/guardian-model` |

Del removes the key from the user file (a reset); resetting `tui.theme` also switches to the
theme the other settings files give, or the default theme when none sets one. Del does nothing on the two model rows, since removing `model.model` would send the
next launch to the setup screen; and it does nothing on read-only rows.

A change is always written to your user settings file, with its comments and layout kept, and never
to a project or project-local file. The file is checked before it is written: a value that would
make it fail to load, such as `autocompact_window = 0`, is refused with the reason and the file is
left alone. When a higher scope sets the same key, the panel still saves to the user file and says
which scope wins: for a live row `applied now; <scope> sets this key and wins from the next launch`,
for a next-launch row `saved, but <scope> sets this key and wins, so the value stays <value>`.
A next-launch change that is not overridden shows `(next launch: <value>)` beside the current
value, and keeps showing it if you close and reopen the panel in the same session; Enter on such a
row cycles on from the saved value. Without a user settings file nothing is saved and the panel
says so.

With screen-reader mode on, the panel prints one plain line per key (`tui.theme: dark-ansi, from
default, editable`) and no box drawing.

## Managing providers

`/login` opens the provider dialog above the message box. It lists every configured account with
its kind, its endpoint (the account's `base_url`, else the kind's own endpoint) and what it
authenticates with:

```
 providers ─────────────────────────────────────────────────────────────
  go          opencode-go        https://opencode.ai/inference/go   signed in (me@example.com)
 > local      ollama             http://localhost:11434             no key needed
  work        anthropic          https://api.anthropic.com          key stored
  + add account
 ───────────────────────────────────────────────────────────────────────
  Enter log in · m pick model · o log out · d remove · a add · Esc close
```

The state column is `key stored`, `key from $VAR` or `$VAR is not set` for an account with
`api_key_env`, `signed in (<email>)`, `not signed in` for an account that needs a key or sign-in
and has none, `no key needed` for an `ollama` or `openai-compatible` account without one, and `login owned by the external CLI (experimental)` for a `claude-subscription` account. It
is what resolving the account would do, so a key left behind by a changed `kind` counts as not
signed in.

| Key | Action |
|---|---|
| Up, Down | Select an account, or the add row. |
| Enter | Log in to the selected account. On the add row, add an account. |
| `a` | Add an account. |
| `m` | Pick a model of the selected account, in the [model picker](#choosing-a-model). |
| `o` | Log out, after a confirmation. |
| `d` | Remove the account, after a confirmation. |
| Esc | Close the dialog. |

**Adding an account** asks for the kind, a name and, for `openai-compatible` (required), `ollama`
and `opencode-go` (optional), a base URL. A name is letters, digits, `-` and `_`, so it can always
be typed as `/login <name>` or in `<account>/<model>`. The account is written to the
`[accounts.<name>]` table of your user settings file; the rest of the file keeps its comments and
layout. Any credential already stored under that name is deleted first, so a leftover is never
sent to the new account's endpoint. The dialog then goes straight to the account's log-in; for a
keyless kind, Enter skips it. When the log-in is done the dialog closes and the model picker opens
on the new account's models. `/login` never switches the model by itself: you choose one, or press
Esc to keep the current model.

**Logging in** to a key-based account opens a key prompt. What you type or paste is drawn as `•`
and goes straight to `credentials.toml`; it never enters the message box, the history, the
transcript, the ledger or the logs, and only a summary line (`stored the key for work`) is shown.
If the account sets `api_key_env`, the prompt says the stored key is ignored while the variable is
set. Logging in to an `opencode-go` account starts the device sign-in inside the dialog: it shows
the verification URL and the code, waits for you to approve it in a browser, and stores the token
pair. Esc cancels the wait.

Logging in to a `claude-subscription` account never opens a key prompt and never starts a login.
It runs the checks of the external Claude CLI (installed, supported version, signed in on a Max or
Ultra subscription, models listed) and then opens the model picker on its models; when a check
fails the dialog says to run `claude auth login` outside Fontaine. See
[Claude subscription](claude-subscription.md).

A new credential takes effect in the running session. If the main model is on the account it is
rebuilt on the new credential, an unavailable guardian on the account is tried again, and the
account's cached model listing is dropped. The summary line says which of these happened.

**Logging out** removes the stored credential from this machine; nothing is revoked at the
provider. For a `claude-subscription` account there is nothing to remove, and the CLI's own login is
never changed: the dialog says to run `claude auth logout` outside Fontaine, and removing the
account leaves that login alone too. A main model that is already running on the account keeps the key it was built with until
you switch models or restart (an `opencode-go` account stops working at its next token refresh),
and the summary line says so. A guardian on the account becomes unavailable, so the calls that need
it are denied until it has a credential again.

**Removing** deletes the account's table from your user settings file and its stored credential. It
is refused while the live main model, the live guardian or a `[guardian.composition]` read model
uses the account, while your user file's `[model] model`, `[guardian] model`,
`[guardian.composition] model` or a `[guardian.composition.reads.*] model` names it, and for an account defined in a settings file other than your user file.

### When no model is ready at startup

If the terminal UI cannot resolve its main model at startup (no `[model] model` and no `--model`,
an unknown account, a missing credential or a provider that refuses the model listing), it shows
the same dialog before the session starts, with the reason above it, instead of exiting. Add an
account, log in and pick a model: the model is resolved again after every account added, log-in
and pick, and the session starts as soon as one works. A picked model is saved as `[model] model`
and replaces `--model` for that start. Esc leaves the screen and exits with the original error and
exit code. A credentials file that cannot be parsed still exits at once, and headless runs never
show the dialog.

## Requests for input from an MCP server

An [MCP server](mcp.md#requests-for-input) can ask you for input while a tool runs. A dialog shows
"MCP server `<name>` asks for input", the server's message and one row per field: its title (or
name) and description, a mark when it is required, and a text field, a checkbox or a list of
choices. Text, number and whole-number fields start with the server's default.

| Key | Action |
|---|---|
| Tab, Shift+Tab, Up, Down | Move between fields. |
| Left, Right | Move among the choices of a list. |
| Space | Toggle a checkbox, pick the highlighted choice of a single-choice list (again to clear an optional one), or toggle it in a several-choice list. |
| Enter | Submit the answer. While a field is invalid the dialog shows the reason inline and nothing is sent. |
| Esc | Cancel: the server is told you stopped it. |
| Ctrl-D | Decline: the server is told you chose not to answer, and carries on. |

The footer reads "This goes to the server, not to the model. Don't enter secrets you wouldn't give
this server." Pasted text goes into the focused field. The grace period of the other dialogs applies,
so keys typed ahead cannot answer. Turning on [AFK mode](afk.md) or interrupting the turn cancels an
open dialog, and so does the end of the call that caused it. A request from a
subagent's call never shows a dialog.

## MCP server approvals

A repository-defined [MCP server](mcp.md#servers-a-repository-defines) in a trusted folder is asked
about before it starts: `y` starts it and remembers the answer for this exact definition, `n`
leaves it off for this session. Servers from your own settings start without a question; a server
that cannot start is reported in the transcript and skipped.

## The task panel

When the model keeps a task list with the [`todo_write` tool](tools.md), it shows above the message
box. Collapsed it is one line, `Tasks 3/7 · ▶ the item in progress`; expanded (Ctrl-T or `/todos`)
it lists every item, marked `[ ]` pending, `[~]` in progress or `[x]` completed. A list taller than
the screen allows ends with a `+N more` line, and an approval prompt shrinks before the list does.
The panel is hidden while the list is empty. The transcript still shows each `todo_write` call as a
one-line entry with its counts. The [agent list](#the-agent-list-and-the-agent-view) takes its rows
from the same room first, so an expanded task list gives way to it when the screen is short.

The list is the main agent's; a subagent's list is not shown. It is kept with the session, so a
resumed or forked session shows it again, and `/clear` empties it. In screen-reader mode there is
no panel: the whole list is printed as plain `[ ]`, `[~]` and `[x]` lines each time it changes.

## The transcript view

Ctrl-O (or `/transcript`) shows the conversation on screen in full: each tool call with every row of
its command or arguments, all the output Fontaine kept (at most 64 KiB per call), the whole of the
model's reasoning, and the whole reason behind each denial, where the transcript folds them to a few
rows. Messages, replies, notices and errors appear as usual. It shows the entries of the
conversation on screen, so inside a subagent's transcript it shows that agent's.

The view is drawn in the live area above the message box, like the agents panel. Closing a view that
was tall enough to push the transcript into scrollback writes the transcript again, and that
rewrite erases the scrollback first, as Ctrl-L does. It opens at the end of the
conversation and follows it while you stay at the bottom, so a streaming reply stays in view;
scrolling up holds the window where it is as entries are added, and scrolling back to the bottom
follows again.

| Key | Effect |
|---|---|
| Up, Down | Scroll by a row. |
| PageUp, PageDown | Scroll by a page. |
| Home, End | Jump to the start or the end. |
| Ctrl-O (`transcript`), Esc, `q`, Ctrl-C | Close the view. Ctrl-C closes it and does not interrupt the turn. |

Other keys are ignored while the view is open: the message box stays on screen, dimmed and without
a cursor, and typing and pastes do nothing. An approval or a question that arrives takes the view's
place; the view is still open under it and returns once it is answered. The agents panel and the
pickers likewise take the keys when they are open. Ctrl-O does nothing while an approval or a question is up, so it neither opens a view
under it nor closes one that is open. `/transcript` is also accepted mid-turn.

In [screen-reader mode](terminal.md#screen-reader-mode) no view opens: Ctrl-O and `/transcript`
print every settled entry once, calls and reasoning in their `/expand` form, or `no transcript` when
there is nothing yet.

## Scheduled prompts

While [AFK mode](afk.md) is blocked or stopped, no scheduled prompt fires and no notification turn
starts; the tasks stay listed and are not deleted, and fire again once the session can run turns.

The terminal UI can run a prompt again later, while the conversation is idle. There are two ways
to set it up, and both end up as tasks in the same per-session schedule.

- **`/loop`**, typed by you:
  - `/loop 5m check the build` (or `/loop check the build every 5 minutes`) runs the prompt now,
    then at a fixed cadence. Units are `s`, `m`, `h` and `d`, or spelled out (`5min`, `2hours`). Only even cadences are accepted: a
    minute step that divides an hour (1, 2, 5, 10, 15, 20 or 30), a whole number of hours that
    divides a day (1, 2, 3, 4, 6, 8 or 12), or 1 to 31 days. Seconds round up to a whole minute
    first, and any other interval is rounded to the nearest accepted cadence, the shorter one when
    two are equally near (`7m` becomes 5 minutes, `90m` becomes 1 hour). The transcript states the
    cadence it chose. Cadences follow the wall clock (`5m` fires at :00, :05, :10 and so on), not
    the moment you typed the command, and a multi-day cadence restarts at each month's start.
  - `/loop check the build` with no interval is a self-paced loop. It runs the prompt now, and the
    model ends each iteration by calling `schedule_wakeup` to say when the next one should run,
    or to stop. If an iteration schedules nothing, the loop tries once more 20 minutes later and
    ends if that one schedules nothing either. Only one self-paced loop runs at a time; Esc at the
    idle prompt while it waits, or `/loop stop <id>`, ends just the loop; a bare `/loop stop`
    cancels every task as well. A self-paced loop is not restored when the session is resumed.
  - `/loop list` shows the tasks and `/loop stop <id>` cancels one; `/loop stop` cancels all of
    them. `/loop` alone, or with an interval and no prompt, prints its usage. The text after the
    interval is a plain prompt; a skill name there is not expanded.
- **The model's tools**, [`cron_create`, `cron_jobs`, `cron_delete` and `schedule_wakeup`](tools.md),
  which the model uses when you ask for something to happen later or repeatedly. `cron_create`
  takes a five-field cron expression in your local time: `*`, values, ranges, steps and lists, with
  Sunday as 0 or 7, and either day field matching when both are restricted. Names such as `MON`
  and the `L`, `W` and `?` forms are not supported.

A task runs only when the conversation is idle. One that comes due during a turn waits and runs once
afterwards, however many runs it missed. The turn it starts is announced in the transcript as
`scheduled: <id>` and carries a note saying it is a scheduled task and not you; it is not a user
message, so what you asked for earlier stays the only intent the guardian reads. A draft you were
typing stays in the input, and messages you queue while it runs are sent as the next turn.

A session holds at most 50 tasks and a prompt is at most 2,000 characters. A recurring task
expires 7 days after it was created: it runs one last time and is removed. A self-paced `/loop` ends 7 days after it started, with no further iterations. Tasks are saved with
the session. Resuming drops tasks that expired or whose time passed while it was closed, and a pending
wakeup of a self-paced loop, tells you how many (a dropped wakeup counts as expired), and re-arms the rest from now without replaying missed runs. `/clear` cancels every
task, because it drops the intent they were scheduled under. A headless run does not
have them, and `[schedule] enabled = false` turns them off; see
[Configuration](configuration.md#schedule). There is no random delay: a task runs at its scheduled
minute.

## Goals

`/goal <condition>` keeps the session working without a prompt per step. It starts a turn at once
with the condition as the directive. After each turn a separate model, the guardian's, reads the
condition and the conversation and answers *met*, *not met* or *impossible* with a reason:

- *Not met* starts another turn. The transcript shows `goal: not met — <reason>` and the next turn
  is told the reason.
- *Met* ends the goal: `Goal achieved: <condition> (3 evaluations, 4m, 182.0k tokens)`.
- *Impossible* ends it too, with the reason: `Goal impossible: <reason>`.

The condition is at most 4,000 characters and states something the transcript can show, such as "the
tests in `src/parser` pass and `cargo clippy` is clean": the evaluator calls no tools and judges
only from what the conversation displays. Setting a new goal replaces the old one.

- `/goal` alone shows the condition, how long it has run, the evaluations, the tokens spent and the
  last reason, or `No goal set` and the last goal that was achieved.
- `/goal clear` (also `stop`, `off`, `reset`, `none`, `cancel`) removes it. `/clear` removes it too.

A goal turn is announced by its verdict and carries a note saying it is a goal turn and not you. It
is not a user message, so what you typed in the `/goal` line stays the only intent the guardian
reads, and the note says it does not widen what you asked. Calls a goal turn makes go through the
same permission pipeline and mode as any other; manual mode still asks. The evaluator's reason is
model output derived from tool results, so it passes through the tool-result scanner and is cut to
1,000 characters before the main model reads it.

**Pausing.** A goal runs unattended, so every stretch is bounded. The goal pauses, staying set,
when:

- `[goal] max_turns` goal turns (50 by default) have run since you last sent a message;
- the spend of the stretch passes `[goal] max_cost_usd`, when that is set and the model is priced;
- 8 verdicts in a row came after turns that called no tool;
- a turn fails or is interrupted with Esc, or Esc interrupts the evaluation; or
- the evaluator gives no verdict (an error, a timeout, an unreadable answer, or a guardian model
  that is unavailable).

The transcript then says `Goal paused: <why>. Send a message to continue.` The next message you
send starts a fresh stretch and the goal is evaluated again after its turn. Text you type while a
goal turn or an evaluation runs is queued as for any turn, and is sent as your next turn ahead of
the goal's.

**AFK.** In [AFK mode](afk.md) a goal still pauses itself the usual ways, and also when the
session is *blocked* by repeated denials (`Goal paused: the denial breaker tripped while the user
was away`) or *stopped* by the temperature ceiling (`Goal paused: the AFK temperature ceiling
stopped the session. Run /afk review to continue.`). A stop's pause survives a message; only
`/afk review` lifts it. While the session is blocked or stopped, no goal evaluation or goal turn
starts.

A goal is saved with the session (`goal.json`) and comes back on `--continue`, `--resume` and
`--fork-session` with its counters reset: the clock, the evaluations and the spend baseline start
again. While subagents run, the goal waits and is evaluated after the turn that reads
their reports. Headless runs accept `-p "/goal <condition>"`; see
[Headless runs](headless.md#goals). The limits are under [Configuration](configuration.md#goal).

## The agent list and the agent view

While a [subagent](agents.md) runs, a list of agents shows below the status bar: `main` first, then
one row per subagent with its [name](agents.md#naming-a-subagent) and its id dimmed after it, type, the model and reasoning effort it runs at, status, how long this run has gone on (followed by ` · `) and what it has used,
for example `◯ survey-tech-debt (agent-3) worker acct/sonnet effort high running 1m02s · 1.5k tok`. The model and `effort <level>` are left out when unset. On a narrow terminal the id is cut first, then the model and effort together, before the status and the age. A nested agent is indented under its
parent. The row on screen is marked `●` and the others `◯`. The list shows every running agent, the
agent on screen, and an agent that failed, was killed or ran out of budget for 30 seconds after it
ended or until you hide it with `x`. An agent that finished normally leaves at once, except an
[idle](agents.md#agent-lifecycle) one that owns a task in progress, which stays listed as `idle`. With nothing
to list, the list is not drawn. `/clear` empties the list at once: an agent that ended before it
is pruned with the conversation instead of staying listed. The spend uses the same format as the
[agents panel](#the-agents-panel), including the `N unpriced tok` segment: on a narrow terminal the
leading columns are cut before it, so the unpriced figure stays visible.

While an agent's model is writing a reply, its reasoning or a tool call's arguments, its row ends in
`· writing 4.2k chars` (counts are plain below 1000, then in thousands `4.2k` or millions `1.2M`, without a trailing `.0`), so a long model step
does not look like a stall. `main` shows its own count the same way, while the list is shown.
Usage and cost still update only when the model call ends; the count is characters, not tokens,
and clears when the step ends, fails or is interrupted. A paused model leaves the last count until
its next output. On a narrow terminal the count is left out before anything else is cut.

Down on the last line of the input moves the keyboard into the list, on `main`. Up and Down select a
row, Enter shows that row, `x` stops a running subagent (or hides an ended one), and Esc, Up on the
first row, or any printable key gives the keyboard back to the input. A hint line above the rows
lists these keys while the list has the keyboard.

**Viewing an agent.** Enter on a subagent erases the screen and the terminal's scrollback and
writes that agent's transcript: its task as the first message, then what it said and each tool
call, drawn like the main chat and streaming as the agent works. The input box's border names the
agent (`survey-tech-debt (agent-3) · worker · running`) and the empty input reads `Message survey-tech-debt (agent-3)…`. The wheel and
tmux copy mode scroll the transcript like any other. The agent's own denials show there, as the
main chat's show, and not in the main chat; the tier and rule of what a subagent was allowed are in
the audit log, as for the main conversation. Selecting `main` and pressing Enter erases the screen
again and writes the whole main chat, including what happened while it was hidden, with none of the
agent's lines in it. Approval prompts stay global: any agent's prompt opens over whichever
transcript is on screen and names the agent.

**Typing to an agent.** What you type in an agent's view goes to that agent, not to the main
conversation:

- A running agent reads it with its next tool result, as the main agent does with
  [messages during a turn](#messages-during-a-turn). Until then it shows dimmed under the
  transcript, and then as a `>` line of its own.
- An idle agent that has not been retired is resumed with it: another run starts on the same
  conversation, with the same tools, mode and budget, and the main conversation shows
  `you resumed agent-3`. The new run's report reaches the agent that started it, or the main
  conversation when that agent has ended, like any other report. A message that a run ended
  without reading resumes the agent the same way once it ends.
- An agent that was retired — by `/clear`, a conversation rewind or a final ancestor, as in the
  [agent lifecycle](agents.md#agent-lifecycle) — cannot be resumed; the view says so and the
  text stays in the input. Resuming an unretired agent holds until it leaves the list, which
  `/clear` does by pruning it, along with its place in the list and the panel.
- An agent that was killed or whose budget is spent cannot be resumed; the view says so and the text
  stays in the input. A resume that would exceed the parallel-agent limit or the session's agent
  budget is refused the same way.

Built-in commands that only show or set something run as usual against the main conversation, and
their notices appear in the view. Commands that act on or change the main conversation (`/btw`,
`/clear`, `/compact`, `/rewind`, `/branch`, `/resume`, `/model`, `/effort`, `/goal`, `/loop`, `/init`, `/output-style` and `/plan`
with an argument), skills and `!` lines are refused there with `switch back to main to run this`,
and the text stays in the input. `/expand` and `/copy` act on the transcript on screen. Ctrl-C
interrupts the main turn, which keeps running while you look at an agent; Esc does not.

In [screen-reader mode](terminal.md#screen-reader-mode) there is no list, and the same switch
prints a `viewing agent-3 (worker)` or `back to the main conversation` line and then only what was
not printed yet.

## The agents panel

`/agents` (or `/tasks`) opens the full overview: one row per [subagent](agents.md), ended
ones included until a `/clear` prunes them, and below it one row per [background job](tools.md#background-jobs) of the main conversation (its id, its state, its age and the start of its command): an agent row shows its name with its id dimmed after it (`survey-tech-debt (agent-3)`), its type, its
model, its status (`running`, `waiting-on-permission`, `idle`, `done`, `killed`, `budget-exhausted` or
`failed`; `idle` is an agent that finished its turn and can be woken; `done` is final) and what it has used, such as `1.5k tok · $0.0021`; each `/ <cap>` form
(`1.5k / 200k tok`, `$0.0021 / $2.00`) appears only for a budget that is set, and `cost n/a`
shows in place of the spend when a cost budget is set but no priced call has been counted yet,
as for a model with no known price. Tokens that carried no price (an unpriced account's main model next to its priced guardian
calls) are split out: the priced tokens sit next to their price and the rest follow as
`N unpriced tok`, as in `110k tok · $0.0066 · 128k unpriced tok`. The dollar figure is a lower
bound, and a cost budget still counts only the priced spend. With a token budget the first figure
stays the total, since the cap binds on it (`238k / 500k tok · $0.0066 · 128k unpriced tok`). An
agent with no priced call reads `4.9M unpriced tok`, or `cost n/a · 4.9M unpriced tok` under a cost
budget. Displayed counts are compact (`999`, `1.2k`, `1.2M`), and agent spend and writing counts
drop a trailing `.0` (`200k`, not `200.0k`); the ledger, the audit
log and JSON output keep exact values. Up and Down select a row; Enter on an agent closes the panel
and shows that agent's transcript, which is how you reach an agent that has left the
[agent list](#the-agent-list-and-the-agent-view). A running agent's spend line also ends in
`· writing 4.2k chars` while its model is mid-step, as in the list; it clears when the step ends.

With [task boards](agents.md#task-boards) on and tasks on a board, a **Board of \<name\>** section
per board owner below the jobs lists each task: its id, status and title, then its owner's name (or `nobody`) and the
tasks blocking it. It is for reading only, and the title is shown with control characters taken
out, since an agent wrote it. `/tasks` stays an alias of `/agents`; the board is not the
[task panel](#the-task-panel) of `todo_write`.

`x` kills the selected subagent: its row shows `killed` and what it had spent, and its pending
approval prompts are cancelled as denied. On a job row it stops the job and its process group,
and the main conversation is told in a note that the job was killed by you; Enter does nothing on a
job row. Esc closes the panel. While the panel is
open it has the keyboard, so a pending approval waits until you close it. Closing a panel that
was tall enough to push the transcript into scrollback writes the transcript again, so the last
messages are on screen.

When a subagent finishes while nothing is running, the main conversation gets a turn of
its own to read the report. A background job that ends, or a monitor that prints lines, does the
same: the transcript says `an agent reported back` or `a background job reported back`
(a job note wins the label when both wait at once), or `tasks on your board are waiting for
agents` for a reminder about its task board, and the turn reads the notice,
which names the output as untrusted data. `/clear` and a `/rewind` that restores the conversation stop running jobs and discard their
notices; `/branch` is refused while a notice is waiting.
