# MCP Scopes and `fontaine mcp` CLI Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/31

## Overview

Claude Code stores MCP servers at three scopes: **local** (private to you, in one project),
**project** (`.mcp.json`, shared through the repository) and **user** (every project). It manages
them with `claude mcp add/list/get/remove`, expands `${VAR}` in `.mcp.json`, and shows server
state in `/mcp`. Fontaine already declares servers under `[mcp.servers]` in its TOML settings
files, runs the workspace-trust gate and per-server approval, and has a `/mcp` listing. This plan
adds the missing parts:

- a local scope;
- reading a repository's `.mcp.json` as project scope, with `${VAR}` expansion;
- the `fontaine mcp add/list/get/remove` subcommands;
- tool counts in `/mcp`.

## Current State

- **Config.** `McpServer` (`rust/fontaine-core/src/config.rs:974`) has `command`, `args`,
  `env_vars`, `env` and `url`. The settings layers fill `Config::mcp_servers` and
  `Config::mcp_origins` (`config.rs:2984-2989`). An `Origin { slot, user_trust }`
  (`config.rs:598`) decides the trust gate: `is_project()` is true for `Slot::Project` and
  `Slot::ProjectLocal`. The layers are the user `config.toml`, `--settings`, the project
  `.fontaine/settings.toml` and the project-local `.fontaine/settings.local.toml`, which gets user
  trust only when it is untracked and its root is trusted (`config.rs:3247-3320`).
- **Plan and approval.** `mcp_plan` (`rust/fontaine-core/src/agent/mcp.rs:118`) drops project
  servers when project executables are off, and marks a repository-defined server
  `needs_approval` until `trust::server_approved` finds its digest. `server_digest` hashes the
  definition. `server_description` builds the command line or URL that the approval prompt
  shows. `server_spec` builds the `ServerSpec`.
- **`.mcp.json` reader.** It exists only for plugins. `plugins::parse_mcp_file`
  (`rust/fontaine-core/src/plugins.rs:921`) validates names and keys, and `expand_vars`
  (`plugins.rs:1855`) expands `${CLAUDE_PLUGIN_ROOT}`, `${CLAUDE_PROJECT_DIR}` and `${VAR}`. It
  has no `${VAR:-default}`. Unknown keys (`type`, `headers`) are ignored without a word.
  Expansion happens at load, so the result is literal.
- **The repository's root `.mcp.json` is not read.** A repository written for Claude Code
  contributes no servers. `.mcp.json` is already a protected path for the floor
  (`rust/fontaine-core/src/floor.rs:31`).
- **No CLI.** `rust/fontaine/src/cli.rs:146` has `auto-mode`, `plugin`, `models`, `doctor` and
  `update`.
  - `rust/fontaine-core/src/user_config.rs` already edits the user file with `toml_edit`, under a
    lock with an atomic write (`append_allow_rule`, `add_account`, `remove_account`).
  - `doctor_cmd::mcp_section` (`rust/fontaine/src/doctor_cmd.rs:231`) probes the servers a
    session would start without a prompt, through `probe_server`, and reports a state and tool
    count for each.
- **`/mcp`.** `Ui::mcp_listing` (`rust/fontaine-tui/src/ui/mcp.rs:11`) shows each server's
  state from `McpServerState` (`rust/fontaine-core/src/agent/mcp_supervisor.rs:43`), but not how
  many tools it registered.

## Design

### Scopes

| Scope | Where | Written by `fontaine mcp add -s` | Trust |
|---|---|---|---|
| `local` | `<config dir>/mcp-local.toml`, `[projects."<root>".servers.<name>]` | `local` (default) | user: starts at once, no workspace trust or approval |
| `project` | `<root>/.mcp.json` (`mcpServers`), and `[mcp.servers]` in `.fontaine/settings.toml` | `project`, into `.mcp.json` | repository: workspace trust, then per-server approval; headless starts them, as today |
| `user` | `[mcp.servers]` in `<config dir>/config.toml` | `user` | user |

These sources still load, and `mcp list`/`get` show them under their own labels:

