# Notify When an Approval or Input Is Pending Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/51

## Overview

When a prompt that needs a human answer opens (an approval, a question, an MCP server prompt, the
guardian-unavailable prompt, the `/guardian-disable` confirmation), the terminal UI tells the
terminal so a user who switched windows finds out. A new `[tui] notify` setting picks the signal:
the terminal bell (default), an OSC 9 or OSC 777 desktop notification, or nothing. Approvals raised
by background agents and guardian escalations reach the user through the same approval overlay,
so one trigger covers them.

## Current State

- Only screen-reader mode rings a bell: `fontaine-tui/src/linear.rs` (`NEEDS_ANSWER`, the `bell`
  flag at ~L446-540) writes BEL when a reply finishes or a listed dialog opens. The graphical TUI
  (`fontaine-tui/src/screen.rs`, `draw_frame`) never signals a pending prompt.
- Prompts are overlays chosen by `view::overlay` (`fontaine-tui/src/view.rs:356`). `Overlay.title`
  plus `Overlay.instance` (a per-prompt id) already identify one prompt opening, which is what the
  linear renderer's `echo.update` uses to detect "opened".
- Approvals from subagents and the guardian's escalations are queued onto `app.approval` /
  `app.questions` like the main agent's, so they surface through the same overlays.
- `[tui]` is `TuiSection` / `Tui` in `fontaine-core/src/config.rs` (~L875, ~L1967, merged ~L2210);
  it is already settable only from the user file or `--settings`, so a repository cannot enable a notification.
- Terminal escapes are written straight to stdout elsewhere (`clipboard.rs`, OSC 52 via
  crossterm).
- Claude Code offers a bell or desktop notification plus a `Notification` hook.

## Design

### Config

`[tui] notify = "bell" | "osc9" | "osc777" | "off"`, default `"bell"`.

