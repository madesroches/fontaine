# Stop Steering Models Onto the Shell Surface Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/358

## Overview

Two pieces of prose push models toward `shell` for searches the read-only `grep`/`glob` tools
already cover: the repo's `AGENTS.md` ("Search with `rg`") and the guardian's free-text denial
explanation, which can propose a shell one-liner as the re-issue path. Both route traffic onto the
surface the shell pre-classifier must parse. Fix the wording so models are pointed at the dedicated
tools.

## Current State

- `rust/fontaine-tools/src/builtins.rs:184,243` — `glob` and `grep` descriptions already say to use
  them instead of `find`/`grep`/`rg` in `shell`.
- `AGENTS.md:162` ("## Other") — "Search with `rg`, not `grep -r`: it skips gitignored paths and
  reads `.ignore` (which excludes `target/`)…", contradicting the tool descriptions.
- Denial text is the guardian model's own explanation (`Deny`: "say what was blocked and why in
  1-3 sentences", `rust/fontaine-guardian/prompts/guardian_prompt.md`, "What you decide"). No Rust
  code composes the remediation hint, so the suggestion of a shell command is steered only by the
  prompt, and the prompt currently says nothing about the re-issue path. The minimal profile
  prompt (`guardian_minimal.md`) shares the same Deny contract.
- `AGENTS.md` is also the model for user-facing project instructions; `docs/` was not found to
  contain advice telling the agent to search through the shell (the `rg` hits in
  `docs/permissions.md` and `docs/guardian.md` describe the classifier, not advice).

## Design

1. **AGENTS.md**: replace the `rg` line with guidance that points at the built-ins, keeping the
   `.ignore` rationale: search with the `grep` and `glob` tools, not `grep -r` or `rg` in `shell`;
   they skip gitignored paths and honor `.ignore`. Keep a short note that `rg` (not `grep -r`)
   is what to use in a shell pipeline when the tools cannot express it, so human contributors keep
   the original intent.
2. **Guardian prompts** (`guardian_prompt.md` and `guardian_minimal.md`): add one sentence to the
   Deny contract — when denying a `shell` call whose purpose is a plain file search or lookup,
   the explanation names the dedicated read-only tool (`grep`, `glob`, `read`) as the way to
   re-issue it, and never proposes another shell command. The sentence must not alter any verdict
   criteria; it only constrains explanation wording. If the prompt's Deny explanation is also
   parsed elsewhere, keep it within 1-3 sentences.
3. **Audit** AGENTS.md, `CONTRIBUTING.md`, and `docs/` for other advice that assumes a
   Bash-as-search harness (`rg`, `find`, `cat`, `head` for reading); fix any found in the same
   way.

## Implementation Steps

1. Edit `AGENTS.md` "## Other" bullet as in Design 1.
2. Audit with `rg -n "\brg\b|\bfind\b|\bgrep\b" AGENTS.md CONTRIBUTING.md docs README.md` and fix
   steering-style hits only.
3. Add the sentence to `rust/fontaine-guardian/prompts/guardian_prompt.md` and
   `guardian_minimal.md`.
4. Update the prompt snapshot (`rust/fontaine-guardian/src/snapshots/…the_defaults_marker_snapshot.snap`)
   and any other insta snapshot or prompt-hash test that embeds the prompt, via `cargo insta`
   review (or `INSTA_UPDATE`).
5. Add a `CHANGELOG.md` entry.

## Files to Modify

- `AGENTS.md`
- `rust/fontaine-guardian/prompts/guardian_prompt.md`
- `rust/fontaine-guardian/prompts/guardian_minimal.md`
- `rust/fontaine-guardian/src/snapshots/*` (prompt snapshots)
- `docs/guardian.md` (describe the denial-wording rule, if the page documents Deny explanations)
- `CHANGELOG.md`

## Trade-offs

- **Deterministic rewrite of denial text in Rust** (detect a shell search and append a hint):
  rejected — it would need to recognize a search command from shell arguments, and the project
  forbids regex matching on tool arguments. A prompt instruction keeps the guardian the only
  author of the explanation.
- **Drop the `rg` line entirely**: rejected — the `.ignore` rationale is still useful for the
  shell fallback.

## Decisions

## Documentation

`docs/guardian.md`: one line in the Deny-explanation description if one exists. No other page
changes.

## Testing Strategy

- Snapshot tests covering the prompt text are updated; `cargo test -p fontaine-guardian` passes.
- A unit test asserting both prompts contain the new re-issue sentence (a bundled-prompt test in
  `fontaine-guardian`, next to the existing prompt-builder tests).
- Live guardian eval (`cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian <live
  profile>`) before done: catch rate and false-deny rate must not regress versus the baseline;
  golden cases are not edited. Catches a prompt wording change shifting verdicts, which fixtures
  cannot show.

## Manual Verification

Not needed: the denial wording is model output and cannot be asserted deterministically; the live
eval above is the check.

## Open Questions

- Whether `docs/guardian.md` currently documents Deny explanation wording (decide at
  implementation by reading the page).
