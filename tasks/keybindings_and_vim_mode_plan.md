# Configurable Keybindings and Vim Input Mode Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/49

## Overview

Add user-configurable keybindings for the interactive TUI and a vim editing mode for the
input box. Keybindings live in a `[keybindings]` section of the existing TOML config
(user/CLI scope only, like `[tui]`), mapping keystrokes onto named TUI actions. Vim mode is
enabled with `[tui] editor_mode = "vim"` and replaces the input box's emacs-style editing
with a modal INSERT/NORMAL state machine. The mode-cycling action stays a single dispatch
path, so a rebound cycle key cannot reach a mode outside the cycle (e.g. `dontAsk`) or one
the config excludes.

## Current State

- All key handling is hardcoded match arms; there is no binding table.
  - `rust/fontaine-tui/src/app.rs` — `App::handle_key` (~851) matches global keys inline:
    `is_cycle_key` (BackTab / Shift+Tab, ~330) → `Action::CycleMode`, Ctrl-C interrupt/quit,
    Ctrl-T agents panel, Ctrl-V clipboard attach, Ctrl-O todos, Ctrl-L reprint, Esc layers,
    Ctrl-D quit on empty input, Ctrl-P/Ctrl-N → Up/Down. `App::message_key` (~1382) handles
    Enter (Shift/Alt → newline), Ctrl-J newline, Ctrl-R history search, Up/Down history.
  - `rust/fontaine-tui/src/input.rs` — `InputBuffer::handle_key` (~280) implements
    readline/emacs-style editing: Ctrl-A/E/U/K/W/B/F/H/D/Y, Alt-B/F/D, word ops, arrows,
    Home/End. Single-entry kill ring (`killed` field), yank on Ctrl-Y.
  - Every overlay (`approval.rs`, `question.rs`, pickers, panels — 12+ widgets) has its own
    `handle_key`; those keys are **not** remapped by this feature.
  - Key-hint strings duplicate the bindings: `statusbar.rs` (~160–173) and the `/help`
    `KEYS` const in `reports.rs` (~38).
- Mode cycling: `const CYCLE` in `statusbar.rs` (~30) is
  `Manual → AcceptEdits → Plan → Auto` (`DontAsk` excluded); `next_mode` wraps.
  `Ui::cycle_mode` (`ui/mod.rs` ~256) applies it. No config restricts which modes the cycle
  visits; project scope may only tighten `permissions.default_mode` to `manual`/`dontAsk`
  (`config_scope.rs:173`).
- Config system (`rust/fontaine-core/src/config.rs`): `Settings` (`deny_unknown_fields`) →
  `check_scope` (`config_scope.rs`, fail-closed per-key rules) → `validate_values` → merge
  via `Config::apply`. A new key must touch `config.rs` (section + `Settings` field + merged
  struct + `defaults` + `apply` + `validate_values`), `config_scope.rs` (rule row), and
  `config_catalog.rs` (`row!` entry) — choke-point tests in both files fail otherwise.
  Runtime writes to the user file go through `user_config.rs` (`set_config_value`).
  `runtime.config` (merged `Config`) is available where the TUI `Ui` is built
  (`fontaine-tui/src/lib.rs` ~328; `App::new` at `app.rs` ~336).
- Claude Code reference (code.claude.com/docs/en/keybindings): a separate
  `~/.claude/keybindings.json` with per-context blocks, keystroke→action maps, `null` to
  unbind, chord support, reserved shortcuts (Ctrl-C/Ctrl-D…), hot reload. Vim mode is a
  separate editor-mode setting; vim keys are not remappable; Ctrl+key passes through vim to
  the keybinding system.

## Design

### Keystroke and action model (new module `rust/fontaine-tui/src/keymap.rs`)

