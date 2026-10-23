# Webfetch Accept Header Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/85

## Overview

`webfetch` sends no `Accept` header, so a server that negotiates content falls back to its own
default. That default can be a non-text representation, which webfetch rejects, and a site that
can serve markdown sends HTML that webfetch then has to convert. Claude Code's WebFetch sends
`Accept: text/markdown, text/html, */*`. Fontaine sends the same value on every request.

## Current State

- `Webfetch::new` (`rust/fontaine-tools/src/webfetch.rs:46`) builds a `reqwest::Client` with
  redirects disabled (`Policy::none()`) and a timeout. It sets no headers, and reqwest adds no
  `Accept` by default.
- `Webfetch::fetch` (`webfetch.rs:58`) follows same-host redirects itself, issuing a fresh
  `self.client.get(url)` for each hop. Client-level default headers therefore apply to every hop.
- The textual-content check (`webfetch.rs:99`) already accepts `markdown`, and only `html` bodies
  are converted. A `text/markdown` response comes back as it is.
- Tests live in `rust/fontaine-tools/tests/webfetch.rs`. They use `wiremock`, currently importing
  only the `method` and `path` matchers.
- `docs/tools.md` `## Web fetch` lists webfetch's behavior as bullets.
- The User-Agent change for #73 (a `USER_AGENT` const, `.user_agent(...)` on the same builder,
  the `requests_carry_a_fontaine_user_agent` test, a `docs/tools.md` bullet) is on the unmerged
  `fetch` branch, not on `main`. This branch is cut from `main`, so whichever of the two merges
  second has a small textual conflict in those three spots. Each conflict resolves by keeping both
  sides.

## Design

In `webfetch.rs`, add a const next to `MAX_SAME_HOST_REDIRECTS`:

```rust
const ACCEPT: &str = "text/markdown, text/html, */*";
```

Set it as a default header on the builder in `Webfetch::new`:

```rust
let mut headers = reqwest::header::HeaderMap::new();
headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static(ACCEPT));
reqwest::Client::builder().default_headers(headers)...
```

`HeaderValue::from_static` panics only on invalid bytes, and the value is a fixed ASCII literal.
No conversion or content-type logic changes. The value matches Claude Code exactly, so no
`## Decisions` entry is needed in the master plan.

## Implementation Steps

1. `rust/fontaine-tools/src/webfetch.rs`: add the `ACCEPT` const, and add `default_headers` to
   the client builder in `Webfetch::new`.
2. `rust/fontaine-tools/tests/webfetch.rs`: add `header` to the `wiremock::matchers` import, and
   add the tests listed under Testing Strategy.
3. `docs/tools.md` (`## Web fetch`): add a bullet saying that it asks for
   `text/markdown, text/html, */*`, so a site that can serve markdown sends it, and that markdown
   is returned as it is.

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

- `requests_ask_for_markdown_first`: a mock matched with
  `header("accept", "text/markdown, text/html, */*")` returns `200 text/plain`. The fetch must
  succeed, and it would get wiremock's 404 if the header were missing or different.
- `same_host_redirects_keep_the_accept_header`: `/a` returns a 302 to `/b`, and `/b` is mounted
  only with the `accept` matcher. This pins the header across the redirect hops that webfetch
  issues itself.
- `markdown_is_returned_as_it_is`: a `text/markdown` body containing markdown syntax
  (e.g. `# Title\n\n*x*`) comes back unchanged, without going through the HTML converter. Add
  this only if an existing test does not already cover a `text/markdown` response.

## Open Questions

None.
