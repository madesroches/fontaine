# Session `[env]` Config Key Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/5

## Overview

Claude Code's `env` settings key sets environment variables for every session and the processes
it starts (Bash, hooks, MCP servers). Fontaine has no equivalent: the only environment-related
config is credential variable *names* (`api_key_env`, `env_vars`). Add an `[env]` table of
literal `NAME = "value"` pairs that is applied to every subprocess a tool or extension starts —
the `shell` tool, `!` user commands, user hooks and stdio MCP servers. Because an override can
loosen the environment (a `PATH` in front of the system's, a proxy), `[env]` comes only from
user-trust scopes (user file, `--settings`, trusted untracked project-local); values are never
logged, and the guardian sees the keys that were set, never their values.

## Current State

- **Shell tool.** `shell_for` (`rust/fontaine-tools/src/builtins.rs:284`) builds a rig
  `Launcher` with `.without_env(settings.scrub_env)` and, on Windows, a Git Bash `PATH`
  (`git_bash.path_value(std::env::var_os("PATH"))`). `ShellSettings`
  (`builtins.rs:222`) carries `max_timeout_secs`, `scrub_env`, `git_bash`. rig's
  `Launcher::command` (`rig-tools 0.45.0` `process.rs`) removes `unset_env`, then applies `env`
  pairs in order (later pairs win).
- **`!` user commands.** `AgentRuntime::run_user_shell` (`rust/fontaine-core/src/user_shell.rs:37`)
  builds its own `ShellSettings` from `self.setup.scrub_env` / `git_bash`.
- **Hooks.** `HookProcess::run` (`rust/fontaine-core/src/user_hooks.rs:56`) spawns `sh -c` with
  `env_remove` for each `scrub_env` name, then `FONTAINE_PROJECT_DIR`, the Git Bash command
  variable (`FONTAINE_SH_COMMAND`) and Git Bash `PATH`.
- **MCP stdio servers.** `start` (`rust/fontaine-mcp/src/servers.rs:174`) runs `env_clear()`, then
  copies `BASE_ENV` plus the server's `env_vars` from the `env` lookup closure.
  `AgentRuntime::connect_mcp` (`rust/fontaine-core/src/agent.rs:834`) builds the `ServerSpec`.
- **Plumbing.** `scrub_env` is computed once in `prepare_session`
  (`rust/fontaine/src/lib.rs:461`, `tools::scrubbed_env`) and flows through `SessionPrep`,
  `AgentSetup` (`agent.rs:84`), the runtime setup (`agent.rs:288`, `:477`, `:526`, `:1379`),
  `RegistryParams` (`fontaine-core/src/tools.rs:66`) into `ShellSettings` and `HookProcess`.
  Several test helpers build these structs too.
- **Config.** `Settings` (`rust/fontaine-core/src/config.rs:639`) is `deny_unknown_fields`;
  `check_scope` (`config.rs:691`) rejects keys per column; `Config::apply` (`config.rs:1281`)
  merges layers (map-valued keys per key, higher scope wins — e.g.
  `telemetry.resource_attributes`); `validate_values` (`config.rs:841`) checks shapes. `Config`
  and `Settings` derive `Debug`.
- **Guardian facts.** `Record::SessionStart` (`rust/fontaine-core/src/ledger.rs:27`) records roots,
  trust and remotes; `LedgerView` copies them; `adjudication::facts`
  (`fontaine-core/src/adjudication.rs:64`) builds `SessionFacts`
  (`fontaine-guardian/src/request.rs:66`), rendered by `prompt_builder.rs:142`.
- **Claude Code (observed with `claude -p --settings`)**: an `env` value is literal — `"PATH":
  "/probe/bin:$PATH"` left Bash with `PATH` set to exactly that string (`printenv` not found).
  Claude Code also honors `env` from project settings after workspace trust, and applies it to its
  own process as well as children.

## Design

### Config

New top-level table, a map of variable name to string value:

```toml
[env]
HTTPS_PROXY = "http://proxy.internal:3128"
PATH = "/home/me/.local/bin:/usr/local/bin:/usr/bin:/bin"
RUST_LOG = "warn"
```

- `Settings` gains `pub env: Option<BTreeMap<String, EnvValue>>`; `Config` gains
  `pub env: BTreeMap<String, EnvValue>`.
- `EnvValue` is a newtype over `String` (`#[serde(transparent)]`) whose `Debug` prints
  `EnvValue(<redacted>)`, so no `{:?}` of `Settings`/`Config` can carry a value into a log or a
  panic message. It exposes `as_str()` for the one place that builds the child environment.
- **Merge**: per key, the higher-precedence scope wins (the existing map-valued rule, like
  `resource_attributes`); on Windows names are folded case-insensitively during the merge, so a
  user `PATH` and a CLI `Path` are one key and the higher scope wins. There is no unset syntax.
- **Values are literal**: no `$VAR` / `~` expansion (Claude Code parity, observed above).
- **Validation** (`validate_values`, exit 2, error names the key, never the value):
  - name non-empty, contains no `=` and no NUL;
  - name does not start with `FONTAINE_` (case-insensitive): that prefix is Fontaine's own
    control namespace — `FONTAINE_SH_COMMAND` carries the judged command to Git Bash and
    `FONTAINE_PROJECT_DIR` tells hooks their project, and an override of either would break the
    "the command that was judged is the one that runs" property or mislead hooks;
  - value contains no NUL;
  - a non-string value is a schema error (TOML type mismatch).
- **Parse errors carry no source text**: toml's `Display` quotes the offending line, which for a
  malformed `[env]` entry would echo a secret to stderr. Like `ConfigError::Credentials`, settings
  parse errors are rendered as path + line/column (from `de::Error::span()`) + toml's `message()`,
  without the quoted source line.
- **Scope**: `check_scope` rejects `env` in the `Project` column with reason "environment
  overrides come only from user-trust scopes" (placed with the other `tighten` checks, after the
  `ProjectLocal` early return). User, CLI (`--settings`) and trusted project-local may set it. A
  project-local file that did not earn user trust is validated as `Project`, so it is rejected —
  that is the "trusted-local only" rule with no extra code.

