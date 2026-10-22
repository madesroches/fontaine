# Architecture

Fontaine is a Cargo workspace. The workspace manifest is `rust/Cargo.toml` and every crate sits
directly under `rust/`. The repository root holds only `rust/`, `evals/`, `docs/`, `tasks/` and
`research/`, plus the top-level project files.

## Workspace map

```
fontaine/
├── rust/
│   ├── fontaine/            binary and library: command line, headless runs, tracing and telemetry export
│   ├── fontaine-core/       configuration, trust, credentials, model accounts, permission rules, sessions, session ledger, system prompt
│   ├── fontaine-tools/      built-in tools, risk classes, workspace scope check, the gate seam and tool wrapper
│   ├── fontaine-guardian/   guardian vocabulary; kept free of Fontaine dependencies
│   ├── fontaine-agents/     subagent orchestration (skeleton)
│   ├── fontaine-mcp/        MCP server wiring (skeleton)
│   ├── fontaine-tui/        terminal UI (skeleton)
│   └── fontaine-evals/      eval runner: binary and library
├── evals/                   golden dataset (JSONL) and its format documentation
├── docs/                    product documentation, one topic per page
├── tasks/                   design and planning documents
└── research/                background analysis
```

## Crates

| Crate | Responsibility today |
|---|---|
| `fontaine` | The `fontaine` binary. `main.rs` is thin; `lib.rs` parses the command line, resolves the directories, loads configuration, installs the tracing subscriber and opens the `fontaine.session` span. `headless.rs` runs `fontaine -p`, drives the agent and prints the versioned output described in [Headless runs](headless.md); `auth.rs` is `fontaine auth login`. `src/telemetry/` is the only place a subscriber is built, and holds the two export backends described in [Telemetry](telemetry.md). |
| `fontaine-core` | The settings scope stack (`config`), config and data directory resolution (`dirs`), the `Tool(specifier)` permission rule syntax (`permission_rules`), the workspace trust store (`trust`), the credentials file (`credentials`: lookup and the `auth login` writer), owner-only file creation (`owner_only`), the permission decision tier enum (`tier`), and the model layer (`models`): config accounts mapped to provider clients and `<account>/<model>` references resolved to model handles. It also holds the session (`session`: the model context store and its compaction), the session ledger (`ledger`), the system prompt (`prompt`), the workspace roots (`workspace`), the permission gate (`gate`) and the wrapped tool registry (`tools`). |
| `fontaine-tools` | The built-in tools and their risk classes, the workspace scope check, the gate interface and the wrapper that puts every tool behind it, the result pipeline and the built-in tool names that rules and agent toolsets may refer to. It depends on no other Fontaine crate. |
| `fontaine-guardian` | The `RuleLabel` enum: the fixed label set a guardian verdict can carry. It has no dependencies, so other harnesses can embed it. |
| `fontaine-agents`, `fontaine-mcp`, `fontaine-tui` | Skeleton crates that reserve their place in the dependency graph. |
| `fontaine-evals` | Loads and validates the golden dataset and scores canned verdicts against the expected outcomes. |

## Dependencies between crates

```
fontaine          -> fontaine-core, fontaine-tools, fontaine-tui
fontaine-evals    -> fontaine-core, fontaine-guardian
fontaine-tui      -> fontaine-core
fontaine-core     -> fontaine-tools, fontaine-guardian, fontaine-mcp, fontaine-agents
fontaine-mcp      -> fontaine-tools
fontaine-agents   -> fontaine-tools
fontaine-guardian -> (no Fontaine crates)
fontaine-tools    -> (no Fontaine crates)
```

There are no cycles. `fontaine-tools` never depends on `fontaine-core`.

## What a run does

`fontaine` parses its flags and loads configuration, then does one of three things:

- `fontaine auth login` stores a credential and exits.
- `fontaine -p "task"` runs the task headless; see [Headless runs](headless.md).
- Without `-p` it starts and exits with code 0; the interactive terminal UI is not part of this
  build.

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
   ls  read  glob  grep  write  edit  shell  webfetch
```

A deny goes back to the model as the tool result and the run goes on. The registry is built once
per session; each file tool and `shell` has one confined instance per workspace root, and
`webfetch` has one. See [Tools](tools.md).

## The session ledger

Each session keeps two stores in `<data dir>/sessions/<session id>/`:

- the **model context**, the conversation the model sees, kept in `logs/` by rig's memory store.
  Compaction rewrites it: when the conversation outgrows the model's context window, the older part
  is replaced by a summary the model wrote.
- the **session ledger**, `ledger/ledger.jsonl`: an append-only record, written by the harness and
  never by the model. It holds the user's messages verbatim, mode changes, approvals and grants,
  denials, actions taken, the workspace roots, the trust state, the repository's git remotes
  (read from the repository's own config, without credentials) and an index of the files present
  when the session started. The index respects `.gitignore` and holds at most 50,000 paths, after
  which it records that it was cut short.

Compaction changes the first store only. A summary is model-written text and is never an input to a
safety decision; the ledger is the only record of what the human asked for. The ledger lives in its
own subdirectory because the memory store refuses a directory that holds a top-level `.jsonl` file.

## Models

Every model, for the session and for the guardian alike, is a rig completion model behind one
handle. `[accounts.<name>]` maps to one provider client each, so two accounts of one kind keep
separate credentials and endpoints. [Configuration](configuration.md#accountsname) describes the
kinds, the credential sources and model discovery.

## Tracing

Every crate instruments with the `tracing` facade only. No crate other than the `fontaine` binary
depends on a subscriber, so library crates stay embeddable: with no subscriber installed their
spans and events cost nothing. The export backends sit behind the binary's `telemetry-micromegas`
and `telemetry-otlp` features, both on by default.

## The eval runner

`fontaine-evals` reads `evals/dataset/*.jsonl`, validates every case against the schema, and scores
the canned `fixture_verdict` of each selected case against its `expected` outcome. It runs offline
and needs no model and no guardian. The case format and the runner flags are documented in
[evals/README.md](../evals/README.md).

## Platforms

Linux, macOS and Windows are supported, and the test suite runs on all three. Files that hold
secrets or trust decisions are created readable by the current user only: mode `0600` on Unix and
a protected, current-user-only ACL on Windows.