- `NotifyMethod` enum in `fontaine-core/src/config.rs` (serde `kebab-case` like `EditorMode` and
  `ThemeName`, with an `as_str` like `EditorMode`'s, `JsonSchema`, `Default` = `Bell`),
  `TuiSection.notify: Option<NotifyMethod>`, `Tui.notify: NotifyMethod`, merged like `editor_mode`.
- An unknown value is a `ConfigError::Parse` at the value's line:column listing the accepted
  values.

### Notifier (`fontaine-tui/src/notify.rs`, new)

```
pub(crate) struct Notifier { method: NotifyMethod, seen: Option<(&'static str, Option<u64>)> }

impl Notifier {
    /// Bytes to write after this frame, if a prompt that needs an answer just opened.
    pub(crate) fn observe(&mut self, overlay: Option<&Overlay>) -> Option<Vec<u8>>
}
```

- `Overlay::notify_message() -> Option<&'static str>` in `view.rs` replaces the `NEEDS_ANSWER`
  title list in `linear.rs`, so the two renderers share one definition of "waiting for the human"
  (open/closed: a new prompt declares itself there); `linear.rs` uses `.is_some()`. It is `Some`
  for the titles approval, question, MCP server, guardian
  unavailable, guardian, `auto-mode setup` (opens after a long background scan, when the user has
  likely switched windows) and `skills` (the skill trust prompt), each with its message
  (`Fontaine needs your approval`, `Fontaine has a question`, ...). `linear.rs`'s bell set grows to
  match.
- `observe` compares `(title, instance)` with the last observed key: a changed key on an overlay
  that needs an answer produces a signal once; closing clears the key, so the next prompt (even
  with the same title) signals again. Repainting the same prompt, scrolling it, or typing into its
  field never re-signals.
- Sequences: `bell` is `\x07`; `osc9` is `ESC ] 9 ; <text> BEL`; `osc777` is
  `ESC ] 777 ; notify ; Fontaine ; <text> BEL`. `<text>` is `notify_message()`'s fixed string, never from model or tool
  output, so no escape can be injected through it.
- The screen backend owns a `Notifier` and writes the bytes after `end_frame`, outside the
  synchronized-update block, so a bell is not swallowed by the frame.
- Screen-reader mode keeps its own bell (it also rings when a reply finishes, which screen-reader
  users rely on) and ignores `notify`.

## Implementation Steps

1. `config.rs`: add `NotifyMethod`, the `TuiSection`/`Tui` fields and the merge.
2. `view.rs`: add `Overlay::notify_message()`; `linear.rs` uses `.is_some()` and drops
   `NEEDS_ANSWER`.
3. `notify.rs`: `Notifier` as above; wire into `screen.rs` and construct it from `config.tui.notify`
   where the TUI is launched.
4. `rust/fontaine-core/src/config_catalog.rs`: add a `tui.notify` row with `Edit::Choice`
   (bell/osc9/osc777/off/unset) and `NextLaunch`, like `tui.editor_mode`, and add it to the test
   `editable_rows_are_exactly_the_preference_set`; `config_panel.rs` needs no change. (Rows live in
   `config_catalog.rs`; test `every_settings_leaf_has_a_row` fails otherwise.)
5. `app.rs`, `view.rs`: give `UnavailablePrompt` an `id` from an opened counter like its sibling
   prompts (`begin_unavailable`), and set `instance` from it in the guardian-unavailable overlay.
6. `tasks/fontaine_harness_plan.md`: append dated (2026-11-04) `(#51)` lines, one per deviation
   with a one-line reason, at the end of the `### TUI` subsection of `## Decisions`: no
   `Notification` hook; key `notify` with values bell/osc9/osc777/off instead of Claude Code's
   `preferredNotifChannel`; no idle notification after a reply finishes; signals immediately
   instead of after Claude Code's ~6 s without typing; default `bell` instead of Claude Code's
   terminal-detecting `auto`; generic `osc9`/`osc777` instead of Claude Code's per-terminal
   `iterm2`/`kitty`/`ghostty` channels; no combined bell+OSC value (Claude Code has
   `iterm2_with_bell`; add on request).
7. `research/research-claude-code-parity.md`: mark the "Notifications when input/approval is
   needed" row (~L386) "shipped (#51)".
8. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-tui/src/notify.rs` (new), `lib.rs`, `view.rs`, `linear.rs`, `screen.rs`
- `rust/fontaine-core/src/config_catalog.rs`
- `rust/fontaine-tui/src/app.rs` (`UnavailablePrompt` id)
- `tasks/fontaine_harness_plan.md` (Decisions)
- `research/research-claude-code-parity.md` (matrix row)
- `docs/configuration.md`, `docs/terminal.md`, `docs/tui.md`, `docs/index.md`, `CHANGELOG.md`

## Trade-offs

- Bell by default: it needs no terminal support beyond what every terminal has and matches the
  screen-reader mode already shipped; OSC 9/777 are opt-in because terminals differ on which they
  honour.
- A single `notify` key instead of separate bell and OSC switches: the methods are alternatives,
  and one value keeps the schema small.
- Detecting the opening from the overlay key rather than from the approval queue: it needs no new
  hook into core and covers every prompt, including background agents' and guardian escalations,
  because they all use the shared overlays.

## Decisions

- No `Notification` hook event: the issue's approach is bell/OSC only; a hook is a separate
  feature that touches the hook schema, trust rules and `/hooks`.
- No tmux passthrough wrapping of OSC sequences: it needs `allow-passthrough` in tmux; the docs
  say so, and the default bell works in tmux through its own bell handling.
- No bell when a reply finishes in the graphical TUI: the issue asks for pending human prompts
  only.
- No combined bell+OSC value: Claude Code has `iterm2_with_bell`; add on request.

## Documentation

- `docs/configuration.md`: `notify` row in the `[tui]` table and example.
- `docs/terminal.md`: a "Notifications" section (methods, terminal support), the tmux
  `allow-passthrough` note for OSC in the existing `## tmux` section, and correct the
  screen-reader bell paragraph to mention `notify` does not apply there.
- `docs/tui.md`: one line in the approval / prompts section pointing to it, and the Settings panel
  section (editable-row count and key table) updated for `tui.notify`.
- `docs/index.md`: update the `terminal.md` summary.
- `CHANGELOG.md`: Added entry under `[Unreleased]` (#51).

## Testing Strategy

Unit tests (`notify.rs`):

- `bell`, `osc9`, `osc777` produce the exact bytes; `off` produces none.
- A prompt signals once across repeated `observe` calls with the same key; a second prompt with
  the same title and a new instance signals again; a closed overlay re-arms.
- Overlays that do not need an answer (`agents`, `denied`, pickers, `config`) never signal.
- `linear.rs` rings for the same set of titles that `notify_message()` accepts (approval, question,
  MCP server, guardian unavailable, guardian, auto-mode setup, skills).

Config tests (`config.rs`/`config_scope.rs`): default is `bell`; each value parses; an unknown
value is a `ConfigError::Parse` at the value's line:column listing the accepted values, modeled on
`tui_editor_mode_accepts_emacs_and_vim_only` in `config_scope.rs`.

Integration (`fontaine-tui/tests`, with a test seam that captures the bytes the notifier produces):
opening an approval writes the configured bytes once.

## Manual Verification

1. In a terminal that supports it, set `[tui] notify = "osc9"`, run `fontaine`, switch to another
   window and ask for a shell command in `manual` mode. Expected: a desktop notification "Fontaine
   needs your approval". Why manual: the terminal's rendering of OSC 9 is outside the process.
2. With `notify = "bell"` in tmux with `monitor-bell on`, the window is flagged when an approval
   opens in a background window.
