# Record the Provider Cassettes Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/106

## Overview

Four tests in `rust/fontaine/tests/cassettes.rs` are `#[ignore = "cassette not recorded"]`, so CI
never replays a full agent run. Record them, drop the ignores, and confirm they replay offline.
The issue names OpenAI `gpt-4o-mini` and `OPENAI_API_KEY`; the only key available is an
OpenRouter key, so the tests are re-pointed at OpenRouter, serving the same model.

## Current State

- All four tests build an `[accounts.paid]` of `kind = "openai"` against
  `https://api.openai.com/v1`, with `api_key_env = "OPENAI_API_KEY"` and model `gpt-4o-mini`
  (`cassettes.rs:25-35, 67-77, 132-142, 178-188`).
- `ProviderCassette::start(root, provider, scenario, upstream)` records through a local proxy
  to `upstream` and replays from `tests/cassettes/<provider>/<scenario>.yaml`. Replay matches
  request bodies, so the model id and wire format must be identical at record and replay.
- Fontaine has an `openrouter` provider kind (`fontaine-sdk/src/core/providers/openrouter.rs`,
  chat route, default endpoint `https://openrouter.ai/api/v1`).
- The image and PDF tests read the recordings back with
  `recorded_json_turns(&root(), "openai", <scenario>)`.

## Design

Switch every cassette test to OpenRouter, once, via shared constants so the four tests cannot drift:

```
const UPSTREAM: &str = "https://openrouter.ai/api/v1";
const KEY_ENV: &str = "OPENROUTER_API_KEY";
const PROVIDER: &str = "openrouter";   // cassette directory and recorded_json_turns label
const MODEL: &str = "openai/gpt-4o-mini";
```

- A helper `account_config(cassette, extra)` returns the user-config TOML:
  `kind = "openrouter"`, `base_url = cassette.base_url()`, `api_key_env = KEY_ENV`,
  `models = [MODEL]`, `[model] model = "paid/<MODEL>"`, plus `extra` (the guardian block for the auto test).
- The auto test's `guardian_model` assertion becomes `paid/openai/gpt-4o-mini`.
- Doc comment on the file names `OPENROUTER_API_KEY` and notes the key file is exported only for the
  recording command, never written anywhere.
- Fixtures land in `rust/fontaine/tests/cassettes/openrouter/`.
- The image test needs a vision-capable model; `openai/gpt-4o-mini` is. If OpenRouter rejects the
  image part, the image cassette stays ignored and a follow-up issue is filed (per the issue's handoff).

## Implementation Steps

1. Edit `cassettes.rs` as above; keep the `#[ignore]`s until recorded.
2. Record, from `rust/`, one test at a time, exporting the key only into that command's environment
   (`OPENROUTER_API_KEY="$(cat ~/open_router_key)" RIG_PROVIDER_TEST_MODE=record cargo test -p fontaine --test cassettes -- --ignored <name>`).
3. Inspect every fixture for secrets, personal paths and hostnames
   (`rg -n "sk-|Authorization|api[_-]?key|Bearer|/home/" rust/fontaine/tests/cassettes/`).
   OpenRouter responses may carry extra headers or ids; scrub or re-record if anything sensitive remains.
4. Remove `#[ignore]` on each recorded test; run `cargo test -p fontaine --test cassettes` with no key
   and no mode set.
5. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`.
6. Update the testing design doc line that says the cassettes record against OpenAI.

## Files to Modify

- `rust/fontaine/tests/cassettes.rs`
- `rust/fontaine/tests/cassettes/openrouter/*.yaml` (new)
- `tasks/design/testing.md` (provider wording around line 49)
- `CHANGELOG.md` if the repo lists test-infra changes

## Trade-offs

- **Keep the `openai` label and only swap URL/key/model**: smaller diff, but `kind = "openai"`
  against OpenRouter exercises a path production users never take, and the fixture directory would
  mislabel its origin. Using `kind = "openrouter"` records what Fontaine ships.
- **Wait for an OpenAI key**: matches the issue literally, blocks a release-blocker on a key we lack.

## Decisions

- Record through OpenRouter, because the only available key is an OpenRouter one; the issue's
  OpenAI wording is superseded for these four tests.

## Documentation

`tasks/design/testing.md` (cassette provider). No `docs/` page covers cassette recording.

## Testing Strategy

The recorded cassettes are the tests. Offline replay with no key and no `RIG_PROVIDER_TEST_MODE`
must pass; the auto cassette asserts verdict, `gate_decision` tier, audit record and final answer.

## Open Questions

- Does OpenRouter accept the base64 image part for `openai/gpt-4o-mini`? Settled by recording.
