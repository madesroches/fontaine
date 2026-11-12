# Safe Read-Only Fast Path for Git Commands Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/61

## Overview

Restore read-only `git` subcommands to the Tier-0.5 shell pre-classifier by executing them through `gix` instead of spawning the `git` binary. Because `gix` opens the repository with isolated, strict configuration, repository-controlled helpers (fsmonitor, filter drivers, gpg.program, textconv, ext-diff) cannot execute, so the golden adversarial cases stay green while common read-only git operations regain the fast path.

## Current State

- `rust/fontaine-tools/src/preclassify.rs` maintains the curated Tier-0.5 command set (`ls`, `cat`, `rg`, `grep`, etc.) and explicitly excludes every `git` subcommand.
- `rust/fontaine-core/src/permissions.rs` runs `Preclassifier::read_paths` for `shell` calls; when it returns `Some(paths)` and no secret/deny/ask rule applies, the call is allowed at `Tier::Tier05`.
- The eval dataset `evals/dataset/tier05.jsonl` already contains adversarial git cases (`git status` with `core.fsmonitor`, `git log` with `log.showSignature` + `gpg.program`, `git diff` with a `.gitattributes` filter driver) that expect `prompt`/`mode`, not `tier05`.
- The codebase already uses `gix` with isolated options for safe git reads in `rust/fontaine-core/src/git_head.rs` and `rust/fontaine-core/src/trust.rs`.
- `gix` is a workspace dependency (`rust/Cargo.toml`) and is already depended on by `fontaine-core`.

## Design

Add a new `git` subcommand spec to the pre-classifier. Instead of running the user's shell command literally, the pipeline detects a supported read-only git invocation, re-executes the equivalent operation through `gix`, and allows it at `tier05` only when `gix` succeeds and every referenced path stays inside the workspace roots.

### Supported subcommands (phase 1)

| Subcommand | Mapped gix operation | Allowed flags / args |
|---|---|---|
| `git status` | Worktree status via `gix::status` or `gix::index` + HEAD comparison | none (bare `git status`) |
| `git diff` | Diff index/worktree or HEAD/index via `gix::diff` | `--cached`, `--staged`, path positionals |
| `git log` | Walk commits from `HEAD` via `gix::revision::walk` | `-n N`, `--max-count=N`, path positionals |
| `git show` | Show a commit or object via `gix::object` lookup | `<rev>[:<path>]`, `--stat` |
| `git ls-files` | List index entries via `gix::index` | none |
| `git branch` | List local branches via `gix::references` | `-a`, `-r`, `-v`, `-vv` |
| `git rev-parse HEAD` | Resolve HEAD to an object id | `HEAD` only |
| `git tag` | List tags via `gix::references` | `-l`, `-n N` |

Everything else falls through to the guardian, including any flag not explicitly allowlisted, any chained/piped/redirected command, any variable or glob, and any subcommand not in the table.

### Safety architecture

1. **No `git` binary spawn.** The fast path never executes `/usr/bin/git` or a repo-relative `git`; it uses `gix::discover_opts(path, Options::default(), Options::isolated().strict_config(true))` to open the repository. This neutralizes `.git/config` helpers and `.gitattributes` drivers because `gix` does not run them in the operations we use.
2. **Path containment.** Every path argument is resolved against the call's `cwd` and verified to be inside a workspace root using the existing `Environment::in_scope` helper. If any path is outside, the call falls through.
3. **No output capture to files.** Any redirect or flag that writes to a file (`--output`, `--ext-diff`, `-o`) is rejected; the pre-classifier only allows commands whose output goes to stdout/stderr of the shell tool.
4. **Literal-only arguments.** The existing `ShellAnalysis` `simple` check guarantees the command is one simple command of literal words; substitutions, globs, and assignments already fall through.
5. **Fail-closed on gix errors.** If `gix` cannot open the repository, parse the command, or perform the operation, the call falls through to the guardian rather than being denied.

### Pre-classifier changes

Introduce a `GitSpec` alongside the existing `Spec` in `preclassify.rs`:

