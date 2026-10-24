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
  (`transcript.rs:73,87`). A notice is rendered with `prefixed` (`transcript.rs:385`), which
  handles multi-line text.
- The data the new commands show is already reachable from `AgentRuntime`
  (`rust/fontaine-core/src/agent.rs:239`). The session id is `runtime.session.id`, the roots are
  `runtime.roots`, the mode is `runtime.state.mode()`, and the guardian is
  `runtime.state.guardian()` (with `label()` and `unavailable_reason()`), plus
  `runtime.guardian_profile` and `runtime.output_style()`. The rules are
  `runtime.config.permissions.{deny,ask,allow}` (each a `SourcedRule`, and `SourcedRule::label()`
  gives `shell(rm) [user]`), the broad allow rules auto mode sets aside are
  `runtime.state.suspended_allow_rules()`, and the hooks are `runtime.config.hooks` (with
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
dialog already logs out). See Trade-offs.

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
  drift. Aliases do not get their own menu rows. A unit test checks that every row parses to its
  own variant, and that no name or alias is used twice.
- `Command`, `parse_command`, `MenuEntry` and `builtin_menu` keep their public paths
  (`lib.rs:53` re-exports them), and `app.rs` re-exports them from `commands.rs`.

New `Command` variants: `Help`, `Status`, `Permissions`, `Mcp`, `Hooks`, `Skills`,
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
  (`statusbar::mode_label`); the main model (`<account>/<model>`, the label the model picker
  shows as current). It also shows the guardian, as `guardian.label()` plus the profile, or
  `unavailable (<reason>)`; the output style (or `none`); and how many MCP servers are connected,
  out of how many are configured.
- **`/permissions`**: a read-only view. It shows the mode, then the `deny`, `ask` and `allow`
  rules, each as `SourcedRule::label()`; an empty list reads `none`. In auto mode it marks the
  allow rules auto sets aside (`suspended_allow_rules()`) with `(set aside in auto mode)`. Then it
  lists the workspace roots, a line naming the user settings file where rules are edited
  (the path `user_config` resolves), and a pointer: `/denied lists what the guardian denied this
  session`. Claude Code opens an editor here; Fontaine's rules stay edited in the config files,
  per the existing 2026-10-21 decision.
- **`/mcp`**: one line per server in `mcp_plan()`, plus project servers left out because the
  workspace is not trusted, with its state: `connected`, `failed: <error>`, `left off` (the human
  declined it), or `not loaded (workspace not trusted)`. To know the state, `start_mcp` stops
  dropping outcomes: it records them in a `Ui` field `mcp_status: Vec<(String, McpState)>`,
  filled from the approval answers and from `connect_mcp`'s `McpOutcome`s. The command is
  read-only. Claude Code's `reconnect`/`enable`/`disable` arguments are left to #35 (MCP
  robustness: reconnect), and `/mcp` with an argument answers with an error naming that limit.
- **`/hooks`**: the hooks by event (`PreToolUse`, `PostToolUse`, `PermissionDenied`), each as
  `matcher (or "every call") → command [scope]`, with `timeout Ns` when one is set. Project hooks
  are marked `not loaded (workspace not trusted)` when `project_executables` is false. This needs
  one core accessor, `AgentRuntime::project_executables(&self) -> bool`, the only change outside
  `fontaine-tui`. With no hooks it reads `no hooks configured`.
- **`/skills`**: every loaded skill (`extensions().skills`) as `name — description [scope]`,
  marked `(model only)` when not user-invocable and `(you only)` when
  `disable_model_invocation` is set. Claude Code's filter, token sort and visibility toggling
  are out of scope.
- **`/plan [description]`**: sets the mode to `Plan` (`state.set_mode`, the call Shift+Tab
  makes) and adds the notice `plan mode on`. With a description, it then runs the description
  as an ordinary message (`converse`), so `/plan fix the auth bug` behaves like Claude Code. Plan
  is never looser than the mode it replaces, so this needs no mode check. The guardian and the
  deterministic floor apply exactly as they do after Shift+Tab.
- **`/copy [N]`**: copies the Nth-latest assistant reply (default 1) to the system clipboard
  with an OSC 52 escape. Replies are counted only over `Entry::Assistant` entries with non-empty
  text; a reply still streaming is never reachable, since commands run between turns. A new
  `Transcript::assistant_reply(n: usize) -> Option<&str>` finds the reply. The escape is written
  through the terminal backend the TUI already draws with. crossterm's `CopyToClipboard` (the
  `osc52` feature) builds it, so no new crate is added. Success adds the notice
  `copied reply N (<chars> characters) to the clipboard`, plus a remark that the terminal must
  support OSC 52. A missing reply, or an `N` that is not a positive integer, is an error notice.
  Claude Code's code-block picker and its `w` (write to a file) are out of scope.
