# Plugins and Marketplaces Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/30

## Progress

- 2026-10-28: Implemented end to end (offline): core `plugins.rs` (registry, manifest/catalog/
  hooks/`.mcp.json` parsing, `PluginContributions`, `load_installed`, `Config::apply_plugins`),
  `Slot::Plugin` / `RuleSource::Plugin` / `TypeOrigin::PluginFile` / `McpServer.env`, plugin scope
  in `extensions::load`, the `fontaine plugin` CLI, the `/plugins` report, and the docs/changelog/
  parity/Decisions updates. Core unit and integration tests and the offline CLI tests are in.
- 2026-10-28: Full verification gate is green: `cargo fmt --check`, `cargo clippy --all-targets
  -- -D warnings`, `cargo test` (all suites), `cargo deny check`, and the Windows
  `x86_64-pc-windows-gnu` clippy target. Fixes along the way: collapsed a nested `if` in
  `expand_vars` (`plugins.rs`), `inspect_err` instead of `map_err` cleanup in the CLI, a
  needless borrow at the two `materialize` call sites, a `cloned_ref_to_slice_refs` in
  `tests/plugins.rs`, and `/plugins` moved after `/plan` in the `BUILTINS` table (the
  sorted-by-name test caught it).
- 2026-10-28: **Manual Verification 1–2 done.** Check 1 (local marketplace): add/install/list
  against a real local git marketplace, both pins moved through `marketplace update` +
  `plugin update`, and a real headless `fontaine -p` run loaded and invoked the namespaced
  `review:review` skill (local Ollama model, fully offline). A PreToolUse hook added to the
  fixture fired on a real tool call with `${CLAUDE_PLUGIN_ROOT}` expanded to the installed
  checkout. Check 2 (real HTTPS marketplace): `claude-market/marketplace` added and installed
  unmodified; `plugin-builder:init` loaded and ran in a headless session. Real-world findings
  and fixes:
  - **Fixed: manifest component paths naming files were dropped.** Real manifests (e.g.
    claude-market's `plugin-builder`) list `skills`/`commands`/`agents`/`outputStyles` as
    `.md` file paths, not directories; the loaders only scanned directories, so the plugin
    installed with 0 components. `skill_components`, `walk_markdown` and `markdown_files`
    now accept a file base, and a manifest-named skill file takes its default name from the
    file stem (a `skills/<name>/SKILL.md` keeps its directory name). Covered by a new test.
  - Per the recorded Decisions, the official Anthropic marketplaces (`anthropics/claude-
    plugins-official`, `anthropics/claude-plugins-community`) are **refused at `marketplace
    add`** because their catalogs carry `git-subdir` source entries — correct per plan, but
    it means the most prominent real marketplaces do not load. Revisit as a follow-up if
    users hit it.
  - Real agent files ship Claude Code model aliases (`model: haiku`), which install refuses
    per the tighten-only rules (a plugin agent cannot pick a model). Also per plan; noted as
    a portability cost.
  - Minor: `plugin marketplace list`'s plugin count is installed plugins per marketplace,
    not catalog entries.
- 2026-10-28: **Code-review fixes (plugins).** `Config::apply_plugins` is idempotent (`/resume`
  and `/branch` reuse the resolved config), two installed plugins whose servers compose to one
  name are a startup error instead of silently shadowing each other, and manifest
  `hooks`/`mcpServers` path keys now merge over the default file (inline objects warn). Four
  regression tests added; fmt/clippy/tests green.
  Status: **done.**
- 2026-10-28: **Review fixes on `resilience`.** A plugin hook identical to another scope's hook
  now merges by attribution only (`union_hooks` no longer replaces the kept origin for
  plugin-origin hooks): the previous replacement let `apply_plugins`' strip phase remove a
  project's own hook after the plugin was disabled. `apply_plugins` also clears stale plugin
  attribution on surviving hooks before re-applying. A path-source install canonicalizes the
  marketplace checkout before the escape check (symlinked config dirs failed spuriously), a
  plugin `.mcp.json` URL server carrying `args`/`env` is a load error (settings refuse the
  same), and `short_pin` lives in `fontaine_core::plugins` (was duplicated in the CLI and the
  TUI).

## Overview

Claude Code's plugins bundle skills, agents, hooks, MCP servers, LSP servers and output styles
into one directory with a manifest, installed from git-hosted marketplaces that catalog them
([plugins/overview](https://code.claude.com/docs/en/plugins/overview)). This plan adds the same
unit to Fontaine: a plugin is a directory in the extension layout (`skills/`, `agents/`,
`output-styles/`, `commands/` as skills, hooks, MCP servers) plus a manifest, and a marketplace
is a git repository with a catalog file. Fontaine reads Claude Code's manifest paths and
component formats so existing plugins and marketplaces load unmodified.

Installs are user-scope only and pinned by commit: `fontaine plugin install` is the explicit
action that approves a plugin's executables, the way trusting a workspace approves a repository's.
Nothing in project scope can add a marketplace, install a plugin or enable one, because a plugin
runs code (code execution = loosening). Installed plugin content is still third-party, so it
loads under tighten-only rules: its `allow` never counts, its skills cannot lend `allowed-tools`,
its agent files can only narrow, and the only permission or guardian effect a plugin may have is
deny-side additions. LSP server components are not supported (Fontaine has no LSP integration).

## Current State

- Extension files load from exactly two roots in `extensions::load`
  (`rust/fontaine-core/src/extensions.rs:391`): `<config dir>/` (`Scope::User`) and
  `<root>/.fontaine/` (`Scope::Project`), through `LoadParams { config_dir, project_root, servers }`
  (`extensions.rs:93`). `Scope` (`extensions.rs:44`) has only `User` and `Project`; the trust line
  is drawn per file: a project skill's `allowed-tools` is validated then ignored
  (`load_skill`), a project agent file cannot pick a model and can only narrow (`load_agent`).
- Hooks and MCP servers are settings, not files in the extension layout: `[[hooks.pre_tool_use]]`
  etc. and `[mcp.servers.<name>]` merge in `config.rs` (`union_hooks` :1928, server merge :1780),
  each carrying an `Origin { slot, user_trust }` (:485). `Origin::is_project()` gates repository
  executables on workspace trust; `user_trust` decides whether a hook's `allow`/`retry` counts.
  `Slot` is the config column enum in the same file.
- The workspace-trust prompt summarizes project hooks/servers/skills in
  `trust_summary` (`rust/fontaine/src/lib.rs:160`); headless loads project executables without
  asking.
- Session startup loads extensions at `rust/fontaine/src/lib.rs:687`; the TUI lists them with
  `/skills`, `/hooks` and `/mcp` (`fontaine-tui/src/commands.rs` `BUILTINS`).
- The guardian's prose rule lists come from `[auto_mode]` spliced over the shipped defaults in
  `Config::effective_auto_mode` (`config.rs:1546`); `hard_deny`/`soft_deny` additions from project
  scope are accepted (tightening), `allow` is not.
- Programmatic config writes have a precedent in the trust file
  (`trust::trust_path(data_dir)`, written when a workspace is trusted), and owner-only atomic
  writes in `owner_only::write_atomic` (as `SessionTodos` uses). `git` is shelled out to through
  `std::process::Command` where needed (`memory.rs`). `gix` is already a dependency, used only
  for read-only repository discovery under an isolated configuration (`trust.rs`, `ledger.rs`);
  it is not used for cloning.
- MCP stdio servers launch with a scrubbed environment (`fontaine-tools` `BASE_ENV`, moved out of
  `fontaine-mcp`); hook commands run through the shell with the same scrub.
- `McpServer` (config.rs) has `command`/`args`/`env_vars` or `url`; `env_vars` passes named
  variables through, and `[env]` entries are set on the child. Claude Code's `.mcp.json` instead
  carries an `env` map of literal values with `${VAR}` expansion.
- `research/research-claude-code-parity.md:341` tracks this feature as `missing | backlog`;
  mods are deliberately skipped (:342) because they can approve calls past deny rules.

## Design

### Layout and manifest

A plugin directory uses Claude Code's layout, read at the same paths so plugins port by copying:

```text
my-plugin/
  .claude-plugin/plugin.json    # manifest: name (required), version, description, author, ...
  skills/<name>/SKILL.md
  agents/<name>.md
  output-styles/<name>.md
  commands/<name>.md            # loaded as user-invocable skills, as Claude Code does
  hooks/hooks.json              # Claude Code's hook schema
  .mcp.json                     # Claude Code's MCP schema
```

`plugin.json` also carries Claude Code's component-path keys — `commands`, `agents`, `skills`,
`hooks`, `mcpServers`, `outputStyles` — which point at files or directories inside the plugin.
They are honored with Claude Code's per-key semantics, because getting them wrong would drop or
duplicate a ported plugin's content: `commands`, `agents` and `outputStyles` *replace* the
default scan, `skills` *adds* to it, and `hooks` and `mcpServers` *merge* over the default file
(`hooks` and `mcpServers` take a file path or a list of paths; an inline config object is not
loaded and warns).
(`lspServers` paths get the same ignore-with-warning as a default LSP component.)
`commands/<name>.md` files, default directory or manifest paths, load as user-invocable skills,
as Claude Code's commands-merged-into-skills does; plugin skill rules apply to them. Other
unknown keys are ignored (as frontmatter unknown keys are), except the two Fontaine extension
keys below, which are validated. Keys with semantics Fontaine does not implement —
`dependencies`, `userConfig`, `settings` — warn at install and at startup rather than silently
degrading (see Component mapping). `${CLAUDE_PLUGIN_ROOT}` in hook commands and `.mcp.json`
values expands to the installed checkout path at load time, and `${CLAUDE_PROJECT_DIR}` to the
session's primary root, as Claude Code does; any other `${...}` reference left unexpanded warns
at load.

A manifest may carry two Fontaine-only keys (a Claude Code loader ignores them):

```json
{
  "name": "my-plugin",
  "permissions": { "deny": ["shell(git push *)"], "ask": ["write(**/.env*)"] },
  "guardian": { "hard_deny": ["never push on Fridays"], "soft_deny": [] }
}
```

Both are deny-side only: an `allow` key in either, or a `guardian.environment` key, is a load
error naming the plugin (startup stops with exit code 2, as extension load errors do). The rules
must parse with `parse_rule`; they merge into the session's deny/ask with a new
`RuleSource::Plugin` and into the `hard_deny`/`soft_deny` lists spliced in
`effective_auto_mode`.

### Marketplace

A marketplace is a git repository or local directory with `.claude-plugin/marketplace.json`:

```json
{
  "name": "team-tools",
  "owner": { "name": "..." },
  "plugins": [
    { "name": "review", "source": "./plugins/review", "description": "...", "version": "1.2.0" },
    { "name": "other", "source": { "source": "github", "repo": "owner/repo" } }
  ]
}
```

`source` is a relative path inside the marketplace repository (`"./plugins/review"`), or an
object with `source: "github"` (`repo: "owner/repo"`, cloned over HTTPS) or `source: "url"`
(`url`, any git URL) — Claude Code's plugin-source names. `github` and `url` entries may pin a
`ref` (branch or tag) or `sha`, which install honors (see the CLI table). Claude Code's other
plugin source types (`git-subdir`, `npm`, `archive`, `command`) are refused with an error naming
the entry, never silently skipped. `version` is informational; the pin is the commit.
Marketplace and plugin names use `valid_name`'s alphabet minus `.` and must start with a letter
or digit; they may not contain `__`, the separator MCP tool names split on, and `.` stays out so
a composed MCP server name is always a valid rule and tool name (see Component mapping).

### Registry and checkouts

One new tree under the config dir:

```text
<config dir>/plugins.toml                    # registry, written programmatically, owner-only
<config dir>/plugins/marketplaces/<name>/    # git clone of the catalog, pinned
<config dir>/plugins/installed/<marketplace>/<name>/    # the plugin checkout or copy, pinned
```

`plugins.toml` (TOML, `deny_unknown_fields`):

```toml
[[marketplace]]
name = "team-tools"
url = "https://github.com/team/tools-marketplace"   # or a local path
pin = "a1b2c3..."                                 # pinned catalog commit; a content hash for a local directory

[[plugin]]
name = "review"
marketplace = "team-tools"
source = "./plugins/review"
pin = "d4e5f6..."                                 # pinned install commit; for a path source, the marketplace's pin
enabled = true
```

Plugin names are unique across marketplaces in the registry — component names are `plugin:name`
— and install enforces it by refusing a name that is already installed from anywhere.
Marketplace names are unique too: `marketplace add` refuses a registry name already registered.
A plugin that lives at a path inside its marketplace repository shares that repository's
history, so its `pin` is the marketplace's pin; only `github`/`git` sources get a pin of their
own.

The file is written with `owner_only::write_atomic` under a mutex, never across an await, as
`SessionTodos` writes `todos.json`. Every mutation holds a lock file beside `plugins.toml`
around the whole read-modify-write, so two concurrent `fontaine plugin` processes serialize
instead of losing each other's entries. Clones and copies land in a temporary directory beside
the target and are renamed into place while the lock is held, so two concurrent `marketplace
add` runs cannot race on the checkout directory. A missing file means no marketplaces and no plugins. A
corrupt file loads nothing and warns once through `Approver::warn`, as an unparseable
`todos.json` does: a broken registry never silently enables or disables anything, it just skips
plugins for the run.

### CLI: `fontaine plugin`

New `Command::Plugin` subcommand beside `auto-mode` in `rust/fontaine/src/cli.rs`. All mutations
are CLI commands, not session commands: they run outside any session, need no permission pipeline
(the user typed them), and keep installs out of a model-reachable surface.

| Command | Effect |
|---|---|
| `fontaine plugin marketplace add <url-or-path> [--name N]` | Clone (or copy a local dir), resolve the pin, read the catalog, register it under the catalog's `name` (`--name` overrides the registry key; the catalog keeps its own). Refuses a registry name that is already registered. A catalog that does not parse is an error and nothing is kept. |
| `fontaine plugin marketplace list` | Name, url, pin, plugin count. |
| `fontaine plugin marketplace update [name]` | Fetch, move the pin to the remote default-branch HEAD, report old -> new sha. Does not touch installed plugins. |
| `fontaine plugin marketplace remove <name>` | Refuses while an installed plugin names it; else drops clone and registry entry. |
| `fontaine plugin install <name>@<marketplace>` | Resolve the entry in the pinned catalog. A `github`/`url` source clones the remote and checks out the entry's `sha` (or `ref`) when it sets one, the remote's current HEAD otherwise; the resolved commit becomes the pin. A path source copies from the pinned marketplace checkout and takes the marketplace's pin (same repository). The manifest's `name` must equal the catalog entry's — namespacing and MCP server composition both use it — and a mismatch is an install error (stricter than Claude Code, which namespaces under the manifest name; see Decisions). Validate the manifest and every component, record it enabled. Refuses a plugin name already installed from any marketplace (registry-wide uniqueness). Fails atomically: a failed install leaves no registry entry and no checkout. |
| `fontaine plugin uninstall <name>` | Remove checkout and registry entry. |
| `fontaine plugin update <name>` | A `github`/`url` source is re-fetched and the pin moves to the new HEAD of the entry's `ref` (default branch when unset); an entry pinned to a `sha` stays on it. A path source is re-copied from the current marketplace checkout and takes the marketplace's pin, so it moves only after `marketplace update` (the two-step stays explicit). Re-validate; a validation failure keeps the old checkout live. |
| `fontaine plugin enable / disable <name>` | Flip the registry flag; a disabled plugin stays on disk and contributes nothing. |
| `fontaine plugin list [--format json]` | Name, marketplace, pinned short sha, enabled, component counts. |

Git operations shell out to `git` through `std::process::Command` (fixed argv, no shell), as
`memory.rs` does; a missing `git` is an error naming it. Pinning: `git clone`, then
`git rev-parse HEAD` recorded as the pin, and the checkout stays at that sha; update is
`git fetch` + checkout of the new sha + registry rewrite. A local-directory marketplace is copied
and pinned by a content hash of the working tree — catalog and plugin sources, `.git`
excluded — so a content change moves the pin even when the catalog file is untouched, while a
metadata-only git operation does not; `update` re-copies and re-hashes it.

Install/update print what the plugin brings before touching the registry: counts of skills,
agents, styles, hooks, servers, its deny/ask and guardian additions, and every warning the load
collected (unsupported Claude Code components, skipped hook entries, external MCP server
references, unexpanded `${...}` references), so the approval the install represents is an
informed one.

### Session loading

New module `rust/fontaine-core/src/plugins.rs`:

```rust
pub struct PluginDir { pub name: String, pub root: PathBuf }
pub struct PluginContributions {
    pub dirs: Vec<PluginDir>,                 // enabled, installed; for extension loading
    pub hooks: Vec<Hook>,                     // Origin { slot: Slot::Plugin, user_trust: false }
    pub servers: BTreeMap<String, McpServer>, // same origin in mcp_origins
    pub deny: Vec<SourcedRule>,               // RuleSource::Plugin
    pub ask: Vec<SourcedRule>,
    pub hard_deny: Vec<String>,
    pub soft_deny: Vec<String>,
    pub warnings: Vec<String>,
}
pub fn load_installed(config_dir: &Path) -> Result<PluginContributions, PluginError>
```

`PluginError` names the plugin and the file at fault; a startup that gets one stops with exit
code 2, as extension load errors do. `warnings` is for non-fatal degradation only: a vanished
checkout, an ignored LSP or other unsupported Claude Code component (`bin/`, plugin
`settings.json`, `themes/`, `monitors/`, `workflows/`, `channels`, `dependencies`,
`userConfig`), a hook entry skipped over an un-mappable matcher or an exec form, an unexpanded
`${...}` reference, an agent file skipped over an absent external MCP server.

- `extensions::LoadParams` gains `plugins: &[PluginDir]`; `load` walks each plugin's `skills/`,
  `commands/` (as skills), `agents/`, `output-styles/` after the user root and before the
  project root. Files load with a new `Scope::Plugin`.
- Config gains `Slot::Plugin`. Plugin hooks and servers carry
  `Origin { slot: Slot::Plugin, user_trust: false }`. The `user_trust: false` is what voids a
  hook's `allow`/`retry` (`can_allow`/`can_retry` in `user_hooks.rs` return it), and
  `is_project()` staying false keeps plugin executables out of the workspace-trust gate: install
  approved them, in the TUI and headless alike. No `is_project()` call site changes — its checks
  are the trust gate, the trust summary and origin display, and plugins join none of them.
  `trust_summary` is untouched: plugins are not repository content.
- Startup (`rust/fontaine/src/lib.rs`, the shared `prepare_session` path the TUI and headless
  runs both take) calls `load_installed`, merges hooks/servers/rules into the `Config` after
  `config::load`, and passes the dirs into
  `extensions::load`. `servers` in `extensions::LoadParams` includes the composed plugin server
  names (`plugin_<plugin>_<name>`), so a plugin agent file may name its own plugin's `mcp__`
  tools.
- A plugin whose checkout vanished (deleted by hand) contributes nothing and warns once; a plugin
  whose manifest no longer validates (after a hand edit) is the `Err` of `load_installed`, a
  startup error naming it, as with extension files.

### `Scope::Plugin` semantics

Plugin prompt content is third-party even though the user installed it, so it follows
`Scope::Project`'s permission rules, not `Scope::User`'s:

- A plugin skill's `allowed-tools` is validated and ignored: a plugin cannot approve calls.
- A plugin agent file follows the project rule exactly: it cannot pick a model, its
  `permissionMode` is absent, `default`, `manual` or `dontAsk` (`inherit` is an error, as for
  project files), and its `tools` must be tools the session has (built-ins or servers of the
  session, including the plugin's own). One relaxation: a `tools` entry
  naming an MCP server the plugin does not ship and the session does not have skips the agent
  file with a warning instead of failing startup — whether that server exists is the user's
  configuration, not a defect in the file. Install validates against built-ins and the plugin's
  own composed servers and warns about such external references in its summary. Entries naming
  anything else the session lacks (an unknown tool, a missing server the plugin itself ships)
  stay load errors.
- `!` commands in plugin skills run without the workspace-trust gate: install approved the
  plugin's content, as it did for its hooks and MCP servers. They still go through the shell
  tool's permission pipeline (`skills.rs` `run_command`).
- The executable-key refusal (`hooks`, `mcpServers` in frontmatter) applies unchanged.

The project-rule checks compare `scope == Scope::Project` (`load_agent`'s model and `tools`
checks in `extensions.rs`): adding the variant compiles without touching them, which would
silently drop the checks for plugin files. They move to a `Scope::is_tightened()` helper — true
for `Project` and `Plugin` — or to exhaustive matches, so a future variant cannot skip them
either. The same trap exists for `TypeOrigin`: `AgentType::accepts_model_override`
(`registry.rs`) is a `matches!` on `ProjectFile`, which would silently let a `PluginFile` type
accept per-call model overrides; it becomes an exhaustive `match`, with a test that a plugin
agent type rejects model overrides. The trust gate in `skills.rs` `run_command` stays
`Project`-only by the decision above.

Components are namespaced `plugin:name`, as Claude Code's `/my-plugin:review`: `valid_name`
gains a plugin form that allows `:`-separated segments, and the loader prefixes skill, agent and
style names with the plugin name. Plugin `agents/` and `commands/` directories are walked
recursively, each subdirectory adding a segment, as Claude Code does
(`agents/review/security.md` loads as `plugin:review:security`). A component's own declared
`name` must still match the plain `valid_name` form — the `:` form exists only after prefixing.
Cross-scope collisions are impossible by construction: user and project names cannot contain `:`
and plugin names are unique in the registry, so no cross-scope tie-break is needed. Within one
plugin, though, two components can land on the same name (`skills/foo/SKILL.md` and
`commands/foo.md` both become `plugin:foo`): that is an error naming the plugin, caught at
install validation and again at load — never the silent same-name replacement `load` applies
across scopes.
`/skills` shows the scope as `plugin <name>`; `TypeOrigin` gains `PluginFile(PathBuf)` beside
`UserFile`/`ProjectFile`, treated like `ProjectFile` where the registry asks whether a type can
widen (`registry.rs:140`, converted to an exhaustive match — see above).

### Component mapping

- **`hooks/hooks.json`**: Claude Code's schema (`{"hooks": {"PreToolUse": [{"matcher": "...",
  "hooks": [{"type": "command", "command": "...", "timeout": N}]}]}}`) maps onto `Hook` entries:
  `timeout` seconds to `timeout_secs`, `${CLAUDE_PLUGIN_ROOT}` expanded. Claude Code matchers are
  regexes over tool names while a Fontaine matcher is a permission rule, so the supported subset
  is: a single tool name (`Bash` -> `shell` through `map_tool_entry`), an alternation of
  mappable names (`Write|Edit` expands to one `Hook` per alternative), and a trailing `.*` or
  `*` (the rule's wildcard form). The bare in-plugin server rewrite applies to matchers too, so
  a hook matching one of the plugin's own servers fires on the composed name. A matcher outside
  this subset skips its hook entry with a warning (in `PluginContributions::warnings`, shown in
  the install summary and at startup) rather than failing the plugin: an un-mappable regex must
  not make an idiomatic plugin uninstallable, and the skipped hook is visible. A structurally
  malformed entry is still a load error naming the file. Unknown events (`SessionStart`, ...) are
  ignored — Fontaine's hook events are exactly `PreToolUse`, `PostToolUse` and
  `PermissionDenied`, mapping onto `pre_tool_use`, `post_tool_use` and `permission_denied`. Only
  shell-form `type: "command"` hooks are supported: any other `type` (`prompt`, `http`, agent
  hooks) is a load error naming the file, and an entry with `args` (Claude Code's exec form)
  skips with a warning, as an un-mappable matcher does. A manifest `hooks` path (or list of
  paths) merges over the default `hooks/hooks.json`; an inline object warns.
- **`.mcp.json`**: `{"mcpServers": {"<name>": {"command", "args", "env"}}}` or `{"url"}` maps
  onto `McpServer`. `env` values support `${VAR}` expansion; to carry literal values `McpServer`
  gains an optional `env: BTreeMap<String, String>` applied to the child after the scrub, beside
  `env_vars`. Server names are composed as Claude Code composes them, `plugin_<plugin>_<name>`,
  so a ported plugin's `mcp__plugin_<plugin>_<name>__<tool>` references resolve unchanged. A
  plugin file that names one of the plugin's own servers bare (`mcp__<name>__*` in `tools`,
  `disallowedTools` or `allowed-tools`) is rewritten to the composed name at load, before
  validation against the session's servers. A composed name that equals any configured
  non-plugin server, or that two installed plugins both compose to, is a startup error naming
  the plugin: a plugin never shadows a server the user, the repository or another plugin
  defined. A manifest `mcpServers` path (or list of paths) merges over the default `.mcp.json`
  (a later file replaces a same-named server); an inline object warns. `.mcp.json` server keys may not
  contain `__`. Long composed names hit provider tool-name length limits sooner; the install
  summary shows them so an oversized name is visible before install.
- **LSP servers**: a `lspServers` key or directory is ignored with a startup warning naming the
  plugin, so a plugin that needs one is visibly degraded rather than silently.
- **Other Claude Code components**: `bin/` executables (on the Bash `PATH` in Claude Code),
  plugin `settings.json`, `themes/`, `monitors/`, `workflows/` and `channels` have no Fontaine
  equivalent. Each warns at install (in the summary) and at startup when present, like the LSP
  component. A `dependencies` or `userConfig` manifest key warns the same way: Fontaine does not
  resolve plugin dependencies and does not prompt for plugin configuration, so
  `${user_config.*}` references stay literal, which the unexpanded-`${...}` warning then
  surfaces.

### TUI

One read-only command: `/plugins` lists installed plugins (name, marketplace, pinned short sha,
enabled, component counts), in the style of `/skills` and `/hooks`. Installs and marketplace
changes stay on the CLI; an interactive browse-and-install surface is a follow-up. `/mcp` and
`/hooks` show plugin servers and hooks with their origin, as they already show scope.

## Implementation Steps

1. `fontaine-core/src/plugins.rs`: registry (`plugins.toml`) read/write with
   `owner_only::write_atomic`, manifest/catalog/component JSON parsing (`serde_json` is already a
   core dependency), `PluginContributions`, `load_installed`. Unit tests.
2. `fontaine-core`: `Slot::Plugin` in `config.rs`; plugin hooks and servers get
   `Origin { slot: Slot::Plugin, user_trust: false }` — the `user_trust: false` voids
   `allow`/`retry`, and no `is_project()` call site changes. The full call-site inventory is the
   workspace-trust gate (`user_hooks.rs`), the trust summary and origin display, the
   `union_hooks` rank (`config.rs`) and `mcp_plan`'s approval gate (`agent/mcp.rs`); the last two
   give plugin origins the right behavior by construction (an identical plugin/project hook
   keeps the plugin origin; a plugin server needs no repository approval), and tests pin both;
   `RuleSource::Plugin` in `permission_rules.rs`; `McpServer.env`; merge of plugin hooks,
   servers and deny/ask rules into `Config` (a `Config::apply_plugins(&PluginContributions)`);
   plugin `hard_deny`/`soft_deny` spliced in `effective_auto_mode`; `TypeOrigin::PluginFile`.
3. `fontaine-core/src/extensions.rs`: `Scope::Plugin`, namespaced names (`valid_name` plugin
   form, prefixing), `LoadParams.plugins`, the bare in-plugin MCP reference rewriting, plugin
   files under project permission rules.
4. `fontaine/src/cli.rs` + a new `fontaine/src/plugin.rs`: the `plugin` subcommand tree, git
   helpers (clone/fetch/rev-parse via `std::process::Command`), install/update/uninstall/enable/
   disable, the pre-install component summary, atomic failure semantics.
5. `fontaine/src/lib.rs`: `load_installed` + `Config::apply_plugins` on the shared session
   startup path, plus the vanished-checkout warning, `extensions::LoadParams` call-site updates
   (also `fontaine-tui/tests/common/mod.rs:338`, `rust/fontaine-core/tests/common/runtime.rs:166`
   and `rust/fontaine-core/tests/extensions.rs:28`).
6. TUI: `/plugins` (`Command::Plugins`, `BUILTINS` sorted position, dispatch arm, listing);
   `/mcp` and `/hooks` origin labels.
7. Docs, changelog, parity-matrix row, master-plan Decisions lines (see Decisions).
8. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
   `cargo deny check`, the Windows clippy target.

## Files to Modify

- `rust/fontaine-core/src/plugins.rs` (new), `lib.rs`, `config.rs`, `config_scope.rs` (no new
  settings keys; the pin tests cover the `Settings` schema only, so `plugins.toml` does not
  interact with them),
  `permission_rules.rs`, `extensions.rs`, `rust/fontaine-agents/src/registry.rs`
- `rust/fontaine/src/cli.rs`, `plugin.rs` (new), `lib.rs`
- `rust/fontaine-tui/src/commands.rs`, the `/plugins` report beside the `/skills` one
- `rust/fontaine-core/src/agent/mcp.rs`, where `Transport::Stdio.env` is populated: merge the new
  `McpServer.env` map over the `[env]` entries (the transport already applies a literal env map
  after the scrub)
- Tests: `rust/fontaine-core/tests/plugins.rs` (new), `extensions.rs`, `config.rs`,
  `permission_rules.rs`, `choke_point.rs` (plugin MCP names are dynamic `mcp__*`, expected sets
  unchanged), `rust/fontaine/tests/plugin.rs` (new, CLI), `rust/fontaine-tui/tests/tui.rs`,
  `rust/fontaine-core/tests/docs.rs` (new `plugins.toml` validator arm)
- Docs: `docs/plugins.md` (new), `docs/index.md`, `docs/extensions.md`, `docs/mcp.md`,
  `docs/hooks.md`, `docs/permissions.md`, `docs/guardian.md`, `docs/tui.md`, `docs/headless.md`,
  `docs/configuration.md`, `CHANGELOG.md`, `research/research-claude-code-parity.md`,
  `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Plugin content tighten-only vs. user-trusted**: Claude Code treats an installed plugin's
  hooks as the user's own (their `allow` counts). Fontaine treats install as approval to *run*,
  not approval to *loosen*: the user reviewed a summary, not every hook's future verdicts. Costs
  some convenience (a plugin cannot ship a working `allow` hook); keeps the
  third-party-can-never-loosen invariant uniform.
