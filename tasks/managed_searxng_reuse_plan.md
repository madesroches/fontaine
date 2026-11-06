# Managed SearXNG Reuse Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/451 (designed together with
https://github.com/madesroches/fontaine/issues/241, as #451 requires)

## Overview

Every session currently starts a new `fontaine-searxng-<uuid>` container even when a healthy
managed SearXNG is already running, because `managed_search::managed` always mints a fresh name
and port and `Websearch::ensure_started` reuses only the child process its own session spawned.
This plan makes the managed SearXNG a single shared, user-level instance: a fixed container name
(`fontaine-searxng`), discovery of a running healthy instance before starting anything, removal
and replacement of an unhealthy one, convergence when two sessions start at once, and a startup
sweep that removes the old per-session strays — which is also the fix for #241.

## Current State

- `rust/fontaine-core/src/managed_search.rs`: `managed()` builds a `Managed` whose `prepare`
  closure writes `searxng/settings.yml` (`ensure_settings`), picks a free loopback port
  (`free_port`), generates a fresh name `fontaine-searxng-<uuid>` (`new_name`), and returns a
  `ManagedPlan` with the fixed `docker run --rm --name <name> -p 127.0.0.1:<port>:8080 -v
  <settings.yml>:/etc/searxng/settings.yml:ro docker.io/searxng/searxng:latest` argv and a
  detached `rm -f <name>` cleanup. A once-per-process notice announces the first start.
