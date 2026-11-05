# AFK Permission Mode Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/431

## Overview

Make AFK a sixth `PermissionMode` (`afk`) instead of a session flag beside the mode, and put it
in the Shift+Tab carousel, which is reordered by increasing autonomy:
`manual → plan → accept edits → auto → AFK → manual`. AFK decides what runs exactly as `auto`
does and converts every would-prompt outcome into a denial, as today. The carousel replaces the
bare `/afk` and `/afk off` toggles; `--afk`, `/afk report [all]` and `/afk review` stay. This
reverses the 2026-11-04 decision that AFK is "a session flag beside the mode, not a sixth
`PermissionMode`" (master plan, Decisions).

## Current State

- `PermissionMode` (`rust/fontaine-core/src/config.rs:210`) has `Manual`, `AcceptEdits`, `Plan`,
  `DontAsk`, `Auto`; it deserializes from `[permissions] default_mode` and maps from
  `--permission-mode` (`rust/fontaine/src/cli.rs:336`).
- AFK is `AfkState { on: Option<AfkStretch>, stopped: Option<AfkStop> }` in `SessionState`
  (`rust/fontaine-core/src/gate.rs:369`). `SessionState::set_afk` (`gate.rs:622`) refuses to
  start a stretch while a stop stands, while the guardian is off, or when the ledger cannot
  record `Record::Afk { on }`; it records the toggle in the ledger and audit log.
  `set_guardian_off` (`gate.rs:568`) refuses while AFK is on or stopped, taking the AFK lock
  first.
- `afk_on()` (15 call sites) drives would-prompt conversion through `no_human()` in
  `rust/fontaine-core/src/permissions.rs`, the `AfkApprover` backstop (`permissions.rs:160`),
  audit `afk` fields, the breaker notice (`permissions.rs:2148`), unattended-turn blocking
  (`gate.rs:750`), the prompt's AFK guidance (`agent/setup.rs:773`) and headless reporting.
- `AgentRuntime::set_afk` (`rust/fontaine-core/src/agent/afk.rs:19`) rebuilds the system prompt
  on a toggle. `--afk` starts the stretch before the agent is built (`agent/setup.rs:415`).
- The carousel is `CYCLE` / `next_mode` in `rust/fontaine-tui/src/statusbar.rs:52`
  (`manual → acceptEdits → plan → auto`); `Ui::cycle_mode` (`rust/fontaine-tui/src/ui/mod.rs:431`)
  calls `SessionState::set_mode` synchronously (it is sync, with `&AgentRuntime`). Other callers:
  `serve_while` (`ui/turn.rs:385`, which also serves goal evaluation, `/compact`, workspace
  commands and skill expansion), `serve_modal` (`ui/turn.rs:711`, between queued submissions in
  `converse`, idle, with `&mut AgentRuntime`), the in-turn input loop (`ui/turn.rs:844`) and the
  provider sign-in loop (`ui/providers.rs:223`, idle). `set_mode` (`gate.rs:497`) records `Record::ModeChange` and
  logs `permission_mode_changed`.
- `/afk` (`rust/fontaine-tui/src/ui/commands/afk.rs`, parsed by `parse_afk` in
  `rust/fontaine-tui/src/commands.rs:737`) handles on, off, report and review. `/afk off` first
  runs `assess_afk` (the evasion review), then turns AFK off, then shows the stretch report.
  `afk_report::assessment_of` already finds an `AfkAssessment` recorded after the stretch's off
  record (matched by `stretch_seq`).
- `/afk review` (`afk.rs:144`, `gate.rs:693`) clears a stop and turns AFK off; the base mode
  under it is unchanged.
- Children run on the `Ladder` (`rust/fontaine-agents/src/mode.rs`), mapped from the root by
  `root_mode` / `permission_mode` (`gate.rs:130`). AFK tightens the whole tree because
  `afk_on()` is session state every agent's call reads.
- The status bar shows the mode label (`statusbar::mode_label`) plus a separate ` AFK ` or
  ` AFK STOPPED ` badge (`statusbar.rs:247`); the linear (screen-reader) surface prints
  `[<mode> mode on]` (`linear.rs:207`). The custom status line payload carries `afk { on, stopped }`.
