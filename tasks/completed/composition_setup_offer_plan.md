# Composition Allow Screen in Auto-Mode Setup Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/372

## Overview

`fontaine auto-mode setup` and `/auto-mode-setup` collect the project's environment facts but know nothing about the
guardian composition (the `screen` preset: an allow screen in front of the LLM guardian), which today is written by hand
into `[guardian.composition]`. This adds a final, optional setup step that detects an Ollama account and a model, offers
to write the `screen` preset into the user settings file as a safe or fast posture. The user cannot be expected to
guess a threshold, so setup proposes the value benchmarked for that exact model and shows the benchmark behind it; it
only offers models that have a benchmark. The section is written to the user file only, and nothing is written unless
the user answers yes.

## Current State

- Setup flow: CLI `setup_run` in `rust/fontaine/src/auto_mode.rs`; TUI `auto_mode_setup` in
  `rust/fontaine-tui/src/ui/commands/auto_mode_setup.rs`. Both run discovery, an optional model scan with a review
  prompt (`SetupReviewPrompt`), then save through the project-environment store. Neither touches settings.
- The composition: `GuardianCompositionSection` / `GuardianComposition` in `rust/fontaine-core/src/config.rs`;
  `check_composition` (~2633) requires an Ollama-kind account for the model. `rust/fontaine-guardian/src/composition/preset.rs`
  `resolve` rejects `shadow = false` with neither `allow_threshold` nor `stop_threshold` ("there is no default").
- Writing settings: `fontaine_core::user_config::set_value(path, key_path, value)` edits the user file with `toml_edit`
  and refuses an edit that would leave the file unloadable.
- Ollama accounts: `Account` with `kind = "ollama"`; `Switcher::listings(main, only)`
  (`rust/fontaine-core/src/models.rs`) returns `Vec<AccountListing{account, kind, models: Result}>`, already fail-soft
  per account with a timeout, and is reused.
- Scope: `[guardian.composition]` is user-scope only; other scopes may only force `shadow = true`.
- Docs: `docs/guardian.md` ("Composing the guardian", "Project environment"), `docs/tui.md` (`/auto-mode-setup`).

## Design

New module `fontaine-core/src/composition_offer.rs` holds the logic shared by the CLI and the TUI, so neither
reimplements it:

```rust
pub struct Benchmark { pub tag: &'static str, pub stop_threshold: f64, pub allow_threshold: f64, pub evidence: &'static str }
pub const BENCHMARKS: &[Benchmark];                                                // one row: qwen3:4b-instruct-2507-q4_K_M

pub struct Candidate { pub account: String, pub benchmark: &'static Benchmark }   // an Ollama account holding a benchmarked model
pub enum Posture { Safe, Fast }
pub struct Offer { pub account: String, pub benchmark: &'static Benchmark, pub posture: Posture }

pub fn detect(listings: &[AccountListing]) -> Vec<Candidate>;                     // production: Switcher::listings
pub fn existing(config: &Config) -> bool;                                          // a composition is already configured
pub fn write(user_settings: &Path, config: &Config, offer: &Offer) -> io::Result<()>; // one edit of the whole table, user file only
pub fn calibration_note(offer: &Offer) -> Vec<String>;                             // the follow-up text
```

`BENCHMARKS` carries the committed fits, keyed by the exact Ollama tag (a threshold is fitted to one model, quantization,
prompt and question, so a near tag never matches): `qwen3:4b-instruct-2507-q4_K_M` with `stop_threshold = 0.0000599`
(stops the riskiest 10% of the allowed cases in the synthetic calibration corpus) and `allow_threshold = 0.00000156`
(the zero-miss threshold on that experiment), both from `docs/evals.md`'s cascade section. `evidence` is the one-line
measured result shown with the proposal.

Flow, after the environment step, in both surfaces:

1. Skip silently when a composition is already configured (`existing`), when the guardian profile is `minimal`, or
   when `detect` finds no reachable Ollama account holding a benchmarked model. In the last case print one line saying
   why, naming the benchmarked tag to pull (`ollama pull qwen3:4b-instruct-2507-q4_K_M`). The guardian model's own
   provider does not matter: the composition's `model` is checked, not the guardian's.
2. Otherwise ask one basic choice, **safe or fast**, then the account (when more than one qualifies), then confirm the
   proposed threshold with its benchmark evidence. The user types no number; declining any prompt writes nothing.
   The two postures are the screen preset's two existing shapes, with no new preset or script:
   - **safe** — `stop_threshold` only. The screen can only escalate or deny (a headless escalation is a denial); it
     never allows, so the LLM guardian reviews every call the screen did not stop and double-checks each allow.
   - **fast** — `allow_threshold` only. The screen allows what it is confident about; every call the screen does not
     clear goes to the LLM guardian, which decides it. The screen never escalates or denies in this posture.
3. `write` stores `[guardian.composition] preset = "screen"`, `model = "<account>/<model>"`, `shadow = false` and
   `[guardian.composition.params]` with the benchmark's `stop_threshold` (safe) or `allow_threshold` (fast),
   in the user settings file. `write` builds the whole `[guardian.composition]` table and writes it in one edit (an
   inline-table value or a small `user_config` helper), since `set_value` only runs `validate_settings` and several
   one-key calls could leave a partial section. Before writing it runs `check_composition` on a `Config` built with the
   section applied, so a section that would fail to load is refused.
