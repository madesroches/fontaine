# Final Guardian Scorecard Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/104

## Overview

The first release publishes a guardian scorecard in `docs/evals.md` and ships the ranked list in
`rust/fontaine-sdk/data/guardian_models.toml`. Both are stale. The list's one entry was scored on a
dataset that has since changed, and the §Results text still opens with the 2026-10-23 two-model
story. This plan reruns the live scorecards on the current rules and dataset, using the
`router` (OpenRouter) and `claudemax` (claude-subscription) accounts the maintainer authorized for
this run. It lets the runner rewrite the models file and makes `docs/evals.md` §Results agree with
that file and the scorecard JSONs. Every number carries its date and dataset version.

## Current State

- **Dataset.** `dataset_version` is the first 12 hex characters of a SHA-256 over
  `evals/dataset/*.jsonl` in file-name order (`rust/fontaine-evals/src/report.rs:133`). At
  authoring time it is `578d7b4a9d1e`. The kept scorecards are on `5a79aa741c27`, `d1740a7c65ae`,
  `5daaae0b478a`, `d1aed44d7742` and `564adbac08bf` (router haiku, filtered/ad-hoc), and
  `f49709411624` / `8f149c7981df` (screens). None of them is on the current dataset.
- **Models file** (`rust/fontaine-sdk/data/guardian_models.toml`, embedded as `MODELS_TOML` in
  `rust/fontaine-sdk/src/guardian/models.rs:10`): one entry, `databricks`
  `system.ai.deepseek-v4-1-flash-1017`, scored 2026-11-16 on `5daaae0b478a`. The runner's
  `report::merge` (`report.rs:185`) drops any entry scored on another dataset version on the next
  unfiltered full run, so the first full run on `578d7b4a9d1e` removes it.
- **Runner writes the file** only on an unfiltered `--profile full` live run with no
  `infrastructure_failures` and a single transport (`runner.rs:1330` `update_models`). Interrupted
  runs leave `<out>.partial.jsonl`; `--resume <that file>` continues them.
- **`docs/evals.md` §Results** (line 590 on): the intro still names the 2026-10-23 kimi and qwen3
  runs and says the ranked list has the Databricks entry. The "Full profile" and "Catch rate per
  attack class" tables are the `5a79aa741c27` kimi/qwen3 rows, one of them flagged "to be rerun".
  "Other small and hosted guardian models" carries the `d1740a7c65ae` table, the `5daaae0b478a`
  Databricks row, and the hand-added haiku-entry history. "Minimal profile" has only the
  `5a79aa741c27` kimi row.
- **`docs/guardian.md:836-840`** names the validated list's `databricks` entry as the one
  exception to "endpoints with no catalog match use only an explicit `guardian.model`".
- **Accounts available for this run** (`~/.config/fontaine/config.toml`): `router` (openrouter;
  models `anthropic/claude-haiku-5.5`, `deepseek/deepseek-v4.1-flash-1017`, `typesafe/jev-1.14`;
  key in `~/open_router_key`, passed as `OPENROUTER_API_KEY`) and `claudemax`
  (claude-subscription, through the Claude CLI). `kimi`, `opencode-go`, `databricks` and Ollama are
  not part of this run.
- **#102** (maintainer sign-off of the dataset labels) is still open. The issue says to rerun after
  it. See Decisions.

## Design

### Runs

All runs are from `rust/` with the default `[guardian]` settings and timeout (verdict cache off by
the runner), and with `OPENROUTER_API_KEY="$(tr -d '[:space:]' < ~/open_router_key)"` exported
without printing it. Let `<v>` be the first five characters of the output of the
dataset-version command in Implementation Step 1. The file names follow the existing `<account-prefix>-<model-slug>-<v>.json`
convention. Every run is
`cargo run -p fontaine-evals --release -- --dataset ../evals/dataset --guardian <spec> --out <out>`,
with `--profile minimal` added for run 4.

| # | Guardian | Profile | `--out` |
|---|---|---|---|
| 1 | `live:router/anthropic/claude-haiku-5.5` | full | `../evals/scorecards/router-claude-haiku-5-5-<v>.json` |
| 2 | `live:router/deepseek/deepseek-v4.1-flash-1017` | full | `../evals/scorecards/router-deepseek-v4-1-flash-<v>.json` |
| 3 | `live:claudemax/haiku` | full | `../evals/scorecards/claudemax-haiku-<v>.json` |
| 4 | each model from 1-3 that cleared the bar, else run 1's model | minimal | `…-<v>-minimal.json` |

- Full runs write the shipped models file at its default path, so the runner, not a hand edit,
  decides the ranked list. Run 1 drops the stale Databricks entry (its `dropped from the models
  file` line is expected). Runs 2-3 merge into what run 1 left.
- Run 3: if the `claudemax` account does not offer `haiku` (the runner rejects the model), use the
  account's listed Haiku alias. If the account is not signed in, record that and skip the run; it
  is not a blocker.
