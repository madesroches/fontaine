# Core Slash Commands Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/47

## Overview

Claude Code ships a large set of built-in commands. Fontaine has eight (`/add-dir`, `/agents`,
`/denied`, `/exit`, `/guardian-model`, `/login`, `/model`, `/output-style`). This plan adds the
subset that maps to concepts Fontaine already has and that no other backlog issue owns, using
Claude Code's names: `/help`, `/status`, `/permissions`, `/mcp`, `/hooks`, `/skills`, `/plan`,
`/copy`, and `/tasks` as an alias of `/agents`. It also replaces the two hand-kept lists, the
`parse_command` match and `builtin_menu`, with one command table, so a command can't be runnable
without appearing in the `/` menu, or appear in the menu without being runnable. Everything is
documented in `docs/tui.md`.

## Current State

- `rust/fontaine-tui/src/app.rs:62` `enum Command` and `app.rs:80` `parse_command` match command
  names by hand (`exit | quit`, `add-dir`, …, falling back to `Command::Other` for skills).
  `app.rs:109` `builtin_menu()` lists the same commands again, with a hint and a description, for
  the `/` menu (`app.rs:216`; skills are appended in `lib.rs:1408` and the menu is sorted).
- `rust/fontaine-tui/src/lib.rs:442` `submit` dispatches each `Command`. Commands run only
  between turns: while a turn runs, Enter queues text as steering, so commands never reach
  `submit` mid-turn.
- Output is written as transcript entries: `Transcript::notice` / `error`
  (`transcript.rs:73,87`). A notice is rendered with `prefixed` (`transcript.rs:290`), which
  handles multi-line text.
- The data the new commands show is already reachable from `AgentRuntime`
  (`rust/fontaine-core/src/agent.rs:239`). The session id is `runtime.session.id`, the roots are
  `runtime.roots`, the mode is `runtime.state.mode()`, and the guardian is
  `runtime.state.guardian()` (with `label()` and `unavailable_reason()`), plus
  `runtime.guardian_profile` and `runtime.output_style()`. The effective rules are
  `runtime.state.rules()` (`gate.rs:615`, a `Rules { deny, ask, allow }` whose `allow` also holds the session's grants;
  each a `SourcedRule`, and `SourcedRule::label()` gives `shell(rm) [user]`;
  `runtime.config.permissions` is only the startup snapshot), the broad allow rules auto mode sets
  aside are `runtime.state.suspended_allow_rules()` (a set of `label()` strings), and the hooks are `runtime.config.hooks` (with
  `Hook::origin`). The skills are `runtime.extensions().skills` (name, description, scope,
  `user_invocable`), and the MCP servers are `runtime.mcp_plan()`. Not exposed:
  `setup.project_executables` (`agent.rs:288`), which decides whether project hooks and servers
  loaded (`user_hooks.rs:486`, `agent.rs:793`).
- MCP start-up outcomes are reported once as notices in `lib.rs:373` `start_mcp` (connected,
  failed, or left off by the human) and are not kept.
- The permission mode changes only through Shift+Tab (`lib.rs:279` `cycle_mode`, which announces
  auto through `announce_auto`).
- There is no clipboard support. The TUI depends on `ratatui =0.30.2`, which brings in
  `crossterm 0.29.0`. crossterm has an optional `osc52` feature that provides
  `crossterm::clipboard::CopyToClipboard`.
- Integration tests drive the TUI with `ui.send("/model …")` in `rust/fontaine-tui/tests/tui.rs`
  and read the transcript back.
- Master plan Decisions already reassign `/permissions` to this issue (2026-10-21: users edit the
  config file, and grants still write to the user file). `/denied` stands in for Claude Code's
  `/permissions` "Recently denied" tab (2026-10-22).

### Commands other issues own (out of scope here)

