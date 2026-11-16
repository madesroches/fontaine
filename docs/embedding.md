# Embedding Fontaine in a Rust program

Another program can run the guarded Fontaine agent: build a session, register its own tools and
hooks, answer approvals through a callback, and read the run as an event stream. There are three
routes, and they share one guarded agent.

| You write | Use | Surface |
|---|---|---|
| Rust | the `fontaine-sdk` crate, this page | typed `Event`s that serialize to `headless-v1` |
| Any language | `fontaine -p --output-format stream-json`, see [Headless runs](headless.md) | `headless-v1` JSON lines over stdio |
| TypeScript or Python | the Claude Agent SDK with `fontaine` as its executable, see [Claude Agent SDK compatibility](agent-sdk.md) | Claude-shaped messages over stdio |

There is no fourth protocol: the Rust route is the same event stream in process, and the other
languages use the stdio surfaces that already exist.

## The crate

`fontaine-sdk` is the one crate a Rust embedder names. Its crate root is the documented surface:
`SessionBuilder`, `Session`, `Turn`, `InterruptHandle`, `Event`, `HostTool`, `EmbedError`, and
`chain` (renders an error's cause chain), and re-exports of `Approver`, `PermissionMode`,
`CallbackHook`, `HookTransport` and `HookEvent`. The `permissions`, `config`, `events` and `error`
modules are stable too. The
`core`, `tools`, `agents`, `guardian`, `assemble` and `run` modules are public because the
`fontaine` binary, its terminal UI and the evals are built on them, and they carry no stability
promise. The crate is `0.x` and not published; depend on it by path or git.

## A session in a few lines

```rust
use std::sync::Arc;
use fontaine_sdk::{PermissionMode, SessionBuilder};
use futures::StreamExt;

let mut session = SessionBuilder::new(project_dir)
    .mode(PermissionMode::Manual)
    .approver(Arc::new(MyApprover))
    .tool(MyTool)
    .build()
    .await?;

let mut turn = session.send("fix the failing test");
while let Some(event) = turn.next().await {
    println!("{}", serde_json::to_string(&event?)?);
}
drop(turn);
session.close().await?;
```

The runnable version is `rust/fontaine-sdk/examples/embed.rs`.

## The builder

`SessionBuilder::new(project_dir)` takes the directory the project settings are looked up from. The
rest is optional.

| Method | Effect |
|---|---|
| `dirs(config_dir, data_dir)` | Where user settings and session data live. The default is the platform's, with the `FONTAINE_CONFIG_DIR` and `FONTAINE_DATA_DIR` overrides. |
| `env(lookup)` | Where environment variables are read, such as API keys. The default is the process environment. The variable names scrubbed from tool subprocesses always come from the real process environment, because a lookup cannot list names. |
| `mode(mode)` | The permission mode to start in. The default is `[permissions] default_mode`, then `manual`. `Afk` starts in `auto` and enters AFK once the session exists, as `--afk` does. |
| `model(reference)` | A model reference or role name. |
| `system_prompt`, `append_system_prompt` | Replace or extend the system prompt. |
| `json_schema(value)` | A JSON Schema object the final answer must follow; it arrives as `structured_output`. |
| `max_turns(n)`, `max_budget_usd(usd)` | The same limits as `--max-turns` and `--max-budget-usd`. A budget needs a main model with a known price. |
| `allowed_tools(rules)`, `disallowed_tools(rules)` | Rules at the command-line scope, below the permission floor. |
| `builtin_tools(names)` | The tools the root agent is built with. The list must also name every host tool you want to keep. |
| `include_partial_messages(on)` | Emit `assistant_delta` events as the answer streams. Off by default. |
| `add_dir(dir)` | An extra workspace root. |
| `resume(SessionRef::Continue \| SessionRef::Id(..))`, `fork()` | Start from a stored session; `fork` copies it. |
| `approver(approver)` | Answers the prompts a person would. |
| `hook(callback_hook)` | A host hook for tool events. |
| `tool(host_tool)` | A tool the model can call. |

`build()` checks the arguments and fails with `EmbedError::Config` for `max_turns` of 0, a
`max_budget_usd` that is negative or not finite, a `json_schema` that is not a JSON object, a `fork`
without `resume`, a `hook` matcher that names a host tool (a matcher cannot address one; match every
call and filter in the callback), or a host tool that breaks the rules below. It then loads the settings, opens or
creates the session, resolves the models, starts the tool-result scanner, connects configured MCP
servers and starts the run. `EmbedError` has the variants `Config`, `Session`, `Credentials`,
`Model` and `Runtime`; each keeps its cause as `source()`, so render a failure with
`fontaine_sdk::chain`. `Credentials` also covers a pinned guardian whose account
has nobody signed in, which would deny every call in `auto` and `afk`.

Warnings found while building, and the run loop's startup lines, go to the approver's `warn`. The
SDK writes nothing to stderr itself; the trait's default `warn` does, so override it to route them
elsewhere. Provider warnings use a process-wide sink, so with several embedded sessions they reach
the approver of the one built most recently.

## Approvals

With an approver, the session's human channel is `delegate`. The approver is asked exactly where a
person would be, after the floor, the hooks, the rules, the mode and the guardian have all spoken,
and it is never offered "always allow": nothing it says outlives the session. Its answer cannot
reach past the floor or the guardian. Without an approver, every call that would prompt is denied
and recorded as such, as in `fontaine -p`. A session never has a person at a terminal, so the
terminal channel is not exposed.

## Host tools

A host tool implements `HostTool`: a `name`, a `description`, JSON-schema `parameters`, and an async
`call` that returns a string or a `ToolExecutionError`. A host tool declares no risk. Every host tool
is registered as arbitrary-risk, the way MCP tools are, and every call goes through the same gate as
a built-in: the floor, hooks, rules, the mode and, in `auto`, the guardian. In `manual` it prompts.
It is never on the read-only fast path, and its result passes the result pipeline and the
tool-result scanner before the model sees it. Telemetry reports it with `tool_source` `host`.

`build()` rejects, with `EmbedError::Config`, a name that is empty, longer than 64 characters, outside
`A-Za-z0-9_-`, equal to one of Fontaine's tool names or starting with `mcp__`, or used by another host
tool. Host tools keep their bare names, so permission rules, `allowed_tools`, `disallowed_tools`,
agent-type `tools` lists and hook matchers cannot address them, and `build()` rejects a host-tool
name in `allowed_tools` or `disallowed_tools`. Tighten a host tool with `hook()` and the approver.
A host tool is subject to `builtin_tools` and subagent toolset cutting like a built-in, so a subagent
never has a host tool its parent lacks.

## Hooks

`hook()` registers a `CallbackHook` with a `HookEvent`, an optional matcher, a callback id and a
`HookTransport`. Fontaine calls `HookTransport::call(callback_id, input)` with the JSON the event
carries, the same input a configured command hook receives on stdin, and reads the JSON the hook
returns, in the format described in [User hooks](hooks.md). A callback hook can only tighten: it can
deny, ask or rewrite the input, and an `allow` it returns is ignored. A failure or timeout denies the
call when `fails_closed` is set and decides nothing otherwise.

## Turns and events

`Session::send(prompt)` returns a `Turn`, a `Stream` of `Result<Event, EmbedError>` that ends after
the turn's `result`. `Event` is a `#[non_exhaustive]` enum with one variant per object that
`fontaine -p` prints (`init`, `assistant`, `assistant_delta`, `tool_call`, `gate_decision`,
`tool_result`, `model_call`, `goal`, `usage_paused`, `usage_resumed`, `result` and `afk_report`).
It serializes to the identical JSON, so the [schema](schemas/headless-v1.schema.json) and the
`SCHEMA_VERSION` constant apply unchanged. `send` borrows the session, so one turn runs at a time,
and `/goal` works as it does headless.

- `init` is the first item of the first turn.
- Events emitted between turns, `usage_paused` and `usage_resumed`, arrive on the next turn.
- The trailing `afk_report`, assessed after the last turn of an AFK run, is not on any turn:
  `close()` returns it.
- A turn dropped before its `result` has the rest of its events skipped, so the next turn starts
  at its own first event.
- When the run itself has ended, for example because a usage window stayed spent and the run
  stopped, `send` returns a turn whose only item is `Err(EmbedError::Runtime)`.

`Session::interrupt_handle()` returns a cloneable `InterruptHandle`. `interrupt()` ends the running
turn with the same `result` Claude Agent SDK mode gives an interrupted turn
(`error_during_execution`, `the turn was interrupted`), and the next turn runs normally.

## Ending a session

`close()` stops accepting prompts, waits for the run to finish, fires the `session_end` hooks,
removes the session's scratch directory and returns the AFK report, if any. It returns an `Err` if the
run ended with a failure. Dropping a `Session` without `close()` cancels the running turn, closes the
input and lets the task end on its own, and removes the scratch directory once it has; no
`session_end` hook fires.

## What the builder cannot loosen

The builder has no switch for the guardian, no way around the deterministic floor, no `bypass`
mode and no way to loosen from project scope. Project settings load as tighten-only, and user
settings are read from the `config_dir` you supply. An embedded session takes headless semantics:
nobody is at a terminal, scheduling is off, repository-defined MCP servers start without asking,
and project hooks, servers and skill commands follow `[headless] require_trust` and the trust store.
The `/guardian-disable` escape hatch is interactive-only and does not exist here.

## Telemetry

Exporting `[telemetry]` is installed by the `fontaine` binary only (`telemetry::install` in
`rust/fontaine/src/lib.rs`). An embedded session's tracing spans go to whatever subscriber your
program installed; see [Telemetry](telemetry.md) for the span names and attributes.
