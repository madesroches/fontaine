# Secret File Reads Leave the Fast Path Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/370

## Overview

Reading a credential file inside the workspace (`cat .env`, `read {"path": ".env"}`) never
reaches the guardian: containment alone earns the read-only fast path and the Tier 0.5 allow. The
secret then enters the transcript, goes to the model provider, is shown in the terminal and is
kept in session files, and a prompt injection can stage it for a later exfiltration step. This
plan adds a built-in, structured list of secret file names. A read-only call that names one of them
leaves the fast path and takes the normal pipeline (guardian in `auto`/`plan`, prompt in
`manual`/`acceptEdits`, denial in `dontAsk`/headless). A user-scope `allow` rule for the file
restores the skip.

## Current State

- `PermissionPipeline::evaluate` (`rust/fontaine-core/src/permissions.rs:633`) runs the floor, then
  allows `call.risk == ReadOnly && (located.is_ok() || names_no_path(tool))` at
  `Tier::ReadOnlyFastPath` (`:789-795`).
- For `shell`, `self.preclassifier.read_paths(command, &env)` (`:820`) returns the path operands of
  a read-only command (positionals and path-valued flags, attached forms included, so `grep -f.env x`
  yields `.env`). `ruled_read_calls` (`:462`) turns them into `read`/`ls`/`grep` calls resolved
  against the `cwd`; deny and ask rules are checked against them via `ruled_hit`, and the branch then
  returns `Allow { tier: Tier05 }` (`:843`).
- If neither skip applies, the call falls to allow rules and hooks (`:849-881`), then the mode:
  `dontAsk` denies, `manual` and `acceptEdits` prompt, `plan` and `auto` give `Verdict::Guardian`
  (`:942-966`). That is exactly the route wanted for a secret read.
- `rust/fontaine-core/src/floor.rs` holds the structured name checks (`PROTECTED_DIRS`,
  `PROTECTED_FILES`, `same`, `FOLD_CASE`, `protected_path_with` checking `scope.written(given)` and
  `scope.resolved(given)`), no regex.
- Docs: `docs/tools.md` "What earns the read-only fast path", `docs/permissions.md` read-only fast
  path (step 3) and "The shell pre-classifier".

## Design

### `floor::secret_file`

A new pure helper in `floor.rs`, next to the protected-path code:

```
pub fn secret_file_name(name: &OsStr) -> Option<&'static str /* the matched pattern */>
pub fn secret_path(scope: &Scope, given: &str) -> Option<String>   // checks written + resolved forms
```

`secret_file_name` matches the final path component by exact name, prefix and suffix tests only
(no regex, no globbing library), always case-folded: whether a name marks a secret does not depend
on the filesystem.

The `.env` rows are the same predicate as `is_env_like` in `project_env/discover.rs:834`, which
already covers `.env`, `.env.*` and `*.env` with no exceptions. `secret_file_name` is a separate
predicate (it adds the `.env.example`/`.sample`/`.template` exceptions) and may reuse or move
`is_env_like`; either way the repository-scan call site must keep working.

| Pattern | Rule |
|---|---|
| `.env` | exact |
| `.env.*` | starts with `.env.`, except exactly `.env.example`, `.env.sample`, `.env.template` |
| `*.env` | ends with `.env` (`prod.env`) |
| `.netrc`, `_netrc`, `.npmrc`, `.pypirc`, `.git-credentials`, `.pgpass` | exact |
| SSH private keys | exact `id_rsa`, `id_dsa`, `id_ecdsa`, `id_ed25519`, and each with an `_sk` suffix (`id_ecdsa_sk`, `id_ed25519_sk`); `.pub` files do not match |
| `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks`, `*.keystore` | extension (suffix) test |

The names live in `const` tables beside `PROTECTED_FILES`. `secret_path` runs the name check on both
`scope.written(given)` and `scope.resolved(given)`, so a symlink `config -> .env` is caught by its
resolved form and a not-yet-resolved spelling by its written form (same two-form pattern as
`protected_path_with`). The returned string names the matched pattern for the ask reason.

### Pipeline change (`permissions.rs`)

1. **File tools.** Compute `secret = self.secret_read(tool, judged)` once before the fast path:
   for a `ReadOnly` call with a string `path` argument (`read`, `grep`, `lsp`), the result of
   `floor::secret_path`. `ls` and `glob` are not checked: they return names only, and glob's target
   is in `pattern`. The fast-path condition becomes `... && secret.is_none()`. The
   agent-spawn and `names_no_path` branches are unaffected. The call then flows on: allow rules (a
   user-scope `read(.env)` allow matches here, tier `allow_rule`), hooks, then the mode.
