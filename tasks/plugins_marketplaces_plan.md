# Plugins and Marketplaces Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/30

## Overview

Claude Code's plugins bundle skills, agents, hooks, MCP servers, LSP servers and output styles
into one directory with a manifest, installed from git-hosted marketplaces that catalog them
([plugins/overview](https://code.claude.com/docs/en/plugins/overview)). This plan adds the same
unit to Fontaine: a plugin is a directory in the extension layout (`skills/`, `agents/`,
`output-styles/`, hooks, MCP servers) plus a manifest, and a marketplace is a git repository with
a catalog file. Fontaine reads Claude Code's manifest paths and component formats so existing
plugins and marketplaces load unmodified.

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
  each carrying an `Origin { slot, user_trust }` (:484). `Origin::is_project()` gates repository
  executables on workspace trust and voids their `allow`/`retry`; `user_trust` decides whether an
  `allow` counts. `Slot` is the config column enum in the same file.
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
  `std::process::Command` where needed (`memory.rs`); there is no git library dependency.
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
  hooks/hooks.json              # Claude Code's hook schema
  .mcp.json                     # Claude Code's MCP schema
```

`plugin.json` unknown keys are ignored (as frontmatter unknown keys are), except the two
Fontaine extension keys below, which are validated. `${CLAUDE_PLUGIN_ROOT}` in hook commands and
`.mcp.json` values expands to the installed checkout path at load time, as Claude Code does.

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

`source` is a path inside the marketplace repository, or an object with `source: "github"`
(`repo: "owner/repo"`, cloned over HTTPS) or `source: "git"` (`url`). A string source that is a
URL is treated as `"git"`, as Claude Code does. `version` is informational; the pin is the
commit. Marketplace names and plugin names use `valid_name`'s alphabet.

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
commit = "a1b2c3..."                              # pinned catalog commit

[[plugin]]
name = "review"
marketplace = "team-tools"
source = "./plugins/review"
commit = "d4e5f6..."                              # pinned install commit
enabled = true
```

The file is written with `owner_only::write_atomic` under a mutex, never across an await, as
`SessionTodos` writes `todos.json`. A missing file means no marketplaces and no plugins. A
corrupt file loads nothing and warns once through `Approver::warn`, as an unparseable
`todos.json` does: a broken registry never silently enables or disables anything, it just skips
plugins for the run.

### CLI: `fontaine plugin`

New `Command::Plugin` subcommand beside `auto-mode` in `rust/fontaine/src/cli.rs`. All mutations
are CLI commands, not session commands: they run outside any session, need no permission pipeline
(the user typed them), and keep installs out of a model-reachable surface.

| Command | Effect |
|---|---|
| `fontaine plugin marketplace add <url-or-path> [--name N]` | Clone (or copy a local dir), resolve the pinned commit, read the catalog, register it. A catalog that does not parse is an error and nothing is kept. |
| `fontaine plugin marketplace list` | Name, url, pinned commit, plugin count. |
| `fontaine plugin marketplace update [name]` | Fetch, move the pin to the remote default-branch HEAD, report old -> new sha. Does not touch installed plugins. |
| `fontaine plugin marketplace remove <name>` | Refuses while an installed plugin names it; else drops clone and registry entry. |
| `fontaine plugin install <name>@<marketplace>` | Resolve the entry in the pinned catalog, clone/copy the plugin at the resolved commit, validate manifest and every component, record it enabled. Fails atomically: a failed install leaves no registry entry and no checkout. |
| `fontaine plugin uninstall <name>` | Remove checkout and registry entry. |
| `fontaine plugin update <name>` | Re-fetch the source, move the pin, re-validate; a validation failure keeps the old checkout live. |
| `fontaine plugin enable / disable <name>` | Flip the registry flag; a disabled plugin stays on disk and contributes nothing. |
| `fontaine plugin list [--format json]` | Name, marketplace, pinned short sha, enabled, component counts. |

Git operations shell out to `git` through `std::process::Command` (fixed argv, no shell), as
`memory.rs` does; a missing `git` is an error naming it. Pinning: `git clone`, then
`git rev-parse HEAD` recorded as the pin, and the checkout stays at that sha; update is
`git fetch` + checkout of the new sha + registry rewrite. A local-directory marketplace is copied
and pinned by content hash of the catalog file (no commit exists); `update` re-copies it.

Install/update print what the plugin brings before touching the registry: counts of skills,
agents, styles, hooks, servers, and its deny/ask and guardian additions, so the approval the
install represents is an informed one.

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
pub fn load_installed(config_dir: &Path) -> PluginContributions
```

- `extensions::LoadParams` gains `plugins: &[PluginDir]`; `load` walks each plugin's `skills/`,
  `agents/`, `output-styles/` after the user root and before the project root. Files load with a
  new `Scope::Plugin`.
- Config gains `Slot::Plugin`. `Origin::is_project()` is unchanged (plugin executables are not
  gated on workspace trust: install approved them, in the TUI and headless alike). A new
  `Origin::is_third_party()` (`is_project() || slot == Plugin`) is what voids a hook's
  `allow`/`retry` and keeps plugin rules out of `allow`; every current `is_project()` check that
  means "cannot loosen" moves to it. `trust_summary` is untouched: plugins are not repository
  content.
- Startup (`rust/fontaine/src/lib.rs`, both TUI and headless paths) calls `load_installed`,
  merges hooks/servers/rules into the `Config` after `config::load`, and passes the dirs into
  `extensions::load`. `servers` in `extensions::LoadParams` includes plugin server names, so a
  plugin agent file may name its own plugin's `mcp__` tools.
- A plugin whose checkout vanished (deleted by hand) contributes nothing and warns once; a plugin
  whose manifest no longer validates (after a hand edit) is a startup error naming it, as with
  extension files.

### `Scope::Plugin` semantics

Plugin prompt content is third-party even though the user installed it, so it follows
`Scope::Project`'s permission rules, not `Scope::User`'s:

- A plugin skill's `allowed-tools` is validated and ignored: a plugin cannot approve calls.
- A plugin agent file cannot pick a model, its `permissionMode` is `default` or `dontAsk`, and
  its `tools` must be tools the session has (built-ins or servers of the session, including the
  plugin's own). `inherit` is an error, as for project files.
- The executable-key refusal (`hooks`, `mcpServers` in frontmatter) applies unchanged.

Components are namespaced `plugin:name`, as Claude Code's `/my-plugin:review`: `valid_name`
gains a plugin form that allows exactly one `:` separator, and the loader prefixes skill, agent
and style names with the plugin name. The namespace makes collisions impossible by construction,
so the user > plugin > project precedence is only a tie-break for identical full names.
`/skills` shows the scope as `plugin <name>`; `TypeOrigin` gains `PluginFile(PathBuf)` beside
`UserFile`/`ProjectFile`, treated like `ProjectFile` where the registry asks whether a type can
widen (`registry.rs:140`).

### Component mapping

- **`hooks/hooks.json`**: Claude Code's schema (`{"hooks": {"PreToolUse": [{"matcher": "...",
  "hooks": [{"type": "command", "command": "...", "timeout": N}]}]}}`) maps onto `Hook` entries:
  matcher strings map through `map_tool_entry` (`Bash` -> `shell`), `timeout` seconds to
  `timeout_secs`, `${CLAUDE_PLUGIN_ROOT}` expanded. Unknown events (`SessionStart`, ...) are
  ignored; a malformed entry is a load error naming the file. Only `type: "command"` is
  supported; `"prompt"` hooks are refused.
- **`.mcp.json`**: `{"mcpServers": {"<name>": {"command", "args", "env"}}}` or `{"url"}` maps
  onto `McpServer`. `env` values support `${VAR}` expansion; to carry literal values `McpServer`
  gains an optional `env: BTreeMap<String, String>` applied to the child after the scrub, beside
  `env_vars`. Server names are prefixed with the plugin name (`<plugin>_<name>`, `_` because
  `validate_mcp_name` forbids nothing there) so two plugins cannot fight over `mcp__github__*`
  and permission rules can name a plugin's servers.
- **LSP servers**: a `lspServers` key or directory is ignored with a startup warning naming the
  plugin, so a plugin that needs one is visibly degraded rather than silently.

### TUI

One read-only command: `/plugins` lists installed plugins (name, marketplace, pinned short sha,
enabled, component counts), in the style of `/skills` and `/hooks`. Installs and marketplace
changes stay on the CLI; an interactive browse-and-install surface is a follow-up. `/mcp` and
`/hooks` show plugin servers and hooks with their origin, as they already show scope.

## Implementation Steps

1. `fontaine-core/src/plugins.rs`: registry (`plugins.toml`) read/write with
   `owner_only::write_atomic`, manifest/catalog/component JSON parsing (`serde_json` is already a
   core dependency), `PluginContributions`, `load_installed`. Unit tests.
2. `fontaine-core`: `Slot::Plugin` in `config.rs`, `Origin::is_third_party()`, move the
   cannot-loosen checks onto it (hook `allow`/`retry` voiding, rule source checks);
   `RuleSource::Plugin` in `permission_rules.rs`; `McpServer.env`; merge of plugin hooks,
   servers and deny/ask rules into `Config` (a `Config::apply_plugins(&PluginContributions)`);
   plugin `hard_deny`/`soft_deny` spliced in `effective_auto_mode`; `TypeOrigin::PluginFile`.
3. `fontaine-core/src/extensions.rs`: `Scope::Plugin`, namespaced names (`valid_name` plugin
   form, prefixing), `LoadParams.plugins`, precedence (user > plugin > project), plugin files
   under project permission rules.
4. `fontaine/src/cli.rs` + a new `fontaine/src/plugin.rs`: the `plugin` subcommand tree, git
   helpers (clone/fetch/rev-parse via `std::process::Command`), install/update/uninstall/enable/
   disable, the pre-install component summary, atomic failure semantics.
5. `fontaine/src/lib.rs`: `load_installed` + `Config::apply_plugins` on both startup paths, the
   vanished-checkout warning, `extensions::LoadParams` call-site updates (also
   `fontaine-tui/tests/common/mod.rs:338` and `rust/fontaine-core/tests/common/runtime.rs:166`).
6. TUI: `/plugins` (`Command::Plugins`, `BUILTINS` sorted position, dispatch arm, listing);
   `/mcp` and `/hooks` origin labels.
7. Docs, changelog, parity-matrix row, master-plan Decisions lines (see Decisions).
8. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
   `cargo deny check`, the Windows clippy target.

## Files to Modify

- `rust/fontaine-core/src/plugins.rs` (new), `lib.rs`, `config.rs`, `config_scope.rs` (no new
  settings keys, but the schema pin tests must not see `plugins.toml` as settings; verify),
  `permission_rules.rs`, `extensions.rs`, `rust/fontaine-agents/src/registry.rs`
- `rust/fontaine/src/cli.rs`, `plugin.rs` (new), `lib.rs`
- `rust/fontaine-tui/src/commands.rs`, the `/plugins` report beside the `/skills` one
- `rust/fontaine-mcp` or wherever `McpServer`'s child env is built (apply the new `env` map)
- Tests: `rust/fontaine-core/tests/plugins.rs` (new), `extensions.rs`, `config.rs`,
  `permission_rules.rs`, `choke_point.rs` (plugin MCP names are dynamic `mcp__*`, expected sets
  unchanged), `rust/fontaine/tests/plugin.rs` (new, CLI), `rust/fontaine-tui/tests/tui.rs`
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
- **git CLI vs. git2**: the CLI matches `memory.rs`, adds no dependency to audit, and inherits
  the user's git configuration and credentials helpers; it needs `git` on `PATH`, which plugin
  use of a git-hosted marketplace implies anyway.
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
  Plugin-to-plugin dependencies are not supported (Claude Code's manifest has none).
- 2026-10-28: `${CLAUDE_PLUGIN_ROOT}` keeps its Claude Code name for portability, expanding to
  the installed checkout.

## Documentation

- `docs/plugins.md` (new): what a plugin is, the layout and manifest (including the Fontaine-only
  `permissions`/`guardian` keys), marketplaces, every CLI command, pinning and updates,
  enable/disable, the tighten-only rule and what install approves, namespacing, LSP unsupported,
  porting notes for Claude Code plugins.
- `docs/index.md`: the one-line entry. `docs/extensions.md`: plugin scope in the roots table and
  the scope rules. `docs/mcp.md` / `docs/hooks.md`: plugin origin behavior (start without the
  workspace-trust gate; `allow`/`retry` ignored). `docs/permissions.md`: `RuleSource::Plugin`,
  deny/ask additions. `docs/guardian.md`: deny-side guardian additions from plugins.
  `docs/tui.md`: `/plugins`. `docs/headless.md`: plugins load in headless runs.
  `docs/configuration.md`: `plugins.toml` described as a registry, not a settings file.
- `docs/` TOML blocks are schema-validated by `rust/fontaine-core/tests/docs.rs`; any JSON
  examples must parse as the documented schemas.

## Testing Strategy

All offline. Git fixtures are local repositories (a path or `file://` URL as the marketplace
source needs no network); JSON fixtures stand in for remote catalogs.

- Unit: `plugin.json` / `marketplace.json` / `hooks.json` / `.mcp.json` parsing, unknown-key
  tolerance, malformed entries naming the file; name validation and the one-`:` namespaced form;
  `${CLAUDE_PLUGIN_ROOT}` and `.mcp.json` `${VAR}` expansion; registry round-trip, atomic write,
  corrupt file warns and loads nothing; matcher mapping through `map_tool_entry`; `permissions`
  / `guardian` manifest keys (deny/ask accepted, `allow` or `environment` a load error naming
  the plugin).
- Git lifecycle (local repos): marketplace add pins the HEAD sha; update moves the pin and
  reports old -> new; install at the catalog's pinned commit; update re-validates and a broken
  new revision keeps the old checkout; remove refuses while installed; uninstall deletes the
  checkout; a failed install leaves neither entry nor directory; a source that vanishes
  mid-update fails with the error chain.
- Scope enforcement (the security core, so every line is automated): a plugin skill's
  `allowed-tools` lends nothing; a plugin agent file with `model` or `permissionMode: inherit`
  is an exit-2 startup error naming the file; a plugin hook answering `allow` does not allow
  (the call still asks / reaches the guardian) while its `deny` denies; a plugin MCP tool is
  `arbitrary` and asks in `manual`; plugin deny/ask rules match their calls; plugin
  `hard_deny`/`soft_deny` appear in `effective_auto_mode` and `allow` never does.
- Session integration (fixture models, no live APIs): a session with an installed fixture plugin
  lists the namespaced skill in the system prompt, runs `/plugin:name`, registers the plugin's
  `mcp__<plugin>_<server>__*` tools, and a deleted checkout warns and contributes nothing;
  `extensions::LoadParams.plugins` precedence (user > plugin > project) on an identical full
  name.
- CLI tests drive `fontaine plugin ...` against fixture marketplaces: list output, `--format
  json`, enable/disable flipping the registry without touching the checkout.
- TUI: `/plugins` listing; `/mcp` and `/hooks` show plugin origins.
- Choke-point and descriptor tests stay green unchanged (plugin tools are dynamic `mcp__*`
  names); `tests/docs.rs` covers the new doc's examples.

## Manual Verification

Only live-network behavior is checked by hand.

1. Build a two-plugin local marketplace, `marketplace add` it, install one plugin with a skill
   and a hook; run `fontaine -p "use the <plugin>:<skill> skill"` and confirm the namespaced
   skill loads; run `fontaine plugin update` after a new commit and confirm the pin moves.
2. Add a real public Claude Code marketplace over HTTPS (for example Anthropic's official one),
   install a small plugin, and confirm the manifests load unmodified and `${CLAUDE_PLUGIN_ROOT}`
   hook commands run. This catches schema drift fixtures cannot.

## Open Questions

- Whether `fontaine plugin update` should also refresh the marketplace catalog first (one
  command) or require `marketplace update` explicitly; the plan takes the explicit two-step,
  matching the recorded pins.
