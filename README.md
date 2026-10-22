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

## Documentation

- [Documentation index](docs/index.md)
- [Configuration](docs/configuration.md)
- [Architecture](docs/architecture.md)
- [Golden dataset and eval runner](evals/README.md)

## Project

- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Changelog](CHANGELOG.md)
- [License](LICENSE): Apache-2.0
