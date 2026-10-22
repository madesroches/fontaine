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
