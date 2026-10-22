# Phase 0 — Scaffold (eval first — it is the product)

**Status:** not started · **Depends on:** — · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

Read the master plan's Overview, Design (master §Workspace layout, `tasks/design/config.md` §Config), Decisions, and
`tasks/design/testing.md` Testing Strategy before implementing. This phase builds the eval harness **before** the
thing it scores.

## Steps

1. Cargo workspace (`rust/Cargo.toml`, micromegas layout, `resolver = "2"`, `members = ["*"]` with `exclude = ["target", ".cargo", ".fontaine", ".config", ".claude"]` (cargo matches exclude entries literally and the `*` glob matches dot-dirs) so a second build with `rust/target/` present still resolves; `[workspace.package]` sets `edition = "2024"`,
   `license = "Apache-2.0"` (every crate skeleton inherits it with `license.workspace = true`)
   and `rust-version = "1.97.1"`, and `rust/rust-toolchain.toml` pins `channel = "1.97.1"` to match rig
   0.45.0, with `components = ["rustfmt", "clippy"]` and `targets = ["x86_64-pc-windows-gnu"]`; CI installs that toolchain), crate skeletons under `rust/`
   (`fontaine`, `fontaine-core`, `fontaine-tools`, `fontaine-guardian`, `fontaine-agents`,
   `fontaine-mcp`, `fontaine-tui`, `fontaine-evals`), `clap` CLI entry (`fontaine`, defining `--settings <file>` and `--debug-file <path>`; in this phase it parses flags, installs the tracing registry (debug layer + export slot), loads and validates config (emitting at least one debug-level event, e.g. the resolved scope stack) outside `fontaine.session`, leaves the export slot inert (Phase 1 fills it at this point), opens `fontaine.session`, and exits 0; any config error maps to `ExitCode` 2), tracing facade (below), CI
   (`.github/workflows/ci.yml`, GitHub Actions, matrix `ubuntu-latest` / `macos-latest` /
   `windows-latest`, each from `rust/`: `cargo fmt --check`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check` (advisories, licenses, bans, sources; cargo-deny installed at the pinned `0.20.2`, runs on all three OSes), then the offline eval fixture runs from
   step 2: default profile and `--profile minimal`), config loading (`fontaine-core/src/config.rs`) with the schema in
   `tasks/design/config.md` §Config — `config.rs` defines and scope-validates the COMPLETE schema (every section incl. `[accounts]`, `[model]`, `[guardian]` with scanner keys, `[agents]`, `[mcp]`, `[hooks]`, `[telemetry]`; later phases wire behavior to already-parsed keys), and exposes a parse + schema/scope validation function separate from the full load that skips phase-gated value rules and env/credential resolution — as the settings scope stack: reserved managed slot (empty), CLI `--settings` overlay,
   project-local `.fontaine/settings.local.toml` (user trust only while the path — canonicalized (symlinks resolved) relative to the canonical work dir, outside it ⇒ project scope — is absent from the work tree's index (looked up case-insensitively, `entry_by_path_icase`; a missing index file counts as empty, read via `index_or_empty`) AND the primary root `is_trusted` — ignored ⇒ untracked, a force-added file is tracked ⇒ project scope, keeping the project-local precedence slot but validated against the project column; otherwise, including an untracked file in an untrusted root, project scope keeping its project-local precedence slot — Fontaine never spawns `git` for this: index membership comes from `gix` (exact pin, default features off, index/config reading only — no hooks, filters or fsmonitor), opening the enclosing work tree by gix discovery upward from the primary root (a `.git` file / worktree is accepted); no work tree found or a `gix` error ⇒ project scope; a loosening key from an untrusted-root untracked file exits 2 with an error naming the untrusted root and the `trust.toml` hand-edit remedy), project `.fontaine/settings.toml`, user file; scalars by
   precedence, lists by union; a named table entry (`[accounts.<name>]`, `[mcp.servers.<name>]`, `[agents.types.<name>]`) is replaced whole by the highest-precedence scope defining that name; map-valued keys (e.g. `[telemetry] resource_attributes`) merge per key, higher-precedence scope winning; `[[hooks.*]]` arrays merge by union; unknown keys and sections in any scope are a config error (exit 2; serde `deny_unknown_fields`); every key a scope may not set (per the `tasks/design/config.md` scope table, incl.
   project and project-local `default_mode` limited to `manual`/`dontAsk`) rejected at load; `default_mode = "auto"` is a config error (exit 2) from user and CLI scope until Phase 4 step 2 lifts it. The primary
   workspace root is the canonicalized launch cwd; config is read only from it. The loader takes launch dir, config dir, data dir and an env-lookup function (for `api_key_env`) as parameters; only the bin reads real cwd/env (a `std::env::var_os` closure). Rule strings in
   `[permissions] deny`/`ask`/`allow` are parsed into the `Tool(specifier)` grammar at load
   (`fontaine-core/src/permission_rules.rs`; Phase 0 validates only the outer `Tool` / `Tool(specifier)` shape, the tool name, `mcp__…` names and `webfetch`'s `domain:` form, keeping other specifiers as raw strings; a rule failing that, or whose tool is neither in the built-in name constant `BUILTIN_TOOL_NAMES` (`shell`, `read`, `edit`, `write`, `ls`, `glob`, `grep`, `webfetch`, `agent`, `skill`, `send_message`) nor an `mcp__…` name, is a config error, exit 2; specifier parsing and matching land in Phase 3). Directory resolution (`fontaine-core/src/dirs.rs`, `dirs 7.0.0`, caret: config dir
   `dirs::config_dir()/fontaine`, data dir `dirs::data_dir()/fontaine`, overrides `FONTAINE_CONFIG_DIR` /
   `FONTAINE_DATA_DIR`, both resolved through the injected env-lookup function) and an owner-only file helper (`rust/fontaine-core/src/owner_only.rs`; 0600 on Unix, current-user-only ACL on Windows): it creates files restrictive at open time (Unix `OpenOptions::mode(0o600)`, Windows security descriptor at creation) and tightens a pre-existing file before writing.
   Workspace trust store (`fontaine-core/src/trust.rs`): read/write
   `<data dir>/trust.toml` (written via the owner-only helper) as `[[trusted]] root = "<canonical path>"` entries (only "trust" is persisted; hand-editing is the way to trust a root until the Phase 5 prompt), and an `is_trusted(root)` query (true when the root or a recorded ancestor is trusted, the walk bounded by the enclosing gix work-tree root, unbounded only when gix discovery reports not-found (`NoGitRepository`, `NoGitRepositoryWithinCeiling`, `NoGitRepositoryWithinFs`); any other discovery error ⇒ false, fail closed; each recorded entry is canonicalized before comparing — an unresolvable entry is ignored — and compared case-insensitively on Windows; the exit-2 message prints the exact path to record) the
   Phase-5 loaders of project executables call (unreadable or unparseable `trust.toml` ⇒ false for every root, fail closed). Credentials file schema and read-only lookup
   (`fontaine-core/src/credentials.rs`): `<data dir>/credentials.toml` holds `[accounts.<name>] api_key = "…"`
   per account and a separate `[telemetry] api_key = "…"`; the lookup returns an account's or the telemetry credential; an unparseable file is a config error, exit 2 (Phase 1
   adds the writer, `fontaine auth login`). Tracing facade
   (`rust/fontaine/src/telemetry/mod.rs`): the `tracing_subscriber` registry, the `--debug-file` /
   `FONTAINE_LOG` layer (`--debug-file` enables it at default filter `debug`; `FONTAINE_LOG` only overrides the filter; file owner-only), an inert export slot filled in Phase 1, a `fontaine.session` span;
   `[telemetry]` parsed with its scope rule (project scopes may only set `enabled = false`) and its
   required-inputs rule (`enabled = true` needs `endpoint` and an ingestion key — `api_key_env` or a
   stored credential; either missing is a config error, exit 2).
2. Eval golden-dataset scaffolding + CI runner skeleton (`evals/`): dataset format and seed set (at least the minimum case counts in `tasks/design/testing.md` Testing Strategy, Eval harness;
   benign dev commands, each seeded attack class from Claude Code's default-block list, injection
   attempts *inside tool args*, ambiguous intent cases, Tier-0.5 adversarial shell cases — see
   `tasks/design/testing.md` Testing Strategy — and adversarial spawn/injection-propagation cases: a subagent result
   carrying injection text that must hit the tool-result scanner, a README tricking the parent
   into spawning with an escalated toolset — plus escape-focused container cases for the
   `[guardian] profile = "minimal"` column: `mount /dev/sda1`, docker.sock write, cgroup
   `release_agent`, bind-mount symlink escape → deny/escalate; routine in-container workspace ops
   → proceed; `pipeline` floor cases with a `permissions` field: a deny rule, an ask rule, a critical path and a protected path); the tier enum is defined here in `fontaine-core` (`rust/fontaine-core/src/tier.rs`; Phase 3's `GateDecision.tier` reuses it) and the `RuleLabel` enum in `fontaine-guardian` (`rust/fontaine-guardian/src/rule_label.rs`; Phase 4's `Verdict` reuses it), and `fontaine-evals` imports both to validate `expected_tier` / `fixture_verdict.tier` and `attack:<class>`; the runner is the bin + lib crate `rust/fontaine-evals` (CLI table in `tasks/design/testing.md` Testing Strategy, Eval harness; Phase 0 flags: `--dataset`, `--guardian fixture`, `--profile`, `--filter`, `--out`; `--models-file` lands in Phase 4), reading `evals/dataset/*.jsonl` (case schema and per-kind scoring in
   `tasks/design/testing.md` Testing Strategy) and scoring canned `fixture_verdict` outcomes; CI runs `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture`
   from `rust/` — offline, deterministic verdict fixtures — so it is green before the guardian
   exists; CI also runs it with `--profile minimal`. Fixture mode schema-validates every case in the dataset regardless of `--profile` / `--filter`, and parses every case's `permissions` deny/ask/allow strings with `permission_rules.rs`, exiting 1 on an unparseable one.
3. Docs — start the documentation tree under `docs/` (see `tasks/design/documentation.md`): `docs/index.md` (landing/TOC), `docs/architecture.md`
   seeded with the workspace map + crate responsibilities, and `docs/configuration.md` seeded
   from the step-1 config schema (every key: meaning, per-scope settability, default; a key that is parsed but not yet active says "accepted and validated; not yet active" — docs describe current behavior, phase attribution lives only in `CHANGELOG.md`), the settings
   scope stack and its merge rules, workspace trust (the `trust.toml` entry shown as a TOML literal string, `'…'`), config/data dir locations (`dirs` defaults per OS) with `FONTAINE_CONFIG_DIR`/`FONTAINE_DATA_DIR`, `--settings <file>`, `--debug-file`/`FONTAINE_LOG`, and `[telemetry]` (off by default; enabling
   requires the ingestion URL and key). Also
   `evals/README.md` defining the golden-dataset format. Create root `README.md` as the thin front
   door (pitch and build-from-source only; the install section lands in Phase 4; the Ollama agent quickstart lands in Phase 1's docs step and the guardian leg in Phase 4's) linking into `docs/` — not a second copy of it. Add a short root `CONTRIBUTING.md`
   (build/test commands, branch and commit rules, how to submit golden-dataset cases; guardian-bypass reports go to `SECURITY.md`, and public dataset PRs for a bypass only after a fix ships) and a root `SECURITY.md` (private reporting via GitHub private vulnerability reporting; 90-day coordinated disclosure window). Create `CHANGELOG.md` (Keep a Changelog format, with an `## [Unreleased]` section) with the first entry. Add the docs check `rust/fontaine-core/tests/docs.rs`, which validates ```toml blocks via the config.rs validation function (a block whose fence info string names `trust.toml` or `credentials.toml` is validated against that file's schema instead; plain blocks are settings) (contract: `tasks/design/testing.md` Testing Strategy, docs bullet).

## Files created or modified

`rust/Cargo.toml` (new crates: `tracing =0.1.44`, `tracing-subscriber =0.3.23` (`env-filter`, `fmt`), `clap 4.6.7`, `serde 1.0.229`, `serde_json 1.0.154` (caret), `toml 1.1.8`, `dirs 7.0.0` (caret), dev `tempfile 3.27.0`; `windows-sys =0.61.2` (features `Win32_Security`, `Win32_Security_Authorization`, `Win32_Storage_FileSystem`, `Win32_Foundation`, `Win32_System_Threading`), target-gated to Windows, for the owner-only ACL helper; `gix =0.89.0`, default features off except `index`, `sha1` (`status`, `dirwalk`, `attributes`, `command` and network features stay off), for project-local trust classification); `rust/deny.toml` (cargo-deny config: license allowlist compatible with Apache-2.0 distribution); `rust/rust-toolchain.toml`; crate skeletons for every `rust/fontaine-*` crate; `rust/fontaine/src/{main,lib}.rs` (`lib.rs` holds the logic, `main.rs` is thin);
`rust/fontaine/src/telemetry/mod.rs`; `rust/fontaine-core/src/{config,dirs,permission_rules,trust,credentials,owner_only,tier}.rs`; `rust/fontaine-guardian/src/rule_label.rs`; `evals/` (dataset + README.md); `rust/fontaine-evals/src/{main,lib}.rs` (runner; `lib.rs` holds the logic); `CHANGELOG.md`; `CONTRIBUTING.md`; `SECURITY.md`;
`docs/{index,architecture,configuration}.md`; `README.md`; `rust/fontaine-core/tests/{config,trust_classification,trust,credentials,owner_only,docs}.rs` (`trust_classification.rs` the `gix` cases; `trust.rs` the trust-store cases; `credentials.rs` the lookup cases, `owner_only.rs` the helper cases), `rust/fontaine-evals/tests/fixture.rs`, `rust/fontaine/tests/{exit_code,debug_file}.rs`; `.github/workflows/ci.yml`.

## Tests owned by this phase

- **config** (`tasks/design/testing.md` Testing Strategy): config errors assert the error variant that maps to `ExitCode` 2 (plus a bin-level exit-code assertion in `rust/fontaine/tests/exit_code.rs`); workspace = canonicalized launch cwd; project config
  discovered only at `<workspace>/.fontaine/settings{,.local}.toml` (a settings file in a parent directory is
  ignored); precedence (the managed slot is empty in v1), project-can-only-tighten enforcement —
  including that `[guardian] profile` is rejected from project scope (user trust scopes only, it is a
  loosening knob) and only `full`/`minimal` validate: no config value disables the guardian. Scope-rejection is table-driven over every "no"/limited cell of the `tasks/design/config.md` §Config scope
  table, for project, project-local and CLI `--settings` scopes (incl. user-file-only `[guardian] scanner*` keys rejected from CLI and project-local). Scope stack: CLI > project-local > project
  > user precedence for scalars and union for lists; a same-name `[accounts.<name>]` entry is replaced whole by the higher scope, not merged (same for `[mcp.servers.<name>]` and `[agents.types.<name>]`); `[[hooks.*]]` arrays union; `[telemetry] resource_attributes` set in two scopes merges per key, higher scope winning a shared key; a tracked `settings.local.toml` loads as project
  scope; project and project-local
  `default_mode` accept only `manual`/`dontAsk` (`"plan"`, `"acceptEdits"`, `"auto"` rejected from both); `[permissions] allow` / `additional_directories`
  rejected from project scope; unparseable rule strings and rules naming an unknown tool rejected; an unknown key or misspelled section (e.g. `[permisions]`) rejected in every scope (exit 2); `default_mode = "auto"` rejected (exit 2) from user and CLI scope until Phase 4.
- **project-local trust classification** (`gix`, temp git repos): an untracked `settings.local.toml` and an ignored one in a trusted root load as user trust; an untracked one in an untrusted root loads as project scope (loosening key ⇒ exit 2, message names the untrusted root and the `trust.toml` hand-edit remedy); in a trusted root, a corrupt `.git/index` with an untracked `settings.local.toml` and a `.fontaine` symlink to a directory outside the work tree both load as project scope; a hand-made `.git` skeleton without trust ⇒ project scope; a repo with no index file (fresh `git init`) counts as an empty index; a tracked one loads as project scope; a tracked symlinked `.fontaine` dir (`.fontaine -> cfg/`, tracked `cfg/settings.local.toml`) and a case-variant tracked file load as project scope; a force-added `settings.local.toml` matched by `.gitignore` loads as project scope; a directory with no work tree loads as project scope; a launch from a subdirectory of the repo and a launch from a git worktree (`.git` file) classify an untracked `settings.local.toml` in a trusted root as user trust; a repo config with `core.fsmonitor` set to a canary command never executes it.
- **dirs**: `FONTAINE_CONFIG_DIR` / `FONTAINE_DATA_DIR` override the `dirs` defaults (tests override them with a constructed env-lookup closure; other tests pass dirs and a closure to the loader); owner-only files (`owner_only.rs`; the `--debug-file` output in `rust/fontaine/tests/debug_file.rs`) are 0600 on Unix and current-user-only on Windows, including a pre-existing 0644 file tightened before writing (`#[cfg(windows)]` ACL test, compile-checked locally with the windows-gnu target and executed in the windows CI job).
- **credentials lookup**: account entries and the telemetry entry round-trip from a temp `credentials.toml`; an account named `telemetry` does not shadow the telemetry key; a missing file or entry yields no credential; an unparseable file is a config error (exit 2).
- **trust store**: a trusted repo root trusts its subdirectories; a trusted subdirectory trusts neither its sibling nor its parent; a trusted entry above the work-tree root (a `$HOME`-like dir) trusts neither a repo inside it nor its untracked `settings.local.toml`; outside any repo, a trusted ancestor trusts a descendant directory; a discovery error other than not-found (e.g. an inaccessible dir) ⇒ not trusted even under a trusted `$HOME`-like entry; a non-canonical but equivalent recorded path (symlinked parent) still matches; the record is written under
  the data dir, never inside the workspace; `trust.toml` is 0600 on Unix (ACL under `#[cfg(windows)]`); an unparseable `trust.toml` trusts no root.
- **debug file** (`rust/fontaine/tests/debug_file.rs`): `--debug-file` writes debug-level events (the config-load event is present); `FONTAINE_LOG=warn` with `--debug-file` drops them (the config-load event absent); `FONTAINE_LOG` without `--debug-file` writes no file and nothing to stderr.
- **telemetry config**: every `[telemetry]` key rejected from project and project-local scope except
  `enabled = false`; `enabled = true` without `endpoint`, or without a resolvable ingestion key (no
  `api_key_env` and no stored credential, or the named env var unset/empty), is a config error (exit
  2).
- Golden-dataset schema validation; runner green against verdict fixtures; fixture mode exits 1 on a schema-invalid case or a `fixture_verdict` / `expected` mismatch, a `pipeline` case's `fixture_verdict.tier` / `expected_tier` mismatch, or an unparseable `permissions` rule string.
- **docs** (`rust/fontaine-core/tests/docs.rs`, run by `cargo test`; ```toml blocks fenced `toml trust.toml` / `toml credentials.toml` validate against that file's schema, plain ones as settings): the contract in `tasks/design/testing.md` Testing Strategy, docs bullet.

## Definition of done

- Local gate (no live APIs): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test` / `cargo deny check` green from `rust/`, plus `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings` (or `cargo check --all-targets --target x86_64-pc-windows-gnu`) where the toolchain supports it.
- Needs a human (not runnable by the agent, which cannot push): push the branch and confirm the CI matrix is green on ubuntu, macOS and Windows.
- `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture` (from `rust/`)
  exits 0 on the fixture set, offline, with no guardian in the tree.
- `docs/index.md`, `docs/architecture.md`, `docs/configuration.md`, `evals/README.md` exist with
  real content; `rust/fontaine-core/tests/docs.rs` is green; `docs/configuration.md` covers the scope
  stack, workspace trust (incl. hand-editing `trust.toml` to trust a root) and `[telemetry]` (incl. its required endpoint and key).
- No exporter is built in this phase.
- `CHANGELOG.md` exists with the Phase 0 entry; `CONTRIBUTING.md` and `SECURITY.md` exist.
- Needs a human: enable GitHub private vulnerability reporting on the repository.
- Log the phase in the master plan's Progress log.
