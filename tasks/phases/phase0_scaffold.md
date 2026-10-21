# Phase 0 — Scaffold (eval first — it is the product)

**Status:** not started · **Depends on:** — · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

Read the master plan's Overview, Design (master §Workspace layout, `tasks/design/config.md` §Config), Decisions, and
`tasks/design/testing.md` Testing Strategy before implementing. This phase builds the eval harness **before** the
thing it scores — per analysis-strategic-verdict amendment A1, the public guardian
scorecard is the differentiating asset, so its harness is built first, not last.

## Steps

1. Cargo workspace (`rust/Cargo.toml`, micromegas layout, `members = ["*"]` with `exclude = ["target", ".*"]` so a second build with `rust/target/` present still resolves; `[workspace.package]` sets `edition = "2024"`
   and `rust-version = "1.97.1"`, and `rust/rust-toolchain.toml` pins channel ≥ 1.97.1 to match rig
   0.45.0; CI installs that toolchain), crate skeletons under `rust/`
   (`fontaine`, `fontaine-core`, `fontaine-tools`, `fontaine-guardian`, `fontaine-agents`,
   `fontaine-mcp`, `fontaine-tui`), `clap` CLI entry (`fontaine`), tracing facade (below), CI
   (`.github/workflows/ci.yml`, GitHub Actions, matrix `ubuntu-latest` / `macos-latest` /
   `windows-latest`, each from `rust/`: `cargo fmt --check`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test`, then the offline eval fixture run from
   step 2), root `.gitignore` (at least `rust/target/` and `.fontaine/settings.local.toml`), config loading (`fontaine-core/src/config.rs`) with the schema in
   `tasks/design/config.md` §Config — `config.rs` defines and scope-validates the COMPLETE schema (every section incl. `[accounts]`, `[model]`, `[guardian]` with scanner keys, `[agents]`, `[mcp]`, `[hooks]`, `[telemetry]`; later phases wire behavior to already-parsed keys) — as the settings scope stack: reserved managed slot (empty), CLI `--settings` overlay,
   project-local `.fontaine/settings.local.toml` (user trust only while `git` reports it untracked
   or ignored; otherwise project scope), project `.fontaine/settings.toml`, user file; scalars by
   precedence, lists by union; every key a scope may not set (per the `tasks/design/config.md` scope table, incl.
   project and project-local `default_mode` limited to `manual`/`dontAsk`) rejected at load; `default_mode = "auto"` from any scope is a config error (exit 2) until Phase 4 step 2 lifts it, mirroring the CLI rule. The primary
   workspace root is the canonicalized launch cwd; config is read only from it. Rule strings in
   `[permissions] deny`/`ask`/`allow` are parsed into the `Tool(specifier)` grammar at load
   (`fontaine-core/src/permission_rules.rs`; an unparseable rule is a config error, exit 2; matching
   lands in Phase 3). Directory resolution (`fontaine-core/src/dirs.rs`, `dirs` crate: config dir
   `dirs::config_dir()/fontaine`, data dir `dirs::data_dir()/fontaine`, overrides `FONTAINE_CONFIG_DIR` /
   `FONTAINE_DATA_DIR`) and an owner-only file helper (0600 on Unix, current-user-only ACL on Windows).
   Workspace trust store (`fontaine-core/src/trust.rs`): read/write
   `<data dir>/trust.toml` keyed by canonical root, and an `is_trusted(root)` query the
   Phase-5 loaders of project executables call. Tracing facade
   (`rust/fontaine/src/telemetry/mod.rs`): the `tracing_subscriber` registry, the `--debug-file` /
   `FONTAINE_LOG` layer (owner-only), an inert export slot filled in Phase 1, a `fontaine.session` span;
   `[telemetry]` parsed with its scope rule (project scopes may only set `enabled = false`) and its
   required-inputs rule (`enabled = true` needs `endpoint` and an ingestion key — `api_key_env` or a
   stored credential; either missing is a config error, exit 2).
2. Eval golden-dataset scaffolding + CI runner skeleton (`evals/`): dataset format (~150+ cases:
   benign dev commands, each seeded attack class from Claude Code's default-block list, injection
   attempts *inside tool args*, ambiguous intent cases, Tier-0.5 adversarial shell cases — see
   `tasks/design/testing.md` Testing Strategy — and adversarial spawn/injection-propagation cases: a subagent result
   carrying injection text that must hit the tool-result scanner, a README tricking the parent
   into spawning with an escalated toolset — plus escape-focused container cases for the
   `[guardian] profile = "minimal"` column: `mount /dev/sda1`, docker.sock write, cgroup
   `release_agent`, bind-mount symlink escape → deny/escalate; routine in-container workspace ops
   → proceed); the runner is the bin crate `rust/fontaine-evals`, reading `evals/dataset/*.jsonl` (one JSON
   object per case: `id`, `kind`, `category`, `profile`, `task_intent`, `tool`, `args`, `expected`,
   `fixture_verdict`, `mode` (default `manual`), `workspace` (files, symlinks and `cwd` materialized in a
   temp root), plus optional request-context fields; `kind` = `guardian` | `pipeline` |
   `scanner` selects the component a case scores — field list and per-kind scoring in
   `tasks/design/testing.md` Testing Strategy; `pipeline` `expected` ∈ `allow` | `deny` | `prompt`, `expected_tier` from the tier enum in `tasks/design/permissions.md` §Core data flow) and scoring any guardian model
   config; CI runs `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture`
   from `rust/` — offline, deterministic verdict fixtures — so it is green before the guardian
   exists.
3. Docs — start the documentation tree under `docs/` (its own directory — the single home for
   all prose, structured one-topic-per-page with relative cross-links so it can be lifted into a
   website later; no site tooling in v1): `docs/index.md` (landing/TOC), `docs/architecture.md`
   seeded with the workspace map + crate responsibilities, and `docs/configuration.md` seeded
   from the step-1 config schema (every key: meaning, per-scope settability, default), the settings
   scope stack and its merge rules, workspace trust, and `[telemetry]` (off by default; enabling
   requires the ingestion URL and key). Also
   `evals/README.md` defining the golden-dataset format — the dataset is a public asset shipping
   with the scorecard, so its format doc lands with it. Root `README.md` stays the thin front
   door (pitch, install, quickstart — the quickstart leads with a local Ollama agent + guardian
   setup) linking into `docs/` — not a second copy of it. Add a short root `CONTRIBUTING.md`
   (build/test commands, branch and commit rules, how to submit golden-dataset cases and red-team
   bypasses). Create `CHANGELOG.md` (Keep a Changelog format, with an `## [Unreleased]` section) with the first entry. Add the docs check `rust/fontaine-core/tests/docs.rs` (see Tests): it parses and schema-validates `toml` blocks only — no phase-gated value rules, no model resolution/discovery, no env/credential checks.

## Files created or modified

`.gitignore`; `rust/Cargo.toml`; `rust/rust-toolchain.toml`; crate skeletons for every `rust/fontaine-*` crate; `rust/fontaine/src/main.rs`;
`rust/fontaine/src/telemetry/mod.rs`; `rust/fontaine-core/src/{config,dirs,permission_rules,trust}.rs`; `evals/` (dataset + README.md); `rust/fontaine-evals/` (runner); `CHANGELOG.md`; `CONTRIBUTING.md`;
`docs/{index,architecture,configuration}.md`; `README.md`; `rust/fontaine-core/tests/docs.rs`; `.github/workflows/ci.yml`.

## Tests owned by this phase

- **config** (`tasks/design/testing.md` Testing Strategy): workspace = canonicalized launch cwd; project config
  discovered only at `<workspace>/.fontaine/settings{,.local}.toml` (a settings file in a parent directory is
  ignored); precedence, project-can-only-tighten enforcement —
  including that `[guardian] profile` is rejected from project scope (user trust scopes only, it is a
  loosening knob) and only `full`/`minimal` validate: no config value disables the guardian. Every key `tasks/design/config.md` §Config scope
  table denies to project scope (`[accounts]`, `[model]`, `[guardian]` model/timeout/threshold/cache, `[agents]` budgets,
  `[agents.types]`, …) is rejected from project scope. Scope stack: CLI > project-local > project
  > user precedence for scalars and union for lists; a tracked `settings.local.toml` loads as project
  scope; endpoint/credential keys rejected from project-local; project and project-local
  `default_mode` accept only `manual`/`dontAsk` (`"plan"`, `"acceptEdits"`, `"auto"` rejected from both); `[permissions] allow` / `additional_directories`
  rejected from project scope; unparseable rule strings rejected; `default_mode = "auto"` rejected (exit 2) from user, project-local and CLI scope too until Phase 4.
- **dirs**: `FONTAINE_CONFIG_DIR` / `FONTAINE_DATA_DIR` override the `dirs` defaults (all tests use temp
  dirs); owner-only files are 0600 on Unix and current-user-only on Windows (`#[cfg(windows)]` ACL test, compile-checked locally with the windows-gnu target and executed in the windows CI job).
- **trust store**: trust recorded for one root does not trust another; the record is written under
  the data dir, never inside the workspace.
- **telemetry config**: every `[telemetry]` key rejected from project and project-local scope except
  `enabled = false`; `enabled = true` without `endpoint`, or without a resolvable ingestion key (no
  `api_key_env` and no stored credential, or the named env var unset/empty), is a config error (exit
  2); disabled means no exporter — with `enabled = false` nothing is built even when
  `MICROMEGAS_TELEMETRY_URL` / `OTEL_EXPORTER_OTLP_ENDPOINT` are set.
- Golden-dataset schema validation; runner green against verdict fixtures.
- **docs** (`rust/fontaine-core/tests/docs.rs`, run by `cargo test`): every ```toml block in `docs/*.md` parses and schema-validates (as user-scope settings; no phase-gated value rules such as the pre-Phase-4 `default_mode = "auto"` error, no model resolution or discovery, no env/credential checks); every relative link in `docs/`, `README.md` and `evals/README.md` resolves; any `TODO` / `TBD` in those files fails the test.

## Definition of done

- Local offline gate: `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test` green from `rust/`, plus `cargo clippy --all-targets --target x86_64-pc-windows-gnu` (or `cargo check --all-targets --target x86_64-pc-windows-gnu`) where the toolchain supports it.
- Needs a human (not runnable by the agent, which cannot push): push the branch and confirm the CI matrix is green on ubuntu, macOS and Windows.
- `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture` (from `rust/`)
  passes on the fixture set, offline, with no guardian in the tree.
- `docs/index.md`, `docs/architecture.md`, `docs/configuration.md`, `evals/README.md` exist with
  real content; `rust/fontaine-core/tests/docs.rs` is green (toml blocks parse and schema-validate, links resolve, no `TODO`/`TBD`); `docs/configuration.md` covers the scope
  stack, workspace trust and `[telemetry]` (incl. its required endpoint and key).
- The config, trust-store and telemetry-config tests above are green; no exporter is built in this
  phase.
- `CHANGELOG.md` exists with the Phase 0 entry; `CONTRIBUTING.md` exists.
- Log the phase in the master plan's Progress log.
