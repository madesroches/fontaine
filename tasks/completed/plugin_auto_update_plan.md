# Plugin and Marketplace Auto-Update Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/283

## Overview

Registered marketplaces and installed plugins update themselves by default, so a checkout that has
moved upstream is pulled without a manual command. The pin stays the approval boundary: an
automatic update applies only when it leaves the set of executables the user approved at install
unchanged. A revision that adds or changes a hook command, MCP/LSP server, skill `!` command or
referenced environment variable is held back, the old checkout stays live, and the user is told to
run `fontaine plugin update <name>`, which prints the install-style summary, extended to list hook
commands, MCP server definitions and skill `!` lines (see Approval summary).

## Current State

- All mutations are CLI commands run outside any session (`rust/fontaine/src/plugin.rs`):
  `marketplace_update` (l.400) fetches the remote default-branch HEAD, validates the catalog, then
  `replace_dir`s the checkout and moves `pin`; `update` (l.697) re-materializes one plugin into a
  temp dir via `materialize`, keeps the old checkout when validation fails, prints
  `print_summary`, and writes the registry under `lock_registry`.
- The registry (`plugins.toml`, `fontaine-core/src/plugins.rs`: `Registry`, `MarketplaceEntry`,
  `InstalledPlugin`) has `deny_unknown_fields`; plugins load at startup through
  `plugins::load_installed` (`rust/fontaine/src/lib.rs` ~l.705).
- A path-source plugin takes its marketplace's pin and moves only after `marketplace update`;
  `github`/`url` sources with a `ref` follow the ref. A `sha`-pinned plugin moves when the
  catalog's sha changes: `update` re-reads the catalog entry and `pin_clone` checks out the
  catalog's sha (`plugin.rs` ~l.247-252, 718). `update` has no up-to-date check today.
- Decision 2026-10-28 (Extensibility): "Installs, updates and marketplace changes are CLI commands
  only." This plan amends it.

## Design

### Policy

| Item | Default | Applies automatically when |
|---|---|---|
| Marketplace | on | the new catalog parses (only the pin and catalog move; no installed code changes) |
| Plugin | on | the new revision validates **and** its approval fingerprint equals the installed one |
| Plugin pinned to a `sha` | on | the catalog's sha differs from the installed pin; same rules (up to date only when equal) |
| Local-directory marketplace | on | content hash changed; same rules |

Order is always marketplace first, then its plugins, so path-source plugins follow their
marketplace in one pass. Disabled plugins are auto-updated like enabled ones, matching manual
`plugin update`.

### Approval fingerprint

`plugins::approval_fingerprint(plugin_root: &Path) -> String`: a stable hash over the
unexpanded definitions read from the raw files — `hooks/hooks.json`, `.mcp.json` and `.lsp.json`
— plus the manifest's `hooks`, `mcpServers` and `lspServers` values (path lists that merge over
the default files, or an inline object) and the raw content of every file they resolve to, plus the
`${VAR}` names they reference, and the `!` command lines of skills and commands, plus the `content_hash`
(`plugins.rs` ~l.2048) of every non-markdown file in the checkout (excluding `.git` and the
markdown under the skill, agent, style and command directories), so a changed script or binary a
hook or server runs under `${CLAUDE_PLUGIN_ROOT}` is held too.
`PluginBuild` is not hashed: its values are already run through `expand_vars` (`${CLAUDE_PLUGIN_ROOT}`
is the per-run `.tmp-<uuid>` dir, and secret env values are substituted), so they would never
match between install and update. Hook entries are hashed in full (event, matcher, command); MCP
and LSP servers are hashed as their complete raw entries (every field: command, args, url, env
keys and values, initialization options, settings). The `!` lines come from running the public
`skills::parts` (`skills.rs:101`) on the raw bodies of `dir.skills` and `dir.commands` before
`$ARGUMENTS` substitution. Skill/agent frontmatter hooks/mcpServers/permissionMode are refused at
load, plugin allowed-tools are dropped, and styles are text. Skill, agent, style and command markdown is excluded, so ordinary prose changes flow through. Stored in `InstalledPlugin` as an optional `approved = "<hash>"` written at
install and at every manual `plugin update` (the user saw the summary). A registry entry without
it (installed by an older build) is never auto-updated until one manual `plugin update` records
it.

### Approval summary

`print_summary` (`plugin.rs` ~l.637-679) today prints counts, MCP server names, LSP commands and
env-var names. It is extended to list every hook command, the full MCP server definitions and the
`!` lines, plus a line stating what the approval fingerprint covers (those definitions and the
content of non-markdown files). The registry stores only the approved hash, so the summary shows
the new revision's full surface, not a diff against the approved state, and says so; the held
notice points to it.

### Opt-out