- `rust/fontaine-tools/src/websearch.rs`: `Websearch::ensure_started` locks
  `SearxngChild.slot: Mutex<Option<(Running, Url)>>`, reuses the slot only while its `Running`
  child's `try_wait` is alive, otherwise runs `prepare()`, spawns the argv, tails stderr, probes
  `base_url` until it answers (or the child exits / `startup_timeout` passes), and on success
  keeps `(Running, Url)` in the slot. `Running`'s `Drop` SIGTERMs the child's process group and
  spawns the plan's detached `cleanup`, so the container is removed when the session ends — but
  only on paths that run `Drop`; a SIGKILL, `process::exit`, or power loss leaks the container
  (#241), and every new session starts a new container anyway (#451).
- `Config::websearch_config_with_notice` (`rust/fontaine-core/src/config.rs:3516`) constructs
  the managed backend when `[websearch] backend` is unset and a runtime is found; the `notice`
  closure is `approver.warn` (`rust/fontaine-core/src/agent/build.rs:176`), so an approver
  warning channel already reaches this code.
- Tests: fake-runtime shell scripts in `rust/fontaine-core/tests/websearch_runtime.rs` (default
  backend, `fake_runtime`) and `rust/fontaine-tools/tests/websearch.rs` (`cleanup_config`),
  plus unit tests in both crates.
- Docs: "Default search" in `docs/tools.md` and the `[websearch]` section of
  `docs/configuration.md` describe the generated `fontaine-searxng-<random>` name, the detached
  `rm -f`, and the `fontaine-searxng-` stray prefix.

## Design

### Naming, port, and lifetime

- The managed container gets a fixed name, `fontaine-searxng` (`pub const NAME` in
  `managed_search.rs`, replacing `new_name`). `NAME_PREFIX` (`fontaine-searxng-`) stays as the
  marker of pre-shared strays.
- The port is read back, not recorded: `<runtime> port fontaine-searxng 8080/tcp` prints the
  published `127.0.0.1:<port>` mapping, so no port file next to `settings.yml` can go stale.
- Lifetime: the container is a user-level service. It is started by whichever session needs it
  first, left running across sessions, and removed only by (a) `docker stop fontaine-searxng`
  (the `--rm` flag then removes it), (b) replacement when it is unhealthy, (c) the startup sweep
  of old uuid-named strays, or (d) a reboot. No reference count or lease: one session's exit
  must not kill another session's search, and the container is small while starting it is the
  slow part.
- Because the container intentionally outlives the session, the whole drop-cleanup and
  signal-handling machinery from #241 becomes unnecessary for the managed backend: the detached
  `rm -f` on `Drop` is removed, and no SIGTERM/SIGHUP/PDEATHSIG work is done. The remaining
  leak cases (SIGKILL mid-start, power loss) leave a container under a known name that the next
  session reuses or replaces — which is the intended state, not debris.

### Runtime CLI helper (`managed_search.rs`)

A small `RuntimeCli { program: PathBuf, env: Env }` runs subcommands with the cleared
environment plus `container_runtime_env` (already threaded into `managed()`), each with a
bounded wait, and parses output:

- `inspect_running(name) -> bool`: `<runtime> inspect -f {{.State.Running}} <name>` parses
  `true`; any error or other output is `false` (docker and podman both accept this template).
- `published_port(name) -> Option<u16>`: `<runtime> port <name> 8080/tcp`, parse the first
  `host:port` line's port.
- `list_by_prefix() -> Vec<String>`: `<runtime> ps -a --filter name=fontaine-searxng
  --format {{.Names}}`, take the first comma-separated name per line.
- `rm(name) -> Result<(), String>`: `<runtime> rm -f <name>`; a non-zero exit returns the
  stderr for reporting.

### Prepare: sweep, discover, decide

`prepare` (now async — see Trade-offs) does, in order:

1. **Sweep strays.** For each name from `list_by_prefix()` that starts with `NAME_PREFIX`,
   `rm` it. These are per-session containers from older Fontaine versions; no pid or lease
   check is possible or needed (a concurrent session still running pre-fix Fontaine loses its
   backend — accepted; see Decisions). A failed sweep `rm` is reported through the same `warn`
    closure and the sweep continues with the remaining names; unlike step 3 it does not abort
    the prepare, since a leftover stray does not affect the steps that follow.
2. **Probe the shared instance.** If `inspect_running(NAME)` and `published_port(NAME)` both
   succeed and a loopback HTTP probe of `http://127.0.0.1:<port>` answers with a non-5xx
   status, return `Prepared::Reuse(that url)`. The probe is a plain `reqwest` GET with a short
   timeout; loopback HTTP needs none of the TLS client plumbing in `fontaine_tools::tls`.
3. **Replace the unhealthy.** If the container exists (`inspect` succeeds) but is not running
   or does not answer, `rm` it first; a failed `rm` is reported through the `warn` closure
   (#241's "report a failed cleanup") and aborts the prepare with that message.
4. **Start fresh.** Return `Prepared::Start(ManagedPlan)` with the fixed-name argv
   (`--name fontaine-searxng`), `fail_cleanup` = detached `rm -f fontaine-searxng` (run by
   `ensure_started` if this start fails or times out, replacing today's `cleanup`), no drop
   cleanup, and `shared: true`. The start notice fires only here, so a session that reuses an
   instance says nothing.

### Concurrency: two sessions start at once

Both sessions can pass step 2 (nothing running yet) and both reach `docker run --name
fontaine-searxng`; the loser exits at once with "The container name ... is already in use" on
stderr. `ensure_started` treats a start failure whose stderr matches the name-conflict as
retryable: it re-runs `prepare` once. By then the winner's container is up, so `prepare`
returns `Reuse` and the loser shares it. A start that fails any other way, or a second
conflict, is reported as today (with the stderr tail). No locking beyond the name itself is
introduced.

### `fontaine-tools` changes (`websearch.rs`)

- `ManagedPlan.cleanup` is replaced by `fail_cleanup: Option<Vec<String>>` (run by
  `ensure_started` on start failure or timeout, detached) and a new `shared: bool`. The
  `cleanup` field and the `spawn_detached` call in `Running::drop` are deleted: `Drop` keeps
  only the SIGTERM/SIGKILL stop of an *owned* child, and a shared container is never owned.
- New `pub enum Prepared { Reuse(Url), Start(ManagedPlan) }`. `type Prepare` becomes
  `dyn Fn() -> Pin<Box<dyn Future<Output = io::Result<Prepared>> + Send>> + Send + Sync`, and
  `Managed::new` takes a closure returning that boxed future. `ensure_started` awaits it.
- `SearxngChild.slot` becomes `Mutex<Option<(Slot, Url)>>` with
  `enum Slot { Owned(Running), Shared }`. The fast path: `Owned` reuses while `try_wait` is
  alive; `Shared` reuses while `probe(url)` answers. Either failing clears the slot and falls
  through to `prepare`, which re-discovers (and can restart a dead shared container).
- After a successful `Start` with `shared: true`, the client is disowned: stderr has already
  been taken by the drain task, so convert the `tokio::process::Child` with `into_std()`
  (consuming it disables `kill_on_drop`) and hand it to a `std::thread` that waits and reaps
  it. The slot stores `(Shared, url)`. A `shared: false` plan (only the managed backend passes
  `true`; the explicit `command` backend passes `false`) keeps today's owned behavior, so a
  user-supplied `command` instance is still stopped with the session.
- `SearxngChild::is_running` (test-only callers) treats `Shared` as `probe(url)`.
- The start loop's failure and timeout paths run `fail_cleanup` via the existing
  `spawn_detached` before returning the error, and the name-conflict retry above wraps the
  whole prepare/spawn/probe sequence once.

### `fontaine-core` changes

- `managed_search.rs`: `NAME`, `RuntimeCli`, the sweep/discover/prepare logic, and a `warn:
  Arc<dyn Fn(&str) + Send + Sync>` parameter on `managed()` used for a failed `rm`. The
  `managed()` closure returns the boxed future.
- `config.rs`: pass a second approver-backed closure (`Arc::new(move |m| approver.warn(m))`)
  into `managed()` from `websearch_config_with_notice`; the signature gains `warn`.
- `agent/build.rs:176`: construct and pass the warn closure alongside the notice.

## Implementation Steps

1. `fontaine-tools/src/websearch.rs`: introduce `Prepared`, make `Prepare`/`Managed::new`
   async (boxed future), replace `ManagedPlan.cleanup` with `fail_cleanup` + `shared`, change
   the slot to `(Slot, Url)`, implement the disown-after-probe path, the `fail_cleanup` on
   failure/timeout, the name-conflict retry, and the `Shared` fast path. Update
   `SearxngChild::is_running`.
2. `fontaine-core/src/managed_search.rs`: `NAME`, `RuntimeCli`, sweep/discover/prepare with
   probe and warn, fixed-name argv, `fail_cleanup`, `shared: true`, notice only on start.
3. `fontaine-core/src/config.rs` + `agent/build.rs`: thread the `warn` closure.
4. Update existing tests for the new signatures and fixed name
   (`websearch_runtime.rs:305`, `managed_search.rs` unit tests, `websearch.rs` tests).
5. Add the new tests (below).
6. Docs: `docs/tools.md` "Default search", `docs/configuration.md` `[websearch]`.
7. `CHANGELOG.md`.

## Files to Modify

- `rust/fontaine-tools/src/websearch.rs`
- `rust/fontaine-core/src/managed_search.rs`, `config.rs`, `agent/build.rs`
- `rust/fontaine-tools/tests/websearch.rs`, `rust/fontaine-core/tests/websearch_runtime.rs`
- `docs/tools.md`, `docs/configuration.md`, `CHANGELOG.md`

## Trade-offs

- **Read the port back with `<runtime> port` instead of recording it in a file next to
  `settings.yml`** (the issue's alternative): a file can go stale (container replaced by hand,
  port reassigned); the runtime's own mapping is authoritative. Cost: one extra CLI call per
  cold start.
- **Leave the container running across sessions instead of a reference count/lease** (the
  issue's two options): a lease needs an owner-of-record, expiry, and cross-session liveness —
  real complexity for a small, cheap container whose slow part is starting. `docker stop
  fontaine-searxng` is the explicit off switch.
- **Async `prepare`**: the discovery runs up to four runtime CLI calls (`ps -a`, `inspect`,
  `port`, maybe `rm`) plus an HTTP probe; synchronous `prepare` would block a tokio worker for
  as long as a slow or unreachable daemon chooses. A boxed future keeps `Managed` object-safe
  without generics.
- **Foreground `docker run` client disowned after the probe, rather than `docker run -d`**:
  keeps the existing stderr tail for start failures and the existing spawn/probe loop; `-d`
  would lose stderr and need `docker logs` plumbing on failure.
- **Sweep old uuid containers unconditionally, without #241's pid/start-time labels**: with a
  fixed shared name there is nothing to attribute a uuid container to, and the labels existed
  only to decide which strays were safe to remove. The cost — killing a concurrent pre-fix
  session's backend — is accepted (Decisions).

## Decisions

- AFK: fixed container name `fontaine-searxng`; port read back via `<runtime> port`, not a
  recorded file.
- AFK: the shared instance's lifetime is "left running across sessions" (user-level service);
  no reference count or lease.
- AFK: `fontaine-searxng-<uuid>` containers are swept unconditionally at managed-start time;
  a concurrent session still running pre-fix Fontaine loses its search backend. Accepted per
  #451's own sweep definition.
- AFK: #241's signal handling (`PR_SET_PDEATHSIG`, SIGTERM/SIGHUP cleanup) is not built; the
  leak becomes the intended steady state and strays are swept at the next start.

## Documentation

- `docs/tools.md` "Default search": fixed name, reuse across sessions, unhealthy replacement,
  the `<uuid>` stray sweep, the new lifetime (runs until `docker stop fontaine-searxng`,
  replacement, or reboot); remove the paragraph saying SIGKILL/power-off leaves a stray that
  only `ps --filter` can list.
- `docs/configuration.md` `[websearch]`: replace the "generated container name
  (`fontaine-searxng-<random>`) removed by a detached `rm -f`" description with the shared
  fixed-name instance and its lifetime.
- `CHANGELOG.md`: shared managed SearXNG instance; #241 fixed by the same change.

## Testing Strategy

All offline; `cfg(unix)` where a fake runtime script is used, following the existing
`fake_runtime`/`cleanup_config` patterns. Everything below is unit/integration level with
constructed inputs — no live API, daemon, or container.

- `managed_search.rs` unit: fixed-name argv; `published_port` parsing (single line, multiple
  lines, garbage); sweep selection (`fontaine-searxng-abc` removed, exact `fontaine-searxng`
  kept, unrelated names ignored).
- `websearch_runtime.rs` default-backend integration, fake `docker` script that keeps state
  across two runtime builds in one test: a second session's first search reuses the running
  instance (one `run` recorded, `inspect`/`port` exercised, search answers); an unhealthy
  instance (inspect false) is `rm -f`'d and replaced; the sweep removes a recorded
  `fontaine-searxng-<uuid>` entry.
- `websearch.rs` integration: a failed shared start runs `fail_cleanup`; dropping the tool and
  child after a successful shared start does not kill the client (its pid survives; the test
  kills it at the end); a start failure whose stderr contains the name conflict re-runs
  `prepare` once and reuses the returned URL; `Reuse` from `prepare` spawns nothing; explicit
  `command` backend keeps owned stop-with-session behavior and passes `shared: false`.
- The two-sessions-starting-concurrently case is covered deterministically by the
  name-conflict retry test above; a literal concurrent double-start test would be timing-flaky
  and adds nothing beyond it.

## Manual Verification

None. Every behavior above is reachable with a scripted runtime and a loopback server; the
real-image path (`searxng/searxng:latest`) differs only in pull time, which the 300 second
start timeout already covers.

## Open Questions

None.
