# `fontaine doctor` and Config Introspection Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/57

## Overview

`fontaine doctor` is a read-only subcommand that answers "why isn't my setup doing what I wrote?":
which config files loaded and which failed, the effective value of each key with the scope that
won, project keys the scope policy refused, workspace trust, guardian model availability, MCP
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
  or no guardian (`auto_mode.rs:285-296`).
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

`doctor` dispatches **before** `load_config` in `run`, next to `plugin`, and calls the loader
itself so it can survive a load failure.

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
   absent/loaded/error. Uses a new `config::diagnose(&LoadParams) -> Vec<LayerDiagnosis>` that
   parses and scope-validates each layer file *independently* and collects every failure
   (`Scope` rejections with key, reason and hint; `Parse`; `Read`) instead of stopping at the
   first. `diagnose` reuses the existing per-file parse and scope walker (no second copy of the
   policy). When `config::load` succeeds the layers are read from `Config::scopes`; when it
   fails, `diagnose` supplies the rows, and the remaining sections that need a `Config` are
   marked `skipped: config did not load`.
3. **Rejected project keys**: each refused key from the `Scope` failures: file, dotted key, the
   rule's `reason`, and the demotion reason when a project-local file loaded as project scope
   (`ProjectDemotion` gets a `Display` so `demotion_hint` and doctor share one wording).
4. **Effective settings**: for each key in `Config::sources` (all of `config_catalog::KEYS` with
   `--all`): effective value and source label via `KeyRow::source_label`. Keys set in a layer but
   with no catalog row print the raw source label only. Secrets: values come only from the
   catalog renderers (`/config` already shows them), never raw TOML, so no credential can appear;
   a test asserts an account key and env value do not.
5. **Trust**: workspace root trusted or not, `trust.toml` readable or the `unreadable` reason,
   project executables (hooks, MCP, skill commands) will load or not, per-server approval state.
6. **Guardian and models**: resolved main model, guardian model with `Ready` /
   `Unavailable{reason}` / none, guardian profile, composition compile status. Reuses
   `models::resolve` bounded by `LISTING_TIMEOUT` (`bounded` moves from `models_cmd.rs` to a
   shared `pub(crate)` spot in `fontaine/src/lib.rs` or a small `timeout.rs`).
7. **MCP servers**: each `Config::mcp_servers` entry: origin scope, transport kind, approval
   state, and connection state `connected (N tools)` / `failed: <chain>` / `not started: <why>`.
   A server is probed only if a session would start it: workspace trusted (or the origin is
   user/plugin scope) and the server approved. A probe uses a counting sink and a 10 s timeout,
   and drops the connection immediately. An untrusted or unapproved project server is never
   spawned, so `doctor` cannot run repository code the user has not accepted.
8. **Instructions, hooks, skills**: instruction files loaded via the existing instructions loader
   (path and origin, in load order), hook counts per event and scope, skills count with hidden or
   shadowed warnings the loader already produces.
9. **Memory**: see below.

### Memory integrity

New `memory::check(dir: &Path) -> Vec<MemoryFinding>` in `fontaine-core/src/memory.rs`, pure
filesystem logic returning typed findings (a typed enum is justified: doctor maps each to a
level):

