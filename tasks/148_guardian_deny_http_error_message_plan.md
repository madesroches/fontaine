# Guardian Deny Says "Could Not Be Reached" for HTTP Errors Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/148

## Overview

When the guardian model call fails, the fail-closed explanation always says the guardian model
"could not be reached". That wording is accurate for transport failures (DNS, TLS, connect,
timeout, reset), but misleading when the daemon answered and refused the request — for example
an Ollama 400 because `context_tokens` exceeds the configured context window, or a 400 because
the model rejects forced tool calls. This plan changes only the explanation text; the decision
stays a fail-closed `FailClosed::Error` / rule `GuardianUnavailable`.

## Current State

- `rust/fontaine-guardian/src/guardian.rs` (~l.345): after `structured::run::<Verdict>` returns
  `RunError::Failed(StructuredOutputError::Prompt(error))`, the explanation is built as either:
  - "the guardian model could not be used: this model does not support tool calls..." when
    `lacks_tool_support(&error)` is true;
  - "the guardian model could not be reached: {error}" for every other `PromptError`.
- `rust/fontaine-guardian/src/structured.rs` (~l.335): `lacks_tool_support` already inspects the
  typed `ProviderError::ProviderResponse` / `PromptError::Report` response to detect a 400 whose
  body contains "does not support tools". The same structured response data is available for the
  new classification.
- `rust/fontaine-guardian/src/screen.rs` already distinguishes a transport failure
  (`request failed: {chain}`) from an HTTP error response (`the reader/screen model answered HTTP
  {status}: {text}`). The guardian path should apply the same split.
- `rust/fontaine-guardian/tests/guardian.rs` already checks that a connection reset is denied
  with wording containing `could not be reached: `, and that a 500 endpoint error is denied as
  `GuardianUnavailable` with `could not be reached`. The issue asks for a new 400-with-body test
  that gets different wording while still being denied.
- `rust/fontaine-core/src/permissions.rs` surfaces the adjudication explanation in the guardian
  denial message (`Denied by guardian: [GuardianUnavailable] {explanation}`), so fixing the
  explanation text fixes that message. The `GuardianUnavailable` dialog itself is built from
  `unavailable_reason()` (backend unavailability), not from the adjudication explanation.
- `docs/guardian.md` (~l.246) lists "Provider error, dropped stream, connection reset or a closed
  socket" as one row in the fail-closed table.

## Design

Add a helper in `structured.rs` that classifies a `PromptError` by whether the model was
reached. Every current `ProviderError` variant is classified explicitly by whether the daemon
answered; future variants fall back conservatively (the enum is `#[non_exhaustive]`):

- **Not reached** — `ProviderError::Http` (transport failure: DNS, connect, TLS, timeout, reset,
  unreadable response), `ProviderError::Json`, `ProviderError::Url`, `ProviderError::Request`,
  `ProviderError::Provider`, `ProviderError::UnsupportedOption`, `ProviderError::Truncated`, and
  `ProviderError::Stalled`: none of these carried an HTTP status from the provider.
