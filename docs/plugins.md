# Plugins and marketplaces

A plugin bundles skills, agent files, output styles, commands, hooks and MCP servers into one
directory with a manifest. A marketplace is a git repository (or local directory) with a catalog
that lists plugins. Fontaine reads Claude Code's manifest paths and component formats, so existing
plugins and marketplaces load unmodified.

Everything lives in the user config directory; nothing in project scope can add a marketplace,
install a plugin or enable one, because a plugin runs code. Install is the explicit action that
approves a plugin's executables: `fontaine plugin install`.

## Layout and manifest

```text
my-plugin/
  .claude-plugin/plugin.json    # manifest: name (required), version, description, author, ...
  skills/<name>/SKILL.md
  agents/<name>.md
  output-styles/<name>.md
  commands/<name>.md            # loaded as user-invocable skills
  hooks/hooks.json
  .mcp.json
```

`plugin.json` also carries Claude Code's component-path keys — `commands`, `agents`, `skills`,
`hooks`, `mcpServers`, `outputStyles` — which point at files or directories inside the plugin.
`commands`, `agents` and `outputStyles` replace the default scan, `skills` adds to it, and `hooks`
and `mcpServers` merge over the default file. Unknown keys are ignored except the two Fontaine
extensions below.

A manifest may carry two Fontaine-only keys (a Claude Code loader ignores them):

```json
{
  "name": "my-plugin",
  "permissions": { "deny": ["shell(git push *)"], "ask": ["write(**/.env*)"] },
  "guardian": { "hard_deny": ["never push on Fridays"], "soft_deny": [] }
}
```

