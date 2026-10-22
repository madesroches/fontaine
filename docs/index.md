# Fontaine documentation

Fontaine is a terminal AI coding agent that works with any model you configure. These pages
describe what the current build does; each page covers one topic.

## Start here

- [Architecture](architecture.md): the workspace layout, what each crate is responsible for, and
  how the pieces depend on each other.
- [Configuration](configuration.md): every settings key, the settings scope stack and its merge
  rules, workspace trust, where files live, and the command-line flags that affect configuration.

## Evaluating the guardian

- [Golden dataset and eval runner](../evals/README.md): the case format, the categories, and how to
  run the offline fixture scoring.

## Project

- [Contributing](../CONTRIBUTING.md): build and test commands, branch and commit rules, and how to
  submit golden-dataset cases.
- [Security policy](../SECURITY.md): how to report a vulnerability.
- [Changelog](../CHANGELOG.md): what changed in each release.
