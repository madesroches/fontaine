# claude-subscription: Persist the Context-Replay Broken-Version Verdicts Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/503

## Overview

The context-replay layout keeps a per-CLI-version "broken" verdict when its self-check sees the
replayed history miss the prompt cache. The verdict lives in a process-wide static, so every new
Fontaine process rediscovers a broken CLI version at the cost of about one full-history cache
rewrite. This plan persists the broken set in the data dir (owner-only), keyed by CLI version and
reminder-template revision, so a restart keeps the fallback and a template update retires the
verdicts recorded against the old template.

## Current State

- `rust/fontaine-core/src/providers/claude_subscription/context.rs`: `BROKEN`
  (`BTreeSet<(u32,u32,u32)>` behind a `Mutex`, `:405`), read by `layout` (`:56`), written by
  `mark_broken` (`:435`) from `finished` (`:441`). `REMINDER_TEMPLATE` (`:26`) and
  `TEMPLATE_SINCE` (`:24`) define the template.
- `transport.rs:243` calls `context::layout`; `transport.rs:478-488` calls `context::finished` and
  emits the one warning via `running.warn` ("…until restart…").
- `Cli` (`cli.rs:243`) holds `data_dir` and the `Warn` sink and is shared (`Arc<Cli>`) with
  `ClaudeTransport`; `ClaudeSubscription::connect` (`mod.rs:242`) builds it from `ctx.data_dir`.
- `owner_only::write_atomic` and `file_lock::{lock, read_shared}` already give owner-only atomic
  replacement and cross-process locking; `credentials.rs` is the precedent for a data-dir file
  guarded by a lock file.
- `docs/claude-subscription.md:~201` states the verdict lasts "for the rest of the process".

## Design

A `Verdicts` store replaces the static, owned by `Cli`:

```
struct Verdicts { path: PathBuf, broken: Mutex<BTreeSet<(u32,u32,u32)>> }
```

- File `<data_dir>/claude-cache-verdicts.json`:
  `{"broken":[{"cli":"2.1.319","template":1}]}`. Entries are keyed by CLI version and
  `TEMPLATE_REVISION`.
- New `const TEMPLATE_REVISION: u32 = 1` beside `REMINDER_TEMPLATE`, with a comment that any edit to
  the template or its placement logic bumps it. A bump needs no migration step: `load` keeps only
  entries whose `template` equals the current revision, and every write rewrites the file from the
  kept set, so stale entries are cleared the next time a verdict is recorded.
- `Verdicts::load(path, warn)` runs in `Cli::new`. A missing file is an empty set. An unreadable or
  unparseable file is an empty set plus one `Warn` (a verdict the user would care about losing); the
  next `mark` overwrites it.
- `mark(version) -> bool` inserts in memory (true only on first insertion), then persists: take
  `file_lock::lock` on the file, re-read it to merge verdicts another process wrote, add the
  version, `owner_only::write_atomic`. A persist failure is reported through `Warn` and does not
  undo the in-memory verdict, so the process still falls back.
- `layout(context, &Verdicts)` and `finished(sent, usage, at, &Verdicts)` take the store instead of
  reading the static; `transport.rs` passes `self.cli.verdicts()` in `prepare` for `layout`; `finished` runs in the
  'static stream closure, which holds only `Running`, so `Running` carries a shared `Arc<Verdicts>`
  handle. `RECORDS` stays a static.
- The warning text drops "until restart" and says the verdict is remembered (names the file's
  location only through the docs, not the message).

Entries are never cleared on a CLI upgrade: the key is the CLI version, so a new version is simply
absent from the set.

## Implementation Steps

1. `context.rs`: add `TEMPLATE_REVISION`, the `Verdicts` type (load/contains/mark, JSON via
   `serde_json`), remove `BROKEN`/`mark_broken`; thread `&Verdicts` through `layout`/`finished`.
2. `cli.rs`: construct `Verdicts` in `Cli::new` from `data_dir` and `warn`; add `verdicts()`
   accessor.
3. `transport.rs`: `layout` takes `self.cli.verdicts()` in `prepare`; add an `Arc<Verdicts>` field to
   `Running`, set where it is built (`self.cli.verdicts().clone()`), and `finished` takes the store
   from `running`; update the warning text.
4. `context/tests.rs`: port existing tests to the store; add the tests below.
5. `docs/claude-subscription.md`: update the verdict-lifetime paragraph. `docs/configuration.md`: add
   the verdicts file and its lock file to the file table.
6. `CHANGELOG.md`: add an entry.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/context.rs`
- `rust/fontaine-core/src/providers/claude_subscription/context/tests.rs`
- `rust/fontaine-core/src/providers/claude_subscription/cli.rs`
- `rust/fontaine-core/src/providers/claude_subscription/transport.rs`
- `docs/claude-subscription.md`
- `docs/configuration.md`
- `CHANGELOG.md`

## Trade-offs

- Store owned by `Cli` rather than a process static keyed by path: the static would leak state
  between tests and between accounts' data dirs; ownership by `Cli` gives each test an isolated
  temp dir.
- Re-read-and-merge under a lock instead of last-writer-wins: two concurrent Fontaine processes
  each discovering a different broken version would otherwise drop one verdict.
- Template revision constant instead of hashing `REMINDER_TEMPLATE`: a hash would also invalidate on
  harmless whitespace edits, and placement-logic changes (not in the string) would not invalidate
  at all; the constant is explicit.

## Documentation

`docs/claude-subscription.md`: the verdict now persists across restarts in the data dir and is
retired when the adapter's template changes; the warning no longer says "until restart".

`docs/configuration.md` file table: a row for `<data dir>/claude-cache-verdicts.json` (owner-only), and
`<data dir>/claude-cache-verdicts.lock` added to the lock-file row.

## Testing Strategy

All unit tests, against a temp data dir (no live CLI):

- A marked version persists: a second `Verdicts::load` on the same path reports it and `layout`
  returns `Legacy` (the restart case this issue is about).
- An entry with a different `template` is ignored on load and absent from the file after the next
  `mark`.
- `mark` merges a verdict another store instance wrote to the file in between.
- A missing file loads empty with no warning; a corrupt file loads empty with one warning and is
  repaired by the next `mark`.
- A persist failure (data dir path is a file) still marks in memory and emits one warning.
- The written file is owner-only on Unix (mode 0600).
- Existing layout and mark-once tests, ported.

## Open Questions

None.
