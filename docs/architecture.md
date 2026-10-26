# Architecture

Fontaine is a Cargo workspace. The workspace manifest is `rust/Cargo.toml` and every crate sits
directly under `rust/`. The repository root holds only `rust/`, `evals/`, `docs/`, `tasks/` and
`research/`, plus the top-level project files.

## Workspace map

```
fontaine/
├── rust/
│   ├── fontaine/            binary and library: command line, headless runs, tracing and telemetry export
│   ├── fontaine-core/       configuration, trust, credentials, model accounts, the permission pipeline and rules, audit log, agent setup and turn runner, sessions, session ledger, system prompt
│   ├── fontaine-tools/      built-in tools, risk classes, workspace scope check, shell parsing and pre-classifier (preclassify), the gate seam and tool wrapper
│   ├── fontaine-guardian/   the guardian: request and verdict types, prompt builder, verdict cache, loop breaker; kept free of Fontaine dependencies
│   ├── fontaine-agents/     subagents: the agent tree, mode ladder, budgets, agent types, the `agent` and `send_message` tools
│   ├── fontaine-mcp/        MCP servers: transports and the wrapped registration of their tools
│   ├── fontaine-tui/        terminal UI
│   └── fontaine-evals/      eval runner: binary and library
├── evals/                   golden dataset (JSONL) and its format documentation
├── docs/                    product documentation, one topic per page
├── tasks/                   design and planning documents
└── research/                background analysis
```

## Crates

| Crate | Responsibility today |
|---|---|
| `fontaine` | The `fontaine` binary. `main.rs` is thin; `lib.rs` parses the command line, resolves the directories, loads configuration, installs the tracing subscriber and opens the `fontaine.session` span. `headless.rs` maps the core turn runner's events to the versioned output described in [Headless runs](headless.md) for `fontaine -p`. `src/telemetry/` is the only place a subscriber is built, and holds the two export backends described in [Telemetry](telemetry.md). |
| `fontaine-core` | The settings scope stack (`config`), config and data directory resolution (`dirs`), the `Tool(specifier)` permission rule syntax (`permission_rules`), the workspace trust store (`trust`), the credentials file (`credentials`: lookup, the writer and removal), the account operations behind the provider dialog (`accounts`: listing accounts with their sign-in state, adding one to the user settings file, storing and removing credentials, signing in and removing an account), owner-only file creation (`owner_only`), the permission decision tier enum (`tier`), the provider registry (`providers`: one module per account kind, with the device sign-in and token refresh a signed-in kind uses), and the model layer (`models`): config accounts mapped to provider clients and `<account>/<model>` references resolved to model handles. It also holds the user hooks (`user_hooks`), the extension files (`extensions`, `frontmatter`, `skills`), the child runner that starts subagents (`child_runner`), and the permission pipeline (`permissions`, with the floor checks in `floor`), the audit log (`audit`), the shared session state (`gate`), the session (`session`: the model context store, its compaction and clearing), what fills the context window (`context`), the session ledger (`ledger`), the system prompt (`prompt`), the workspace roots (`workspace`), the wrapped tool registry (`tools`), and the agent setup and turn runner that headless and the terminal UI share (`agent`, `turn`). The guardian's side of the core is the request builder that reads only the ledger (`adjudication`), the structural destructive-class derivation (`destructive`), the container probe (`containment`), and default-guardian selection and live model switching in `models`. |
| `fontaine-tools` | The built-in tools and their risk classes, the workspace scope check, shell parsing and the Tier-0.5 pre-classifier (`preclassify`), the gate interface and the wrapper that puts every tool behind it, the result pipeline and the built-in tool names that rules and agent toolsets may refer to. It depends on no other Fontaine crate. |
| `fontaine-guardian` | The guardian: a separate, tool-less model conversation that judges one tool call and answers with a typed verdict. It holds the `AdjudicationRequest` and `Verdict` types, the `RuleLabel` enum, the prompt assets and the prompt builder, the shipped default rule lists, the verdict cache, the loop breaker, the minimal profile and the ranked model list types. It depends on rig and `serde` but on no other Fontaine crate, so other harnesses can embed it; see [The guardian](guardian.md). |
| `fontaine-tui` | The terminal UI: transcript, input, approval prompts, the agents panel, the workspace trust prompt, the provider dialog (`provider_dialog`, opened by `/login`, and hosted by `setup` before the session when no main model resolves) and the status bar, driven by the core turn runner. See [The terminal UI](tui.md). |
| `fontaine-agents` | Subagent orchestration without the core: the live tree of agents (who runs, what each spent, the messages waiting), the permission-mode ladder, budgets, the agent-type registry, and the `agent` and `send_message` tools. It defines the `ChildRunner` and `RootLedger` interfaces, which `fontaine-core` implements, in the same way `fontaine-tools` defines the gate. See [Subagents](agents.md). |
| `fontaine-mcp` | Starts MCP servers over stdio or streamable HTTP and registers their tools through a sink that wraps every tool in the gate, always as `arbitrary`. See [MCP servers](mcp.md). |
| `fontaine-evals` | Loads and validates the golden dataset, scores canned verdicts, runs `pipeline` cases through the real permission pipeline, scores a live guardian model and writes the models that clear the inclusion bar, scores a typed-decision model with its eval-only `screen:` scorer, and captures and diffs Claude Code baselines. |