- The session mode is never restored on resume (only `ModeChange` records are written).

## Design

### The mode

Add `PermissionMode::Afk`, `as_str() == "afk"`. It is never read from settings: the variant is
`#[serde(skip_deserializing)]` and `#[schemars(skip)]`, so `default_mode = "afk"` is a parse
error like any unknown mode, and `--permission-mode` gets no `afk` value. The only ways in are
`--afk` and the carousel (the previous rule that only the user's own action starts AFK holds).

Add one helper on `PermissionMode`:

```rust
/// The mode whose rules decide what runs. AFK decides as `auto` does; what it changes is that
/// nobody is asked.
pub fn rules(self) -> PermissionMode { match self { Afk => Auto, m => m } }
```

- `root_mode(Afk)` is `RootMode::Ladder(Ladder::Auto)`: children run at the `auto` rung, so no
  `Ladder` change and the never-looser-than-the-parent property is untouched.
- `PermissionPipeline::posture_of` returns `state.mode().rules()` for the root, so every tier
  decision in `permissions.rs` (the `agent` spawn tier, the final mode match, `unjudged_edit`,
  broad-allow-rule suspension) sees `Auto` and needs no `Afk` arm. Hooks therefore still receive
  `"auto"` under AFK, as they do today. Sites outside the pipeline that ask "is this auto"
  (`native_search::offered`, `project_env` setup reminder, `suspended_allow_rules` reporting in
  `headless.rs:464` and `reports.rs:225`, `announce_auto` in `ui/mod.rs:434` and
  `fontaine-tui/src/lib.rs:452`, `headless.rs:645`, the eval runner) compare `mode.rules()`.
  Exhaustive matches that display a mode (`mode_label`, `linear.rs`, `mode_style`,
  `baseline.rs`) get an explicit `Afk` arm.
- Direct `state.mode()` readers that would otherwise emit `"afk"` use `mode().rules()`: the
  session-event `DecisionRecord` (`gate.rs:804`), the root `PostToolUse` payload
  (`user_hooks.rs:424`, so it matches `PreToolUse`'s `posture_of`), and the user-shell
  `DecisionRecord` (`user_shell.rs:72`).
- Audit and decision records keep `mode = "auto"` plus `afk: true` under AFK (their shape is
  unchanged); `ModeChange` ledger records and `permission_mode_changed` carry `"afk"`.

### One source of truth

`SessionState::afk_on()` becomes `mode() == Afk`. `AfkState.on` is replaced by
`stretch: Option<AfkStretch>` (start seq and source, still needed by reports), which is `Some`
exactly while the mode is `Afk`. `SessionState::set_afk` is removed; its rules move into
`set_mode`, which becomes fallible:

```rust
pub fn set_mode(&self, to: PermissionMode, source: AfkSource) -> Result<ModeChanged, AfkRefusal>
```

- Takes the AFK lock first, then the mode lock (same order as `set_guardian_off`, so
  guardian-off and AFK still exclude each other without interleaving).
- Entering `Afk`: refused with the existing `AfkRefusal::{Stopped, GuardianOff,
  LedgerUnwritable}`; on success appends `Record::Afk { on: true, source }`, then `ModeChange`,
  sets `stretch`. Both guards are dropped before the `afk_on` session event is recorded
  (`record_session_event_as` reads `mode()` / `afk_on()`, so recording under the write guard would
  deadlock; `set_afk` does the same with `drop(afk)` today).
- Leaving `Afk`: appends `Record::Afk { on: false, source }` and `ModeChange`, clears `stretch`,
  drops both guards, then records `afk_off`. Never refused (as `/afk off` is not today, a stop included).
- Any other change: as today. `changed == false` when nothing changed.
- `AfkSource` gains `Cycle` (Shift+Tab / a rebound `cycle_mode` key) and `Plan` (`/plan` out of
  AFK); `Command` stays for older ledgers that recorded `/afk`, and is still written by
  `/afk review`. Old ledgers parse unchanged. `source` is recorded only on AFK enter and leave
  (for other changes it is ignored). Callers pass `Cycle` for the carousel, `Plan` for `/plan`,
  `Command` for the `permissions.rs:1849` guardian-unavailable switch and `/afk review`, and `Cli`
  for `--afk` and the headless run end.
- `AfkRefusal::AfkOn`'s text becomes "AFK mode is on: leave it with Shift+Tab before disabling
  the guardian" (built from the live `cycle_mode` binding where a surface has it; the core
  string names the default key).

`/afk review` (`afk_review`) clears the stop and, if the mode is `Afk`, leaves it for `manual`
through the same path (`Record::Afk { on: false }` + `ModeChange`). Manual is the carousel's next
mode after AFK and the most conservative one; the human who just reviewed is present.

The other existing caller, `permissions.rs:1849` (the guardian-unavailable "switch to manual"
answer), only runs when `no_human()` is `None`, so never under AFK; it discards the result.

### Runtime and prompt

`AgentRuntime::set_afk` becomes `AgentRuntime::set_mode(&mut self, to, source)` for idle-time
callers (`/plan`, the idle carousel, `/afk review`): it calls `SessionState::set_mode`. In-turn carousel presses only have
`&AgentRuntime`: they change the mode on `SessionState`, and the pipeline enforces it on the next
call (as the existing comment in `agent/afk.rs:22` relies on). The runtime records whether the
built prompt carries AFK guidance; `refresh_native_search`, which every turn driver already calls
before a turn, is generalized to `refresh_before_turn` and also rebuilds when that flag differs
from `afk_on()`.

`lib.rs::session_mode` yields `Auto` for `--afk` (still overriding `default_mode`), so the state
is built in `auto`; `setup::start_afk` then enters AFK with `set_mode(Afk, Cli)` before the agent
is built, so the stretch start, the `Afk { on: true }` record and the refusal checks all run. A
refusal is warned and the session stays in `auto`. The headless init / `SessionStart`
`permission_mode` therefore reports `auto` (the start mode) under `--afk`, and the headless v1
schema enum needs no change. At the headless run end, when the mode is `Afk`, the run calls
`set_mode(Auto, AfkSource::Cli)` (`headless.rs:1091` today calls `set_afk(false, Cli)`). `--afk` conflicts with `--permission-mode` at the clap level (two answers to
one question is a usage error) and overrides `default_mode`.

### Leaving AFK: the stretch review

Leaving AFK must still produce the evasion assessment and the stretch report. Because
`assessment_of` already accepts an assessment recorded after the off record, the mode switch
itself is synchronous, and the review follows it:

- `AgentRuntime::assess_afk` takes the stretch it reviews (`stretch_seq`) instead of always
  reviewing the latest one, so a review that runs later never grades a newer stretch.
- `SessionState::set_mode` returns, on leaving AFK, the closed stretch's start seq (the return
  type carries it: `Result<ModeChanged, AfkRefusal>` with `ModeChanged { changed, closed_stretch:
  Option<u64> }`).
