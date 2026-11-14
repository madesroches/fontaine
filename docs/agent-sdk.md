# Claude Agent SDK compatibility

An unmodified TypeScript (`@anthropic-ai/claude-agent-sdk`) or Python (`claude-agent-sdk`)
application can use `fontaine` as its Claude Code executable. Fontaine serves the server side of
Claude Code's stream-json protocol: Claude-shaped `system`, `assistant`, `user`, `stream_event` and
`result` messages, and `control_request` / `control_response` traffic in both directions. It keeps
its own agent loop, models, tools, sessions, guardian and permission pipeline. The surface is
separate from [`headless-v1`](headless.md), which does not change.

Tested against TypeScript SDK `0.3.325`, Python SDK `0.2.182` and the Claude Code `2.1.325`
protocol.

## Selecting `fontaine` as the executable

```ts
import { query } from "@anthropic-ai/claude-agent-sdk";

for await (const message of query({
  prompt: "Summarize the repository",
  options: { pathToClaudeCodeExecutable: "/usr/local/bin/fontaine" },
})) {
  console.log(message);
}
```

```python
from claude_agent_sdk import ClaudeAgentOptions, query

options = ClaudeAgentOptions(cli_path="/usr/local/bin/fontaine")
async for message in query(prompt="Summarize the repository", options=options):
    print(message)
```

Fontaine enters SDK mode when its command line holds `--output-format stream-json`,
`--input-format stream-json` and `--verbose` and neither `-p` nor `--print`, which is the shape
every SDK invocation has. `fontaine -p --output-format stream-json` stays `headless-v1`. The
operand of a value-taking flag is skipped when the line is scanned, so a system prompt that reads
`-p` does not change the mode.