## Dependencies between crates

```
fontaine          -> fontaine-core, fontaine-tools, fontaine-tui
fontaine-evals    -> fontaine-core, fontaine-guardian, fontaine-tools
fontaine-tui      -> fontaine-core, fontaine-guardian, fontaine-tools, fontaine-agents
fontaine-core     -> fontaine-tools, fontaine-guardian, fontaine-mcp, fontaine-agents
fontaine-mcp      -> fontaine-tools
fontaine-agents   -> fontaine-tools (no fontaine-core: the core implements its interfaces)
fontaine-guardian -> (no Fontaine crates)
fontaine-tools    -> (no Fontaine crates)
```

There are no cycles. `fontaine-tools` never depends on `fontaine-core`.

## What a run does

`fontaine` parses its flags and loads configuration, then does one of two things:

- `fontaine -p "task"` runs the task headless; see [Headless runs](headless.md).
- Without `-p` it starts the terminal UI; see [The terminal UI](tui.md).

The steps of a run:

1. It parses its flags and opens the debug log if one was requested, validating `FONTAINE_LOG`.
2. Under a subscriber that only feeds the debug log, it resolves the config and data directories,
   loads and validates the settings scope stack, and emits a debug-level `config loaded` event
   carrying the resolved scope stack.
3. For a headless run it checks everything that needs no network: the flag combinations, the
   permission mode and the `--json-schema` value.
4. It installs the global tracing registry once: the debug layer plus, when `[telemetry]` is
   enabled, one export backend. With the micromegas backend it first turns on micromegas's
   per-thread streams, which must happen before any thread exists, and then builds the async
   runtime with the stream callbacks.
