# Agent Tool: Accept a Bare Model Name Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/663

## Overview

The `agent` tool's `model` argument accepts a role, `$main`, `$guardian_default` or
`<account>/<model>`. A bare name such as `claude-sonnet-5-5` parses as a role name and fails at
spawn with "not a defined model role". Skills dispatch workers with bare names, so the chain
stalls. A bare name that is not a role is resolved against the configured accounts' model
listings, preferring the main model's account. Resolution is no-network: only the main account's
listing (already in hand) and accounts with an explicit `models` list in config are consulted.
Only role-name-shaped bare names (`[A-Za-z0-9_-]`) are resolved; names with `.` or `:`
(`gpt-4.1`, `llama3:8b`) keep erroring and are pointed to `<account>/<model>`.

## Current State

- `ModelSpec::parse` (`rust/fontaine-agents/src/registry.rs`) turns any text without `/` that is a
  valid role name into `ModelSpec::Named`.
- `ChildContext::model_for` (`rust/fontaine-core/src/child_runner.rs`) resolves `Named` through
  `RoleChains::chain_for` -> `Roles::entries_of`, whose `unknown()` yields the error
  ("`x` is not a defined model role; defined roles: ...").
- `Switcher::listings(main, only)` (`rust/fontaine-core/src/models.rs`) lists each account's
  models; `Switcher::main("<account>/<model>")` builds a handle and validates against the listing.
- The `agent` tool description (`rust/fontaine-agents/src/spawn.rs`, `parameters`) and
  `docs/agents.md` (the `model` row) list the accepted forms.
- Child resume re-parses the stored model text (`rust/fontaine-agents/src/tree.rs`), so it goes
  through the same `model_for`.

## Design

In `ChildContext::model_for`, for `ModelSpec::Named(name)` when `self.roles` has no role `name`,
call a new `Switcher::bare_model(name, main)`:

1. Test `name` against `main.listing.models` (already in hand in `model_for`; tested with the account kind's `lists` check, the predicate
   `Switcher::main` uses, e.g. Ollama accepts `:latest` suffixes, not literal string equality). On a hit choose the main account with no `listings`
   call, so the spawn never waits on other accounts' network calls.
2. Only on a miss, collect the accounts in config that carry an explicit `models` list (no
   network; `Switcher::listings` is not called for other accounts) and keep those whose list
   offers `name` by the same `lists` check. Accounts without an explicit list are not consulted.
3. If exactly one account offers it, choose it. Else if several, error listing the
   `<account>/<model>` candidates. If none, error naming the accounts checked (the main account
   and the explicit-list accounts) and pointing to `<account>/<model>` for other names.
4. The result is a `ModelRef`; continue as `ModelSpec::Model` does (`switcher.main(ref)`), with
   no `ChildList` (a one-entry choice, like a lone reference).

The unknown-name error reuses `Roles::unknown`'s text for both its forms (the empty-roles form
"define roles under `[models.roles]`" and the "defined roles: ..." form) and appends the other accepted forms (`$main`, `$guardian_default`, `<account>/<model>`, a bare
model name an account offers), and lists the accounts checked. The existing
assertion in `parallel_agents.rs` (~781-786, spawn with bare "sonnet", expects "not a defined
model role") stays valid. The role-only message stays for the `/model` and agent-file paths that
share `Roles::entries_of`; the new error is produced in `model_for`. `Roles::unknown`
(`model_roles.rs:117`) is private, so it becomes `pub(crate)` for `bare_model` to extend.

`ModelSpec::parse` is unchanged, so a bare name with `.` or `:` is rejected before `model_for`
with the `not_a_model` error; its text, the `parameters` description in `spawn.rs` and the docs
say to use `<account>/<model>` for such names. The description also gains "or a bare model name
the main account or an account with a `models` list offers".

## Implementation Steps

1. `models.rs`: add `Switcher::bare_model` returning `Result<ModelRef, String>` with the
   resolution order above; unit-testable through a pure helper
   `pick_bare(name, &[(account, models)]) -> Result<ModelRef, String>` that takes only the
   no-network candidate lists (the main listing plus explicit-list accounts).
2. `model_roles.rs`: make `Roles::unknown` `pub(crate)` so `bare_model` can extend it.
3. `child_runner.rs`: in `model_for`, route `Named(name)` with no matching role to `bare_model`.
4. `spawn.rs`: update the `model` parameter description.
5. `docs/agents.md`: update the `model` row and the model-resolution prose, noting that names with `.` or `:` need
   `<account>/<model>`; `CHANGELOG.md` entry.
6. Tests (below).

## Files to Modify

- `rust/fontaine-core/src/models.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine-core/src/model_roles.rs`
- `rust/fontaine-agents/src/spawn.rs`
- `rust/fontaine/tests/parallel_agents.rs`
- `docs/agents.md`, `CHANGELOG.md`

## Decisions

- Bare-name resolution lives at `model_for`, not in `ModelSpec::parse`, and there is no `ModelSpec::Bare`.
- A role always wins over a bare name.
- Agent-file `model:` keeps the role/reference rules, so repository content gets no new route.
- AFK: `ModelSpec::parse` stays unchanged; only role-name-shaped bare names resolve, and names with `.` or `:` (`gpt-4.1`, `llama3:8b`) keep the `not_a_model` error, whose text, the tool description and docs say to use `<account>/<model>`.
- AFK: a miss never calls `Switcher::listings` for other accounts (no credentialed network fan-out from a model-chosen string); it resolves only against the main account's listing plus accounts with an explicit `models` list in config, and the unknown-name error lists the accounts checked.

## Testing Strategy

- Unit: `pick_bare` (miss path) - single explicit-list account chosen; several accounts -> error
  listing candidates; none -> error listing the accounts checked; an account without an explicit
  `models` list is not consulted and `listings` is not called for it.
- Unit: a dotted bare name (`gpt-4.1`) still errors with the `<account>/<model>` hint.
- Integration (`parallel_agents.rs`, wiremock-backed headless run, no live APIs): an `agent` call
  with `model` set to a bare name the endpoint lists starts the child on that model (request body
  carries it); an unknown bare name returns the error listing roles and accepted forms (extends the existing
  "sonnet" case at `parallel_agents.rs:739` rather than duplicating it; the "sonnet" case needs no extra account, since the existing `mock` account has an
  explicit `models` list). The first case also covers the main-account hit: the endpoint lists the
  model on the main account and no other account is configured.
- Unit (`pick_bare`/`model_for`): a defined role of the same name as a model wins.
