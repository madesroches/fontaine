# `fontaine doctor` and Config Introspection Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/57

## Overview

`fontaine doctor` is a read-only subcommand that answers "why isn't my setup doing what I wrote?":
which config files loaded and which failed (including project keys the scope policy refused), the
effective value of each key with the scope that won, workspace trust, guardian model availability, MCP
server state, and whether the auto-memory index agrees with the files in the store. It mirrors
Claude Code's `/doctor`, `--debug` and status panels (issue #57; the issue's comment adds memory
integrity checks). It starts no session and sends no prompt.

## Current State

- Subcommands are a clap enum, `Command` in `rust/fontaine/src/cli.rs:134` (`auto-mode`, `plugin`,
  `models`). `run` in `rust/fontaine/src/lib.rs:244` dispatches `plugin` before config load and
  `auto-mode`/`models` after `load_config` (`lib.rs:278-283`).
- A config problem is a hard `ConfigError` from `config::load` (`fontaine-core/src/config.rs:2641`),
  which stops at the first failing file. A project key the scope policy forbids
  (`config_scope.rs`) is `ConfigError::Scope { path, column, key, reason, hint }`. So today a
  user whose project file is refused sees one error and nothing about the other layers.
- `Config` already records provenance: `sources` (key to winning slots), `scopes: Vec<ScopeEntry>`
  (slot, path, loaded), `mcp_origins`, `shadowed_accounts`. `config_catalog::KEYS` renders the
  effective value and source label for the keys `/config` shows (`config_catalog.rs`).
- Project-local demotion has a typed reason, `trust::ProjectDemotion`, rendered only inside
  load-time hints (`config.rs:3204`). Trust state: `trust::is_trusted`, `trust::unreadable`,
  `trust::server_approved`.
- `fontaine models` (`rust/fontaine/src/models_cmd.rs`) is the pattern: build a report struct in
  core with `render_text` and `to_json`, bounded by a timeout, `--format text|json`.
  `model_report.rs` is the template for the report type.
- Guardian availability: `models::resolve` returns `GuardianModel::{Ready, Unavailable{reason}}`
  (`auto_mode.rs:285-296`); `resolve` always sets one, with a `guardian_choice` of
  `Pinned`/`Default`/`MainFallback`.
- MCP: `fontaine_mcp::connect(spec, sink, guard, reveal_names, network, env)` (`servers.rs`).
- Memory: `fontaine-core/src/memory.rs` has `dir`, `load` (caps, warnings), `files` (index first,
  then topic `*.md`, links and directories skipped). The index convention is one line per topic
  file linking to it.

## Design

### Command surface

```
fontaine doctor [--format text|json] [--connect yes|no] [--all]
```

- `--connect` (default `yes`) controls the MCP connection probe. Written as `--connect=no` to skip.
- `--all` also lists keys that no layer set (default values); default output lists only keys a
  layer set.
- Exit code `0` when no check failed (warnings allowed), `1` when any check is `fail`.
  Config-load failure is reported *inside* the report, not as a bare error exit, because diagnosing
  it is the command's purpose.

`doctor` dispatches in `run` after `DebugSink::open` and before `load_config`, and calls the
loader itself so it can survive a load failure. `load_config` (`lib.rs:82-109`) is split into
`resolve_dirs` and `load_params(cli, host, &dirs)` helpers used by both it and doctor. A
`dirs::resolve` failure becomes a single `Fail` finding in Install with every other section
skipped.

### Report type (fontaine-core)

New `fontaine-core/src/doctor_report.rs`, modelled on `model_report.rs`:

```
enum Level { Ok, Warn, Fail }
struct Finding { level: Level, subject: String, detail: String }   // detail = error chain text
struct Section { title: &'static str, findings: Vec<Finding> }
struct DoctorReport { sections: Vec<Section> }
impl DoctorReport { fn render_text(&self) -> String; fn to_json(&self) -> Value; fn failed(&self) -> bool }
```

