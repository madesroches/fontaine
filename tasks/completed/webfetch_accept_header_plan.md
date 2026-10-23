# Webfetch Accept Header Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/85

## Overview

`webfetch` sends reqwest's default `Accept: */*`, so a server that negotiates content falls back to
its own default. That default can be a non-text representation, which webfetch rejects, and a site that
can serve markdown sends HTML that webfetch then has to convert. Claude Code's WebFetch sends
`Accept: text/markdown, text/html, */*`. Fontaine sends the same value on every request.

## Current State

- `Webfetch::new` (`rust/fontaine-tools/src/webfetch.rs:46`) builds a `reqwest::Client` with
  redirects disabled (`Policy::none()`) and a timeout. It sets no headers, so it sends reqwest's
  default `Accept: */*`; `default_headers` replaces it (`HeaderMap::insert`).
- `Webfetch::fetch` (`webfetch.rs:58`) follows same-host redirects itself, issuing a fresh
  `self.client.get(url)` for each hop. Client-level default headers therefore apply to every hop.
- The textual-content check (`webfetch.rs:99`) already accepts `markdown`, and only `html` bodies
  are converted. A `text/markdown` response comes back as it is.
- Tests live in `rust/fontaine-tools/tests/webfetch.rs`. They use `wiremock`, currently importing
  only the `method` and `path` matchers.
- `docs/tools.md` `## Web fetch` lists webfetch's behavior as bullets.
- PR #84 (#73) touches the same client builder, `wiremock::matchers` import and `docs/tools.md`
  list; resolve conflicts by keeping both sides.

## Design

In `webfetch.rs`, add a const next to `MAX_SAME_HOST_REDIRECTS`:

```rust
const ACCEPT: &str = "text/markdown, text/html, */*";
```

Set it as a default header on the builder in `Webfetch::new`:

```rust
let mut headers = reqwest::header::HeaderMap::new();
headers.insert(reqwest::header::ACCEPT, reqwest::header::HeaderValue::from_static(ACCEPT));
reqwest::Client::builder().default_headers(headers)...
```

## Implementation Steps

1. `rust/fontaine-tools/src/webfetch.rs`: add the `ACCEPT` const, and add `default_headers` to
   the client builder in `Webfetch::new`.
2. `rust/fontaine-tools/tests/webfetch.rs`: add `headers` to the `wiremock::matchers` import, and
   add the tests listed under Testing Strategy.
3. `docs/tools.md` (`## Web fetch`): add a bullet saying that it asks for
   `text/markdown, text/html, */*`, so a site that can serve markdown sends it.

## Files to Modify

- `rust/fontaine-tools/src/webfetch.rs`
- `rust/fontaine-tools/tests/webfetch.rs`
- `docs/tools.md`

## Trade-offs

- **Default header on the client vs. per-request `.header()`:** a client default covers every
  same-host redirect hop with no per-call code, and it stays in the one place the client is
  configured.
- **Claude Code's exact value vs. adding q-values** (e.g. `text/html;q=0.9`): the bare list
  leaves ordering to the server, as Claude Code does. Sites already tuned for Claude Code's
  requests respond the same way to Fontaine's.

## Documentation

- `docs/tools.md` `## Web fetch`: one bullet (step 3).

## Testing Strategy

Integration tests in `rust/fontaine-tools/tests/webfetch.rs`, using `wiremock` and no live
network:

- `same_host_redirects_keep_the_accept_header`: `/a` (302 to `/b`) and `/b` are both mounted with
  the matcher `headers("accept", vec!["text/markdown", "text/html", "*/*"])`. A missing header on
  either the first request or the redirect hop gets wiremock's 404 and fails the test. `/b`
  returns 200 `text/markdown` with a markdown body; the test unwraps the output and asserts it
  equals that body as sent (so markdown passes through unconverted), following `a_same_host_redirect_is_followed` in `tests/webfetch.rs`.

## Open Questions

None.
