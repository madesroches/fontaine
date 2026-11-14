# Shell Allow Rules Tolerate a Workspace `cd` Prefix Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/635

## Overview

A subagent whose `shell` is limited by specifiers (the built-in `reviewer`) ran
`cd /home/mad/fontaine && gh issue view 627` and was denied although `shell(gh issue view *)` is on
its allowlist: an allow rule matches only a command that is exactly one simple command, and
`cd X && cmd` is a list. Models habitually prefix `cd <repo>`, especially when their stated
working directory is not the repo. This plan makes an allow-posture shell match tolerate exactly
one leading `cd <literal dir> &&` whose target lies inside the workspace roots, and matches the
rule against the command after it.

## Current State

- `permission_rules.rs::shell_matches` (Allow posture) accepts only `analysis.simple`, which
  `preclassify.rs::simple_of` sets to `Some(argv)` only for a single simple command with literal
  words, no assignments, no redirects, no list operators.
- The same `matches(.., Posture::Allow)` serves user `allow` rules and the per-agent-type `only`
  restrictions (`gate.rs::restriction_on`).
- Deny and ask postures already match a rule's words anywhere in a compound command, so they are
  unaffected.
- `preclassify.rs::Environment` already resolves a `cwd` against the roots (`base()`,
  `in_scope()`, `Scope::find`). `CallContext` carries `scope` and the call `args` (with `cwd`).
- The shell tool persists the working directory between calls, so a `cd` is also meaningful to a
  later call; this plan does not change that.
- The reviewer's model sees `claude-cli-cwd` as its primary directory when the model transport is
  the `claude-subscription` CLI (`providers/claude_subscription/cli.rs`, `WORKDIR_NAME`); that is
  the CLI's own transport directory, not the shell tool's cwd (the primary root), so it is not
  changed here.

## Design

Add to `ShellAnalysis` a field `after_cd: Option<CdPrefixed>` with
`CdPrefixed { dir: String, argv: Vec<String> }`, filled by a new `cd_prefixed_of(&Program)` next
to `simple_of`. It is `Some` only when the program is exactly
`cd <one literal word> && <one simple command>` where:

- the first command is a simple command, `cd`, with exactly one literal argument that does not
  start with `-` (no `-`, `-P`, `--`, no `$VAR`, `~`, globs), no assignment prefix, no redirects;
- the connector is `&&` (not `;`, `||`, `&`, `|`), with exactly two pipeline items, neither
  banged/timed;
- the second command satisfies everything `simple_of` demands of a simple command (literal words,
  no prefix assignment, no redirects), and `nested_chain` is false.

Refactor `simple_of` so the "one simple command to argv" part is a shared helper; both use it.

In `permission_rules.rs`, `shell_matches` for `Posture::Allow` matches against `analysis.simple`
as today, or else against `analysis.after_cd.argv` when `after_cd.dir` is inside the roots. The
check uses `Environment { scope, cwd }` built from the call's `cwd` argument (new
`Environment::contains_dir(&self, text) -> bool`, wrapping `base()` + `in_scope()`; the target
need not exist, matching how `in_scope` already works). A `cd` out of the roots, or from an
out-of-scope `cwd`, gets no tolerance and falls through as before.

Soundness: nothing about what runs changes. The `cd` is a builtin that only moves the directory
within the roots (the shell tool already confines the persisted cwd to the roots), and the command
after it must match the allow rule's words exactly as it would alone. The floor, hooks, guardian
and the critical-path check still see the whole original command line. Anything else with a list
operator, substitution, redirect or second `cd` still fails to match.

`propose` (the "allow this call again" rule) is unchanged: it returns `None` for a compound, so no
rule is proposed for `cd X && cmd`. The reviewer's denial text is unchanged.

## Implementation Steps

1. `fontaine-tools/src/preclassify.rs`: add `CdPrefixed`, `ShellAnalysis::after_cd`, the shared
   simple-command helper, `cd_prefixed_of`, and `Environment::contains_dir`. Fill `after_cd` in
   `analyze_at` (cleared when `nested_chain`).
2. `fontaine-core/src/permission_rules.rs`: use `after_cd` in the Allow branch of `shell_matches`,
   building the `Environment` from `call.scope` and the call's `cwd` argument.
3. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-tools/src/preclassify.rs`
- `rust/fontaine-core/src/permission_rules.rs`
- `rust/fontaine-core/tests/permission_rules.rs`
- `rust/fontaine-core/tests/extensions.rs` (reviewer regression, if it already drives the gate)
- `docs/agents.md`, `docs/permissions.md` (or the page that documents shell allow rules)
- `CHANGELOG.md`

## Trade-offs

- **Tolerate a `cd` prefix (chosen)** vs. **fix the subagent's cwd only**: the issue's second
  option does not cover a model that `cd`s anyway (it is a habit, and user allow rules hit the
  same wall), and the cwd shown to the model comes from the transport, not the tool.
- **Accept `;` and `||` too**: rejected; `&&` is the idiom and keeps the rule one line to state.
- **A general "strip cd" in the analysis for all postures**: unnecessary; deny and ask already
  match anywhere in a compound.

## Decisions

## Documentation

Update the shell allow-rule description (where it says an allow matches only a single simple
command) to state the one tolerated `cd <dir> &&` prefix, and note in `docs/agents.md` that the
`reviewer`'s shell entries accept it.

## Testing Strategy

Unit tests, no live services:

- `preclassify.rs`: `after_cd` is `Some` for `cd /x && git status`, `cd x && git status -s`;
  `None` for `cd -`, `cd -P x`, `cd`, `cd a b`, `cd $X`, `cd ~`, `cd *`, `cd x; git status`,
  `cd x || git status`, `cd x && a && b`, `cd x && a | b`, `cd x && a > f`, `cd x && A=1 a`,
  `cd x && $(a)`, `cd x & a`, `cd x && cd y && a`, `cd x > f && a`.
- `tests/permission_rules.rs` (extends `shell_allow_never_matches_a_compound_command`): allow
  `shell(gh issue view *)` matches `cd <root> && gh issue view 627` and a relative in-root dir,
  and does not match a dir outside the roots, `cd <root> && gh issue view 1 && rm x`, or
  `cd <root> && rm x`; deny/ask results unchanged.
- Regression for #635: through the gate with the `reviewer` restrictions, `cd <primary root> &&
  gh issue view 627` is allowed and `cd <primary root> && rm -rf x` is still denied.

## Open Questions

None.
