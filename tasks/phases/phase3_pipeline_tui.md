# Phase 3 — Permission pipeline & manual TUI

**Status:** not started · **Depends on:** [Phase 2](phase2_runtime_tools.md) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

Fills the Phase-2 `GuardedTool` stub with the real gate: the pipeline order is specified by the
master plan's §Core data flow diagram (steps 1–9). **Milestone: usable Claude-Code-like agent
with manual approvals.**

## Design (excerpt owned by this phase)

### Permission modes

Five modes with the **same semantics as Claude Code**: `manual`, `acceptEdits`, `plan`, `auto`,
`dontAsk`. The shipped v1 default is `auto` (Claude Code parity — its built-in starting mode since
v2.1.283+); during the Phase-3 interim the default is `manual`, because nothing runs `auto` before
the guardian exists (Phase 4) — no auto without a guardian. **Deliberate subtraction from Claude
Code**: `bypassPermissions` is not shipped at all in v1 — there is no permission-bypass mode, so
the deterministic deny → ask → critical-path floor (and, from Phase 4, the guardian) gates every
call in every mode (reasoning in Decisions). Shift+Tab cycles manual → acceptEdits → plan → auto.
Keeping the same set so muscle memory transfers; the difference is `auto` is powered by our local
guardian against any model.

### TUI (fontaine-tui) — base surface

ratatui + crossterm, codex-rs-style: transcript pane (streaming assistant text, collapsed tool
calls), input box, status bar showing mode (`⏵⏵ auto mode on`). Approval prompt widget for
manual/escalate (allow once / allow session / deny + feedback). `/permissions` edits rules;
`/model` switches live; Shift+Tab cycles modes. (The denial UX — inline `[Rule]` rendering,
`/denied`, `/guardian-model` — is designed and built in Phase 4; the background-agents panel in
Phase 5; see those plans.)

## Steps

1. `PermissionPipeline` with modes `manual`/`acceptEdits`/`plan`/`dontAsk` (`auto` wires in at
   Phase 4 step 2; there is no `bypass` mode) and the step order from the master's data-flow
   diagram (the step-2 deterministic floor: deny → ask → critical-path, in every mode; steps 4–5:
   read-only fast path, then the Tier-0.5 shell pre-classifier with its curated set +
   per-command read-only flag allowlist + audit log); human-approval callback trait.
   Plan mode's step-9 guardian leg falls through to the human prompt until the guardian ships.
   Interim default mode: `manual` — no auto without a guardian.
2. `fontaine-tui`: transcript, streaming render, input, approval prompts, status bar, mode
   cycling.
3. Docs — `docs/permissions.md`: the mode table (`manual`/`acceptEdits`/`plan`/`dontAsk`, the
   interim default and its Phase-4 flip to `auto`), the deterministic floor (deny → ask →
   critical-path, with the critical-path list), and the Tier-0.5 pre-classifier (curated set,
   per-command read-only flag allowlist, fail-through rule, `[shell_preclassifier] allow_extra` —
   user-scope only); complete the full permission-pipeline diagram in `docs/architecture.md`;
   start `docs/tui.md` (keybinds incl. Shift+Tab, approval-prompt options); fill the README mode
   table.

## Files created or modified

`rust/fontaine-core/src/permissions.rs`; `rust/fontaine-tui/src/{app,transcript,approval,statusbar}.rs`;
`rust/fontaine/src/main.rs` (TUI wiring); `docs/{permissions,tui}.md`, `docs/architecture.md`,
`docs/index.md`, `README.md`.

## Tests owned by this phase

- **permission pipeline** (master Testing Strategy): mode × risk-class matrix; plan-mode terminal
  branch 9 behavior; read-only scope check incl. symlink-escape denial; batch tool-call
  partial-deny semantics; the `[permissions] deny` rule and critical-path floor hold in EVERY
  mode; an `[permissions] ask` rule forces a prompt even in `auto` (denies in `dontAsk`).
- **Tier 0.5 pre-classifier**: the golden adversarial set from master Testing Strategy — the
  fail-through soundness property must hold for anything not fully parsed as one simple command.

## Definition of done

- Manual Verification item 1 passes (TUI manual approval flow, end to end).
- Interim default is `manual`; no mode reaches past the deterministic floor.
- `docs/permissions.md` + pipeline diagram match the shipped step order.
- Log the phase in the master plan's Progress log.
