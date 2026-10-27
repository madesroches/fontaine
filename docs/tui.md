# The terminal UI

`fontaine` without `-p` starts the terminal UI. It needs a terminal: with piped standard input or
output it exits with code 2 and points at `fontaine -p`. Configuration, accounts and permission
rules work as in headless runs; the mode is the one from `[permissions] default_mode` or
`--permission-mode`. With neither set, the terminal UI starts in `auto`, the mode where the
[guardian](guardian.md) judges each call that nothing pre-approved. Newline keys, tmux and macOS
settings, colour themes and screen-reader mode are on [Terminal setup](terminal.md).

The UI draws inline, not on the alternate screen, but it takes the whole terminal when it starts:
the shell's earlier output scrolls into the terminal's history. The transcript reads top-down, with
the input box and status bar on the bottom rows, and this holds across resizes. A live area down to
the bottom row holds the input box, the status bar, any open prompt and the part of the transcript
that still fits above the input. A short transcript, typically just the startup warnings, sits on
the rows directly above the input box with empty space above it. Once the transcript no longer
fits, the oldest entries move into the terminal's normal scrollback, and leaving the UI (or
`/clear`) writes whatever was still held back there too, so the conversation stays in the
terminal. Scroll back with the terminal or with tmux copy mode, and select text with the mouse as
in any shell program.

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
more were cut. An `edit`, `write` or `notebook_edit` call shows its diff instead (a `path  +added -removed` header,
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
touch the model's context. In screen-reader mode a call is still one labelled line, followed by
the last ten lines of its output as plain lines and the same `expand:` hint.

A long command under approval scrolls inside
half the screen, so the call it asks about stays visible above the input.

### Notices

Notices are the lines Fontaine adds itself. An informational notice is a dim line starting with
`·`. A warning starts with `⚠` and shows its headline in the theme's warning colour, with what to
do about it on dim lines indented underneath; text in backticks and `/commands` in that hint are
bold, so the actionable part stands out without relying on colour. Warnings open the conversation:
a guardian that runs on your main model, a `[guardian] profile = "minimal"` that is not in effect,
a tool-result scanner that is unavailable, an additional directory that was skipped and an ignored
`FONTAINE_AX_SCREEN_READER` value. Set `FONTAINE_ASCII_GLYPHS=1` to use `*` and `-` in place of
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

The bar shows the permission mode, the model, how many subagents are running, `⏱ N` when `N` [scheduled tasks](#scheduled-prompts) exist, `◎ goal 12m` while a [goal](#goals) is active (`◎ goal paused` when it waits for you), and a key hint. The mode labels are `manual`,
`⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ auto mode on` and `dontAsk`.

## Keys

| Key | Action |
|---|---|
| Enter | Submit. Alt+Enter, Shift+Enter or Ctrl+J insert a newline; a trailing `\` before Enter also continues the line. Which of these your terminal sends, and how to bind Shift+Enter, is in [Terminal setup](terminal.md#new-lines-in-the-message-box). While a turn runs, a message is queued (see [Messages during a turn](#messages-during-a-turn)). With a `@` list open, Enter completes instead; with the [slash-command menu](#slash-command-menu) open, it runs the selected command. |
| Up, Down | With the `@` list or the slash-command menu open, move the selection. Otherwise, on the first or last line of the input, recall older or newer submitted inputs; elsewhere they move the cursor. With messages queued, Up takes them back into the input. |
| Ctrl-R | Search the prompt history; see [History](#history). |
| Tab | Completes the `@` file under the cursor, or the command name in the slash-command menu, when a list is open. |
| Shift+Tab | Cycle the mode: manual, accept edits, plan, auto, and back to manual. It takes effect on the next tool call, even mid-turn. Entering `auto` lists the broad allow rules it sets aside and says so when the guardian cannot be asked. |
| Esc, Ctrl-C while a turn or a `!` command runs | Interrupt it. The interrupted turn is discarded, earlier turns are kept, and an open prompt is answered with deny. An open history search, `@` list or slash-command menu is closed first. |
| Esc when idle and a self-paced `/loop` waits for its next iteration | Ends the loop; see [Scheduled prompts](#scheduled-prompts). |
| Esc twice in quick succession when idle with an empty input | Opens the `/rewind` picker; see [Checkpoints and /rewind](checkpoints.md). Any other meaning of Esc above wins first. |
| Ctrl-C, or Ctrl-D on an empty input, when idle | Quit. |
| PageUp, PageDown | Scroll the command or diff in an open approval prompt. |
| Arrows, Home, End, Backspace, Delete, Ctrl-A/E/U/K/W/B/F | Edit the input. |
| Ctrl-T | Open or close the [agents panel](#the-agents-panel), even over an approval prompt. |
| Ctrl-O | Expand or collapse the [task list](#the-task-panel). Ignored while the agents panel is open. |
| `/` as the first character | Opens the [slash-command menu](#slash-command-menu). |

## Slash-command menu

Typing `/` as the first character lists the commands and skills, and narrows the list as you type
the name. Names and aliases that start with what you typed come first, then those that merely
contain it; the match ignores case. A row found through an alias shows it in parentheses, as in
`/clear (reset)`. Up to eight rows are shown, scrolling to keep the selection visible.

| Key | Action |
|---|---|
| Up, Down | Move the selection. |
| Tab | Complete the selected name, with a trailing space when the command takes an argument. An alias becomes the command's own name. |
| Enter | Run the selected command. A command with a required argument, shown as `<arg>` in its hint, is completed instead, since it cannot run without one. |
| Esc, Ctrl-C | Close the menu until the text changes. |

The menu only opens while the input is a bare `/name`: it closes once you type a space, and it
never opens over a panel or prompt, during a history search or beside a `@` list. A recalled
history entry that starts with `/` does not open it, so Up keeps walking the history.

## Shell commands

An input that starts with `!` is a shell command. The input box title changes and its border is
coloured; Enter runs the command instead of sending a message, and the `!` is not part of it.

- It runs as you, in the primary workspace root, with no approval prompt: it is your own action, so
  the [permission pipeline](permissions.md) and the [guardian](guardian.md) do not see it.
- It runs with the same environment (scrubbed, plus [`[env]`](configuration.md#env)) and the same
  output cut as the agent's `shell` tool, because the output goes to the agent. It is killed after 120 seconds, or after 60 seconds without
  output, and has no standard input.
- It runs only between turns. While a turn runs, Enter on a `!` input says so and keeps the text.
  While a command runs nothing else can be sent or queued; Esc or Ctrl-C interrupts it and kills
  its process group.
- The output is shown in the transcript and handed to the agent with your next message, marked as
  output you produced and as data, not instructions. An interrupted command tells the agent nothing.
- Each command is written to the session ledger and to the audit log, an interrupted one included,
  since it may already have had effects. It never counts as one of your messages to the agent.

## File mentions

Typing `@` after whitespace, or at the start of the input, opens a list of up to eight workspace
files that match what follows it, best matches first (file name prefix, then file name substring,
then shorter paths). Up and Down select, Tab or Enter put `@path ` in the input, and Esc closes
the list until you edit the token. Files come from the same index the session starts with, which
respects `.gitignore`, and is refreshed when a turn ends and after `/add-dir`. Paths under the
primary root are shown relative to it.

Completion only edits your input. The message is sent as you typed it, and the agent reads the
file through its `read` tool, so the content passes the same scope checks, deterministic floor and
guardian as any other read.

## History

Every input you submit, `/` commands and `!` commands included, is kept in `history.jsonl` in the
[data directory](configuration.md#files-in-these-directories), readable by the current user only.
It holds what you typed, so treat it like your shell history. The file is shared by all projects;
each line records the primary root it was typed in, and only this project's lines are recalled. It
keeps the newest 5,000 lines. A file that cannot be read or written is reported once and the
history then lasts for the session only.

- Up on the first line of the input shows older entries, Down on the last line shows newer ones,
  and going past the newest restores what you had typed. Editing a recalled entry stops the walk.
- Ctrl-R opens a search above the input. Typing narrows the match (case-insensitive), Ctrl-R again
  moves to the next older match, Enter or Tab put the match in the input without sending it, and Esc
  cancels.

## Messages during a turn

Enter on an ordinary message while a turn runs queues it. Queued messages show above the input as
`queued: <first line>`. They are not sent at once: the agent receives them with its next tool
result, appended to that result after a line saying the user sent the message while it was working,
and the transcript then shows the message as a `>` line at that point. Only then do the words count
as yours for the [guardian](guardian.md).

- Up on the first line takes the queued messages back into the input, for editing.
- If the agent answers without another tool result, the queued messages are sent as the next turn.
- If the turn is interrupted, fails or hits its turn limit, undelivered messages return to the
  input, in front of whatever is there, and nothing is sent. Messages already delivered in that turn
  are discarded with it, like its prompt, and must be sent again.
- `/` commands are not queued; Enter on one says a turn is running.

## Approval prompts

When a call needs approval, a prompt lists the call and why it asked:

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
for a path outside the workspace roots, for a file that is missing (`edit`, `notebook_edit`), not UTF-8 or larger
than 1 MiB, and for an edit that matches nowhere or more than once. A `notebook_edit` diff is the cell's source before and after, not the notebook's JSON. Control characters in file
content are blanked and credential-shaped text is redacted before it is shown.

When several agents ask at once their prompts queue and are shown one at a time, and a prompt that
belongs to a subagent names it: `Allow shell (agent-2)?`. A prompt whose agent was killed goes away.

Up, Down and Tab move the highlight, Enter confirms. Prompts forced by the floor (an `ask` rule, a
critical path, a protected path) and shell calls that are not one literal simple command offer
only allow once and deny. Enter and the letter keys are ignored for the first 400 ms after a prompt
opens, and key repeats never answer, so text typed ahead during a turn cannot approve a call. See [Permissions](permissions.md) for what each answer does.

## Questions from the model

The model can stop and ask you 1 to 4 multiple-choice questions with `ask_user_question`. A dialog
shows one question at a time: a header chip, the question and numbered options with their
descriptions, then an "Other" row for your own words.

| Key | Action |
|---|---|
| Up, Down, Tab, `j`, `k` | Move the highlight. |
| `1`-`9` | Jump to that row. |
| Space | Toggle the highlighted option, when the question allows several. On "Other" it opens the field for your text. |
| Enter | Choose the option (several: confirm the toggled ones) and go to the next question. On "Other", opens a field for your text. |
| Esc | Decline the whole call and discard the answers already given. Inside the "Other" field it only leaves the field. |

With several selections allowed, "Other" text is added to the toggled options. An answer needs at
least one selection or some text. Enter, Esc and the letter and number keys are ignored for the
first 400 ms after a dialog opens, and key repeats never answer, so text typed ahead during a turn
cannot answer for you. Esc here declines the question rather than interrupting the turn; Ctrl-C
still interrupts, and answers the question as declined. Only the main conversation asks: questions
queue behind each other, and behind an open approval prompt, which takes the keys first. Declining
tells the model so, and it carries on with its best judgment or asks in plain text. The words you
type under "Other" are recorded as your message, so the [guardian](guardian.md#what-the-guardian-sees) reads
them as intent; the options you pick are not.

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
continues; the model has the earlier conversation. The session starts in the mode your settings
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
first message since the last `/clear`, cut to 60 characters (a `/rewind` does not change this); it makes no model call, and is an error
when there is no message yet.

`/branch [<name>]` (also `/fork`) copies the session under a new id and carries on in the copy,
leaving the original as it was. The copy is named `<name>`, or the original's name followed by
` (branch)`, or nothing when the original has none. `/branch` is refused while a subagent is
running or has a report to deliver, since the copy would not carry the subagent's state.

`/export [<path>]` writes the conversation as Markdown. The default is `fontaine-<first 8 characters
of the session id>.md` in the project directory (the launch directory, or `--cd`); a relative path is relative to it.
The text is the model's conversation, so after a compaction the summary stands where the older
messages were (labelled as one), and after `/clear` or a `/rewind` only the conversation that remains is in it. A tool
call is one line, and a tool result appears only as its length. The file is readable by you alone,
since a transcript can hold secrets, and an existing file is an error: an export never overwrites.

## Commands

| Command | Action |
|---|---|
| `/add-dir <path>` | Adds a workspace root. The path is validated and canonicalized like `--add-dir`; file tools may reach it from the next turn, with the conversation kept. Until then a file-tool call outside the roots is denied with a hint naming this command and the session's scratch directory, which is always a root. Only between turns. |
| `/agents`, `/tasks` | Opens the [agents panel](#the-agents-panel). |
| `/branch [<name>]`, `/fork` | Copies the session under a new id and carries on in the copy; the original is left as it was. Only between turns, and refused while a subagent runs. See [Sessions](#sessions). |
| `/clear`, `/reset`, `/new` | Starts over with an empty context. See [Context](#context). It takes no name for the old conversation; `/rename` names a session, and `/resume` returns to one later. |
| `/compact [<focus>]` | Summarizes the conversation so far to free context; the optional focus tells the summary what to keep above all. See [Context](#context). |
| `/config`, `/settings` | Opens the [settings panel](#settings-panel): every settings key with its effective value and the scope that supplied it, and a few preferences you can change. |
| `/context [all]` | Shows what fills the context window. See [Context](#context). |
| `/copy [<n>]` | Copies the latest assistant reply, or the `n`th latest, to the system clipboard. A reply is the text between tool calls, so a turn that calls a tool and then answers has two; `/copy` takes the last one. It is an error when there is no such reply. |
| `/denied` | Lists the calls the guardian denied this session, with their rule label and explanation. Up and Down select; `r` approves the selected call once, as if you had approved it at a prompt, and tells the agent on its next turn that it may retry exactly that call. The retry runs without asking the guardian, never enters the verdict cache and does not count toward the denial limits. The deterministic floor still runs first, so an `ask` rule or a critical path still prompts. Esc closes the list. |
| `/exit`, `/quit` | Leave. |
| `/expand [<n>]` | Prints tool call `n` in full (command, retained output, diff) as new transcript lines; the latest call without a number. `n` is the number in the card's `expand:` hint, counted from the start of the session. It is an error for a call that does not exist or that `/clear` removed. |
| `/export [<path>]` | Writes the conversation to a Markdown file, owner-only, never over an existing file. See [Sessions](#sessions). |
| `/goal [<condition> \| clear]` | Keeps working until the condition is met; see [Goals](#goals). Bare `/goal` shows the goal's status. |
| `/guardian-model [<account>/<model>]` | Without an argument, opens the [model picker](#choosing-a-model) over a live listing. With one, pins the guardian to that model for the live session and saves it as `[guardian] model`. A model whose account has no credential becomes an unavailable guardian, which the next adjudication reports. When a higher scope (`--settings` or a project-local file) sets `[guardian] model`, the notice adds that the scope wins from the next launch. |
| `/help` | Lists the built-in commands, your user-invocable skills and the main keys. |
| `/hooks` | Lists the [hooks](hooks.md) by event with their matcher, command and scope, and marks a repository's hooks that did not load because the workspace is not trusted. |
| `/init` | Has the agent write the project instruction file, or improve the one that exists. See [Instruction files](instructions.md#init). Takes no argument. |
| `/login [<account>]` | Opens the [provider dialog](#managing-providers) to add an account, log in, log out or remove one. With an account name it opens straight on that account's log-in: a hidden key prompt, or the device sign-in for `opencode-go`. Only between turns. |
| `/loop [<interval>] <prompt>`, `/loop list`, `/loop stop [<id>]` | Runs a prompt again and again while the session is idle; see [Scheduled prompts](#scheduled-prompts). |
| `/mcp` | Lists each [MCP server](mcp.md) as `connected`, `failed` with the error, `left off` (you declined it) or `not loaded` (the workspace is not trusted). It takes no arguments. |
| `/memory [<n>]` | Lists the instruction files, numbered, with where each came from and the `FONTAINE.md` files that do not exist yet. With a number it opens that file in `$VISUAL` or `$EDITOR`, creating it first when needed. See [Instruction files](instructions.md#memory). |
| `/model [<account>/<model>]` | Without an argument, opens the [model picker](#choosing-a-model). With one, switches the live session to that model, rebuilds the agent on it with the conversation kept, and saves it as `[model] model` in your user settings file; when a higher scope sets `[model] model`, the notice adds that the scope wins from the next launch. The guardian moves with it only when it is on the main-model fallback (no `[guardian] model` and no validated model available); a pinned or validated guardian stays. |
| `/output-style [<name>\|none]` | Without an argument, lists the [output styles](extensions.md#output-styles). With one, switches to it for the next turn; `none` removes it. |
| `/permissions` | Shows the permission mode and the `deny`, `ask` and `allow` rules in force, each with the scope that contributed it (session and always grants included), the workspace roots and the settings file where rules are edited. It is read-only; see [Permissions](permissions.md). |
| `/plan [<description>]` | Switches to plan mode, as Shift+Tab does. With a description it also sends it as your next message, so `/plan fix the auth bug` plans the fix. |
| `/rename [<name>]` | Names the session. Without a name, draws one from the first line of your first message. See [Sessions](#sessions). |
| `/resume [<name-or-id>]` | Switches to another session started in this folder: the picker without an argument, that session with one. Sessions where nothing was said and that have no name are not listed. Only between turns. See [Sessions](#sessions). |
| `/rewind`, `/checkpoint` | Opens a list of this conversation's prompts to restore the code, the conversation or both to the start of one. Esc twice does the same. See [Checkpoints and /rewind](checkpoints.md). Refused while a subagent runs. |
| `/skills` | Lists every loaded [skill](extensions.md#skills) with its scope, marking those only the model can use, those only you can run, and those a built-in command hides. |
| `/status` | Shows the version, the session id, the workspace roots (the scratch directory labelled separately), the permission mode, the main model, the guardian (or why it is unavailable) with its profile, the output style, and how many MCP servers are connected. |
| `/theme [<name>]` | Without an argument, lists the colour themes, marks the current one and names the file the choice is saved to. With a name, switches to that theme at once and saves it as `[tui] theme` in your user settings file. An unknown name is an error that lists the themes. See [Terminal setup](terminal.md#colours). |
| `/todos` | Expands or collapses the [task list](#the-task-panel), as Ctrl-O does. |
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
grants and permission mode. The transcript is emptied (what was already written stays in your
terminal's scrollback), and so is the guardian's view of what you have asked, which starts again
from the next message you send; the files, actions and denials it knows about are kept.
[`/rewind`](checkpoints.md) trims the guardian's view the same way for the prompts it takes back. The old
conversation cannot be resumed: `--resume` reopens the empty one. `/clear` is refused while a
subagent is running or its report is waiting to be delivered, since the subagent's calls are judged against what you asked; wait for it
or kill it from the [agents panel](#the-agents-panel).

`/compact [<focus>]` asks the main model to summarize the conversation, keeping only the newest
reply as it is, and replaces the older part with the summary. It shows
`compacted the conversation: ~41.2k → ~6.3k tokens`, or `nothing to compact` when there is nothing
older to summarize. Esc interrupts it and leaves the conversation as it was. Messages you type
meanwhile are put back in the input box when it ends instead of being sent. The model must be one
the catalog lists. Compaction also runs on its own before a turn once the conversation outgrows
the model's window less a reserve, or earlier if you set
[`[context] autocompact_window`](configuration.md#context).

`/context` shows what the next request would carry: the system prompt, instruction files, the
skill index, the tool definitions (MCP tools apart), the messages and what is free of the window,
with the point where compaction runs on its own. `/context all` also lists each instruction file (imported files and rules included)
and tool, largest first. The numbers are estimates from a character-based counter, not the
provider's tokenizer, and they count the same way the compaction trigger does. When the model is
not in the catalog there is no `free` row and it never compacts automatically; a catalogued model
whose window is not known has no `free` row either and compacts automatically only when
`autocompact_window` is set. When the total is over the window it says so.

## Choosing a model

`/model` with no argument opens an overlay above the message box. The first line shows the current
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
provider error or the timeout.

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
names. `[env]` shows names only, never values, and a proxy or endpoint URL is shown without its
credentials. The column names config scopes only: a value set by a command-line flag or an
environment variable (`--model`, screen-reader mode) is not labelled as such.

Typing filters the rows by key, space included. Up and Down select and wrap around, and Esc closes.

Six rows can be changed, marked `✎`. The rest are read-only: guardian, permission, account,
network, telemetry, `env`, hook and MCP keys change what the guardian and the permission pipeline
allow or send data off the machine, so they take a deliberate edit of the settings file. Rule
editing stays in the files too; see [Permissions](permissions.md).

| Key | Enter | Takes effect |
|---|---|---|
| `tui.theme` | Cycles the themes | At once, as `/theme` |
| `tui.screen_reader` | Cycles `true`, `false`, unset | From the next launch |
| `context.autocompact_window` | Opens a field for a positive whole number; empty clears the key | From the next launch |
| `schedule.enabled` | Cycles `true`, `false` | From the next launch |
| `model.model` | Closes the panel and opens the [model picker](#choosing-a-model) | At once, as `/model` |
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
and has none, and `no key needed` for an `ollama` or `openai-compatible` account without one. It
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

A new credential takes effect in the running session. If the main model is on the account it is
rebuilt on the new credential, an unavailable guardian on the account is tried again, and the
account's cached model listing is dropped. The summary line says which of these happened.

**Logging out** removes the stored credential from this machine; nothing is revoked at the
provider. A main model that is already running on the account keeps the key it was built with until
you switch models or restart (an `opencode-go` account stops working at its next token refresh),
and the summary line says so. A guardian on the account becomes unavailable, so the calls that need
it are denied until it has a credential again.

**Removing** deletes the account's table from your user settings file and its stored credential. It
is refused while the live main model, the live guardian or the `[guardian.screen]` model uses the
account, while your user file's `[model] model`, `[guardian] model` or `[guardian.screen] model`
names it, and for an account defined in a settings file other than your user file.

### When no model is ready at startup

If the terminal UI cannot resolve its main model at startup (no `[model] model` and no `--model`,
an unknown account, a missing credential or a provider that refuses the model listing), it shows
the same dialog before the session starts, with the reason above it, instead of exiting. Add an
account, log in and pick a model: the model is resolved again after every account added, log-in
and pick, and the session starts as soon as one works. A picked model is saved as `[model] model`
and replaces `--model` for that start. Esc leaves the screen and exits with the original error and
exit code. A credentials file that cannot be parsed still exits at once, and headless runs never
show the dialog.

## MCP server approvals

A repository-defined [MCP server](mcp.md#servers-a-repository-defines) in a trusted folder is asked
about before it starts: `y` starts it and remembers the answer for this exact definition, `n`
leaves it off for this session. Servers from your own settings start without a question; a server
that cannot start is reported in the transcript and skipped.

## The task panel

When the model keeps a task list with the [`todo_write` tool](tools.md), it shows above the message
box. Collapsed it is one line, `Tasks 3/7 · ▶ the item in progress`; expanded (Ctrl-O or `/todos`)
it lists every item, marked `[ ]` pending, `[~]` in progress or `[x]` completed. A list taller than
the screen allows ends with a `+N more` line, and an approval prompt shrinks before the list does.
The panel is hidden while the list is empty. The transcript still shows each `todo_write` call as a
one-line entry with its counts.

The list is the main agent's; a subagent's list is not shown. It is kept with the session, so a
resumed or forked session shows it again, and `/clear` empties it. In screen-reader mode there is
no panel: the whole list is printed as plain `[ ]`, `[~]` and `[x]` lines each time it changes.

## Scheduled prompts

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
- *Met* ends the goal: `Goal achieved: <condition> (3 evaluations, 4m, 182k tokens)`.
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

A goal is saved with the session (`goal.json`) and comes back on `--continue`, `--resume` and
`--fork-session` with its counters reset: the clock, the evaluations and the spend baseline start
again. While background subagents run, the goal waits and is evaluated after the turn that reads
their reports. Headless runs accept `-p "/goal <condition>"`; see
[Headless runs](headless.md#goals). The limits are under [Configuration](configuration.md#goal).

## The agents panel

`/agents` (or `/tasks`) or Ctrl-T opens a list with one row per [subagent](agents.md): its id and type, its
model, its status (`running`, `waiting-on-permission`, `done`, `killed`, `budget-exhausted` or
`failed`) and what it has used against its budget, such as `1500 / 200000 tok · $0.0021 / $2.00`
(`cost n/a` when the model has no known price). Up and Down select a row; Enter looks inside it:

- the calls it is **waiting for you** to decide, so an approval cannot hide behind the main
  transcript;
- its **gate decisions**, with the tier and rule of each (the same records as the audit log);
- its **transcript**: what it said and which tools it called.

`x` kills the selected subagent: its row shows `killed` and what it had spent, and its pending
approval prompts are cancelled as denied. Esc goes back, and closes the panel. While the panel is
open it has the keyboard, so a pending approval waits until you close it.

When a background subagent finishes while nothing is running, the main conversation gets a turn of
its own to read the report.
