# Plugins and marketplaces

A plugin bundles skills, agent files, output styles, commands, hooks and MCP servers into one
directory with a manifest. A marketplace is a git repository (or local directory) with a catalog
that lists plugins. Fontaine reads Claude Code's manifest paths and component formats, so existing
plugins and marketplaces load unmodified.

Everything lives in the user config directory; nothing in project scope can add a marketplace,
install a plugin or enable one, because a plugin runs code. Install is the explicit action that
approves a plugin's executables: `fontaine plugin install`. Marketplaces and plugins then
[update themselves](#auto-update), but only to revisions that leave those executables unchanged.

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
and `mcpServers` merge over the default file (`hooks` and `mcpServers` take a file path or a list
of paths; an inline config object is not loaded and warns). Unknown keys are ignored except the
two Fontaine extensions below.

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
commit. Names use letters, digits, `_` and `-`, start with a letter or digit, and never contain
`__` or `.`: a plugin name becomes part of its MCP servers' composed names, which follow the
stricter MCP name alphabet.

## Registry and checkouts

One tree under the config directory:

```text
<config dir>/plugins.toml                    # the registry, written programmatically, owner-only
<config dir>/plugins/marketplaces/<name>/    # a pinned catalog checkout
<config dir>/plugins/installed/<marketplace>/<name>/  # a pinned plugin checkout
```

The registry is not a settings file: no scope stack applies to it, and only the `plugin` CLI and
the [auto-update](#auto-update) pass write it. A missing file means no marketplaces and no plugins. A corrupt file loads nothing and
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
approved = "9f2c..."    # fingerprint of the executables you approved; see Auto-update
```

`approved` is written by `install` and by every `plugin update`. Auto-update also records
`staged_pin` (a validated revision waiting for the next start) and `held_pin` (a newer revision it
refused to apply), and `fontaine plugin auto-update` / `marketplace auto-update` write
`auto_update = false` on an entry; none of these is meant to be edited by hand.

Plugin names are unique across marketplaces (component names are `plugin:name`), and marketplace
names are unique. A path-source plugin lives inside its marketplace repository and shares its pin;
`github` and `url` sources get a pin of their own.

## Commands

Installs, manual updates and marketplace changes are CLI commands, run outside any session. The
only change made from inside a running Fontaine is the staged [auto-update](#auto-update) pass.

| Command | Effect |
|---|---|
| `fontaine plugin marketplace add <url-or-path> [--name N]` | Clone (or copy a local dir), pin it, read the catalog, register it under the catalog's `name` (`--name` overrides the registry key). A catalog that does not parse keeps nothing. |
| `fontaine plugin marketplace list` | Name, url, pin, plugin count. |
| `fontaine plugin marketplace update [name]` | Fetch, move the pin to the remote default-branch HEAD, report old and new sha. Installed plugins are not touched: a path-source plugin follows on its next `plugin update` or auto-update. Discards a staged revision of the marketplace. |
| `fontaine plugin marketplace auto-update <name> yes\|no` | Opts one marketplace out of (or back into) [auto-update](#auto-update). |
| `fontaine plugin marketplace remove <name>` | Refuses while an installed plugin names it; else drops the clone and the entry. |
| `fontaine plugin install <name>@<marketplace>` | Resolve the entry in the pinned catalog, fetch or copy it, pin the commit, validate every component, record it enabled. Refuses a plugin name already installed from any marketplace. A failed install leaves nothing. |
| `fontaine plugin uninstall <name>` | Remove the checkout and the entry. |
| `fontaine plugin update <name>` | Re-fetch or re-copy, re-validate, print the approval summary and record the new approval; a broken new revision keeps the old checkout live. Clears a held or staged revision. This is how a revision that auto-update held is approved. |
| `fontaine plugin auto-update <name> yes\|no` | Opts one plugin out of (or back into) [auto-update](#auto-update). |
| `fontaine plugin enable / disable <name>` | Flip the registry flag; a disabled plugin stays on disk and contributes nothing. |
| `fontaine plugin list [--format json]` | Name, marketplace, pinned short sha, enabled, component counts; `held <sha>` marks a plugin with a newer revision auto-update refused to apply, and `auto-update off` an opt-out. The JSON adds `held_pin` and `auto_update` (plugins and marketplaces). |

Install and update print what the plugin brings: counts of skills, agents, styles, hooks, servers,
its deny/ask and guardian additions, every hook command, every MCP server definition and language
server command, the `!` command lines of its skills and commands, the environment variables it
reads (names only, never values) and every warning the load collected, so the approval is an
informed one. `${CLAUDE_PLUGIN_ROOT}` and `${VAR}` references are shown as written. The summary
lists the revision's whole executable surface, not a difference from what you approved before,
because the registry keeps only a hash of it.

## Pinning

Installs pin a commit, not a version tag; the catalog's `version` is display text. A
local-directory marketplace is copied and pinned by a content hash of its working tree (`.git`
excluded), so a content change moves the pin even when the catalog file is untouched.

A path-source plugin takes its marketplace's pin, so `plugin update` for one re-copies from the
current marketplace checkout and moves only after the marketplace moves, by `marketplace update`
or by [auto-update](#auto-update) (which moves the marketplace first and the plugin in the same
pass). A `github`/`url` source is re-fetched; an entry pinned to a `sha` follows the catalog: it
moves when the catalog names a different sha, and an entry pinned to a `ref` moves to that ref's
new HEAD.

## Auto-update

Registered marketplaces and installed plugins update themselves, on by default. The pin stays the
approval boundary: an automatic update applies only when it leaves the set of executables you
approved at install unchanged.

| Item | Applies automatically when |
|---|---|
| Marketplace | the new catalog parses. Only the pin and catalog move; no installed code changes. |
| Plugin | the new revision validates **and** its approval fingerprint equals the one recorded at install or at your last `plugin update`. |
| Plugin pinned to a `sha` | the catalog names a different sha; the same rules apply. |
| Local-directory marketplace | its content hash changed; the same rules apply. |

Disabled plugins update like enabled ones. A plugin installed by an older build has no recorded
fingerprint and is never updated automatically until one `fontaine plugin update` records it.

**What the fingerprint covers.** The hook, MCP and LSP definitions as written (`hooks/hooks.json`,
`.mcp.json`, `.lsp.json`, the manifest's `hooks`, `mcpServers` and `lspServers` values and the
files they name), the `${VAR}` names they reference, the `!` command lines of skills and commands,
and the content of every file in the checkout except the markdown under the skill, agent, style
and command directories, so a changed script, binary or other markdown file under
`${CLAUDE_PLUGIN_ROOT}` is covered too. JSON is compared as its parsed value, so reordered keys or
reformatting are not a change. Skill, agent, style and command prose is left out, so ordinary
content edits flow through.

**Held revisions.** A revision whose fingerprint differs is held: the old checkout stays live,
`held_pin` records the revision (shown as `held` in `plugin list`), and a notice, shown once in
the open session when the background pass finishes, tells you to run `fontaine plugin update <name>`, which prints the approval
summary and records the new approval. A held revision is not fetched or announced again until the
upstream moves on.

**When it runs.** At the start of an interactive session, in the background, at most once every 24
hours (`plugin-update.stamp` in the data dir; written even when a fetch failed, so an offline
machine does not retry at every start). It never delays startup and never changes the running
session: a validated revision is staged beside the live checkout (`.staged-<name>`), and the next
interactive start swaps it in before plugins load. A swap that cannot rename a checkout (a running
server on Windows) is retried at the next start. `fontaine -p` runs never fetch, stage or swap, so
scripted runs see a fixed set of plugins. Git never prompts on the terminal during the pass; a
remote that needs credentials fails and is reported.

An interrupted swap is repaired at the next start: a checkout left only as `.<name>.bak-*` is
restored, and backup, scratch and staged leftovers older than a day are removed.

**Outcomes.** Each moved, held or failed update is appended as one JSON line to
`plugin-updates.jsonl` in the data dir (timestamp, kind, name, old and new short pin, outcome and
the reason) and shown once as a [notice](tui.md#notices): `plugin review updated to d4e5f6; active
next session`, the held notice above, or the failure.

**Opting out.** `[plugins] auto_update = false` in the
[user config](configuration.md#plugins) turns it off everywhere (a project file or `--settings`
cannot set it). `fontaine plugin auto-update <name> no` and `fontaine plugin marketplace
auto-update <name> no` opt out one entry; `yes` removes the override. A manual `plugin update` or
`marketplace update` works regardless and discards any staged revision of its entry.

## What install approves

Installed plugin content is third-party, so it loads under tighten-only rules:

- its `allow` never counts, and its skills cannot lend `allowed-tools`;
- its agent files can only narrow: no model, no `permissionMode: inherit`, and only tools the
  session has (including the plugin's own MCP servers);
- its deny/ask rules and guardian deny-side additions apply; nothing else may touch permissions
  or the guardian;
- its hooks answer `deny`/`ask` but their `allow`/`retry` never counts;
- its hooks, MCP servers and skill `!` commands run without the workspace-trust gate, because
  install approved them, in the terminal UI and headless runs alike;
- an [automatic update](#auto-update) keeps that approval: it never applies a revision that adds or
  changes a hook command, server, skill `!` command, referenced variable or script.

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
  `mcp__plugin_<plugin>_<name>__<tool>` references resolve unchanged. A server name follows the
  [MCP server name alphabet](mcp.md): letters, digits, `_` and `-`, with no `__` and no leading
  or trailing `_`. A bare `mcp__<name>__*` reference in a plugin file is rewritten to the
  composed name.
- `${CLAUDE_PLUGIN_ROOT}` expands to the installed checkout and `${CLAUDE_PROJECT_DIR}` to the
  session's primary root, in hook commands and `.mcp.json` values. `${VAR}` reads any other
  variable from the process environment — its *value* lands in the hook command or server
  env/URL, so a plugin can move an environment variable (including a credential) into its own
  server. Install and update name every referenced variable in the summary they print before
  you approve; a reference to a variable that is not set stays literal and warns.
- `.lsp.json` at the plugin root and the manifest `lspServers` key (an inline map, a path, or a
  list of either; a later name replaces an earlier one, and `.lsp.json` comes first) load as
  [language servers](tools.md#language-servers) under the name `plugin:<plugin>:<server>`. Claude
  Code's fields map onto [`[lsp.servers.<name>]`](configuration.md#lspserversname); `transport =
  "socket"` runs over stdio, and `workspaceFolder` is ignored with a warning because every server
  is rooted at the workspace. A file with an unknown key is skipped whole, with a warning. Install
  names each server's command in the summary it prints, since the server runs on every edit. There
  is no `lspServers/` directory component.
- Claude Code components with no Fontaine equivalent (`bin/`, plugin `settings.json`,
  `themes/`, `monitors/`, `workflows/`, `channels`, `dependencies`, `userConfig`) warn at install
  and startup instead of silently degrading.

## Porting from Claude Code

Copy the plugin directory into a marketplace and install it. Check the component-mapping notes
above for the supported hook-matcher subset and source types; what Fontaine refuses is an error
naming the entry, and what it cannot map is skipped with a warning. See
[Extensions](extensions.md) for how the individual component files are read.

The terminal UI lists installed plugins with `/plugins`; see [the TUI](tui.md).
