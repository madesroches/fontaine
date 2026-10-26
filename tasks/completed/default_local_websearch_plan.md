# Default Local Web Search Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/19 (follow-up to the WebSearch tool)

## Overview

With no `[websearch] backend`, a local web search should still work. When `docker` or `podman` is installed,
Fontaine registers the client-side `websearch` tool backed by a SearXNG container it starts on the first
search, with no configuration. `backend = "off"` disables it. Without a runtime, a startup warning says search
is unavailable when no other search path exists.

## Design

- `BackendConfig::Managed` in `fontaine-tools` reuses the SearXNG lifecycle (lazy shared child, process-group
  stop, readiness probe). The argv and `base_url` come from a `prepare` closure run at each start, so the
  per-start free port is picked then; the shared slot keeps the address with the process.
- `fontaine-core/src/managed_search.rs`: runtime lookup over a PATH value (docker before podman), the
  settings file (`use_default_settings`, `json` format, limiter off, a UUID-derived secret kept across runs),
  the free loopback port, the fixed argv with the fully qualified image, and a once-only start notice.
- `Config::websearch_config_with_notice` yields the managed backend when `backend` is unset and a runtime is
  found; `Off` yields none. Registration is unchanged otherwise: the tool is exposed alongside provider-native
  search exactly as an explicit backend is.
- Security: still `Arbitrary`/destructive-always through the same pipeline; loopback bind only; `[websearch]`
  stays user/CLI scope; the command is fixed in code; child env is `BASE_ENV` plus the four container-runtime
  variables. The settings directory is owner-only; the file is mode 0644 inside it because the container's
  user reads it through the bind mount (checked: a 0600 file is unreadable by another uid).

## Files

- `rust/fontaine-tools/src/websearch.rs`, `child_env.rs`
- `rust/fontaine-core/src/managed_search.rs` (new), `config.rs`, `agent.rs`, `agent/build.rs`, `native_search.rs`, `lib.rs`
- Tests: `fontaine-tools/tests/websearch.rs`, `fontaine-core/tests/{config,websearch_runtime}.rs`, `tests/common/runtime.rs`
- `docs/tools.md`, `docs/configuration.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Testing

All offline, `cfg(unix)` where a fake runtime script is used: runtime detection and preference, settings
content/secret/permissions, argv shape and port, default registration with and without a runtime, `off`,
explicit backend unchanged, the unavailable warning, and a managed start through a fake `docker` script whose
published port a wiremock server listens on.
