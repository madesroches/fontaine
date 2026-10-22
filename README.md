# Fontaine

Fontaine is a terminal AI coding agent, written in Rust, that works with any model you configure:
Anthropic, OpenAI, Gemini, or any OpenAI-compatible endpoint such as OpenRouter, Ollama, LM
Studio, vLLM and LiteLLM.

Its signature feature is an auto mode in which a separate guardian agent reviews each tool call that
is not provably safe, using your task and a short written policy, and answers allow, deny or ask
the human. A deterministic floor of deny rules, ask rules, critical paths and protected paths sits
beneath every mode and cannot be argued with.

This repository is under construction. The documentation describes what the current build does.

## Build from source

You need the Rust toolchain; `rust/rust-toolchain.toml` pins the version and `rustup` installs it
on first use.

```
cd rust
cargo build
cargo test
cargo run -p fontaine
```

## Quickstart: a local agent with Ollama

With [Ollama](https://ollama.com) running and a model pulled (`ollama pull qwen3:4b`), create
`~/.config/fontaine/config.toml`:

```toml
[accounts.local]
kind = "ollama"
base_url = "http://localhost:11434"

[model]
model = "local/qwen3:4b"
```

Then run a task from `rust/`:

```
cargo run -p fontaine -- -p "explain what a Cargo workspace is"
```

The answer goes to stdout and the exit code is 0 when the run completed. Add `--output-format
stream-json` for one JSON event per line. Other providers, several accounts, credentials and
OpenAI-compatible endpoints such as OpenRouter, LM Studio, vLLM and LiteLLM are in
[Configuration](docs/configuration.md); the flags and the output format are in
[Headless runs](docs/headless.md).

## Permission modes

Every tool call is adjudicated before it runs; the mode decides what happens to a call that nothing
pre-approved. A deterministic floor (deny and ask rules, critical paths, protected paths) applies
in every mode.

| Mode | Behavior |
|---|---|
| `manual` (default) | Asks you. |
| `acceptEdits` | Runs edits inside the workspace; asks for the rest. |
| `plan` | Never edits; asks for other consequential calls. |
| `dontAsk` | Denies anything not pre-approved. |
| `auto` | Not available yet. |

Run `fontaine` for the terminal UI (Shift+Tab cycles the mode) or `fontaine -p` for a headless run.

## Documentation

- [Documentation index](docs/index.md)
- [Configuration](docs/configuration.md)
- [Permissions](docs/permissions.md)
- [The terminal UI](docs/tui.md)
- [Headless runs](docs/headless.md)
- [Telemetry](docs/telemetry.md)
- [Architecture](docs/architecture.md)
- [Golden dataset and eval runner](evals/README.md)

## Project

- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Changelog](CHANGELOG.md)
- [License](LICENSE): Apache-2.0