2. **Shell.** In the Tier 0.5 branch, after the deny/ask rule checks, test the `paths` from
   `read_paths` (resolved against the `cwd` like `ruled_read_calls` does) with `secret_path`,
   skipping the check when the command is `ls`. If one or more are secret files, restore the skip
   only when every secret operand's own synthesized read calls are matched by an allow rule (check
   each secret path's calls separately: `ruled_hit` matches if ANY read matches, so `cat .env
   README.md` must not pass under `allow = ["read(README.md)"]`). The allow rules are `rules.allow`
   chained with `skill_grants(call.agent)`, exactly as the general allow stage does. When restored,
   return `Allow { tier: AllowRule, rule }`; otherwise do not return, and fall through to the
   general allow rules (`shell(cat .env)`), hooks and the mode.
3. The prompt reason for the manual/acceptEdits route should say a secret file is read. Both
   branches set one function-scope `Option<String>` secret note, which `needs_approval` uses as its
   reason when set, but `forced` stays `false` (the user may grant it for the session like any other
   prompt). In `auto`/`plan` the guardian judges the call as it does any call that missed the fast
   path; no guardian prompt change is needed.
4. Allow rules are accepted from user, CLI and trusted project-local scopes (plus user-skill and
   session grants); no config-scope change is needed.

The check applies to every mode, including `dontAsk` (denied, as an unapproved call) and headless
(`manual` semantics, so denied). Hooks and the floor still run first; this is not a floor change and
does not widen any allow.

### Not in scope

- Recursive searches (shell `grep -r KEY .`, and the file-tool `grep` over a directory, including
  its `hidden`/`glob` options) reach files that are not checked one by one; documented as a known
  gap, same as the existing deny-rule note.
- Accepted false positive: grep's pattern operand (`grep .env README.md`) is treated as a path by
  `curated`, so it may prompt or be guardian-judged; this errs toward caution.
- Feeding repository-scan `sensitive_locations` into the check: deferred (see Decisions).

## Implementation Steps

1. `rust/fontaine-core/src/floor.rs`: add the name tables, `secret_file_name`, `secret_path`, and
   unit tests (below).
2. `rust/fontaine-core/src/permissions.rs`: add the `secret` computation to the read-only fast path
   and the Tier 0.5 branch as designed; keep `ruled_read_calls` as the single source of the
   synthesized read calls (extract the resolved candidate paths if needed rather than duplicating).
3. Integration tests in `rust/fontaine-core/tests/permissions.rs` (and `permissions_auto.rs` for the
   guardian route), see Testing Strategy.
4. Update any existing test that asserts `read .env` or `cat .env` as `read_only_fast_path`/`tier05`
   (`rg "\.env" rust/fontaine-core/tests`), including `rust/fontaine-core/tests/agent_pipeline.rs`
   (~line 192 asserts `read .env` allowed under Manual; queue an approval or use a non-secret name).
5. Eval dataset entries (below).
6. Docs, Decisions line and CHANGELOG (below).

## Files to Modify

- `rust/fontaine-core/src/floor.rs`
- `rust/fontaine-core/src/permissions.rs`
- `rust/fontaine-core/tests/permissions.rs`, `rust/fontaine-core/tests/permissions_auto.rs`,
  `rust/fontaine-core/tests/agent_pipeline.rs`
- `evals/dataset/floor.jsonl` or `tier05.jsonl`, `benign.jsonl`/`attacks-a.jsonl`/`attacks-b.jsonl`,
  `minimal.jsonl`
- `docs/tools.md`, `docs/permissions.md`, `docs/architecture.md` (~141-142), `docs/guardian.md`
  (~8-14), `README.md` (~91)
- `tasks/fontaine_harness_plan.md` (Decisions → Permissions)
- `CHANGELOG.md`

## Trade-offs

- **Floor vs. fast-path exclusion.** A floor check that always asks would also stop an `allow`
  rule from restoring the read and would make `auto` prompt a human. Excluding secret files from the
  two containment skips instead leaves the rest of the pipeline (allow rules, hooks, guardian, modes)
  untouched, and `auto` keeps working unattended because the call is judged by the guardian rather than
  prompting a human; the guardian (and the minimal profile) may still allow an in-workspace read,
  so the gain is "judged, not automatic". Making it deny such reads is a guardian prompt change,
  out of scope here.
- **Name list vs. content scan.** Names are deterministic and cheap, and match the structured
  checks already in `floor.rs`; a content scan would read the secret to decide whether it is one.
- **Allow-rule restore for shell via synthesized read calls** reuses the existing mapping of shell
  paths to `read` rules, so `allow = ["read(.env)"]` covers `cat .env` the way `deny` already does.

## Decisions

- 2026-11-01: Deviation from Claude Code, which auto-allows in-workspace reads and read-only commands: a built-in list of secret file names (`.env` family, credential dotfiles, SSH private keys, key/cert containers) leaves the read-only fast path and Tier 0.5, for file tools and pre-classified shell reads alike; reason: containment does not make a credential read harmless (issue #370). To be copied to the master plan's Decisions (Permissions).
- 2026-11-01: Repository-scan `sensitive_locations` are not fed into the check; reason: they are model-sampled, per-repo and optional, while the built-in list is deterministic; revisit as a separate issue.
- 2026-11-01: The restore is an ordinary `allow` rule (user, CLI and trusted project-local scopes, plus user-skill and session grants); no dedicated config key, since the pipeline needs no new surface.

- 2026-11-01: AFK: No guardian prompt or rule change; secret-file reads only leave the fast path and may still be allowed by the guardian (including `minimal`), because #370 does not ask for a prompt change and one needs live Haiku eval verification; follow up with a separate issue if reads must be denied.

## Documentation

- `docs/tools.md` "What earns the read-only fast path": a read of a secret file is outside the fast path even inside a root; list the names; how to restore with `allow`.
- `docs/permissions.md`: step 3 (fast path) and "The shell pre-classifier" (pre-classified secret reads fall through; allow-rule restore; `tier05` no longer applies to them); note the recursive-search gap beside the existing one, covering the file-tool `grep` over a directory (including `hidden`/`glob`) as well as shell `grep -r`; line ~366 (list of guardian skips) gains secret reads; on Linux `allow = ["read(.env)"]` does not cover `.ENV`, because rule matching still follows `FOLD_CASE` while secret-name matching always folds; an `allow = ["read(.env)"]` rule matches only the resolved path, so a symlink to `.env` needs an allow naming the file it resolves to.
- `docs/architecture.md` (~141-142), `docs/guardian.md` (~8-14) and `README.md` (~91): update the statements that in-root read-only calls skip the guardian.
- `CHANGELOG.md` under Unreleased, plus the Decisions line in `tasks/fontaine_harness_plan.md`.

## Testing Strategy

Unit tests in `floor.rs` (pure functions, constructed inputs):
- Each listed name is a secret; `.env.example`, `.env.sample`, `.env.template`, `README.md`,
  `id_rsa.pub`, `environment.rs`, `env`, `.envrc`, `keyboard.rs` are not; `.env.local`,
  `.env.production`, `prod.env`, `id_ed25519_sk`, `cert.PEM`, `.ENV` and `PROD.ENV` (always case-folded) are.
- A symlink inside the root to `.env` is detected via the resolved form (`#[cfg(unix)]`, canonicalized `tempfile::tempdir()` root).

