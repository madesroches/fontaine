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
  it is already user-scope only, so a repository cannot enable a notification.
- Terminal escapes are written straight to stdout elsewhere (`clipboard.rs`, OSC 52 via
  crossterm).
- Claude Code offers a bell or desktop notification plus a `Notification` hook.

## Design

### Config

`[tui] notify = "bell" | "osc9" | "osc777" | "off"`, default `"bell"`.

- `NotifyMethod` enum in `fontaine-core/src/config.rs` (serde lowercase, `JsonSchema`, `Default` =
  `Bell`), `TuiSection.notify: Option<NotifyMethod>`, `Tui.notify: NotifyMethod`, merged like
  `editor_mode`.
- Error for an unknown value is the usual typed-enum parse error naming the key.

### Notifier (`fontaine-tui/src/notify.rs`, new)

```
pub(crate) struct Notifier { method: NotifyMethod, seen: Option<(&'static str, Option<u64>)> }

impl Notifier {
    /// Bytes to write after this frame, if a prompt that needs an answer just opened.
    pub(crate) fn observe(&mut self, overlay: Option<&Overlay>) -> Option<Vec<u8>>
}
```

- `Overlay::needs_answer()` in `view.rs` replaces the `NEEDS_ANSWER` title list in `linear.rs`, so
  the two renderers share one definition of "waiting for the human" (open/closed: a new prompt
  declares itself there).
- `observe` compares `(title, instance)` with the last observed key: a changed key on an overlay
  that needs an answer produces a signal once; closing clears the key, so the next prompt (even
  with the same title) signals again. Repainting the same prompt, scrolling it, or typing into its
  field never re-signals.
- Sequences: `bell` is `\x07`; `osc9` is `ESC ] 9 ; <text> BEL`; `osc777` is
  `ESC ] 777 ; notify ; Fontaine ; <text> BEL`. `<text>` comes from a fixed table keyed by title
  (`Fontaine needs your approval`, `Fontaine has a question`, ...), never from model or tool
  output, so no escape can be injected through it.
- The screen backend owns a `Notifier` and writes the bytes after `end_frame`, outside the
  synchronized-update block, so a bell is not swallowed by the frame.
- Screen-reader mode keeps its own bell (it also rings when a reply finishes, which screen-reader
  users rely on) and ignores `notify`.

## Implementation Steps

1. `config.rs`: add `NotifyMethod`, the `TuiSection`/`Tui` fields and the merge; regenerate the
   config JSON schema if the repo checks one in.
2. `view.rs`: add `Overlay::needs_answer()` and the title-to-message table; `linear.rs` uses
   `needs_answer()` and drops `NEEDS_ANSWER`.
3. `notify.rs`: `Notifier` as above; wire into `screen.rs` and construct it from `config.tui.notify`
   where the TUI is launched.
4. Settings panel: if `editor_mode` is listed in `config_panel.rs`, list `notify` the same way.
5. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-tui/src/notify.rs` (new), `lib.rs`, `view.rs`, `linear.rs`, `screen.rs`
- `rust/fontaine-tui/src/config_panel.rs` (if `[tui]` keys are listed there)
- `docs/configuration.md`, `docs/terminal.md`, `docs/tui.md`, `CHANGELOG.md`

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
- Screen-reader mode ignores `notify`, so those users keep the bell that announces replies.

## Documentation

- `docs/configuration.md`: `notify` row in the `[tui]` table and example.
- `docs/terminal.md`: a "Notifications" section (methods, terminal support, tmux
  `allow-passthrough` for OSC), and correct the screen-reader bell paragraph to mention `notify`
  does not apply there.
- `docs/tui.md`: one line in the approval / prompts section pointing to it.
- `CHANGELOG.md`: Added entry under `[Unreleased]` (#51).

## Testing Strategy

Unit tests (`notify.rs`):

- `bell`, `osc9`, `osc777` produce the exact bytes; `off` produces none.
- A prompt signals once across repeated `observe` calls with the same key; a second prompt with
  the same title and a new instance signals again; a closed overlay re-arms.
- Overlays that do not need an answer (`agents`, `denied`, pickers, `config`) never signal.
- Each prompt title that needs an answer has a message in the table (loop over the list).

Config tests (`config.rs`/`config_scope.rs`): default is `bell`; each value parses; an unknown
value errors naming `tui.notify`; a project-scope `[tui]` table is still rejected.

Integration (`fontaine-tui/tests`, through the existing `TestBackend`/recording writer in
`screen.rs`): opening an approval writes the configured bytes once; a subagent's approval and a
guardian-escalation approval signal the same way (`tests/agents.rs` already drives background
agent approvals); the screen-reader renderer's output is unchanged for every `notify` value.

## Manual Verification

1. In a terminal that supports it, set `[tui] notify = "osc9"`, run `fontaine`, switch to another
   window and ask for a shell command in `manual` mode. Expected: a desktop notification "Fontaine
   needs your approval". Why manual: the terminal's rendering of OSC 9 is outside the process.
2. With `notify = "bell"` in tmux with `monitor-bell on`, the window is flagged when an approval
   opens in a background window.

## Open Questions

- Should `notify` also have a `both` value (bell plus OSC)? Defaulting to no until someone asks.
