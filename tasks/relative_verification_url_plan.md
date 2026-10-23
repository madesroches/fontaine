# Relative Verification URL Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/118

## Overview

`fontaine auth login <account>` on an `opencode-go` account always fails with "the sign-in server
sent an unusable reply: the verification URL is not a URL". The OpenCode Console answers the
device-code request with relative verification URIs (`/console/device?user_code=…`), and the device
flow only accepts absolute URLs. Resolve the verification URI as a URI reference against the
device-code endpoint it came from, then apply the existing `http(s)` check, so Go sign-in works.

## Current State

- `DeviceFlow::start` (`rust/fontaine-core/src/providers/device_flow.rs:168`) posts to
  `endpoints.code_url`, takes `verification_uri_complete` or else `verification_uri`, and runs
  `Url::parse` on it (`device_flow.rs:181`). A relative reference fails there with
  `FlowError::Invalid("the verification URL is not a URL")`. The scheme check (`http`/`https`) follows
  at `device_flow.rs:183`.
- `DeviceFlowEndpoints::code_url` is a `String`; `opencode_go::endpoints`
  (`rust/fontaine-core/src/providers/opencode_go.rs:191`) builds it as
  `{server}/auth/device/code`, with `server` = `https://opencode.ai/console` in the product.
- The live Console reply, observed 2026-10-23:
  ```json
  {"device_code":"…","user_code":"TVCL-HHTD","verification_uri":"/console/device",
   "verification_uri_complete":"/console/device?user_code=TVCL-HHTD&client_id=opencode-cli",
   "expires_in":600,"interval":5}
  ```
- Tests: `rust/fontaine-core/tests/device_flow.rs` (`start_sends_the_client_id_and_scope_and_returns_the_code`,
  `a_verification_url_that_is_not_http_is_refused` with `javascript:alert(1)`, `file:///etc/passwd`,
  `not a url`) and `rust/fontaine-core/tests/opencode_go.rs` (`sign_in_stores_the_tokens_and_names_the_account`,
  `a_refused_sign_in_stores_nothing`). Every fixture uses an absolute verification URL, which is why
  the bug shipped.

## Design

In `DeviceFlow::start`, replace `Url::parse(&url)` with a resolution against the endpoint:

```rust
let base = Url::parse(&self.endpoints.code_url)
    .expect("reqwest already parsed code_url with the same url crate when the request was sent");
let parsed = base
    .join(&url)
    .map_err(|_| FlowError::Invalid("the verification URL is not a URL".to_string()))?;
```

`Url::join` is RFC 3986 reference resolution: an absolute URL is returned unchanged (today's
behavior), an absolute-path reference keeps the endpoint's scheme and host
(`/console/device` against `https://opencode.ai/console/auth/device/code` →
`https://opencode.ai/console/device`), and a malformed absolute URL still errors. The existing
`http`/`https` check runs on the resolved URL, so `javascript:` and `file:` stay refused. The
endpoint parse is an invariant, so it uses `expect`: `post` already parsed the same string with the
same `url` crate before this point.

RFC 8628 §3.2 does not require `verification_uri` to be absolute, and resolving a relative
reference against the request URL is the standard RFC 3986 interpretation, so this is not a
behavior deviation needing a `## Decisions` line in the master plan.

Security: resolution can change the host (a scheme-relative `//other.host/…` takes a new
authority), but the result still has to pass the `http`/`https` check, so a relative reference
cannot point the user anywhere an absolute URL from the same server could not already.

## Implementation Steps

1. `rust/fontaine-core/src/providers/device_flow.rs`: resolve the verification URI against
   `code_url` in `start` as above. Update the `FlowError::Invalid` doc comment only if it no longer
   matches.
2. `rust/fontaine-core/tests/device_flow.rs`:
   - add `a_relative_verification_url_resolves_against_the_endpoint`: reply with the Console's
     actual shape (`/console/device?user_code=ABCD-1234&client_id=opencode-cli`) and assert
     `verification_url == format!("{}/console/device?user_code=ABCD-1234&client_id=opencode-cli", server.uri())`;
   - in `a_verification_url_that_is_not_http_is_refused`, replace `"not a url"` (now a valid
     relative reference) with a malformed absolute URL that `join` still rejects, `"http://[::1"`.
3. `rust/fontaine-core/tests/opencode_go.rs`: change `sign_in_stores_the_tokens_and_names_the_account`'s
   fixture to the relative `verification_uri_complete` the live Console sends, so the end-to-end
   sign-in test reproduces issue #118.

## Files to Modify

- `rust/fontaine-core/src/providers/device_flow.rs`
- `rust/fontaine-core/tests/device_flow.rs`
- `rust/fontaine-core/tests/opencode_go.rs`
- `CHANGELOG.md` (`### Fixed` under `[Unreleased]`, via the PR step)

## Trade-offs

- **Join against `code_url`** (chosen) vs. against the account's `server`/Console URL: the code
  endpoint is the URL the reference was actually served from, which is what RFC 3986 resolution
  means, and it keeps `DeviceFlow` provider-agnostic. For an absolute-path reference both give the
  same result.
- **Prefixing the origin by string concatenation**: rejected; it mishandles query-only or
  path-relative references and duplicates what `Url::join` already does.

## Documentation

None. `docs/configuration.md` already says the sign-in "prints a verification URL and a code";
that remains true and now works.

## Testing Strategy

All unit/integration tests with `wiremock`, no live calls:
- `device_flow.rs`: the new relative-URL test pins issue #118 at the flow level; the refused-scheme
  test keeps `javascript:` / `file:` / malformed-absolute refused; the existing absolute-URL test
  confirms absolute URLs pass through unchanged.
- `opencode_go.rs`: the sign-in test with the Console's real reply shape pins the bug end to end
  through `sign_in_at`.

## Manual Verification

1. From `rust/`: `cargo run -p fontaine -- auth login go` prints
   `Open https://opencode.ai/console/device?user_code=…&client_id=opencode-cli and confirm the code …`;
   approving it in the browser prints `signed in to OpenCode Go as …`. Not automated because it needs
   a real OpenCode account and a human approval in the browser.