- The TUI keeps `pending_afk_review: Vec<u64>`. Idle: the review runs at once, as `/afk off`
  does now ("AFK mode off. Reviewing what happened…", then `afk_outcome_notice`). During a turn:
  the notice "AFK mode off; the stretch is reviewed when this turn ends" is shown, the seq is
  queued, and `cycle_mode` returns the closed stretch (or pushes it onto `pending_afk_review`), and the queue is
  drained by `drain_deferred` (`ui/mod.rs:1148`, beside `pending_pick` / `pending_resume`; called
  from `mod.rs:473` and `turn.rs:306`) and also between submissions in `converse`. Idle callers
  (`serve_modal`, the sign-in loop) therefore review at once; `serve_while` and in-turn presses
  review when that work ends.
- Every TUI exit from AFK goes through this same idle-review / queued-review path: the carousel
  and `/plan`, and any other caller that receives `ModeChanged.closed_stretch`.

### Carousel

```rust
const CYCLE: &[PermissionMode] = &[Manual, Plan, AcceptEdits, Auto, Afk];
```

`next_mode` keeps "a mode outside the cycle returns to the first" (`dontAsk → manual`).
`Ui::cycle_mode` asks for `next_mode(mode)`; if entering AFK is refused, it shows the refusal as
an error and moves on to the mode after AFK (`manual`), never staying in a looser mode than the
press asked to leave. Entering AFK shows the existing "AFK mode on: …" notice with "/afk off
turns it off" replaced by "Shift+Tab leaves it" (the live binding) and the "not in auto mode"
sentence dropped (AFK now always decides as auto). Entering `auto` still runs `announce_auto`;
entering AFK runs it too (the same allow rules are set aside).

