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
  prompt, and the prompt currently says nothing about the re-issue path. The deny wrapper
  (`permissions.rs:1871`: "Do not retry it or look for another route to the same effect; tell the
  user what you could not do."), the scouting rule (`guardian_prompt.md:52`) and the system
  prompt's "# Denied tool calls" (`system_prompt.md:31-38`: a different tool is not a route around
  a denial; only a denial blaming the arguments may be corrected once) all stay as they are. The
  new sentence is advisory: the agent may still be bound by the wrapper's no-retry text, which
  says the opposite for a different-tool retry. The minimal profile
  never denies a plain file-search shell call, so only `guardian_prompt.md` changes.
- `rust/fontaine-tools/src/builtins.rs` (~589-602): the Unix `shell` tool description has no
  redirect to `read`/`glob`/`grep`; the PowerShell variant (~745-746) already has one.
  `system_prompt.md:17-26` already steers correctly.
- `AGENTS.md` is also the model for user-facing project instructions; `docs/` was not found to
  contain advice telling the agent to search through the shell (the `rg` hits in
  `docs/permissions.md` and `docs/guardian.md` describe the classifier, not advice).

## Design

1. **AGENTS.md**: replace the `rg` line with guidance that points at the built-ins, keeping the
   `.ignore` rationale: search with the `grep` and `glob` tools, not `grep -r` or `rg` in `shell`;
   they skip gitignored paths and honor `.ignore`. Keep a short note that `rg` (not `grep -r`)
   is what to use in a shell pipeline when the tools cannot express it, so human contributors keep
   the original intent.
2. **Guardian prompt** (`guardian_prompt.md`): add one sentence to the `submit` paragraph
   (`guardian_prompt.md:118`), next to the existing explanation-content rules. The explanation
   never proposes a shell command as a way forward. Only when the deny turns on the shell
   construct itself (a redirection, a compound or unparseable command) and the call's purpose is
   a plain file search or lookup may it name the dedicated read-only tool (`grep`, `glob`, `read`)
   instead. When the deny rests on the target or content a rule protects (Secret Exposure, an
   out-of-workspace path, a prior denial), the explanation names no alternative tool or route at
   all: it says what was blocked and why. This keeps the sentence consistent with the deny wrapper
   (`permissions.rs:1871`), the scouting rule (`guardian_prompt.md:52`) and the system prompt's
   "Denied tool calls". The sentence must not alter any verdict criteria; it only constrains
   explanation wording. The 1-3 sentence limit stays; nothing in product code parses the
   explanation (`verdict.rs:82` only trims). The wrapper and system prompt are unchanged.
3. **Shell tool description** (`builtins.rs`): add one sentence to the Unix `shell` description:
   use `read`/`glob`/`grep`, not `cat`/`find`/`grep`/`rg`.
4. **Audit** AGENTS.md, `CONTRIBUTING.md`, `README.md`, and `docs/` for other advice that assumes a
   Bash-as-search harness (`rg`, `find`, `cat`, `head` for reading); fix any found in the same
   way.

## Implementation Steps

1. Edit `AGENTS.md` "## Other" bullet as in Design 1.
2. Audit with `rg -n "\brg\b|\bfind\b|\bgrep\b|\bcat\b|\bhead\b" AGENTS.md CONTRIBUTING.md docs README.md` and fix
   steering-style hits only.
3. Add the sentence to `rust/fontaine-guardian/prompts/guardian_prompt.md`.
4. Add the redirect sentence to the Unix `shell` description in
   `rust/fontaine-tools/src/builtins.rs`.
5. Update the prompt snapshot
   (`rust/fontaine-guardian/src/snapshots/fontaine_guardian__prompt_builder__tests__the_defaults_marker_snapshot.snap`)
   via `cargo insta` review (or `INSTA_UPDATE`).
6. Add a `CHANGELOG.md` entry.

## Files to Modify

- `AGENTS.md`
- `rust/fontaine-guardian/prompts/guardian_prompt.md`
- `rust/fontaine-tools/src/builtins.rs`
- `rust/fontaine-guardian/src/snapshots/fontaine_guardian__prompt_builder__tests__the_defaults_marker_snapshot.snap`
- `docs/guardian.md` (see Documentation)
- `docs/tools.md` (see Documentation)
- `CHANGELOG.md`

## Trade-offs

- **Deterministic rewrite of denial text in Rust** (detect a shell search and append a hint):
  rejected — it would need to recognize a search command from shell arguments, and the project
  forbids regex matching on tool arguments. A prompt instruction keeps the guardian the only
  author of the explanation.
- **Change the `permissions.rs:1871` wrapper** to allow a re-issue hint: rejected — it would
  loosen the no-other-route rule for every deny reason.
- **Drop the `rg` line entirely**: rejected — the `.ignore` rationale is still useful for the
  shell fallback.

## Documentation

`docs/guardian.md`: in "The verdict" (near lines 114-115 and 168), add the new rule: a deny
never proposes a shell command, and names a dedicated read-only tool only when the deny turns on
the shell construct itself (redirection, compound or unparseable command), never when it rests on
the target or content a rule protects. Correct line 168's
wrapper description: a deny reaches the agent as `Denied by guardian: [Rule] explanation. Do not
retry it or look for another route to the same effect; tell the user what you could not do.`
(`permissions.rs:1871`).

`docs/tools.md` (near lines 45-46): the note that the system prompt steers away from shell search also
holds for the Unix shell tool description, so say that the shell tool description steers too.

## Testing Strategy

- Snapshot tests covering the prompt text are updated; `cargo test -p fontaine-guardian` passes.
- Live guardian eval (`cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian
  live:<account>/<model>`, e.g. `live:work/claude-haiku-5-5`) before done. An unfiltered
  `--profile full` live run rewrites `rust/fontaine-guardian/data/guardian_models.toml`; discard
  that file (`git checkout`) unless the prompt change is meant to regenerate it. Pass criteria: catch rate and false-deny rate must not regress versus the baseline;
  golden cases are not edited. Catches a prompt wording change shifting verdicts, which fixtures
  cannot show. It guards only against verdict regression: the eval scores verdicts and never
  reads explanation wording.

## Manual Verification

Spot check: read the explanations of the shell-search deny cases in the live eval output and
confirm none proposes a shell command and that none of the Secret Exposure or out-of-workspace
denies names an alternative tool. This is not automated because explanation text is model
output and cannot be asserted deterministically.

## Decisions

- AFK: guardian-prompt sentence limited to never proposing a shell command and naming a read-only tool only when the deny turns on the shell construct itself (not on a protected target or content); permissions.rs:1871 wrapper unchanged, no deny loosened (most conservative option, chosen without the user).