- `project-local`: `.fontaine/settings.local.toml`;
- `cli`: `--settings`;
- `plugin`: a plugin's `.mcp.json`.

`<root>` is `Config::workspace`, the canonical launch directory that the project files are read
from.

**Precedence by name: cli > local > project-local > project > user.** This is Claude Code's
local > project > user, with Fontaine's existing project-local and `--settings` layers kept in
their current places.
Within project, `.fontaine/settings.toml` wins over `.mcp.json`, and the collision warns once
through the load warnings. A plugin server that collides is still an error (`apply_plugins`),
unchanged.

**New slot.** Add `Slot::Local` (label `local`). Its origin is
`Origin { slot: Local, user_trust: true }`, and `is_project()` stays false: the file lives in
the user's config directory, which the floor protects, so a repository cannot write it. Apply it
in `config::load` after project-local and before the cli layer, and before plugins. Load it only when the file has a
`projects."<root>"` table for this workspace.

Local entries do not go through `apply`, which derives `user_trust` from a settings `Column`
and has no Local column. Each entry runs the same per-server validation as `[mcp.servers]`
(command xor url, stdio-only keys, name rules), factored out of `validate_values`, and is
inserted into `mcp_servers` and `mcp_origins` directly. A malformed file or an invalid entry
warns through the load warnings and is skipped, as for `.mcp.json`, so `fontaine mcp remove`
can repair it. `config::diagnose` parses this file on its own path, since `diagnose_file`
parses only settings files.

The local file is TOML with the same `McpServer` schema, keyed by root. It is the analogue of
Claude Code's `~/.claude.json` `projects` map, in Fontaine's settings format.

### The repository's `.mcp.json`

