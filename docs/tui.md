# The terminal UI

`fontaine` without `-p` starts the terminal UI. It needs a terminal: with piped standard input or
output it exits with code 2 and points at `fontaine -p`. Configuration, accounts and permission
rules work as in headless runs; the mode is the one from `[permissions] default_mode` or
`--permission-mode`. With neither set, the terminal UI starts in `auto`, the mode where the
[guardian](guardian.md) judges each call that nothing pre-approved.

The UI draws inline, not on the alternate screen, but it takes the whole terminal when it starts:
the shell's earlier output scrolls into the terminal's history. The transcript reads top-down, with
the input box and status bar on the bottom rows, and this holds across resizes. Finished output goes
into the terminal's normal scrollback, and a live area down to the bottom row holds the input box,
the status bar, any open prompt and the unfinished tail of the transcript. Scroll back with the terminal or with tmux copy mode, and
select text with the mouse as in any shell program.

The input box title says what Enter does: it sends a message, or, when the input starts with `!`,
runs it as a shell command. The transcript shows your messages (`>`), the streaming answer, and one line per tool call with its
status: pending, done, failed, denied with the reason, or interrupted. A call the guardian refused
shows its rule label and the guardian's own explanation on lines of their own, for example
`[Download+Execute] pipes a download into a shell`. A line is written to scrollback once it is
settled, so a call's status is final by then.

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

The bar shows the permission mode, the model, how many subagents are running, and a key hint. The mode labels are `manual`,
`⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ auto mode on` and `dontAsk`.

## Keys