Sections are built by independent functions, each a pure function of its inputs so each is unit
tested with constructed values. Sections:

1. **Install**: version, config dir, data dir, workspace root.
2. **Config layers**: per layer (managed, user, project, project-local, `--settings`): path,
   absent/loaded/error. The managed layer has no file and shows "not supported", as
   `Config::scopes` does. Uses a new `config::diagnose(&LoadParams) -> Vec<LayerDiagnosis>` that
   parses and scope-validates each layer file *independently* and reports every failing layer
   with its first rejected key (`Scope` with key, reason and hint) or its `Parse`/`Read` error;
   the scope walker stops at the first rejection per file, so not every key is listed.
   `diagnose` reuses the existing per-file parse and scope walker (no second copy of the policy).
   `LayerDiagnosis` carries any per-file `ConfigError` (`Parse`, `Scope`, `Invalid`, `Read`). A
   workspace canonicalize failure gives one `Fail` finding with the project and project-local rows
   marked "not checked" while the user and `--settings` rows are still diagnosed.
   A project-local file loaded as project scope shows its `ProjectDemotion` reason in its row.
   When `config::load` succeeds the layers are read from `Config::scopes`. When it fails,
   `diagnose` supplies the rows, and `load`'s own `ConfigError` (rendered with
   `fontaine_tools::error_chain::chain`) is added as a `Fail` finding in this section, because
   cross-layer failures (workspace canonicalize, `--allowed-tools` rule parse, the
   network/scanner/composition/telemetry/websearch checks) leave every layer row "loaded". The
   remaining sections that need a `Config` are marked `skipped: config did not load`.
3. **Effective settings**: iterate `config_catalog::KEYS`, keeping rows whose `slots()` is
   non-empty (all rows with `--all`): effective value and source label via
   `KeyRow::source_label`. `Config::sources` is not iterated for rows, since its leaf paths
   (`accounts.foo.kind`) never match Map-shaped catalog rows, which are prefixes.
   The `Running` values (`config_catalog.rs:13-19`) are filled with theme from config, effort as
   `cli.effort.or(config.effort)` (as the session seeds it, `lib.rs:920`), and main/guardian model
   from the resolved models of the Guardian and models section;
   `Running.guardian_model` is `Some` only when `guardian_choice == Pinned`, as `/config` does. Secrets:
   values come only from the catalog renderers (`/config` already shows them), never raw TOML, so
   no credential can appear; a test asserts an account key and env value do not.
4. **Trust**: workspace root trusted or not (untrusted = `Warn`), `trust.toml` readable or the
   `unreadable` reason (unreadable or malformed = `Fail`),
   project executables (hooks, MCP, skill commands) will load or not.
   Headless and interactive outcomes are stated separately: headless starts every server;
   interactive prompts for unapproved project servers.