| Claude Code command | Owner |
|---|---|
| `/clear`, `/compact`, `/context` | #11 |
| `/cost`, `/usage`, `/stats` | #56 |
| `/resume`, `/rename`, `/export`, `/branch` | #10 |
| `/init`, `/memory` | #7 |
| `/config` | #4 |
| `/doctor` | #57 |
| `/rewind` | #12 |
| `/statusline` | #48 |
| `/keybindings`, `/vim` | #49 |
| `/theme`, `/terminal-setup` | #50 |
| `/btw`, `/recap` | #52 |
| `/loop`, `/goal` | #16, #17 |

Also left out: `/diff`, `/release-notes`, `/ide`, `/bug`/`/feedback`, and `/logout` (`/login`'s
dialog already logs out). See Decisions.

## Design

### One command table

A new module `rust/fontaine-tui/src/commands.rs` owns the built-in set:

```rust
pub struct Builtin {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub hint: Option<&'static str>,
    pub description: &'static str,
    pub command: fn(String) -> Command, // builds the Command from the trimmed argument
}

pub const BUILTINS: &[Builtin] = &[ /* one row per command, sorted by name */ ];
```

- `parse_command` looks the name up in `BUILTINS`, matching the name or one of its aliases, and
  calls `command(argument)`. An unknown name still becomes `Command::Other`. Commands that take
  no argument ignore it, as they do today (`/denied foo` runs `/denied`).
- `builtin_menu()` maps `BUILTINS` to `MenuEntry` rows, so the menu and the parser can no longer
  drift. Aliases do not get their own menu rows. Unit tests check a hand-written list (`/quit` → `Exit`, `/tasks` → `Agents`, `/plan x` →
  `Plan("x")`, `/copy 2` → `Copy("2")`, `/mcp x` → `Mcp("x")`), that no name or alias is used twice, and that the menu
  has one row per entry.
- `Command`, `parse_command`, `MenuEntry` and `builtin_menu` keep their public paths
  (`lib.rs:53` re-exports them), and `app.rs` re-exports them from `commands.rs`.

New `Command` variants: `Help`, `Status`, `Permissions`, `Mcp(String)`, `Hooks`, `Skills`,
`Plan(String)`, and `Copy(String)`. `tasks` is an alias row on `Agents`.

### What each command does

All of them run between turns. Every command except `/plan` and `/copy` only writes one
transcript notice, built by a pure function that takes plain data, so each report can be unit
tested without a runtime. These functions go in a new `rust/fontaine-tui/src/reports.rs`; the
`lib.rs` handlers only gather the inputs from `AgentRuntime` and call them.

- **`/help`**: the built-in commands from `BUILTINS` (`/name hint — description`, with aliases
  shown as `/exit, /quit`), then the user-invocable skills (`runtime.slash_skills()`), then one
  line pointing at the keys (`shift+tab mode · ctrl+t agents · ! shell · @ file · esc interrupt`).
  Claude Code opens a help dialog; here it is a notice, in the same form as `/output-style`'s
  listing.
- **`/status`**: Fontaine's version (`env!("CARGO_PKG_VERSION")`) and the session id; the primary
  root and any added roots; the permission mode, named as the status bar names it
  (`statusbar::mode_label`); the main model (`runtime.models.main.reference`, `<account>/<model>`, the label the model
  picker shows as current). It also shows the guardian, as `guardian.label()` plus the profile (`Profile::as_str()`), or
  `unavailable (<reason>)`; the output style (or `none`); and how many MCP servers are connected
  (counted from `mcp_status`, since `runtime.mcp` is private), out of how many are configured.
- **`/permissions`**: a read-only view. It shows the mode, then the `deny`, `ask` and `allow`
  rules from `runtime.state.rules()`, so session and always grants appear, each as
  `SourcedRule::label()`; an empty list reads `none`. In auto mode the builder marks an allow rule
  with `(set aside in auto mode)` when its `label()` is in `suspended_allow_rules()`. Then it
  lists the workspace roots, a line naming the user settings file where rules are edited
  (`Ui.user_config: Option<PathBuf>`; the line is omitted when it is `None`), and a pointer:
  `/denied lists what the guardian denied this session`.
