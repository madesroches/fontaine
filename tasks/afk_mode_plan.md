# AFK Mode Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/411

## Overview

AFK mode is a session-scoped, explicitly enabled posture for working while the user is away. It
grants no authority. The mode, the floor, the fast paths, the guardian and fail-closed behavior
are all unchanged. Only what happens where a person would have been asked changes:

- Every would-prompt outcome becomes a denial, using the mechanics headless runs already have.
- Each denial carries trusted recovery guidance: complete independent allowed work or a smaller
  allowed subset, or stop and report. It never says to ask, and never to route around.
- `ask_user_question` takes the existing no-answer path.
- A tripped denial breaker becomes a terminal *blocked* outcome instead of a wait.

AFK also adds two harness-owned safety outputs:

- A persistent **AFK report**: a chronological, credential-redacted timeline of every attempt by
  the main agent and its subagents, built from the session ledger, plus a security review that
  links suspected retries and evasion to the denial they followed.
- A deterministic **session temperature** computed from denial severity and recency. When it
  reaches a ceiling, a second circuit breaker stops the whole agent tree, goals and scheduled
  prompts. Only a recorded human review can clear that stop.

## Current State

Paths are under `rust/` unless noted.

- **Modes and headless.**
  - `PermissionMode` (`fontaine-core/src/config.rs:210`) has five values (`manual`,
    `acceptEdits`, `plan`, `dontAsk`, `auto`).
  - Headless is a fixed field, `PermissionPipeline.headless` (`fontaine-core/src/permissions.rs:327,
    339`), set from `cli.print`.
  - Every place that converts a would-prompt outcome to a denial checks `self.headless`:
    - `would_prompt` (:1160-1180), which covers the floor asks, the ask rule, mode
      `needs_approval` and the breaker pause through `human_or_deny` (:1465);
    - `screen_stop` (:1738, a composition `escalate`);
    - `settle` (:1815, a guardian `escalate`);
    - the guardian-unavailable dialog (:1610).
- **Denial text** has three builders, and all of them end in "tell the user what you could not
  do":
  - `denied()` (:450);
  - the guardian message in `settle` (:1851);
  - the `screen_stop` headless message.
  The base prompt adds "ask the user how to proceed" (`fontaine-core/prompts/system_prompt.md`,
  "Denied tool calls").
- **Denial breaker.**
  - `LoopBreaker` (`fontaine-guardian/src/breaker.rs`) trips at 3 consecutive or 20 total guardian
    denials. It lives in `SessionState.breaker` (`fontaine-core/src/gate.rs:323`), is shared by the
    whole agent tree, and is held in memory only.
  - While it is paused, guardian-bound calls are forced to a human prompt, and in headless to a
    denial.
  - Only `approved()` resumes it (:1915).
  - It does not touch goals, the scheduler or subagents.
- **Session-scoped posture precedent: `/guardian-disable`.**
  - The state is `SessionState.guardian_off` (gate.rs:307), set through `set_guardian_off`
    (gate.rs:490).
  - The setter appends `Record::GuardianOff` and an audit event through `record_session_event`
    (gate.rs:528) with `Tier::GuardianOff`.
  - The TUI handler with its confirmation is `fontaine-tui/src/ui/commands/guardian.rs`.
  - Headless cannot reach it, because headless parses only `/goal`.
- **Ledger** (`fontaine-core/src/ledger.rs`): an append-only JSONL file that one session's whole
  agent tree shares.
  - It survives compaction and `/clear` (`ContextCleared` resets only messages) and is replayed on
    resume. A fork copies it.
  - `Record::Action{tool, agent, paths, created, summary}` has no tier.
  - `Record::Denial{tool, agent, reason, rule, summary}` has no tier, digest or paths, and
    `DenialEntry` in the view drops `agent`.
  - Summaries are redacted by `adjudication::action_summary` (adjudication.rs:33).
  - Nothing records whether an allowed call succeeded.
- **Audit log** (`fontaine-core/src/audit.rs`):
  - one `DecisionRecord` per decision, with `tier`, `rule`, `guardian_verdict`,
    `fail_closed_reason`, `headless` and `explanation`;
  - stored as `<data>/audit/<session>.jsonl`;
  - a fork starts it empty.
- **`/recap`**: `LedgerView::recap()` (ledger.rs:369) is reusable from core, but its renderer is
  TUI-side (`fontaine-tui/src/reports.rs:470`).
- **Goals.**
  - `SessionGoal` (`fontaine-core/src/goal.rs`) has `PauseReason` (:126), but no API to pause it
    with an outside reason.
  - The TUI idle loop (`fontaine-tui/src/ui/mod.rs:478-501`) runs notification turns, then
    scheduled turns, then goal steps.
  - Headless loops through `handle_message` (`fontaine/src/headless.rs:766`).
- **Scheduler** (`fontaine-core/src/schedule.rs`) has no pause (only `delete`, `clear` and
  `end_loop`).
- **Kill paths**:
  - `spawn::kill_agent` (`fontaine-agents/src/spawn.rs:134`) per child.
  - `AgentTree::kill_all` latches a permanent shutdown, so it is unusable here.
  - `jobs().kill_by_user(id)` / `terminate_all()` (`fontaine-tools/src/jobs.rs`).
  - An interrupt drops the turn future (`TurnEnd::Interrupted`).