### Applying it

One computed list, built once per session next to `scrub_env`:

```rust
// fontaine-core/src/tools.rs
/// The `[env]` overrides every tool and extension subprocess gets, in name order.
pub fn subprocess_env(config: &Config) -> Vec<(OsString, OsString)>
```

It flows as a sibling field `set_env: Vec<(OsString, OsString)>` on `SessionPrep`, `AgentSetup`,
the runtime setup, `RegistryParams`, `ShellSettings` and `HookProcess` only — not on
`ScannerParams` / `CommandScanner`. Order of application at each spawn site — **scrub, then `[env]`, then Fontaine's
own variables**:

| Site | Change |
|---|---|
| `shell_for` | after `.without_env(scrub_env)`, `.with_env(name, value)` for each `set_env` pair; the Git Bash `PATH` is then built from the configured `PATH` when `[env]` sets one, else from `std::env::var_os("PATH")`, so Git for Windows' directories stay in front either way. `ShellTool::prepared` already adds `FONTAINE_SH_COMMAND` last. |
| `run_user_shell` | passes `self.setup.set_env` into its `ShellSettings`. |
| `HookProcess::run` | after the `env_remove` loop, `process.env(name, value)` per pair; then `FONTAINE_PROJECT_DIR`, the Git Bash command variable and `PATH` as today (the `PROJECT_DIR_VAR` `.env` call moves below the loop; the `FONTAINE_` rejection makes the order moot but keeps it obviously right). Git Bash `PATH` uses the same "configured `PATH` else process `PATH`" base. |
| MCP `start` | `Transport::Stdio` gains `env: Vec<(OsString, OsString)>`; after the `BASE_ENV` + `env_vars` copy, each pair is set (overriding a copied value of the same name). `connect_mcp` fills it from `setup.set_env`. |

A small helper in `fontaine-tools/src/git_bash.rs` — `fn base_path(set_env: &[(OsString,
OsString)]) -> Option<OsString>` (configured `PATH` if present, else the process `PATH`) — is shared
by `shell_for` and `HookProcess::run` so the two sites cannot drift. On Windows it matches the
name case-insensitively (as `scrubbed_env` in `fontaine-core/src/tools.rs` does, via
`to_ascii_uppercase`), so
`[env] Path = ...` is found; elsewhere the match is exact.

Scrub-then-set means an `[env]` entry may deliberately set a scrubbed name (e.g. a different
`OTEL_EXPORTER_OTLP_HEADERS` for a tool): the value is the user's own config, not Fontaine's
inherited credential, so nothing of Fontaine's leaks.

Not covered: the guardian's `scanner = "command"` program and provider/telemetry clients get no
`[env]`; Fontaine's own process environment is unchanged.

Subagents share the parent runtime's setup, so children get the same `set_env` with no extra code.

### Guardian sees keys

- `Record::SessionStart` gains `#[serde(default, skip_serializing_if = "Vec::is_empty")]
  env_keys: Vec<String>` (old ledgers still parse); `AgentRuntime` writes the sorted names from
  `setup.set_env` at session start (`agent.rs:1240`).
