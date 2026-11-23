# Update Rate-Limit Resilience Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/755

## Overview
`fontaine update` on the dev channel can fail with a bare `403 Forbidden` because it spends several unauthenticated GitHub API calls (60/hour per IP) scanning release pages. This plan cuts the dev scan to one call in the common case, lets an available GitHub token raise the quota, and makes rate-limit refusals say what happened and when the limit resets. Issue item 4 (a `latest-dev.json` asset bypassing the API) needs a release workflow change and is out of scope.

## Current State
`latest()` in `rust/fontaine/src/update/release_index.rs` walks `/releases?per_page=100&page=N` for up to `MAX_PAGES = 5` pages and keeps the highest `dev-N` across all of them. It stops only on a short page. `get()` sends no credentials and turns any non-success status into `IndexError::Status(status, url)`, displayed as `"{url} answered {status}"`. Errors map to `UpdateError::Network` in `index_error()` (`update/mod.rs`). The HTTP client is built in `build_client()` (`update/mod.rs`).

## Design
1. **Stop after the first page that has a dev build.** The endpoint is newest-first, so once a page yields at least one non-draft `dev-N`, the max over that page is the answer: break after processing it. Pages with no dev build (e.g. many release-only entries) continue to `MAX_PAGES`.
2. **Optional token.** `latest()` gains a `token: Option<&str>` parameter; `get()` adds `Authorization: Bearer <token>` when present. The caller resolves the token from `GITHUB_TOKEN`, then `GH_TOKEN` (first non-empty), via a small helper in `update/mod.rs`. The token is attached only to requests built by `get()` (API base URLs); asset downloads use a separate path and never carry it. reqwest drops `Authorization` on cross-origin redirects by default, and the token is never logged or included in errors.
3. **Clear rate-limit error.** Add `IndexError::RateLimited { url, reset: Option<SystemTime-derived value>, authenticated: bool }`. In `get()`, a 403 or 429 response with `x-ratelimit-remaining: 0` (or a 429) becomes `RateLimited`, parsing `x-ratelimit-reset` (epoch seconds). Display: `GitHub API rate limit reached for <url>; resets in N minutes (HH:MM UTC)` plus, when unauthenticated, a hint `set GITHUB_TOKEN to raise the limit`. Reset formatting is a pure function taking `now` for testability. Other 403s keep the existing `Status` message. `index_error()` maps it to `UpdateError::Network` like other fetch failures.

## Implementation Steps
1. `update/release_index.rs`: early break after a page with a dev build; token param on `latest()`/`get()`; `RateLimited` variant, header parsing, pure `describe_reset(reset_epoch, now)` helper, Display, `source()` arm.
2. `update/mod.rs` and `update/auto.rs`: add `github_token()` helper and pass it at every `latest()` call site.
3. Update `MAX_PAGES` comment to reflect the new behavior.
4. Docs: note the token env vars in `docs/installing.md` (update section).

## Files to Modify
- `rust/fontaine/src/update/release_index.rs`
- `rust/fontaine/src/update/mod.rs`
- `rust/fontaine/src/update/auto.rs` (if it calls `latest()`)
- `docs/installing.md`

## Trade-offs
- Taking the first dev page's max rather than scanning all pages assumes newest-first ordering, which the GitHub API guarantees by creation date; dev builds are created in increasing order.
- `latest-dev.json` bypass rejected for now: needs workflow changes and a new trust surface.

## Documentation
`docs/installing.md`: mention `GITHUB_TOKEN`/`GH_TOKEN` and the rate-limit message.

## Testing Strategy
Unit/wiremock tests in `release_index.rs` (wiremock is already a dev-dependency):
- Page 1 full (100) with a dev build, page 2 mounted to fail the test if hit → exactly one request, returns the page-1 max. Pins the issue's reported failure (page 2 403).
- Page 1 without dev builds, page 2 with one → continues and finds it.
- Token set → request carries `Authorization: Bearer`; unset → no Authorization header.
- 403 with `x-ratelimit-remaining: 0` and reset header → `RateLimited`; message contains reset info and the token hint when unauthenticated, no hint when authenticated. 429 likewise. A 403 without the header stays `Status`.
- `describe_reset` pure tests: future, past/now, missing header.
- Token helper: precedence and empty-value handling (take values as parameters or a closure to avoid env races).

## Open Questions
None.