- **`/mcp`**: one line per server in `mcp_plan()`, plus project servers left out because the
  workspace is not trusted, with its state: `connected`, `failed` plus the outcome's error text (which
  already names the server, so the name is not repeated),  `left off` (the human
  declined it), or `not loaded (workspace not trusted)`. To know the state, `start_mcp` stops
  dropping outcomes: it records them in a `Ui` field `mcp_status: Vec<(String, McpState)>`,
  filled from the approval answers and from `connect_mcp`'s `McpOutcome`s. The command is
  read-only. Claude Code's `reconnect`/`enable`/`disable` arguments are left to #35 (MCP
  robustness: reconnect), and `/mcp` with a non-empty argument answers with an error naming that limit.
- **`/hooks`**: the hooks by event (`PreToolUse`, `PostToolUse`, `PermissionDenied`), each as
  `matcher (or "every call") → command [scope]`, the scope being `origin.slot.label()`, with `timeout Ns` when one is set. Project hooks
  are marked `not loaded (workspace not trusted)` when `project_executables` is false. This needs
  one core accessor, `AgentRuntime::project_executables(&self) -> bool`, the only change outside
  `fontaine-tui`. With no hooks it reads `no hooks configured`.
- **`/skills`**: every loaded skill (`extensions().skills`) as `name — description [scope]` (skill `Scope` has no label, so the builder matches it),
  marked `(model only)` when not user-invocable and `(you only)` when
  `disable_model_invocation` is set. Claude Code's filter, token sort and visibility toggling
  are out of scope.
- **`/plan [description]`**: sets the mode to `Plan` (`state.set_mode`, the call Shift+Tab
  makes) and adds the notice `plan mode on`. With a description, it then runs the description
  as an ordinary message (`converse`), so `/plan fix the auth bug` behaves like Claude Code. No
  mode check is needed: `/plan` is the human's own action, the same `set_mode` call Shift+Tab
  makes, which already reaches plan from any mode. The guardian and the deterministic floor apply
  exactly as they do after Shift+Tab.
- **`/copy [N]`**: copies the Nth-latest assistant reply (default 1) to the system clipboard
  with an OSC 52 escape. Replies are counted only over `Entry::Assistant` entries with non-empty
  text; a reply still streaming is never reachable, since commands run between turns. A new
  `Transcript::assistant_reply(n: usize) -> Option<&str>` finds the reply. The escape is written
  through the seam that carries the clipboard text to the terminal writer. crossterm's
  `CopyToClipboard` (the `osc52` feature) builds it, so no new crate is added. Success adds the notice
  `copied reply N (<chars> characters) to the clipboard`, plus a remark that the terminal must
  support OSC 52. A missing reply, or an `N` that is not a positive integer, is an error notice.
  Claude Code's code-block picker and its `w` (write to a file) are out of scope.
- **`/tasks`**: an alias of `/agents`. Claude Code's `/tasks` is the background-work view
  Fontaine's `/agents` panel already is.

## Implementation Steps

1. **Command table.** Create `rust/fontaine-tui/src/commands.rs` with `Builtin`, `BUILTINS`,
   `Command` (moved from `app.rs`), `parse_command`, `MenuEntry` and `builtin_menu`. Re-export
   them from `app.rs` so the existing call sites and `lib.rs:53` keep compiling. Move the
   existing `parse_command` tests along with them, then add the table-consistency tests.
2. **Report builders.** Create `rust/fontaine-tui/src/reports.rs` with pure functions `help`,
   `status`, `permissions`, `mcp`, `hooks` and `skills`. Each takes plain inputs (slices, labels
   and options, never `AgentRuntime`) and returns the notice text. Unit-test each one.
3. **Core accessor.** Add `AgentRuntime::project_executables()` in
   `rust/fontaine-core/src/agent.rs`.
