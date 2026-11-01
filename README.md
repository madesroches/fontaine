# Fontaine

Fontaine is a terminal AI coding agent, written in Rust, that works with any model you configure:
Anthropic, OpenAI, Gemini, OpenRouter (with prompt caching), or any OpenAI-compatible endpoint such
as Ollama, LM Studio, vLLM and LiteLLM.

Its signature feature is an auto mode in which a separate guardian agent reviews each tool call that
is not provably safe, using your task and a short written policy, and answers allow, deny or ask
the human. A deterministic floor of deny rules, ask rules, critical paths and protected paths sits
beneath every mode and cannot be argued with.

This repository is under construction. The documentation describes what the current build does.

## Install

Binaries for Linux, macOS and Windows are attached to each
[GitHub Release](https://github.com/madesroches/fontaine/releases). Unpack the archive for your
platform and put `fontaine` on your `PATH`; the archive also holds the license, readme and
changelog. With the Rust toolchain you can instead install from
source:

```
cargo install --git https://github.com/madesroches/fontaine fontaine
```

Fontaine is not published to crates.io. Release binaries include the PromptGuard 2 tool-result
scanner; a source build has it only with `--features pg2` (`cargo install --features pg2 ...`).
Its weights are a gated download under the Llama 4 Community License, and the scanner is off by
default: see [Scanning tool results](docs/guardian.md#scanning-tool-results).

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

Fontaine asks Ollama for a 16384-token context window on every call; the
[`context_tokens`](docs/configuration.md#accountsname) key changes it.

Then run a task from `rust/`:

```
cargo run -p fontaine -- -p "explain what a Cargo workspace is"
```

The answer goes to stdout and the exit code is 0 when the run completed. Add `--output-format
stream-json` for one JSON event per line. Other providers, several accounts, credentials and
OpenRouter and OpenAI-compatible endpoints such as LM Studio, vLLM and LiteLLM are in
[Configuration](docs/configuration.md); the flags and the output format are in
[Headless runs](docs/headless.md).

## Permission modes

Every tool call is adjudicated before it runs; the mode decides what happens to a call that nothing
pre-approved. A deterministic floor (deny and ask rules, critical paths, protected paths) applies
in every mode.

| Mode | Behavior |
|---|---|
| `manual` | Asks you. The headless default. |
| `acceptEdits` | Runs edits inside the workspace; asks for the rest. |
| `plan` | Never edits; the guardian judges other consequential calls. |
| `dontAsk` | Denies anything not pre-approved. |
| `auto` | The guardian decides. The terminal UI's default. |

Run `fontaine` for the terminal UI (Shift+Tab cycles the mode) or `fontaine -p` for a headless run.

## Auto mode

In `auto` a call is judged by the guardian only after everything that can be decided without a
model has been: your deny and ask rules, the critical and protected paths, the read-only fast path
inside your workspace (secret files such as `.env` excluded), a spawn of a subagent (read-only or not; each call the child makes is judged on its own), a small set of read-only shell commands, and file-tool edits to tracked,
new or session-created files and to the scratch directory never reach it. For the rest,
the guardian is a separate model with no tools. It reads what you asked for (your own messages,
taken from a record the agent cannot edit), the call and a short written policy, and answers
**allow**, **deny** with a reason the agent sees, or **escalate** to ask you.

If it cannot answer, the call is denied: no credential, a provider error, a timeout, a malformed
verdict, or an allow it is not confident about on a destructive call. Repeated denials hand the
session back to you, and `/denied` lets you approve one refused call yourself. By default the
guardian is the best benchmark-validated model your accounts offer; `/guardian-model` changes it.
The [guardian page](docs/guardian.md) has the threat model and the details, and the
[scorecard](docs/evals.md) has the numbers.

## Documentation

- [Documentation index](docs/index.md)
- [Configuration](docs/configuration.md)
- [Permissions](docs/permissions.md)
- [The guardian](docs/guardian.md)
- [Guardian scorecard](docs/evals.md)
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
