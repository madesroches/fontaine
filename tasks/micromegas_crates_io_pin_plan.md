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
- `rust/Cargo.lock` records the six micromegas crates (the three direct ones and their siblings) from `git+https://…micromegas?rev=…`.
- Consumers (`rust/fontaine/Cargo.toml:39-40,60-61`) use `workspace = true`, so they need no change.
- Doc-trail mentions: `tasks/fontaine_harness_plan.md:739-740` (Decisions), and
  `tasks/design/research-findings.md:250-252` (dependency list) and `:259-264` (caveat naming the git
  revision as unreleased).
- crates.io has `micromegas-tracing`, `-telemetry-sink` and `-transit` at 0.33.0. The upstream
  `v0.33.0` tags contain `7a08f6541`, so the builder switch (`with_cpu_tracing_enabled`), reqwest 0.13
  and `TracingCaptureLayer` span capture the git pin was taken for are in the release.

## Design
Manifest, lockfile and policy, plus one field in two `TracingCaptureLayer` literals:

1. `rust/Cargo.toml`: three entries become `micromegas-… = "=0.33.0"`; remove the "Git pin" comment.
2. `rust/deny.toml`: delete the `allow-git` line and its comment. `unknown-git = "deny"` then holds
   for every dependency.
3. `cargo update -p micromegas-tracing -p micromegas-telemetry-sink -p micromegas-transit`
   rewrites the lock entries to the registry source (`-p` keeps every other version stable). If the
   release's transitive requirements differ from the git revision (e.g. sibling crates such as
   `micromegas-tracing-proc-macros`, `micromegas-telemetry`), they move with it; review the lock diff to
   confirm that only micromegas crates and their direct transitive dependencies changed. The release
   also carries upstream changes since the git rev (source-path trimming, 429 retry, a dependency
   bump), so check the telemetry tests.
4. Docs trail: keep the dated 2026-10-24 Decisions lines at `tasks/fontaine_harness_plan.md:739-740`
   and mark each "(Superseded by …)"; append one new 2026-11-07 line at the end of the Observability
   subsection recording the pin and the `allow-git` removal, and the corrected span-bridge rationale
   (0.33.0 has the field hook, `span_field_hook`, which Fontaine does not adopt, so the `SpanBridge`
   span half stays; source paths are trimmed rather than absolute; only a switch to omit locations is
   still missing). In `tasks/design/research-findings.md`, reword `:250-252` to say the exact
   crates.io `=0.33.0`, and rewrite the caveat at `:259-264` to refer to the 0.33.0 release instead of
   "that revision" and to state the same corrected span-bridge rationale.
5. Published 0.33.0 `TracingCaptureLayer` has a third public field, `span_field_hook:
   Option<SpanFieldHook>` (the git rev has only `max_level` and `capture_spans`). Set
   `span_field_hook: None` in the struct literals at `rust/fontaine/src/telemetry/micromegas.rs:49-52`
   and `rust/fontaine/tests/telemetry_micromegas.rs:496`. Adopting the hook is out of scope. Also
   correct the module doc comment at `rust/fontaine/src/telemetry/micromegas.rs:1-2`, which says the
   stock layer exports absolute source paths and every field verbatim, to the 0.33.0 behavior above.

## Implementation Steps
1. Edit `rust/Cargo.toml` and `rust/deny.toml` as above.
2. Run `cargo update -p …` from `rust/`; inspect `git diff rust/Cargo.lock`.
3. Add `span_field_hook: None` to the two `TracingCaptureLayer` literals.
4. Edit the two task docs (including the `micromegas.rs` module doc comment); scan `docs/` and `README.md` for git-pin mentions (none found by grep).
5. Edit the existing `CHANGELOG.md` [Unreleased] line ("pinned to a git commit until 0.33 is published") to say crates.io 0.33.0.
6. Run the verification below.

## Files to Modify
- `rust/Cargo.toml`, `rust/Cargo.lock`, `rust/deny.toml`
- `rust/fontaine/src/telemetry/micromegas.rs`, `rust/fontaine/tests/telemetry_micromegas.rs`
- `tasks/fontaine_harness_plan.md`, `tasks/design/research-findings.md`
- `CHANGELOG.md`

## Trade-offs
- Exact `=` pin rather than caret: matches how every other pre-1.0 dependency here is pinned.
- Pin `0.33.0` rather than a later 0.33.x: it is the only 0.33 release and contains the needed revision.

## Documentation
No `docs/` page mentions the git pin. Task docs and CHANGELOG as listed above.

## Testing Strategy
The change adds no new logic, so no new tests. Existing checks cover it:
- `cargo deny check` passes with `allow-git` removed (catches any remaining git source).
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — the telemetry
  tests (`telemetry_micromegas` in `rust/fontaine/Cargo.toml:73`) confirm Fontaine works against
  the release.
- `cargo check --features fontaine/pg2` and the Windows clippy check from AGENTS.md still pass.

## Open Questions
None.
