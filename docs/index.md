# Fontaine documentation

Fontaine is a terminal AI coding agent that works with any model you configure. These pages
describe what the current build does; each page covers one topic.

## Start here

- [Architecture](architecture.md): the workspace layout, what each crate is responsible for, and
  how the pieces depend on each other.
- [Configuration](configuration.md): every settings key, the settings scope stack and its merge
  rules, workspace trust, model accounts, where files live, and the command-line flags that affect
  configuration.
- [Tools](tools.md): the built-in tools, their risk classes, workspace roots and what earns the
  read-only fast path, the shell and web fetch settings.
- [Permissions](permissions.md): the modes, the deterministic floor, the `Tool(specifier)` rules and
  allow rules, and the shell pre-classifier.
- [The guardian](guardian.md): what `auto` mode asks a separate model, what it sees, how every
  failure becomes a denial, how to choose the guardian model, and the container-mode profile.
- [Subagents](agents.md): the `agent` and `send_message` tools, background subagents, agent types
  and agent files, the permission-mode rules, budgets, and the agents panel.
- [User hooks](hooks.md): the `pre_tool_use`, `post_tool_use` and `permission_denied` hooks, their
  JSON input and output, and what a hook can and cannot decide.
- [MCP servers](mcp.md): declaring stdio and streamable-HTTP servers, workspace trust and
  per-server approval, and how MCP tools are gated.
- [Skills, agent files and output styles](extensions.md): the extension files, their
  frontmatter, slash commands and porting from `.claude/`.
- [The terminal UI](tui.md): keys, the status bar, approval prompts, the guardian's denials,
  `/denied`, `/login`, `/model`, `/guardian-model`, `/add-dir`, `/output-style`, `/help`,
  `/status`, `/permissions`, `/mcp`, `/hooks`, `/skills`, `/plan`, `/copy`, `/clear`, `/compact`,
  `/context`, the agents panel
  and the workspace trust prompt.
- [Headless runs](headless.md): `fontaine -p`, its flags, exit codes, and the versioned JSON output
  that CI and the eval runner consume.
- [Telemetry](telemetry.md): enabling export, the micromegas and OTLP backends, what is exported,
  and the privacy defaults.

## Evaluating the guardian

- [Guardian scorecard](evals.md): catch rate, false-deny rate and latency per guardian model, and
  the inclusion bar for the default-guardian list.
- [Golden dataset and eval runner](../evals/README.md): the case format, the categories, and how to
  run the offline scoring and the live guardian mode.

## Project

- [Contributing](../CONTRIBUTING.md): build and test commands, branch and commit rules, and how to
  submit golden-dataset cases.
- [Security policy](../SECURITY.md): how to report a vulnerability.
- [Changelog](../CHANGELOG.md): what changed in each release.