- store directory missing: `Ok` (nothing saved yet); store is a symlink or not a directory: `Fail`
  (matches `ensure_dir`'s refusal).
- `MEMORY.md` unreadable or not UTF-8: `Fail`.
- index over `MAX_INDEX_LINES` / `MAX_INDEX_BYTES` (what `load` truncates): `Warn`, with how many
  lines are beyond the cap and so never reach the prompt.
- **Dangling entry**: an index line links to a relative `*.md` that is not in `memory::files`:
  `Fail`, naming line number and target.
- **Orphan file**: a topic file that no index line links to: `Warn`.
- **Duplicate entry**: two index lines link to the same file: `Warn`.
- **Escaping link**: a link target with `..`, an absolute path, or a scheme: `Warn` (not followed,
  never read).
- Links inside fenced code blocks are ignored; external `http(s)` links are ignored.

Link extraction is a small Markdown inline-link scan (`[text](target)`, optional `#fragment`
stripped), not regex over tool arguments. The store checked is the one for the current primary
root (`memory::dir(data_dir, root)`); `[memory] auto = false` still checks an existing store
and notes that auto memory is off. `doctor` never writes, repairs or deletes.

### Dispatch module

New `rust/fontaine/src/doctor_cmd.rs` (`run(cli, args, host) -> ExitCode`), like `models_cmd.rs`:
builds a current-thread runtime, assembles sections, prints text or JSON, returns the exit code.
`DoctorArgs { format: FormatArgs, connect: Toggle, all: bool }` in `cli.rs`.

## Implementation Steps

1. `config.rs`: add `diagnose` and `LayerDiagnosis`; give `ProjectDemotion` a `Display` and make
   `demotion_hint` use it. Unit-test: two bad layers both reported; a good file after a bad one
   still shown.
2. `memory.rs`: add `check` and `MemoryFinding`. Unit-test every case above.
3. `doctor_report.rs`: report types, text and JSON rendering, section builders that take plain
   inputs (config sources, trust facts, resolved models, probe results, memory findings).
4. `cli.rs`: `Command::Doctor(DoctorArgs)`. `lib.rs`: dispatch before `load_config`.
5. `doctor_cmd.rs`: gather live facts (load or diagnose, trust, `models::resolve`, MCP probe with
   counting sink, instructions/hooks/skills), call section builders, print, exit code.
6. Move `bounded` to a shared helper; `models_cmd.rs` uses it.
7. Integration test through the binary (see Testing).
8. Docs, parity matrix row, CHANGELOG, plan Decisions.

## Files to Modify

- `rust/fontaine/src/cli.rs`, `rust/fontaine/src/lib.rs`, `rust/fontaine/src/models_cmd.rs`
- `rust/fontaine/src/doctor_cmd.rs` (new)
- `rust/fontaine-core/src/doctor_report.rs` (new), `lib.rs` (module), `config.rs`, `trust.rs`
  (demotion `Display`), `memory.rs`
- `docs/doctor.md` (new), `docs/index.md`, `docs/configuration.md`, `docs/memory.md`
- `research/research-claude-code-parity.md` (the `/doctor` row), `CHANGELOG.md`,
  `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Standalone subcommand, not just `/doctor`.** Config that fails to load never reaches a TUI,
  which is exactly when the user needs diagnosis. Claude Code's `/doctor` is in-session; the
  subcommand covers the broken-startup case and is scriptable (`--format json`).
- **Collecting all layer errors (`diagnose`) rather than changing `load`.** `load` stays fail-fast
  and fail-closed for sessions; only the diagnostic path collects.
- **Probing MCP by default.** The issue asks for connection state. Gated on trust and approval so
  it never runs unaccepted repository code.

## Decisions

- Records go here as they are settled; plan-level deviations are copied to the master plan's
  Decisions.

## Documentation

- New `docs/doctor.md`: command, flags, each section, exit codes, the memory checks.
- `docs/index.md`: link the page. `docs/configuration.md`: point to `fontaine doctor` for
  provenance and rejected keys. `docs/memory.md` `## Auditing and clearing`: mention the
  integrity check. Parity matrix `/doctor` row moves from missing to shipped, with deviations
  (subcommand rather than in-session; no `/doctor` slash command yet).
- Master plan Decisions (`### Config`): `fontaine doctor` is a CLI subcommand only, `/doctor` in
  the TUI deferred; MCP probe skips untrusted servers.

## Testing Strategy

Unit tests (no live APIs):

- `memory::check`: each finding kind, plus a clean store; fenced-code links ignored.
- `config::diagnose`: multiple bad layers all reported, scope rejection carries key and reason,
  parse error carries the path.
- `doctor_report`: text and JSON rendering per section; `failed()` true only on `Fail`; skipped
  sections when config did not load; no secret values in settings rows.
- MCP section: not-started reasons for untrusted/unapproved servers (pure function of trust
  facts); a connected/failed row from a constructed probe result.

Integration test through the `fontaine` binary, using the existing headless test harness with a
temp config dir and a mock guardian endpoint (`wiremock`): a project file with a user-only key
yields exit 1 with the key named; a clean setup yields exit 0 and valid JSON; a memory store with
a dangling link yields a `fail` finding. The MCP probe is tested against an in-process server
over `connect_transport`-style plumbing where the existing MCP tests already do so; the stdio
spawn path is covered by the existing MCP tests, not repeated here.

## Open Questions

- Should `doctor` also probe the main model account's reachability, or rely on the listing
  `models::resolve` already performs? Plan assumes the latter.
- Whether the probe sink can reuse an existing no-op `ManagedToolSink` implementation or needs a
  small counting one: check in `fontaine-mcp` tests before step 5.