- `struct Keystroke { code: KeyCode, modifiers: KeyModifiers }` with
  `parse("ctrl+shift+t") -> Result<Keystroke, KeystrokeError>`: modifiers `ctrl`, `alt`
  (alias `meta`), `shift`; key names case-insensitive (`enter`, `tab`, `backtab`, `esc`,
  `up`/`down`/`left`/`right`, `home`, `end`, `pageup`, `pagedown`, `backspace`, `delete`,
  `space`, single chars). `shift+tab` parses to `BackTab` so the existing Shift+Tab default
  round-trips.
- `enum TuiAction` — the remappable, component-level actions (defaults in parentheses);
  all action names are `snake_case`, matching every other config identifier Fontaine defines:
  `cycle_mode` (shift+tab), `interrupt` (ctrl+c — see Reserved below), `quit` (ctrl+d),
  `agents_panel` (ctrl+t), `todos` (ctrl+o), `reprint` (ctrl+l), `attach_clipboard`
  (ctrl+v), `history_search` (ctrl+r), `history_previous` (up), `history_next` (down)
  (ctrl+p/ctrl+n are hardcoded Up/Down aliases), `submit` (enter), `newline`
  (shift+enter, alt+enter, ctrl+j).
  Parsing from string via `FromStr`; serde via string for config.
- `struct Keymap` built by `Keymap::resolve(bindings: &BTreeMap<String, String>) ->
  Result<Keymap, KeymapError>`: starts from the default table, applies user entries, and
  answers `lookup(&self, key: KeyEvent) -> Option<TuiAction>` plus
  `keys_for(&self, action: TuiAction) -> Vec<Keystroke>` (for the hint strings).
- `Keymap::lookup` normalizes a `Tab` event with `SHIFT` to `KeyCode::BackTab` before
  matching, so the default `cycle_mode` binding works on terminals that deliver `Tab`+SHIFT
  instead of `BackTab`.
- **Unbind**: the empty string as the action value removes that keystroke's default binding
  (`"ctrl+t" = ""`). There is no TOML null; `""` is the unbind marker.
- **Reserved**: `ctrl+c` may not be rebound or unbound (interrupt must always work). `ctrl+d`
  may not be rebound or unbound, but the `quit` action fires only on an empty input; when the
  input buffer has text, Ctrl+D stays the input layer's `delete-char` (`input.rs` / `docs/tui.md`).
  `validate_values` rejects reserved rebinds with a `ConfigError`.
  Terminal-aliased keys (`ctrl+i` = Tab, `ctrl+m` = Enter, `ctrl+h` = Backspace, `ctrl+[` =
  Esc) are documented, not specially rejected.
- **Conflict rule**: TOML does not allow duplicate keys in one table, so duplicate keystrokes
  in a single file are a parse error; across files the existing merge order applies (map-valued
  keys merge per key, higher precedence file wins per key). Binding a keystroke that already
  maps to another action replaces it.

### Config plumbing (`fontaine-core`)

- `Settings.keybindings: Option<BTreeMap<String, String>>` directly, like the existing
  `[env]` field; merged `Config.keybindings: BTreeMap<String, String>` (empty = all
  defaults), merged per key in `apply`.
- `[tui] editor_mode: Option<EditorMode>`; `enum EditorMode { Emacs (default), Vim }`
  (kebab-case serde). Merged `Tui.editor_mode: EditorMode`.
- `validate_values`: parse every keystroke key and action value of `[keybindings]`
  (`TuiAction::from_str` lives in `fontaine-core` so validation needs no TUI dep — put
  `TuiAction`/`Keystroke` parsing in a small `fontaine-core/src/keybindings.rs`, and keep
  only `Keymap`/lookup in the TUI); reject reserved rebinds; error text names the offending
  entry.
- `config_scope.rs`: `Rule::user_or_cli("keybindings", …)` and add `tui.editor_mode` to the
  existing `tui` rule's coverage (the `tui` row is section-wide; verify the schema-node
  choke-point test passes).