4. **MCP state.** Add `McpState` and `Ui::mcp_status` in `lib.rs`, and fill them in `start_mcp`
   from the approval answers and `connect_mcp`'s outcomes. Also record the project servers that
   `mcp_plan()` leaves out when `project_executables()` is false (their names come from
   `runtime.config.mcp_servers` and `mcp_origins`).
5. **Dispatch.** In `lib.rs` `submit`, add an arm for each new variant: gather the inputs, call
   the report builder, and add the notice. `/plan` sets the mode, then calls `converse` when
   there is a description. `/copy` calls `Transcript::assistant_reply` and the OSC 52 writer.
6. **Clipboard.** Add `crossterm = { version = "=0.29.0", features = ["osc52"] }` to the workspace
   dependencies in `rust/Cargo.toml`, exact-pinned like the other workspace dependencies, and add it to `fontaine-tui/Cargo.toml`.
   Write the escape through the same writer `screen.rs` draws to. `base64` is already in the
   tree; check that `cargo deny check` stays green.
7. **Docs.** Update the docs and `CHANGELOG.md` (see Documentation).
8. **Master plan.** Add the Decisions lines below to `tasks/fontaine_harness_plan.md`
   `## Decisions`.

## Files to Modify

- `rust/fontaine-tui/src/commands.rs` (new)
- `rust/fontaine-tui/src/reports.rs` (new)
- `rust/fontaine-tui/src/app.rs`: `Command`/`parse_command`/`builtin_menu` move out; re-exports
- `rust/fontaine-tui/src/lib.rs`: dispatch arms, `mcp_status`, `/plan`, `/copy`
- `rust/fontaine-tui/src/transcript.rs`: `assistant_reply`
- `rust/fontaine-tui/src/screen.rs`: OSC 52 write, if the writer lives there
- `rust/fontaine-core/src/agent.rs`: `project_executables()`
- `rust/Cargo.toml`, `rust/fontaine-tui/Cargo.toml`: crossterm with `osc52`
- `rust/fontaine-tui/tests/tui.rs`: integration tests
- `docs/tui.md`, `docs/permissions.md`, `docs/agents.md`, `docs/mcp.md`, `docs/hooks.md`, `docs/extensions.md`,
  `docs/index.md`
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md`: Decisions

## Decisions

- 2026-10-24: `/help`, `/status`, `/permissions`, `/mcp`, `/hooks` and `/skills` are read-only
  transcript notices instead of Claude Code's interactive dialogs. Reason: one testable path, and
  rule editing stays in config files (2026-10-21 decision), and the text stays in
  scrollback.
- 2026-10-24: `/copy` uses OSC 52 only, with no code-block picker and no `w`-to-file. Reason: no
  new dependency, and it works over SSH, where `arboard` needs X11/Wayland and fails; some
  terminals ignore OSC 52, which the success notice says.
- 2026-10-24: `/diff` is not implemented. Reason: it would spawn `git`, which runs
  repo-controlled config (`diff.external`, textconv drivers); a `gix` diff is a feature of its own;
  `!git diff` covers it as the human's own action.
- 2026-10-24: `/mcp` takes no `reconnect`/`enable`/`disable` arguments until #35.
- 2026-10-24: `/release-notes` is not implemented. Reason: it would compile `CHANGELOG.md` into the
  binary or need network access, with little value before the first release.
- 2026-10-24: `/logout` is not implemented. Reason: the `/login` dialog already logs out.
- 2026-10-24: `/ide` is not implemented. Reason: IDE integration is backlog issue #59 (ACP).
- 2026-10-24: `/bug` and `/feedback` are not implemented. Reason: Claude Code sends reports to
  Anthropic; Fontaine has its GitHub issue tracker.
- 2026-10-24: `/tasks` is an alias of `/agents`. Reason: the `/agents` panel is already the
  background-work view Claude Code's `/tasks` opens.
- 2026-10-24: `/skills` has no filter, sort or visibility toggling. Reason: a read-only listing
  covers discovery; the interactive dialog is not worth a second path.

## Documentation

- `docs/tui.md` `## Commands`: add a row for each new command (`/help`, `/status`,
  `/permissions`, `/mcp`, `/hooks`, `/skills`, `/plan [<description>]`, `/copy [<n>]`) and
  `/tasks` beside `/agents`. The table is not sorted today; re-sort it the way the menu is. Under the table, add a
  sentence that `/copy` needs a terminal with OSC 52 clipboard support (with tmux,
  `set-clipboard on`).
