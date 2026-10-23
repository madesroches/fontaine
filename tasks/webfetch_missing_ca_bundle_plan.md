# Webfetch Without a System CA Bundle Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/112

## Overview
On a host with no system CA bundle (a bare `ubuntu` container), `Webfetch::new` fails at client
construction and `registry()` turns that into `cannot set up the webfetch tool: builder error`,
which aborts the whole session. The model transport already survives this by falling back to the
bundled Mozilla roots; `webfetch` and the scanner-model downloader do not. Give them the same
fallback so startup no longer fails on a missing trust store through these two paths. HTTP MCP
servers (rmcp builds its own client) are out of scope.

## Current State
- `rust/fontaine-core/src/transport.rs`: `reqwest_client()` builds a default client and, on
  failure, `with_bundled_roots` retries with `webpki_root_certs::TLS_SERVER_ROOT_CERTS` and logs a
  `tracing::warn!` naming the cause.
- `rust/fontaine-tools/src/webfetch.rs` (`Webfetch::new`) builds its own
  `reqwest::Client::builder()...build()?` with no fallback; the error is propagated up through
  `builtins::build` to `rust/fontaine-core/src/tools.rs:107`, which fails registry creation.
- `rust/fontaine-core/src/scanner_model.rs:92` (`ensure`) has the same unguarded build, but fails
  only that download, not startup.
- `fontaine-core` depends on `fontaine-tools`, not the reverse; `webpki-root-certs` is a
  `fontaine-core` dependency only.

## Design
- On Unix, reqwest 0.13.5's `build()` calls `rustls_platform_verifier::Verifier::new(...)`, which
  errors with "No CA certificates were loaded from the system" at build time (Windows and macOS use
  other verifiers), so retrying the build is the right place for the fallback.
- Move the fallback into `fontaine-tools` (the lower crate) as a public helper, e.g.
  `fontaine_tools::tls::build_client(make: impl Fn() -> reqwest::ClientBuilder) -> reqwest::Result<reqwest::Client>`:
  build `make()`; on error retry `make().tls_certs_only(bundled roots)`; if that also fails return
  the original error. The closure form is needed because `ClientBuilder` is not `Clone`.
- Move the `webpki-root-certs` dependency from `fontaine-core` to `fontaine-tools`.
- `Webfetch::new` builds through the helper (keeping its UA, Accept, redirect and timeout settings).
- `transport::reqwest_client` calls the same helper, deleting `with_bundled_roots`, so there is one
  implementation. Keep the `tracing::warn!` naming the missing system roots, with its message
  wording. Update the `caused_client` doc comment (it mentions the bundled-roots fallback) to point
  at the shared helper.
- `scanner_model::ensure` builds through the helper too, and renders its download error (currently
  `e.to_string()`) with `chain(&e.without_url())`.
- If the bundled-root build also fails, the error still surfaces; make `registry()`'s message in
  `tools.rs` use the error chain rather than `{e}`, rendered as
  `fontaine_tools::error_chain::chain(&e.without_url())` (`tools.rs` does not import `chain`), so it
  no longer reads `builder error` alone. Keep `reqwest_client`'s "could not build the HTTP client:
  ..." `map_err` message.

## Implementation Steps
1. Add `fontaine-tools/src/tls.rs` with the helper; export it from `lib.rs`; add `webpki-root-certs.workspace = true` to `fontaine-tools/Cargo.toml`.
2. Switch `Webfetch::new`, `transport::reqwest_client`, and `scanner_model::ensure` to it; remove
   `with_bundled_roots` and the `webpki-root-certs` dependency line in `fontaine-core/Cargo.toml`.
3. Chain-render the registry error in `tools.rs` as `fontaine_tools::error_chain::chain(&e.without_url())`,
   and the scanner download error in `scanner_model.rs` as `chain(&e.without_url())`.
4. Tests (below), `docs/` note, changelog entry under a new `### Fixed` subsection in
   `[Unreleased]` of `CHANGELOG.md`.

## Files to Modify
- `rust/fontaine-tools/Cargo.toml`, `rust/fontaine-core/Cargo.toml` (`Cargo.lock` follows)
- `rust/fontaine-tools/src/{lib.rs,tls.rs,webfetch.rs}`
- `rust/fontaine-core/src/{transport.rs,tools.rs,scanner_model.rs}`
- `docs/tools.md` (webfetch trust-store behavior), `CHANGELOG.md` (new `### Fixed` subsection in `[Unreleased]`)

## Trade-offs
- Alternative: let startup continue and have `webfetch` calls report the missing trust store.
  Rejected: needs a deferred-error state in the tool, and leaves `webfetch` dead in containers
  where bundled roots would work.
- Silent trust of bundled roots weakens "use the host's policy" for hosts with a custom CA setup;
  mitigated because the fallback only applies when the system store fails to load, and it is recorded in the debug log (`--debug-file`), not shown.

## Decisions
- 2026-10-23 Bundled-roots fallback chosen over deferred error (issue #112 offered both).

- 2026-10-23 AFK: Helper test injects the first-build failure with an invalid user agent on the first call only and drops the "both builds fail" assertion, avoiding a new `build_client_with` seam.
- 2026-10-23 AFK: HTTP MCP / OAuth client construction declared out of scope (rmcp 3.6.0 `from_uri` and OAuth build their own clients and can panic on a bare host); `with_client` exists but rmcp's same-origin redirect policy is private, so passing our own client would silently change redirect behavior. Follow-up issue: route MCP HTTP through the tls helper.

## Documentation
`docs/tools.md`: webfetch trusts the system CA store, falling back to bundled Mozilla roots with a
warning when it cannot be loaded; `SSL_CERT_FILE`/`SSL_CERT_DIR` override.

## Testing Strategy
- Unit test in `tls.rs`: with a builder closure whose first build is made to fail (an invalid user
  agent, `user_agent("bad\nvalue")`, on the first call only, via a call counter), the helper
  returns a working client built with bundled roots. The retry is unconditional (any build error, not only TLS), as `with_bundled_roots` is
  today. This pins issue #112 (no system roots stopped startup).
- Existing transport and webfetch tests stay green.

## Manual Verification
In `ubuntu:26.04` with no `ca-certificates`, run `fontaine -p "hi"` and confirm it starts, logs the
fallback warning, and `webfetch` of an HTTPS URL succeeds. Not automated because it needs a bare
container image.
