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
  valid role name into `ModelSpec::Named`. A bare model name is indistinguishable from a role
  there, so parsing stays unchanged.
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

A role always wins over a bare name (existing configs keep their meaning).

In `ChildContext::model_for`, for `ModelSpec::Named(name)` when `self.roles` has no role `name`
(and `name` is not a role the user defined), call a new `Switcher::bare_model(name, main)`:

1. Take `switcher.listings(Some((&main.handle.reference, &main.listing)), None)`; the main
   account's listing is already fetched, so it costs no call.
2. Collect accounts whose `models` is `Ok` and contains `name` exactly.
3. If the main model's account is among them, choose it. Else if exactly one account offers it,
   choose it. Else if several, error listing the `<account>/<model>` candidates. If none, error.
4. The result is a `ModelRef`; continue as `ModelSpec::Model` does (`switcher.main(ref)`), with
   no `ChildList` (a one-entry choice, like a lone reference).

Errors (unknown name) list the defined roles, the other accepted forms (`$main`,
`$guardian_default`, `<account>/<model>`, a bare model name an account offers) so the message
is no longer roles-only. The role-only message stays for the `/model` and agent-file paths that
share `Roles::entries_of`; the new error is produced in `model_for`.

Nothing loosens: the child's mode is still computed from the parent's, `$guardian_default` and the
guardian model are untouched, and project agent files still cannot override the model
(`accepts_model_override`). Agent-file `model:` fields keep the existing role/reference rules
(a bare name in a project file stays a role lookup; no new route for repository content).

The `parameters` description in `spawn.rs` gains "or a bare model name an account offers".

## Implementation Steps

1. `models.rs`: add `Switcher::bare_model` returning `Result<ModelRef, String>` with the
   resolution order above; unit-testable through a pure helper
   `pick_bare(name, main_account, &[AccountListing]) -> Result<ModelRef, BareError>`.
2. `child_runner.rs`: in `model_for`, route `Named(name)` with no matching role to `bare_model`.
3. `spawn.rs`: update the `model` parameter description.
4. `docs/agents.md`: update the `model` row and the model-resolution prose; `CHANGELOG.md` entry.
5. Tests (below).

## Files to Modify

- `rust/fontaine-core/src/models.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine-agents/src/spawn.rs`
- `rust/fontaine/tests/parallel_agents.rs`
- `docs/agents.md`, `CHANGELOG.md`

## Trade-offs

- Resolving in `ModelSpec::parse` would need listings (async, I/O); parse is pure and shared by
  config. Resolving at `model_for` keeps parse unchanged.
- Adding a `ModelSpec::Bare` variant would touch every match on the enum; the role-miss fallback
  needs none.

## Decisions

## Testing Strategy

- Unit: `pick_bare` - main account preferred when several offer the name; single non-main account
  chosen; several non-main accounts and no main -> error listing candidates; none -> error;
  accounts whose listing failed are skipped.
- Integration (`parallel_agents.rs`, wiremock-backed headless run, no live APIs): an `agent` call
  with `model` set to a bare name the endpoint lists starts the child on that model (request body
  carries it); an unknown bare name returns the error listing roles and accepted forms; a defined
  role of the same name as a model still wins.

## Documentation

`docs/agents.md` (`model` row and resolution prose); the tool description; `CHANGELOG.md`.

## Open Questions

- Should an exact-match only be required, or also the provider-normalised ids (e.g. dated
  suffixes)? Plan: exact match only, as `<account>/<model>` validation does.
