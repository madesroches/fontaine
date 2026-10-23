# Webfetch Without a System CA Bundle Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/112

## Overview
On a host with no system CA bundle (a bare `ubuntu` container), `Webfetch::new` fails at client
construction and `registry()` turns that into `cannot set up the webfetch tool: builder error`,
which aborts the whole session. The model transport already survives this by falling back to the
bundled Mozilla roots; `webfetch` and the scanner-model downloader do not. Give them the same
fallback so startup never depends on the host's trust store.

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
Choice: fall back to the bundled roots rather than letting startup continue with a broken tool.
It matches what model requests already do, keeps `webfetch` usable in the containers the `minimal`
profile targets, and avoids a second, differently-shaped failure path. The warning naming the
missing system roots is kept.

- Move the fallback into `fontaine-tools` (the lower crate) as a public helper, e.g.
  `fontaine_tools::tls::build_client(make: impl Fn() -> reqwest::ClientBuilder) -> reqwest::Result<reqwest::Client>`:
  build `make()`; on error retry `make().tls_certs_only(bundled roots)`; if that also fails return
  the original error. The closure form is needed because `ClientBuilder` is not `Clone`.
- Move the `webpki-root-certs` dependency from `fontaine-core` to `fontaine-tools`.
- `Webfetch::new` builds through the helper (keeping its UA, Accept, redirect and timeout settings).
- `transport::reqwest_client` calls the same helper, deleting `with_bundled_roots`, so there is one
  implementation. Keep its message wording for the warning.
- `scanner_model::ensure` builds through the helper too.
- If the bundled-root build also fails, the error still surfaces; make `registry()`'s message use
  the error chain (`error_chain::chain`) rather than `{e}`, so it no longer reads `builder error`
  alone.

## Implementation Steps
1. Add `fontaine-tools/src/tls.rs` with the helper; export it from `lib.rs`; add the dependency.
2. Switch `Webfetch::new`, `transport::reqwest_client`, and `scanner_model::ensure` to it; remove
   `with_bundled_roots` and the core dependency on `webpki-root-certs`.
3. Chain-render the registry error in `tools.rs`.
4. Tests (below), `docs/` note, changelog entry.

## Files to Modify
- `rust/Cargo.toml`, `rust/fontaine-tools/Cargo.toml`, `rust/fontaine-core/Cargo.toml`
- `rust/fontaine-tools/src/{lib.rs,tls.rs,webfetch.rs}`
- `rust/fontaine-core/src/{transport.rs,tools.rs,scanner_model.rs}`
- `docs/tools.md` (webfetch trust-store behavior), `CHANGELOG.md`

## Trade-offs
- Alternative: let startup continue and have `webfetch` calls report the missing trust store.
  Rejected: needs a deferred-error state in the tool, and leaves `webfetch` dead in containers
  where bundled roots would work.
- Silent trust of bundled roots weakens "use the host's policy" for hosts with a custom CA setup;
  mitigated because the fallback only applies when the system store fails to load, and it warns.

## Decisions
- 2026-10-23 Bundled-roots fallback chosen over deferred error (issue #112 offered both).

## Documentation
`docs/tools.md`: webfetch trusts the system CA store, falling back to bundled Mozilla roots with a
warning when it cannot be loaded; `SSL_CERT_FILE`/`SSL_CERT_DIR` override.

## Testing Strategy
- Unit test in `tls.rs`: with a builder closure whose first build is made to fail (e.g. an
  invalid configuration on the first call only, via a call counter), the helper returns a working
  client built with bundled roots; and when both fail it returns the original error. This pins
  issue #112 (no system roots stopped startup).
- A `fontaine-tools` test that `Webfetch::new` succeeds when the system roots cannot load, if the
  test can force that portably (e.g. `SSL_CERT_FILE` pointing at a missing file in a subprocess);
  otherwise covered by the helper test above.
- Existing transport and webfetch tests stay green.

## Manual Verification
In `ubuntu:26.04` with no `ca-certificates`, run `fontaine -p "hi"` and confirm it starts, logs the
fallback warning, and `webfetch` of an HTTPS URL succeeds. Not automated because it needs a bare
container image.

## Open Questions
- Does reqwest 0.13's rustls platform verifier actually fail at `build()` when roots are missing,
  or at request time? The issue's `builder error` says build time; confirm while writing the test.
