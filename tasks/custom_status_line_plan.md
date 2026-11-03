# Custom Status Line Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/48

## Overview

A user-configured shell command that receives JSON session data on stdin and whose stdout
replaces the built-in TUI status bar, matching Claude Code's
[statusline](https://code.claude.com/docs/en/statusline) contract where Fontaine has the data,
plus Fontaine-specific fields (permission mode, guardian model, recent guardian verdicts). The
command is configured at **user scope only** — it executes arbitrary code, so a project must not
be able to smuggle one in. This issue also drops the `esc to interrupt · shift+tab mode` hint
from the built-in status bar's running state (user request during design).

The headless surface is out of scope: this is a TUI feature. The `/statusline` interactive
command (name reserved in `tasks/completed/core_slash_commands_plan.md`) is also out of scope;
configuration is by editing the user config file.

## Current State

- `rust/fontaine-tui/src/statusbar.rs:191-256` renders the one-row built-in bar: mode label,
  guardian-off badge, `· dir (branch) · model`, quota segments, effort/agents/scheduled/goal
  segments, and a trailing hint. The running-state hint is `esc to interrupt · shift+tab mode`
  (statusbar.rs:238); the idle hint lists slash commands (statusbar.rs:240).
- The bar is drawn from exactly one call site, `rust/fontaine-tui/src/view.rs:212`, in a row of
  fixed height 1 (view.rs:208). Nothing else depends on the row's content.
- `App` (`rust/fontaine-tui/src/app.rs:147-291`) already holds model, mode, effort, cwd, branch,
  usage (`Vec<AccountUsage>`), `guardian_off`, and running state. It has no session id, no
  guardian model name, no cost/context figures, and no retained verdicts.
- `AccountUsage::json()` (`rust/fontaine-core/src/quota.rs:76-104`) is the reserved shared
  serialization for this feature ("shared by headless output and the custom status line"); the
  usage-window plan earmarked it to embed under `account_usage` in this payload.
- Guardian verdicts arrive as `SessionEvent::Decision` and reach `App::on_event`
  (app.rs:640-642) but nothing retains them. `DecisionRecord`
  (`rust/fontaine-core/src/audit.rs:49-83`) carries `tool`, `decision`, `rule`,
  `guardian_verdict`, `guardian_model`.
- Hook execution (`rust/fontaine-core/src/user_hooks.rs:65-115`, `HookProcess::run`) is the
  established pattern for running a user command: `sh -c` via `fontaine_tools::git_bash`, stdin
  JSON, scrubbed env plus `[env]`, `FONTAINE_PROJECT_DIR`, callers enforce timeout with
  `tokio::time::timeout`.
- Config: one `Option<…Section>` field per `[section]` in `Settings`
  (config.rs:919-944); scope rules are one `Rule` per dotted key in
  `rust/fontaine-core/src/config_scope.rs:108-206`, failing closed on keys with no row, with a
  required `SNIPPETS` entry per row and schema tests tying `RULES` to the schemars schema.
- Cost: `UsageLedger` (`rust/fontaine-core/src/usage_ledger.rs:34-112`), reachable via
  `runtime.state.usage_ledger()`, holds per-source `Bucket { cost_usd, … }` and `total()`.
- Context: `ContextUsage` (`rust/fontaine-core/src/context.rs:17-59`, `total()`) is computed
  on demand by `runtime.context_usage().await`; not cached in `App`.
- Session id: `runtime.session.id` (`rust/fontaine-core/src/session.rs:175-176`); headless
  threads it through every event.
- The TUI loop `Ui::run` (`rust/fontaine-tui/src/ui/mod.rs:286-408`) is a `tokio::select!` over
  input, approvals, agent events, quota watchers (ui/mod.rs:336-353) and `soonest_tick`
  (ui/mod.rs:46-53); idle with nothing pending is `std::future::pending()` (ui/mod.rs:354-359).

## Design

### Config

A `[statusline]` section on `Settings` (config.rs):

```toml
[statusline]
command = "jq -r '.mode' # or any shell command"
refresh_interval_secs = 10 # optional
```

- `command: Option<String>` — the shell command; when absent or empty the feature is off.
- `refresh_interval_secs: Option<u64>` — optional periodic re-run while idle, mirroring Claude
  Code's `refreshInterval` (minimum 1; the timer arm joins `soonest_tick` only when set).

Scope: `Rule::user_only("statusline", …)` in config_scope.rs — project and local scopes are
refused, with a reason in the style of the existing `CHECKPOINTS`/`PLUGINS_AUTO_UPDATE` user-only
rows. No CLI flag. Add the `SNIPPETS` entry the schema tests require. Document the section in
`docs/configuration.md` with the user-only restriction called out.

### Payload

One JSON object on stdin, Claude Code field names where Fontaine has the data, Fontaine
extensions beside them:

| Field | Source |
| - | - |
| `model.id`, `model.display_name` | the main model reference string; `display_name` is the models-table label when resolvable, else the reference |
| `cwd`, `workspace.current_dir` | `App.cwd` (both set, as Claude does) |
| `workspace.project_dir` | the session's primary workspace root |
| `workspace.added_dirs` | non-primary workspace roots, `[]` when none |
| `session_id` | `runtime.session.id` |
| `version` | the `fontaine` bin crate's `CARGO_PKG_VERSION` |
| `effort.level` | `App.effort` when set; key omitted otherwise |
| `mode` | permission mode string: `manual`, `acceptEdits`, `plan`, `auto`, `dontAsk` |
| `guardian.model` | the live guardian's label, `null` when unavailable |
| `guardian.off` | `App.guardian_off` |
| `recent_verdicts` | last 5 gate decisions carrying a `guardian_verdict`: `{tool, decision, rule, guardian_verdict}`, oldest first |
| `account_usage` | `App.usage` mapped through `AccountUsage::json()` (main and guardian entries) |
| `context_window.used_percentage` | `ContextUsage::total()` as a percentage of the window, rounded; `0` before the first API response. Only `used_percentage` — Fontaine's per-layer accounting does not split into Claude's `total_input_tokens`/`current_usage` shape |
| `cost.total_cost_usd` | main-model `UsageLedger` bucket `cost_usd`; key omitted when no priced call yet |

Absent data is omitted (or `null`) rather than guessed, following `AccountUsage::json()`'s rule.
`transcript_path` is deliberately not included: Fontaine's transcript layout differs and nothing
in the bar needs it.

### Execution

New module `rust/fontaine-core/src/statusline.rs`:

- `StatuslineScript` holding the configured command, modeled on `HookProcess`: spawn
  `sh -c` via `fontaine_tools::git_bash::sh_command`, same scrubbed env plus `[env]`, cwd and
  `FONTAINE_PROJECT_DIR` set to the project dir, JSON payload written to stdin, stdout/stderr/
  exit captured. Stderr goes to the telemetry/debug log, never the UI.
- Fixed 5-second timeout (`tokio::time::timeout`; a hung script must not stall the bar). Not
  configurable in this iteration — Claude Code has no timeout knob either.
- Result policy: exit 0 with non-empty stdout → the **first line** becomes the custom line,
  with ANSI escape sequences stripped (the built-in bar is one row of plain styled spans; full
  ANSI/OSC and multi-line support is a documented follow-up, not this plan). Any other outcome
  (non-zero exit, timeout, empty output) keeps the previous good line; with none yet, the bar
  falls back to the built-in rendering. A generation counter drops stale results when a newer
  run has started.

### TUI integration

- `App` gains `statusline: Option<StatuslineState>` (last good line + generation) and a
  fixed-size (5) ring of recent verdict records, fed in `App::on_event` from
  `SessionEvent::Decision` (app.rs:640-642) — records with `guardian_verdict: None` are skipped.
  The payload builder reads the ring.
- `Ui::run` spawns one supervisor task when `[statusline]` is configured, following the
  `QuotaPoller` pattern: a trigger channel plus a result channel, with a new `select!` arm for
  results (ui/mod.rs:337-359). Triggers: once at startup; then a coalesced trigger from
  `Ui::draw` (skipped while a run is in flight — that is the debounce); plus the
  `refresh_interval_secs` timer folded into `soonest_tick` when configured. The supervisor
  builds the payload (awaiting `context_usage()`, reading the ledger, session id, guardian
  label), runs the script, and publishes `(line, generation)`; `Ui::draw` copies the result into
  `App.statusline`.
- Rendering in `statusbar::render`: when `App.statusline` holds a line, the row shows the
  guardian-off badge first when `guardian_off` is set (that warning stays visible in every
  mode — it reports a security-relevant state), then the custom line truncated to the row width.
  Otherwise the built-in rendering is unchanged, except the running-state hint is gone (below).
- Built-in bar change: remove the `esc to interrupt · shift+tab mode` hint. When running with no
  refusal, attach notice, or viewed-agent state, the hint segment is empty and the trailing
  separator is not drawn; the idle hint (`shift+tab mode · /add-dir · …`) is unchanged.

### Payload assembly cost

The script runs on events, not per frame, and the supervisor does the async work off the draw
path — `context_usage()` and the ledger read never block rendering.

## Implementation Steps

1. Config: add `StatuslineSection` to `Settings` and a value-shape check that
   `refresh_interval_secs >= 1` when set (config.rs); add `Rule::user_only("statusline", …)`
   and its `SNIPPETS` entry (config_scope.rs).
2. `fontaine-core/src/statusline.rs`: `StatuslineScript` runner (env posture copied from
   `HookProcess::run`), payload builder struct + serialization, ANSI stripping, generation
   handling. Unit tests in-module.
3. `App`: `statusline` state, verdict ring, `SessionEvent::Decision` feed (app.rs).
4. Supervisor task + select arm + `soonest_tick` entry (ui/mod.rs); wire session id, guardian
   label, ledger, `context_usage()` into the payload build.
5. `statusbar::render`: custom-line branch with guardian-off badge and width truncation; remove
   the running hint; keep the idle hint. Update the existing hint tests and add custom-line
   tests.
6. Docs: `docs/tui.md` (the feature and its JSON contract), `docs/configuration.md`
   (`[statusline]`, user-only), `CHANGELOG.md`. Flip the parity row in
   `research/research-claude-code-parity.md` (line 383) to shipped.

## Files to Modify

- `rust/fontaine-core/src/config.rs` — `StatuslineSection`, `Settings.statusline`, value check
- `rust/fontaine-core/src/config_scope.rs` — user-only rule + snippet
- `rust/fontaine-core/src/statusline.rs` — new: runner, payload, ANSI strip (with tests)
- `rust/fontaine-tui/src/app.rs` — status line state, verdict ring, Decision feed
- `rust/fontaine-tui/src/statusbar.rs` — custom-line rendering, hint removal (with tests)
- `rust/fontaine-tui/src/ui/mod.rs` — supervisor task, select arm, tick
- `docs/tui.md`, `docs/configuration.md`, `CHANGELOG.md`,
  `research/research-claude-code-parity.md`

## Trade-offs

- **First line, plain text** vs Claude's multi-line + ANSI/OSC support: the Fontaine bar is one
  row rendered as styled ratatui spans; full escape support needs an ANSI-to-span parser
  dependency. Plain first line ships the feature; escape support is a follow-up if users ask.
- **Draw-coalesced triggers** vs Claude's exact trigger list (new assistant message, mode
  change, vim toggle, …): Fontaine's draw already funnels those events; drawing while a run is
  in flight is the only coordination needed. Slightly more runs than Claude's list, bounded by
  the in-flight skip.
