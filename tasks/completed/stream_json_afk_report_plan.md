# Stream-json AFK Report Event Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/428

## Overview
A headless `--input-format stream-json --output-format stream-json --afk` run pays for the
evasion assessment once, after input EOF, but no streamed line carries it: every `result` shows
`assessment.status = "pending"` and the post-loop fallback in `rust/fontaine/src/headless.rs`
(`final_afk_report`, ~line 1754) stores the assessed report in `last.afk`, which only
`--output-format json` prints. Emit the assessed `afk` object once, as a new `afk_report` event.

## Current State
- Line input's `is_exhausted()` is always false, so the in-loop review (`headless.rs` ~1708) never
  fires for a normally ending stream; only failed or goal-failed turns trigger it.
- The post-loop block sets `last.afk`; the final `match options.output_format` has
  `StreamJson => {}`.
- `EventSink` (`headless.rs` ~63) is the single emission seam; `HeadlessV1Sink` writes the
  `headless-v1` schema, the Claude Agent SDK sink writes its own shapes.
- `docs/schemas/headless-v1.schema.json` top level is a closed `oneOf` of event types; `afk` is
  already a `$defs` entry. `rust/fontaine/src/headless/afk.rs::result_object` builds the object.

## Design
- Issue option 1: new event `{"type":"afk_report","schema_version":1,"session_id":..,"afk":{..}}`,
  emitted once, after the last `result`, whenever the stream ends at EOF with AFK still active
  (the post-loop fallback; `final_afk_report` returns a report even with no denials, so the
  status may be `no_denials`). An unreadable input line ends the run earlier, with no report and
  no event. When the in-loop review already ran (single prompt, failed turn, goal
  failure), that `result` carries the assessed report and no event follows. One guardian call per
  run is unchanged.
- Add `fn afk_report(&mut self, _session_id: &str, _report: &AfkReport) {}` to `EventSink`
  (default no-op, so the SDK sink is untouched); `HeadlessV1Sink` implements it for stream output
  using `event("afk_report", session_id, json!({"afk": afk::result_object(report)}))`.
- In the post-loop block, call `sink.afk_report(...)` after `final_afk_report` returns a report
  (keep setting `last.afk` for `--output-format json`). The sink must be still in scope there.
- Schema: add an `afk_report` def (envelope + `type` const + required `afk` -> `#/$defs/afk`) and
  its `oneOf` entry.

## Implementation Steps
1. `rust/fontaine/src/headless.rs`: trait method, `HeadlessV1Sink` impl, post-loop call.
2. `docs/schemas/headless-v1.schema.json`: `afk_report` event.
3. `docs/headless.md` (event list and AFK row) and `docs/afk.md` "Headless runs": say that with
   stream-json a mid-run `result` shows `pending` when the stretch has denials, and that a stream
   ending at EOF gets the assessed report in one `afk_report` event after the last `result`,
   unless an earlier `result` already carried it.
4. `tasks/fontaine_harness_plan.md` `## Decisions` > Headless: dated line for the new event.
5. `CHANGELOG.md` entry per existing convention (the `pr` step handles it).
6. Tests in `rust/fontaine/tests/afk.rs`.

## Files to Modify
`rust/fontaine/src/headless.rs`, `docs/schemas/headless-v1.schema.json`, `docs/headless.md`,
`docs/afk.md`, `tasks/fontaine_harness_plan.md`, `rust/fontaine/tests/afk.rs`, `CHANGELOG.md`.

## Trade-offs
- A second `result` after EOF breaks consumers expecting one `result` per input message.
- Assessing after every message costs a guardian call per message (the regression #427 removed).
- Both rejected in favour of a new event type, at the cost of a schema contract addition
  (additive; `schema_version` stays 1. The `oneOf`
  is closed, so the schema file is updated in the same change).

## Decisions
- Headless: schema_version stays 1; a new event type is additive, as `usage_paused` was.
- Headless: no `afk_report` event when an earlier `result` already carried the assessed report.

## Documentation
`docs/headless.md`, `docs/afk.md`, schema file, master plan Decisions (Headless).

## Testing Strategy
In `rust/fontaine/tests/afk.rs`, a multi-message stream-json AFK integration test (none exists
today; pins #428) with a wiremock guardian: the scripted agent makes one call per message that
the adjudication route denies (a review runs only when the stretch has denials), the review
route returns `nothing_suspected()`; three user lines on stdin, then EOF. Assert every `result`
shows `pending`, exactly one `afk_report` line follows the last `result` with status
`no_suspected_evasion`, all lines validate against the schema, and the guardian endpoint saw
exactly one request matching the `REVIEW` marker. Also add an `afk_report`-is-absent assertion
to the existing single-prompt test `a_guardian_denial_carries_its_rule_...`.
