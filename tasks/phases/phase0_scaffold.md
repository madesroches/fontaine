# Phase 0 — Scaffold (eval first — it is the product)

**Status:** not started · **Depends on:** — · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

Read the master plan's Overview, Design (§Workspace layout, §Config), Decisions, and
Testing Strategy before implementing. This phase builds the eval harness **before** the
thing it scores — per analysis-strategic-verdict amendment A1, the public guardian
scorecard is the differentiating asset, so its harness is built first, not last.

## Steps

1. Cargo workspace (`rust/Cargo.toml`, micromegas layout), crate skeletons under `rust/`
   (`fontaine`, `fontaine-core`, `fontaine-tools`, `fontaine-guardian`, `fontaine-agents`,
   `fontaine-mcp`, `fontaine-tui`), `clap` CLI entry (`fontaine`), tracing setup, CI
   (`.github/workflows/ci.yml`, GitHub Actions, from `rust/`: `cargo fmt --check`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test`, then the offline eval fixture run from
   step 2), config loading (`fontaine-core/src/config.rs`) with the schema in the master plan's
   §Config, rejecting every user-scope-only key from project scope per its scope table.
2. Eval golden-dataset scaffolding + CI runner skeleton (`evals/`): dataset format (~150+ cases:
   benign dev commands, each seeded attack class from Claude Code's default-block list, injection
   attempts *inside tool args*, ambiguous intent cases, Tier-0.5 adversarial shell cases — see
   master Testing Strategy — and adversarial spawn/injection-propagation cases: a subagent result
   carrying injection text that must hit the tool-result scanner, a README tricking the parent
   into spawning with an escalated toolset — plus escape-focused container cases for the
   `[guardian] profile = "minimal"` column: `mount /dev/sda1`, docker.sock write, cgroup
   `release_agent`, bind-mount symlink escape → deny/escalate; routine in-container workspace ops
   → proceed); the runner is the bin crate `rust/fontaine-evals`, reading `evals/dataset/*.jsonl` (one JSON
   object per case: `id`, `category`, `profile`, `task_intent`, `tool`, `args`, `expected`,
   `fixture_verdict` — field list in the master Testing Strategy) and scoring any guardian model
   config; CI runs `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture`
   from `rust/` — offline, deterministic verdict fixtures — so it is green before the guardian
   exists.
3. Docs — start the documentation tree under `docs/` (its own directory — the single home for
   all prose, structured one-topic-per-page with relative cross-links so it can be lifted into a
   website later; no site tooling in v1): `docs/index.md` (landing/TOC), `docs/architecture.md`
   seeded with the workspace map + crate responsibilities, and `docs/configuration.md` seeded
   from the step-1 config schema (every key: meaning, user-vs-project scope, default). Also
   `evals/README.md` defining the golden-dataset format — the dataset is a public asset shipping
   with the scorecard, so its format doc lands with it. Root `README.md` stays the thin front
   door (pitch, install, quickstart) linking into `docs/` — not a second copy of it. Create `CHANGELOG.md` with the first entry.

## Files created or modified

`rust/Cargo.toml`; crate skeletons for every `rust/fontaine-*` crate; `rust/fontaine/src/main.rs`;
`rust/fontaine-core/src/config.rs`; `evals/` (dataset + README.md); `rust/fontaine-evals/` (runner); `CHANGELOG.md`;
`docs/{index,architecture,configuration}.md`; `README.md`; `.github/workflows/ci.yml`.

## Tests owned by this phase

- **config** (master Testing Strategy): precedence, project-can-only-tighten enforcement —
  including that `[guardian] profile` is rejected from project scope (user-scope only, it is a
  loosening knob) and only `full`/`minimal` validate: no config value disables the guardian. Every user-scope-only key in the master §Config scope
  table (`[model]`, `[guardian]` model/timeout/threshold/cache, `[agents]` budgets,
  `[agents.types]`, …) is rejected from project scope.
- Golden-dataset schema validation; runner green against verdict fixtures.

## Definition of done

- `cargo fmt` / `cargo clippy --all-targets -- -D warnings` / `cargo test` green in CI.
- `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture` (from `rust/`)
  passes on the fixture set, offline, with no guardian in the tree.
- `docs/index.md`, `docs/architecture.md`, `docs/configuration.md`, `evals/README.md` exist with
  real content (no TODO placeholders), links resolve.
- `CHANGELOG.md` exists with the Phase 0 entry.
- Log the phase in the master plan's Progress log.