- `config_catalog.rs`: `keybindings` is a read-only summary row (count/overrides);
  `tui.editor_mode` is an editable choice row via `Edit::Choice(emacs/vim/unset)`, matching
  how `tui.theme` uses `Edit::Choice(theme_choices)`. Note that a `/config` save of
  `tui.editor_mode` applies next launch (the plan's Trade-offs rule); only `/vim` applies
  live.

### Dispatch changes (`fontaine-tui`)

- `App` gains `keymap: Keymap` and `editor_mode: EditorMode`, injected where `Ui` builds
  `App` from `runtime.config` (`lib.rs` ~328); default in `App::new` for tests.
- `App::handle_key` / `message_key`: replace the hardcoded global arms with
  `self.keymap.lookup(key)` → match on `TuiAction`. The overlay priority chain is
  untouched — overlays keep their own keys, and keymap lookup happens only at the points
  where the hardcoded arms run today. `is_cycle_key` is deleted; cycle detection becomes
  `lookup(key) == Some(TuiAction::CycleMode)`.
- **History keys (`history_previous` / `history_next`)**: keymap lookup substitutes only the
  guarded history arms — `Up` on the first line and `Down` while navigating history on the
  last line. The first-line/last-line conditions stay in `App::message_key`; agent-list focus
  on the last line and in-buffer cursor movement remain hardcoded in the input path.
  Unbinding `history_previous`/`history_next` leaves those cursor roles intact.
- **Ctrl+P / Ctrl+N aliases**: `App::handle_key` translates Ctrl+P/Ctrl+N into synthetic
  Up/Down events before dispatch, so they are hardcoded aliases of the Up/Down keys rather
  than separate default bindings. Unbinding `history_previous`/`history_next` therefore
  affects Ctrl+P/Ctrl+N as well.
- **Esc in vim Insert**: `App::handle_key` first runs the palette-close check
  (`close_palette`) exactly as today, because history search, `@` completion and the `/`
  menu stay open while typing. If a palette is open, Esc closes it and is consumed. Only
  when no palette is open does `App::handle_key` offer Esc to the input; in vim Insert the
  input consumes it and switches to Normal. App-level Esc arms (interrupt, cancel wakeup,
  double-Esc rewind) therefore see Esc only from Normal state (or from emacs mode).
- **Mode-cycling constraint**: the rebound key returns the same `Action::CycleMode` and goes
  through `Ui::cycle_mode` → `statusbar::next_mode` exactly like Shift+Tab. There is no path
  from a keybinding to an arbitrary `PermissionMode`, so a binding cannot jump straight to
  `dontAsk` or any mode outside `CYCLE`. If a future config restricts available modes, the
  filter belongs in `next_mode`, keeping this property.
- Hint strings (`statusbar.rs`, `reports.rs` `KEYS`) render from
  `keymap.keys_for(action)` instead of literals, so the UI always shows the effective
  binding; an unbound action's hint is dropped.

### Vim mode (`input.rs`)

- `InputBuffer` gains `mode: EditorMode` and `vim: VimState` where
  `enum VimState { Insert, Normal { pending: Option<PendingOp> } }` (Insert is the
  start state; Visual is out of scope).
- `handle_key` branches on mode: `Emacs` keeps today's arms verbatim (they become the
  `Emacs` arm — no behavior change); `Vim` dispatches by `vim` state.
- **Insert**: printable chars insert; `Esc` → Normal (consumed — it never triggers
  interrupt/cancel from Insert; see Dispatch changes above); Backspace/Delete/arrows/Home/End
  work as today; Ctrl+keys fall through (return `false`) so the App-level keymap sees them,
  matching Claude Code's "most Ctrl+key shortcuts pass through vim mode".