- `docs/permissions.md`: in `## Modes`, add "or `/plan`" beside Shift+Tab as a way to enter plan mode; add a line that `/permissions` shows the effective rules with their
  source, and the auto set-aside marker.
- `docs/agents.md` (~line 130): `/tasks` beside `/agents`.
- `docs/mcp.md`: one line on `/mcp`. `docs/hooks.md`: one line on `/hooks`. `docs/extensions.md`
  `## Skills`: one line on `/skills`.
- `docs/index.md` (lines 27-29): add the new commands to the terminal UI command list.
- `CHANGELOG.md`: an `### Added` entry under `[Unreleased]`, in the Terminal UI group, citing #47,
  added in the PR step.

## Testing Strategy

Unit tests (`commands.rs`, `reports.rs`, `transcript.rs`):
- The table: a hand-written list parses to the expected variants (`/quit` → `Exit`, `/tasks` →
  `Agents`, `/plan x` → `Plan("x")`, `/copy 2` → `Copy("2")`, `/mcp x` → `Mcp("x")`); no name or alias appears twice; `builtin_menu()` has one row per `BUILTINS` entry and
  none for aliases; an unknown name is still `Command::Other`; argument trimming is kept
  (existing tests move over).
- `/help`: it lists every built-in, then the skills, then the keys line; aliases are joined.
- `/status`: an available guardian shows its label and profile; an unavailable one shows
  `unavailable (<reason>)`; extra roots are listed; `none` output style; the MCP count.
- `/permissions`: rules grouped by kind with their `[scope]`; empty lists read `none`; the
  set-aside marker appears only when the mode is auto and the rule is in the suspended set; the
  settings file path and the `/denied` pointer.
- `/mcp`: one line for each of the four states; an empty list reads `no MCP servers configured`.
- `/hooks`: grouping by event, `every call` for a missing matcher, the timeout, the untrusted
  project-hook marker, and the empty case.
- `/skills`: the scope and the `(model only)` / `(you only)` markers.
- `assistant_reply`: `n = 1` is the latest reply; `n` past the end is `None`; empty assistant
  entries and non-assistant entries are skipped.
- `/copy` argument parsing: empty is 1; `0`, `-1` and `x` are errors.

Integration tests (`fontaine-tui/tests/tui.rs`, through `ui.send` with the deterministic test
model):
- `/plan` sets the mode, and the status bar shows `⏸ plan mode on`. `/plan do x` also sends `do x`
  to the model in plan mode, so a write the model then attempts is handled by plan mode's rules,
  not the previous mode's.
- `/permissions` with a user `deny` rule and a project `ask` rule shows both with their scopes.
- `/mcp` after a declined project server shows it `left off`. This wiring through `start_mcp` is
  what a unit test of the builder cannot reach.
- `/tasks` opens the agents panel.
- `/copy` after one reply: the text handed to the seam that carries the clipboard text to the
  terminal writer equals the reply (the test backend stores cells, not bytes).
- `/help` lists the built-ins plus a user-invocable skill, exercising the `slash_skills` wiring.

## Manual Verification

1. `cargo run -p fontaine`, ask for a short reply, then `/copy`, and paste into another
   application. The reply text is pasted. Whether a real terminal honors OSC 52 is the
   terminal's behavior, which no test backend reproduces. A failure would be obvious the first
   time anyone uses it.
