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

- Phase 4, the guardian: `auto` mode. A separate, tool-less model judges each call that nothing
  pre-approved and answers allow, deny with a labelled reason, or escalate to the human; the
  verdict is a typed extraction validated by code, and every outcome that is not a verdict
  (missing credential, provider error, timeout, malformed or out-of-enum verdict, a low-confidence
  allow on a destructive call) is a denial. Its request is built from the session ledger only, with
  a structurally derived destructive class, delimited untrusted blocks and redaction; a 60-second
  verdict cache, a loop breaker (3 in a row, 20 in total) and the seeded default rule lists
  adapted from Claude Code's auto-mode defaults.
- The `minimal` guardian profile for containers, loaded only with `--in-container` and a positive
  runtime probe.
- The terminal UI: `auto` joins the Shift+Tab cycle and is the default mode, guardian denials
  render as `[Rule] explanation`, `/denied` with a one-shot retry approval, `/model` and
  `/guardian-model` (live switch, saved to the user file), and a prompt for an unavailable pinned
  guardian (retry, pick another, or manual).
- Default guardian selection from the ranked list of validated models (`guardian_models.toml`,
  empty until a scorecard run fills it), with a main-model fallback and a one-time notice.
- Telemetry: the `fontaine.guardian` span, `query_source = guardian` on the guardian's model
  calls, and the `fontaine.guardian.latency` and `fontaine.guardian.fail_closed.count` metrics;
  audit and `gate_decision` records carry the guardian's model, profile, verdict, confidence,
  cache hit, latency and fail-closed reason; `result.usage.guardian` totals its tokens and cost.
- `fontaine-evals`: `--guardian live:<account>/<model>` scores a live guardian, records latency,
  and writes the models that clear the inclusion bar (catch rate 90%, false-deny rate 5%, p95 3 s)
  to `--models-file`; `baseline capture` now poses `auto` cases to `claude -p`.
- Release setup: cargo-dist builds and publishes binaries for Linux, macOS and Windows on a
  `vX.Y.Z` tag, and CI runs `cargo dist plan`.
- `docs/guardian.md` and the scorecard page `docs/evals.md`.

- Phase 5, extensibility. Parallel subagents: the `agent` tool (blocking, or `background: true`
  with the report delivered as a notification) and `send_message`, agent types from `[agents.types]`
  and agent files, a mode that is never looser than the parent's and follows it on every call, a
  `plan` parent blocking every child edit, `[agents] max_parallel`, per-subagent and session token
  and cost budgets that include the guardian's calls, a nesting limit of three layers, and a
  background-agents panel in the terminal UI (`/agents`, Ctrl-T) with kill and per-agent
  decisions. Headless runs wait for background subagents and count their spend.
- MCP servers over stdio and streamable HTTP from `[mcp.servers.*]`, registered as wrapped
  `arbitrary` tools named `mcp__<server>__<tool>`, refreshed on list changes; server annotations
  reach the guardian only as untrusted text. A repository's servers need workspace trust and the
  user's approval of each in the terminal UI.
- User hooks: `pre_tool_use`, `post_tool_use` and `permission_denied` command hooks with Claude
  Code's JSON input, output and exit codes; a repository's hook can tighten but never allow.
- Skills, agent files and output styles with Claude Code's frontmatter, the `skill` tool, skills as
  `/name` commands with arguments, `` !`cmd` `` context run as gated `shell` calls, `allowed-tools`
  lent for a turn, and `/output-style`.
- Sessions: `--continue`, `--resume <id>` and `--fork-session`; the `fontaine.session.count`
  metric carries `start_type` (`fresh`, `resume`, `continue`) and `headless`.
- The workspace trust prompt in the terminal UI, which records trust in `trust.toml`.
- Telemetry: `fontaine.subagent`, `fontaine.hook` and `fontaine.mcp.call` spans, `subagent` and
  `mcp_server_connection` events, `agent.id` on `tool_decision`, `query_source = subagent`.
- `docs/agents.md`, `docs/hooks.md`, `docs/mcp.md` and `docs/extensions.md`.

### Changed

- `[permissions] default_mode` is optional: the terminal UI starts in `auto` and a headless run in
  `manual`. `auto` is now accepted from the user file, `--settings` and `--permission-mode`; shared
  project settings still cannot set it.
- In `plan` mode, consequential non-edit calls go to the guardian instead of the prompt; edits stay
  blocked whatever it says.
- In `auto`, broad allow rules are suspended by program: interpreters, shells (including `pwsh`,
  `powershell`, `cmd`), `sudo`/`doas` and argv launchers, versioned names (`python3.12`) and
  wildcard rules that cover them (`shell(cargo *)`, `shell(npm *)`).
- Headless `total_cost_usd` and `--max-budget-usd` include guardian spend, retried and fail-closed
  adjudications included.
- `fontaine` without `-p` starts the terminal UI and exits with code 2 when there is no terminal.
- A file-tool call outside the workspace roots is denied with an `/add-dir` hint instead of
  prompting; Fontaine's own config and data directories are protected paths; path case is folded
  on macOS as well as Windows.
- The critical-path check also covers `rm` reached through shell runners and launchers
  (`bash -c`, `su -c`, `env -S`, …), `find -exec`/`xargs`, full-path and `.exe` program names,
  command names built from expansions, and MSYS-style `/c/...` targets.
- Approval prompts show the full call, scrollable, and ignore answer keys for a moment after they
  open so type-ahead cannot answer them.
- Permission rules now carry their source scope, and a rule listed in two scopes is kept once per
  scope.
- Update the pinned Rust toolchain and minimum `rust-version` to 1.99.1, and fix the clippy
  lints it adds.
- Headless `gate_decision` events carry `agent_id`; `total_cost_usd` and `--max-budget-usd` include
  the subagents' model calls; `result.usage.subagents` is filled in.
- The `fontaine.session.count` attribute `start_type` is now `fresh`, `resume` or `continue`, and the
  surface moved to a `headless` attribute (it was `headless` or `interactive`).
- `gen_ai.tool.name` on tool spans, like the other MCP names, reads `third-party` for an MCP tool
  unless `log_tool_details` is on.
- Notices (for example the pause of `auto`) go to stderr in a headless run instead of being
  dropped.

### Removed

- The test-only `echo` tool and its `FONTAINE_SPIKE_ECHO_TOOL` switch.