```rust
struct GitSpec {
    subcommand: &'static str,
    short: &'static [(char, GitVal)],
    long: &'static [(&'static str, GitVal)],
    positional_kind: GitVal,
}

enum GitVal {
    None,
    Int,
    Rev,       // a literal revision name (HEAD, branch, commit-ish)
    Path,      // a workspace-relative path
}
```

The `Preclassifier` gains a `git_specs: Vec<GitSpec>` initialized with the table above. `read_paths` is extended:

1. Parse the command with `analyze`.
2. If `argv[0]` is `git` (or a full path whose base name is `git`), look up `argv[1]` as a subcommand.
3. Validate flags and positional kinds using the `GitSpec`.
4. Collect every `Path` positional and verify `env.in_scope(base, arg)`.
5. Return `Some(paths)` only when all checks pass and the command is fully understood.

The `permissions.rs` pipeline already applies `ruled_read_calls` and `secret_read_calls` to the returned paths, so git commands reading secret files or matching deny/ask rules behave like other pre-classified reads.

### Execution path

When the pre-classifier returns `Some(paths)` for a git command, the pipeline allows it at `Tier::Tier05` **and** rewrites the execution so the shell tool runs the gix-based implementation instead of the raw command. To keep the implementation in `fontaine-core` (where `gix` is available) and the pre-classifier in `fontaine-tools` (which stays dependency-light):

- `fontaine-tools` exposes a new `preclassify::GitCommand` enum describing the parsed git command.
- `fontaine-core` implements `git_fast_path::run(cmd, repo_root) -> Result<String, GitFastPathError>` using `gix`.
- The shell tool (`rust/fontaine-tools/src/command_shell.rs` or its gate wrapper) checks whether the pipeline marked the call as a git fast path and, if so, calls the `git_fast_path` runner and returns its stdout as the tool result.

The actual user-visible output should match `git` closely enough for agent consumption; exact fidelity is not required because the fast path is only taken for read-only inspection the model uses to understand state.

## Implementation Steps

1. **Model the parsed git command in `fontaine-tools`.**
   - Add `preclassify::GitCommand` enum with variants for each supported subcommand and their args.
   - Add `Preclassifier::parse_git(command, env) -> Option<(GitCommand, Vec<String>)>`.
   - Extend `read_paths` to detect `git` and return paths for git commands (for rule/secret checking).

2. **Add the gix-based runner in `fontaine-core`.**
   - Create `rust/fontaine-core/src/git_fast_path.rs`.
   - Implement `run(cmd: GitCommand, repo_root: &Path) -> Result<String, GitFastPathError>`.
   - Open the repo with `gix::discover_opts(..., Options::isolated().strict_config(true))`.
   - Implement each subcommand using the smallest gix API surface that returns useful output.

3. **Wire the fast path into shell execution.**
   - Extend the shell tool's execution context to carry an optional `git_fast_path` result.
   - When the pipeline allows a `shell` call at `tier05` and the pre-classifier identified it as a git command, run `git_fast_path::run` and return its output.

4. **Update the pre-classifier command table and tests.**
   - Add unit tests in `rust/fontaine-tools/src/preclassify.rs` for parsing/validation of each supported git subcommand and for rejection of unsupported flags and chained commands.

5. **Update adversarial eval expectations.**
   - Change the three existing git config-execution cases in `evals/dataset/tier05.jsonl` to expect `tier05`/`allow` when the workspace contains only benign config, because `gix` ignores those helpers.
   - Add new adversarial cases that confirm unsupported git invocations (e.g., `git diff --ext-diff`, `git log --output=x`, `git status --porcelain=v2` if unsupported, `git push`) still fall through to `mode`.

6. **Add integration tests.**
   - In `rust/fontaine-core/tests/permissions.rs` (or a new test file), create temporary git repositories, issue supported `shell(git ...)` calls, and assert `Tier::Tier05` and correct output.
   - Add a test that a repo with `core.fsmonitor = touch /tmp/pwn` still allows `git status` via the fast path and does not create the canary file.

7. **Update documentation.**
   - Extend the pre-classifier table in `docs/permissions.md` with the supported git subcommands and their allowed flags.
   - Update `docs/guardian.md` if it mentions git explicitly as always guardian-judged.
   - Update `CHANGELOG.md`.