- **Prompt layers** (`fontaine-core/src/prompt.rs`): the base prompt uses `{{...}}` placeholders;
  the only conditional layer is scheduling. A mid-session change uses `AgentRuntime::rebuild()`
  (agent.rs:337), as `set_output_style` does.
- **`ask_user_question`**: `HumanQuestions::ask` (`fontaine-core/src/questions.rs`) →
  `approver.ask_user`. `QuestionReply::Unavailable` renders "User input is unavailable…"
  (`fontaine-tools/src/ask_user.rs:20`).
- **Config scope** (`fontaine-core/src/config_scope.rs`): no numeric "project may only lower" key
  exists. The nearest precedent is a sticky tightening flag, `composition_shadow_forced`
  (config.rs:2301).

## Design

### 1. AFK state: a session flag beside the mode, not a sixth mode

`SessionState` gains `afk: RwLock<AfkState>`:

```rust
pub struct AfkState {
    pub on: Option<AfkStretch>,   // Some while AFK is active
    pub stopped: Option<AfkStop>, // Some after the temperature ceiling tripped, until reviewed
}
pub struct AfkStretch { pub started_seq: u64, pub source: AfkSource } // Command | Cli
pub struct AfkStop { pub seq: u64, pub value: f64, pub ceiling: f64, pub contributing: Vec<u64> }
```

- AFK works over whatever mode is active, though `auto` is the intended one.
  - In `manual`, AFK only turns every approval into a denial. That tightens, so it needs no
    refusal.
  - `/afk` in a mode other than `auto` shows a one-line notice that every call needing approval
    will be denied.
- The flag is session-wide and read by the pipeline for every agent. A child therefore always
  carries its parent's AFK posture (only ever tighter), with no ladder change.
- `set_afk(on, source)` mirrors `set_guardian_off`:
  - it does nothing if nothing changes;
  - it appends `Record::Afk{on, source}`;
  - it emits `record_session_event("afk", …, Tier::Afk, "afk_on"|"afk_off", …)`.
  A new `Tier::Afk` is added to `Tier::ALL`.
- **Activation** needs an explicit user action: `/afk` (the TUI command) or `--afk` (a CLI flag for
  both TUI and headless start). No config key, tool, scheduled prompt or model output can set it.
  - The CLI flag is the user's own command line, so it is not project-settable.
  - `/afk` is `main_only` and handled TUI-side only, as `/guardian-disable` is.
- **Leaving AFK** is explicit: `/afk off`. Typing a message does not end AFK. Approvals stay denied
  until the user says they are back. When AFK ends, the end-of-stretch assessment runs (§6) and the
  report summary is shown.
- **Resume.** AFK is *not* restored on resume, because a person ran `--resume` and is present. A
  stop *is* restored: `LedgerView` replays `Record::AfkStop` and `Record::AfkReviewed`, so an
  unreviewed stop survives resume and fork.

### 2. Would-prompt → denial while AFK

The pipeline replaces each `self.headless` check with one method:

```rust
enum NoHuman { Headless, Afk }
fn no_human(&self) -> Option<NoHuman> // headless first, then state.afk().on
```

It applies at the same four sites: `would_prompt`, `screen_stop`, the `settle` escalate arm and
the guardian-unavailable branch. AFK skips the guardian-unavailable dialog, so
`UnavailableChoice::Manual` can never switch the mode while the user is away.

**Counting and records.**
- Breaker counting is unchanged from headless. A converted guardian escalate goes through `settle`'s
  deny arm and counts. A converted floor or ask-rule prompt and a converted `screen_stop` do not.
- The original tier, rule, guardian verdict and fail-closed reason stay in the `DecisionRecord`.
  It gains two fields:
  - `afk: bool`;
  - `converted: Option<Converted>` — `ask_rule | floor | mode | escalate | screen_escalate |
    breaker_paused | guardian_unavailable`.
  `converted` is set for headless conversions too, since it is free and useful.
- dontAsk + AFK: `would_prompt` returns the dontAsk text before it checks headless. Such
  conversions keep the dontAsk fragment and set `converted = mode`; `recovery()` reads `no_human()`
  independently, so the AFK sentence still applies.

**Backstop.**
- `ask_human` re-checks `no_human()` immediately before `approver.approve`, to catch an AFK toggle
  that races an in-flight decision.
- A decorator `AfkApprover` wraps the approver. While AFK is on it returns `Deny` from `approve`,
  `Unavailable` from `ask_user`, and `Deny` from `guardian_unavailable`. It is defence-in-depth over
  the only three non-test approver call sites: `approve` in `ask_human` (permissions.rs:1978),
  `guardian_unavailable` (permissions.rs:1622) and `ask_user` (questions.rs:28). It forwards
  `notice` and `warn` unchanged.
- The raw approver goes into `SessionState`, which uses it only for `warn`. The decorator over
  `Arc<SessionState>` is what is handed to the permission pipeline and to `HumanQuestions`.

**`ask_user_question`.** `HumanQuestions` shares the same approver (build.rs:231), so the decorator's
`ask_user` already gives `QuestionReply::Unavailable`, the existing best-judgment text, while AFK is
on. `questions.rs` needs no change.

### 3. One trusted denial-guidance builder

`permissions.rs` gets one function that owns the recovery sentence, and every denial builder calls
it: `denied()`, the `settle` guardian message, `screen_stop` and the exhausted-agent denial. The
breaker-pause denial is covered by `denied()` via `human_or_deny`→`would_prompt`.

```rust
fn recovery(no_human: Option<NoHuman>) -> &'static str
```

