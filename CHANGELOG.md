# Changelog

All notable changes are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/) (0.x until the first stable release).

## [Unreleased]

### Added

- Phase 0 scaffold: the Cargo workspace (`fontaine`, `fontaine-core`, `fontaine-tools`,
  `fontaine-guardian`, `fontaine-agents`, `fontaine-mcp`, `fontaine-tui`, `fontaine-evals`), a
  `fontaine` binary with `--settings` and `--debug-file`, and a pinned toolchain.
- Settings scope stack with strict, scope-aware validation, workspace trust store, read-only
  credentials lookup, directory resolution with `FONTAINE_CONFIG_DIR` and `FONTAINE_DATA_DIR`,
  owner-only file creation, and the `[telemetry]` required-inputs rule.
- Tracing facade with an optional owner-only debug file (`--debug-file`, `FONTAINE_LOG`) and an
  inert export layer.
- Golden dataset seed and the offline `fontaine-evals` fixture runner.
- Documentation tree under `docs/`, root README, contributing guide and security policy.
- GitHub Actions CI on Linux, macOS and Windows: format, clippy, tests, `cargo deny` and the offline
  eval runs.
- Model layer: `[accounts.<name>]` mapped to rig provider clients (Anthropic, OpenAI, Gemini,
  Ollama, OpenAI-compatible endpoints), `<account>/<model>` resolution with model discovery,
  credentials from an environment variable or the stored-credentials file, and
  `fontaine auth login [--telemetry]`.
- Headless surface `fontaine -p` with `--output-format text|json|stream-json`,
  `--input-format stream-json`, `--include-partial-messages`, `--max-turns`, `--max-budget-usd`,
  `--json-schema`, `--system-prompt`, `--append-system-prompt`, `--model` and `--permission-mode`;
  exit codes 0, 1 and 2; the output is a versioned contract with a JSON Schema in
  `docs/schemas/headless-v1.schema.json`.
- Telemetry backends: micromegas (native span trees) and OTLP/HTTP, with token and cost metrics,
  an `api_request` event, a target allowlist, opt-in content that is redacted and truncated, and a
  capped flush on exit.
- Documentation: model accounts and provider examples, headless runs, telemetry, and a README
  quickstart.
- Phase 2, agent runtime and tools: `fontaine -p` now runs rig's agent over a wrapped tool
  registry (`read`, `ls`, `glob`, `grep`, `write`, `edit`, `shell`, `webfetch`). Every tool has a
  risk class and sits behind one gate; this build's gate is deny-by-default and allows only
  read-only calls whose paths are inside the workspace roots and that no `deny` or `ask` rule
  names. A result pipeline (empty for now) sits after every tool, and each call emits a
  `fontaine.tool` span and a `tool_result` telemetry event.
- Workspace roots: the repeatable `--add-dir` flag and `[permissions] additional_directories`,
  with one confined tool instance per root; entries that are missing or not directories are
  skipped with a warning. On Windows the shell runs through Git Bash, found on `PATH`, in the
  standard install locations or through `FONTAINE_GIT_BASH_PATH`.
- The `shell` tool's child processes no longer inherit telemetry credentials, configured accounts'
  API key variables or the scanner token variable.
- Sessions: `--session-id`, a per-session directory under the data directory with the
  conversation persisted by rig's memory store, between-runs compaction by a summarizing call, and
  an append-only session ledger of user messages, decisions, roots, trust, git remotes and a
  pre-session file index.
- A versioned system prompt (steering toward the dedicated read tools, denial handling, the
  workspace roots) layered with `FONTAINE.md` from the user config directory and the primary root.
- `docs/tools.md`, and extensions to the architecture, configuration, headless and telemetry pages.
- Phase 3, permission pipeline and terminal UI: a real permission pipeline replaces the
  deny-by-default stub. Modes `manual`, `acceptEdits`, `plan` and `dontAsk`; a deterministic floor
  (deny rules, ask rules, critical-path `rm`/`rmdir`, protected paths) in every mode; the read-only
  fast path; a shell pre-classifier on a real shell parser with a per-command flag allowlist and
  `[shell_preclassifier] allow_extra`; allow rules, session grants and "always allow" written to
  the user settings file; a `pre_tool_use` hook slot; an append-only audit log per session;
  `tool_decision` events, `fontaine.permission` spans and the `fontaine.tool_decision.count`
  metric; headless `gate_decision` events and `result.denials`; `--allowed-tools` and
  `--disallowed-tools`.
- The terminal UI (`fontaine` without `-p`): transcript, streaming output, approval prompts, mode
  cycling with Shift+Tab and `/add-dir`. Agent setup and the turn runner moved into
  `fontaine-core`, shared by headless and the UI.
- `fontaine-evals`: `--guardian none` scores `pipeline` cases through the real pipeline, and
  `baseline capture` / `baseline diff` compare it with `claude -p`; CI runs the `none` mode.
- `docs/permissions.md`, `docs/tui.md` and a manual-mode cassette test (not yet recorded).

### Changed

- `fontaine` without `-p` starts the terminal UI and exits with code 2 when there is no terminal.
- Permission rules now carry their source scope, and a rule listed in two scopes is kept once per
  scope.

### Removed

- The test-only `echo` tool and its `FONTAINE_SPIKE_ECHO_TOOL` switch.