The rebindable `cycle_mode` action needs no keybinding change: every binding calls the same
`cycle_mode`.

### Commands

`parse_afk`: the bare form and `off` become usage errors naming the replacement —
`"/afk no longer toggles AFK: Shift+Tab cycles into and out of it; usage: /afk report [all], /afk
review"`. The builtin's hint becomes `report [all] | review` and its description "the AFK report
and the review after a stop". `AfkRequest::{On, Off}` and `Ui::{afk_on, afk_off}` are removed.

### Status display

`mode_label(Afk) = "⏵⏵⏵ AFK mode on"`, with a new `theme.mode_afk` colour (the badge's current
colour). The separate ` AFK ` badge is dropped (the mode label says it); ` AFK STOPPED ` stays.
`linear.rs` prints `[AFK mode on]`. The status line payload keeps `afk { on, stopped }` and its
`mode` field becomes `"afk"`.

### Unchanged

The deterministic floor, guardian judging, fail-closed rules, no-human conversion text, temperature
tracking and stop, breaker blocking, the `AfkApprover` backstop, guardian-disable exclusion, resume
(AFK is not restored; a stop is), headless conversion, and `/afk review` after a stop.

## Implementation Steps

1. **Core mode** — `config.rs`: add `Afk` (serde/schemars skipped), `as_str`, `rules()`.
   `gate.rs`: `root_mode(Afk)`; fold `set_afk` into fallible `set_mode` with `ModeChanged`;
   `AfkState.on → stretch`; `afk_on()` from the mode; `afk_review` leaves AFK for manual;
   `AfkRefusal::AfkOn` text. `afk_report.rs`: `AfkSource::{Cycle, Plan}`.
   Refresh the doc comments at `gate.rs:62,68`, `afk_report.rs:25,338` and `commands.rs:10`.
2. **Pipeline** — `permissions.rs`: `posture_of` uses `rules()`; fix `set_mode` caller at 1849;
   breaker notice text ("after /afk off" → "after leaving AFK mode").
3. **Other mode readers** — `gate.rs:804`, `user_hooks.rs:424` and `user_shell.rs:72` use
   `mode().rules()`; replace `== Auto` checks with `.rules() == Auto` in
   `native_search.rs`, `project_env.rs`, `headless.rs`, `fontaine-tui/src/lib.rs`,
   `reports.rs`, `ui/mod.rs`, `fontaine-evals/src/runner.rs`; add `Afk` arms in display
   matches (`statusbar.rs`, `linear.rs`, `fontaine-evals/src/baseline.rs`).
4. **Runtime** — `agent/afk.rs`: `set_mode` replaces `set_afk`; `assess_afk(stretch_seq)`;
   `finish_afk` / headless pass the current stretch; `headless.rs:1091` ends the run with
   `set_mode(Auto, Cli)` when the mode is `Afk`. `agent.rs`: prompt-AFK flag and
   `refresh_before_turn`, the only prompt rebuild site (callers of `refresh_native_search`
   updated). `agent/setup.rs`: `start_afk` enters AFK via `set_mode(Afk, Cli)`.
5. **CLI** — `cli.rs`: `--afk` `conflicts_with = "permission_mode"`; `lib.rs::session_mode`:
   `--afk` yields `Auto` (overriding `default_mode`); `start_afk` alone enters AFK; doc comments.