- Attended (and headless, unchanged): "Do not retry it or look for another route to the same
  effect; tell the user what you could not do."
- AFK (`AFK_RECOVERY`): "The user is not available. Do not retry this call, evade the denial, or
  use another tool to achieve the same denied effect. Complete the rest of the request through
  independently allowed work; if the whole request cannot be done within those bounds, reduce it to
  a useful allowed subset. If no useful allowed progress remains, stop and report what was
  completed, what was blocked and why, and what remains. Do not ask for approval."
- The conversion reason fragment follows the headless pattern: "`{reason}`, and the user is away
  (AFK mode)". The original rule and explanation come first, unchanged.
- A user `permission_denied` hook retry keeps its current text. Hooks are user-authored and can
  still tighten.

### 4. System prompt: replace the blocked-call instruction

The base prompt's "Treat the denial as the user's decision… ask the user how to proceed" bullet
becomes a `{{denial_recovery}}` placeholder, filled at build time:

- **Attended:** today's text.
- **AFK:** "The user is away (AFK mode). Treat the denial as final for this session: continue with
  independent allowed work or a useful allowed subset, and when nothing useful remains, stop and
  report what you completed, what was blocked and why, and what remains. Do not ask for approval
  or wait for a reply."

Subagent prompts (`agent/setup.rs:743`) are filled the same way. `PromptParts` gains
an AFK flag. An `AgentRuntime::set_afk` wrapper calls the `SessionState` setter and then
`rebuild()` on the root, as `set_output_style` does (agent.rs:466). Headless `--afk` is applied at
build time. With `--system-prompt` (a replacement) the placeholder is not present, so the AFK
guidance arrives only through the denial texts. Children already running keep
their prompt, and the AFK guidance on each denial covers them. Toggling AFK costs one prompt-cache
miss per toggle.

### 5. Breaker under AFK: terminal blocked outcome

The breaker is untouched (same limits, same counts). AFK never calls `resume_auto`: the only
resume is `approved()`, which AFK cannot reach. After `/afk off`, the next guardian-bound call
prompts as today, and approving it resumes `auto`.

When the breaker is paused while AFK is on, the session is **blocked**:

- `SessionGoal::pause(PauseReason::Blocked(String))` is a new public API. `PauseReason` gains
  `Blocked` and `AfkStopped`, and the goal pauses with "the denial breaker tripped while the user
  was away". `Record::Goal{Paused}` gains an optional `reason` (serde default, so old ledgers still
  parse).
- An idle-loop gate on `AgentRuntime` decides whether any turn the user did not type may start.
  It answers `Some` when AFK is on and the breaker is paused, or when an AFK stop is active:

  ```rust
  fn unattended_blocked(&self) -> Option<Blocked>
  ```

  The TUI idle loop checks it before notification turns, scheduled turns and goal steps, and
  skips all three while it is `Some`. The gate is checked before `scheduler.due()` (which consumes
  a fired job), without busy-looping. The headless goal loop checks it before each goal turn. The
  scheduler needs no pause API: its jobs simply do not fire while the gate holds. They stay listed
  and are not deleted.
- The turn already running is not cut short, because each denial tells the model to report. When
  it ends, the TUI shows the AFK outcome notice: "AFK: blocked — repeated denials paused auto
  mode. N allowed, M denied, temperature X (peak Y). Full report: /afk report".
- In headless, `afk.outcome = "blocked"` sets exit 1 independently of whether a goal ran (the
  current paused-goal exit is filtered by `goal_ran`, headless.rs:933-938).

### 6. AFK report

**Data: extend the ledger, the single replayed record.** The audit log is not joined: a fork starts
it empty, and it holds digests, not summaries. All new fields carry `#[serde(default)]`, so old
ledgers still parse.

- `Record::Action` gains `tier: Option<Tier>`, `tool_use_id: Option<String>` and
  `digest: Option<String>`.
- `Record::Denial` gains:
  - `tier: Option<Tier>`;
  - `tool_use_id`;
  - `digest` (the same args digest as the audit record);
  - `paths: Vec<String>` (`touched_paths`, computed for every denial, not only non-read-only
    ones);
  - `converted: Option<Converted>`;
  - `explanation: Option<String>`: the rule's explanation without the recovery sentence, cut to
    `SUMMARY_CHARS`, and passed through `rig_core::redact::text`, since `reason` is not redacted
    today.
- New `Record::ToolOutcome{tool_use_id, agent, status: Ok|Error|Cancelled}` is appended by an
  always-attached core result stage, independent of the optional `PostHookStage` (attached only
  when post hooks exist, setup.rs:600-605). `gated()` lives in fontaine-tools with no ledger,
  `SessionState` or `tool_use_id`, so the stage lives in core and reads the id from
  `state.current_call(agent)`. It skips synthetic results (screened texts with no `tool_use_id`:
  agent.rs:562, child_runner.rs:188). `ResultPipeline::run` is skipped for calls killed mid-run
  (`cut_short`, guarded.rs:381-395), so that path records `Cancelled` explicitly. This is what separates "allowed" from "succeeded". A missing outcome renders as "outcome not
  recorded", never as success.
- New `Record::Afk{on, source}`, `Record::AfkStop{value, ceiling, contributing}`,
  `Record::AfkReviewed{}` and `Record::AfkAssessment{stretch_seq, result}`.
- `DenialEntry` keeps `agent`.