5. **Guardian and models**: resolved main model, guardian model as `Ready` /
   `Unavailable { reference, reason }` (`Fail`: every judged call would be denied;
   `setup.rs:92-93` only lets a session continue) with `guardian_choice` (`Pinned`/`Default`/`MainFallback`),
   guardian profile, composition: "validated at load". A `models::resolve` `Err` becomes a `Fail` finding rendered with its chain and the
   guardian is shown as not resolved (do not copy `models_cmd.rs`'s eprintln-and-continue). The
   `Err` cases: no main model (`models.rs:546-551`); no credential, listing failure or unknown
   model (`:508-523`); pinned guardian with unknown account or model (`:565`, `:625-638`). Each
   `Models.notices` entry (e.g. `MainFallback`) is a `Warn` in this section.
   Reuses `models::resolve` with `model_override: cli.model` (as `fontaine models` does), bounded by `LISTING_TIMEOUT` (`bounded` moves from
   `models_cmd.rs` to a `timeout` helper in `fontaine/src/lib.rs`). Relies on `resolve`:
   reachability is not checked for a static-list main account or the guardian account
   (`models.rs:511-533`).
6. **MCP servers**: each `Config::mcp_servers` entry: origin scope, transport kind, approval
   state, and connection state `connected (N tools)` (`Ok`) / `failed: <chain>` (`Warn`: a session warns and
   continues, `mcp.rs:197-205`) / `not started: <why>` (`Ok`, with the reason).
   The probe gate follows `AgentRuntime::mcp_plan` (`agent/mcp.rs:101-134`): user and plugin
   servers always start; any project or project-local server is skipped without workspace trust
   (`trust::is_trusted`); repository-defined ones (Project slot, or project-local without user
   trust) also need `server_approved` against `server_digest` (`agent/mcp.rs:115-125`; user trust
   only affects `needs_approval`). This is the interactive rule with no prompt, so `doctor` never
   runs unaccepted repository code. Before this and the extensions section, doctor runs
   `plugins::load_installed` + `config.apply_plugins` (so plugin servers appear) but not
   `plugin_auto_update::apply_staged`, which writes. A `load_installed`/`apply_plugins` error
   (e.g. a server-name collision, `plugins.rs:472-481`) is a `Fail` finding and plugin servers and
   rows are skipped; `contributions.warnings` and `config.lsp_table().warnings`
   (`lib.rs:746-759`) are shown as `Warn`. A probe uses a small counting sink that `store`s
   `tools.len()` in both `add_managed_tools` and `reconcile_managed_tools` (so a list change does
   not double count) and returns an empty token map; the count is of registered tools
   (`GuardedSink` drops unusable names first). The probe has a 10 s timeout and closes with
   `connected.connection.close().await` (there is no `close` on `Connected` itself) so doctor
   waits for a clean shutdown before exiting.
7. **Instructions, hooks, skills**: instruction files loaded via the existing instructions loader
   (path and origin, in load order), hook counts per event and scope (from `config.hooks`), skills count with the
   warnings `extensions::load` returns (plugin skip, skill-trust read, trust mismatch). An
   `ExtensionError` from `extensions::load` (`extensions.rs:499-516`; stops at the first bad file)
   becomes a `Fail` finding naming the file, with skills and agents listed as not loaded; instruction,
   hook, skill and count rows are `Ok`; project-scope hook and skill-command rows carry the
   load state from the Trust section: headless = loaded; interactive = loaded only when the workspace
   is trusted, else "not loaded (workspace not trusted)", as the TUI /hooks report does;
   `servers` and `contributions.dirs` from the plugin load are passed in. The
   instructions `Sources`/`Scope` (roots, home, deny rules) come from a helper extracted out of
   session setup (`lib.rs` ~638-661, 778-782; `agent/setup.rs` ~358-370).
8. **Memory**: see below. Needs no `Config`: it uses `canonicalize(cwd)` with
   `memory::dir(&dirs.data, ..)` and is not skipped when config fails to load.

### Memory integrity

New `memory::check(dir: &Path) -> Vec<MemoryFinding>` in `fontaine-core/src/memory.rs`, pure
filesystem logic returning typed findings (a typed enum is justified: doctor maps each to a
level):

- store directory missing: `Ok` (nothing saved yet); store is a symlink or not a directory: `Fail`
  (matches `ensure_dir`'s refusal).
- `MEMORY.md` is a link or not a regular file (`memory::load` refuses it, `memory.rs:108-116`):
  `Fail`. Unreadable or not UTF-8: `Fail`; the UTF-8 check runs on the same capped prefix `load`
  reads (`MAX_INDEX_BYTES + 1`), so a char cut at the cap is accepted (`:127-147`).
- index over `MAX_INDEX_LINES` / `MAX_INDEX_BYTES` (what `load` truncates): `Warn`, with how many
  lines are beyond the cap and so never reach the prompt.
- **Dangling entry**: an index line links to a relative `*.md` whose target does not exist on
  disk (`symlink_metadata` reports missing): `Fail`, naming line number and target. A target
  that exists but is not loaded by `memory::files` (a directory or nested path, or a link, which
  `files` skips, `:201-206`) is a `Warn`, naming "is a link" explicitly for links.
- **Orphan file**: a topic file that no index line links to: `Warn`.
- **Duplicate entry**: two index lines link to the same file: `Warn`.
- **Escaping link**: `http(s)` links are ignored; any other scheme, a `..` or an absolute target
  is `Warn` (not followed, never read).
- Links inside fenced code blocks are ignored.

Link extraction is a small Markdown inline-link scan (`[text](target)`, optional `#fragment`
stripped), not regex over tool arguments; fences use the helpers in
`fontaine-core/src/instructions/markdown.rs:13-30` (`opening_fence`/`closes_fence`, made
`pub(crate)`) rather than a second fence parser. The store checked is the one for the current
primary root (`memory::dir(data_dir, root)`); `[memory] auto = false` still checks an existing
store. The "auto memory is off" note appears only when config loaded; otherwise the section still
checks the store and says the auto setting is unknown. `doctor` never writes, repairs or deletes.

### Dispatch module

New `rust/fontaine/src/doctor_cmd.rs` (`run(cli, args, host) -> ExitCode`), like `models_cmd.rs`:
builds a current-thread runtime, assembles sections, prints text or JSON, returns the exit code.
It receives the debug `sink` (`run(cli, args, host, sink)`) and runs dirs resolution and
load/diagnose inside `telemetry::with_debug_only(sink, ..)`.
`DoctorArgs { format: FormatArgs, connect: Toggle, all: bool }` in `cli.rs`.

## Implementation Steps

1. `config.rs`: extract a helper from `load` (workspace canonicalize plus the project-local
   column/demotion/hint, ~2642 and 2658-2673) that both `load` and `diagnose` call; add `diagnose`
   and `LayerDiagnosis`. `trust.rs`: `Display` for `ProjectDemotion` giving a short reason for all
   six variants; `demotion_hint` keeps appending the trust.toml remedy, which needs `data_dir`.
   Unit-test: two bad layers both reported (first rejected key each); a good file after a bad one
   still shown.
2. `memory.rs`: add `check` and `MemoryFinding`. Unit-test every case above.
3. `agent/mcp.rs`: extract `mcp_plan`'s filter/digest and `connect_mcp`'s `ServerSpec` building
   (`connect_mcp` spans `mcp.rs:138-200`) into free functions over `&Config`, e.g.
   `mcp_plan(config, root, data_dir, project_executables, confirm)` and
   `server_spec(config, name, root) -> Option<ServerSpec>`; make `server_digest` visible. Connect
   needs `config.loaded_network(None)` and an env lookup. Add a deny-all `ToolGate`/`Guard` for
   the probe (see `fontaine-mcp/tests/common/mod.rs:67-75`, `fontaine-tools/src/guarded.rs:50-60`).
   Extract the instructions `Sources`/`Scope` helper from session setup. Make `opening_fence` /
   `closes_fence` in `instructions/markdown.rs` `pub(crate)`, and `mod markdown;` in
   `instructions/mod.rs` `pub(crate)`, so `memory.rs` can reach them.
4. `doctor_report.rs`: report types, text and JSON rendering, section builders that take plain
   inputs (config sources, trust facts, resolved models, probe results, memory findings).
5. Move `bounded` to a `timeout` helper in `fontaine/src/lib.rs`, with its unit test
   `a_stalled_call_is_cut_off` (`models_cmd.rs:117-121`); `models_cmd.rs` uses it.
6. `cli.rs`: `Command::Doctor(DoctorArgs)`. `lib.rs`: extract `resolve_dirs` and
   `load_params(cli, host, &dirs)` from `load_config`; dispatch after `DebugSink::open`.
7. `doctor_cmd.rs`: gather live facts (load or diagnose inside `telemetry::with_debug_only`, trust,
   `models::resolve`, plugin load, MCP probe with the counting sink, instructions/hooks/skills
   passing plugin `servers` and `contributions.dirs`), call section builders, print, exit code.
8. Integration test through the binary (see Testing).
9. Docs, parity matrix row, CHANGELOG, plan Decisions.

## Files to Modify

- `rust/fontaine/src/cli.rs`, `rust/fontaine/src/lib.rs`, `rust/fontaine/src/models_cmd.rs`
- `rust/fontaine-core/src/agent/mcp.rs`, `rust/fontaine-core/src/agent/setup.rs` (extractions)
- `rust/fontaine/src/doctor_cmd.rs` (new)
- `rust/fontaine-core/src/doctor_report.rs` (new), `lib.rs` (module), `config.rs`, `trust.rs`
  (demotion `Display`), `memory.rs`
- `docs/doctor.md` (new), `docs/index.md`, `docs/configuration.md`, `docs/headless.md`, `docs/memory.md`
- `research/research-claude-code-parity.md` (the `/doctor` row), `CHANGELOG.md`,
  `tasks/fontaine_harness_plan.md` (Decisions)

## Decisions

- 2026-11-03: `fontaine doctor` is a subcommand only; the `/doctor` slash command is deferred.
- 2026-11-03: the MCP probe gate follows `mcp_plan` with no prompt.
- 2026-11-03: `config::load` stays fail-fast and fail-closed for sessions; only `diagnose` collects every layer's error.
- 2026-11-03: finding levels: guardian `Unavailable` and unreadable/malformed `trust.toml` are `Fail`; untrusted workspace and MCP probe `failed` are `Warn`; MCP `not started` and instruction/hook/skill/count rows are `Ok`.

## Documentation

- New `docs/doctor.md`: command, flags, each section, exit codes, the memory checks.
- `CHANGELOG.md` `## [Unreleased]` Added: an entry naming `fontaine doctor`, noting that
  `fontaine -p doctor` now runs the subcommand (a prompt exactly equal to a subcommand name does)
  and `fontaine -p -- doctor` sends "doctor" as a literal prompt.
- `docs/index.md`: link the page. `docs/configuration.md`: point to `fontaine doctor` for
  provenance and rejected keys. `docs/headless.md`: add a `fontaine doctor` row next to
  `auto-mode`/`models` (a prompt equal to a subcommand name runs it). `docs/memory.md` `## Auditing and clearing`: mention the
  integrity check. Parity matrix: note `/doctor` as deferred (row ~382) and mark the row ~404 partial,
  not shipped (subcommand rather than in-session; no `/doctor` slash command yet).
- Master plan Decisions (`### Config`): `fontaine doctor` is a CLI subcommand only, `/doctor` in
  the TUI deferred; MCP probe skips untrusted servers.

## Testing Strategy

Unit tests (no live APIs):

- `memory::check`: each finding kind, plus a clean store; fenced-code links ignored.
- `config::diagnose`: multiple bad layers all reported (first rejected key per file), scope
  rejection carries key and reason, parse error carries the path.
- `doctor_report`: text and JSON rendering per section; `failed()` true only on `Fail`; skipped
  sections when config did not load, with load's own error as a `Fail`; no secret values in
  settings rows.
- MCP section: not-started reasons for untrusted/unapproved servers (pure function of trust
  facts); a connected/failed row from a constructed probe result.

Integration test through the `fontaine` binary, using the existing headless test harness with a
temp config dir and a wiremock `GET /models` openai-compatible account as in
`rust/fontaine/tests/models_cmd.rs`: a project file with a user-only key yields exit 1 with the
key named; a clean setup yields exit 0 and valid JSON: an openai-compatible account with a static
`models = [..]` list used for both main and guardian (guardian is `Ready` with no network call);
an untrusted workspace is fine (`Warn` only). An MCP case configures the existing
example `repo_mcp_server` (3 tools; `fontaine/tests/project_executables.rs:31-60`) as a user-scope
server and asserts "connected (3 tools)".