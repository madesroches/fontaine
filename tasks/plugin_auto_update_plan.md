# Plugin and Marketplace Auto-Update Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/283

## Overview

Registered marketplaces and installed plugins update themselves by default, so a checkout that has
moved upstream is pulled without a manual command. The pin stays the approval boundary: an
automatic update applies only when it leaves the set of executables the user approved at install
unchanged. A revision that adds or changes a hook command, MCP/LSP server, skill `!` command or
referenced environment variable is held back, the old checkout stays live, and the user is told to
run `fontaine plugin update <name>`, which prints the install-style summary as today.

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
  `github`/`url` sources with a `sha` stay put, with a `ref` follow the ref.
- Decision 2026-10-28 (Extensibility): "Installs, updates and marketplace changes are CLI commands
  only." This plan amends it.

## Design

### Policy

| Item | Default | Applies automatically when |
|---|---|---|
| Marketplace | on | the new catalog parses (only the pin and catalog move; no installed code changes) |
| Plugin | on | the new revision validates **and** its approval fingerprint equals the installed one |
| Plugin pinned to a `sha` | n/a | never moves (as `plugin update` today) |
| Local-directory marketplace | on | content hash changed; same rules |

Order is always marketplace first, then its plugins, so path-source plugins follow their
marketplace in one pass.

### Approval fingerprint

`plugins::approval_fingerprint(&PluginBuild) -> String`: a stable hash over a sorted canonical
listing of everything the install summary asks the user to approve for execution — hook commands
(event, matcher, command), MCP server definitions (command/args/url/env keys), LSP server
commands, skill and command `!` command lines, `${VAR}` names referenced, and the plugin's
deny/ask/guardian additions. Skill prose, agent prompts and other non-executable text are
excluded, so ordinary content changes flow through. Stored in `InstalledPlugin` as an optional
`approved = "<hash>"` written at install and at every manual `plugin update` (the user saw the
summary). A registry entry without it (installed by an older build) is never auto-updated until
one manual `plugin update` records it.

### Opt-out

- `[plugins] auto_update = true|false` in user config, default `true`. User scope only; project
  and local scope can neither set nor see it (consistent with "nothing in project scope can touch
  plugins").
- Per-entry override in the registry: `auto_update = false` on a `[[marketplace]]` or `[[plugin]]`
  entry, set by a `--no-auto-update` flag on `marketplace add` / `plugin install` and by
  `fontaine plugin auto-update <name> on|off` (a name resolves to a plugin, or to a marketplace
  with `plugin marketplace auto-update <name> on|off`).

### Trigger

At interactive session start (TUI), after plugins are loaded, spawn one detached background task
that runs the update pass; it never delays startup and never alters the running session — new
content loads at the next start. Headless runs (`-p`) never auto-update (deterministic CI/eval
runs, no surprise network). The pass is throttled by a stamp file `<data dir>/plugin-update.stamp`
(skip if checked within 24 h; the stamp is written even on failure so an offline machine does not
retry each start). The pass takes the registry lock with a non-blocking try-lock and skips when
another session or CLI holds it.

### Mechanism

Extract the bodies of `marketplace_update` and `update` in `plugin.rs` into functions returning an
outcome enum instead of printing: `MarketplaceOutcome { UpToDate, Moved{old,new}, Failed }` and
`PluginOutcome { UpToDate, Moved{old,new}, Held{reason}, Failed }`. The CLI commands print from
the outcome (output unchanged). A new module `rust/fontaine/src/plugin_auto_update.rs` drives
the pass: for each non-opted-out marketplace, update; for each non-opted-out, non-sha-pinned
plugin, materialize into a temp dir, compare fingerprints, and only then replace the checkout.
A failing marketplace or plugin leaves the old checkout live (existing guarantee) and does not
stop the rest.

### Recording

Each outcome other than `UpToDate` appends one JSON line to `<data dir>/plugin-updates.jsonl`
(owner-only, via `owner_only`): timestamp, kind, name, old/new short pin, outcome, and the hold or
failure reason. The same outcomes surface to the user once per session as a `Notice`: moved
("plugin `review` updated to d4e5f6; active next session"), held ("`review` has a new revision
that changes its hooks/servers; run `fontaine plugin update review` to review"), failed. Failure
to write the log reaches the user through `Approver::warn`. `fontaine plugin list` gains a
`held` marker for plugins with a held revision (recorded as `held_pin` in the registry so the
marker survives restarts and clears on update).

## Implementation Steps

1. `fontaine-core/src/plugins.rs`: add `approval_fingerprint`; add optional `approved`,
   `held_pin`, `auto_update` fields to `InstalledPlugin` and `auto_update` to `MarketplaceEntry`
   (serde defaults, skipped when absent); unit tests.
2. `fontaine-core` config: add the user-scope-only `[plugins] auto_update` key with the scope
   table entry; test that project scope is rejected.
3. `fontaine/src/plugin.rs`: record `approved` in `install` and `update`; refactor update paths
   to return outcomes; add `--no-auto-update` and `plugin auto-update <name> on|off`
   (`cli.rs`); show `held` in `list`.
4. `fontaine/src/plugin_auto_update.rs`: the pass, the stamp, the update log, the notices.
5. `fontaine/src/lib.rs`: spawn the pass after plugin load in the TUI path only.
6. Docs, changelog, master plan Decisions.

## Files to Modify

- `rust/fontaine-core/src/plugins.rs`, config files for the new key (`config.rs`,
  `config_scope.rs`, `config_catalog.rs`)
- `rust/fontaine/src/plugin.rs`, `cli.rs`, `lib.rs`, new `plugin_auto_update.rs`
- `rust/fontaine/tests/plugin.rs`, `rust/fontaine-core/tests/plugins.rs`
- `docs/plugins.md`, `docs/configuration.md`, `docs/tui.md` if the notice is described there,
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

- AFK: Plugin and marketplace auto-update defaults to on, as the request states, but a plugin
  revision that changes its executable approval surface is held for a manual `plugin update`.
- AFK: Headless runs never auto-update.
- AFK: Plugins installed before this change are not auto-updated until one manual update records
  their fingerprint.

## Documentation

- `docs/plugins.md`: new "Auto-update" section (policy, hold behavior, opt-out, log file); amend
  Commands (new flag/subcommand, `held` in `list`) and Pinning.
- `docs/configuration.md`: the `[plugins]` section and the per-key scope table row.
- Master plan Decisions (Extensibility): amend the 2026-10-28 "CLI commands only" line with a
  dated line for auto-update, with reasons.
- `CHANGELOG.md`.

## Testing Strategy

Unit tests (git repos created in temp dirs, no network):

- fingerprint: stable under reordering and prose-only changes; changes with a new hook command,
  server, LSP command, `!` line, or referenced env var.
- pass: marketplace moves; path-source plugin follows in the same pass; unchanged-surface plugin
  updates; changed-surface plugin is held with the old checkout live and `held_pin` recorded;
  broken new revision keeps old checkout; sha-pinned plugin untouched; opted-out entries and
  `auto_update = false` skipped; missing `approved` skipped; stamp throttling; busy lock skips;
  update log line written for each outcome.
- config: `[plugins] auto_update` rejected in project scope.
- CLI: existing `marketplace update` / `plugin update` output unchanged after the refactor;
  `plugin update` records `approved` and clears `held_pin`.
- Headless `fontaine -p` never touches the registry.

## Open Questions

- Whether `PluginBuild` exposes every executable-bearing component the summary lists; if not,
  extend it rather than narrowing the fingerprint.