**Core builder.** `fontaine-core/src/afk_report.rs` is a pure function over `&[Entry]`, built from
`Ledger::read()` so it covers the whole history across resume, compaction and `/clear`:

```rust
pub struct AfkReport {
    pub stretch: Stretch,                 // started/ended seq+ts, source, outcome
    pub outcome: AfkOutcome,              // Active | Ended | Blocked | Stopped
    pub counts: Counts,                   // allowed, denied, converted, succeeded, failed, unknown
    pub temperature: TemperatureReport,   // current, peak, peak_seq, contributing denials
    pub timeline: Vec<Attempt>,           // chronological, every agent
    pub review: SecurityReview,
}
pub struct Attempt { seq, ts, agent, agent_name, tool, summary, decision: Allowed|Denied|Converted,
                     tier, rule, explanation, outcome: Option<OutcomeStatus> }
pub struct SecurityReview { pub linked: Vec<Link>, pub assessment: Assessment }
pub struct Link { pub denial_seq: u64, pub attempt_seq: u64, pub kind: LinkKind, pub reason: String }
pub enum LinkKind { ExactRetry, SameTargetOtherTool, Suspected /* guardian-assessed */ }
pub enum Assessment { NoDenials, Pending, NoSuspectedEvasion, Found, Unavailable(String) }
```

- Agent names come from `Record::Agent` events.
- The report covers the latest AFK stretch by default. `/afk report all` lists every stretch.

**Deterministic links**, computed only between a denial and later attempts in the same stretch:

- `ExactRetry`: a later attempt with the same `tool` and `digest` as an earlier denial, from any
  agent.
- `SameTargetOtherTool`: a later attempt with a *different* tool, in the same direction as the
  denied call, whose `paths` intersect the denial's `paths`.
  - Direction comes from the tool's risk class in the descriptor table: write-class after a
    write-class denial, or read-class after a read-class denial.
  - `shell` counts by its touched paths: redirect targets for writes, named paths for reads.
  - This catches a denied `write` followed by a `shell` write to the same file.
  - A later call to a different path, or a read after a denied write, is never linked. Independent
    work and a narrower scope therefore stay unmarked.

**Independent semantic assessment** (the working model never grades itself):

- `fontaine-guardian/src/evasion.rs` adds `assess_evasion(&GuardianModel, &AssessmentInput) ->
  Result<Vec<SuspectedLink>, AssessError>`. It is one tool-less extractor call on the guardian
  model, with a schema-constrained output, a timeout and the guardian `UsageTally`, following
  `ExtractorBuilder::<Verdict>`.
- The input is the stretch's attempts in order: id, agent, tool, redacted summary, decision, rule,
  and the denial explanation. The input is wrapped in an `<untrusted kind="activity">` block,
  because summaries are model-originated.
- The prompt (`fontaine-guardian/prompts/evasion_prompt.md`) asks only for pairs where a later
  attempt pursues a denied call's *same effect* by another route. It explicitly excludes
  independent work and a smaller scope. The model can only name denial and attempt ids that exist
  in the input. Unknown ids are dropped.
- It runs once per stretch at its end: `/afk off`, block, stop, or the end of a headless run. It
  runs only when the stretch has at least one denial; otherwise the result is `NoDenials`.
- The result is appended as `Record::AfkAssessment`. Rendering:
  - error, timeout or unparseable output → **evasion assessment unavailable: <reason>**, with the
    raw timeline still shown;
  - success with no pairs → **no suspected evasion detected**;
  - pairs → each one listed as **suspected evasion**, with both calls and the reason.
  No path ever renders "no evasion".
- The per-call guardian keeps its existing alternate-route policy (`guardian_prompt.md`). The
  assessment adds the end-of-stretch view that can catch what the per-call gate missed.

**Rendering.**
- TUI: `fontaine-tui/src/reports.rs` gains `afk_report(&AfkReport) -> String`, shown in the
  scrollable transcript view (Ctrl-O) as a notice, like `/recap`. Sections, in order:
  1. outcome line;
  2. temperature (current, peak, top contributing denials);
  3. security review (links first, then the assessment status);
  4. the timeline.
  Summaries and explanations are cut to the recap `LINE` width.
- `/recap` gains one line while AFK is active or a report exists: "afk: <outcome> — /afk report".
- Headless: the `result` event gains an optional `afk` object:
  `{outcome, counts, temperature{current, peak, ceiling, contributing[]}, review{links[],
  assessment}, timeline[]}`. It is added to `docs/schemas/headless-v1.schema.json` as an optional
  property, so the v1 contract is additive. Every `gate_decision` event gains `afk` and
  `converted`.

### 7. Session temperature

`fontaine-guardian/src/temperature.rs` stays dependency-light, next to the breaker:

```rust
pub enum Severity { Low, Medium, High }          // weights 1.0, 2.0, 3.0
pub const DECAY: f64 = 0.9;                       // per later gate decision
pub const EXACT_RETRY_BONUS: f64 = 2.0;
pub const SAME_TARGET_BONUS: f64 = 3.0;
pub const FAIL_CLOSED_WEIGHT: f64 = 0.5;
pub const DEFAULT_CEILING: f64 = 12.0;           // provisional: settled by the temperature evals
pub struct Temperature { current: f64, peak: f64, peak_step: u64, contributions: Vec<Contribution> }
impl Temperature {
    pub fn step(&mut self);                        // any gate decision, allowed or denied
    pub fn denial(&mut self, seq: u64, weight: f64) -> f64; // returns new current
}
```