- **Reached and refused** — `ProviderError::ProviderResponse`,
  `ProviderError::InvalidAuthentication`, and `ProviderError::CacheExpired` preserve the
  provider's reply, so the explanation surfaces its status and body;
  `ProviderError::Response` and `ProviderError::MismatchedDimensions` also mean the daemon
  answered, but carry no preserved status/body, so the explanation is formatted from the error's
  own message. `ProviderError::Relayed` reports as its inner `ErrorReport` and flattens into
  `PromptError::Report` (rig-agent's `#[from]` conversion), so it is covered by the report rule
  below: reached when that report carries `http_status` or `provider_response`, unreachable
  otherwise. `PromptError::Report` counts as reached only when the report carries
  `http_status` or `provider_response`; a report with neither is treated as unreachable.
- **Fallback** — any future `ProviderError` variant classifies as not reached: claim a refusal
  only when a provider reply is certain.
- **Tool-support refusal** — keep the existing `lacks_tool_support` special case, which is a
  reached-and-refused 400 with a specific body.

The helper returns an enum or a small struct with:
- a display-friendly classification string (`"could not be reached"` vs `"refused the request"`);
- the original error text to append.

In `guardian.rs`, replace the `format!("the guardian model could not be reached: {error}")` arm
with a call to the helper:

- "the guardian model could not be reached: {error}" for transport failures;
- "the guardian model refused the request: {status} {body}" (or similar) for HTTP error
  responses, surfacing the status and the provider's own message when available; reached
  variants with no preserved status/body use the error's own message instead.

The `lacks_tool_support` special case stays unchanged. The `FailClosed::Error` outcome, the
`GuardianUnavailable` rule label, the audit telemetry, and the retry behavior are all unchanged.

## Implementation Steps

1. `rust/fontaine-guardian/src/structured.rs`
   - Add a `pub(crate) enum ModelReachability { Unreachable, Refused { status: Option<u16>, body:
     String } }` (or equivalent) and a `classify_prompt_error(error: &PromptError) ->
     ModelReachability` helper.
   - Reuse the response extraction already present in `lacks_tool_support` to populate `status`
     and `body`.

2. `rust/fontaine-guardian/src/guardian.rs`
   - In the `RunError::Failed(StructuredOutputError::Prompt(error))` arm, after the
     `lacks_tool_support` check, call `classify_prompt_error`.
   - Build "could not be reached" for `Unreachable`, "refused the request" for `Refused`,
     appending status/body when available.
   - Keep `tracing::warn!` logging the raw error so debug logs still have the full chain.

3. `rust/fontaine-guardian/tests/guardian.rs` and `tests/common/mod.rs`
   - Add a `Reply::FailWithBody(u16, String)` variant (or extend `Reply::Fail`) that returns the
     given status with a JSON body, e.g. `{"error": "... exceeds the available context size
     (2048 tokens) ..."}`.
   - Update or extend `an_endpoint_error_is_denied_as_guardian_unavailable` so that a 400 with a
     context-size body is denied as `GuardianUnavailable` / `FailClosed::Error` and its
     explanation starts with "the guardian model refused the request:" and contains the status
     and body.
   - Update the existing 500 endpoint-error case in that test to expect "the guardian model
     refused the request:" instead of "could not be reached" (a 500 is an answered HTTP
     response, so the reached/refused split changes its wording), or split it into separate 400
     and 500 tests.
   - Keep the existing connection-reset test asserting `could not be reached`.
   - Keep the call still denied (decision `Deny`, rule `GuardianUnavailable`).

4. `docs/guardian.md`
   - Split the "Provider error, dropped stream..." table row into two rows:
     - transport failure / dropped stream / reset / closed socket → `error`;
     - provider HTTP error response (e.g. 400 context too large, 401/403 auth) → `error`.
   - Keep the result `deny, rule GuardianUnavailable` for both.

5. `CHANGELOG.md`
   - Add an entry under `### Fixed` describing the clearer guardian-deny wording for provider
     HTTP refusals.

## Files to Modify

- `rust/fontaine-guardian/src/structured.rs`
- `rust/fontaine-guardian/src/guardian.rs`
- `rust/fontaine-guardian/tests/guardian.rs`
- `rust/fontaine-guardian/tests/common/mod.rs`
- `docs/guardian.md`
- `CHANGELOG.md`

## Trade-offs

- No new config key: the wording change is automatic and conservative.

## Decisions

- Use rig's `ProviderError` variant as the split, not the HTTP status family.
- Keep `FailClosed::Error` and `GuardianUnavailable` unchanged; only the human-readable
  explanation changes.

## Documentation

- Update `docs/guardian.md` fail-closed table.
- Update `CHANGELOG.md`.

## Testing Strategy

- Unit tests in `rust/fontaine-guardian/tests/guardian.rs`:
  - transport failure (connection reset) keeps "could not be reached";
  - 400 with a context-size error body gets "refused the request" with status and body, and is
    still denied as `GuardianUnavailable`;
  - 500 endpoint error is denied as `GuardianUnavailable` with an explanation starting with
    "the guardian model refused the request:";
  - 401/403 auth refusal gets an explanation starting with "the guardian model refused the
    request:", and is still denied (`Deny` / `GuardianUnavailable`);
  - 400 "does not support tools" keeps the existing "could not be used" wording.
- Run `cargo test -p fontaine-guardian` and the full `cargo test` workspace gate.
- No live provider needed: wiremock simulates the HTTP responses.

## Open Questions

None.