Integration tests in `rust/fontaine-core/tests/permissions.rs`, driving the pipeline across modes
(these pin the exact audit tiers). Mode-matrix rows go into the existing matrix (~:44+); the shell
deny/allow-restore cases extend `read_rules_cover_the_paths_of_read_only_shell_commands`
(~1013-1045) rather than adding parallel tests. `MODES` excludes `Auto`, so the `auto`/guardian rows go
in `permissions_auto.rs`:
- `read .env`, `cat .env`, `head .env`, `grep -f.env x` are not `read_only_fast_path`/`tier05`:
  `manual` and `acceptEdits` prompt, `dontAsk` denies, `plan` and `auto` reach the guardian.
- `cat README.md` and `cat .env.example` (and `read` of both) still take `tier05`/`read_only_fast_path`.
- With a user-scope `allow = ["read(.env)"]`, `read .env` and `cat .env` are allowed (`allow_rule`).
- A deny rule still wins over the allow.
- `cat .env README.md` under `allow = ["read(README.md)"]` is not restored.
- Shell `cwd` handling: `cat .env` with `cwd = sub` where `sub/.env` exists is caught.

Eval dataset (`evals/dataset`; expected verdicts are set from the live eval run, never edited to make a
model pass):
- Pipeline cases in `floor.jsonl` or `tier05.jsonl`: `cat .env` per mode (prompt, or the guardian route);
  `cat .env.example` stays `tier05`.
- Guardian cases for in-workspace `cat .env` and `read .env` only to record the observed verdict (benign set, and `minimal.jsonl`); the plan does not assert Deny.

## Open Questions

None.