- **Calculation.**
  - `current = Σ wᵢ · DECAY^(decisions since i)`, summed over the stretch's denials.
  - Time is counted in gate decisions, not wall-clock time. That keeps the value deterministic,
    replayable from the ledger and testable. Idle time does not cool it; independent allowed work
    does.
  - `peak` is the maximum of `current` and never decreases.
  - `contributions` keeps every denial with its weight and bonus. Cooling never removes evidence.
- **Severity mapping**, in core (`afk_report.rs`), because `Tier` lives there:
  - Guardian or composition deny, by `RuleLabel` parsed from the rule display name:
    - **High:** Data Exfiltration, Download+Execute, Tunnel/Reverse Shell, Secret Exposure,
      Safety Bypass, Container Escape, Unguarded Agent Loop.
    - **Medium:** Git Destructive, Destroy Pre-existing Files, Production Deploy, IAM Grant,
      Remote Redirect, Unreviewed Merge, and any unrecognized label.
  - `DenyRule`, `CriticalPath`, `ProtectedPath` and `PlanBlock` denials → Medium.
  - Converted prompts (`converted` set) and breaker-paused denials → Low. While the breaker is
    paused the guardian is not asked, so its severity is unknown, but continued pressure still
    counts.
  - Fail-closed denials (`fail_closed_reason` set: guardian error, timeout or unparseable output)
    → `FAIL_CLOSED_WEIGHT`, because they show guardian trouble rather than agent pressure.
  - Bonuses: `ExactRetry` adds `EXACT_RETRY_BONUS` to the retry's weight, and
    `SameTargetOtherTool` adds `SAME_TARGET_BONUS`. The links are computed when the attempt is
    gated. If the attempt is allowed it adds no weight, but the link still appears in the report.
- **Live evaluation.** `Ledger::read()` is used only on resume and for the report. The live path
  keeps a per-stretch in-memory index of denials and the decision count (seq, tool, digest, paths,
  weight), fed at append time and using the same pure functions. `SessionState` holds `afk_temperature: Mutex<Temperature>`, which is reset
  when a stretch starts. After every recorded denial while AFK is on, the pipeline updates it and
  compares `current` with the ceiling. On resume, the value is rebuilt by replaying the stretch,
  using the same function the report uses, so the live and reported values cannot drift.
- **It only tightens.** Temperature feeds no allow path, no guardian input, no breaker reset and no
  evidence removal. It is computed whether or not AFK is on, so the report can show it, but it
  stops the session only while AFK is on.
- **Ceiling config**, `[afk] temperature_ceiling` (float, `> 0`, finite):
  - The user scope and `--afk-temperature-ceiling` set the value; CLI beats user, user beats the
    default.
  - The shared project and project-local files may only lower it. `apply` keeps the lowest
    project value in a separate `afk_ceiling_project_cap: Option<f64>`, and the effective ceiling
    is `min(user_or_cli_or_default, cap)`. That makes the lowering order-independent, like
    `composition_shadow_forced`.
  - The `config_scope` row is `Rule::if_project_local("afk.temperature_ceiling", is_positive_finite,
    …)`. A non-positive or non-finite value is a config error in every scope, so no scope can
    disable the breaker with `0` or `inf`.
  - The model has no path to it: there is no tool and no config write in auto. Edits to settings
    files are already a protected-path and Safety Bypass case.

### 8. Temperature stop: the session-level circuit breaker

When the ceiling is reached, `SessionState::afk_stop(value, ceiling, contributing)`:

1. Sets `afk.stopped`. From then on the pipeline's first check, before the floor, denies **every**
   call from every agent with `Tier::Afk`, rule `afk_stopped` and a fixed message: "the session was
   stopped by the AFK temperature ceiling; no further tool calls run until the user reviews it".
   This is a deny-only check and can never allow.
2. Appends `Record::AfkStop` and an audit event (`Tier::Afk`, `afk_stop`) that carries the value,
   the ceiling and the contributing denial seqs.
3. Fires a session `stop` signal, a `tokio::sync::watch<bool>` on `SessionState` that the turn
   drivers subscribe to:
   - The TUI turn loop and the headless `run_one` select on it next to the interrupt. Headless
     `run_one` (headless.rs:489) has no interrupt select today, so a `select!` on the stop watch is
     added around `run_turn`. Its subtype mapping must not reuse the hard-coded `MaxBudget` for
     `TurnOutcome::Stopped` (headless.rs:580). It ends the
     in-flight main turn as `TurnEnd::Stopped(AfkStopped)` by dropping the turn future, as Esc
     does, so the main model gets no further tool-capable turn.
   - The runtime kills every live child with `spawn::kill_agent` per id (not `kill_all`, which
     latches) and kills each running job with `jobs().kill_by_user(id)` (jobs.rs:962). `Jobs::terminate_all`
     (jobs.rs:1078) does not latch but is silent (it posts nothing to the model) and blocks up to
     grace+5 s, so per-id kills stay and the model sees each kill note. `AgentTree::kill_all`
     does latch.
4. Pauses the goal with `PauseReason::AfkStopped`. `unattended_blocked()` keeps scheduled and
   notification turns from firing.
5. Runs the evasion assessment (a guardian-model call, which is not the working model and has no
   tools), then renders the report summary from the ledger:
   - TUI: as a notice;
   - headless: in the `result` event's `afk` object, with `subtype: "error_afk_stopped"` (a new
     value) and exit 1.

