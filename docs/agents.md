# Subagents

The `agent` tool starts a subagent: a separate conversation with its own model, its own context and
a subset of the main conversation's tools. Subagents let the model split a task, explore in
parallel and keep long output out of its own context. They follow Claude Code's model of subagents
and background tasks, with stricter permission rules.

## The `agent` tool

| Argument | Meaning |
|---|---|
| `agent_type` | One of the [agent types](#agent-types). Required. |
| `prompt` | The task, with everything the subagent needs: it cannot see the conversation. Required. |
| `model` | Optional: `$main`, `$guardian_default` or `<account>/<model>`. |
| `background` | Optional, default `false`. |

By default the call waits and its result is the subagent's final message. With `background: true`
it returns `{"agent_id": "agent-1"}` at once and the subagent runs alongside; when it ends, its
report is delivered to the conversation that started it as a notification, in the next tool result
that conversation reads or, if it is idle, as the prompt of a turn of its own. The report passes
through the same result pipeline as any tool result. A killed background subagent delivers
nothing. When a subagent ends, any background subagents it started are stopped too, and their
reports are not delivered.

The model used is the call's `model`, else the agent type's `model`, else the session model. An
invalid value is an error result, and so is a `model` on a type defined by a project file (see
[agent files](#agent-files)).

The model gets no tool to list, inspect or kill agents: those are controls for you, in the
[agents panel](tui.md#the-agents-panel).

## Agent types

Two types are built in:

| Type | Model | Tools |
|---|---|---|
| `explore` | `$guardian_default` | `ls`, `read`, `glob`, `grep`, `send_message` |
| `worker` | `$main` | `ls`, `read`, `glob`, `grep`, `edit`, `write`, `shell`, `send_message` |

`$main` is the session model. `$guardian_default` is the top-ranked validated guardian model your
accounts offer, or the session model when there is none.

Types come from four places, later ones replacing earlier ones of the same name: the built-in
ones, [agent files](#agent-files) in your config directory, `[agents.types.<name>]` entries, and
agent files in the repository.

```toml
[agents.types.reviewer]
model = "$guardian_default"
tools = ["ls", "read", "glob", "grep"]
mode = "inherit"
```

`tools` names built-in or `mcp__` tools; leave it out to give the type every tool its parent has.
The toolset a subagent really gets is that list **cut down to what its parent has**: a type can
never reach a tool its parent lacks. `agent` is withheld from a subagent at the nesting limit.

## Modes: never looser than the parent

A subagent's permission mode is never looser than its parent's. The modes form a ladder,
`dontAsk < manual < acceptEdits < auto`, and the `mode` of a type is `inherit` (the default: the
parent's mode), `manual` or `dontAsk`; an explicit mode can only be at or below the parent's. A
subagent can run in `acceptEdits` or `auto` only by inheriting it from a parent that is already
there.

The mode is not fixed when the subagent starts. On every call it is the tighter of the subagent's
own mode and its parent's mode *at that moment*, so if you cycle the session from `auto` to
`manual` while a background subagent works, its next call asks you.

`plan` is not on the ladder. While the parent is in `plan`, **every edit by any subagent is
blocked**, and the rest of its calls are judged as `manual` at most: an `auto` or `acceptEdits`
subagent drops to `manual` for non-edit calls. This follows the parent mid-run, too.

## Every call is gated

A subagent's tool calls go through the same [permission pipeline](permissions.md) as the main
conversation's, with the same hooks, rules, pre-classifier and guardian. Starting a subagent is a
pipeline request of its own: the `agent` call is an `arbitrary` call, so `manual` asks, `auto` has
the guardian judge the task, toolset and model together, and a rule like `agent(worker)` in `deny`
or `ask` works. A bare `agent` allow rule is one of the broad rules `auto` sets aside.

The guardian's picture of what you asked for comes from your own messages in the root session's
ledger, for every agent in the tree. A subagent's prompt, and a message another agent sends, reach
the guardian only inside its delimited, untrusted blocks, never as the task intent.

## Messages between agents

`send_message` takes `to` (an agent id, or `main` for the main conversation) and `message`. An agent
can message its parent, its subagents, its siblings and `main`. The message is queued and the
recipient reads it at its next turn marked with the sender's id and as untrusted data, never as an
instruction from you. The sender gets `{"status": "queued"}`, or the error `agent not running` for a
target that has finished or was killed. `send_message` is `arbitrary` like `agent`, so it is gated
the same way.

## Limits and budgets

| Key | Meaning | Default |
|---|---|---|
| `[agents] max_parallel` | Subagents running at once across the whole session, at any depth. | `4` |
| `[agents] subagent_token_budget` | Tokens one subagent may use. | `200000` |
| `[agents] subagent_cost_budget_usd` | Dollars one subagent may spend. | `2.0` |
| `[agents] session_token_budget` | A ceiling for the whole tree, when set. | unset |
| `[agents] session_cost_budget_usd` | The same in dollars, when set. | unset |

- A spawn past `max_parallel` is not queued. It returns an error result saying the limit was reached
  and not to retry, and starts nothing, whether it was blocking or background.
- Subagents nest up to **three layers** below the main conversation; at the limit the `agent` tool is
  withheld.
- A subagent's spend includes the guardian calls made for it, counted per agent. When it passes a
  cap it **stops**, its report is marked partial, and its pending approval prompts are cancelled as
  denied. When a session ceiling is set, every agent in the tree debits it, and no new subagent
  starts once it is spent.
- A cost cap on a model with no known price is inactive, with a one-time notice; the token cap still
  binds. (`--max-budget-usd` on a main model with no known price remains a usage error.)

## Agent files

An agent file defines a type in Claude Code's format, so `.claude/agents/*.md` files port by
copying. Files live in `<config dir>/agents/<name>.md` and `<root>/.fontaine/agents/<name>.md`; see
[Extensions](extensions.md#agent-files) for the frontmatter and what is refused.

A file in a repository can only narrow: its tools must be tools its parent has, its mode is
`default` (the default) or `dontAsk`, and it names no model, so it runs on the session model. A file
that breaks this stops startup with exit code 2 naming the file. A repository's file replaces a
user file or `[agents.types]` entry of the same name, and so stays narrowed.

## Watching and stopping subagents

In the terminal UI, `Ctrl+T` (or `/agents`) opens the [agents panel](tui.md#the-agents-panel): one row
per subagent with its status, spend against budget, a view of what it did and the gate decisions it
got, and a key to kill it. A background subagent's approval prompts appear like any other, labelled
with its id, and the panel lists them per agent, so none hides behind the main transcript.

A headless run has no panel. If background subagents are still running when the main conversation
gives its final answer, the run waits for them and gives the main conversation a further turn for
each batch of reports, then emits `result`. `--max-turns` and `--max-budget-usd` count those turns
and the subagents' spend; a run stopped by either kills the running subagents and drops their
reports. Each `gate_decision` event carries the `agent_id` that made the call, and
`result.usage.subagents` holds their model usage.

## Telemetry and audit

Every gate decision is written to the [audit log](permissions.md#the-pipeline) with the id of the
agent that made it. Telemetry carries `fontaine.subagent` spans, parented to the `agent` call that
started them; `subagent` events (`spawn`, `complete`, `killed`, `budget_exhausted`); and the
`agent.id` and `agent.type` attributes on each `tool_decision`. Model calls made by subagents count
as `query_source` `subagent`. See [Telemetry](telemetry.md).