| Key | Action |
|---|---|
| Enter | Submit. Alt+Enter, Shift+Enter or Ctrl+J insert a newline; a trailing `\` before Enter also continues the line. While a turn runs, a message is queued (see [Messages during a turn](#messages-during-a-turn)). With a `@` list open, Enter completes instead. |
| Up, Down | On the first or last line of the input, recall older or newer submitted inputs; elsewhere they move the cursor. With messages queued, Up takes them back into the input. |
| Ctrl-R | Search the prompt history; see [History](#history). |
| Tab | Completes the `@` file under the cursor when a list is open. |
| Shift+Tab | Cycle the mode: manual, accept edits, plan, auto, and back to manual. It takes effect on the next tool call, even mid-turn. Entering `auto` lists the broad allow rules it sets aside and says so when the guardian cannot be asked. |
| Esc, Ctrl-C while a turn or a `!` command runs | Interrupt it. The interrupted turn is discarded, earlier turns are kept, and an open prompt is answered with deny. An open history search or `@` list is closed first. |
| Ctrl-C, or Ctrl-D on an empty input, when idle | Quit. |
| PageUp, PageDown | Scroll the command in an open approval prompt. |
| Arrows, Home, End, Backspace, Delete, Ctrl-A/E/U/K/W/B/F | Edit the input. |
| Ctrl-T | Open or close the [agents panel](#the-agents-panel), even over an approval prompt. |
| `/` as the first character | Lists the commands and skills that match what you typed. |

## Shell commands

An input that starts with `!` is a shell command. The input box title changes and its border is
coloured; Enter runs the command instead of sending a message, and the `!` is not part of it.

- It runs as you, in the primary workspace root, with no approval prompt: it is your own action, so
  the [permission pipeline](permissions.md) and the [guardian](guardian.md) do not see it.
- It runs with the same scrubbed environment and the same output cut as the agent's `shell` tool,
  because the output goes to the agent. It is killed after 120 seconds, or after 60 seconds without
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
  to `[permissions] allow` in your user settings file.
- `n` deny. Enter on the deny row opens an optional reason, which is sent to the model with the
  denial.

When several agents ask at once their prompts queue and are shown one at a time, and a prompt that
belongs to a subagent names it: `Allow shell (agent-2)?`. A prompt whose agent was killed goes away.

Up, Down and Tab move the highlight, Enter confirms. Prompts forced by the floor (an `ask` rule, a
critical path, a protected path) and shell calls that are not one literal simple command offer
only allow once and deny. Enter and the letter keys are ignored for the first 400 ms after a prompt
opens, and key repeats never answer, so text typed ahead during a turn cannot approve a call. See [Permissions](permissions.md) for what each answer does.

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
`--resume <id>` a session by id, and `--fork-session` with either starts a copy under a new id. The
transcript starts empty with a note saying which session continues; the model has the earlier
conversation. The session starts in the mode your settings give, with no grants from before. See
[Headless runs](headless.md#continuing-a-session) for the details.

## Commands

| Command | Action |
|---|---|
| `/add-dir <path>` | Adds a workspace root. The path is validated and canonicalized like `--add-dir`; file tools may reach it from the next turn, with the conversation kept. Until then a file-tool call outside the roots is denied with a hint naming this command. Only between turns. |
| `/agents`, `/tasks` | Opens the [agents panel](#the-agents-panel). |
| `/clear`, `/reset`, `/new` | Starts over with an empty context. See [Context](#context). It takes no name for the old conversation; that needs `/resume`, which is not available yet. |
| `/compact [<focus>]` | Summarizes the conversation so far to free context; the optional focus tells the summary what to keep above all. See [Context](#context). |
| `/context [all]` | Shows what fills the context window. See [Context](#context). |
| `/copy [<n>]` | Copies the latest assistant reply, or the `n`th latest, to the system clipboard. A reply is the text between tool calls, so a turn that calls a tool and then answers has two; `/copy` takes the last one. It is an error when there is no such reply. |
| `/denied` | Lists the calls the guardian denied this session, with their rule label and explanation. Up and Down select; `r` approves the selected call once, as if you had approved it at a prompt, and tells the agent on its next turn that it may retry exactly that call. The retry runs without asking the guardian, never enters the verdict cache and does not count toward the denial limits. The deterministic floor still runs first, so an `ask` rule or a critical path still prompts. Esc closes the list. |
| `/exit`, `/quit` | Leave. |
| `/guardian-model [<account>/<model>]` | Without an argument, opens the [model picker](#choosing-a-model) limited to the models the guardian could be. With one, pins the guardian to that model for the live session and saves it as `[guardian] model`. A model whose account has no credential becomes an unavailable guardian, which the next adjudication reports. |
| `/help` | Lists the built-in commands, your user-invocable skills and the main keys. |
| `/hooks` | Lists the [hooks](hooks.md) by event with their matcher, command and scope, and marks a repository's hooks that did not load because the workspace is not trusted. |
| `/login [<account>]` | Opens the [provider dialog](#managing-providers) to add an account, log in, log out or remove one. With an account name it opens straight on that account's log-in: a hidden key prompt, or the device sign-in for `opencode-go`. Only between turns. |
| `/mcp` | Lists each [MCP server](mcp.md) as `connected`, `failed` with the error, `left off` (you declined it) or `not loaded` (the workspace is not trusted). It takes no arguments. |
| `/model [<account>/<model>]` | Without an argument, opens the [model picker](#choosing-a-model). With one, switches the live session to that model, rebuilds the agent on it with the conversation kept, and saves it as `[model] model` in your user settings file. The guardian moves with it only when it is on the main-model fallback (no `[guardian] model` and no validated model available); a pinned or validated guardian stays. |
| `/output-style [<name>\|none]` | Without an argument, lists the [output styles](extensions.md#output-styles). With one, switches to it for the next turn; `none` removes it. |
| `/permissions` | Shows the permission mode and the `deny`, `ask` and `allow` rules in force, each with the scope that contributed it (session and always grants included), the workspace roots and the settings file where rules are edited. It is read-only; see [Permissions](permissions.md). |
| `/plan [<description>]` | Switches to plan mode, as Shift+Tab does. With a description it also sends it as your next message, so `/plan fix the auth bug` plans the fix. |
| `/skills` | Lists every loaded [skill](extensions.md#skills) with its scope, marking those only the model can use, those only you can run, and those a built-in command hides. |
| `/status` | Shows the version, the session id, the workspace roots, the permission mode, the main model, the guardian (or why it is unavailable) with its profile, the output style, and how many MCP servers are connected. |
| `/<skill> [arguments]` | Runs a [skill](extensions.md#slash-commands-and-arguments). |

Typing `/` lists the built-in commands and your user-invocable skills with their argument hints,
narrowing as you type; a skill marked `user-invocable: false` does not appear.

`/copy` writes the reply with an OSC 52 escape, so it needs a terminal that supports OSC 52
clipboard access. Inside tmux it first loads the reply with `tmux load-buffer -w -`, which tmux
forwards to the outer terminal under its default `set-clipboard external`; if that fails the
reply still lands in a tmux buffer. A terminal that ignores the escape leaves the clipboard
unchanged without reporting an error.

## Context

`/clear` empties the model's context. The session carries on: the same id, ledger, audit log,
grants and permission mode. The transcript is emptied (what was already written stays in your
terminal's scrollback), and so is the guardian's view of what you have asked, which starts again
from the next message you send; the files, actions and denials it knows about are kept. The old
conversation cannot be resumed: `--resume` reopens the empty one. `/clear` is refused while a
subagent is running, since the subagent's calls are judged against what you asked; wait for it
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
with the point where compaction runs on its own. `/context all` also lists each instruction file
and tool, largest first. The numbers are estimates from a character-based counter, not the
provider's tokenizer, and they count the same way the compaction trigger does. When the model is
not in the catalog or its window is not known there is no `free` row and no automatic compaction
unless `autocompact_window` is set; when the total is over the window it says so.

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

`/guardian-model` with no argument opens the same overlay with only the models the guardian could
use: the benchmark-validated models your accounts offer and every model an account lists in its
`models` setting. A pick pins the guardian and saves it as `[guardian] model`.

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