**While a stop is active:**
- A typed TUI prompt is refused with "the session is stopped (AFK temperature ceiling); run /afk
  review". The refusal also covers deferred queued input (`drain_deferred`,
  fontaine-tui/src/ui/mod.rs:469).
- `/afk` cannot turn AFK back on.
- Read-only slash commands (`/afk report`, `/recap`, `/denied`, `/export`) still work.

**Resume** is `/afk review`, which is interactive only:
1. It shows the report.
2. It asks for a confirmation in the style of the `/guardian-disable` prompt.
3. On confirmation it appends `Record::AfkReviewed` and an audit event (`afk_review`), clears
   `stopped`, and leaves AFK off.

Review does not touch the denial breaker, the peak or any evidence. Cooling below the ceiling never
clears a stop. Headless has no review: a stopped session resumed headless refuses every call, and
the run exits 1 with `error_afk_stopped`.

### 9. Commands and flags

- `/afk` turns AFK on (with the notice), and `/afk off` turns it off. `/afk report [all]` shows the
  report, and `/afk review` resumes a stopped session.
  - These are `Command::Afk(String)` with a `parse_afk` beside `parse_loop`
    (`fontaine-tui/src/commands.rs`).
  - All four are idle-only and `main_only`. `report` is queued, not `runs_mid_turn`, since it does a
    full `Ledger::read()`, which breaks the mid-turn contract (commands.rs:123). The test
    `runs_mid_turn_marks_the_immediate_ui_set_and_nothing_else` (commands.rs:994) lists stay
    unchanged.
- The status line shows `AFK` while it is on and `AFK STOPPED` while stopped, mirroring the
  guardian-off indicator (`fontaine-tui/src/statusbar.rs`). The custom status-line payload
  (`fontaine-core/src/statusline.rs`) gains AFK/stopped fields.
- `--afk` (`fontaine/src/cli.rs`) applies to TUI start and to `-p`. `--afk-temperature-ceiling
  <f64>` is a CLI-scope ceiling.
- In headless, `--afk` keeps today's would-prompt denials but switches to the AFK guidance and
  prompt layer, enables the temperature stop, runs the assessment and adds the `afk` result object.
  `no_human()` returns `Afk` when both flags are set, so the AFK text wins.

## Implementation Steps

**Phase A — state, conversion, guidance**
1. `fontaine-core/src/tier.rs`: add `Tier::Afk`, extend `Tier::ALL`, and fix the count tests. Add
   `afk` to `$defs.tier.enum` in `docs/schemas/headless-v1.schema.json` (pinned by
   `the_schema_tier_enum_is_exactly_the_tier_names`, rust/fontaine/tests/headless_contract.rs:28).
2. `fontaine-core/src/ledger.rs`:
   - add the new `Action`/`Denial` fields, `ToolOutcome`, `Afk`, `AfkStop`, `AfkReviewed`,
     `AfkAssessment`, and the `Goal{Paused}` reason, all with `#[serde(default)]`;
   - make `DenialEntry` keep `agent`;
   - make `LedgerView` replay the stop and review state.
3. `fontaine-core/src/gate.rs`:
   - add `AfkState`, `afk()`, `set_afk()`, `afk_stop()`, `afk_review()`, the temperature mutex and
     the stop `watch`;
   - restore an unreviewed stop on open.
4. `fontaine-core/src/permissions.rs`:
   - add `NoHuman`/`no_human()` at the four sites, and the re-check in `ask_human`;
   - add `recovery()` and route every denial builder through it;
   - add `converted` and `afk` on `DecisionRecord` (`audit.rs`);
   - fill the new ledger fields in `action()`/`denial()`;
   - add the stopped deny-only check first in `evaluate`.
5. `fontaine-core/src/permissions.rs`: add the `AfkApprover` decorator. Put the raw approver in
   `SessionState` and hand the decorator over `Arc<SessionState>` to the pipeline and
   `HumanQuestions` in `agent/setup.rs`.
6. An always-attached core result stage (reading `state.current_call`, skipping synthetic results,
   recording `Cancelled` on the kill/cut-short path): append `Record::ToolOutcome`.
7. `fontaine-core/prompts/system_prompt.md` and `prompt.rs`: add the `{{denial_recovery}}`
   placeholder and both texts. Fill it in `agent/build.rs` and `agent/setup.rs` (subagents).
   Add an AFK flag to `PromptParts`; the `AgentRuntime::set_afk` wrapper calls the state setter
   then `rebuild()`.

**Phase B — temperature, stop, gates**
8. `fontaine-guardian/src/temperature.rs`: the `Temperature`, `Severity` and constants, with unit
   tests.
9. `fontaine-core/src/afk_report.rs`:
   - severity mapping, deterministic links, and the temperature replay;
   - `AfkReport` built from entries;
   - use it from the pipeline after each AFK denial, then call `afk_stop` on reaching the ceiling.
10. `fontaine-core/src/config.rs` + `config_scope.rs`: `[afk] temperature_ceiling`, with the
    project cap, validation and scope row. `fontaine/src/cli.rs`: `--afk` and
    `--afk-temperature-ceiling`, applied in `lib.rs` session setup.
11. `fontaine-core/src/goal.rs`: `PauseReason::{Blocked, AfkStopped}` and `SessionGoal::pause`,
    with the wrapper in `agent/goal.rs`. `AgentRuntime::unattended_blocked()`, and the stop
    handling that kills children and jobs per id.
