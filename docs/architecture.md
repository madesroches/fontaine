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
│   ├── fontaine-guardian/   guardian vocabulary; kept free of Fontaine dependencies
│   ├── fontaine-agents/     subagent orchestration (skeleton)
│   ├── fontaine-mcp/        MCP server wiring (skeleton)
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
| `fontaine` | The `fontaine` binary. `main.rs` is thin; `lib.rs` parses the command line, resolves the directories, loads configuration, installs the tracing subscriber and opens the `fontaine.session` span. `headless.rs` maps the core turn runner's events to the versioned output described in [Headless runs](headless.md) for `fontaine -p`; `auth.rs` is `fontaine auth login`. `src/telemetry/` is the only place a subscriber is built, and holds the two export backends described in [Telemetry](telemetry.md). |
| `fontaine-core` | The settings scope stack (`config`), config and data directory resolution (`dirs`), the `Tool(specifier)` permission rule syntax (`permission_rules`), the workspace trust store (`trust`), the credentials file (`credentials`: lookup and the `auth login` writer), owner-only file creation (`owner_only`), the permission decision tier enum (`tier`), and the model layer (`models`): config accounts mapped to provider clients and `<account>/<model>` references resolved to model handles. It also holds the permission pipeline (`permissions`, with the floor checks in `floor`), the audit log (`audit`), the shared session state (`gate`), the session (`session`: the model context store and its compaction), the session ledger (`ledger`), the system prompt (`prompt`), the workspace roots (`workspace`), the wrapped tool registry (`tools`), and the agent setup and turn runner that headless and the terminal UI share (`agent`, `turn`). |
| `fontaine-tools` | The built-in tools and their risk classes, the workspace scope check, shell parsing and the Tier-0.5 pre-classifier (`preclassify`), the gate interface and the wrapper that puts every tool behind it, the result pipeline and the built-in tool names that rules and agent toolsets may refer to. It depends on no other Fontaine crate. |
| `fontaine-guardian` | The `RuleLabel` enum: the fixed label set a guardian verdict can carry. It has no dependencies, so other harnesses can embed it. |
| `fontaine-tui` | The terminal UI: transcript, input, approval prompts and the status bar, driven by the core turn runner. See [The terminal UI](tui.md). |
| `fontaine-agents`, `fontaine-mcp` | Skeleton crates that reserve their place in the dependency graph. |
| `fontaine-evals` | Loads and validates the golden dataset, scores canned verdicts, runs `pipeline` cases through the real permission pipeline, and captures and diffs Claude Code baselines. |

## Dependencies between crates

```
fontaine          -> fontaine-core, fontaine-tools, fontaine-tui
fontaine-evals    -> fontaine-core, fontaine-guardian, fontaine-tools
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
   ls  read  glob  grep  write  edit  shell  webfetch
```

A deny goes back to the model as the tool result and the run goes on. The registry is built at
session start (and rebuilt between turns by `/add-dir`); each file tool and `shell` has one confined
instance per workspace root, and `webfetch` has one. See [Tools](tools.md).

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

`fontaine-evals` reads `evals/dataset/*.jsonl` and validates every case against the schema. With
`--guardian fixture` it scores the canned `fixture_verdict` of each selected case against its
`expected` outcome; with `--guardian none` it runs the `pipeline` cases through the real permission
pipeline (no guardian, non-`auto` modes) and scores the first decision. Both run offline. The
`baseline capture` and `baseline diff` subcommands compare the pipeline with Claude Code. The case
format and the runner flags are documented in [evals/README.md](../evals/README.md).

## Platforms

Linux, macOS and Windows are supported, and the test suite runs on all three. Files that hold
secrets or trust decisions are created readable by the current user only: mode `0600` on Unix and
a protected, current-user-only ACL on Windows.
