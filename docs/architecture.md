# Architecture

Fontaine is a Cargo workspace. The workspace manifest is `rust/Cargo.toml` and every crate sits
directly under `rust/`. The repository root holds only `rust/`, `evals/`, `docs/`, `tasks/` and
`research/`, plus the top-level project files.

## Workspace map

```
fontaine/
├── rust/
│   ├── fontaine/            binary and library: command line, tracing setup
│   ├── fontaine-core/       configuration, workspace trust, credentials, permission rule syntax
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
| `fontaine` | The `fontaine` binary. `main.rs` is thin; `lib.rs` parses the command line, resolves the directories, loads configuration, installs the tracing subscriber and opens the `fontaine.session` span. `src/telemetry/` is the only place a subscriber is built. |
| `fontaine-core` | The settings scope stack (`config`), config and data directory resolution (`dirs`), the `Tool(specifier)` permission rule syntax (`permission_rules`), the workspace trust store (`trust`), the read-only credentials lookup (`credentials`), owner-only file creation (`owner_only`), and the permission decision tier enum (`tier`). |
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

`fontaine` currently starts, configures itself and exits:

1. It parses its flags (`--settings`, `--debug-file`).
2. It opens the debug log if one was requested, and validates `FONTAINE_LOG`.
3. Under a subscriber that only feeds the debug log, it resolves the config and data directories,
   loads and validates the settings scope stack, and emits a debug-level `config loaded` event
   carrying the resolved scope stack.
4. It installs the global tracing registry once: the debug layer plus a slot for an export layer.
   No exporter is built, so that slot is empty.
5. It opens the `fontaine.session` span and exits with code 0.

Any configuration or usage error exits with code 2 and a message on stderr.

## Tracing

Every crate instruments with the `tracing` facade only. No crate other than the `fontaine` binary
depends on a subscriber, so library crates stay embeddable: with no subscriber installed their
spans and events cost nothing.

## The eval runner

`fontaine-evals` reads `evals/dataset/*.jsonl`, validates every case against the schema, and scores
the canned `fixture_verdict` of each selected case against its `expected` outcome. It runs offline
and needs no model and no guardian. The case format and the runner flags are documented in
[evals/README.md](../evals/README.md).

## Platforms

Linux, macOS and Windows are supported, and the test suite runs on all three. Files that hold
secrets or trust decisions are created readable by the current user only: mode `0600` on Unix and
a protected, current-user-only ACL on Windows.