6. **TUI** — `statusbar.rs`: `CYCLE`, `mode_label`, `mode_style`, badge; theme `mode_afk`.
   `ui/mod.rs`: `cycle_mode` with refusal skip, notices, idle review / in-turn queue; `/plan`
   through `runtime.set_mode`, its exit from AFK using the same review path as the carousel.
   Startup notice `fontaine-tui/src/lib.rs:462` ("/afk off turns it off" becomes "Shift+Tab leaves
   it"); review confirmation `ui/commands/afk.rs:173` ("AFK mode is off; /afk turns it back on"
   becomes a line saying the session is now in manual); `AfkSource` labels in
   `fontaine-tui/src/reports.rs:462` gain `Cycle` and `Plan` arms. `ui/turn.rs`: drain `pending_afk_review` after a turn.
   `commands.rs` / `ui/commands/afk.rs`: drop on/off.
7. **Docs** — see Documentation.
8. **Decisions** — master plan: append the lines under `## Decisions` below to the end of the AFK
   subsection of its `## Decisions` (after the "Measured on `evals/sequences/afk.jsonl`" line).

## Files to Modify

- `rust/fontaine-core/src/config.rs`, `gate.rs`, `permissions.rs`, `afk_report.rs`,
  `user_hooks.rs`, `user_shell.rs`, `native_search.rs`, `project_env.rs`, `statusline.rs`, `agent.rs`, `agent/afk.rs`,
  `agent/setup.rs`
- `rust/fontaine/src/cli.rs`, `lib.rs`, `headless.rs`, `headless/afk.rs`
- `rust/fontaine-tui/src/statusbar.rs`, `linear.rs`, `lib.rs`, `reports.rs`, `commands.rs`,
  `theme` (wherever `mode_auto` lives), `ui/mod.rs`, `ui/turn.rs`, `ui/commands/afk.rs`,
  `ui/commands/guardian.rs`
- `rust/fontaine-evals/src/runner.rs`, `baseline.rs`
- Tests: `rust/fontaine-core/tests/afk.rs`, `rust/fontaine-tui/tests/afk.rs`,
  `rust/fontaine/tests/afk.rs`, `statusbar.rs` unit tests, `ui/tests.rs`
- Test callers of `set_mode` / `set_afk` (signature change) in `rust/fontaine-core/tests/`:
  `permissions.rs`, `audit_telemetry.rs`, `agent_pipeline.rs`, `project_environment.rs`,
  `runtime_state.rs`, `native_search.rs`, `permissions_auto.rs`, `goal.rs`