- **CLI-only mutations**: no in-session install means no prompt-driven install path at all, at
  the cost of Claude Code's `/plugin` browser convenience.
- **git CLI over a library**: cloning needs the user's git configuration and credentials
  helpers — the only way private marketplaces authenticate — which `gix` deliberately isolates
  away from (see `trust.rs`). The CLI matches `memory.rs` and needs `git` on `PATH`, which
  plugin use of a git-hosted marketplace implies anyway.
- **Namespacing (`plugin:name`)**: matches Claude Code, makes collisions impossible, and makes a
  component's provenance visible in every prompt and listing; it extends the name grammar with
  one `:` form.

## Decisions

- 2026-10-28: Plugin hooks' `allow`/`retry` never count and plugin skills/agents follow project
  permission rules, even though install is a user action (stricter than Claude Code). Record in
  the master plan.
- 2026-10-28: Installs, updates and marketplace changes are CLI commands only; the TUI gets a
  read-only `/plugins`. No interactive marketplace browser in v1. Record in the master plan.
- 2026-10-28: LSP server components are unsupported (ignored with a warning); Fontaine has no
  LSP integration. Mods stay skipped (parity doc). Record in the master plan.
- 2026-10-28: Plugins may add `deny`/`ask` rules and `hard_deny`/`soft_deny` guardian entries
  via Fontaine-only manifest keys, and nothing else that touches permissions or the guardian
  (per the issue: guardian rules unmodifiable by plugins except deny-side additions).