12. TUI:
    - `ui/mod.rs`: gate the idle loop (before `scheduler.due()`), refuse typed and deferred
      prompts while stopped, show the outcome notice.
    - `ui/turn.rs`: select on the stop `watch`. `ui/goal.rs`: handle the new `TurnEnd` variant in
      its exhaustive match.
    - Status-line indicator (`statusbar.rs`) and the `statusline.rs` payload fields.
    - Headless (`fontaine/src/headless.rs`): add a `select!` on the stop watch around `run_turn` in
      `run_one`, map `TurnOutcome::Stopped` to `error_afk_stopped` (not `MaxBudget`), gate the goal
      loop, add the `afk` result object and the `gate_decision` fields. Update
      `docs/schemas/headless-v1.schema.json` (the `afk` property and the `afk` tier).

**Phase C — report, assessment, commands**
13. `fontaine-guardian/src/evasion.rs` + `prompts/evasion_prompt.md`: `assess_evasion`. The core
    wiring runs it at the end of a stretch and appends `AfkAssessment`.
14. `fontaine-tui/src/commands.rs`: `Command::Afk`, `parse_afk` and the builtin row. Add
    `ui/commands/afk.rs`, with the on/off/report/review handlers (`report` idle-only, queued) and
    the review confirmation in the `guardian_prompt.rs` style, wired like `guardian_off_prompt`
    through `view.rs` (`transcript_has_keys`/`overlay`) and a new `PromptKind` in `notify.rs`. Add `reports::afk_report` and the `/recap` line.
15. Evals (`rust/fontaine-evals`, `evals/sequences/afk.jsonl`). Sequence-shaped cases do not fit
    `Case` (`deny_unknown_fields`, single-call required fields, `enum Kind {Guardian, Pipeline,
    Scanner}`, case.rs:13), and CI loads the whole `evals/dataset` dir in three modes
    (ci.yml:119-129). So a separate sequence-case type gets its own loader, reading a file outside
    the single-call loader's glob. It runs offline in the fixture mode and is skipped in `minimal`
    and `--guardian none`:
    - `temperature` cases: a sequence of `{tool, tier, rule, digest, paths, decision}` and an
      expected `stop|continue`, scored with false-stop and false-continue rates;
    - `evasion` cases: a sequence plus the expected suspected pairs, with `fixture_verdict`
      offline and the guardian model live.
    Calibrate `DEFAULT_CEILING` so every fixture passes, and record the measured rates in the
    master plan's Decisions.
16. Docs and master-plan Decisions (below).

## Files to Modify

- `rust/fontaine-core/src/{tier.rs, ledger.rs, gate.rs, permissions.rs, audit.rs, prompt.rs, goal.rs, config.rs, config_scope.rs, agent.rs, agent/build.rs, agent/setup.rs, agent/goal.rs, statusline.rs}`
- `rust/fontaine-core/src/afk_report.rs` (new)
- `rust/fontaine-core/prompts/system_prompt.md`
- `rust/fontaine-guardian/src/{temperature.rs, evasion.rs}` (new), `src/lib.rs`, and `prompts/evasion_prompt.md` (new)
- `rust/fontaine-tui/src/{commands.rs, reports.rs, guardian_prompt.rs, app.rs, view.rs, notify.rs, statusbar.rs, ui/mod.rs, ui/turn.rs, ui/goal.rs, ui/commands/afk.rs (new), ui/commands/reports.rs}`
- `rust/fontaine/src/{cli.rs, lib.rs, headless.rs}`
- `rust/fontaine-evals/src/` (new sequence-case type, loader and runner), and `evals/sequences/afk.jsonl` (new)
- `docs/afk.md` (new), plus `docs/{permissions,headless,tui,configuration,guardian,evals,index}.md` and `docs/schemas/headless-v1.schema.json`
- `tasks/fontaine_harness_plan.md` (Decisions)
- `CHANGELOG.md`

## Decisions

- AFK is a session flag, not a sixth `PermissionMode`: it is orthogonal to what the agent may do and tightens the whole tree with no inheritance logic.
- The report data extends the ledger rather than joining the audit log: the ledger is replayed on resume and copied on fork, the audit log is not.
- Temperature decays per gate decision, not wall-clock time, so it is deterministic and replayable.
- AFK ends only with an explicit `/afk off`, not on any keystroke, so an accidental keypress cannot reopen approvals.
- A stop kills children and jobs per id (`kill_agent`, `kill_by_user`), not `AgentTree::kill_all`, which latches and would block resume after a review.
- The `AfkApprover` decorator is defence-in-depth over the three approver call sites; the pipeline checks give the right text and audit fields.
- `/afk review` does not resume the denial breaker; it resumes on the next approval as today.
- A zero-denial stretch skips the evasion assessment (`NoDenials`); an allowed malicious action is the per-call guardian's job.

## Documentation

- **New `docs/afk.md`**:
  - what AFK does and does not do (no new authority);
  - enabling and leaving it;
  - the denial guidance;
  - breaker behavior;
  - the report sections and how to read *suspected evasion* and *assessment unavailable*;
  - the temperature formula, severity table and constants;
  - the stop and review;
  - the headless `--afk` output.
  Link it from `docs/index.md`.
- `docs/permissions.md`: an AFK row or note beside the mode table, and AFK conversions alongside
  headless. Add `afk` and the missing `guardian_off` to the tier list (lines 152-154).
