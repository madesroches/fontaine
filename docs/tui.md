# The terminal UI

`fontaine` without `-p` starts the terminal UI. It needs a terminal: with piped standard input or
output it exits with code 2 and points at `fontaine -p`. Configuration, accounts and permission
rules work as in headless runs; the mode is the one from `[permissions] default_mode` or
`--permission-mode`. With neither set, the terminal UI starts in `auto`, the mode where the
[guardian](guardian.md) judges each call that nothing pre-approved.

The UI draws inline, not on the alternate screen. Finished output goes into the terminal's normal
scrollback, and a live area at the bottom holds the input box, the status bar, any open prompt and
the unfinished tail of the transcript. Scroll back with the terminal or with tmux copy mode, and
select text with the mouse as in any shell program.

The transcript shows your messages (`>`), the streaming answer, and one line per tool call with its
status: pending, done, failed, denied with the reason, or interrupted. A call the guardian refused
shows its rule label and the guardian's own explanation on lines of their own, for example
`[Download+Execute] pipes a download into a shell`. A line is written to scrollback once it is
settled, so a call's status is final by then.

Narrowing the terminal clears the visible screen and redraws the live area at the top. Output
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
| Enter | Submit. Alt+Enter, Shift+Enter or Ctrl+J insert a newline; a trailing `\` before Enter also continues the line. |
| Shift+Tab | Cycle the mode: manual, accept edits, plan, auto, and back to manual. It takes effect on the next tool call, even mid-turn. Entering `auto` lists the broad allow rules it sets aside and says so when the guardian cannot be asked. |
| Esc, Ctrl-C while a turn runs | Interrupt the turn. The interrupted turn is discarded, earlier turns are kept, and an open prompt is answered with deny. |
| Ctrl-C, or Ctrl-D on an empty input, when idle | Quit. |
| PageUp, PageDown | Scroll the command in an open approval prompt. |
| Arrows, Home, End, Backspace, Delete, Ctrl-A/E/U/K/W/B/F | Edit the input. |
| Ctrl-T | Open or close the [agents panel](#the-agents-panel), even over an approval prompt. |
| `/` as the first character | Lists the commands and skills that match what you typed. |

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
  accounts list explicitly, for this session. `/guardian-model` makes the choice permanent.
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
| `/denied` | Lists the calls the guardian denied this session, with their rule label and explanation. Up and Down select; `r` approves the selected call once, as if you had approved it at a prompt, and tells the agent on its next turn that it may retry exactly that call. The retry runs without asking the guardian, never enters the verdict cache and does not count toward the denial limits. The deterministic floor still runs first, so an `ask` rule or a critical path still prompts. Esc closes the list. |
| `/model [<account>/<model>]` | Without an argument, shows the model and the guardian. With one, switches the live session to that model, rebuilds the agent on it with the conversation kept, and saves it as `[model] model` in your user settings file. The guardian moves with it only when it is on the main-model fallback (no `[guardian] model` and no validated model available); a pinned or validated guardian stays. |
| `/guardian-model [<account>/<model>]` | Without an argument, shows the guardian and the models it could be. With one, pins the guardian to that model for the live session and saves it as `[guardian] model`. A model whose account has no credential becomes an unavailable guardian, which the next adjudication reports. |
| `/agents` | Opens the [agents panel](#the-agents-panel). |
| `/output-style [<name>\|none]` | Without an argument, lists the [output styles](extensions.md#output-styles). With one, switches to it for the next turn; `none` removes it. |
| `/<skill> [arguments]` | Runs a [skill](extensions.md#slash-commands-and-arguments). |
| `/exit`, `/quit` | Leave. |

Typing `/` lists the built-in commands and your user-invocable skills with their argument hints,
narrowing as you type; a skill marked `user-invocable: false` does not appear.

## MCP server approvals

A repository-defined [MCP server](mcp.md#servers-a-repository-defines) in a trusted folder is asked
about before it starts: `y` starts it and remembers the answer for this exact definition, `n`
leaves it off for this session. Servers from your own settings start without a question; a server
that cannot start is reported in the transcript and skipped.

## The agents panel

`/agents` or Ctrl-T opens a list with one row per [subagent](agents.md): its id and type, its
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