- 2026-10-28: No project or local scope can add a marketplace or install/enable a plugin
  (Claude Code allows project-scope enabling via `.claude/settings.json`); `plugins.toml` lives
  in the user config dir and is not a settings file. Record in the master plan.
- 2026-10-28: Manifest and component files are read at Claude Code's paths
  (`.claude-plugin/plugin.json`, `hooks/hooks.json`, `.mcp.json`,
  `.claude-plugin/marketplace.json`) so existing marketplaces work unmodified; no
  `.fontaine-plugin/` alias.
- 2026-10-28: Installs pin a commit, not a version tag; the catalog's `version` is display text.
  Plugin-to-plugin dependencies are not supported (Claude Code has them); a `dependencies` key
  warns at install and startup instead of resolving.
- 2026-10-28: `${CLAUDE_PLUGIN_ROOT}` keeps its Claude Code name for portability, expanding to
  the installed checkout.
- 2026-10-28: A path-source plugin's pin is the marketplace's pin (same repository), and
  `plugin update` for one re-copies from the current marketplace checkout, so it moves only
  after `marketplace update` — the explicit two-step stands.
- 2026-10-28: `!` commands in plugin skills run without the workspace-trust gate — install is
  the approval, as for plugin hooks and MCP servers — and still pass the permission pipeline.
