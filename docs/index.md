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
- [The terminal UI](tui.md): keys, the status bar, approval prompts and `/add-dir`.
- [Headless runs](headless.md): `fontaine -p`, its flags, exit codes, and the versioned JSON output
  that CI and the eval runner consume.
- [Telemetry](telemetry.md): enabling export, the micromegas and OTLP backends, what is exported,
  and the privacy defaults.

## Evaluating the guardian

- [Golden dataset and eval runner](../evals/README.md): the case format, the categories, and how to
  run the offline fixture scoring.

## Project

- [Contributing](../CONTRIBUTING.md): build and test commands, branch and commit rules, and how to
  submit golden-dataset cases.
- [Security policy](../SECURITY.md): how to report a vulnerability.
- [Changelog](../CHANGELOG.md): what changed in each release.
