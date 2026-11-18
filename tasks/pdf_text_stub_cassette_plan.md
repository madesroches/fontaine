# Record the PDF Text-Stub Cassette Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/701

## Overview

`reading_a_pdf_replays_the_text_stub_fallback` in `rust/fontaine/tests/cassettes.rs` is the last
`#[ignore]`d cassette test. Its recording attempt through OpenRouter with `openai/gpt-4o-mini` failed
because the model called `read_mcp_resource` with `uri: notes.pdf` instead of `read`, so the "is a PDF"
stub never reached the provider. This plan steers the model to `read`, records the cassette, and
un-ignores the test so CI replays it offline.

## Current State

- `cassettes.rs` (`reading_a_pdf_replays_the_text_stub_fallback`): writes `notes.pdf` (`%PDF-1.7\nnot
  really a pdf`) into the sandbox work dir, runs headless with the bare prompt `Read notes.pdf.`,
  and asserts the requests the binary sent (through `Tap`) contain `is a PDF` and not `JVBER`.
- The sibling image test uses the prompt `Read pixel.png and tell me what you see.` and recorded
  fine (`cassettes/openrouter/read_png_image_part.yaml`), so a bare `Read <file>.` is the outlier:
  a bare filename reads like a resource URI to a small model.
- `cassettes/openrouter/` holds no `read_pdf_text_stub.yaml`; the test's `#[ignore]` reason and the
  Phase Tracker item (`tasks/fontaine_harness_plan.md`, Cassettes list, "Record the PDF text-stub
  cassette") both still describe the failed attempt.
- The test module's doc comment and the `#[ignore]` text explain the `-- --ignored` recording form.

## Design

Test-only change plus a recorded fixture; no product code changes.

1. Change the prompt so it names the tool and the workspace file, e.g. `Use the read tool to read
   notes.pdf in the working directory.` Naming the tool is the smallest steer that keeps the test
   about `read`'s PDF branch rather than about model choice.
2. If the model still picks another tool with that prompt, try a stronger prompt (the absolute
   path of the file, which the model cannot mistake for a resource URI) before changing the model.
   Only if both fail, record this one test with a different model via a dedicated constant
   (the shared `MODEL` stays) and say why in the test's doc comment.
3. Record with the existing recipe, then drop `#[ignore]`.

The assertions stay as they are: they read the bytes the binary sent, so they do not depend on what
the model replies.

## Implementation Steps

1. Edit the prompt in `reading_a_pdf_replays_the_text_stub_fallback` (`rust/fontaine/tests/cassettes.rs`).
2. From `rust/`, record (human-gated key at `~/open_router_key`, exported into this command only):
   `OPENROUTER_API_KEY="$(cat ~/open_router_key)" RIG_PROVIDER_TEST_MODE=record cargo test -p fontaine --test cassettes -- --ignored reading_a_pdf`.
   Confirm the recorded first assistant turn calls `read` (`rg -n '"name":"read"' ` in the new yaml);
   if it calls anything else, apply design step 2 and re-record.
3. Inspect the fixture for credentials and personal paths:
   `rg -n "sk-|Authorization|api[_-]?key|Bearer|/home/" rust/fontaine/tests/cassettes/openrouter/read_pdf_text_stub.yaml`.
   Sandbox `/tmp/.tmp*` paths are harmless (shape matching).
4. Remove the `#[ignore = ...]` line; update the module doc comment if it still implies an ignored
   test remains (the `-- --ignored` clause is generic and stays).
5. Replay twice with no key and no mode set: `cargo test -p fontaine --test cassettes` (fresh
   sandbox each run, so a new temp path).
6. Tick the PDF item in the Phase Tracker under Cassettes and drop its failure note
   (`tasks/fontaine_harness_plan.md`).
7. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`.

## Files to Modify

- `rust/fontaine/tests/cassettes.rs`
- `rust/fontaine/tests/cassettes/openrouter/read_pdf_text_stub.yaml` (new)
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- Prompt steer vs. changing the model: the other three cassettes share `openai/gpt-4o-mini`; a
  prompt change keeps one recording model. A per-test model is the fallback only.
- Asserting on the recorded reply instead of the wire would be weaker: shape matching lets a replay
  differ from the recording, so the `Tap` assertion is what proves the stub is sent.

## Documentation

None: no `docs/` page covers cassette recording. `CHANGELOG.md` is not needed for a test-only
change unless the maintainer's convention says otherwise; `pr` decides.

## Testing Strategy

The recorded cassette is the test: offline replay with no key must pass, asserting the wire carries
`is a PDF` and no `JVBER`. Recording is the live-provider check; it catches that the model really
routes to `read`, which no fake model could show.

## Open Questions

- Recording needs a live OpenRouter key (`~/open_router_key`) and network; if the implementer
  cannot record, the chain must report that rather than hand-write a fixture.
