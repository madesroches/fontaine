# Drop the HTTP status text parse on MCP connect errors Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/541

## Overview
Connect-error classification reads a status back from the `HTTP {status}: {body}` text of
`StreamableHttpError::UnexpectedServerResponse` (`classify_status_text`). The issue asks rmcp for a
typed status and then to drop that parse. The pinned rmcp already provides it, so the parse can go.

## Current State
- `rust/fontaine-tools/src/mcp/servers.rs`: `classify_http` maps `HttpStatus(e)` through
  `classify_status(e.status)` and `UnexpectedServerResponse(text)` through `classify_status_text`
  (Transient only for `HTTP 5…` / `HTTP 429…` prefixes). Unit test
  `a_status_text_is_transient_only_for_5xx_and_429` covers it.
- rmcp `=3.9.0` (`rust/Cargo.toml`) reqwest transport: `http_status_error` yields
  `HttpStatusError { status, retry_after }` for exactly 429 and 5xx, and the POST path returns it
  as `StreamableHttpError::HttpStatus` (early for non-JSON bodies and 429; after a failed JSON-RPC
  parse of the body otherwise, or when the body is oversized). The `HTTP {status}: {body}` text
  fallback is reached only for other statuses, which `classify_status_text` already classed
  Permanent. The text parse is dead for every Transient case. (rmcp's unix-socket transport still
  emits the text for 5xx, but its feature is not enabled here.)

## Design
Delete `classify_status_text` and its `UnexpectedServerResponse` arm; that variant falls to the
existing `_ => Permanent` arm of `classify_http`. Keep `classify_status`.

## Implementation Steps
1. `servers.rs`: remove the `UnexpectedServerResponse` arm in `classify_http` and the
   `classify_status_text` function with its doc comment.
2. Replace test `a_status_text_is_transient_only_for_5xx_and_429` with cases in
   `typed_http_errors_classify_as_their_variant_says`: `UnexpectedServerResponse("HTTP 503 …")` is
   Permanent (text is never read back); `HttpStatus` 429 and 500 Transient; 404 and 401 Permanent
   where not already covered.
3. Master plan: amend the 2026-11-08 (#35) Decisions line's "follow-up issue asks rmcp for a typed
   status" note to say the typed `HttpStatus` is used and the text parse is gone.

No `CHANGELOG.md` or `docs/` page mentions the text parse.

## Files to Modify
- `rust/fontaine-tools/src/mcp/servers.rs`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Decisions
- 2026-11-13: (#541) No upstream request: rmcp 3.9.0 already types 429/5xx as `HttpStatus`; the text parse is removed and `UnexpectedServerResponse` is Permanent. Keeping the parse as a safety net was rejected: it is dead code.

## Testing Strategy
Unit tests in `servers.rs` as in step 2; `cargo test -p fontaine-tools`, clippy, fmt.