- 2026-10-28: Hook matchers support single tool names, `|` alternation and trailing wildcards;
  a matcher outside that subset skips its hook entry with a warning rather than failing the
  plugin (Claude Code matchers are regexes over tool names).
- 2026-10-28: A plugin agent file naming an MCP server the plugin does not ship and the session
  does not have is skipped with a warning, not an exit-2 startup error: whether that server
  exists is the user's configuration, not a defect in the file.
- 2026-10-28: A local-directory marketplace pins a content hash of its working tree (`.git`
  excluded), so plugin content changes move the pin but metadata-only git operations do not.
  Registry mutations hold a lock file beside `plugins.toml` around the read-modify-write,
  clones and copies rename into place under the lock, and `marketplace add` refuses a duplicate
  registry name.
- 2026-10-28: Claude Code's plugin source types are `github`, `url` and relative paths only;
  `git-subdir`, `npm`, `archive` and `command` sources are refused with a named error (Claude
  Code supports them). Entry `ref`/`sha` pins are honored at install.
- 2026-10-28: The catalog entry's `name` must equal the manifest's, an install error on
  mismatch; Claude Code tolerates the mismatch (the entry name is the install id, the manifest
  name the namespace). Requiring equality keeps namespacing and MCP server composition
  unambiguous.
- 2026-10-28: Bare in-plugin `mcp__<server>__*` references (in `tools`, `disallowedTools`,
  `allowed-tools` and hook matchers) are rewritten to the composed `plugin_<plugin>_<server>`
  name; Claude Code does not rewrite — a bare matcher there never fires. The rewrite keeps
  idiomatic plugin files working.
