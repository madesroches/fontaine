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
- `rust/fontaine-core/src/permissions.rs` builds the `GuardianUnavailable` dialog from the
  adjudication explanation, so fixing the explanation text fixes the unavailable-guardian screen.
- `docs/guardian.md` (~l.246) lists "Provider error, dropped stream, connection reset or a closed
  socket" as one row in the fail-closed table.

## Design

Add a helper in `structured.rs` that classifies a `PromptError` by whether the model was reached:

- **Not reached** — `ProviderError::Http` (transport failure: DNS, connect, TLS, timeout, reset,
  unreadable response), `ProviderError::Request`, `ProviderError::Url`, `ProviderError::Truncated`,
  `ProviderError::Stalled`, or any other variant that never carried an HTTP status from the
  provider.
- **Reached and refused** — `ProviderError::ProviderResponse`,
  `ProviderError::InvalidAuthentication`, `ProviderError::CacheExpired`,
  `ProviderError::MismatchedDimensions`, `ProviderError::UnsupportedOption`, and the matching
  `PromptError::Report` cases. These all carry a provider reply or status.
- **Tool-support refusal** — keep the existing `lacks_tool_support` special case, which is a
  reached-and-refused 400 with a specific body.

The helper returns an enum or a small struct with:
- a display-friendly classification string (`"could not be reached"` vs `"refused the request"`);
- the original error text to append;
- an optional hint, e.g. when the provider message mentions the context size being exceeded.

In `guardian.rs`, replace the `format!("the guardian model could not be reached: {error}")` arm
with a call to the helper:

- "the guardian model could not be reached: {error}" for transport failures;
- "the guardian model refused the request: {status} {body}" (or similar) for HTTP error
  responses, surfacing the status and the provider's own message when available;
- optionally append "raise `context_tokens` on the account" when the provider message indicates
  the context size was exceeded.

The `lacks_tool_support` special case stays unchanged. The `FailClosed::Error` outcome, the
`GuardianUnavailable` rule label, the audit telemetry, and the retry behavior are all unchanged.

### Context-size hint

Detect the hint from the provider response body (case-insensitive substring match for
"context size" or "context window" plus "exceed" or a status 400 that names `num_ctx` / context
window). Keep the detection conservative: only add the hint when the message clearly points at
the context window, so other 400s stay clean.

## Implementation Steps

1. `rust/fontaine-guardian/src/structured.rs`
   - Add a `pub(crate) enum ModelReachability { Unreachable, Refused { status: Option<u16>, body:
     String } }` (or equivalent) and a `classify_prompt_error(error: &PromptError) ->
     ModelReachability` helper.
   - Reuse the response extraction already present in `lacks_tool_support` to populate `status`
     and `body`.
   - Add a `context_size_exceeded(body: &str) -> bool` helper for the optional hint.

2. `rust/fontaine-guardian/src/guardian.rs`
   - In the `RunError::Failed(StructuredOutputError::Prompt(error))` arm, after the
     `lacks_tool_support` check, call `classify_prompt_error`.
   - Build "could not be reached" for `Unreachable`, "refused the request" for `Refused`,
     appending status/body and the context-size hint when detected.
   - Keep `tracing::warn!` logging the raw error so debug logs still have the full chain.

3. `rust/fontaine-guardian/tests/guardian.rs` and `tests/common/mod.rs`
   - Add a `Reply::FailWithBody(u16, String)` variant (or extend `Reply::Fail`) that returns the
     given status with a JSON body, e.g. `{"error": "... exceeds the available context size
     (2048 tokens) ..."}`.
   - Update or extend `an_endpoint_error_is_denied_as_guardian_unavailable` so that a 400 with a
     context-size body is denied as `GuardianUnavailable` / `FailClosed::Error` and its
     explanation starts with "the guardian model refused the request:" and contains the status
     and body.
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

- Classify by `ProviderError` variant rather than by HTTP status code. This matches rig's own
  taxonomy (`Http` means "no provider reply", `ProviderResponse` means "the provider replied
  with a non-success status"), so a 500 still reads as "refused the request" even though the
  server side errored. The issue's tests focus on 400, and the distinction the user cares about
  is "did the daemon answer?" not "was the status 4xx or 5xx".
- The context-size hint is a substring heuristic. It keeps the change local and avoids parsing
  every provider's error envelope, but it may miss phrasings used by providers other than
  Ollama. If missed, the explanation still shows the provider's own message, which is already
  enough to point at `context_tokens`.
- No new config key: the wording change is automatic and conservative.

## Decisions

- Use rig's `ProviderError` variant as the split, not the HTTP status family.
- Keep `FailClosed::Error` and `GuardianUnavailable` unchanged; only the human-readable
  explanation changes.
- Detect the context-size hint from the provider response body only.

## Documentation

- Update `docs/guardian.md` fail-closed table.
- Update `CHANGELOG.md`.

## Testing Strategy

- Unit tests in `rust/fontaine-guardian/tests/guardian.rs`:
  - transport failure (connection reset) keeps "could not be reached";
  - 400 with a context-size error body gets "refused the request" with status and body, and is
    still denied as `GuardianUnavailable`;
  - 400 "does not support tools" keeps the existing "could not be used" wording.
- Run `cargo test -p fontaine-guardian` and the full `cargo test` workspace gate.
- No live provider needed: wiremock simulates the HTTP responses.

## Open Questions

None.