- `[plugins] auto_update = true|false` in user config, default `true`. User scope only; project
  and local scope refuse it with a Scope error (consistent with "nothing in project scope can touch
  plugins").
- Per-entry override in the registry: `auto_update = false` on a `[[marketplace]]` or `[[plugin]]`
  entry, set only by
  `fontaine plugin auto-update <name> yes|no` (a plugin) or
  `plugin marketplace auto-update <name> yes|no` (a new `MarketplaceAction::AutoUpdate` variant).

### Trigger

At interactive session start (TUI), after plugins are loaded, spawn the update pass on a plain
`std::thread` (not tokio `spawn_blocking`, whose runtime drop waits); process exit does not wait
on it. It never delays startup and never alters the running session: it only stages the validated
revision (see Staging); the swap happens at the next start. It is spawned once per process (a `/resume` or `/branch` re-enters `run_interactive`,
`lib.rs` ~l.363). Headless runs (`-p`) never auto-update (deterministic CI/eval runs, no surprise
network). The pass is throttled by a stamp file `<data dir>/plugin-update.stamp` (skip if checked
within 24 h; the stamp is written even on failure so an offline machine does not retry each start).

Locking: the pass does not hold the exclusive registry lock through git network work (`read_shared`
in `load_installed` would block other sessions' startup). It fetches and materializes outside the
lock into a temp dir with a distinct `.auto-<uuid>` prefix, which the CLI's `temp_workdir` sweep
of `.tmp-*` siblings does not touch. It then calls `file_lock::try_lock(&registry_path(config).with_extension("lock"))` (the lock file
`lock_registry` uses; `try_lock` opens the path as given) only to
re-read the registry, check the pin has not moved, and swap + write; it skips when the lock is
held. Git runs with `GIT_TERMINAL_PROMPT=0` and `GIT_SSH_COMMAND="ssh -o BatchMode=yes"` so it
never prompts on the tty. The extracted update functions take an env lookup (`EnvLookup` from
`shared_env(host)`, `lib.rs` ~l.855-865, `Send + Sync`) instead of `&Host` so the pass can run on a
`std::thread`; `git()` (`plugin.rs` ~l.78-89) gains an env parameter carrying those two variables.

Staging: the pass never touches a live checkout. Under the registry lock it moves the validated
revision to `.staged-<name>` beside the checkout (marketplaces and plugins alike; plugins
materialize from the staged marketplace when it has one) and records `staged_pin` in the registry
entry. At the next interactive start, before `plugins::load_installed`, a startup swap takes the
registry lock, then for each marketplace and then each plugin with a `staged_pin`: renames the
checkout to `.<name>.bak-<uuid>`, renames the staged dir into place, writes `pin = staged_pin`,
clears `staged_pin`, and deletes the `.bak`. No session of this process is running the checkout at
that point (another live session of the same install may be, so the swap skips an entry whose
rename fails, e.g. a running server on Windows, and retries next start). Headless runs neither
stage nor swap. A manual `plugin update` or `marketplace update` discards the entry's staged dir
and `staged_pin`. A pass skips an entry whose `staged_pin` already equals the new pin.

Interruption and leftovers: the swap order is rename-to-`.bak`, rename-staged, write pin. At the
start of each pass and each startup swap, under the lock, a missing checkout with a leftover
`.<name>.bak-*` is restored from it (an interrupted swap is then redone by the next start's
`staged_pin`/pin comparison), and `.bak-*`, `.auto-*` and `.staged-*` leftovers older than a day
(and any `.staged-*` without a matching `staged_pin`) are removed.

### Mechanism

Up-to-date is decided before materializing: a git marketplace or ref/default-branch plugin source
compares `git ls-remote` (no clone) with the installed pin; a `sha`-pinned plugin compares the
catalog's sha with no network; a local-directory marketplace compares its content hash. Only when
the pin would move does the pass clone, and a held pin (`held_pin`) equal to the remote revision is
neither re-cloned nor re-announced.

Extract the bodies of `marketplace_update` and `update` in `plugin.rs` into functions returning an
outcome enum instead of printing: `MarketplaceOutcome { UpToDate, Moved{old,new}, Failed }` and
`PluginOutcome { UpToDate, Moved{old,new}, Held{reason}, Failed }`, where `UpToDate` means the new
pin equals the installed pin (for a `sha`-pinned plugin, the catalog's sha equals it).
`materialize` and `validate_components` (which `eprintln`s today) return their warnings in the
outcome instead of printing. The CLI commands print from the outcome (summary, warnings and
output unchanged). A new module `rust/fontaine/src/plugin_auto_update.rs` drives the pass: for
each non-opted-out marketplace, update; for each non-opted-out plugin, materialize into a temp
dir, compare fingerprints, and only then replace the checkout. A failing marketplace or plugin
leaves the old checkout live (existing guarantee) and does not stop the rest.

### Recording

Each outcome other than `UpToDate` appends one JSON line to `<data dir>/plugin-updates.jsonl`
(owner-only, via `owner_only`): timestamp, kind, name, old/new short pin, outcome, and the hold or
failure reason. The same outcomes surface to the user once per session as a `Notice`, sent over a channel threaded
through `fontaine_tui::run` (`fontaine/src/lib.rs` ~l.1067 → `fontaine-tui/src/lib.rs` ~l.431) into
`drive` (~l.241/511), where its receiver is merged into drive's prompts as `Prompt::Notice` (the
`ChannelApprover` is created inside `drive`, so the pass cannot hold the approver; default `Approver::warn` is `eprintln` and would corrupt the screen): moved
("plugin `review` updated to d4e5f6; active next session"), held ("`review` has a new revision
that changes its hooks/servers; run `fontaine plugin update review` to review"), failed. Failure
to write the log reaches the user through the same notice channel. The receiver lives in the outer
loop in `fontaine/src/lib.rs` (~l.363-433) and is passed into each session's `run`, so outcomes
undelivered across a `/resume` or `/branch` reach the next session. `fontaine plugin list` gains a
`held` marker for plugins with a held revision (recorded as `held_pin` in the registry so the
marker survives restarts and clears on update); `plugin list --format json` (plugins and
marketplaces) gains `held_pin` and `auto_update`, and the text `marketplace list` gets an
auto-update marker.

## Implementation Steps

1. `fontaine-core/src/plugins.rs`: add `approval_fingerprint`; add optional `approved`,
   `held_pin`, `staged_pin`, `auto_update` fields to `InstalledPlugin` and `auto_update` and `staged_pin` to `MarketplaceEntry`
   (serde defaults, skipped when absent); unit tests.
2. `fontaine-core` config: add a `[plugins]` section struct on `Settings` (`config.rs` ~l.879-902,
   `deny_unknown_fields`), a merged `Config` field with default and merge like `checkpoints`, and the
   user-scope-only `auto_update` key: a `RULES` row (`Rule::user_only`, `config_scope.rs` ~l.62-69;
   project, local and the CLI `--settings` layer all refuse it), the `SNIPPETS` entry
   (`config_scope.rs` ~l.384), the `config_catalog.rs` row, and the `docs/configuration.md`
   key-reference block (`fontaine-core/tests/docs.rs` only validates TOML fences); extend the
   coverage tests (`config_scope.rs` ~l.349, `config_catalog.rs` ~l.572) and test that project,
   local and `--settings` are rejected.
3. `fontaine/src/plugin.rs`: record `approved` in `install` and `update`; refactor update paths
   to return outcomes (warnings returned, not printed); add `MarketplaceAction::AutoUpdate`
   and `plugin auto-update <name> yes|no` (`cli.rs`); show `held` in `list`; extend
   `print_summary`; ls-remote/catalog up-to-date checks before cloning; `staged_pin` handling in
   manual updates.
4. `fontaine/src/plugin_auto_update.rs`: the pass (staging only), the startup swap, leftover
   cleanup and `.bak` recovery, the stamp, the update log, the notices.
5. `fontaine/src/lib.rs` and `fontaine-tui/src/lib.rs`: run the startup swap before
   `plugins::load_installed` and spawn the pass once after plugin load, in the TUI path only, with a notice channel threaded through `run` into `drive`.
6. Docs, changelog, master plan Decisions.

## Files to Modify

- `rust/fontaine-core/src/plugins.rs`, config files for the new key (`config.rs`,
  `config_scope.rs`, `config_catalog.rs`)
- `rust/fontaine/src/plugin.rs`, `cli.rs`, `lib.rs`, new `plugin_auto_update.rs`,
  `rust/fontaine-tui/src/lib.rs`, `rust/fontaine-tui/tests/common/mod.rs` (`drive` callers at
  ~l.435, 472)
- `rust/fontaine/tests/plugin.rs`, `rust/fontaine-core/tests/plugins.rs`
- `docs/plugins.md`, `docs/configuration.md`, `docs/tui.md`,
  `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Silent apply of everything** (what "auto-update by default" literally asks) would change what
  third-party code runs with no review, contradicting the install-as-approval model. Holding only
  executable-surface changes keeps content updates automatic and the approval boundary intact.
- **Confirmation prompt on every update** blocks unattended use and needs the terminal; a held
  notice plus the existing `plugin update` summary reuses the current approval surface.
- **Update at startup, blocking** adds network latency to every start; a throttled background
  pass applying on the next start avoids it and avoids mutating loaded content mid-session.
- **Fingerprint as subset check** (allow strictly narrower executables) is more permissive but
  needs a semantic diff of server/hook definitions; equality is simple and sound.

## Decisions

- AFK: Plugin and marketplace auto-update defaults to on because the user instructed default-on
  (issue #283 itself asks for opt-in), but a plugin
  revision that changes its executable approval surface is held for a manual `plugin update`.
- AFK: Headless runs never auto-update.
- AFK: Plugins installed before this change are not auto-updated until one manual update records
  their fingerprint.
- AFK: The approval fingerprint also covers the content of every non-markdown file in the checkout,
  so a changed script or binary under `${CLAUDE_PLUGIN_ROOT}` is held (conservative: any code change
  needs a manual update).
- AFK: `print_summary` is extended to list hook commands, MCP definitions and `!` lines and states
  what the fingerprint covers; no diff against the approved state is shown (only the hash is stored).
- AFK: The pass never swaps a live checkout: it stages the revision (`.staged-<name>` plus
  `staged_pin`) and a startup swap under the registry lock applies it before plugins load, so
  nothing executable changes mid-session; headless runs do not swap.
- AFK: Each pass and startup swap restores a missing checkout from a leftover `.bak-*` and removes
  `.bak-*`, `.auto-*` and unrecorded `.staged-*` leftovers older than a day.
- AFK: Up-to-date is decided before cloning (`git ls-remote`, or the catalog sha with no network);
  a held pin equal to the remote is not re-cloned.
- AFK: No `--auto-update` option on `marketplace add`/`plugin install` (no `InstallArgs`, no
  `MarketplaceAddArgs` change); per-entry opt-out is only `plugin auto-update` / `marketplace
  auto-update`.

## Documentation

- `docs/plugins.md`: new "Auto-update" section (policy, hold behavior and what the fingerprint covers, staging and
  next-start swap, opt-out, log file); amend
  Commands (new subcommands, `held` in `list`) and Pinning.
- `docs/configuration.md`: the `[plugins]` section and the per-key scope table row; amend :30
  (`plugins.toml` "written by the `fontaine plugin` CLI") and the files table (:25-37) for the
  stamp and `plugin-updates.jsonl` files.
- Master plan Decisions (Extensibility): amend the 2026-10-28 "CLI commands only" line, and add
  a dated line per deviation from Claude Code, each with a one-line reason: fingerprint hold;
  trigger = interactive start + 24h stamp vs Claude Code's post-first-message randomized delay;
  global key + per-entry opt-out vs per-marketplace `autoUpdate`/env vars; default-on for all
  marketplaces vs Claude Code official-only. Headless-off cites Claude Code parity.
- Stale lines to amend: `docs/plugins.md` :104 ("All mutations are CLI commands, run outside any
  session"), :9-10, the `marketplace update` row (:110, "Installed plugins are not touched") and
  :128-131 (path sources move only after `marketplace update`); the harness plan Extensibility
  lines saying so (:447, :464); `docs/tui.md` :401 ("Installs and marketplace changes stay on the
  CLI") and "Notices", which gains the auto-update notice.
- `CHANGELOG.md`.

## Testing Strategy

Unit tests (no network): pass tests are `#[cfg(test)]` in `plugin_auto_update.rs`, using
local-directory marketplaces where possible and git-repo fixtures in temp dirs only for
remote-source cases:

- fingerprint: stable under reordering and markdown-only changes; changes with a new hook command,
  server field (incl. env value), LSP field, `!` line, referenced env var, or an edited script or
  binary under the plugin root; identical between
  install-time and update-time materialization.
- pass: marketplace moves; path-source plugin follows in the same pass; unchanged-surface plugin
  updates; changed-surface plugin is held with the old checkout live and `held_pin` recorded;
  broken new revision keeps old checkout; sha-pinned plugin moves only when the catalog's sha differs; disabled plugin updated; the pass leaves the live
  checkout untouched and the startup swap applies the staged revision; a swap interrupted after the
  `.bak` rename restores the checkout; stale `.bak-*`/`.auto-*`/`.staged-*` removed; up-to-date git
  and sha-pinned entries do not clone (ls-remote only / none) and a held pin equal to the remote is
  not re-cloned; headless neither stages nor swaps; opted-out entries and
  `auto_update = false` skipped; missing `approved` skipped; stamp throttling; busy lock skips;
  update log line written for each outcome.
- config: `[plugins] auto_update` rejected in project scope.
- CLI: existing `marketplace update` / `plugin update` output unchanged after the refactor;
  `plugin update` records `approved` and clears `held_pin` and `staged_pin`; `print_summary` lists
  hook commands, MCP definitions and `!` lines.
- Headless `fontaine -p` never touches the registry (checked with `Sandbox::run`,
  `rust/fontaine/tests/common/mod.rs:663`).