- 2026-10-28: Only shell-form `command` hooks load; other hook types are load errors and
  exec-form (`args`) entries skip with a warning (Claude Code runs them).
- 2026-10-28: Claude Code components with no Fontaine equivalent (`bin/`, plugin
  `settings.json`, `themes/`, `monitors/`, `workflows/`, `channels`, `userConfig`) warn at
  install and startup instead of silently degrading; `${CLAUDE_PROJECT_DIR}` expands and other
  unexpanded `${...}` references warn.
- 2026-10-28: Plugin and marketplace names follow Claude Code's alphabet minus `.` (see the
  master plan's Decisions; `.` breaks composed MCP server names in the rule grammar). Plugin
  `agents/` and `commands/` load recursively, with multi-segment `plugin:sub:name` names.
- 2026-10-28: A manifest `hooks`/`mcpServers` key takes a file path or list of paths that merge
  over the default file; Claude Code's inline config-object form is not loaded and warns instead
  of failing the plugin.
- 2026-10-28: Two enabled plugins whose MCP servers compose to one name are a startup error, not
  a silent shadow: `_` in plugin and server names makes the composed name ambiguous.

## Documentation

- `docs/plugins.md` (new): what a plugin is, the layout and manifest (including the Fontaine-only
  `permissions`/`guardian` keys), marketplaces, every CLI command, pinning and updates
  (including the path-source pin rule), enable/disable, the tighten-only rule and what install
  approves, namespacing, LSP unsupported, porting notes for Claude Code plugins (the supported
  hook-matcher subset and source types, and what is refused with an error versus skipped with a
  warning).
