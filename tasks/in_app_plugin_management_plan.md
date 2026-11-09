# In-App Plugin and Skill Management Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/554

## Overview

Let the user install, update, remove, enable and disable plugins from inside the TUI (`/plugin`),
and let a running session apply the result live (`/reload`): load, reload and unload of a plugin's
MCP servers, hooks, skills, agents, deny/ask rules and language servers without losing the
conversation. Skill authors get the same path for iterating on a skill directory (`/reload`, plus
a cheap on-disk change notice). Today all of this is CLI-only and takes effect on the next
session; `Extensions`, hooks and rules are built once and frozen behind `Arc`s.

## Current State

- `fontaine plugin install|remove|update|enable|disable|trust` live in the `fontaine` bin crate
  (`rust/fontaine/src/plugin.rs`: `install` :722, `uninstall` :1101, `update` :1147, `set_enabled`
  :1257). Each mutates `plugins.toml` under `file_lock::lock`, prints to stdout, and approval is
  the install itself (`approval_of` :794 → `plugins::approval_fingerprint`, `plugins.rs:2118`).
  `fontaine-tui` cannot depend on the bin crate, so none of it is callable from a slash command.
- Plugins load once in `prepare_session` (`rust/fontaine/src/lib.rs:845`, `plugins::load_installed`,
  `plugins.rs:398`) and merge via the idempotent `Config::apply_plugins` (`plugins.rs:1880`), which
  already strips earlier plugin entries first.
- Frozen after setup: `Extensions` (`extensions.rs:97`, `Arc<Extensions>` + `Arc<SkillRuntime>`,
  `setup.rs:1029`), `AgentTypes` (`setup.rs:369`), pre-hooks inside `Arc<PermissionPipeline>`
  (`permissions.rs:424`), post hooks in `PostHookStage`, lifecycle hooks in `Setup.lifecycle`,
  deny/ask in `SessionState` (`gate.rs`, only `grant()` for allow rules), guardian prose from
  `config.effective_auto_mode()`, LSP in `Setup.lsp`.
- Live-capable: the rig `ToolServerHandle` (`setup.rs:920`) takes `add_dynamic_tool`; MCP slots
  and `Supervisor::start`/`reconnect` exist (`mcp_supervisor.rs`), but only whole-plan
  `AgentRuntime::connect_mcp` (`agent/mcp.rs:406`); there is no per-server start or stop.
- `AgentRuntime::rebuild` (`agent.rs:383`) re-renders the prompt (including the skill index,
  `skills::index`, `skills.rs:349`) and re-registers tools; the session store carries the
  conversation. It costs one prompt-cache read and is already used by `/output-style` and
  `refresh_before_turn`.
- TUI: `/plugins` and `/skills` are read-only; `/skills trust` is the template for a modal
  confirmation (`ui/commands/skills.rs`, `skill_trust_prompt.rs`, `Action::SkillTrust`).
- No file watching exists and `notify` is not a direct dependency.
- Documentation and the master plan state "management is CLI-only, applies next session"
  (`docs/plugins.md:109`, `docs/extensions.md:190`; Decisions 2026-10-28 and 2026-11-03).

## Design

### 1. Plugin operations move to `fontaine-core`

New module `fontaine-core/src/plugin_ops.rs` (name may follow neighbors) holding the registry
mutations as library functions that return structured results and never print:
`materialize`, `fetch_plugin`, `would_pin`, `summary_lines`, `approval_of`, `install`,
`uninstall`, `update`, `set_enabled`, each taking the config dir and a `GitMode`
(`Interactive` | `Unattended`, mapping to `INTERACTIVE_GIT`/`UNATTENDED_GIT`). The CLI in
`fontaine/src/plugin.rs` becomes a thin printer over them, so CLI and TUI share one code path
(DRY) and the CLI's observable behavior is unchanged. Blocking git/IO is called from the TUI via
`spawn_blocking`; the TUI uses `Unattended` so git can never grab the terminal (a credential
prompt becomes an error shown in the transcript).

Split an install into two steps so the TUI can interpose consent:
`prepare_install(..) -> Prepared { summary, fingerprint, .. }` (materializes into the temp dir,
pure of registry changes) and `commit_install(Prepared)` (registry entry with `approved=`,
`replace_dir`, rollback on failure). The CLI calls both back to back; the TUI shows the summary
between them. Same split for `update`.

### 2. Slash commands

- `Command::Plugin(String)` in `fontaine-tui/src/commands.rs` (`BUILTINS` table, menu and parser
  share it): `/plugin install <name>@<marketplace>`, `remove <name>`, `update [<name>|all]`,
  `enable <name>`, `disable <name>`, `reload`. Bare `/plugin` is an alias of `/plugins` (list).
