# Changelog

All notable changes are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/) (0.x until the first stable release).

## [Unreleased]

### Added

- Agent and models
  - The agent loop runs over rig. Models come from `[accounts.<name>]` entries mapped to provider
    clients (Anthropic, OpenAI, Gemini, Ollama, OpenAI-compatible endpoints) and are named
    `<account>/<model>`, with model discovery. Credentials come from an environment variable or
    the stored-credentials file, and `fontaine auth login [--telemetry]` stores them.
  - Sessions live in a per-session directory under the data directory, with the conversation
    persisted by rig's memory store, between-runs compaction by a summarizing call and an
    append-only ledger. `--session-id`, `--continue`, `--resume <id>` and `--fork-session` select
    or branch a session.
  - A versioned system prompt steers the agent toward the dedicated read tools, denial handling
    and the workspace roots, layered with `FONTAINE.md` from the user config directory and the
    primary root.
  - A model request that cannot connect names its transport cause (connection refused, reset,
    TLS, DNS, timeout) without the request URL (#88).
- Configuration
  - A scope stack of user settings, shared project settings, project-local settings
    (`.fontaine/settings.local.toml`, never committed) and `--settings`, with strict, scope-aware
    validation. An unknown key is a configuration error (exit code 2).
  - Project scope can only tighten (deny and ask additions); loosening keys load only from the
    user-trust scopes (the user file, project-local settings and `--settings`).
  - `FONTAINE_CONFIG_DIR` and `FONTAINE_DATA_DIR` override the directories, files that hold
    secrets are created owner-only, and workspace trust is recorded in `trust.toml`.
  - `[permissions] default_mode` is optional: the terminal UI starts in `auto` and a headless run
    in `manual`. `auto` is accepted from the user file, `--settings` and `--permission-mode`;
    both project scopes (shared and project-local) accept only `manual` and `dontAsk`.
- Tools
  - The built-in tools `read`, `ls`, `glob`, `grep`, `write`, `edit`, `shell` and `webfetch`. Every
    tool has a risk class and sits behind one gate, and each call emits a `fontaine.tool` span and
    a `tool_result` telemetry event.
  - Workspace roots from the primary directory, the repeatable `--add-dir` flag, `/add-dir` and
    `[permissions] additional_directories`, with one confined tool instance per root.
  - `shell` child processes do not inherit telemetry credentials, configured accounts' API key
    variables or the scanner token variable. On Windows the shell runs through Git Bash, found on
    `PATH`, in the standard install locations or through `FONTAINE_GIT_BASH_PATH`, and commands
    reach it unchanged, backslashes included (#78).
  - `webfetch` sends the User-Agent `fontaine/<version> (+https://github.com/madesroches/fontaine)`
    (#73) and `Accept: text/markdown, text/html, */*` (#85).
- Permissions
  - The modes `manual`, `acceptEdits`, `plan`, `dontAsk` and `auto`; Shift+Tab cycles `manual`,
    `acceptEdits`, `plan` and `auto`, and `dontAsk` is set through settings or
    `--permission-mode`. `plan` blocks every edit and sends consequential non-edit calls to the guardian.
  - A deterministic floor in every mode: deny rules, ask rules, critical-path `rm`/`rmdir` (also
    reached through shell runners, launchers, `find -exec`, `xargs` and MSYS-style paths) and
    protected paths, including Fontaine's own config and data directories. A file-tool call outside
    the workspace roots is denied with an `/add-dir` hint.
  - Allow, ask and deny rules that carry their source scope, session grants, and "always allow"
    written to the user settings file. In `auto`, broad code-execution allow rules are suspended:
    a bare `shell` or `agent`, interpreters, shells, package runners (`cargo run`, `npm run`,
    `npx`, `make`), `sudo`/`doas` and argv launchers, and any wildcard that can reach them.
  - A shell pre-classifier on a real shell parser with a per-command flag allowlist and
    `[shell_preclassifier] allow_extra`; anything it cannot fully parse is never pre-approved: it
    falls through to the rules and the mode, and goes to the guardian in `auto` and `plan`.
  - An append-only audit log per session, `tool_decision` events, `fontaine.permission` spans, the
    `fontaine.tool_decision.count` metric, headless `gate_decision` events and `result.denials`,
    and `--allowed-tools` and `--disallowed-tools`.
- Guardian
  - `auto` mode: a separate, tool-less model judges each call that nothing pre-approved and
    answers allow, deny with a labelled reason, or escalate to the human. Every outcome that is
    not a verdict (missing credential, provider error, timeout, malformed verdict, a
    low-confidence allow on a destructive call) is a denial.
  - Its request is built from the session ledger only, with a structurally derived destructive
    class, delimited untrusted blocks and redaction; a 60-second verdict cache, a loop breaker
    (3 in a row, 20 in total) and seeded default rule lists adapted from Claude Code's auto-mode
    defaults.
  - The `minimal` profile for containers, loaded only with `--in-container` and a positive runtime
    probe.
  - Default guardian selection from the ranked list of validated models (`guardian_models.toml`,
    empty until a scorecard run fills it), with a main-model fallback and a one-time notice;
    `/guardian-model` switches it live and saves the choice.
  - Denials render as `[Rule] explanation`; `/denied` lists them and offers a one-shot retry
    approval; an unavailable pinned guardian prompts to retry, pick another or fall back to
    manual.
  - The tool-result scanner, selected by `[guardian] scanner` (`off` by default, `pg2`, `llm` or
    `command`; user file only). A result it flags, or cannot scan, is kept and followed by a note
    telling the agent it is possible prompt injection or unscanned and suspect; subagent reports
    pass the same stage. `pg2` runs PromptGuard 2 (86M) locally through candle, scoring a whole
    result in 512-token windows, behind the `pg2` cargo feature (on in release binaries,
    `--features pg2` from source). Its gated weights are never bundled: a local
    `scanner_model_path`, or a download with the token in `scanner_hf_token_env` (default
    `HF_TOKEN`, always removed from `shell` and hook environments), with the Llama 4 Community
    License notice shown on every startup in every mode. `llm` uses the guardian's model;
    `command` runs your own program (stdin `{tool, content}`, stdout `{verdict, reason}`).
- Terminal UI and headless
  - The terminal UI (`fontaine` without `-p`) draws inline, so finished output goes into the
    terminal's scrollback: transcript, streaming output, scrollable approval prompts, mode cycling,
    `/model`, `/guardian-model`, `/output-style` and the workspace trust prompt. It exits with code
    2 when there is no terminal.
  - `!` runs a shell command as the user, outside the permission pipeline and recorded in the
    ledger and the audit log; `@` completes workspace file paths; Up/Down and Ctrl-R recall and
    search a persistent prompt history; messages typed during a turn are queued and delivered with
    the agent's next tool result (#46).
  - `fontaine -p` with `--output-format text|json|stream-json`, `--input-format stream-json`,
    `--include-partial-messages`, `--max-turns`, `--max-budget-usd`, `--json-schema`,
    `--system-prompt`, `--append-system-prompt`, `--model` and `--permission-mode`. The output is
    a versioned contract with a JSON Schema in `docs/schemas/headless-v1.schema.json`; exit codes
    are 0, 1 and 2. `total_cost_usd` and `--max-budget-usd` include guardian and subagent spend, and
    `result.usage` breaks tokens and cost down into `main`, `guardian` and `subagents`. Notices go
    to stderr.
- Extensibility
  - Parallel subagents: the `agent` tool (blocking, or `background: true` with the report
    delivered as a notification) and `send_message`, agent types from `[agents.types]` and agent
    files, a mode that is never looser than the parent's, a nesting limit of three layers,
    `[agents] max_parallel`, per-subagent and session token and cost budgets, and a
    background-agents panel (`/agents`, Ctrl-T) with kill and per-agent decisions. Headless runs
    wait for background subagents.
  - MCP servers over stdio and streamable HTTP from `[mcp.servers.*]`, registered as wrapped
    tools named `mcp__<server>__<tool>`. Server annotations reach the guardian only as untrusted
    text, and a repository's servers need workspace trust and the user's approval of each.
  - User hooks: `pre_tool_use`, `post_tool_use` and `permission_denied` command hooks with Claude
    Code's JSON input, output and exit codes; a repository's hook can tighten but never allow.
  - Skills, agent files and output styles with Claude Code's frontmatter, the `skill` tool, skills
    as `/name` commands with arguments, `` !`cmd` `` context run as gated `shell` calls, and
    `allowed-tools` lent for a turn.
- Telemetry
  - Backends: micromegas (native span trees) and OTLP/HTTP, with token and cost metrics, an
    `api_request` event, a target allowlist, opt-in content that is redacted and truncated, and a
    capped flush on exit. Exported are Fontaine's own target (`fontaine.tool`,
    `fontaine.permission`, `fontaine.guardian`, `fontaine.subagent`, `fontaine.hook`,
    `fontaine.mcp.call` and the session, decision and guardian metrics) and the model layer's
    completion and agent targets.
  - `query_source` separates the guardian's, the scanner's and subagents' model calls;
    `tool_result` carries `scanner_verdict` and `scanner_latency_ms`; `fontaine.session.count`
    carries `start_type` and `headless`.
  - `subagent` and `mcp_server_connection` events. MCP server and tool names read `third-party`
    unless `log_tool_details = true`.
  - An optional owner-only debug file (`--debug-file`, `FONTAINE_LOG`).
- Evals
  - A golden dataset and the `fontaine-evals` runner: `--guardian fixture` (offline),
    `--guardian none` (cases through the real pipeline) and `--guardian live:<account>/<model>`
    (a live guardian or scanner, with latency; writes the models that clear the inclusion bar to
    `--models-file`). `baseline capture` and `baseline diff` compare cases with `claude -p`.
  - A live run streams: a progress line per case on stderr, each case's result line as it is
    scored, and a flushed `<out>.partial.jsonl` sidecar that `--resume` continues from after an
    interruption. A case the runner cannot pose is `UNSCORED` instead of aborting the run, and
    `--out`'s parent directory is created and checked before the first case (#94).
- Distribution
  - Release binaries for Linux, macOS and Windows built with `pg2` by cargo-dist on a `vX.Y.Z`
    tag, each archive carrying the license, readme and changelog; `cargo install --git
    https://github.com/madesroches/fontaine fontaine` from source.
  - Documentation under `docs/`, a README quickstart, contributing guide and security policy.