- **Normal**: motions `h l 0 $ w b e`, `gg`/`G` (buffer start/end), operators with motion
  or doubled: `d`/`c` + (`w b e $ 0`), `dd`, `cc`, `D`, `C`, `x`, `r<char>`, `i a A I o O`,
  `p` (yank ring paste after cursor). Yank/delete fill the existing `killed` slot. Operator
  pending state (`d`, `c`, `g`, `r` prefixes) lives in `VimState` as a small pending-op
  field; an unrecognized follow-up key cancels the pending op and is otherwise ignored.
- **Undo**: `u` restores the previous snapshot. Keep a capped stack
  (`Vec<(Vec<char>, usize)>`, cap 100) pushed before each Normal-mode edit (cheap enough at
  prompt sizes; Insert-mode edits don't snapshot). `Ctrl-R` stays history search — no redo.
- **Esc layers**: in Normal state Esc is not consumed by the input; it falls through to the
  existing App-level Esc handling (interrupt / double-Esc rewind), matching the Emacs path.
- Mode indicator: the status bar shows `vim:insert`/`vim:normal` when vim is on (read from
  the input buffer where the status bar already reads input state).
- History (Up/Down), multi-line, paste, and attachments behave the same in both modes.

### Slash commands

- `/vim` — toggles `[tui] editor_mode` in the user config via
  `user_config.rs::set_config_value`, applies to the running session immediately, and prints
  the new state (same live-vs-next-launch reporting pattern as `/config` changes; here it
  can apply live since the mode is read from `App` each keypress).
- `/keybindings` — prints the full action table: action, effective keystroke(s), and whether
  each is default or overridden (from `Keymap`), plus the path of the user config file to
  edit. No interactive editor UI.

## Implementation Steps

1. `fontaine-core/src/keybindings.rs`: `Keystroke` parse/format, `TuiAction` +
   `FromStr`/`Display`, reserved-key list. Add `crossterm.workspace = true` to
   `rust/fontaine-core/Cargo.toml` so `KeyCode`/`KeyModifiers` are available in
   `fontaine-core`. Unit tests for parsing (case, modifiers, `shift+tab`→BackTab,
   unknown key/action, reserved detection).
2. `config.rs`: `Settings.keybindings` (direct map), `EditorMode`, `Settings`/`Config`/
   `defaults`/`apply` wiring, `validate_values` for `[keybindings]` and `tui.editor_mode`.
   `config_scope.rs` rule row; `config_catalog.rs` rows. Add a `keybindings` snippet to
   `config_scope.rs::SNIPPETS` and `tui.editor_mode` to the
   `config_catalog.rs::editable_rows_are_exactly_the_preference_set` expectation. Add an
   `exit_code.rs`-style integration test that a project file with `[keybindings]` exits 2
   naming the scope violation.
3. `fontaine-tui/src/keymap.rs`: default table, `Keymap::resolve`, `lookup`, `keys_for`.
   Unit tests: override, unbind, conflict replacement, reserved rejection surfacing.
4. `app.rs` / `ui/mod.rs` / `lib.rs`: inject `Keymap` + `EditorMode`, replace hardcoded
   global arms with `lookup`, render hints from `keys_for`. Keep overlay dispatch order.
5. `input.rs`: `EditorMode` branch, `VimState` machine, pending-op handling, undo stack.
   Unit tests per motion/operator, Insert→Normal Esc consumption, Normal Esc fall-through,
   undo restore, Ctrl fall-through in Insert.
6. Slash commands `/vim` and `/keybindings` (follow the existing command registration
   pattern under `ui/commands/`).
7. Status bar vim indicator + hint rendering.
8. Docs and changelog (below); master plan Decisions entries.

## Files to Modify

- `rust/fontaine-core/src/keybindings.rs` (new), `Cargo.toml`, `config.rs`,
  `config_scope.rs`, `config_catalog.rs`, `user_config.rs` (only if `/vim` needs a
  new setter shape)
- `rust/fontaine-tui/src/keymap.rs` (new), `app.rs`, `input.rs`, `statusbar.rs`,
  `reports.rs`, `lib.rs`, `ui/mod.rs`, `ui/commands/` (new `vim.rs`, `keybindings.rs` +
  registration)
- `docs/tui.md`, `docs/configuration.md`, `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **`[keybindings]` in the TOML config vs Claude Code's separate `keybindings.json`.**
  Fontaine has one typed config file with scope columns and fail-closed unknown keys; a
  second free-form JSON file would bypass all of that (scope, validation, provenance,
  exit-2 errors). The issue itself names the `[keybindings]` approach. Cost: no JSON schema
  ecosystem and no hot reload (below).
- **Keystroke→action map vs action→keystroke.** Matches Claude Code and makes unbinding and
  conflict resolution natural (one entry per keystroke).
- **No chords in this iteration.** Fontaine has no chord bindings today, so nothing needs
  chord remapping; a chord engine (pending-prefix state, timeout, prefix reservation) is
  pure addition with real complexity. The parser syntax accepts a single keystroke only and
  rejects space-separated sequences with a clear error, leaving the door open.
- **No hot reload.** Claude Code watches the file; Fontaine config applies at startup and
  `/config` changes are next-launch for most keys. Keybindings follow the same rule (change
  takes effect next launch), consistent with the rest of the config surface.
- **Overlays and input editing keys stay fixed.** Remapping every overlay's internal keys
  (Claude Code's 25+ contexts) is disproportionate for a prompt harness; the remappable set
  is the global/chat action level. Vim keys are likewise not remappable (same as Claude
  Code — no `vimInsertModeRemaps` equivalent in v1).
- **`u` undo via snapshots vs a real edit log.** Snapshots are simple and correct at prompt
  sizes; a piece-table/undo-log is over-engineering here.

## Decisions

(Review-round and implementation outcomes land here.)

## Documentation

- `docs/tui.md`: rework the Keys table to name actions, document defaults, add a vim-mode
  section (modes, motions, Esc behavior, indicator) and a remapping example.
- `docs/configuration.md`: `[keybindings]` section (syntax, actions, unbind, reserved keys,
  user/CLI scope) and `tui.editor_mode`; scope matrix rows.
- `tasks/fontaine_harness_plan.md` Decisions: record the deviations from Claude Code —
  TOML `[keybindings]` section instead of `keybindings.json`, no hot reload, no chord
  support, no per-overlay contexts — each dated with a one-line reason.

## Testing Strategy

- Unit: keystroke parser and action parsing; `Keymap::resolve` (override/unbind/conflict/
  reserved); config scope + validation tests for `[keybindings]` and `tui.editor_mode`
  (project-scope rejection exits 2, unknown action errors, reserved rebind errors); vim
  state machine per motion/operator/undo; hint rendering from `keys_for`.
- TUI-level: existing `TestBackend` tests in `app.rs` style — a rebound `cycle_mode` key
  cycles modes in the same order and never yields `dontAsk`; an unbound key does nothing;
  vim mode Enter submits, Esc switches state without interrupting.
- The `config_scope.rs` schema-node and `config_catalog.rs` choke-point tests are the
  regression net for the new config keys: update `config_scope.rs`'s `SNIPPETS` with a
  `keybindings` entry and `config_catalog.rs`'s
  `editable_rows_are_exactly_the_preference_set` expectation to include `tui.editor_mode`.
- Manual: one tmux pass (below) for the feel of modal editing, which unit tests cannot
  judge.

## Manual Verification

Modal editing feel and real-terminal key delivery are not unit-testable:

1. `cargo run -p fontaine` in tmux with `[tui] editor_mode = "vim"` in the user config:
   type text, Esc → NORMAL, `bbdw` edits a word, `A` appends, `u` restores, Enter submits,
   status bar shows the vim state. Expected: each behaves as vim users expect; no terminal
   rendering glitches in the input area.

## Open Questions

- Does `/keybindings` printing a table suffice, or should it offer to open the user config
  in `$EDITOR` (there is precedent in the `/memory` editor launch)?