- **Fixed 5 s timeout** vs a config knob: matches hooks' fixed default in spirit, keeps the
  section to two keys.
- **Keeping the guardian-off badge in custom mode** vs replacing the row wholesale: the badge
  reports a security-relevant state the user must not lose by configuring a script.

## Decisions

- 2026-11-03: The built-in bar's running hint `esc to interrupt · shift+tab mode` is removed
  outright; it is not relocated elsewhere in the UI. The idle hint stays as is.
- 2026-11-03: The guardian-off warning badge renders even when a custom status line is active.
- 2026-11-03: First stdout line only, ANSI escapes stripped; multi-line and colored output are
  deferred.

## Documentation

- `docs/tui.md`: new section — the feature, the `[statusline]` config, the full JSON contract
  (Claude-compatible fields + Fontaine extensions), the fallback behavior.
- `docs/configuration.md`: the `[statusline]` section with its user-only scope restriction.
- `CHANGELOG.md` entry under Unreleased.
- `research/research-claude-code-parity.md`: row 383 flipped from `missing` to shipped.

## Testing Strategy

- Unit (`statusline.rs`): payload JSON shape — field presence/absence across states (no effort,
  no priced calls, no quota, guardian verdict ring empty/full); the runner against real local
  shell snippets — echo round-trip, first-line selection, ANSI stripping, non-zero exit keeps
  the previous line, timeout (a `sleep` past the limit) keeps the previous line, empty stdout
  falls back. These spawn `sh` locally only; no network.