Both are deny-side only. An `allow` key in either, or a `guardian.environment` key, is a load
error naming the plugin (startup stops with exit code 2). The rules must parse as
[permission rules](permissions.md#rules); they merge into the session's deny/ask rules with a
`plugin` source and into the guardian's deny-side lists.

## Marketplaces

A marketplace is a git repository or local directory with
`.claude-plugin/marketplace.json`:

```json
{
  "name": "team-tools",
  "plugins": [
    { "name": "review", "source": "./plugins/review", "description": "...", "version": "1.2.0" },
    { "name": "other", "source": { "source": "github", "repo": "owner/repo" } }
  ]
}
```

`source` is a relative path inside the marketplace repository, or an object with `source:
"github"` (`repo`, cloned over HTTPS) or `source: "url"` (`url`, any git URL). `github` and `url`
entries may pin a `ref` (branch or tag) or `sha`, which install honors. Claude Code's other source
types are refused with an error naming the entry. `version` is informational; the pin is the
commit. Names use letters, digits, `.`, `_` and `-`, start with a letter or digit, and never
contain `__`.

## Registry and checkouts

One tree under the config directory:

```text
<config dir>/plugins.toml                    # the registry, written programmatically, owner-only
<config dir>/plugins/marketplaces/<name>/    # a pinned catalog checkout
<config dir>/plugins/installed/<marketplace>/<name>/  # a pinned plugin checkout
```

The registry is not a settings file: no scope stack applies to it, and only the `plugin` CLI
writes it. A missing file means no marketplaces and no plugins. A corrupt file loads nothing and
warns once; a broken registry never silently enables or disables anything.

```toml plugins.toml
[[marketplace]]
name = "team-tools"
url = "https://github.com/team/tools-marketplace"
pin = "a1b2c3"

[[plugin]]
name = "review"
marketplace = "team-tools"
source = "./plugins/review"
pin = "d4e5f6"
enabled = true
```

Plugin names are unique across marketplaces (component names are `plugin:name`), and marketplace
names are unique. A path-source plugin lives inside its marketplace repository and shares its pin;
`github` and `url` sources get a pin of their own.

## Commands

All mutations are CLI commands, run outside any session:

| Command | Effect |
|---|---|
| `fontaine plugin marketplace add <url-or-path> [--name N]` | Clone (or copy a local dir), pin it, read the catalog, register it under the catalog's `name` (`--name` overrides the registry key). A catalog that does not parse keeps nothing. |
| `fontaine plugin marketplace list` | Name, url, pin, plugin count. |
| `fontaine plugin marketplace update [name]` | Fetch, move the pin to the remote default-branch HEAD, report old and new sha. Installed plugins are not touched. |
| `fontaine plugin marketplace remove <name>` | Refuses while an installed plugin names it; else drops the clone and the entry. |
| `fontaine plugin install <name>@<marketplace>` | Resolve the entry in the pinned catalog, fetch or copy it, pin the commit, validate every component, record it enabled. Refuses a plugin name already installed from any marketplace. A failed install leaves nothing. |
| `fontaine plugin uninstall <name>` | Remove the checkout and the entry. |
| `fontaine plugin update <name>` | Re-fetch or re-copy, re-validate; a broken new revision keeps the old checkout live. |
| `fontaine plugin enable / disable <name>` | Flip the registry flag; a disabled plugin stays on disk and contributes nothing. |
| `fontaine plugin list [--format json]` | Name, marketplace, pinned short sha, enabled, component counts. |

Install and update print what the plugin brings before touching the registry: counts of skills,
agents, styles, hooks, servers, its deny/ask and guardian additions, and every warning the load
collected, so the approval the install represents is an informed one.

## Pinning

Installs pin a commit, not a version tag; the catalog's `version` is display text. A
local-directory marketplace is copied and pinned by a content hash of its working tree (`.git`
excluded), so a content change moves the pin even when the catalog file is untouched.

A path-source plugin takes its marketplace's pin, so `plugin update` for one re-copies from the
current marketplace checkout and moves only after `marketplace update`. A `github`/`url` source is
re-fetched; an entry pinned to a `sha` stays on it, and an entry pinned to a `ref` moves to that
ref's new HEAD.

## What install approves

Installed plugin content is third-party, so it loads under tighten-only rules:

- its `allow` never counts, and its skills cannot lend `allowed-tools`;
- its agent files can only narrow: no model, no `permissionMode: inherit`, and only tools the
  session has (including the plugin's own MCP servers);
- its deny/ask rules and guardian deny-side additions apply; nothing else may touch permissions
  or the guardian;
- its hooks answer `deny`/`ask` but their `allow`/`retry` never counts;
- its hooks, MCP servers and skill `!` commands run without the workspace-trust gate, because
  install approved them, in the terminal UI and headless runs alike.

## Namespacing

Components are namespaced `plugin:name`, as Claude Code's `/my-plugin:review`. Skill, agent and
style names get the plugin name prefixed, and `agents/` and `commands/` load recursively, each
subdirectory adding a segment (`agents/review/security.md` loads as `plugin:review:security`).
User and project names cannot contain `:`, so cross-scope collisions are impossible by
construction. Two components of one plugin that land on the same name are an error naming the
plugin.

## Component mapping

- `hooks/hooks.json` uses Claude Code's schema. Supported matchers are a single tool name
  (`Bash`), an alternation of mappable names (`Write|Edit`), and a trailing `.*` or `*`
  wildcard. A matcher outside that subset skips its hook entry with a warning. Only shell-form
  `type: "command"` hooks load; other hook types are load errors and exec-form (`args`) entries
  skip with a warning. `timeout` seconds map to `timeout_secs`.
- `.mcp.json` maps onto the MCP server settings. `env` values support `${VAR}` expansion, and
  server names are composed as `plugin_<plugin>_<name>`, so a ported plugin's
  `mcp__plugin_<plugin>_<name>__<tool>` references resolve unchanged. A bare
  `mcp__<name>__*` reference in a plugin file is rewritten to the composed name.
- `${CLAUDE_PLUGIN_ROOT}` expands to the installed checkout and `${CLAUDE_PROJECT_DIR}` to the
  session's primary root, in hook commands and `.mcp.json` values. Any other `${...}` reference
  left unexpanded warns at load.
- LSP server components are unsupported (Fontaine has no LSP integration) and are ignored with a
  warning. Claude Code components with no Fontaine equivalent (`bin/`, plugin `settings.json`,
  `themes/`, `monitors/`, `workflows/`, `channels`, `dependencies`, `userConfig`) warn at install
  and startup instead of silently degrading.

## Porting from Claude Code

Copy the plugin directory into a marketplace and install it. Check the component-mapping notes
above for the supported hook-matcher subset and source types; what Fontaine refuses is an error
naming the entry, and what it cannot map is skipped with a warning. See
[Extensions](extensions.md) for how the individual component files are read.

The terminal UI lists installed plugins with `/plugins`; see [the TUI](tui.md).