- `Command::Reload`: `/reload`, equivalent to `/plugin reload`.
- Both are `main_only` and **not** `runs_mid_turn` (a mutating command waits for the turn to
  end), so swaps never race the running agent loop on the root agent.
- Standalone skills: there is no CLI skill-install today, so the in-app equivalent is `/reload`
  (picks up added, edited and removed skill files in the user, project and plugin skill
  directories) plus the existing `/skills trust`. No new skill-install surface.
- Install/update confirmation: new `plugin_prompt` modal modelled on `skill_trust_prompt`
  (`ConfirmPrompt`, `Action::PluginApprove(bool)`, wiring in `view.rs`/`app.rs` as
  `SkillTrust` has). It shows `summary_lines` (the full executable surface: hooks, MCP servers,
  LSP servers, `!` lines, referenced `${VAR}` names) and the fingerprint. Install always confirms
  in the TUI (the CLI treats the command itself as consent). Update confirms only when the new
  fingerprint differs from the recorded `approved` one, mirroring the auto-update hold rule.
  Remove/enable/disable need no modal.
- After a successful change the command runs the live apply below. `/plugin update` does
  unload → swap checkout → load for that plugin (see 4) so files never change under a running
  hook script or MCP process.

### 3. Live apply: `AgentRuntime::reload_extensions`

`reload_extensions(&mut self, approver)` (between turns only) does, in order:

