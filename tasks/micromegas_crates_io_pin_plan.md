# Pin micromegas to crates.io 0.33.0 Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/141

## Overview
Replace the three git dependencies on micromegas (`7a08f6541`) with an exact crates.io pin
`=0.33.0` and drop `allow-git` from `deny.toml`. micromegas 0.33.0 is now published, and the git
pin was never to ship in a release.

## Current State
- `rust/Cargo.toml:56-59`: `micromegas-tracing`, `micromegas-telemetry-sink` and
  `micromegas-transit` are `git` + `rev = 7a08f654…` workspace dependencies.
- `rust/deny.toml:44-45`: `allow-git` lists the micromegas repository; `unknown-git = "deny"`.
- `rust/Cargo.lock` records the three crates from `git+https://…micromegas?rev=…`.
- Consumers (`rust/fontaine/Cargo.toml:39-40,60-61`) use `workspace = true`, so they need no change.
- Doc-trail mentions: `tasks/fontaine_harness_plan.md:740` (Decisions) and
  `tasks/design/research-findings.md:259-261` (caveat naming the git revision as unreleased).
- crates.io has `micromegas-tracing`, `-telemetry-sink` and `-transit` at 0.33.0. The upstream
  `v0.33.0` tags contain `7a08f6541`, so the builder switch (`with_cpu_tracing_enabled`), reqwest 0.13
  and `TracingCaptureLayer` span capture the git pin was taken for are in the release.

## Design
No code change. Manifest, lockfile and policy only:

1. `rust/Cargo.toml`: three entries become `micromegas-… = "=0.33.0"`; remove the "Git pin" comment.
2. `rust/deny.toml`: delete the `allow-git` line and its comment. `unknown-git = "deny"` then holds
   for every dependency.
3. `cargo update -p micromegas-tracing -p micromegas-telemetry-sink -p micromegas-transit`
   rewrites the lock entries to the registry source (`-p` keeps every other version stable). If the
   release's transitive requirements differ from the git revision (e.g. sibling crates such as
   `micromegas-proc-macros`, `micromegas-derive-transit`), they move with it; review the lock diff to
   confirm that only micromegas crates and their direct transitive dependencies changed.
4. Docs trail: update the Decisions line in `tasks/fontaine_harness_plan.md:740` and the caveat in
   `tasks/design/research-findings.md:259-261` to say the pin is the exact crates.io `=0.33.0`
   (the git-pin description is now history and belongs in the CHANGELOG, not in current-state docs).

## Implementation Steps
1. Edit `rust/Cargo.toml` and `rust/deny.toml` as above.
2. Run `cargo update -p …` from `rust/`; inspect `git diff rust/Cargo.lock`.
3. Edit the two task docs; scan `docs/` and `README.md` for git-pin mentions (none found by grep).
4. Add a `CHANGELOG.md` entry under the unreleased section.
5. Run the verification below.

## Files to Modify
- `rust/Cargo.toml`, `rust/Cargo.lock`, `rust/deny.toml`
- `tasks/fontaine_harness_plan.md`, `tasks/design/research-findings.md`
- `CHANGELOG.md`

## Trade-offs
- Exact `=` pin rather than caret: matches how every other pre-1.0 dependency here is pinned.
- Pin `0.33.0` rather than a later 0.33.x: it is the only 0.33 release and contains the needed revision.

## Documentation
No `docs/` page mentions the git pin. Task docs and CHANGELOG as listed above.

## Testing Strategy
The change has no new logic, so no new tests. Existing checks cover it:
- `cargo deny check` passes with `allow-git` removed (catches any remaining git source).
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — the telemetry
  tests (`telemetry_micromegas` in `rust/fontaine/Cargo.toml:73`) prove the released crates expose
  the same API as the git revision.
- `cargo check --features fontaine/pg2` and the Windows clippy check from AGENTS.md still pass.
- `grep -n "git+" rust/Cargo.lock` shows no micromegas git source.

## Open Questions
None.