4. Print the calibration note: the value is a fit on a small dataset and is not zero-miss on every live run (the
   fast threshold let a call through live that the dataset expects denied or escalated), so score it on your own traffic with
   `fontaine-evals --screen live:<account>/<model>` / the recorded `screen_risk` (safe) or `screen_allow_risk` (fast) audit
   values, and set `shadow = true` to log scores without acting while you tune. Safe may also stop some calls that would otherwise
   be allowed.

Non-interactive CLI runs (`stdin().is_terminal()` false, as the scan gates; `--format json` only picks the output
encoding) list candidates but never prompt and never write; the JSON report gains a `composition` object (`status`:
`skipped`/`offered_unanswered`, `candidates`) so scripts can see what was detected. `--no-model` does not affect this
step (it concerns the scan); a new `--no-composition` flag skips it entirely, including `Switcher::listings`.

The TUI adds a prompt to the existing `setup_prompt.rs`, modelled on `SetupReviewPrompt` (posture select + account
select + confirm), which needs a new `Action` variant and `App` fields in `rust/fontaine-tui/src/app.rs`
(`Action::SetupReview(bool)`, `setup_review`, `begin_setup_review` are the pattern). It skips with a message when
`Ui.user_config` is `None`, and applies nothing to the running session: a composition takes effect at the next session start, and the report says so.

## Implementation Steps

1. `composition_offer.rs` in `fontaine-core` with `detect`, `existing`, `write`, `calibration_note`, plus a
   `detect(&[AccountListing])` so tests feed fake listings; the production callers use `Switcher::listings`.
2. CLI: call the step in `setup_run` (`rust/fontaine/src/auto_mode.rs`) before `setup_report` (~line 376) is built, so
   the JSON `composition` object is filled; add a composition report struct and a numbered-choice prompt helper (the
   CLI has only the y/N `confirm()`); `--no-composition` in `cli.rs`.
3. TUI: add the prompt to the existing `setup_prompt.rs`, wired from `auto_mode_setup.rs`; add the `Action` variant
   and `App` fields in `app.rs`.
4. Append dated lines for this plan's Decisions to `tasks/fontaine_harness_plan.md` `## Decisions`, at the end of `### Guardian`
   (after the `--composition-script` entry, before `### Headless`).
5. Docs and changelog.

## Files to Modify

- `rust/fontaine-core/src/{composition_offer.rs (new),lib.rs}`
- `rust/fontaine/src/{auto_mode.rs (incl. module doc, lines 1-2),cli.rs}`
- `rust/fontaine-tui/src/ui/commands/auto_mode_setup.rs`, `rust/fontaine-tui/src/setup_prompt.rs`,
  `rust/fontaine-tui/src/app.rs`
- `docs/{guardian.md,tui.md,headless.md,configuration.md}`, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (`## Decisions`)

## Trade-offs

- **Propose benchmarked values, gated to the benchmarked model, over asking the user to type one.** A threshold is
  fitted to one model, prompt and question, so a value is only ever proposed with the exact tag it was measured on;
  any other model gets no offer rather than a guessed number. The cost is that setup helps one model until more are
  benchmarked.
- **Shared core module over per-surface logic.** One tested `write` keeps the CLI and TUI identical.

## Decisions

- 2026-11-01 (user): The written composition has `shadow = false` by default, not shadow mode as the issue text
  proposed. The `screen` preset refuses enforcement without a threshold, so setup always writes the benchmarked one.
- 2026-11-01 (user): The user cannot guess thresholds, so setup proposes the benchmarked value instead of asking for
  one. This ships a threshold in code, paired with the exact model it was fitted on, which overrides the composition
  plan's "no threshold ships" for that pairing only.
- 2026-11-01 (user): Setup offers one basic choice, safe or fast. Safe = the screen can only escalate or deny and the
  LLM guardian double-checks allows (`stop_threshold`); fast = the screen allows and the LLM guardian decides every
  call the screen does not clear (`allow_threshold`). Fast writes an `allow_threshold` (AGENTS.md fast-path exception)
  only after explicit confirmation. No other knobs (question, steering veto, concurrency) are offered.
- AFK: 2026-11-01: The setup step is skipped under the effective `minimal` profile (read from `containment::resolve`, not only the configured value): the benchmarked thresholds were fitted on full-profile reads, so a composition with them is not offered there.

## Documentation

- `docs/guardian.md`: describe the setup step, what it writes and the calibration pointer; update the setup synopsis
  (~628) and the related lines (~639, ~665); fix the statements that become false: "changes no setting" (~600-602),
  `/auto-mode-setup` "applies its result to the running session at once" (~700; a composition applies at the next
  session) and "no model or threshold ships" (~269).
- `docs/headless.md` (~24): the `setup [--no-model]` synopsis gains `--no-composition`.
- `docs/configuration.md` (~33, ~741-742): setup now also writes settings.
- `docs/tui.md`: `/auto-mode-setup` row mentions the offer and that it applies from the next session.
- `CHANGELOG.md`: entry.

## Testing Strategy

- Unit (`composition_offer`): `detect` with fake listings (none, one, several, an unreachable account, a near tag such
  as another quantization that must not match); `existing`; every `BENCHMARKS` threshold is valid for the preset;
  `write` produces a section that loads and validates for each posture (only the posture's threshold param set),
  loads through the real `Config` load path, preserves unrelated settings and never writes to a project file; `shadow = false` and the benchmark's threshold are
  what lands in the file.
- Integration (`rust/fontaine/tests/auto_mode_setup.rs`): headless/non-TTY run never writes settings and reports
  detection in JSON; `--no-composition` skips; an existing composition is left untouched.
- TUI: prompt state-machine tests like `SetupReviewPrompt`'s (decline writes nothing).