The model is whatever Fontaine is configured to use: `--model` and `set_model` take a model
reference or a [role](configuration.md#modelsroles). A Claude alias such as `sonnet` works only when
you defined a role of that name. Authentication, accounts and the guardian come from your Fontaine
configuration, not from the SDK's options.

A flag Fontaine does not implement exits with code 2 and one line on stderr naming it, before
anything is read from stdin or any model is contacted. The TypeScript SDK appends the tail of
stderr to the error it raises. The Python SDK raises `ProcessError` with the exit code; the line
reaches the parent's stderr, or the `stderr` callback option.

## Capability matrix

Each row is a flag, control request or message field of the SDK protocol. The command-line parser
and the control router read the same table, and a test compares it with this page.

<!-- matrix:start -->

| Item | Kind | State | Note |
|---|---|---|---|
| `--output-format` | flag | mapped | must be `stream-json` |
| `--input-format` | flag | mapped | must be `stream-json` |
| `--verbose` | flag | supported |  |
| `--system-prompt` | flag | mapped | an empty value keeps Fontaine's base prompt; a non-empty one replaces it |
| `--system-prompt-file` | flag | mapped | read as `--system-prompt` |
| `--append-system-prompt` | flag | supported |  |
| `--tools` | flag | mapped | the base set of builtin tools; entries with a specifier are refused |
| `--allowedTools`, `--allowed-tools` | flag | mapped | each entry is a CLI-scope allow rule; it sits below the floor |
| `--disallowedTools`, `--disallowed-tools` | flag | supported |  |
| `--max-turns` | flag | supported |  |
| `--max-budget-usd` | flag | supported |  |
| `--model` | flag | mapped | a Fontaine model reference or role, not a Claude alias |
| `--permission-mode` | flag | mapped | `default` is `manual`; `bypassPermissions` does not exist; `afk` is Fontaine's |
| `--permission-prompt-tool` | flag | mapped | only `stdio`, the `canUseTool` callback; a named MCP tool is refused |
| `--continue`, `-c` | flag | supported |  |
| `--resume`, `-r` | flag | supported |  |
| `--session-id` | flag | supported |  |
| `--fork-session` | flag | supported |  |
| `--settings` | flag | mapped | only an empty object: Claude's settings schema is not Fontaine's |
| `--add-dir` | flag | supported |  |
| `--mcp-config` | flag | mapped | `stdio`, `http` and `sdk` servers; `sse` is refused |
| `--strict-mcp-config` | flag | supported |  |
| `--include-partial-messages` | flag | supported |  |
| `--setting-sources` | flag | mapped | accepted, but never drops Fontaine's project-scope tightening |
| `--effort` | flag | supported |  |
| `--json-schema` | flag | supported |  |
| `--fallback-model` | flag | unsupported | Fontaine falls through model lists configured in its own settings |
| `--betas` | flag | unsupported | Anthropic beta headers are not Fontaine's to set |
| `--thinking` | flag | unsupported | reasoning is set with `--effort` |
| `--max-thinking-tokens` | flag | unsupported | reasoning is set with `--effort` |
| `--thinking-display` | flag | unsupported | reasoning is not streamed |
| `--task-budget` | flag | unsupported | use `--max-budget-usd` |
| `--plugin-dir`, `--plugin-dir-no-mcp` | flag | unsupported | Fontaine installs plugins with `fontaine plugin` |
| `--resume-session-at` | flag | unsupported | sessions resume whole |
| `--resume-drops-turn` | flag | unsupported | sessions resume whole |
| `--session-mirror` | flag | unsupported | Fontaine keeps its own session store |
| `--await-initialize` | flag | unsupported | not needed: `initialize` is optional |
| `--channels` | flag | unsupported | no channels |
| `--sdk-url` | flag | unsupported | no remote transport |
| `--include-hook-events` | flag | unsupported | hook events are not streamed |
| `--no-session-persistence` | flag | unsupported | every session is recorded in the ledger |
| `--permission-prompts` | flag | unsupported | no prompt routing |
| `--agent` | flag | unsupported | no agent selection |
| `--replay-user-messages` | flag | unsupported | not echoed |
| `--allow-dangerously-skip-permissions` | flag | unsupported | there is no bypass mode |
| `--dangerously-skip-permissions` | flag | unsupported | there is no bypass mode |
| `--debug`, `--debug-to-stderr` | flag | unsupported | use `--debug-file` on a plain run |
| `--debug-file` | flag | unsupported | use the debug environment variable of a plain run |
| `--hard-fail` | flag | unsupported | no equivalent |
| `--managed-settings`, `--managed-settings-in-initialize` | flag | unsupported | no managed settings |
| `--project-config-root` | flag | unsupported | no equivalent |
| `--exec-path` | flag | unsupported | no equivalent |
| `initialize` | control request | mapped | hooks, agents, system prompts, schema and SDK servers; see the unsupported fields below |
| `interrupt` | control request | supported |  |
| `set_permission_mode` | control request | mapped | never `bypassPermissions`, never `afk` |
| `set_model` | control request | mapped | a Fontaine model reference or role; applied when no turn is running |
| `mcp_status` | control request | supported |  |
| `get_context_usage` | control request | unsupported | no equivalent report |
| `rewind_files` | control request | unsupported | Fontaine rewinds from its terminal UI |
| `mcp_reconnect` | control request | unsupported | restart a server from the terminal UI |
| `mcp_toggle` | control request | unsupported | no equivalent |
| `stop_task` | control request | unsupported | no `task_*` messages are streamed |
| `update_environment_variables` | control request | unsupported | the environment is fixed at start |
| `can_use_tool.blocked_path` | message field | unsupported | the pipeline has no source for it |
| `can_use_tool.title`, `can_use_tool.display_name` | message field | unsupported | the pipeline has no source for them |
| `system/task_*` | message field | unsupported | subagent messages are not streamed; the `Agent` tool call and result stand for them |
| `user message image and document blocks` | message field | unsupported | only text blocks are read; any other block ends the run with an error |
| `parent_tool_use_id` | message field | mapped | always null: only the root conversation is streamed |
| `hook PreToolUse allow` | message field | mapped | ignored: a host hook can deny, ask and rewrite the input, never skip the mode, `canUseTool` or the guardian |
<!-- matrix:end -->

## Permissions

By default a call that would ask a person is denied, as in a headless run. Passing
`canUseTool` (TypeScript) or `can_use_tool` (Python) makes the SDK send
`--permission-prompt-tool stdio`, which makes the host the human channel: Fontaine sends a
`can_use_tool` request wherever the terminal UI would show an approval prompt.

The callback sits after the unregistered-agent check, the [AFK](afk.md) stop, hooks, the
deterministic floor, rules, the mode and, in `auto` and `afk`, the [guardian](guardian.md). A floor
deny, a guardian deny, or a guardian that failed closed never reaches it. A prompt the floor forces
(a protected path, a critical deletion) or a rule asks for reaches it as it would reach a person,
with no session allow offered. It is not a bypass: nothing the callback answers skips a stage that
has already decided.

A request carries `tool_name`, `input`, `tool_use_id`, `agent_id` (for a subagent's call),
`decision_reason`, `description` and, when there is a rule to propose, one `permission_suggestions`
entry that adds an allow rule at the `session` destination. `blocked_path`, `title` and
`display_name` are not sent.

| The callback answers | Fontaine does |
|---|---|
| `allow`, with the input unchanged or absent | Allows this call. |
| `allow` with `updatedPermissions` adding the suggested rule at `session` | Allows this call and the next calls the rule matches, for the session. |
| `allow` with `setMode` or other session allow rules | Allows this call, then applies them. |
| `allow` with a changed `updatedInput` | Judges the changed input from the start (hooks, floor, rules, guardian) as if the model had made that call. If that asks again, the callback is asked once more. An answer that changes the input a second time is denied. |
| `deny` | Denies with the message as feedback. `interrupt: true` also stops the turn. |
| An error, malformed JSON, an unknown behavior, a cancel, or a closed pipe | Denies. |

`updatedPermissions` is accepted only as `addRules` with `behavior: "allow"` and destination
`session` or `cliArg` for rules Fontaine can parse, and as `setMode`. Anything else (deny or ask
rules, `replaceRules`, `removeRules`, directory changes, allow rules written to a settings file)
denies the whole answer. The list is checked whole before anything is applied; the answer decides
the current call and the updates hold from the next one. A `setMode` that is refused denies the
answer and no rule is added. Allow-always is never offered, so nothing a host answers is written
to your configuration.

There is no bypass mode: `bypassPermissions` and `--allow-dangerously-skip-permissions` are
refused, at the command line or as a control error. `set_permission_mode` and `setMode` go through
the same switch as the terminal UI and are recorded in the ledger. Entering `afk` is possible only
at startup with `--permission-mode afk`; a request to enter it at runtime is refused, and leaving
it is allowed. `default` means `manual`.

`AskUserQuestion` arrives as a `can_use_tool` request with `tool_name: "AskUserQuestion"`. An
`allow` whose `updatedInput.answers` answers every question returns those answers to the model as
the person's own; a `deny` is a declined question. MCP elicitation requests stay declined in SDK
mode.

Permission requests and question requests have no time limit, as an approval prompt has none.
They end on an answer, a disconnect, an interrupt or the end of the run, and in each of the last
three Fontaine sends `control_cancel_request` where the pipe is still open and denies.

## Host-provided tools, hooks and agents

**SDK MCP servers** (`createSdkMcpServer`, `create_sdk_mcp_server`) are served over the SDK's
`mcp_message` control requests. Their tools are registered like any other MCP tool: an
`arbitrary` risk class, the full gate and the result scanner; see [MCP servers](mcp.md#servers-in-the-sdk-host).
`--mcp-config` entries of type `stdio` and `http` become servers at the command-line scope; `sse` is
not supported. `--strict-mcp-config` starts only those.

**Hook callbacks** registered in `hooks` run when their matcher fires: Fontaine sends
`hook_callback` with the same JSON a command hook receives. They can only tighten, as a repository's
hooks can: `deny`, `ask` and `updatedInput` count, an `allow` is ignored. That differs from Claude
Code, where a PreToolUse `allow` skips the permission prompt. See [User hooks](hooks.md#sdk-callback-hooks).

**Programmatic agents** (`agents`) become subagent types for the session, with the toolset,
model and effort restrictions of a repository's agent files; see [Subagents](agents.md#agent-types).

## Sessions

`continue`, `resume`, `forkSession` and `sessionId` work on Fontaine's own sessions. The SDKs'
`listSessions` and `getSessionMessages` read Claude Code's transcript files and do not see them.
A session id from Claude Code cannot be resumed.

## Output

- `system/init` goes out at the start of each turn, with Claude's field names, `tools` under their
  wire names, `claude_code_version` (the protocol level served, `2.1.325`), plus
  `fontaine_version` and `compat_protocol: "claude-sdk-1"`.
- `assistant` carries `text` and `tool_use` blocks. Reasoning is not sent. Only the root
  conversation's messages are streamed: `parent_tool_use_id` is always null, and there are no
  `system/task_*` messages. A subagent shows as the `Agent` call and its result; a subagent's own
  tool call that needs approval carries its `agent_id`.
- With `--include-partial-messages`, `stream_event` messages wrap the text deltas as Anthropic raw
  events. A tool call's arguments arrive as a single `input_json_delta`.
- `system/permission_denied` is sent for each call a gate denied, and each denial is in
  `result.permission_denials`.
- `result` has Claude's `subtype`s (`success`, `error_max_turns`, `error_max_budget_usd`,
  `error_during_execution`, `error_max_structured_output_retries`) and Fontaine's
  `error_usage_limit`. `total_cost_usd` is `null` when no model is priced. `modelUsage` includes
  the guardian and subagent models, and `fontaine_usage` carries the `headless-v1` buckets.
- An interrupted turn ends with `subtype: "error_during_execution"`, `is_error: true` and a
  `terminal_reason`, except when the last message was already assistant text.

### Tool names

A builtin tool is named on the wire as Claude Code names it (`shell` is `Bash`, `read` is `Read`,
`agent` is `Agent`, `ask_user_question` is `AskUserQuestion`); MCP tools keep `mcp__server__tool`; a Fontaine
tool without a Claude name keeps its own. **Input objects are in Fontaine's schema**, not rewritten
to Claude's: for instance `shell` takes `command`, and its other fields are Fontaine's. The same
names are mapped back in `--allowedTools`, `--disallowedTools` and `--tools`, so a host's
name-based policy keeps working. See [Skills, agent files and output styles](extensions.md#tool-names).

## Differences from Claude Code

- `--setting-sources` is accepted and changes nothing. Fontaine reads its own TOML files, and
  never drops a project scope's deny and ask tightening because a host asked. `--settings` other
  than `{}` is refused.
- An empty system prompt (Python's `--system-prompt ""`, TypeScript's `[""]`) keeps Fontaine's base
  prompt, which carries the workspace roots, scratch directory and denial guidance, instead of
  meaning "no preset". A non-empty one replaces the base layer. When the command line and
  `initialize` both give one, `initialize` fails.
- `initialize` fields Fontaine cannot honor (`toolAliases`, `title`, `plugins`, `managedSettings`,
  `excludeDynamicSections` and similar) fail with an error naming them when set to a non-default
  value.
- A failed `initialize` (an unsupported field, a prompt given twice) is answered with its error and
  then ends the run with exit code 2 and the reason on stderr, rather than running the host's
  prompt without its hooks, agents, servers and prompts. An `initialize` after the first user
  message is an error response.
- A user message's text blocks are joined with a newline; an image or document block ends the run
  with an error naming the block type.
- A failed turn (`error_max_turns`, `error_max_budget_usd`, a provider error) is reported as its
  `error_*` `result` and the session keeps reading messages; it ends on end of input (exit code 0),
  a malformed line or a termination signal.
- `get_context_usage`, `rewind_files`, `mcp_reconnect`, `mcp_toggle` and `stop_task` return a
  control error naming the request.
- `interrupt` is answered once the turn has stopped, and its `result` goes out first. `set_model`
  takes effect between turns: sent during a turn, it is answered when the turn ends.
- A message sent while a turn runs is delivered to the model at the next tool result, as in the
  terminal UI, or becomes the next turn if the turn ends first. Control requests are served while
  a turn runs.
- Input to a closed stdin: with no turn running the run ends after the last `result`; during a
  turn the turn finishes and every pending request denies at once. A malformed line, or an
  `update_environment_variables` request, ends the run with an `error_during_execution` result and
  exit code 1.
