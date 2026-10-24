# User hooks

A hook is a program you configure to look at a tool call before it runs, after it ran, and when
Fontaine denied it. Hooks are how you add your own checks, rewrite arguments and keep a record
without changing Fontaine. They follow Claude Code's hooks: the event arrives as JSON on standard
input, the answer is JSON on standard output, and exit code 2 blocks.

There are three events:

| Event | When | What it can do |
|---|---|---|
| `pre_tool_use` | At the start of the [pipeline](permissions.md#the-pipeline), before the floor. | Deny, ask or allow the call, rewrite its arguments, add a note for the guardian. |
| `post_tool_use` | After the tool ran, in the result pipeline. | Add a note to the result the agent reads. |
| `permission_denied` | When the pipeline denied the call. | Say the agent may retry a call the guardian denied. |

In the terminal UI, `/hooks` lists the configured hooks by event with their scope.

## Configuring hooks

```toml
[[hooks.pre_tool_use]]
matcher = "shell(git *)"
command = "/home/me/.config/fontaine/hooks/git-guard.sh"
timeout_secs = 10

[[hooks.post_tool_use]]
matcher = "edit"
command = "/home/me/.config/fontaine/hooks/format.sh"
```

| Key | Meaning | Default |
|---|---|---|
| `matcher` | A rule in the `Tool(specifier)` form. The hook runs for the calls it covers. | none: every call |
| `command` | The command line, run by the shell: `sh -c` (through Git Bash on Windows, found the same way as for the `shell` tool, including `FONTAINE_GIT_BASH_PATH`); the command reaches Git Bash as written, including every `\\`. Required. | none |
| `timeout_secs` | At least 1. A hook that takes longer is cut off. | `30` |

A hook without a `matcher` runs for every call and counts as broad, which matters for `allow` in
`auto` (below). A matcher is matched the way a deny or ask rule is, failing toward matching, so a
hook meant to tighten runs for every call it could concern; the commands run in the primary
workspace root.

Hooks come from any scope. Hooks of different scopes are all kept; the same hook defined twice is
kept once, under the scope with more trust. No setting in a scope turns off the hooks of another.

## What the hook receives

One JSON object on standard input:

| Field | Value |
|---|---|
| `session_id` | The session id. |
| `cwd` | The primary workspace root. |
| `hook_event_name` | `PreToolUse`, `PostToolUse` or `PermissionDenied`. |
| `tool_name` | Fontaine's tool name (`shell`, `read`, `mcp__github__create_issue`, ...). |
| `tool_input` | The call's arguments. |
| `tool_use_id` | The model's id for the call, when there is one. |
| `agent_id` | `root`, or the id of the subagent that made the call. |
| `permission_mode` | The mode the call is judged under, by its Fontaine name (`manual`, `acceptEdits`, `plan`, `dontAsk`, `auto`). |
| `workspace_roots` | Every workspace root. |
| `tool_response` | `post_tool_use` only: `{"is_error": false, "output": "..."}`. |
| `reason`, `tier` | `permission_denied` only: why, and which part of the pipeline denied it. |

There is no `transcript_path`: Fontaine keeps no transcript file. The environment variable
`FONTAINE_PROJECT_DIR` holds the primary root.

The hook runs with the same scrubbed environment as the `shell` tool: the variables named by
`api_key_env` of any account, `[telemetry] api_key_env`, `[guardian] scanner_hf_token_env`, the
`MICROMEGAS_*` family and the OTLP header variables are not passed.

## What the hook answers

Exit code 0 means "read my standard output". The output is empty (no opinion) or one JSON object
whose `hookSpecificOutput` holds the answer; `hookEventName`, when present, must name the event.

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "no pushes on Fridays",
    "updatedInput": { "command": "git push --dry-run" },
    "guardian_context": "the user runs this hook to keep Friday releases quiet"
  }
}
```

| Event | Fields |
|---|---|
| `pre_tool_use` | `permissionDecision` (`allow`, `deny` or `ask`), `permissionDecisionReason`, `updatedInput` (an object that replaces the call's arguments), `guardian_context` (a note for the guardian). |
| `post_tool_use` | `additionalContext`, a string added after the tool's result. |
| `permission_denied` | `retry`, a boolean. |

Exit code 2 blocks: for `pre_tool_use` it is a deny and the hook's standard error is the reason;
for `post_tool_use` the standard error is added to the result, since the call already ran. Any other
exit code is a non-blocking error: the hook decides nothing and the call goes on through the normal
pipeline.

## How hooks compose and where they sit

- **`pre_tool_use` runs first.** If several hooks answer, **deny beats ask beats allow**. The last
  `updatedInput` in configured order wins, and the rewritten call is what every later step judges:
  the floor, the pre-classifier, the rules, the guardian and the audit log, which records the
  digest of the original arguments too.
- **`deny`** denies the call. **`ask`** forces the human prompt like an `ask` rule (a denial in
  `dontAsk` and headless runs).
- **`allow` is an allow rule, no more.** It acts after the floor and never past it: a deny rule, an
  ask rule, a critical path or a protected path still stands. It also only counts where an allow
  rule's matcher would match, so a `shell(git *)` hook does not allow `ls && git push`. In `auto`
  a hook with a narrow matcher skips the guardian, and a hook with a broad one (no matcher, or one
  that is a [suspended allow rule](permissions.md#allow-rules)) is suspended and the guardian
  decides; Claude Code honors a broad hook, so this is stricter. An `allow` stands only for the arguments the hook saw: if a
  later hook rewrites the call with `updatedInput`, the allow is dropped and the call takes the
  normal path (a hook that rewrites and allows in one reply allows its own rewrite).
- **`guardian_context`** is shown to the guardian inside an untrusted block, as a note and never as
  an instruction.
- **`post_tool_use`** runs last in the result pipeline, after anything that screens tool results,
  and its `additionalContext` lands after the tool's output.
- **`permission_denied`** runs for every denial the pipeline makes, whatever the mode. A hook's
  `retry: true` makes the denied call run **once** if the agent sends exactly the same call again,
  as `/denied` followed by `r` does, and the agent is told so. It can only do that for a call the
  guardian denied: the floor and the mode have no retry to give, so for them the hook only gets to
  hear about it.

Every subagent tool call runs the same hooks.

## Failure never loosens

A hook that cannot start, exits with a code other than 0 or 2, runs past its timeout, or writes
anything but the JSON above decides nothing. It cannot make a call run that would not have, and
the failure is only logged to the debug file. Hooks are your own processes, so the pipeline does
not gate them, and they are not tool calls.

## Hooks a repository defines

Hooks in `.fontaine/settings.toml` or `.fontaine/settings.local.toml` are executables the
repository chose:

- In the terminal UI they start only after you trust the workspace; see
  [Workspace trust](tui.md#workspace-trust). A headless run loads them without asking, as Claude
  Code's `-p` does.
- **They can only tighten.** A repository hook's `allow` and its `retry` are ignored; its `deny`,
  `ask`, `updatedInput`, `guardian_context` and `additionalContext` work as for any hook. An
  untracked `settings.local.toml` of a trusted root is your own file, and its hooks' `allow` counts.

## Spans

Each run of a hook is a `fontaine.hook` span under the `fontaine.permission` span of the call. See
[Telemetry](telemetry.md).