## Files to Modify

- `rust/fontaine-tools/src/preclassify.rs`
- `rust/fontaine-core/src/permissions.rs`
- `rust/fontaine-core/src/tools.rs` (or the shell tool runner)
- `rust/fontaine-tools/src/command_shell.rs`
- `rust/fontaine-core/src/lib.rs` (module export)
- `evals/dataset/tier05.jsonl`
- `docs/permissions.md`
- `docs/guardian.md` (if needed)
- `CHANGELOG.md`
- New: `rust/fontaine-core/src/git_fast_path.rs`
- New: tests for git fast path

## Trade-offs

- **gix vs. launcher profile.** The issue mentions pinning attributes/config via a launcher profile. A launcher profile would require building a sanitized `git` invocation environment (override `HOME`, `XDG_CONFIG_HOME`, pass `-c` overrides for every risky option, filter `.gitattributes`). That surface is large and error-prone; `gix` with isolated options gives the same guarantee with a smaller trusted computing base and aligns with existing `git_head.rs`/`trust.rs` practice.
- **Fidelity vs. safety.** `gix` output may differ slightly from `git` output. The fast path is for model inspection, not for user-facing porcelain, so close-enough output is acceptable. If a user needs exact `git` output, they can still be prompted and run the real binary.
- **Scope of subcommands.** Limiting phase 1 to status/diff/log/show/ls-files/branch/rev-parse/tag keeps the implementation tractable. Additional subcommands (`git blame`, `git stash list`, `git remote -v`) can be added later behind the same safety model.

## Decisions

- Use `gix` isolated/strict open options for all fast-path git reads, matching the existing `git_head.rs` and `trust.rs` patterns.
- Keep the pre-classifier in `fontaine-tools` dependency-light by only parsing git commands there; the actual gix execution lives in `fontaine-core`.
- Reject any flag or subcommand not explicitly allowlisted; do not try to enumerate every unsafe git flag.
- Preserve the existing `tier05` audit tier for pre-classified shell commands rather than introducing a new tier.

## Documentation

- Update `docs/permissions.md` §The shell pre-classifier to list the supported git subcommands and note that they run through `gix` with isolated config, so repo config cannot execute programs.
- Update `docs/guardian.md` if it states that git commands always reach the guardian.
- Add a changelog entry under the unreleased section.

## Testing Strategy

- **Unit tests** in `fontaine-tools/src/preclassify.rs` for parsing each supported git subcommand, rejecting unsupported flags, chaining, and out-of-scope paths.
- **Unit tests** in `fontaine-core/src/git_fast_path.rs` for gix output against small repositories created with `gix::init`.
- **Integration tests** driving `PermissionPipeline` with temporary git repositories to assert `Tier::Tier05` for allowed commands and `mode`/`guardian` fall-through for unsupported ones.
- **Adversarial regression tests** confirming the three golden git config-execution cases do not spawn programs and still allow the fast path.
- **Eval dataset updates** so the existing `tier05.jsonl` git cases reflect the new behavior and new adversarial cases cover rejected git invocations.

## Manual Verification

1. In a workspace with a `.git/config` containing `[core] fsmonitor = touch /tmp/fontaine-canary`, run `fontaine -p` and ask the agent to run `git status`. The call should complete without prompting and `/tmp/fontaine-canary` must not be created.
2. Run `git log -n 5` and `git diff --cached` in the same workspace; both should complete via the fast path.
3. Run `git push origin main`; this should fall through to the guardian (or prompt in manual mode) because `push` is not in the supported set.

## Open Questions

- Should `--porcelain` variants of `git status` be supported, or should we require the default human-readable output? (Default: reject `--porcelain` in phase 1; add later if needed.)
- Should `git diff` without path arguments be supported, or only path-restricted diffs? (Default: support both; the whole-repo diff still reads only inside the workspace.)
- Is there an existing shell-tool execution context in `fontaine-core` where the gix runner should be injected, or does the wrapper live in `fontaine-tools` with a callback supplied by `fontaine-core`? (To be resolved during implementation by inspecting `command_shell.rs` and the gate wrapper.)