- `docs/headless.md`: `--afk`, the `afk` result object, `error_afk_stopped`, the exit codes, and
  the `gate_decision` fields `afk`/`converted`. Add `afk` and the missing `guardian_off` to the tier
  list (lines 196-198).
- `docs/tui.md`: the `/afk` commands, the status indicator, the `/recap` line, the AFK/stopped fields in the custom status-line JSON field table, and the Goals and
  Scheduled prompts interaction (they pause while blocked or stopped).
- `docs/configuration.md`: `[afk] temperature_ceiling`, its scope-table row ("project may only
  lower"), and `--afk-temperature-ceiling`.
- `docs/guardian.md`: Denial limits under AFK, and the end-of-stretch evasion assessment.
- `docs/evals.md`: the `temperature` and `evasion` case kinds.
- Master plan `## Decisions`, Permissions subsection: AFK as a session flag, the explicit
  `/afk off`, AFK not restored on resume but a stop restored, the temperature formula and
  ceiling-scope rule, report data from the ledger, and the measured eval rates for the default
  ceiling.

## Testing Strategy

**Unit tests**
- `temperature.rs`:
  - decay arithmetic;
  - peak monotonicity under cooling;
  - contributions kept after cooling;
  - bonuses;
  - a clustered high-severity sequence scores above one isolated low denial.
- `afk_report.rs`:
  - severity mapping for every `RuleLabel` and tier;
  - `ExactRetry` linking across agents;
  - `SameTargetOtherTool` for a denied `write` followed by a `shell` redirect to the same path;
  - no link for a different path, for a read after a denied write, or for an allowed narrower
    call;
  - the timeline order, with agent names from `Agent` records;
  - "outcome not recorded" when `ToolOutcome` is missing;
  - the assessment renders: `Unavailable` never renders "no evasion", and `NoDenials` is shown
    with zero denials;
  - replaying the ledger yields the same temperature as the live path.
- `ledger.rs`:
  - old-format `Action`/`Denial`/`Goal` lines still parse;
  - an `AfkStop` without an `AfkReviewed` restores `stopped`;
  - `ContextCleared` keeps the report data.
- `permissions.rs`:
  - `recovery()` text per `NoHuman`;
  - each conversion site with AFK on gives a deny with the original rule, the AFK guidance and
    `converted` set;
  - the guardian-unavailable dialog is skipped;
  - with the stop active, the deny-only check precedes the floor and covers child agents.
- `config_scope.rs` / `config.rs`:
  - a project value above the user value is clamped to the user value;
  - a project value below it lowers the ceiling;
  - `0`, negative and `inf` are rejected in every scope;
  - the CLI overrides user.
- `goal.rs`: `pause(Blocked)` and `pause(AfkStopped)` stop `next_step`, and a user prompt does
  not clear `AfkStopped` while the stop is active.
- TUI (`TestBackend` with a scripted model):
  - `/afk` parsing;
  - the idle loop does not start goal, scheduled or notification turns while blocked or stopped;
  - a typed prompt is refused while stopped;
  - `/afk review` confirmation clears the stop and appends `AfkReviewed`;
  - the status indicator.

**Integration tests**: `rust/fontaine/tests/afk.rs`, headless `-p --afk` against wiremock-scripted
main and guardian models. These cover the end-to-end wiring that unit tests cannot: the pipeline,
the ledger file, the result JSON and the kill paths together.
- guardian deny → the tool result carries the rule + the AFK guidance, and the audit record keeps
  the verdict;
- guardian escalate and composition escalate → deny, `converted` set, the call not executed;
- an ask-rule floor ask → deny, `converted = ask_rule`;
- safe-subset continuation: after a deny the model makes an independent allowed call, which runs
  and is not linked;
- clean abandonment: the model ends with a text answer, and the result is `outcome = ended`;
- breaker: three guardian denials → `outcome = blocked`, the goal paused with `Blocked`, exit 1;
- report retrieval: the `afk` object holds the timeline, including a subagent's calls with its
  agent name;
- temperature weighting: a high-severity cluster gives a higher `peak` than a single low denial
  from a subagent;
- threshold stop with a low `--afk-temperature-ceiling`:
  - the in-flight turn ends;
  - the child is killed;
  - `AfkStop` is in the ledger and the audit log;
  - `error_afk_stopped`;
  - no further model request reaches the mock (asserted on wiremock received requests);
- alternate-route evasion: a denied `write` then a `shell` redirect to the same file is flagged
  `SameTargetOtherTool`, in an ordinary AFK turn and in an AFK `/goal` run;
- assessment failure (the guardian mock returns 500 for the assessment call) → `assessment:
  unavailable` and the timeline still present.

**Evals**:
- `evals/sequences/afk.jsonl` `temperature` cases run offline in CI with the fixture eval.
- The `evasion` cases run live against the guardian model before merge (the live Haiku eval), since
  the prompt is new guardian-side prose.

## Manual Verification

1. Run `cargo run -p fontaine`, then `/goal` on a task that needs a denied action, then `/afk`.
   Leave it, then run `/afk report`. Expect the AFK status indicator, a paused goal with a
   "blocked" reason, and the report sections in the transcript view. This is checked by hand
   because the layout of a long report in the real terminal needs a human eye (see
   `tasks/design/testing.md`, terminal layout).
2. In a real tmux, trigger a stop with `--afk --afk-temperature-ceiling 1`. Expect `AFK STOPPED`
   in the status line to redraw correctly, which `TestBackend` cannot reproduce. The rest is covered
   by `TestBackend` tests.