- Unit (`statusbar.rs`): custom line renders with the guardian-off badge ahead of it; long lines
  truncate to the row; no custom line → built-in bar unchanged; running state shows no
  interrupt hint and no dangling separator; idle hint intact.
- Unit (config): a project-scope `[statusline]` is refused by `check_scope`; user-scope parses;
  `refresh_interval_secs = 0` is rejected by the value check.
- Unit (`app.rs`): a `SessionEvent::Decision` with a guardian verdict enters the ring; one
  without is skipped; the ring caps at 5.
- No live API, network, or terminal required at any tier. End-to-end wiring (config → task →
  bar) is covered by the manual step below because the existing TUI test harness drives widgets,
  not the `Ui::run` loop.

## Manual Verification

1. Add to the user config: `[statusline] command = "echo AFK-TEST $(pwd)"` and run
   `cargo run -p fontaine` in a repo. Expected: the bottom bar shows `AFK-TEST <dir>` instead of
   the built-in segments, and still shows `⚠ guardian off` after `/guardian-disable`.
2. While a turn runs, watch the bar: the built-in `esc to interrupt` hint never appears (with
   the config removed, the running bar shows no hint segment at all).
3. Set `command = "sleep 10"` — the bar keeps the previous line and the UI never stalls.
   Not automated because these need a real terminal and the interactive loop; breakage is
   immediately visible to anyone running the TUI.

## Open Questions

- None blocking. The `model.display_name` source (models-table label vs raw reference) is left
  to implementation: use the label when the models table resolves one, else the reference.