- `docs/index.md`: the one-line entry. `docs/extensions.md`: plugin scope in the roots table and
  the scope rules. `docs/mcp.md` / `docs/hooks.md`: plugin origin behavior (start without the
  workspace-trust gate; `allow`/`retry` ignored). `docs/permissions.md`: `RuleSource::Plugin`,
  deny/ask additions. `docs/guardian.md`: deny-side guardian additions from plugins.
  `docs/tui.md`: `/plugins`. `docs/headless.md`: plugins load in headless runs.
  `docs/configuration.md`: `plugins.toml` described as a registry, not a settings file.
- `docs/` TOML blocks are schema-validated by `rust/fontaine-core/tests/docs.rs`, where a bare
  ```toml block is checked as a settings file: `tests/docs.rs` gains a `plugins.toml` arm beside
  `trust.toml`/`credentials.toml`, and the registry examples are tagged ```toml plugins.toml.
  Any JSON examples must parse as the documented schemas.

## Testing Strategy

All offline. Git fixtures are local repositories (a path or `file://` URL as the marketplace
source needs no network); JSON fixtures stand in for remote catalogs.

- Unit: `plugin.json` / `marketplace.json` / `hooks.json` / `.mcp.json` parsing, unknown-key
  tolerance, malformed entries naming the file; the component-path manifest keys honored with
  their per-key replace/add/merge semantics, `skills` and `outputStyles` included, and manifest
  `hooks`/`mcpServers` paths merging over the default files with an inline object warning;
  `commands/*.md` loading as skills; recursive `agents/` and `commands/` loading with
  multi-segment `plugin:sub:name` names; name validation with `.` rejected and a leading letter
  or digit required; `__` rejected in plugin, marketplace and server names; `dependencies`,
  `userConfig`, `bin/` and `settings.json` warn; exec-form hook entries skip with a warning and
  non-`command` hook types are load errors; bare in-plugin `mcp__` references rewritten
  to the composed server name; `${CLAUDE_PLUGIN_ROOT}` and `.mcp.json` `${VAR}` expansion;
  registry round-trip, atomic write, corrupt file warns and loads nothing; matcher mapping
  through `map_tool_entry`, `|` alternation expansion, trailing-wildcard mapping, an un-mappable
  matcher warns and skips its hook entry, bare in-plugin server references in matchers rewritten
  to the composed name; two components of one plugin landing on the same name are an error
  naming the plugin; `permissions`
  / `guardian` manifest keys (deny/ask accepted, `allow` or `environment` a load error naming
  the plugin).
