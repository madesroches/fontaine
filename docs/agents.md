# Subagents

The `agent` tool starts a subagent: a separate conversation with its own model, its own context and
a subset of the main conversation's tools. Subagents let the model split a task, explore in
parallel and keep long output out of its own context. They follow Claude Code's model of subagents
and asynchronous tasks, with stricter permission rules.

## The `agent` tool

| Argument | Meaning |
|---|---|
| `agent_type` | One of the [agent types](#agent-types). Required. |
| `prompt` | The task. A subagent cannot see the conversation, so it needs everything in `prompt`; a [`fork`](#forking-the-conversation) already has the conversation and needs only a short task. Required. |
| `model` | Optional: `$main`, `$guardian_default` or `<account>/<model>`. |
| `name` | Optional [name](#naming-a-subagent) for the subagent. |
| `teammate` | Optional, only with [teams](#agent-teams) on: start a teammate that goes idle between pieces of work. |

The call returns `{"agent_id": "agent-1", "name": "explore-1"}` at once and the subagent runs alongside its parent and
its siblings; spawn several in a row and they all run concurrently, up to `max_parallel`. When one
ends, its report is delivered to the conversation that started it as a notification, in the next
tool result that conversation reads or, if it is idle, as the prompt of a turn of its own. The
report passes through the same result pipeline as any tool result. A killed subagent delivers
nothing. A conversation that wants a result before going on waits for it with
[`job_output`](#waiting-for-a-subagent).

A subagent that gives its final answer while its own subagents still run does not end yet: it
waits for their reports (or for a message, or for words you type to it) and takes a further turn
on each batch, as a headless run's main conversation does, and only then ends. Its report is the
answer of its last turn. A subagent that is killed, fails or runs out of budget cannot do this:
the subagents it started are stopped with it, and their reports are not delivered.

The model used is the call's `model`, else the agent type's `model`, else the session model. An
invalid value is an error result, and so is a `model` on a type defined by a project file (see
[agent files](#agent-files)).

### Naming a subagent

Every subagent has a name besides its `agent-N` id. The id stays the identity: the ledger, the
audit log, telemetry and budgets key on it. The name is the label you and the other agents read
(`survey-tech-debt (agent-3)` in the panel, `Report from agent survey-tech-debt (agent-3)` in a
notification) and an address that `send_message` and `job_output` accept in place of the id.

- **Choosing one.** The model may pass `name`: 1 to 32 characters of `a-z`, `0-9` and `-`,
  starting with a letter, such as `survey-tech-debt`. Its tool description asks for a short role
  name when it starts several agents.
- **Fallback.** Without a `name` the subagent is called `<type>-<k>`, the agent type in that
  character set and the first free number: `explore-1`, `explore-2`, `worker-1`.
- **Unique for the session.** A name is never reused, not even after its agent ended, so a late
  message cannot reach the wrong agent. A name that is taken, is `main` or `root`, starts with
  `job-` or has the form `agent-<number>` is refused as invalid arguments, with the reason, and
  the model picks another; nothing is renamed silently. A new process of a resumed session still
  refuses the names of the agents the earlier process started.
- **Reach.** A name resolves to its id before the usual rules apply, so it reaches exactly the
  agents the id would: a parent, its children, siblings and `main`.

### Waiting for a subagent

`job_output` with the `agent_id` or the name blocks until that subagent ends, or for `wait_secs` if given.
It reports only status; the report itself is delivered as a notification attached to that same
result, and never a second time.

- **Ended.** The result names how it ended and says its report follows, that it was killed and
  delivers no report, or that the report was already delivered to the model.
- **Still running.** A timeout (or `wait_secs` of 0) answers with the subagent's age, or that
  it waits on a permission prompt. A later wait or the notification still gets the report.
- **Only the parent.** Only the agent that started a subagent may wait on it; any other id is
  refused.
- **Early return.** A wait of any kind also returns when a message from another agent reaches
  the waiter or, in the main conversation, when you type one. The message is delivered with
  that result.
- **Interrupted.** Interrupting a wait or a turn (Esc) leaves every subagent running; their
  reports arrive later as usual. Stop one from the agents panel. `/clear`, `/rewind` and
  `/branch` are refused while any subagent runs or its report waits to be delivered; run them
  again once it ends or you stop it.

The model still gets no tool to list or kill agents: those are controls for you, in the
[agent list and the agents panel](tui.md#the-agent-list-and-the-agent-view).

## Agent types

Three types are built in:

| Type | Model | Tools |
|---|---|---|
| `explore` | `$main` | `ls`, `read`, `glob`, `grep`, `lsp`, `send_message` |
| `worker` | `$main` | `ls`, `read`, `glob`, `grep`, `lsp`, `edit`, `write`, `notebook_edit`, `shell`, `send_message` |
| `fork` | the session model | every tool the main conversation has, except `ask_user_question` |

`$main` is the session model. `$guardian_default` is the top-ranked validated guardian model your
accounts offer, or the session model when there is none; it is opt-in — no built-in type uses it.

Types come from four places, later ones replacing earlier ones of the same name: the built-in
ones, [agent files](#agent-files) in your config directory, `[agents.types.<name>]` entries, and
agent files in the repository. A type of yours named `fork` replaces the built-in one and loses its
ability to inherit the conversation: starting from the conversation is a built-in capability that no
file or config entry can grant.

```toml
[agents.types.reviewer]
model = "work/claude-sonnet-5"
tools = ["ls", "read", "glob", "grep"]
mode = "inherit"
effort = "low"
```

`effort` (`off`, `low`, `medium`, `high` or `max`) sets the reasoning level of the type's model calls. Left out, the subagent runs at the level the session had when it started (see [`[model] effort`](configuration.md#model)). It may differ from its parent's: effort is not a permission. A level the type's model cannot take is dropped with a one-time warning.

`tools` names built-in or `mcp__` tools; leave it out to give the type every tool its parent has.
The toolset a subagent really gets is that list **cut down to what its parent has**: a type can
never reach a tool its parent lacks. A type whose `tools` names `agent` also gets `job_output` (when its parent has it), which it
needs to wait for the subagents it starts; `disallowed` can still remove it. `agent` is
withheld from a subagent at the nesting limit.
A subagent with `todo_write` keeps its own list, which is not shown in the UI and not saved with the session.

## Forking the conversation

`fork` starts a subagent from a copy of the main conversation instead of a briefing. It sees what
the main model saw when it made the call: the earlier turns, this turn's prompt and everything the
turn has done so far. Its `prompt` is its task, which can be short because the context is already
there. It then works like any subagent, with the same permission checks, the mode-never-looser rule,
budgets and limits, and it appears in the agents panel as `fork`.

- It runs on the session model, so `model` is refused: the session model produced the context it
  inherits.
- Only the main conversation can fork; a subagent that calls `fork` is refused and should start a
  typed subagent instead.
- Nothing in the copied context becomes the user's intent for the guardian. Intent is still only
  your own messages, and the copied tool results and model text stay untrusted data.
- If the main conversation's context cannot be copied, the fork does not start; it is never started
  empty.
- A `fork` uses the subagent system prompt with a fork-specific note, not the main conversation's
  exact prompt, which carries sections only the main conversation gets, such as memory and
  scheduling.
- The inherited context is billed on the fork's first call, so a long conversation counts
  against an explicit `subagent_token_budget` or `subagent_cost_budget_usd` from the start;
  with a tight cap set, the fork can end as `BudgetExhausted` with a partial result.
- Forking is a rule target like any type: `deny = ["agent(fork)"]` forbids it.

This is not the `/fork` slash command, an alias of `/branch` that splits your own session
(see [the TUI](tui.md)).

## Modes: never looser than the parent

A subagent's permission mode is never looser than its parent's. The modes form a ladder,
`dontAsk < manual < acceptEdits < auto`, and the `mode` of a type is `inherit` (the default: the
parent's mode), `manual` or `dontAsk`; an explicit mode can only be at or below the parent's. A
subagent can run in `acceptEdits` or `auto` only by inheriting it from a parent that is already
there.

The mode is not fixed when the subagent starts. On every call it is the tighter of the subagent's
own mode and its parent's mode *at that moment*, so if you cycle the session from `auto` to
`manual` while a subagent works, its next call asks you.

`plan` is not on the ladder. While the parent is in `plan`, **every edit by any subagent is
blocked**, and the rest of its calls are judged as `manual` at most: an `auto` or `acceptEdits`
subagent drops to `manual` for non-edit calls. This follows the parent mid-run, too.

## Every call is gated

A subagent's tool calls go through the same [permission pipeline](permissions.md) as the main
conversation's, with the same hooks, rules, pre-classifier and guardian. Starting a subagent is a
pipeline request of its own: the `agent` call is an `arbitrary` call, so `manual` and `acceptEdits` ask, `auto` and `plan`
allow it at the `spawn` tier without a guardian call, and a rule like `agent(worker)` in `deny`
or `ask` works. A bare `agent` allow rule is one of the broad rules `auto` sets aside, though
that changes nothing for spawns.

The exception is a spawn whose child can only read, such as `explore`: Fontaine computes the
child's toolset, and when every tool in it is read-only, the spawn takes the read-only fast path
in every mode, with no prompt and no guardian call. `worker`, `fork` and any type with `agent`,
`shell`, a write tool, a network tool or an MCP tool still go through the paths above. The
guardian never judges the spawn itself: every call the child makes is gated on its own, and its
mode is never looser than its parent's. A `deny` or `ask` rule on `agent` still applies first.

The guardian's picture of what you asked for comes from your own messages in the root session's
ledger, for every agent in the tree. A subagent's prompt, and a message another agent sends, reach
the guardian only inside its delimited, untrusted blocks, never as the task intent.

## Messages between agents

`send_message` takes `to` (an agent name or id, or `main` for the main conversation) and `message`. An agent
can message its parent, its subagents, its siblings and `main`. The message is queued and the
recipient reads it at its next turn marked with the sender's name and id and as untrusted data, never as an
instruction from you. The sender gets `{"status": "queued"}`, or the error `agent not running` for a
target that has finished or was killed. A teammate that is [idle](#agent-teams) is the one exception:
the message wakes it. `send_message` is `arbitrary` like `agent`, so it is gated the same way.

Only the main conversation can use `ask_user_question`. The built-in types do not have it, and a
type that names it (or that names no tools, and so inherits every tool of its parent) is accepted
but refused when called, with the "unavailable" result, so a subagent can never put a
dialog in front of you.

## Agent teams

Agent teams let several subagents work as named teammates that share a task board, message each
other and are held to quality gates by hooks. They are built on the subagents above, with no new
agent loop and no new permission path: a teammate is an ordinary subagent with an idle state, and
every board change and every message is an ordinary gated request.

Teams are on by default. Turn them off in your user config or with `--settings`; a repository's
shared settings cannot change the key either way:

```toml
[agents]
teams = false
```

With teams off, the `teammate` argument is not offered and a call that passes it is refused,
and the board tools are not registered.

- **The lead and the team.** The team is the session's agent tree and its lead is always `main`.
  There is one team per session and nothing to create or name. Only `main` can start a teammate
  (`teammate: true` on the `agent` call); a teammate cannot start teammates of its own, though it
  may start plain subagents under the usual rules.
- **Same rules as any subagent.** A teammate has the same toolset cut-down, a mode never looser than
  its parent's, its own and the session's budgets, the nesting limit and `max_parallel`. Every call
  it makes passes the permission pipeline. Nothing about teams reaches past the deterministic floor.
- **Idle instead of done.** When a teammate's turn ends, it goes `idle` instead of `done`. It keeps its
  name, its conversation and the tasks it owns, and it makes no model calls and takes no
  `max_parallel` slot while it waits. The lead is told with a report that says the teammate finished
  its turn and is idle. A teammate that fails, is killed or runs out of budget is final, as any
  agent is.
- **Waking a teammate.** A `send_message` to an idle teammate wakes it: the message becomes its next
  prompt, marked with the sender's name and id and as untrusted data. The sender still gets
  `{"status": "queued"}`. You can also type into its view, as for any ended agent. A wake can be
  refused like any resume: a spent budget, a full `max_parallel`, or a session that is ending.
  The sender then gets the reason as the tool's error, and the teammate's tasks go back to the board.
  What wakes a teammate is never recorded as something you said, so a peer's message, a hand-off
  note or a hook's reason never becomes the guardian's idea of your intent; only words you type
  into its view are recorded, as for any subagent.
- **Idle teammates and the session.** An idle teammate never keeps a headless run open and holds
  nothing when the session ends. `/clear` and `/rewind` are refused while a teammate is idle, since
  its conversation holds history a later wake needs; kill it from the agents panel or end the session.

### The task board

The board is the shared list the team claims work from. It is separate from the private
`todo_write` list of each agent, and from the `/tasks` alias of `/agents`. A task has an id (`t1`,
`t2`, never reused), a title (200 characters), a description (5 000), an owner, a status
(`pending`, `in_progress`, `completed`) and up to 20 tasks that block it; the board holds 100.

| Tool | Class | Does |
|---|---|---|
| `board_show` | read-only | Lists every task, or shows one by `id`, with status, owner and blockers. |
| `board_update` | `arbitrary`, gated like `send_message` | `create` (`title`, optional `description` and `blocked_by`), `claim`, `complete` and `release` (`id`). |

- **Claiming is atomic.** A task can be claimed only while it is pending, has no owner and every task
  that blocks it is completed. Of two simultaneous claims exactly one wins. `blocked_by` must name
  tasks that already exist and is fixed at creation, so dependencies cannot form a cycle.
- **Who may change what.** Only a task's owner, or the lead, can complete or release it while it is in
  progress.
- **Dead owners give tasks back.** A task owned by an agent that ends as failed, killed or out of
  budget, or whose wake was refused, returns to pending with no owner. An idle teammate's tasks stay
  its own.
- **Task text is data.** Titles and descriptions are written by agents. When `board_show` shows
  them to an agent they are fenced and labelled with their author, and they are never the guardian's
  idea of what you asked for.
- **Permissions.** `board_show` is read-only, so it takes the read-only fast path; a subagent type
  that holds only read-only tools and `board_show` still takes the read-only spawn fast path.
  `board_update` is `arbitrary`: `manual` and `acceptEdits` ask, `auto` and `plan` send it to the
  guardian, and a `deny` or `ask` rule on `board_update` applies first. It has no fast path.
  The built-in `worker` type gets both tools and `explore` gets `board_show` when teams are on; a
  type of yours has them only if its `tools` names them (or names none), and `fork` inherits what
  its parent has. An `explore` teammate therefore cannot claim or complete tasks; the `agent` result
  says so when a type without `board_update` starts as a teammate, and a team that should claim its own work uses
  `worker`.

### The idle hand-off

When a teammate's turn ends and a task is claimable (pending, unowned, unblocked), the runtime
wakes the teammate with a note that the task is available, to claim it with `board_show` and
`board_update`. The note is marked as harness text, not as you. A teammate whose type has no
`board_update` is not handed off to.

The hand-off and a [`teammate_idle` hook](hooks.md#teams-task_completed-and-teammate_idle) that
blocks can keep a teammate going, and neither is bounded by finite news, so each teammate is woken
this way at most three times in a row without the board changing in between (a create, claim,
complete or release resets the count). Past that it goes idle and you get one warning.

### Persistence and resume

The board is saved as `tasks.json` in the session directory, owner-only, after every change. Agent ids
do not survive a resume, so a resumed session loads the board with every in-progress task back to
pending and no owner, and teammates are not recreated; the lead starts the ones it needs.

### Not built

Claude Code's tmux split-pane display and a per-teammate model beyond what `agent` already offers
are not part of teams here. Teammates are Fontaine subagents in the same process and session tree,
not independent sessions, which keeps one permission pipeline and one budget tree.

## Limits and budgets

| Key | Meaning | Default |
|---|---|---|
| `[agents] max_parallel` | Subagents running at once across the whole session, at any depth. | `4` |
| `[agents] subagent_token_budget` | Tokens one subagent may use; uncapped when unset. | unset |
| `[agents] subagent_cost_budget_usd` | Dollars one subagent may spend; uncapped when unset. | unset |
| `[agents] session_token_budget` | A ceiling for the whole tree, when set. | unset |
| `[agents] session_cost_budget_usd` | The same in dollars, when set. | unset |

- A spawn past `max_parallel` is not queued. It returns an error result saying the limit was reached
  and not to retry, and starts nothing.
- Subagents nest up to **three layers** below the main conversation; at the limit the `agent` tool is
  withheld.
- A subagent's spend includes the guardian calls made for it, counted per agent. When it passes a
  cap it **stops**, its report is marked partial, and its pending approval prompts are cancelled as
  denied. When a session ceiling is set, every agent in the tree debits it, and no new subagent
  starts once it is spent.
- A cost cap, when set, is inactive on a model with no known price, with a one-time notice; a
  token cap, if set, still binds. (`--max-budget-usd` on a main model with no known price
  remains a usage error.)

## A subagent's conversation

A subagent does not keep its conversation in memory: it is written to the session store, one
`logs/agent-<n>.jsonl` per agent beside the root transcript, and it stays there for the session's
life. The log is saved when a run ends, so a subagent that is still running has its completed runs
only on disk; a run that fails keeps what it committed, exactly like the main conversation. The
ledger's `spawned` records anchor agent id continuity: a resumed session mints `agent-<n>` ids past
the largest one its ledger recorded, so a restarted session never reuses an id whose transcript is
already on disk. `/export` reads these logs (see [Sessions](tui.md#sessions)); pruning them is
part of deleting the session.

## Agent files

An agent file defines a type in Claude Code's format, so `.claude/agents/*.md` files port by
copying. Files live in `<config dir>/agents/<name>.md` and `<root>/.fontaine/agents/<name>.md`; see
[Extensions](extensions.md#agent-files) for the frontmatter and what is refused.

A file in a repository can only narrow: its tools must be tools its parent has, its mode is
`default` (the default) or `dontAsk`, and it names no model and no effort, so it runs on the session model at the session's level. A file
that breaks this stops startup with exit code 2 naming the file. A repository's file replaces a
user file or `[agents.types]` entry of the same name, and so stays narrowed.

## Watching and stopping subagents

In the terminal UI, a list below the status bar shows `main` and every running subagent, and
`x` in it stops one. Enter on an agent shows its own transcript, drawn like the main chat, and
what you type there goes to that agent: see [the agent list and the agent
view](tui.md#the-agent-list-and-the-agent-view). `Ctrl+T` (or `/agents`, or `/tasks`) opens the
[agents panel](tui.md#the-agents-panel), the full overview with one row per subagent, ended ones
included, its status (a teammate waiting for work shows `idle`, not `done`), spend against budget, a key to kill it, and the team's task board. A subagent's approval
prompts appear like any other, labelled with its id, whichever transcript is on screen, so none
hides behind another. While a subagent's model is mid-step, its row shows how many characters it
has written so far (`· writing 4.2k chars`), since its usage only updates when the call ends.

Killing a subagent stops it at the next boundary: the in-flight model stream ends before the next
request, an in-flight tool call or permission wait is cut short at the same gate every call
passes, and a run that stays wedged past a bounded grace is dropped as before. The row shows
`killed` immediately; the report and the ledger's `finished` record wait for the run to settle,
normally within the grace. What the subagent committed up to the kill is kept in its own log,
`logs/agent-<n>.jsonl` in the session store, so `/export` renders its conversation up to the kill
and a later look has the evidence of what it was doing. A killed subagent still cannot be
resumed.

### Messaging a subagent

Text typed in a subagent's view is yours, not another agent's, and is handled as the main
agent handles what you type mid-turn:

- **A running subagent** reads it with its next tool result, marked as sent by you. It enters
  the ledger then, as a message typed to that agent (`user_message` with an `agent` field), and
  is intent for that agent and its own subagents only: the guardian never judges the main
  conversation's calls, or a sibling's, by words those models have not read.
- **An ended subagent** (`done` or `failed`) is resumed: another run starts on the same
  conversation — its own per-agent log, persisted in the session store — with the same tools,
  mode, budget (spend carries over, so the cap is cumulative) and a `resumed` event in the
  ledger, followed by your words.
  The resumed run is held to the same posture as before, never looser than its parent's on any
  call. Its report goes to the agent that started it, or to the main conversation when that agent
  has ended, and an idle main conversation gets a turn to read it.
- **A killed subagent, one that ran out of budget, or any whose budget is spent** is never
  resumed. A resume that would exceed `max_parallel` or the session's agent budget is refused.
- A message a run ended without reading (the agent answered without another tool call) resumes the
  agent with it once the run ends; if that resume is refused, you are told what was not
  delivered.

`send_message` from another agent keeps its rule: it reaches only a running agent (or wakes an
idle teammate), and the recipient reads it as untrusted data, not as you.

A headless run has no panel. If subagents are still running when the main conversation
gives its final answer, the run waits for them and gives the main conversation a further turn for
each batch of reports, then emits `result`. `--max-turns` and `--max-budget-usd` count those turns
and the subagents' spend; a run stopped by either kills the running subagents and drops their
reports. Each `gate_decision` event carries the `agent_id` that made the call, with its `agent_name`, and
`result.usage.subagents` holds their model usage.

## Telemetry and audit

Every gate decision is written to the [audit log](permissions.md#the-pipeline) with the id of the
agent that made it, and its name. Telemetry carries `fontaine.subagent` spans, parented to the `agent` call that
started them; `subagent` events (`spawn`, `resume`, `complete`, `killed`, `budget_exhausted`), which carry `agent.id`, `agent.name` and `agent.type`; `task` events for the board; and the
`agent.id` and `agent.type` attributes on each `tool_decision`. Model calls made by subagents count
as `query_source` `subagent`. See [Telemetry](telemetry.md).
