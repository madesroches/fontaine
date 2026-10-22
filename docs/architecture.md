# Architecture

Fontaine is a Cargo workspace. The workspace manifest is `rust/Cargo.toml` and every crate sits
directly under `rust/`. The repository root holds only `rust/`, `evals/`, `docs/`, `tasks/` and
`research/`, plus the top-level project files.

## Workspace map

```
fontaine/
├── rust/
│   ├── fontaine/            binary and library: command line, headless runs, tracing and telemetry export
│   ├── fontaine-core/       configuration, workspace trust, credentials, model accounts, permission rule syntax
│   ├── fontaine-tools/      tool name registry (and, as it grows, tool risk metadata)
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
| `fontaine` | The `fontaine` binary. `main.rs` is thin; `lib.rs` parses the command line, resolves the directories, loads configuration, installs the tracing subscriber and opens the `fontaine.session` span. `headless.rs` runs `fontaine -p` and prints the versioned output described in [Headless runs](headless.md); `auth.rs` is `fontaine auth login`. `src/telemetry/` is the only place a subscriber is built, and holds the two export backends described in [Telemetry](telemetry.md). |
| `fontaine-core` | The settings scope stack (`config`), config and data directory resolution (`dirs`), the `Tool(specifier)` permission rule syntax (`permission_rules`), the workspace trust store (`trust`), the credentials file (`credentials`: lookup and the `auth login` writer), owner-only file creation (`owner_only`), the permission decision tier enum (`tier`), and the model layer (`models`): config accounts mapped to provider clients and `<account>/<model>` references resolved to model handles. |
| `fontaine-tools` | The built-in tool names that rules and agent toolsets may refer to. It depends on no other Fontaine crate. |
| `fontaine-guardian` | The `RuleLabel` enum: the fixed label set a guardian verdict can carry. It has no dependencies, so other harnesses can embed it. |
| `fontaine-agents`, `fontaine-mcp`, `fontaine-tui` | Skeleton crates that reserve their place in the dependency graph. |
| `fontaine-evals` | Loads and validates the golden dataset and scores canned verdicts against the expected outcomes. |

## Dependencies between crates

```
fontaine          -> fontaine-core, fontaine-tui
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
   list an account's models), and drives the agent over that handle.
6. It closes the session span, flushes the exporter for at most two seconds, and exits with the
   run's code.

Any configuration or usage error exits with code 2 and a message on stderr; a run error exits
with code 1.

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