- Git lifecycle (local repos): marketplace add pins the HEAD sha; update moves the pin and
  reports old -> new; an entry's `ref`/`sha` is honored at install and a pinned `sha` does not
  move on update; a `git-subdir`/`npm`/`archive`/`command` source is a named error; concurrent
  adds serialize (temp-dir clone, rename under lock); a path-source install copies from the pinned marketplace checkout and
  takes the marketplace's pin; `plugin update` for a path source re-copies after `marketplace
  update` and moves the pin with it; a `github`/`url`-source update re-fetches and re-validates
  and a broken new revision keeps the old checkout; remove refuses while installed; uninstall
  deletes the checkout; a failed install leaves neither entry nor directory; install refuses a
  plugin name already installed from another marketplace; `marketplace add` refuses a duplicate
  registry name; a catalog/manifest name mismatch is an install error; a local marketplace's
  tree hash moves when plugin content changes without a catalog edit and ignores `.git` (a
  metadata-only commit moves nothing); a source that vanishes mid-update fails with the error
  chain.
- Scope enforcement (the security core, so every line is automated): a plugin skill's
  `allowed-tools` lends nothing; a plugin agent file with `model` or `permissionMode: inherit`
  is an exit-2 startup error naming the file; a plugin hook answering `allow` does not allow
  (the call still asks / reaches the guardian) while its `deny` denies; a plugin MCP tool is
  `arbitrary` and asks in `manual`; plugin deny/ask rules match their calls; plugin
  `hard_deny`/`soft_deny` appear in `effective_auto_mode` and `allow` never does; a composed
  server name colliding with a configured non-plugin server, and two installed plugins whose
  servers compose to one name, are startup errors naming the plugin; re-applying plugin
  contributions to the resolved config is a no-op (the session-switch path reuses it); a plugin
  skill's `!` commands run without workspace trust and through the permission
  pipeline; a plugin agent naming an MCP server the plugin does not ship and the session does
  not have is skipped with a warning, while one naming anything else the session lacks is an
  exit-2 startup error naming the file; a plugin agent type rejects a per-call `model` override
  (`accepts_model_override`); a plugin hook identical to a project hook keeps the plugin origin
  in `union_hooks`; a plugin MCP server needs no repository approval in `mcp_plan`.
- Session integration (fixture models, no live APIs): a session with an installed fixture plugin
  lists the namespaced skill in the system prompt, runs `/plugin:name`, registers the plugin's
  `mcp__plugin_<plugin>_<server>__*` tools, and a deleted checkout warns and contributes
  nothing.
- CLI tests drive `fontaine plugin ...` against fixture marketplaces: list output, `--format
  json`, enable/disable flipping the registry without touching the checkout.
- TUI: `/plugins` listing; `/mcp` and `/hooks` show plugin origins.
- Choke-point and descriptor tests stay green unchanged (plugin tools are dynamic `mcp__*`
  names); `tests/docs.rs` covers the new doc's examples.

## Manual Verification

Only live-network behavior is checked by hand.

1. Build a two-plugin local marketplace, `marketplace add` it, install one plugin with a skill
   and a hook; run `fontaine -p "use the <plugin>:<skill> skill"` and confirm the namespaced
   skill loads; commit a change in the marketplace, run `marketplace update` then `fontaine
   plugin update`, and confirm both pins move.
2. Add a real public Claude Code marketplace over HTTPS (for example Anthropic's official one),
   install a small plugin, and confirm the manifests load unmodified and `${CLAUDE_PLUGIN_ROOT}`
   hook commands run. This catches schema drift fixtures cannot.