- `LedgerView` gains `env_keys`, copied in `apply`.
- `SessionFacts` gains `env_keys: Vec<String>`; the private `adjudication::facts` copies it.
- `prompt_builder` renders, when non-empty, one line inside `session_facts`:
  `subprocess_env_set_by_user: HTTPS_PROXY, PATH, RUST_LOG` (each name through `one_line`).
  Values never reach `fontaine-core`'s ledger or the guardian crate.

## Implementation Steps

1. **Config** (`rust/fontaine-core/src/config.rs`): `EnvValue` newtype with redacting `Debug`;
   `Settings.env`, `Config.env` (+ `defaults`), merge in `apply`, scope rule in `check_scope`,
   value checks in `validate_values`; render settings parse errors (`parse_settings_with_hint`)
   from `message()` and `span()` instead of `e.to_string()`.
2. **Plumbing**: `tools::subprocess_env` (`fontaine-core/src/tools.rs`); `set_env` field on
   `ShellSettings` (+ `Default`), `HookProcess`, `RegistryParams`, `AgentSetup`, runtime setup,
   `SessionPrep` (`rust/fontaine/src/lib.rs`, filled in `prepare_session`). Update every struct
   literal: test helpers in `fontaine-core/tests/common/runtime.rs`,
   `choke_point.rs`, `extensions.rs`, `project_origins.rs`, `user_hooks.rs`,
   `fontaine-tools/tests/choke_point.rs`, `fontaine-tui/tests/common/mod.rs`.
3. **Spawn sites**: `git_bash::base_path`; `shell_for`; `run_user_shell`; `HookProcess::run`;
   `Transport::Stdio.env` + `start` in `fontaine-mcp/src/servers.rs`; `connect_mcp`; fix the
   existing `Transport::Stdio` literals in `fontaine-mcp/tests/mcp_registration.rs`.
4. **Guardian facts**: `ledger.rs` (`SessionStart.env_keys`, `LedgerView.env_keys`), the
   `SessionStart` writers (`agent.rs`, `fontaine-evals/src/none.rs`,
   `fontaine-core/tests/sessions.rs`), `SessionFacts`
   (`fontaine-guardian/src/request.rs`), `adjudication::facts`, `prompt_builder.rs`; update
   `SessionFacts { .. }` literals in guardian tests/`profile.rs` (including the full-field fixture
   at `prompt_builder.rs:245`) (`profile.rs` copies selected
   facts for the minimal profile — copy `env_keys` too: a `PATH` override matters to the escape
   ruleset). Add one sentence each to `rust/fontaine-guardian/prompts/guardian_prompt.md` and
   `guardian_minimal.md`: `subprocess_env_set_by_user` names variables the user's own config sets
   for every subprocess (values withheld), and a `PATH` override means command names may resolve
   to other binaries; re-accept any affected prompt snapshot.
5. **Docs and design** (see `## Documentation`), the master plan Decisions lines, and the parity
   matrix row.

## Files to Modify

- `rust/fontaine-core/src/config.rs`, `tools.rs`, `agent.rs`, `user_shell.rs`, `user_hooks.rs`,
  `ledger.rs`, `adjudication.rs`
- `rust/fontaine-tools/src/builtins.rs`, `git_bash.rs`
- `rust/fontaine-mcp/src/servers.rs`
- `rust/fontaine-guardian/src/request.rs`, `prompt_builder.rs`, `profile.rs`
- `rust/fontaine/src/lib.rs`
- `rust/fontaine-evals/src/none.rs`
- `rust/fontaine-guardian/prompts/guardian_prompt.md`, `guardian_minimal.md`
- Tests: `rust/fontaine-core/tests/{config*,tools,user_hooks,common/runtime,choke_point,extensions,project_origins,sessions}.rs`,
  `rust/fontaine-tools/tests/{choke_point,guarded}.rs`, `rust/fontaine-mcp/tests/mcp_registration.rs`,
  `rust/fontaine-tui/tests/common/mod.rs`, `rust/fontaine/tests/headless_tools.rs`
- Docs: `docs/configuration.md`, `docs/tools.md`, `docs/hooks.md`, `docs/mcp.md`, `docs/guardian.md`
- Design: `tasks/design/config.md`, `tasks/fontaine_harness_plan.md`,
  `research/research-claude-code-parity.md`

## Trade-offs

- **A redacting newtype vs. plain `String`.** Plain strings are simpler, but `Config` derives
  `Debug` and one stray `{config:?}` would leak a proxy password; the newtype makes the guarantee
  structural.
