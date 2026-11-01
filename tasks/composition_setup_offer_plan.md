# Composition Allow Screen in Auto-Mode Setup Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/372

## Overview

`fontaine auto-mode setup` and `/auto-mode-setup` collect the project's environment facts but know nothing about the
guardian composition (the `screen` preset: an allow screen in front of the LLM guardian), which today is written by hand
into `[guardian.composition]`. This adds a final, optional setup step that detects an Ollama account and a model, offers
to write the `screen` preset into the user settings file, and points the user at calibrating `allow_threshold` from the
recorded scores. Constraints carried over from the composition plan: no threshold ships, the section is written to the
user file only, and nothing is written unless the user answers yes.

## Current State

- Setup flow: CLI `setup_run` in `rust/fontaine/src/auto_mode.rs`; TUI `auto_mode_setup` in
  `rust/fontaine-tui/src/ui/commands/auto_mode_setup.rs`. Both run discovery, an optional model scan with a review
  prompt (`SetupReviewPrompt`), then save through the project-environment store. Neither touches settings.
- The composition: `GuardianCompositionSection` / `GuardianComposition` in `rust/fontaine-core/src/config.rs`;
  `check_composition` (~2633) requires an Ollama-kind account for the model. `rust/fontaine-guardian/src/composition/preset.rs`
  `resolve` rejects `shadow = false` with neither `allow_threshold` nor `stop_threshold` ("there is no default").
- Writing settings: `fontaine_core::user_config::set_value(path, key_path, value)` edits the user file with `toml_edit`
  and refuses an edit that would leave the file unloadable.
- Ollama accounts: `Account` with `kind = "ollama"`; a live model listing exists (`/api/tags`, used by the guardian
  picker) and should be reused.
- Scope: `[guardian.composition]` is user-scope only; other scopes may only force `shadow = true`.
- Docs: `docs/guardian.md` ("Composing the guardian", "Project environment"), `docs/tui.md` (`/auto-mode-setup`).

## Design

New module `fontaine-core/src/composition_offer.rs` holds the logic shared by the CLI and the TUI, so neither
reimplements it:

```rust
pub struct Candidate { pub account: String, pub models: Vec<String> }   // reachable Ollama accounts and their models
pub struct Offer { pub model: String /* account/model */, pub allow_threshold: f64 }

pub fn detect(config: &Config, listing: &dyn ModelListing) -> Vec<Candidate>;     // fail-soft per account
pub fn existing(config: &Config) -> bool;                                          // a composition is already configured
pub fn write(user_settings: &Path, offer: &Offer) -> io::Result<()>;              // set_value calls, user file only
pub fn calibration_note(offer: &Offer) -> Vec<String>;                             // the follow-up text
```

Flow, after the environment step, in both surfaces:

1. Skip silently when a composition is already configured (`existing`), when the guardian profile is `minimal`, or
   when `detect` finds no reachable Ollama account with a model. In the last case print one line saying why the step
   was skipped.
2. Otherwise ask: pick account/model (default: the only candidate), then ask for an `allow_threshold` — a number in
   (0, 1) typed by the user. **No value is suggested or defaulted.** Declining either prompt writes nothing.
3. `write` stores `[guardian.composition] preset = "screen"`, `model = "<account>/<model>"`, `shadow = false` and
   `[guardian.composition.params] allow_threshold = <typed>` in the user settings file. It is validated through the
   same `check_composition` path before writing, so a section that would fail to load is refused.
4. Print the calibration note: score the threshold on your own traffic with `fontaine-evals --screen live:<model>`
   / the recorded `screen_allow_risk` audit values, and set `shadow = true` to log scores without acting while you
   tune. Nothing in the note proposes a threshold value.

Non-interactive CLI runs (no TTY, `--format json`) never prompt and never write; the JSON report gains a
`composition` object (`status`: `skipped`/`offered_unanswered`, `candidates`) so scripts can see what was detected.
`--no-model` does not affect this step (it concerns the scan); a new `--no-composition` flag skips it.

The TUI uses a small prompt component modelled on `SetupReviewPrompt` (select + number entry) and applies nothing to
the running session: a composition takes effect at the next session start, and the report says so.

## Implementation Steps

1. `composition_offer.rs` in `fontaine-core` with `detect`, `existing`, `write`, `calibration_note`, plus a
   `ModelListing` trait so tests feed a fake listing (reuse the picker's real implementation in production).
2. CLI: call the step at the end of `setup_run` (`rust/fontaine/src/auto_mode.rs`), `--no-composition` in `cli.rs`,
   JSON report field.
3. TUI: prompt component in `fontaine-tui`, wired from `auto_mode_setup.rs`.
4. Docs and changelog.

## Files to Modify

- `rust/fontaine-core/src/{composition_offer.rs (new),lib.rs}`
- `rust/fontaine/src/{auto_mode.rs,cli.rs}`
- `rust/fontaine-tui/src/ui/commands/auto_mode_setup.rs`, `rust/fontaine-tui/src/setup_prompt.rs`
- `docs/{guardian.md,tui.md}`, `CHANGELOG.md`

## Trade-offs

- **Ask for the threshold, not ship one.** A threshold is fitted to one model, prompt and question, so none is
  suggested. The cost is a prompt the user may not know how to answer; the calibration note and docs cover it.
- **Shared core module over per-surface logic.** One tested `write` keeps the CLI and TUI identical.

## Decisions

- 2026-11-01 (user): The written composition has `shadow = false` by default, not shadow mode as the issue text
  proposed. Because the `screen` preset refuses enforcement without a threshold, setup requires the user to type an
  `allow_threshold`; with none given, nothing is written.

## Documentation

- `docs/guardian.md`: describe the setup step, what it writes and the calibration pointer.
- `docs/tui.md`: `/auto-mode-setup` row mentions the offer and that it applies from the next session.
- `CHANGELOG.md`: entry.

## Testing Strategy

- Unit (`composition_offer`): `detect` with fake listings (none, one, several, an unreachable account); `existing`;
  `write` produces a section that loads and validates, preserves unrelated settings, rejects thresholds outside (0, 1),
  and never writes to a project file; `shadow = false` and the typed threshold are what lands in the file.
- Integration (`rust/fontaine/tests/auto_mode_setup.rs`): headless/non-TTY run never writes settings and reports
  detection in JSON; `--no-composition` skips; an existing composition is left untouched.
- TUI: prompt state-machine tests like `SetupReviewPrompt`'s (decline writes nothing; invalid number re-asks).

## Open Questions

- Should the step also be offered when the guardian model itself is not on an Ollama account? Assumed no: the
  composition's reads require an Ollama account, so the step is skipped with a note.
