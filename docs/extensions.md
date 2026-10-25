# Skills, agent files and output styles

Three kinds of file extend what Fontaine tells the model, and each has Claude Code's frontmatter, so
files from a `.claude/` directory work by copying. `.claude/` itself is not read.

| Kind | File | What it is |
|---|---|---|
| Skill | `skills/<name>/SKILL.md` | Instructions for a kind of task, loaded when asked for. Also a `/name` command. |
| Agent file | `agents/<name>.md` | A [subagent type](agents.md#agent-types). |
| Output style | `output-styles/<name>.md` | A change to how the agent writes, chosen with `/output-style`. |

They live under two roots: `<config dir>/` (yours, loaded in every session) and
`<root>/.fontaine/` (the repository's, from the primary workspace root only). Extra roots added
with `--add-dir` contribute nothing. These files are prompt content, so they load whether or not
you trust the workspace; what they may *do* is limited as described below.

## Frontmatter

A file starts with a `---` block of `key: value` lines, then the body. Values can be plain or quoted
text, a list written `[a, b]` or one item per `- ` line, or a `|` / `>` text block. A key Fontaine
does not know is ignored, so files written for Claude Code load.

### Skills

| Key | Meaning |
|---|---|
| `name` | The skill's name; the directory name when absent. Letters, digits, `-` and `_`. |
| `description` | One line listed in the system prompt, so the model knows when to load the skill. The first line of the body when absent. |
| `allowed-tools` | Rules lent to the agent while the skill is active; see below. |
| `disable-model-invocation` | `true` keeps the skill out of the model's skill list and refuses the `skill` tool; only you run it, as `/name`. |
| `user-invocable` | `false` hides the skill from the `/` menu and from `/name`; only the model loads it. Default `true`. |
| `argument-hint` | Shown after the name in the `/` menu, such as `[issue-number]`. |
| `model` | Checked and otherwise unused. |

### Agent files

| Key | Meaning |
|---|---|
| `name`, `description` | As above; the file name when `name` is absent. |
| `tools` | The tools of the type: a list or a comma-separated string. Absent means every tool the parent has. |
| `disallowedTools` | Tools taken away from that set. |
| `model` | `inherit` (no model: the session's), `$main`, `$guardian_default` or `<account>/<model>`. |
| `permissionMode` | `inherit`, `default` (that is, `manual`) or `dontAsk`. |

The body is the subagent's instructions.

### Output styles

`name` (the file name when absent) and `description`. The body is added to the system prompt.

## Tool names

`tools`, `disallowedTools` and `allowed-tools` take Claude Code's tool names, renamed on load:

| Claude Code | Fontaine |
|---|---|
| `Read` | `read` |
| `Edit`, `MultiEdit` | `edit` |
| `Write` | `write` |
| `Glob`, `Grep`, `LS` | `glob`, `grep`, `ls` |
| `Bash` | `shell` |
| `WebFetch` | `webfetch` |
| `Task`, `Agent` | `agent` |
| `Skill` | `skill` |
| `SendMessage` | `send_message` |
| `AskUserQuestion` | `ask_user_question` |
| `TodoWrite` | `todo_write` |
| `CronCreate` | `cron_create` |
| `CronList` | `cron_jobs` |
| `CronDelete` | `cron_delete` |
| `ScheduleWakeup` | `schedule_wakeup` |

A `(specifier)` is kept as written, so `Bash(git *)` becomes `shell(git *)`, and the result must
be a valid [permission rule](permissions.md#rules). Fontaine's own names (`shell`,
`mcp__github__create_issue`) are accepted as they are. In an agent file, an entry with a specifier
limits that tool: the subagent may use `shell` only as `git status`, and a `disallowedTools` entry
with a specifier forbids just the calls it matches.

**Anything else is an error that stops startup with exit code 2 and names the file**: a Claude Code
tool Fontaine does not have (`WebSearch`, `NotebookEdit`), the colon form
`Bash(git add:*)`, which is not Fontaine's rule syntax, and a `model` such as `sonnet`, which is an
alias and not a model reference. So are keys that would run something or change permissions from
inside a file: `hooks` and `mcpServers` in an agent or skill file, and `permissionMode` in a
skill. Files load without workspace trust, so they carry no executables of their own.

## Skills

The names and descriptions of the skills go into the system prompt, between the instructions files
and the output style. The body loads through the `skill` tool, whose only argument is the skill's
name. Because it can load nothing but skill text, `skill` is a read-only tool and needs no approval;
what the skill then asks the model to do goes through the permission pipeline like anything else.
A personal skill outranks a repository skill of the same name. In the terminal UI, `/skills` lists
every loaded skill with its scope.

### Slash commands and arguments

A skill with `user-invocable` not `false` is the command `/name`. Typing `/` lists the commands
and skills with their `argument-hint`. `/name some arguments` runs the skill with the rest of the
line as its arguments:

- `$ARGUMENTS` is replaced by the whole argument string, `$ARGUMENTS[N]` and `$N` by the N-th
  argument counting from 0 (words, with single or double quotes keeping one together); one past the
  end is empty.
- A body with no such placeholder gets `ARGUMENTS: <arguments>` appended when there are arguments.

A built-in command wins over a skill of the same name or alias. The skill gets no `/` menu entry,
startup names it, and the model can still load it with the `skill` tool.

What you typed (`/name some arguments`) is recorded as your message; the skill's text is the
agent's prompt for that turn, not your words.

### Commands in a skill

A line `` !`git status` `` in a skill body, or a fenced block that starts with `` ```! ``, is a
command run when the skill loads, with its output put in its place. Arguments are substituted
first. Each command goes through the permission pipeline **as a `shell` call by the agent that
loaded the skill**: it asks, is judged by the guardian or is denied exactly as if the model had
called `shell`. A denied command's place shows `[command failed: ...]` and the skill still loads.
A repository's skill commands run only when the workspace is trusted; a headless run (`fontaine -p`) runs them without asking, as it does for hooks and MCP servers.

### `allowed-tools`

A skill from your own config directory can lend rules, in the `Tool(specifier)` grammar, to the
agent that runs it:

```yaml
---
description: Run the test suite
allowed-tools: Bash(cargo test), Read
---
```

They act like allow rules (after the floor and the mode, suspended in `auto` if broad) from the
moment the skill is loaded until the end of that user turn, whether it was loaded by `/name` or by
the `skill` tool. They belong to the invoking agent only: a subagent it starts does not get them. A
repository's skill cannot approve calls, so its `allowed-tools` is checked and then ignored.

## Agent files

An agent file adds a type to the registry that `agent` offers; see
[Subagents](agents.md#agent-types) for how types combine. A file in your config directory follows
the rules of `[agents.types]`: `permissionMode` is `inherit`, `default` or `dontAsk`, and
`acceptEdits`, `auto`, `plan` and `bypassPermissions` are errors.

A file in a repository can only narrow what its parent can do:

- no `model`, other than `inherit`: it runs on the session model;
- `permissionMode` is `default` (the default) or `dontAsk`, never `inherit`;
- every tool in `tools` is a tool the parent has: a built-in tool, or a tool of an MCP server that
  is configured for the session.

A file that breaks these is an error that stops startup with exit code 2.

## Output styles

`/output-style` lists the styles, `/output-style <name>` switches to one for the next turn and
`/output-style none` removes it. The style's text is added to the agent's system prompt after the
skill list. It changes nothing the guardian sees: the guardian's request and prompt are the same
with and without a style.

## Porting from `.claude/`

Copy `skills/`, `agents/` and `output-styles/` into `<config dir>/` or `<root>/.fontaine/`, then:

- check the `tools`, `disallowedTools` and `allowed-tools` lines against the table above, and drop
  the colon form (`Bash(git add:*)` becomes `Bash(git add *)`);
- replace Claude Code model aliases (`sonnet`, `opus`) in `model` with `inherit` or a
  `<account>/<model>`;
- remove `hooks` and `mcpServers` from the file and declare them in [settings](configuration.md)
  (`[[hooks.*]]`, `[mcp.servers.*]`), where they are subject to workspace trust;
- in a skill, remove `permissionMode`.
