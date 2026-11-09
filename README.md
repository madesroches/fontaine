# Fontaine

A terminal AI coding agent for any model, with a guardian that judges risky tool calls, and an AFK
mode that runs unattended without YOLO.

[![CI](https://github.com/madesroches/fontaine/actions/workflows/ci.yml/badge.svg)](https://github.com/madesroches/fontaine/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/madesroches/fontaine?include_prereleases)](https://github.com/madesroches/fontaine/releases)
[![License](https://img.shields.io/github/license/madesroches/fontaine)](LICENSE)

**Auto mode.** A separate guardian model reviews each tool call that is not provably safe, using
your task and a short written policy. A deterministic floor of deny and ask rules, critical paths
and protected paths sits beneath every mode and cannot be argued with. If the guardian cannot
answer, the call is denied.

**AFK mode.** Not YOLO, and not an `auto` session that stalls waiting for you. It decides exactly as
`auto` does, but every would-prompt outcome becomes a denial with recovery guidance. When you
return, a report shows everything the agents tried, and a session temperature stops the agent tree
when denials pile up.

## Install

On Linux (x86_64) and macOS (Apple silicon and Intel):

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.sh | sh
```

On Windows (x86_64), in PowerShell or a Command Prompt:

```
powershell -ExecutionPolicy Bypass -c "irm https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.ps1 | iex"
```

Archives, source builds, signature verification, proxies and updating are in
[Installing Fontaine](docs/installing.md).

## Quickstart

With [Ollama](https://ollama.com) running and a model pulled (`ollama pull qwen3:4b`), create
`~/.config/fontaine/config.toml`:

```toml
[accounts.local]
kind = "ollama"
base_url = "http://localhost:11434"

[model]
model = "local/qwen3:4b"
```

Then run `fontaine` for the terminal UI, or `fontaine -p "explain what a Cargo workspace is"` for a
headless run. Other providers and accounts are in [Configuration](docs/configuration.md).

## Why Fontaine

- [AFK mode](docs/afk.md) lets you leave a session running and read what the agents tried when you return.
- [The guardian and auto mode](docs/guardian.md) fail closed, and the [scorecard](docs/evals.md) measures catch and false-deny rates per guardian model.
- [Permission modes and the floor](docs/permissions.md): six modes (`manual`, `acceptEdits`, `plan`, `dontAsk`, `auto`, `afk`) over one deterministic floor.
- [Subagents and task boards](docs/agents.md) run asynchronously, and a child is never looser than its parent.
- Claude Code compatibility: `CLAUDE.md` and `AGENTS.md` ([instructions](docs/instructions.md)), `.claude/` skills ([extensions](docs/extensions.md)), [plugins](docs/plugins.md), hooks and MCP servers.
- Any provider: Anthropic, OpenAI, Gemini, OpenRouter, Ollama and OpenAI-compatible endpoints, plus a Claude subscription ([configuration](docs/configuration.md)).

## Documentation

Read the docs at <https://madesroches.github.io/fontaine/>, or start from [docs/index.md](docs/index.md) in the repository.

## Project

- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Changelog](CHANGELOG.md)
- [License](LICENSE): Apache-2.0

This repository is under construction. The documentation describes what the current build does.
