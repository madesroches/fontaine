# Webfetch User-Agent Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/73

## Overview

`webfetch` sends no `User-Agent` header, and Wikimedia (and many other sites and CDNs) answer
such requests with `403 Forbidden`. Give the webfetch client a descriptive User-Agent that names
Fontaine, its version, and a contact URL, as Wikimedia's User-Agent policy asks.

## Current State

- `Webfetch::new` (`rust/fontaine-tools/src/webfetch.rs:46-52`) builds its `reqwest::Client` with
  only a no-redirect policy and a timeout. reqwest 0.13 sends no `User-Agent` by default.
- Reproduction from the issue: `curl` (which sends `curl/<ver>`) gets 200 from
  `https://fr.wikipedia.org/wiki/Jean_de_La_Fontaine`; `curl -A ""` gets 403.
- Tests live in `rust/fontaine-tools/tests/webfetch.rs`, driving the tool against `wiremock`
  servers through the `serve` / `fetch` helpers. None inspects request headers.
- `docs/tools.md` "Web fetch" section lists the tool's behavior; it says nothing about headers.
- Claude Code's WebFetch identifies itself as
  `Claude-User (claude-code/<version>; +https://support.anthropic.com/)`: a product token, the
  client version, and a contact URL.

## Design

Add a constant in `webfetch.rs`:

```rust
const USER_AGENT: &str = concat!(
    "fontaine/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/madesroches/fontaine)"
);
```

and call `.user_agent(USER_AGENT)` on the client builder in `Webfetch::new`.

- `CARGO_PKG_VERSION` of `fontaine-tools` is the workspace version (`version.workspace = true`),
  so it tracks the released `fontaine` version.
- The shape matches Claude Code's (product/version plus a `+URL` contact comment), but names
  Fontaine: a harness must not present itself as Claude Code's agent to sites that key policy on
  `Claude-User`.
- The constant stays private; the test builds the expected string itself from
  `env!("CARGO_PKG_VERSION")`, which in the test crate is the same workspace version.
- Scope is the webfetch client only. The PromptGuard download client in
  `rust/fontaine-core/src/scanner_model.rs` talks to Hugging Face, which does not reject it; it is not
  part of this issue.

## Implementation Steps

1. `rust/fontaine-tools/src/webfetch.rs`: add `USER_AGENT` and `.user_agent(USER_AGENT)` in
   `Webfetch::new`.
2. `rust/fontaine-tools/tests/webfetch.rs`: add a test (below).
3. `docs/tools.md`: one bullet in "Web fetch" stating the User-Agent sent, e.g. "It identifies itself
   with the User-Agent `fontaine/<version> (+https://github.com/madesroches/fontaine)`."
4. `tasks/fontaine_harness_plan.md` `## Decisions`: one dated line recording the User-Agent string
   and why it differs from Claude Code's `Claude-User` identity.

## Files to Modify

- `rust/fontaine-tools/src/webfetch.rs`
- `rust/fontaine-tools/tests/webfetch.rs`
- `docs/tools.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Copy Claude Code's `Claude-User` string** — rejected: it would misidentify the client to site
  operators and to Anthropic's crawler policy.
- **A browser-like User-Agent** — rejected: it disguises an automated client, which is what
  Wikimedia's policy asks clients not to do, and some sites block spoofed browsers harder.
- **Make the User-Agent configurable** — not needed by the issue; can come later if a site needs it.

## Testing Strategy

Unit-tier `wiremock` test in `rust/fontaine-tools/tests/webfetch.rs`, pinning the bug seen in the
wild (Wikipedia 403 on a request with no User-Agent):

- `requests_carry_a_fontaine_user_agent`: mount its own mock (the `serve` helper only matches
  method and path) using `wiremock::matchers::header("user-agent", <expected>)`, added to the
  `use wiremock::matchers::{method, path}` import, answering 200 with text, where
  `<expected>` is built from `env!("CARGO_PKG_VERSION")`; fetch succeeds and returns the body.
  An unmatched request gets wiremock's 404, so a missing or different User-Agent fails the test —
  the same shape as the real failure.

The existing CI steps (`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`) cover the rest. No live-network test: a unit test against wiremock reproduces the
missing header exactly, and live tests are not allowed in CI.

## Open Questions

None.