- **Separate `set_env` field vs. folding into a richer "subprocess env policy" struct.** A struct
  holding both `scrub_env` and `set_env` would be tidier but touches every existing `scrub_env`
  site for no behavior change; a sibling field follows the existing pattern.
- **Keys in `SessionStart` vs. a new ledger record.** The overrides are fixed for the session, the
  same lifetime as roots and trust, so they belong in the same record.

## Decisions

- 2026-10-25: project scope cannot set `[env]` (Claude Code honors it after trust); it cannot
  tighten, and a project `PATH` would shadow commands the pre-classifier and guardian reason about.
- 2026-10-25: `[env]` applies to tool and extension subprocesses only, not Fontaine's own process
  (Claude Code applies it to both); credential and telemetry lookups keep their explicit keys.

## Documentation

- `docs/configuration.md`: new `### [env]` key-reference section (literal values, no expansion,
  applies to `shell`, `!`, hooks and stdio MCP servers, applied after the scrub, `FONTAINE_` names
  rejected, values never logged, guardian sees names); per-key scope table row
  `[env] | no | yes | yes`; mention under "Trust asymmetry".
- `docs/tools.md`: the shell environment = inherited minus scrubbed plus `[env]`.
- `docs/hooks.md` (line ~66): hooks get `[env]` too.
- `docs/mcp.md` "Environment" paragraph: stdio servers also get every `[env]` entry.
- `docs/guardian.md`: the request's session facts include the names of `[env]` overrides; the
  passage (~lines 264-265) listing what the minimal profile receives (roots, remotes, touched
  paths) also names env keys.
- `docs/configuration.md` (~line 414): the `command` scanner's "same scrubbed environment as
  hooks" becomes "the same scrubbed environment as hooks, without `[env]`".
- `tasks/design/config.md`: add `[env]` to the example TOML block and the per-key table.
- `tasks/fontaine_harness_plan.md` `## Decisions`: copy the two dated lines from this plan's
  `## Decisions`.
- `research/research-claude-code-parity.md:281`: mark the row as covered.

## Testing Strategy

All unit/integration, no live APIs.

- **Config** (`fontaine-core` config tests): user, CLI and project-local columns accept `[env]`;
  project column rejects with a `Scope` error (exit 2) naming `env`; an untrusted project-local
  file is rejected (it loads as project); per-key merge with the higher scope winning; rejects
  empty name, `=` in name, NUL in name or value, `FONTAINE_SH_COMMAND` and `fontaine_x`
  (case-insensitive prefix), non-string value; the rejection message never contains the value;
  `format!("{config:?}")` does not contain a value; a malformed `[env]` line (unterminated string,
  non-string value) yields a parse error whose text does not contain the value.
- **Shell** (`fontaine-tools` tests, unix): a `ShellTool` from `shell_for` with `set_env` sees
  the value (`printf %s "$X"`); a name both scrubbed and set gets the set value; a value with
  `$HOME` is passed literally. `base_path` unit tests: configured `PATH` wins over the process
  one; absent ⇒ process `PATH`; the `Path` spelling is found on Windows (Windows CI). Git Bash `PATH` composition is tested through `base_path` +
  `path_value` (a Windows-only spawn is not reachable on Linux CI; Windows CI runs the same tests).
- **Hooks** (`fontaine-core/tests/user_hooks.rs`): next to
  `hook_children_get_the_scrubbed_environment`, a hook sees a `set_env` value and still sees the
  real `FONTAINE_PROJECT_DIR`.
- **MCP** (`fontaine-mcp/tests/mcp_registration.rs`): the existing test stub server that reports
  its environment receives a `Transport::Stdio.env` pair, and an `env` pair overrides a
  `BASE_ENV` value of the same name.
- **Guardian facts**: ledger round-trip of `SessionStart` with and without `env_keys` (an old
  record without the field parses); `adjudication::build` (which calls the private `adjudication::facts`) copies the keys;
  `prompt_builder` renders the names and, given a secret-looking value in config, the rendered
  prompt contains no value (the `prompt_builder` test only pins the line format and absence of
  the line when empty; the no-value assertion is on the serialized `SessionStart` written from a
  `set_env` with a secret-looking value).
- **End to end** (`rust/fontaine/tests/headless_tools.rs`): `fontaine -p` with `--settings`
  carrying `[env] PROBE_ENV = "x"` and `[permissions] allow = ["shell(printenv *)"]` (the harness
  runs `--permission-mode manual`, which denies a headless `shell` call otherwise) and a scripted
  model that calls `shell` with `printenv PROBE_ENV`; the tool result carries
  `x`. This catches a plumbing
  break between config and spawn site that unit tests of each piece cannot.

## Open Questions

None.