- An interrupted run continues with `--resume <out>.partial.jsonl`. If a finished full run has
  `infrastructure_failures`, rerun the same command, with the same `--out`, guardian and profile and no filter, adding
  `--resume <out>.partial.jsonl`, before accepting the result. If they persist, keep the report and leave the file as the runner left it. **No hand-added
  entries.**
- If the dataset changes during the run (a rebase onto a `main` that touched `evals/dataset/`),
  restart the runs. Every run must share one `dataset_version`.
- After the runs, check that the `dataset_version` in the models file matches every new
  scorecard, and that the file still passes `the_shipped_file_is_a_models_array` and
  `the_shipped_model_list_parses_and_every_entry_clears_the_bar`.

### `docs/evals.md` §Results rewrite

1. **Intro.** Say which runs were made (models, date, `dataset_version`, default settings, hosted
   latency measured from the maintainer's machine through OpenRouter or the Claude CLI). State the
   ranked list exactly as the models file now holds it: the entries, or "empty, so the guardian
   falls back to your main model". State that no local model is on it. Keep the existing
   paragraph on how infrastructure failures are excluded.
2. **Full profile** table: the new rows (catch with counts, false-deny with counts, p50, p95,
   scored on, dataset), plus each model's served model where the scorecard records one. Under it,
   one line per model on which bar it cleared or missed.
3. **Catch rate per attack class**: the new rows, from each report's `catch_by_class`.
4. **Minimal profile**: new-dataset rows only, for each run-4 model, with the same columns as
   today. The `5a79aa741c27` kimi row and its paragraph move under `### Earlier datasets`, since
   kimi was not rerun.
5. **Earlier datasets.** Move the `5a79aa741c27` kimi/qwen3 full-profile and per-class tables
   (headings `#### Full profile (\`5a79aa741c27\`)` and
   `#### Catch rate per attack class (\`5a79aa741c27\`)`, so the anchors are not duplicated), the
   `d1740a7c65ae` schema-constrained section and the "Other small and hosted guardian models"
   section under one heading, `### Earlier datasets`. Each subsection already names its dataset.
   Keep the moved subsections in their current order and reword their relative references ("the
   next section", "the rows above") to name their targets.
   Remove the sentences that are no longer true: "is to be rerun", "It is the ranked list's only
   entry", and the haiku hand-added-entry paragraph (replace that with one sentence: it was on the
   list for `d1740a7c65ae` and is re-scored above). Keep the measurements themselves, since they are
   the only record for models not rerun here.
6. Anchors other pages link to (`#other-small-and-hosted-guardian-models`, `#verdict-transport`)
   stay valid. Before renaming any heading, grep `docs/` for `evals.md#` and, within `evals.md`,
   for in-page `](#` links. lychee in `.github/scripts/docs-site.sh` is what checks fragments.

### Other docs

- `docs/guardian.md:836-840`: if the Databricks entry is gone from the models file, delete the
  "except for the validated list's own `databricks` entry …" clause. If a new entry's account kind
  needs a similar note on how it is picked up, describe it from the selection code
  (`rust/fontaine-sdk/src/guardian/models.rs` and its caller), not from memory.
- `docs/configuration.md:334-339`: it says a list-less account offers the model only "when its
  `kind` matches and a credential is present" and that "no listing call is made with a guardian
  credential". If a `claude-subscription` entry lands, correct that passage for a discovering
  kind from the selection code in `rust/fontaine-sdk/src/core/models.rs`.
- `CHANGELOG.md` has only `## [Unreleased]`. Edit the unreleased #673 entries (Removed, ~lines
  292-294, haiku "dropped until it is re-scored on `5daaae0b478a`"; Added, ~lines 315-320, "The
  ranked guardian list has its first entry (#673)", Databricks) so they state the final ranked
  list and dataset, rather than appending a new entry.

## Implementation Steps

1. Compute and note the current `dataset_version`, from the repo root:
   `cat $(ls evals/dataset/*.jsonl | sort) | sha256sum | cut -c1-12` (it reproduces
   `report::dataset_version`). Build once with
   `cargo build -p fontaine-evals --release`, or use `cargo run` consistently.
2. Run 1 (router haiku, full). Inspect the summary, `infrastructure_failures` and the models file
   diff. Resume or retry infrastructure failures as in Design.
3. Runs 2 and 3, the same way.
4. Run 4: the minimal runs for the models that cleared the bar. If none cleared, run the minimal
   profile for run 1's model anyway, so that the release has a current `minimal` row.
5. Rewrite `docs/evals.md` §Results as in Design. Update `docs/guardian.md` if the entry set changed.
6. Add the `CHANGELOG.md` entry.
7. From `rust/`: `cargo test` for the whole workspace (it includes the docs test and the models
   tests) and `cargo fmt --check`. Then run the fixture
   evals from AGENTS.md (they read the dataset, not the models file, so they should be unchanged).
   From the repo root: `.github/scripts/docs-site.sh <scratch dir>` (needs `mdbook` and `lychee`)
   for the page listing and fragment checks.
8. Delete any leftover `<out>.partial.jsonl` sidecar, then commit the scorecards, models file and
   docs.

## Files to Modify

- `evals/scorecards/router-claude-haiku-5-5-<v>.json`, `router-deepseek-v4-1-flash-<v>.json`,
  `claudemax-haiku-<v>.json`, and the `-minimal` variants (new)
- `rust/fontaine-sdk/data/guardian_models.toml` (written by the runner)
- `docs/evals.md` (§Results)
- `docs/guardian.md` (the Databricks-entry clause, conditionally)
- `docs/configuration.md` (the list-less account passage, conditionally)
- `CHANGELOG.md`
- Test files named in Testing Strategy, conditionally
- Leftover `<out>.partial.jsonl` sidecars are deleted, not committed.

## Trade-offs

- **Rerun only the authorized accounts.** kimi, opencode-go, databricks and Ollama are not rerun.
  Their rows move under "Earlier datasets" with their dataset versions rather than being deleted.
  A table that mixes dataset versions would invite false comparisons, and deleting the rows would
  lose the only record of those models.
- **Same model on two providers.** Run 2 (DeepSeek on OpenRouter) gives the model the old
  Databricks entry measured a current number on an authorized account.

## Decisions

- 2026-11-26 AFK: #102 (label sign-off) is still open. Proceed with the reruns anyway. The
  maintainer authorized live runs on `router` and `claudemax` for this issue, and every number is
  pinned to its `dataset_version`, so a later label change shows up as a version mismatch that
  calls for a rerun, not as silently wrong numbers. This PR does not close #102.
- 2026-11-26 AFK: if no model clears the bar, ship the empty ranked list the runner writes and say
  so in `docs/evals.md`. Do not hand-add an entry.

- 2026-11-26: the maintainer's ruling that the guardian always runs with reasoning minimal or off made `openrouter` send `{"reasoning": {"enabled": false}}` by default (`thinking_off` opts out); the three `router` runs were redone on that default, and the OpenRouter credit gap that had cut the first DeepSeek and minimal runs short was closed by a top-up.

## Documentation

`docs/evals.md` §Results (rewrite), `docs/guardian.md` (conditional, see Design), `CHANGELOG.md`.
No plan or phase citations in `docs/`, and no `TODO`/`TBD`.

## Testing Strategy

- `cargo test` (workspace, from `rust/`) includes the docs test (`rust/fontaine-sdk/tests/docs.rs`):
  relative file links resolve (`#fragment`/`#anchor` links are skipped) and there are no
  `TODO`/`TBD`. The `SUMMARY.md`/`index.md` listing check and lychee's fragment check are in
  `.github/scripts/docs-site.sh`, which `cargo test` does not run.
- The shipped models file is checked by `the_shipped_model_list_parses_and_every_entry_clears_the_bar`
  in `rust/fontaine-sdk/tests/models.rs` (parse and bar), not only by the string check
  `the_shipped_file_is_a_models_array`, a unit test in `rust/fontaine-sdk/src/guardian/models.rs`.
- A new `openrouter` or `claude-subscription` entry changes the default guardian
  (`default_guardian`/`available_validated` in `rust/fontaine-sdk/src/core/models.rs`) for test
  accounts of that kind that have no `[guardian] model`. A discovering kind with no `models` list
  counts as offering any model (`core/models.rs:1001-1006`), and `claude_cli_double` lists `haiku`.
  When an entry of that kind lands, check and update these tests, by updating the test setup (for
  example pinning `[guardian] model`) rather than weakening the assertion:
  `rust/fontaine-sdk/tests/claude_subscription.rs` (`a_supported_cli_discovers_models_with_initialize_only`
  ~:149, `a_listed_main_model_has_no_facts_notice` ~:269), and possibly
  `rust/fontaine-tui/tests/providers.rs` (~:87, :117), `rust/fontaine-tui/tests/setup.rs` (~:356),
  `rust/fontaine/tests/cassettes.rs` (~:39) and `rust/fontaine/tests/auto_mode_setup.rs`
  (~:299-302).
- The live runs themselves are the measurement. They cannot be unit-tested, because what they
  measure is the hosted models' actual verdicts and latency on the shipped prompt. The runner's
  merge and inclusion logic is already unit-tested in `report.rs`.
- Consistency check before commit: each rate and p50/p95 in the new doc rows equals the
  corresponding `summary` field of its JSON, and the models file's entries equal the set of new
  full runs that cleared the bar.

## Open Questions

- None that block. Whether a model joins the ranked list is decided by the bar, not by a choice in
  this plan.