5. It opens the `fontaine.session` span, resolves `<account>/<model>` to a model handle (which may
   list an account's models), and drives the agent over that handle with the wrapped tool
   registry.
6. It closes the session span, flushes the exporter for at most two seconds, and exits with the
   run's code.

Any configuration or usage error exits with code 2 and a message on stderr; a run error exits
with code 1.

## The agent loop

The agent loop is rig's: Fontaine builds an agent from a model handle, a system prompt, a
conversation store and a tool registry, and runs it once per user message. It writes no loop of its
own. Every tool call crosses the gate before the tool runs, and every result crosses the result
pipeline before the model sees it:

```
                 user message
                      |
                      v
   +---------------------------------------+
   |  rig agent run  (model <-> tool calls) |
   +---------------------------------------+
        |  tool call                  ^  tool result
        v                             |
   +----------------------------------------------+
   | wrapped tool                                  |
   |   1. gate: allow (maybe with changed          |
   |      arguments) or deny with a reason         |
   |   2. on allow: the tool instance of the root  |
   |      that holds the call's path               |
   |   3. result pipeline                          |
   +----------------------------------------------+
        |
        v
   ls  read  glob  grep  write  edit  shell  webfetch  agent  skill  send_message  ask_user_question  mcp__*
```

A deny goes back to the model as the tool result and the run goes on. The registry is built at
session start (and rebuilt between turns by `/add-dir`); each file tool and `shell` has one confined
instance per workspace root, and `webfetch` has one. MCP tools join the registry as their servers
connect, wrapped by the same gate. A subagent gets a copy of the registry cut down to its type's
tools, so its calls reach the same gate. See [Tools](tools.md).

## The permission pipeline

The gate is the permission pipeline. It reads the session state (mode, workspace scope, rules and
grants, which the terminal UI can change between calls), decides, and writes one audit record per
decision before anything else sees it:

```
 tool call
    |
    v
 1. hooks (deny / ask / allow / rewrite the arguments)
 2. floor: deny rule -> [plan: block edits] -> ask rule -> critical path -> protected path
    |
    v   (the floor can only ask or deny, in every mode)
 3. read-only fast path: read-only tool, every path inside a workspace root -> allow
 4. shell pre-classifier: one literal simple command of the curated set -> allow
 5. allow rules (user trust scopes, session grants, always-allow rules) -> allow
 6. the mode: manual / acceptEdits / plan ask, dontAsk denies
    |
    v
 audit log -> tool_decision event -> gate_decision event / transcript
```

Asking is a call into an approval callback; headless runs and `dontAsk` turn every ask into a
denial. [Permissions](permissions.md) describes each step.

## Runs and the turn runner

`fontaine-core` builds the agent for both surfaces (`agent`) and runs one user message to its end
(`turn`), yielding typed events (assistant text, tool call, gate decision, tool result, usage) to
a callback that may stop the run between model calls. Headless prints the events as JSON lines; the
terminal UI renders them and answers approval prompts concurrently with the run.

## The session ledger

Each session keeps two stores, a task list file and a schedule file, in `<data dir>/sessions/<session id>/`:

- the **model context**, the conversation the model sees, kept in `logs/` by rig's memory store.
  Compaction rewrites it: when the conversation outgrows the model's context window, or the
  `[context] autocompact_window` setting, or when you run `/compact`, the older part is replaced
  by a summary the model wrote. `/clear` empties it.
- the **session ledger**, `ledger/ledger.jsonl`: an append-only record, written by the harness and
  never by the model. It holds the user's messages verbatim (a message typed while a turn runs is
  recorded when the agent receives it with a tool result, not when it is typed), mode changes, approvals and grants,
  denials, actions taken, `!` commands you ran in the terminal UI (`user_shell`, the command and its
  exit code), the workspace roots, the trust state, the repository's git remotes
  (read from the repository's own config, without credentials), the names of the `[env]` variables (never their values) and an index of the files present
  when the session started. The index respects `.gitignore` and holds at most 50,000 paths, after
  which it records that it was cut short.

- the **task list**, `todos.json`: the root agent's list from the `todo_write` tool, written on every
  change. Subagents keep their own lists in memory only, so a resumed session has the root's.

- the **schedule**, `schedule.json`: the session's [scheduled prompts](tui.md#scheduled-prompts)
  (`cron_create` tasks and `/loop`), written on every change. A task that comes due runs as a turn
  the harness starts, not as a user message; the ledger's `scheduled` records (`created`, `fired`,
  `deleted`, `expired`, with the task's id, who set it and its cron expression, never its prompt)
  show what ran and who scheduled it, and do not count toward what you asked for.

Compaction changes the first store only, and so does `/clear`, which empties the model context in
the same session and adds a `context_cleared` record to the ledger. The ledger keeps everything it
held; only the guardian's view of what you asked restarts after the record. A summary is model-written text and is never an input to a
safety decision; the ledger is the only record of what the human asked for. The ledger lives in its
own subdirectory because the memory store refuses a directory that holds a top-level `.jsonl` file.

### Resuming and forking

`--continue` and `--resume` reopen the stores of an earlier session: the conversation log, the task list, the schedule and the
ledger, whose records rebuild the view the guardian's requests are built from, and new records
continue the sequence. The session starts in the mode the settings resolve to and with no
"allow session" grants, which were never persisted. `--fork-session` copies the session directory, `todos.json` and `schedule.json` included, into a
session directory under a new id and opens the copy, leaving the original as it was; the audit log
of the copy starts empty. A resumed schedule keeps its tasks that are still worth running and
re-arms them from now, so nothing replays what came due while the session was closed; the rest
are dropped from the file and reported once at startup, when scheduling is enabled (see [Scheduled prompts](tui.md#scheduled-prompts)).

A session can carry a name, recorded in its ledger and changed by appending another; `--resume`
and `/resume` find a session by id, or by name among those started in the same directory, by
reading each ledger once. In the terminal UI `/resume` and `/branch` end the running session and
start the chosen one in the same process: the interface returns which session it wants, and the
caller opens it exactly as `--resume` would, with the telemetry, configuration and workspace-trust
decision carried over and the agents, MCP servers and scanner started again. `/branch` makes the
copy itself, before leaving, so a failure is shown in the session that is still running.

### Subagents in the ledger

A subagent tree has one source of intent, the human messages in the root session's ledger. The
ledger records when subagents start and end and when they message each other, but a spawn prompt or
a message is never recorded as something the human said.

## Models

Every model, for the session and for the guardian alike, is a rig completion model behind one
handle. `[accounts.<name>]` maps to one provider each, with one wire client, or several for a
kind that routes models over more than one wire, so two accounts of one kind keep separate
credentials and endpoints. Each kind is a module in the provider registry; resolution, login and
listing only talk to the registry. [Configuration](configuration.md#accountsname) describes the
kinds, the credential sources and model discovery.

### Prompt caching

Providers reuse a request prefix only when it is byte-identical from the start. Nothing before the
latest message therefore changes between the requests of a session: the system prompt and the tool
array are fixed, and conversation history is only appended. Runtime state (queued steering,
schedule notes, the task list, denial notices) reaches the model as appended messages or tool
results, never as an edit to the front. A change that has to edit the front is allowed only at an
explicit boundary and costs one lost cache read: `/add-dir`, `/output-style`, a model switch, an
MCP server connecting (including on-demand approval and a `tools/list_changed` refresh), `/clear`
and compaction. An MCP server's tools are sorted by name so the order does not depend on the
server's `tools/list`.

On the Anthropic Messages wire Fontaine asks for the cache explicitly (`prompt_caching` and
`cache_ttl` in [Configuration](configuration.md#accountsname)); the other wires cache by prefix
automatically. The cache read and write token counts of the main model are in the headless
[`usage`](headless.md) output.

## Tracing

Every crate instruments with the `tracing` facade only. No crate other than the `fontaine` binary
depends on a subscriber, so library crates stay embeddable: with no subscriber installed their
spans and events cost nothing. The export backends sit behind the binary's `telemetry-micromegas`
and `telemetry-otlp` features, both on by default. The binary's `pg2` feature builds the PromptGuard 2
tool-result scanner (candle and a tokenizer); it is off in a plain source build and on in release
binaries.

## The eval runner

`fontaine-evals` reads `evals/dataset/*.jsonl` and validates every case against the schema. With
`--guardian fixture` it scores the canned `fixture_verdict` of each selected case against its
`expected` outcome; with `--guardian none` it runs the `pipeline` cases through the real permission
pipeline (no guardian, non-`auto` modes) and scores the first decision. Both run offline. With
`--guardian live:<account>/<model>` it resolves the account through the same configuration and
credential loading as `fontaine -p`, routes `guardian` cases to the guardian and `pipeline` cases to
the pipeline under the case's mode, records each case's latency, and writes the models that clear
the inclusion bar to the shipped model list; see [the scorecard](evals.md). The
`baseline capture` and `baseline diff` subcommands compare the pipeline with Claude Code. The case
format and the runner flags are documented in [evals/README.md](../evals/README.md).

## Platforms

Linux, macOS and Windows are supported, and the test suite runs on all three. Files that hold
secrets or trust decisions are created readable by the current user only: mode `0600` on Unix and
a protected, current-user-only ACL on Windows.
