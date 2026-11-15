# Agent Tool: Accept a Bare Model Name Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/663

## Overview

The `agent` tool's `model` argument accepts a role, `$main`, `$guardian_default` or
`<account>/<model>`. A bare name such as `claude-sonnet-5-5` parses as a role name and fails at
spawn with "not a defined model role". Skills dispatch workers with bare names, so the chain
stalls. A bare name that is not a role is resolved against the configured accounts' model
listings, preferring the main model's account.

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

1. Test `name` against `main.listing.models` (already in hand in `model_for`; exact match only, as
   `<account>/<model>` validation does). On a hit choose the main account with no `listings`
   call, so the spawn never waits on other accounts' network calls.
2. Only on a miss (step 1 already tested the main listing, so `pick_bare` never needs the main account) take `switcher.listings(Some((&main.handle.reference, &main.listing)), None)`;
   the main account (and accounts with an explicit `models` list) is free; the others are listed
   concurrently under the 10s `LISTING_TIMEOUT`, so an unknown name can block a spawn up to 10s.
   Collect accounts whose `models` is `Ok` and contains `name` exactly.
3. If exactly one account offers it, choose it. Else if several, error listing the
   `<account>/<model>` candidates. If none, error.
4. The result is a `ModelRef`; continue as `ModelSpec::Model` does (`switcher.main(ref)`), with
   no `ChildList` (a one-entry choice, like a lone reference).

The unknown-name error reuses `Roles::unknown`'s text for both its forms (the empty-roles form
"define roles under `[models.roles]`" and the "defined roles: ..." form) and appends the other accepted forms (`$main`, `$guardian_default`, `<account>/<model>`, a bare
model name an account offers), and mentions accounts whose listing failed. The existing
assertion in `parallel_agents.rs` (~781-786, spawn with bare "sonnet", expects "not a defined
model role") stays valid. The role-only message stays for the `/model` and agent-file paths that
share `Roles::entries_of`; the new error is produced in `model_for`. `Roles::unknown`
(`model_roles.rs:117`) is private, so it becomes `pub(crate)` (or a small `Roles` method returns the
role-only text) for `bare_model` to extend.

The `parameters` description in `spawn.rs` gains "or a bare model name an account offers".

## Implementation Steps

1. `models.rs`: add `Switcher::bare_model` returning `Result<ModelRef, String>` with the
   resolution order above; unit-testable through a pure helper
   `pick_bare(name, &[AccountListing]) -> Result<ModelRef, String>`.
2. `model_roles.rs`: make `Roles::unknown` `pub(crate)` (or add a small `Roles` method returning the
   role-only text) so `bare_model` can extend it.
3. `child_runner.rs`: in `model_for`, route `Named(name)` with no matching role to `bare_model`.
4. `spawn.rs`: update the `model` parameter description.
5. `docs/agents.md`: update the `model` row and the model-resolution prose; `CHANGELOG.md` entry.
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

## Testing Strategy

- Unit: `pick_bare` (miss path) - single non-main account
  chosen; several accounts -> error listing candidates; none -> error;
  accounts whose listing failed are skipped.
- Integration (`parallel_agents.rs`, wiremock-backed headless run, no live APIs): an `agent` call
  with `model` set to a bare name the endpoint lists starts the child on that model (request body
  carries it); an unknown bare name returns the error listing roles and accepted forms (extends the existing
  "sonnet" case at `parallel_agents.rs:739` rather than duplicating it; the mock endpoint must
  answer account listings); a defined
  role of the same name as a model still wins.
- Integration, main-hit path (`parallel_agents.rs`, `bare_name_main_account_hit_without_listings`): the
  endpoint lists the model on the main account and no other account is configured; the child starts on it.
