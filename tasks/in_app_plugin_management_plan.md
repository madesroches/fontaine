# In-App Plugin and Skill Management Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/554

## Overview

Let the user install, update, uninstall, enable and disable plugins from inside the TUI (`/plugin`),
and let a running session apply the result live (`/reload-plugins`): load, reload and unload of a plugin's
MCP servers, hooks, skills, agents, deny/ask rules and language servers without losing the
conversation. Skill authors get the same path for iterating on a skill directory (`/reload-plugins`, plus
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
`uninstall`, `update`, `update_plugin`, `set_enabled`, each taking the config dir and a `GitMode`
(`Interactive` | `Unattended`, mapping to `INTERACTIVE_GIT`/`UNATTENDED_GIT`). The CLI in
`fontaine/src/plugin.rs` becomes a thin printer over them, so CLI and TUI share one code path
(DRY) and the CLI's observable behavior is unchanged. Blocking git/IO is called from the TUI via
`spawn_blocking`; the TUI uses `Unattended` so git can never grab the terminal (a credential
prompt becomes an error shown in the transcript).

The move set also carries their dependencies: the `Fetch`/`EnvRef`/`GitEnv` types; the git helpers
(`git`, `require_git`, `clone_git`, `pin_clone`, `clone_url_of`, `ls_remote_*`, `head_sha`,
`same_pin`), which marketplace code staying in the bin also uses, so they become `pub` in core and
are re-exported; `copy_tree`/`replace_dir`/`remove_dir`/`read_catalog`; the registry lock helpers;
`validate_components`, `unexpanded_build`; and the `PluginSource`/`Materialized`/`PluginOutcome`/
`Review` types. `approval_of` returns its warning instead of `eprintln!`-ing it. `plugin_ops`
errors keep a `source()` chain (precedent `plugins::PluginError`, `plugins.rs:71`), rendered with
`error_chain::chain`, not `Result<_, String>`.

Split an install into two steps so the TUI can interpose consent:
`prepare_install(..) -> Prepared { summary, fingerprint, .. }` (materializes into the temp dir,
pure of registry changes) and `commit_install(Prepared)` (registry entry with `approved=`,
`replace_dir`, rollback on failure). The CLI calls both back to back; the TUI shows the summary
between them. Same split for `update`. `prepare_*` releases the registry lock; `commit_*` re-takes
it and re-validates (name still free for install; entry and pin unchanged for update), failing
otherwise. The CLI keeps printing its summary after commit.

### 2. Slash commands

- `Command::Plugin(String)` in `fontaine-tui/src/commands.rs` (`BUILTINS` table, menu and parser
  share it): `/plugin install <name>@<marketplace>`, `uninstall <name>`, `update [<name>|all]`,
  `enable <name>`, `disable <name>`. Bare `/plugin` lists plugins, like `/plugins`.
  Marketplace add/remove/update stay CLI-only (`fontaine plugin marketplace`).
- `Command::ReloadPlugins`: `/reload-plugins`, matching Claude Code.
- Both are `main_only`. Mutating forms are **not** `runs_mid_turn` (they wait for the turn to
  end); an empty-argument `/plugin` is `runs_mid_turn`, like `Skills(arg) if arg.is_empty()`.
  Background subagents outlive the root turn, so `/reload-plugins` and the mutating `/plugin`
  subcommands also refuse while subagents are live, reusing the `/clear` guard and its error.
- Standalone skills: there is no CLI skill-install today, so the in-app equivalent is `/reload-plugins`
  (picks up added, edited and removed skill files in the user, project and plugin skill
  directories) plus the existing `/skills trust`. No new skill-install surface.
- Install/update confirmation: new `plugin_prompt` modal modelled on `skill_trust_prompt`
  (`ConfirmPrompt`, `Action::PluginApprove(bool)`, wiring in `view.rs`/`app.rs` as
  `SkillTrust` has). It shows `summary_lines` (the full executable surface: hooks, MCP servers,
  LSP servers, `!` lines, referenced `${VAR}` names) and the fingerprint. Install always confirms
  in the TUI (the CLI treats the command itself as consent). Update confirms when the recorded
  `approved` is absent or differs from the new fingerprint, mirroring the auto-update hold rule.
  Uninstall/enable/disable need no modal.
- After a successful change the command runs the live apply below. `/plugin update` does
  unload → swap checkout → load for that plugin (see 4) so files never change under a running
  hook script or MCP process.

### 3. Live apply: `AgentRuntime::reload_extensions`

`reload_extensions(&mut self, approver)` (between turns only) does, in order:

1. `apply_staged` for the plugins whose staged copy is present, after stopping their MCP servers
   (the pass that today runs only at startup; fixes "auto-updated plugin silently waits"). It and
   its bin-only helpers, including the staged marketplace swap, move into core;
   `/reload-plugins` keeps the `plugins.auto_update` gate and also swaps staged marketplaces.
   Plugins to stop first are found by reading each registry entry's `staged_pin` before the call.
   Held (fingerprint-changed) stages stay held and are reported.
2. Re-run `plugins::load_installed` + `config.apply_plugins` and `extensions::load`.
3. Diff old vs new contributions per plugin (by name and definition equality) into
   `Load`/`Reload`/`Unload` sets. An unchanged plugin is untouched (its MCP processes keep
   running).
4. Apply the swaps (sections 3a–3d).
5. Compare the joined rendered system prompt (`prompt_pieces`) and the full tool definitions
   (name, description, schema) to the previous ones; call `rebuild()` only if they differ, so a
   no-op `/reload-plugins` costs no cache read. Tool names alone miss a plugin agent type (embedded
   in the `agent` tool description) or a changed MCP schema.
6. Report a summary line per change through `Approver::notice` and write a ledger/audit record.

3a. **Skills/agents/styles.** `Live<T>` is a cell following the existing `RwLock<Arc<T>>` idiom
(`fontaine_tools::lock::{read,write}`; contents feed permission decisions via `allowed-tools`, so
poison panics). Every holder shares the one cell or derives from it: for `Extensions`,
`AgentRuntime.extensions` and `SkillRuntime.extensions` (so `slash_skills()`, output-style and the
TUI `runtime.extensions()` callers see the new set); for `AgentTypes`, `AgentRuntime.agent_types`,
`SessionState.agent_types` (spawn permission check), `AgentTool.types` and the MCP supervisor
refresh closure. `reload_extensions` installs a new `Arc<Extensions>`/`AgentTypes` into the cell,
then `rebuild()` re-renders the skill index from it. Re-running `extensions::load` also re-reads
skill trust, so a trust change takes effect here.

3b. **Plugin layer for hooks and rules.** Only plugin-sourced items change at runtime.
- Deny/ask rules are already tagged `RuleSource::Plugin` in `SessionState`'s lock-guarded
  `Inner{deny,ask}`; a swap is "retain non-Plugin, push new", as `set_scope` does. They are not
  part of the layer.
- Hooks are one merged list split only by event. A `PluginLayer` held in a `Live<PluginLayer>`
  covers every hook event plugins can contribute (pre/post, `denied`, `post_failure`,
  `task_completed`/`task_created`/`teammate_idle` on Teams, `subagent_start`/`subagent_stop` via
  `tree.set_lifecycle`, and lifecycle), split by `origin.slot == Slot::Plugin`, and is consulted
  alongside the frozen user/project hooks. `LifecycleHooks` (`user_hooks.rs`) is cloned by
  `goal.rs`, so its live state sits inside its shared `LifecycleInner`.
- The floor order (`deny → ask → critical-path → protected-path`) is unchanged. User and project
  hooks stay frozen. Plugin hooks keep `user_trust:false`, so their allow/retry never counts.
  `session_start` hooks of a newly loaded plugin do not fire (that event already happened); they
  fire at the next session.
- Guardian prose (`hard_deny`/`soft_deny`) applies live: `SessionState.auto` becomes
  `RwLock<AutoContext>` with a setter mirroring `guardian: RwLock<Arc<Guardian>>`/`set_guardian`,
  and the three reads (including `adjudication.rs`'s per-request `state.auto.rules` clone) go
  through it. The guardian renders its prompt from the per-request rules and the verdict cache key
  already digests them.

3c. **MCP.** `McpServers` (`fontaine-core/src/agent/mcp_supervisor.rs`) gains `remove`, which
retires the slot and closes the connection (today `retire()` is private and the connection closes
only when the last `Arc<ServerSlot>` drops). `Connected` exposes an accessor for its in-flight
`CallTracker` (`fontaine-tools/src/mcp/elicitation.rs`) so unload can drain it. A slot's spec is
frozen, so a changed definition replaces its slot via `insert` (reuse model: `reconnect_mcp`,
`agent/mcp.rs`). Add `AgentRuntime::start_mcp_server(name)` and `stop_mcp_server(name)` over
`Supervisor::start` and `McpServers`. Plugin servers never need approval (`origin.slot == Plugin`),
so start goes straight through `mcp_plan` rules. Unload: mute the server's `list_changed` sink
(a refresh during drain would re-register the removed tool via rig's `reconcile_managed`), remove
its tools from the toolset (no new calls), drain in-flight calls for a bounded window (a named
`MCP_DRAIN_BOUND` constant, 10 s; closest existing is `KILL_GRACE`, `fontaine-tools/src/kills.rs`)
rather than cancel, since cancelling may leave a tool's side effect half done, then close the
connection; stragglers end with the existing transport error. The supervisor `refresh` closure
(private, capturing `types` once) reads the live `AgentTypes` cell. `refresh_rebuilt_toolsets`
only re-derives tool lists of revived nodes; new children already see the new set via `fork_tools`.
LSP: `LspServers` fixes its slots in `new` and exposes no per-server stop, so a changed plugin LSP
set is reported "applies next session" (never silently ignored).

3d. **Stale history.** Conversation history is never edited. When a plugin or skill is unloaded
or reloaded, the next user turn gets a reminder via `SessionState::queue_hook_context`/
`take_hook_context` listing the unloaded/changed names: "skill content or tool results from these
earlier in this conversation may be stale". Running child agents keep their spawn-time toolset
snapshot (`child_runner.rs` `fork_tools`); removed MCP tools vanish from children at their next
snapshot, and only in-flight calls get the transport error. New children see the new set.

### 4. Update and remove ordering

Unload before filesystem change, load after: `update` = stop plugin's MCP servers → drop its
layer entries → `commit_update` (`replace_dir`) → `reload_extensions`. `uninstall` = unload →
registry/checkout removal. `disable` = registry flag → unload. A failure between steps leaves the
plugin unloaded and reports it (conservative: a half-updated plugin never runs).

### 5. External changes

No file watcher and no new dependency. At the existing turn boundary (`refresh_before_turn`,
`agent.rs:427`), a cheap fingerprint (path + mtime + len of the skill directories, plugin
checkouts' manifests, and `plugins.toml`) is compared with the one taken at the last load; on a
difference it emits one notice, "skills/plugins changed on disk — run /reload-plugins", and stores
the new fingerprint so it does not repeat.

### 6. Security and audit

- The only entry is a user-typed slash command; the model gets no install/remove tool. If one is
  ever added it needs its own risk class and an ask-always rule.
- `plugins.toml` and `skill-trust.toml` stay in the protected config dir; the TUI writes them
  through `plugin_ops` (the user's action), not through a tool.
- New `Record::PluginChange { op, plugin, fingerprint }` in the ledger and audit log for every
  install/update/uninstall/enable/disable/reload, like `Record::SkillTrust` (`ledger.rs:77`). It
  reaches the audit log through a `gate.rs` `record_plugin_change` helper (as
  `SessionState::record_skill_trust` does), and the ledger fold match (`ledger.rs:631-649`) gets a
  new arm.
- Headless `-p` stays startup-only and never fetches or swaps.
- Project scope still cannot install or enable plugins.

## Implementation Steps

1. **Core ops extraction.** Create `fontaine-core/src/plugin_ops.rs` with the functions in
   Design 1; move logic out of `fontaine/src/plugin.rs`, leave the CLI as a printer; keep CLI
   tests green. Add the install/update prepare/commit split.
2. **Live cells.** Introduce `Live<T>` (in `fontaine-tools` next to `lock`), convert
   every holder listed in 3a: `AgentRuntime.extensions`/`agent_types`, `SkillRuntime.extensions`,
   `SessionState.agent_types`, `AgentTool.types`, the MCP refresh closure (`skills.rs`,
   `agent.rs`, `gate.rs`, `fontaine-agents/src/spawn.rs`, `setup.rs`, `agent/mcp.rs`).
3. **Plugin layer.** Add `PluginLayer` + `Live` for every plugin-fed hook set (`permissions.rs`,
   `PostHookStage`, `LifecycleInner` in `user_hooks.rs`, Teams and subagent lifecycle in
   `setup.rs`); build it in `setup::permission_pipeline` from `contributions`. In `gate.rs`, swap
   plugin deny/ask in place by `RuleSource::Plugin` and make `SessionState.auto` an
   `RwLock<AutoContext>` with a setter.
4. **MCP per-server API.** `start_mcp_server`/`stop_mcp_server`, `McpServers::remove`, the
   `Connected` tracker accessor, list_changed muting and bounded drain (`agent/mcp.rs`,
   `mcp_supervisor.rs`, `fontaine-tools/src/mcp/elicitation.rs`), plus the live-types refresh.
5. **`reload_extensions`.** Implement the diff/apply sequence in `agent.rs`/a new `agent/reload.rs`,
   including `apply_staged` (moved into core) for staged plugins, conditional `rebuild()`, the stale-history
   reminder, and the disk fingerprint check in `refresh_before_turn`.
6. **Records.** `Record::PluginChange` in the ledger, `gate.rs` `record_plugin_change`.
7. **TUI.** `Command::Plugin`/`Command::ReloadPlugins` (`commands.rs`, table tests),
   `ui/commands/plugin.rs`, `fontaine-tui/src/plugin_prompt.rs` (+ `pub mod` in `lib.rs`), `Action::PluginApprove`, view/app wiring, the `Action::SkillTrust` name matches in `ui/turn.rs` and `ui/providers.rs`,
   `ui/commands/mod.rs`, host wiring so `fontaine-tui` calls `plugin_ops` via core. Update the slash-menu hint text and
   `/help` output.
8. **Docs and master plan.** See Documentation. Amend the two Decisions entries.
9. **CHANGELOG.** `### Added` bullet ending `(#554)`.

## Files to Modify

- `rust/fontaine-core/src/plugin_ops.rs` (new), `plugins.rs`, `permissions.rs`, `gate.rs`,
  `agent.rs`, `agent/reload.rs` (new), `agent/mcp.rs`, `agent/setup.rs`, `skills.rs`,
  `ledger.rs`, `user_hooks.rs`, `mcp_supervisor.rs`
- `rust/fontaine-agents/src/spawn.rs`
- `rust/fontaine-tools/src/` (`Live<T>`, `mcp/elicitation.rs`)
- `rust/fontaine/src/plugin.rs`, `lib.rs`, `plugin_auto_update.rs`
- `rust/fontaine-tui/src/commands.rs`, `lib.rs`, `ui/mod.rs`, `ui/turn.rs`, `ui/providers.rs`,
  `ui/commands/mod.rs`, `ui/commands/plugin.rs` (new), `plugin_prompt.rs` (new), `app.rs`,
  `view.rs`, `tests/tui.rs`
- `docs/plugins.md`, `docs/extensions.md`, `docs/tui.md`, `docs/mcp.md`, `docs/hooks.md`,
  `docs/index.md`, `research/research-claude-code-parity.md`, `tasks/fontaine_harness_plan.md`
  (Decisions), `CHANGELOG.md`

## Trade-offs

- **`Outcome::Switch` to the same session as `/reload-plugins`** (zero plumbing) was rejected: it tears
  down every MCP server, job and LSP connection, defeating "live".
- **Make all hooks/rules swappable** was rejected for a plugin-only `PluginLayer`: user/project
  items never change at runtime, so the frozen path stays untouched and the blast radius is
  small.
- **File watcher (`notify`)** was rejected for v1: new direct dependency and `deny.toml` review,
  and auto-applying on edit costs surprise cache misses. A notice plus explicit `/reload-plugins` gives
  skill authors a fast loop; the watcher can feed `reload_extensions` later. The notice never
  auto-applies, so a stray edit cannot cost a cache read or change the toolset unexpectedly.
- **Prompt-cache cost:** a reload changes the skill index mid-prompt, invalidating the suffix and
  conversation prefix once. It is an explicit boundary like `/output-style`, and skipped entirely
  when the rendered prompt and toolset are unchanged.

## Decisions

(Entries are added as review settles questions.)

## Documentation

- `docs/plugins.md`: rewrite `## Commands` (:109) and `## Auto-update` (:149) for in-app
  management, `/reload-plugins`, and that reload applies staged updates; update
  `## What install approves` (TUI confirmation); fix stale passages at :101, :183-185, :195, :294.
- `docs/extensions.md`: skills reload, the on-disk change notice, trust takes effect on `/reload-plugins`
  (replaces the next-start note, :190).
- `docs/tui.md`: slash table rows for `/plugin`, `/reload-plugins`; fix stale passages at :85, :637, :642.
- `docs/index.md:48` and `research/research-claude-code-parity.md:342`: update the "applies next session" wording.
- `docs/mcp.md`: plugin servers start and stop live with the plugin.
- `docs/hooks.md`: live plugin hooks; no `session_start` on hot load.
- Check `docs/SUMMARY.md` and `docs/index.md` need no new page (none planned).
- Master plan `## Decisions`, `### Extensibility`: add a dated line superseding the 2026-10-28
  "CLI-only" rule and the 2026-11-03 "trust applies next session" rule, plus lines for the plugin
  layer, drain-on-unload, notice-not-watch, install-always-confirms-in-TUI, `/reload-plugins` also
  reloading standalone skills, and bare `/plugin` as a list (Claude Code's is an interactive
  manager); marketplace registration stays CLI-only per the existing Decision. Cache note at
  :907: add "plugin/skill reload" to the explicit boundaries.

## Testing Strategy

Unit tests (constructed inputs, no network, no live APIs):

- `plugin_ops`: prepare/commit install, rollback on failure, update with unchanged vs changed
  fingerprint, uninstall, enable/disable against a temp config dir and local-path marketplaces;
  the CLI tests keep passing through the printer.
- Diff logic: load/reload/unload classification, unchanged plugin untouched, MCP name collision.
- `PluginLayer`: a removed plugin's hooks stop firing for every hook event; swapped plugin
  deny/ask rules take effect at the same floor stage and leave user/project rules alone;
  user/project hooks unaffected; plugin hooks still `user_trust:false`; a guardian prose swap
  changes the next adjudication's prompt.
- `Live<T>` skills: index and `skill` tool reflect add/edit/remove; stale reminder content.
- Conditional `rebuild()`: no-op reload does not rebuild; a changed index does; a reload where
  only an agent type changes does.
- Disk fingerprint: notice emitted once per change.
- Commands table (parse, menu, `main_only`, not mid-turn), modal flow for approve/deny.
- MCP start/stop with the existing test double server: tools appear/leave the toolset; drain
  waits for an in-flight call and times out after the bound; the in-flight-call-on-closing
  behavior the research left unverified is pinned here, as is rig tolerating a managed tool
  removal.
- Choke-point test stays green (no new tool).
- `Record::PluginChange` reaches ledger and audit (modeled on `tests/guardian_off.rs`).
- TUI integration test: edit a skill file, see the notice, `/reload-plugins`, `/skills` shows the
  change (harness `fontaine-tui/tests/common/mod.rs`, `World::with_user_file`, `Ui::send`,
  precedent `tests/extensions.rs:219-228`). Update the `tests/tui.rs:827-847` assertions ("active
  next session", "run `fontaine plugin update review`") if the notice text changes.
- MCP-double stop test (`fontaine-tools/tests/mcp_common/server.rs`): after a plugin uninstall the
  server process exits and its tool is gone.
- Headless integration (`fontaine -p`): startup load unchanged.

## Manual Verification

1. Run `fontaine plugin marketplace add <dir> --name local`. In a tmux session run `fontaine`,
   `/plugin install demo@local`, confirm the modal lists the executable surface, approve, then
   invoke the plugin's skill without restarting. Not automated because it needs the real TUI modal
   and terminal rendering.