- `docs/afk.md`, `docs/permissions.md`, `docs/tui.md`, `docs/headless.md`,
  `docs/configuration.md`, `docs/guardian.md`, `docs/index.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **`rules()` at the pipeline's entry** rather than an `Afk` arm in every match: tier logic stays
  written once, and a future change to `auto` cannot drift from AFK.
- **Synchronous switch, deferred review** rather than blocking the turn on an LLM call when
  Shift+Tab leaves AFK mid-turn, or refusing the press until the turn ends: the press acts at
  once like every other mode switch, and the ledger already supports a review recorded after the
  off record.

## Decisions

- 2026-11-05: AFK (#431): AFK is a sixth `PermissionMode` that decides as `auto`, superseding the 2026-11-04 flag-not-mode decision; only `--afk` and the carousel enter it, never a setting (`default_mode = "afk"` is rejected).
- 2026-11-05: AFK (#431): AFK ends on Shift+Tab (or `/plan`), superseding "ends only with explicit `/afk off`, not on any keystroke"; the issue asks for carousel exit, leaving needs a deliberate press of the cycle binding, and leaving only makes the session ask the human again (manual is next), never looser.
- 2026-11-05: AFK (#431): `/afk` and `/afk off` are removed; `/afk review` clears a stop and lands in manual (supersedes "leaves AFK off"); the `/guardian-disable` exclusion is unchanged, now triggered by the mode.
- 2026-11-05: AFK (#431): Deviation from Claude Code: the carousel order is manual, plan, accept edits, auto, AFK (Claude Code: default, acceptEdits, plan, ...); reason: the issue orders modes by increasing autonomy.
- 2026-11-05: AFK (#431): `--afk` conflicts with `--permission-mode`; `-p --afk` now decides as auto (previously manual plus AFK) since AFK decides as auto; manual plus AFK is no longer expressible, `dontAsk` covers it.
- 2026-11-05: AFK (#431): A refused AFK entry from the carousel skips to manual.

## Documentation

- `docs/afk.md` (including :196, "keeps the denials a headless run already has"): entering and leaving (Shift+Tab, `--afk`), AFK decides as `auto`, review on
  leaving (deferred to turn end when mid-turn), `/afk review` returns to manual, commands table.
- `docs/permissions.md`: modes table gains `afk`; not settable as `default_mode`; :30 ("AFK is not a mode but a flag").
- `docs/tui.md`: carousel order, status label, `/afk` subcommands (lines 117, 176, 179, 219, 572).
- `docs/headless.md`: `--afk` selects AFK mode (decides as `auto`), conflicts with
  `--permission-mode`.
- `docs/configuration.md` (:1145 `cycle_mode` row cycle text; :1299 `--afk` row), `docs/guardian.md`, `docs/index.md`: any `/afk` / `/afk off` mention.
- `research/research-claude-code-parity.md:258`: Shift+Tab "same names/semantics" (order now differs).
- `CHANGELOG.md`: via `pr`.

## Testing Strategy

Unit tests:
- `statusbar.rs`: the full cycle from `manual` back to `manual` through all five modes;
  `dontAsk → manual`; `mode_label(Afk)`.
- `config.rs` / `config` tests: `default_mode = "afk"` is rejected; `rules()` mapping.
- `gate.rs` / `fontaine-core/tests/afk.rs`: `set_mode(Afk)` refused while stopped, while the
  guardian is off, and on an unwritable ledger, with no mode change; entering writes
  `Afk{on:true}` + `ModeChange`; leaving writes `Afk{on:false}` and returns the closed stretch;
  `afk_on()` follows the mode; `set_guardian_off` refused in AFK; `afk_review` leaves AFK for
  manual and clears the stop.
- Pipeline (`permission_rules.rs` / `afk.rs`): under `Afk`, a call `auto` sends to the guardian
  is judged and a would-prompt is converted (deny with `afk`/`converted` fields, record mode
  `auto`); a `tracked_edit` is allowed as in `auto`; a child's posture is `auto` and its prompts
  convert too; the floor still denies.
- `fontaine-core/tests/afk.rs`: tests that turn AFK on under Manual/DontAsk (e.g.
  `a_mode_prompt_becomes_a_denial_and_nobody_is_asked`, expecting a `Tier::Mode` conversion) are
  rewritten for AFK-as-auto (conversion of an auto would-prompt, e.g. an ask rule or a guardian
  escalate) or removed where the premise no longer exists.
- `assess_afk(stretch_seq)` grades the named stretch when a newer one exists.
- `parse_afk`: bare and `off` are errors naming Shift+Tab; `report`, `report all`, `review` parse.
- Prompt: the rebuild-before-turn picks up AFK guidance after an in-turn switch.
- TUI (`fontaine-tui/tests/afk.rs`, rewritten around Shift+Tab; `ui/tests.rs`): Shift+Tab from `auto` enters AFK with the
  notice; Shift+Tab from AFK while idle shows the review and the report and lands in manual;
  pressed during a turn, the review runs after the turn; refused entry (guardian off) shows the
  error and lands in manual; `/plan` from AFK shows the review and the report; a rebound `cycle_mode` key walks the same cycle; the TUI starts in
  `auto` with no mode configured.
- Headless (`fontaine/tests/afk.rs`): drop `--permission-mode auto` from `run_afk` (lines 88-93; rejected by the new conflict); `-p --afk` runs in AFK (the record's mode is `auto` with
  `afk: true`, report present); `--afk --permission-mode manual` is a usage error.

## Open Questions