- **Parser.** Generalize `plugins::parse_mcp_file` into a shared reader, `mcp_json.rs` in
  `fontaine-core`. Plugins and the project root both use it. It takes an expansion policy:
  - plugin: expand now with `CLAUDE_PLUGIN_ROOT` and `CLAUDE_PROJECT_DIR` (today's behavior);
  - project: keep the text raw and mark the server for expansion at start.
- **Keys.** It reads `type`:
  - `stdio` and `http` are accepted, and an absent `type` means `stdio` for a `command` entry or
    `http` for a `url` entry;
  - `sse` and `ws` skip the server with a warning naming #36.

  A `headers` key warns that headers are not supported yet (#32) and keeps the server, so it
  connects without them. These warnings apply to plugins too.
- **Malformed project file.** For the project root, an invalid file or entry warns through the
  load warnings and is skipped, so `fontaine mcp` can still run to fix it. Plugins keep the hard
  error.
- **Raw text.** `McpServer` gains `#[serde(skip)] pub expand: bool`. The text stays raw in
  `Config`, so:
  - `server_digest` hashes the definition as written, and approval survives a rotated token but
    not a changed reference;
  - `server_description`, and so the approval prompt, shows `${GITHUB_TOKEN}` and never the
    value. Extend the prompt's "environment:" line to list the variables a project definition
    references, as it lists `env_vars`.
- **Expansion at start.** `server_spec` expands `command`, `args`, `env` values and `url` when
  `expand` is set, through the session's `EnvLookup`.
  - It supports `${VAR}` and `${VAR:-default}`, as Claude Code does. Add `:-default` to the
    shared expander, so plugins get it too.
  - A reference to an unset variable with no default fails that server's start with an error
    naming the variable. Claude Code fails to parse the config in that case.
- **Credential variables are never expanded.** A repository-defined definition never expands a
  variable that holds a credential Fontaine itself uses: the set that `tools::scrubbed_env` withholds
  from tool subprocesses (account and telemetry `api_key_env`, `guardian.scanner_hf_token_env`,
  `MICROMEGAS_*`, `OTEL_EXPORTER_OTLP_*HEADERS`). Such a reference fails the server's start with an
  error.
  - Reason: in a headless run a project server starts without asking, and
    `url = "https://x/${ANTHROPIC_API_KEY}"` would hand a model key to a repository-chosen host.
    This is the same boundary as the tool-subprocess scrub.
  - Local and user definitions are TOML and are not expanded, as today, so this rule touches only
    `.mcp.json`.

### `fontaine mcp` subcommands

These live in `rust/fontaine/src/mcp_cmd.rs`. The file edits go in a new
`rust/fontaine-core/src/mcp_config.rs` (TOML through `toml_edit`, under `file_lock` with an
`owner_only` atomic write; `.mcp.json` through `serde_json` with key order preserved).

- **`fontaine mcp add [-s local|project|user] [-t stdio|http] [-e KEY=value]... [--env-var NAME]... <name> <command-or-url> [args...]`**
  - `-t` defaults to `stdio`. A stdio server needs `--` before its own flags, as in Claude Code.
  - The name passes the existing `.mcp.json` name check (letters, digits, `_`, `-`; no `__`; no
    leading or trailing `_`).
  - A name already in that scope is an error: ``MCP server `foo` already exists in <scope>``.
  - `-e` sets `env`. `--env-var` is Fontaine's pass-through (`env_vars`), because Fontaine scrubs
    the environment. In `.mcp.json` it is written as `"NAME": "${NAME}"`, which is what Claude
    Code users write.
  - `-e` and `--env-var` with `-t http` are an error.
  - Writes:
    - `local`: `mcp-local.toml`;
    - `user`: `config.toml`;
    - `project`: `.mcp.json`, in Claude Code's shape (`type`, `command`, `args`, `env` /
      `type`, `url`).
  - Prints a confirmation and the file it modified:
    ``Added stdio MCP server `foo` (echo hi) to local config`` and `File modified: <path>`.
  - Adding a project server records no approval. The next interactive session asks, as Claude
    Code does.
  - Not offered: `-H`/`--header` (#32), `--transport sse` (#36), the OAuth flags (#32) and
    `add-json`. Each is rejected with a message that names its issue (clap's own error for an
    unknown flag is enough for `add-json`).
- **`fontaine mcp list`**
  - One line per server, in name order: `<name>: <command line or URL> - <status>`. Status
    is one of:
    - `connected (N tools)`;
    - `failed: <reason>`;
    - `pending approval (run fontaine to approve)`;
    - `not started: workspace not trusted`.
  - It probes exactly the servers `fontaine doctor` would. Lift `doctor_cmd`'s per-server
    gate+probe loop into a shared function (`fontaine/src/mcp_probe.rs`) that both commands call.
  - `--connect=no` skips the probes, and the status is then `not checked`. Default `yes`, like
    doctor.
  - With no servers: `No MCP servers configured. Use fontaine mcp add to add one.`
- **`fontaine mcp get <name>`**
  - Shows scope (label and file), status (one probe as in `list`), type, command, args, the
    `env_vars` names, the `env` keys with values shown as `***`, and URL.
  - Ends with the remove hint ``To remove this server, run: fontaine mcp remove <name> -s <scope>``.
    A plugin server instead names its plugin and `fontaine plugin uninstall`.
  - An unknown name exits 1.
- **`fontaine mcp remove <name> [-s scope]`**
  - Without `-s`: removes it from the one writable scope that holds it. If several do, it exits 1
    and lists each scope with the `-s` command to run, as Claude Code does.
  - `-s project` removes the server from `.mcp.json` and from `.fontaine/settings.toml`,
    wherever it is.
  - Plugin, `--settings` and `settings.local.toml` servers are not writable scopes. The error
    names the file to edit or the plugin to uninstall.
  - An approval in `trust.toml` is left in place. It is keyed by digest, so it is inert.

Exit codes: 0 on success; 1 for unknown name, duplicate, ambiguous remove or I/O error; 2 for
usage and config errors, as `doctor` and `models` do.

### `/mcp` tool counts

- `GuardedSink` (`rust/fontaine-tools/src/mcp/register.rs`) records how many tools its last
  `add`/`reconcile` committed, in an `Arc<AtomicUsize>` that lives in `register::Shared` (which
  the sink gets from `servers.rs`). `Connected` exposes it as `tool_count()`.
- `McpServerState::Connected` becomes `Connected { tools: usize }`. The supervisor slot keeps a
  handle to the counter, and `Supervisor::states()` fills `tools` from the live counter on each
  call, so a `list_changed` refresh shows up. State is set only once, at connect.
- `fontaine-tui`'s own `reports::McpState::Connected` gains the count too, and `/status`
  filters with `matches!(state, McpState::Connected { .. })`.
- `/mcp` renders `connected · 12 tools` (`1 tool` for a single tool). `/status` keeps its
  summary.

## Implementation Steps

1. **Shared `.mcp.json` reader.**
   - Move `parse_mcp_file` and `expand_vars` from `plugins.rs` into
     `fontaine-core/src/mcp_json.rs`, with an `Expansion::{Now(ctx), AtStart}` policy, `type`
     handling, the `headers` warning and `${VAR:-default}`.
   - Plugins call it with `Now`, and their existing tests stay green. `plugins.rs` imports the
     moved expander back for its hook and LSP parsers (and a test), with behavior unchanged apart
     from gaining `:-default`.
   - The project root uses `AtStart` and the skip-with-warning behavior for a malformed file.
2. **Config.**
   - Add `Slot::Local` and `McpServer::expand`.
   - Read `<root>/.mcp.json` as `Slot::Project`, merged under `.fontaine/settings.toml` with the
     collision warning.
   - Read `mcp-local.toml` for this root as `Slot::Local`, applied after project-local and before
     the cli layer. Entries are validated like `[mcp.servers]` and inserted directly with
     `user_trust: true`; a malformed file or entry warns and is skipped.
   - Push a `ScopeEntry` for `mcp-local.toml` (`Slot::Local`) and for `<root>/.mcp.json`
     (`Slot::Project`), so doctor's layer listing (`LayerDiagnosis::from_scopes`), the headless
     init `sources` and the scope_stack log show them.
   - Give `config::diagnose` its own parse path for `mcp-local.toml`, and handle `Slot::Local` in
     `config_catalog::rank`.
3. **Start-time expansion.**
   - `server_spec` takes the `&agent::EnvLookup` and the scrubbed-name set, expands raw
     definitions through them, and returns
     `Result<Option<ServerSpec>, String>`, because a missing or credential variable is an error.
     Its callers are session connect (`agent/mcp.rs`) and the doctor probe. Session connect today
     drops a `None` silently through `filter_map`; for a spec that fails to build it pushes an
     `McpOutcome { error }`, reported through the existing warn path. The doctor probe reports a
     failed server. Reconnect reuses the stored spec.
   - The credential-variable set comes from `tools::scrubbed_env(config, process_env_names)`.
     Session connect passes `setup.env` and `setup.scrub_env`; the shared probe computes
     `scrubbed_env(config, host.env_names)`.
   - `server_description` lists the referenced variable names.
4. **Edit helpers.** `mcp_config.rs` gets `add_server(scope, root, name, &McpServer)` and
   `remove_server(scope, root, name) -> bool` over the three file formats, all under a lock with
   an atomic write. A newly created `.mcp.json` gets normal file permissions, not the owner-only
   mode (`owner_only::write_atomic_keeping_mode` would leave a new target at the tempfile's 0600).
5. **CLI.**
   - Add `Command::Mcp(McpArgs)` with `Add`, `List`, `Get` and `Remove` to `cli.rs`.
   - Implement them in `mcp_cmd.rs`, with the shared probe in `mcp_probe.rs`, which
     `doctor_cmd.rs` now uses too.
6. **Tool counts.**
   - Put the counter in `register::Shared` and expose it on `Connected` (`servers.rs`,
     `register.rs`); the supervisor slot keeps a handle and `Supervisor::states()` fills
     `Connected { tools }` from it.
   - Add the count to `fontaine-tui`'s `reports::McpState::Connected`, change the `/status`
     filter in `ui/commands/reports.rs` to `matches!`, and render it in `/mcp`.
   - Update the match arms and tests (`mcp_supervisor.rs` unit tests,
     `fontaine-core/tests/mcp_reconnect.rs`, `reports.rs` tests).
7. **Docs and records.**
   - Update the pages listed under `## Documentation`.
   - In the master plan, add the Decisions lines below under `### MCP`. In
     `research/research-claude-code-parity.md`, update the MCP-scopes row to done.

## Files to Modify

- `rust/fontaine-core/src/mcp_json.rs` (new), `rust/fontaine-core/src/plugins.rs`
- `rust/fontaine-core/src/config.rs`, `rust/fontaine-core/src/lib.rs`
- `rust/fontaine-core/src/agent/mcp.rs`, `rust/fontaine-core/src/agent/mcp_supervisor.rs`,
  `rust/fontaine-core/src/doctor_report.rs`
- `rust/fontaine-core/src/mcp_config.rs` (new)
- `rust/fontaine-core/src/config_catalog.rs`
- `rust/fontaine-core/tests/mcp_reconnect.rs`
- `rust/fontaine-tools/src/mcp/register.rs`, `rust/fontaine-tools/src/mcp/servers.rs`
  (`Connected`)
- `rust/fontaine/src/cli.rs`, `rust/fontaine/src/lib.rs`, `rust/fontaine/src/mcp_cmd.rs` (new),
  `rust/fontaine/src/mcp_probe.rs` (new), `rust/fontaine/src/doctor_cmd.rs`
- `rust/fontaine-tui/src/ui/mcp.rs`, `rust/fontaine-tui/src/reports.rs`,
  `rust/fontaine-tui/src/ui/commands/reports.rs`, `rust/fontaine-tui/src/server_prompt.rs`
- `docs/mcp.md`, `docs/plugins.md`, `docs/configuration.md`, `docs/doctor.md`, `docs/tui.md`,
  `docs/headless.md`, `docs/index.md`
- `tasks/fontaine_harness_plan.md`, `research/research-claude-code-parity.md`

## Trade-offs

- **Local scope in a user-dir file vs. in `.fontaine/settings.local.toml`.** Writing into the
  repository's local file would reuse a layer, but that file needs the root to be trusted and
  untracked before it gets user trust. It also sits in the work tree, where it can be committed by
  mistake. Claude Code keeps local servers out of the repository for the same reason. A
  user-directory file is always user-trusted and is protected by the floor.

## Decisions

Lines to add under the master plan's `### MCP`:

- (#31) Local-scope servers live in `<config dir>/mcp-local.toml`, keyed by workspace root, not
  in `~/.claude.json`-style JSON: Fontaine's settings are TOML, and the file sits under the
  floor-protected config dir.
- (#31) The repository's `.mcp.json` is read as project scope alongside
  `.fontaine/settings.toml` `[mcp.servers]`, and the TOML wins a name collision with a warning.
- (#31) Project `.mcp.json` `${VAR}` references are expanded when the server starts, never into a
  credential variable Fontaine uses (the set `tools::scrubbed_env` withholds: account and
  telemetry keys, the scanner HF token, `MICROMEGAS_*`, OTLP headers); such a reference fails that server. Claude Code expands any variable. Reason: a
  headless run starts project servers without asking.
- (#31) An unset `${VAR}` with no default fails only that server's start (Claude Code fails the
  config parse).
- (#31) `fontaine mcp add` has `--env-var NAME` for the scrub's pass-through. It has no `-H`,
  `sse` transport, OAuth flags or `add-json` yet (#32, #36).
- (#31) `fontaine mcp get` masks `env` values as `***` (Claude Code prints them): the output
  lands in terminals and logs.
- (#31) `fontaine mcp list` and `get` probe only what `fontaine doctor` would, so an unapproved
  repository server shows as pending and is never started by the CLI.

## Documentation

- `docs/mcp.md`: a new "Scopes" section replacing the bold "Where they are declared" paragraph
  in "Declaring servers", with scopes and
  precedence; `.mcp.json` support and `${VAR}`/`${VAR:-default}` expansion with the credential
  rule, in "Servers a repository defines" (`.mcp.json` as a repository source); the `fontaine mcp`
  subcommands; and tool counts in `/mcp`.
- `docs/plugins.md`: plugin `.mcp.json` expansion now has `${VAR:-default}`, and the `type` and
  `headers` warnings.
- `docs/configuration.md`: `mcp-local.toml` in "Where files live", the scope stack, and the
  `[mcp.servers.<name>]` cross-link.
- `docs/doctor.md`: local and `.mcp.json` sources in the MCP section.
- `docs/tui.md`: the `/mcp` line format.
- `docs/headless.md`: an `mcp` row in the subcommand table, with the same "a prompt of exactly
  `mcp` runs the subcommand" note as the other rows, linking to `mcp.md`.
- `docs/index.md`: mention the `fontaine mcp` CLI on the `mcp.md` entry.

## Testing Strategy

Unit tests (`fontaine-core`):

- `mcp_json`:
  - `type` handling (stdio/http accepted; sse/ws skipped with a warning);
  - the `headers` warning;
  - `${VAR:-default}` with the default used and not used;
  - raw text kept under `AtStart`;
  - plugin expansion unchanged;
  - for the project root, an invalid file or entry warns and is skipped, while a plugin's still
    errors.
- Config loading:
  - a `.mcp.json` server loads as `Slot::Project` and needs approval in `mcp_plan`;
  - a `.fontaine/settings.toml` server of the same name wins and warns;
  - a `mcp-local.toml` entry for this root loads as `Slot::Local`, needs no approval and is
    planned even when project executables are off;
  - an entry for another root is ignored;
  - local beats project and user by name.
- `server_digest` is stable across a changed variable value and changes with a changed reference.
- `server_spec`:
  - expands `${VAR}` into command, args, env and url;
  - fails on an unset variable without a default;
  - fails on a credential variable from `tools::scrubbed_env` (account `api_key_env`, telemetry
    `api_key_env`, `guardian.scanner_hf_token_env`, `MICROMEGAS_*`, OTLP headers);
  - leaves TOML definitions unexpanded.
- `mcp_config` add/remove round trips for each scope:
  - comments and order kept in TOML;
  - `.mcp.json` written in Claude Code's shape;
  - duplicate add is an error;
  - remove of a missing name returns false.
- `server_description` lists the referenced variable names.

Unit tests (`fontaine-tools`, `fontaine-tui`):

- The `GuardedSink` count follows a `list_changed` refresh. Extend the existing
  `mcp_registration.rs` refresh test.
- The `/mcp` listing renders `connected · 2 tools` and `1 tool`.

Integration (`rust/fontaine/tests/mcp_cmd.rs`, driving the binary against temp config/data dirs
and the `repo_mcp_server` example):

- `add` then `list` then `get` then `remove` for each scope.
- `remove` with the same name in two scopes exits 1 and names both `-s` commands.
- `list` shows an unapproved project server as pending and does not start it. Use the `hook_marker`
  example (`project_executables.rs` uses it from line 157), or another binary that writes a marker
  on start, as the `.mcp.json` server command, and assert the marker file is absent after
  `fontaine mcp list`. This is the
  silent failure that matters, a repository's code running from a CLI listing, so it is an
  automated test.
- A `fontaine-core` runtime test (harness in `fontaine-core/tests/common/runtime.rs`, as
  `mcp_reconnect.rs` uses) asserts that a `.mcp.json` server with a credential reference yields an
  `McpOutcome.error` naming the variable.

## Manual Verification

1. From `rust/`, run `cargo build -p fontaine --examples`. In a scratch git repo, run
   `<abs>/rust/target/debug/fontaine mcp add -s project demo -- <abs>/rust/target/debug/examples/repo_mcp_server`,
   then start `<abs>/rust/target/debug/fontaine` and trust the folder.
   Expected: the approval prompt shows the command. After approval, `/mcp` shows
   `demo … connected · N tools`. This checks the prompt and live rendering in a real terminal,
   which `TestBackend` covers only partly.

## Open Questions

None.