1. `plugin_auto_update::apply_staged` for the plugins whose staged copy is present, after
   stopping their MCP servers (the pass that today runs only at startup; fixes "auto-updated
   plugin silently waits"). Held (fingerprint-changed) stages stay held and are reported.
2. Re-run `plugins::load_installed` + `config.apply_plugins` and `extensions::load`.
3. Diff old vs new contributions per plugin (by name and definition equality) into
   `Load`/`Reload`/`Unload` sets. An unchanged plugin is untouched (its MCP processes keep
   running).
4. Apply the swaps (sections 3a–3d).
5. Compare the rendered system prompt + registered tool names to the previous ones; call
   `rebuild()` only if they differ, so a no-op `/reload` costs no cache read.
6. Report a summary line per change through `Approver::notice` and write a ledger/audit record.

3a. **Skills/agents/styles.** `SkillRuntime.extensions` (`skills.rs:158`) and the `AgentTypes`
holder become a `Live<T>` cell (`RwLock<Arc<T>>`, `fontaine_tools::lock::{read,write}`; contents
feed permission decisions via `allowed-tools`, so poison panics). `reload_extensions` installs a
new `Arc<Extensions>`/`AgentTypes` into the cells, then `rebuild()` re-renders the skill index
from the live cell. Slash-menu rows (`slash_skills()`) read the same cell.

3b. **Plugin layer for hooks and rules.** Only plugin-sourced items change at runtime, so
introduce one `PluginLayer { pre_hooks, post_hooks, lifecycle, deny, ask }` held in a
`Live<PluginLayer>` and consulted by `PermissionPipeline`, `PostHookStage`, `LifecycleHooks` and
`SessionState` alongside their existing frozen user/project items. The floor order
(`deny → ask → critical-path → protected-path`) is unchanged: plugin deny/ask are read from the
live layer at the same stage they are read from today. User and project hooks stay frozen. Plugin
hooks keep `user_trust:false`, so their allow/retry never counts. `session_start` hooks of a
newly loaded plugin do not fire (that event already happened); they fire at the next session.
Guardian prose (`hard_deny`/`soft_deny`): applied live only if the guardian already supports
swapping its rules; otherwise a changed set is reported as "applies next session" (see Open
Questions) — never silently ignored.

3c. **MCP.** Add `AgentRuntime::start_mcp_server(name)` and `stop_mcp_server(name)` over
`Supervisor::start` and slot retirement (`McpServers`), then call the supervisor `refresh`
(`refresh_rebuilt_toolsets`) so idle child agent types see the new set. Plugin servers never need
approval (`origin.slot == Plugin`), so start goes straight through `mcp_plan` rules. Unload
semantics for in-flight calls: the server's tools leave the toolset immediately (no new calls),
in-flight calls are **drained** for a bounded window (default 10 s, reusing the tracker in
`register.rs:138`), then the connection is closed and stragglers end with the existing transport
error. LSP servers follow the same stop/start only if a per-server API exists; otherwise they
apply next session and the summary says so (same non-silent rule).

3d. **Stale history.** Conversation history is never edited. When a plugin or skill is unloaded
or reloaded, the next user turn is prefixed with a reminder (via the existing turn-prefix
reminder mechanism) listing the unloaded/changed names: "skill content or tool results from these
earlier in this conversation may be stale". Running child agents keep their spawn-time toolset
snapshot (`child_runner.rs` `fork_tools`); calls to a removed MCP tool from a child fail with the
transport error. New children see the new set.

### 4. Update and remove ordering

Unload before filesystem change, load after: `update` = stop plugin's MCP servers → drop its
layer entries → `commit_update` (`replace_dir`) → `reload_extensions`. `remove` = unload →
registry/checkout removal. `disable` = registry flag → unload. A failure between steps leaves the
plugin unloaded and reports it (conservative: a half-updated plugin never runs).

### 5. External changes

No file watcher and no new dependency. At the existing turn boundary (`refresh_before_turn`,
`agent.rs:427`), a cheap fingerprint (path + mtime + len of the skill directories, plugin
checkouts' manifests, and `plugins.toml`) is compared with the one taken at the last load; on a
difference it emits one notice, "skills/plugins changed on disk — run /reload", and stores the
new fingerprint so it does not repeat. It never auto-applies, so a stray edit cannot cost a cache
read or change the toolset unexpectedly. A real watcher can be added later behind the same
`reload_extensions` entry point.

### 6. Security and audit

- The only entry is a user-typed slash command; the model gets no install/remove tool. If one is
  ever added it needs its own risk class and an ask-always rule.
- `plugins.toml` and `skill-trust.toml` stay in the protected config dir; the TUI writes them
  through `plugin_ops` (the user's action), not through a tool.
- New `Record::PluginChange { op, plugin, fingerprint }` in the ledger and audit log for every
  install/update/remove/enable/disable/reload, like `Record::SkillTrust` (`ledger.rs:77`).
- Headless `-p` stays startup-only; it calls the same `load_installed` path, now sharing
  `reload_extensions`' diff/apply code only where reasonable. `-p` still never fetches or swaps.
- Project scope still cannot install or enable plugins.

## Implementation Steps

1. **Core ops extraction.** Create `fontaine-core/src/plugin_ops.rs` with the functions in
   Design 1; move logic out of `fontaine/src/plugin.rs`, leave the CLI as a printer; keep CLI
   tests green. Add the install/update prepare/commit split.
2. **Live cells.** Introduce `Live<T>` (in `fontaine-tools` next to `lock`), convert
   `SkillRuntime.extensions` and the `AgentTypes` holder (`skills.rs`, `setup.rs:369,1029`).
3. **Plugin layer.** Add `PluginLayer` + `Live` to `PermissionPipeline` (`permissions.rs`),
   `PostHookStage`, `LifecycleHooks` (`agent.rs`), `SessionState` deny/ask (`gate.rs`); build it
   in `setup::permission_pipeline` from `contributions`. Verify guardian-prose swap support; wire
   it or the "next session" notice.
4. **MCP per-server API.** `start_mcp_server`/`stop_mcp_server` with bounded drain
   (`agent/mcp.rs`, `mcp_supervisor.rs`, `fontaine-tools/src/mcp/servers.rs`), plus refresh.
5. **`reload_extensions`.** Implement the diff/apply sequence in `agent.rs`/a new `agent/reload.rs`,
   including `apply_staged` for staged plugins, conditional `rebuild()`, the stale-history
   reminder, and the disk fingerprint check in `refresh_before_turn`.
6. **Records.** `Record::PluginChange` in ledger and audit.
7. **TUI.** `Command::Plugin`/`Command::Reload` (`commands.rs`, table tests),
   `ui/commands/plugin.rs`, `plugin_prompt.rs`, `Action::PluginApprove`, view/app wiring, host
   wiring so `fontaine-tui` calls `plugin_ops` via core. Update the slash-menu hint text and
   `/help` output.
8. **Docs and master plan.** See Documentation. Amend the two Decisions entries.
9. **CHANGELOG.** `### Added` bullet ending `(#554)`.

## Files to Modify

- `rust/fontaine-core/src/plugin_ops.rs` (new), `plugins.rs`, `permissions.rs`, `gate.rs`,
  `agent.rs`, `agent/reload.rs` (new), `agent/mcp.rs`, `agent/setup.rs`, `skills.rs`,
  `ledger.rs`, audit module, `mcp_supervisor.rs`
- `rust/fontaine-tools/src/` (`Live<T>`, `mcp/servers.rs`)
- `rust/fontaine/src/plugin.rs`, `lib.rs`, `plugin_auto_update.rs`
- `rust/fontaine-tui/src/commands.rs`, `ui/mod.rs`, `ui/commands/plugin.rs` (new),
  `plugin_prompt.rs` (new), `app.rs`, `view.rs`
- `docs/plugins.md`, `docs/extensions.md`, `docs/tui.md`, `docs/mcp.md`, `docs/hooks.md`,
  `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **`Outcome::Switch` to the same session as `/reload`** (zero plumbing) was rejected: it tears
  down every MCP server, job and LSP connection, defeating "live".
- **Make all hooks/rules swappable** was rejected for a plugin-only `PluginLayer`: user/project
  items never change at runtime, so the frozen path stays untouched and the blast radius is
  small.
- **File watcher (`notify`)** was rejected for v1: new direct dependency and `deny.toml` review,
  and auto-applying on edit costs surprise cache misses. A notice plus explicit `/reload` gives
  skill authors a fast loop; the watcher can feed `reload_extensions` later.
- **Prompt-cache cost:** a reload changes the skill index mid-prompt, invalidating the suffix and
  conversation prefix once. It is an explicit boundary like `/output-style`, and skipped entirely
  when the rendered prompt and toolset are unchanged.
- **Drain vs cancel for in-flight MCP calls:** drain with a bound, since cancelling may leave a
  tool's side effect half done; the bound keeps a hung call from blocking unload.

## Decisions

(Entries are added as review settles questions.)

## Documentation

- `docs/plugins.md`: rewrite `## Commands` (:109) and `## Auto-update` (:149) for in-app
  management, `/reload`, and that reload applies staged updates; update `## What install approves`
  (TUI confirmation).
- `docs/extensions.md`: skills reload, the on-disk change notice, trust takes effect on `/reload`
  (replaces the next-start note, :190).
- `docs/tui.md`: slash table rows for `/plugin`, `/reload`.
- `docs/mcp.md`: plugin servers start and stop live with the plugin.
- `docs/hooks.md`: live plugin hooks; no `session_start` on hot load.
- Check `docs/SUMMARY.md` and `docs/index.md` need no new page (none planned).
- Master plan `## Decisions`, `### Extensibility`: add a dated line superseding the 2026-10-28
  "CLI-only" rule and the 2026-11-03 "trust applies next session" rule, plus lines for the plugin
  layer, drain-on-unload, notice-not-watch, and install-always-confirms-in-TUI. Cache note at
  :907: add "plugin/skill reload" to the explicit boundaries.

## Testing Strategy

Unit tests (constructed inputs, no network, no live APIs):

- `plugin_ops`: prepare/commit install, rollback on failure, update with unchanged vs changed
  fingerprint, uninstall, enable/disable against a temp config dir and local-path marketplaces;
  the CLI tests keep passing through the printer.
- Diff logic: load/reload/unload classification, unchanged plugin untouched, MCP name collision.
- `PluginLayer`: a removed plugin's hooks stop firing; added deny/ask rules take effect at the
  same floor stage; user/project items unaffected; plugin hooks still `user_trust:false`.
- `Live<T>` skills: index and `skill` tool reflect add/edit/remove; stale reminder content.
- Conditional `rebuild()`: no-op reload does not rebuild; a changed index does.
- Disk fingerprint: notice emitted once per change.
- Commands table (parse, menu, `main_only`, not mid-turn), modal flow for approve/deny.
- MCP start/stop with the existing test double server: tools appear/leave the toolset; drain
  waits for an in-flight call and times out after the bound; the in-flight-call-on-closing
  behavior the research left unverified is pinned here, as is rig tolerating a managed tool
  removal.
- Choke-point test stays green (no new tool; a new `Record` variant is covered).
- Headless integration (`fontaine -p`): startup load unchanged.

## Manual Verification

1. In a tmux session run `fontaine`, `/plugin install demo@local`, confirm the modal lists the
   executable surface, approve, then invoke the plugin's skill without restarting. Not automated
   because it needs the real TUI modal and terminal rendering.
2. Edit a skill file in `.fontaine/skills/`, see the change notice, run `/reload`, confirm the new
   description appears in `/skills`.
3. `/plugin remove demo`, confirm its MCP process exited (`ps`) and its tool is gone.

## Open Questions

- Does the guardian support swapping `hard_deny`/`soft_deny` at runtime? If not, the plan ships
  "applies next session" with an explicit notice rather than adding guardian plumbing.
- Is there a per-server LSP stop/start primitive? Same fallback if not.