- **`/tasks`**: an alias of `/agents`. Claude Code's `/tasks` is the background-work view
  Fontaine's `/agents` panel already is.

### Scope and safety

None of these commands change a permission decision. They read state or, for `/plan`, move the
mode to the most restrictive one in the cycle. `/permissions` displays rules and never edits
them, so the "project scope can only tighten" invariant is untouched. `/copy` sends only text the
transcript already shows to the human's own terminal. Nothing is logged to the ledger or the
audit log, since none of it is a tool call.

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
   dependencies in `rust/Cargo.toml`, pinned to the version ratatui already uses so the tree
   keeps one crossterm, and add it to `fontaine-tui/Cargo.toml`. Write the escape through the
   same writer `screen.rs` draws to. Check that `cargo deny check` stays green, since the
   feature pulls in `base64`.
7. **Status bar hint.** Leave `statusbar.rs`'s idle hint as it is. It already points at `/`
   commands, and `/help` is in the menu.
8. **Docs.** Update `docs/tui.md` (see Documentation).
9. **Master plan.** Add the Decisions lines below to `tasks/fontaine_harness_plan.md`
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
- `docs/tui.md`
- `tasks/fontaine_harness_plan.md`: Decisions

## Trade-offs

- **Notices, not dialogs.** Claude Code opens interactive panels for `/help`, `/status`,
  `/permissions`, `/mcp`, `/hooks` and `/skills`. Fontaine renders each one as a transcript
  notice, as `/output-style` already does. It is one code path with pure, testable builders, and
  the text stays in scrollback. An interactive `/permissions` editor would need its own design
  for scope-aware writes (project scope can only tighten), which the 2026-10-21 decision already
  set aside.
- **OSC 52 rather than a clipboard crate.** `arboard` and similar crates need X11, Wayland or
  platform clipboard APIs, and fail over SSH. OSC 52 needs no new dependency, works over SSH,
  and is the route Claude Code itself offers through its `w` fallback. Its cost is that a few
  terminals ignore it, which the success notice says.
- **`/diff` left out.** Claude Code's `/diff` runs `git diff`. Fontaine never spawns `git` on its
  own (2026-10-21 decision), because repo-controlled git config such as `diff.external` or
  textconv drivers can execute commands. Doing it with `gix` blob diffs is a feature of its own
  size. The human can run `!git diff` today, as their own action.
- **`/release-notes` left out.** It would compile `CHANGELOG.md` into the binary, or need
  network access. That has little value before the first release, and it is cheap to add later.
- **One table (as #71 proposed) rather than appending to both lists.** Adding nine commands to
  two hand-kept lists is exactly the drift #71 describes, and the table makes it impossible.

## Decisions

- 2026-10-24: `/help`, `/status`, `/permissions`, `/mcp`, `/hooks` and `/skills` are read-only
  transcript notices instead of Claude Code's interactive dialogs. Reason: one testable path, and
  rule editing stays in config files (2026-10-21 decision). This goes to the master plan.
- 2026-10-24: `/copy` uses OSC 52 only, with no code-block picker and no `w`-to-file. Reason: no
  new dependency, and it works over SSH. This goes to the master plan.
- 2026-10-24: `/diff` is not implemented. Reason: it would spawn `git`, which runs
  repo-controlled config; `!git diff` covers it as the human's own action. This goes to the
  master plan.
- 2026-10-24: `/mcp` takes no `reconnect`/`enable`/`disable` arguments until #35. This goes to
  the master plan.

## Documentation

- `docs/tui.md` `## Commands`: add a row for each new command (`/help`, `/status`,
  `/permissions`, `/mcp`, `/hooks`, `/skills`, `/plan [<description>]`, `/copy [<n>]`) and
  `/tasks` beside `/agents`. Keep the table sorted the way the menu is. Under the table, add a
  sentence that `/copy` needs a terminal with OSC 52 clipboard support (with tmux,
  `set-clipboard on`).
- `docs/permissions.md`: if it describes editing rules, add a line that `/permissions` shows the
  effective rules with their source.

## Testing Strategy

Unit tests (`commands.rs`, `reports.rs`, `transcript.rs`):
- The table: every `BUILTINS` row parses to its own variant through its name and through every
  alias; no name or alias appears twice; `builtin_menu()` has one row per `BUILTINS` entry and
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
- `/copy` after one reply: the bytes written to the test backend contain the OSC 52 sequence
  with the base64 of the reply. This checks the escape actually reaches the writer.
- `/help` lists `/permissions`. This is a smoke test that the dispatch arm exists.

## Manual Verification

1. `cargo run -p fontaine`, ask for a short reply, then `/copy`, and paste into another
   application. The reply text is pasted. Whether a real terminal honors OSC 52 is the
   terminal's behavior, which no test backend reproduces. A failure would be obvious the first
   time anyone uses it.
