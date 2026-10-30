# Permissions

Every tool call crosses one permission pipeline before the tool runs. The pipeline decides from
structured facts about the call (the tool, its risk class, the parsed shell command, the resolved
paths), never from a pattern over raw arguments. A denial goes back to the model as the tool
result and the run goes on.

## Modes

The session mode is set by `[permissions] default_mode` or `--permission-mode`, and in the
terminal UI Shift+Tab cycles it, and `/plan` enters plan mode. With neither set, the terminal UI starts in `auto` and a headless
run (`fontaine -p`) starts in `manual`, as Claude Code's `-p` starts in `default`.

| Mode | What happens to a call nothing pre-approved |
|---|---|
| `manual` | The human is asked. |
| `acceptEdits` | `write`, `edit` and `notebook_edit` inside the workspace roots run; everything else asks. |
| `plan` | No edit ever runs: `write`, `edit` and `notebook_edit` are blocked outright. Other consequential calls (write-capable or unparsed `shell`, `webfetch`, `websearch`, every MCP call, reads outside the roots) go to the guardian, as in `auto`. |
| `dontAsk` | Denied. Only calls that are pre-approved run. |
| `auto` | The [guardian](guardian.md) decides: allow, deny with a reason, or ask you. Edits to tracked, new and session-created files and file-tool writes into scratch skip it (step 6 below). |

In the terminal UI, `/permissions` shows the mode and the effective rules with the scope each came
from, including session and always grants; in `auto` it marks the broad allow rules that mode sets
aside.

There is no bypass mode. The deterministic floor below applies in every mode, and no mode, flag or
allow rule reaches past it. In a headless run (`fontaine -p`) nothing can ask, so every call that
would ask is denied instead; see [Headless runs](headless.md#denials).

## The pipeline

A call goes through these steps in order and the first one that decides wins.

A call from an agent the session no longer knows (its id was never registered, or it or an
ancestor has been released) is denied in every mode before step 1: no hook or guardian sees it.

1. **Hooks.** Pre-tool-use hooks may deny, ask, allow or rewrite the call's arguments; every later
   step judges the rewritten call. Deny beats ask beats allow. A hook that fails, times out or
   answers something unreadable decides nothing. Hooks are configured under `[[hooks.*]]`; see
   [User hooks](hooks.md).
2. **The deterministic floor**, in this order:
   - a `[permissions] deny` rule: denied.
   - in `plan`, an edit tool: blocked.
   - an `ask` rule, a hook's ask, or an MCP tool its server
     [marked as requiring your approval](mcp.md#requiring-approval-for-a-tool): asks.
   - **critical path**: a `shell` command that removes the file system root, a top-level
     directory, your home directory, a workspace root or a parent of one asks. The command is
     parsed with a real shell parser, and every `rm` and `rmdir` word in any segment of a list,
     pipe or substitution is inspected. The string a launcher runs as a command line (`sh -c`,
     `su -c`, `watch`, `flock -c`, `script -c`, `env -S`, `ssh host`) is parsed too; other quoted
     text is data. A command name built from an expansion counts as critical.
     A target that cannot be resolved (a variable, a glob, a tilde, a brace pattern), a command that does not parse, and an `rm` inside a command that is
     not one simple command all count as critical.
   - **protected path**: `write`, `edit` and `notebook_edit` on a path with a component named `.git`, `.vscode`,
     `.idea`, `.husky`, `.fontaine` or `.claude`, or whose file name is `.gitconfig`, `.gitmodules`,
     `.bashrc`, `.bash_profile`, `.zshrc`, `.zprofile`, `.profile`, `.ripgreprc` or `.mcp.json`,
     in any workspace root or the home directory, asks; so does any path under Fontaine's own
     config or data directory (the [auto-memory store](memory.md) is the one gated location under the
     data directory that is also reachable as a workspace root, and it takes this check like any other).
     Matching is on resolved path components, so separators and, on
     Windows and macOS, case do not matter. In `auto` the guardian takes the place of the
     prompt, for a path the file tools can reach.

   A floor outcome that asks is a denial in `dontAsk` and in headless runs. In `auto` an `ask`
   rule, a server's approval marker and a critical path still ask you: no verdict approves them.
3. **Read-only fast path.** A read-only tool (`read`, `ls`, `glob`, `grep`, `lsp`) whose paths all
   resolve inside a workspace root runs. Symlinks are resolved first. The label alone earns
   nothing: a path outside the roots does not qualify, and an external read never takes this
   step. `lsp` has no external route at all: a path outside every root is denied, with the
   `/add-dir` hint. `skill`, `ask_user_question`, `todo_write` and the read-only scheduling tools name
   no path and take this step too. `ask_user_question` only shows you text and collects your choice,
   and no mode, `auto` included, ever answers it: the answer comes from you or, in a headless run,
   from nobody (the model is told to use its best judgment). Your typed "Other" answers count as
   intent for the guardian; the options you pick do not.
   A file-tool *write* (`write`, `edit`, `notebook_edit`) that names a path outside every root
   is denied, with a hint naming the session's scratch directory and `/add-dir`: the write tools
   are confined to the roots, so an approval could never make the call run. The scratch
   directory is an ordinary root (see [Tools](tools.md#the-scratch-directory)): this step and the
   floor treat a path in it like one in the workspace, and `shell` treats it like any root:
   read-only commands whose operands are inside it take the pre-classifier fast path, and other
   commands go through the guardian as usual. Claude Code prompts for a write outside the workspace; Fontaine denies
   it and offers the scratch directory instead.

   An out-of-root **read** by `read`, `ls`, `glob` or `grep` is not denied here: it falls through
   to the same steps as any other call nobody pre-approved. After the hooks, the target is
   resolved once — symlinks followed — and that resolved absolute path is what the approval
   prompt, the guardian and the execution all use, so retargeting a symlink mid-decision cannot
   redirect an approved call, and deny and ask rules still match the path as written. In
   `manual` and `acceptEdits` it asks; in `auto` and `plan` the guardian decides; in `dontAsk`
   and in headless runs it is denied. An allow rule or a hook written for the resolved path runs
   it at the `allow_rule` or `hook` tier; it is never audited as `read_only_fast_path` or
   `tier05`. Malformed arguments (a missing or non-string path) are an immediate argument
   error after the floor: never a prompt, never the guardian. Execution uses one unconfined,
   read-only fallback instance per tool, reached only after the gate allows the call; writes
   and `shell` have no such route (see [Tools](tools.md#workspace-roots)). An external path does
   not become a root: it grants no write reach, no fast path and no instruction-file discovery.
4. **Shell pre-classifier**, below.
5. **Allow rules**, below.
6. **Edits that skip the guardian (`auto` only).** After the floor and the hooks, a `write`,
   `edit` or `notebook_edit` call is allowed without a guardian verdict in two cases. A file-tool
   write whose resolved target is inside the session scratch directory is audited as
   `tier = scratch_write`; a shell command that touches scratch is not a file-tool call and goes
   to the guardian. An edit inside the primary workspace root is audited as `tier = tracked_edit`
   when the target is a file git tracks (an exact entry in the index for the resolved path), a path
   that does not exist yet in a git work tree, or a file this session created. Symlinks are
   resolved first, so a link is judged by its target. These go to the guardian as before: an
   existing file git does not track, such as `.env` or uncommitted work that predates the
   session; any path in a root that is not a git work tree; the auto-memory store and `/add-dir`
   roots; the protected paths above; instruction files (`FONTAINE.md`,
   `FONTAINE.local.md`, `CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md`), anything under a `rules`
   directory and anything under `.github`, because an edit there changes later decisions or what
   CI runs; and every shell command. A request in your own message that names an excluded file
   clears the guardian's soft deny for it. `@import`ed files are not excluded. Both allows
   end a run of consecutive guardian denials, so routine work between denials keeps `auto` from
   pausing. Turn them off with
   [`auto_tracked_edits` and `auto_scratch_writes`](configuration.md#permissions); project scope
   may only switch them off.
7. **The mode**, as in the table above. In `auto`, and for the consequential calls of `plan`,
   this is the guardian: it answers allow, deny or escalate. An allow runs the call; a deny comes
   back to the agent as `Denied by guardian: [Rule] explanation`; an escalate asks you with the
   guardian's explanation (and is a denial in a headless run). Three denials in a row, or twenty
   in a session, pause `auto` and prompting resumes until you approve a call.

Every decision is appended to the audit log (`<data dir>/audit/<session id>.jsonl`, readable by the
current user only) before anything else sees it: the time, session, agent and tool-call ids, the
tool, a SHA-256 digest of the arguments (never the arguments), the `tier` that decided, the
`decision` (`allow`, `deny` or `prompt`), the rule and its source, and the latency. A human's answer
to a prompt adds a second record with `tier = human`. The same record is projected to the
`tool_decision` telemetry event and to the headless `gate_decision` event, so the three agree. The
`tier` is one of `deny_rule`, `ask_rule`, `critical_path`, `protected_path`, `hook`,
`read_only_fast_path`, `tracked_edit`, `scratch_write`, `tier05`, `allow_rule`, `mode`, `screen`,
`guardian`, `dont_ask`, `plan_block` and `human`. A guardian decision also records the guardian model and profile, its
verdict, confidence, whether the verdict came from the cache, how long the guardian took, the
`verdict_transport` it was asked on (`tool`, a forced `submit` call, or `schema`, for an Ollama
model with no tool support; see [Models with no tool
support](guardian.md#models-with-no-tool-support)) and, when it failed closed, the reason (`error`,
`timeout`, `unparseable`, `low_confidence` or `unavailable`). With a [screen
stage](guardian.md#the-screen-stage) configured, the record also carries `screen_risk`,
`screen_allow_risk`, `screen_steering`, `screen_model`, `screen_latency_ms` and `screen_outcome`,
and a call the screen stopped or, with an `allow_threshold`, allowed has `tier = screen` (`decision
= prompt` or `deny` for a stop, `allow` for an allow). When an enforcing screen settles a call while
the guardian request that raced it is still pending, the record also carries
`guardian_dropped_input_tokens`, an estimate of that request's input, with
`guardian_dropped_provider`, `guardian_dropped_model` and `guardian_dropped_priced`. They are
projected onto `tool_decision` as `guardian.dropped_input_tokens`, `guardian.dropped_provider`,
`guardian.dropped_model` and `guardian.dropped_priced`; see [Telemetry](telemetry.md#metrics).

## Background jobs and monitors

`shell` is judged when it is called; a command that later moves to the background, or that was
started with `run_in_background`, is not asked about again. `job_output` is read-only and names no
path, so it takes the fast path. `job_cancel` is an `arbitrary` tool that can only signal a process
this session started, by id. `monitor` is an `arbitrary` tool with `shell`'s command analysis: the
critical-path floor, the destructive class and `shell` deny and ask rules (and hook matchers) cover
it, so a `shell(git push)` deny also stops a `monitor` that runs `git push`. An allow rule does not
cross over: `shell(...)` allows are for `shell` only, and the read-only pre-classifier never
allows a `monitor` by itself, so allow one with `monitor(tail *)` or let the guardian judge it. In
`auto`, `monitor` with no specifier is a broad allow like `shell`, and is set aside.

## Scheduled prompts

`cron_create` is an `arbitrary` tool, and always a destructive call for the guardian (so a low-confidence allow is denied), even though it touches no file: it plants model-written text
that will later run as a turn with nobody typing anything. In `auto` the guardian judges the call,
in `manual` you are asked, and the floor (a `deny` or `ask` rule naming `cron_create`, or a hook)
runs first as for any tool. `cron_jobs`, `cron_delete` and `schedule_wakeup` are read-only: they
list or cancel scheduled tasks, or re-time the prompt you gave a self-paced `/loop`, and name no
path.

A task that comes due runs as a turn the harness starts, framed by a note saying it is a scheduled
task and not you. It is never recorded as a user message, so it cannot add to what the guardian
takes as your intent; the intent stays what you typed, including the `/loop` line that started a
loop. The text of a task the model scheduled also passes through the tool-result scanner before the
model reads it. Calls the task's turn makes go through the same pipeline as any other. `/clear`
cancels every scheduled task, because it drops the intent they were scheduled under. See
[Scheduled prompts](tui.md#scheduled-prompts).

A [goal](tui.md#goals) turn is a harness turn in the same way: the evaluator's reason and the goal's
condition arrive behind a note saying they are not you and do not widen what you asked, no goal turn
is recorded as a user message, and only the `/goal` line you typed counts as your intent. The
evaluator's reason passes through the tool-result scanner first. A goal does not change the mode,
and its calls go through the same pipeline.

## Your own shell commands

A command you run in the terminal UI with `!` is your own action, not a tool call: it bypasses the
pipeline above, including the floor and the guardian, and runs as you. It is still recorded. The
audit log gets one record with `tier = human`, `decision = allow`, `rule = user_shell`, `tool =
shell` and `agent_id = user`, and the session ledger gets a `user_shell` record. See
[Shell commands](tui.md#shell-commands).

## Rewinding

[`/rewind`](checkpoints.md) restores files from the copies `write` and `edit` saved. Like a `!`
command it is your own action, taken in the terminal: it does not pass through the pipeline above,
and the model has no tool that can request it. Because it writes on your behalf it checks each path
against the workspace roots and refuses symbolic links, and it never touches the memory directory.

## Rules

`[permissions] deny`, `ask` and `allow` (and `--disallowed-tools` / `--allowed-tools`) share one
grammar: `Tool` or `Tool(specifier)`. A bare tool name matches every call of the tool.

| Tool | Specifier | Matches |
|---|---|---|
| `shell` | words, optionally ending in ` *`: `shell(git push)`, `shell(npm run test *)` | the parsed command line; quoting keeps a word whole; no wildcard inside a word |
| `read`, `edit`, `write`, `notebook_edit`, `ls`, `glob`, `grep`, `lsp` | a gitignore-style path pattern: `src/**`, `~/.aws/**`, `//etc/hosts` | the resolved path; relative patterns anchor at each workspace root, `~/` at your home (a deny or ask rule with `~/` matches everything when no home directory can be found; an allow rule matches nothing), `//` at the file system root |
| `webfetch` | `domain:example.com` or `domain:*.example.com` | the URL's host; `*.` matches subdomains only |
| `websearch` | none | the tool name; the destination is the configured backend, not a host the model picks, so `domain:` does not apply |
| `mcp__<server>`, `mcp__<server>__<tool>`, `mcp__<server>__*` | none | the registered tool name |
| `agent`, `skill` | the agent type or skill name | exact name |
| `monitor` | words, as for `shell` | the parsed command line, as for `shell`; a `shell` deny or ask rule covers `monitor` too, an allow rule does not |
| `send_message`, `ask_user_question`, `todo_write`, `cron_create`, `cron_jobs`, `cron_delete`, `schedule_wakeup`, `job_output`, `job_cancel` | none | the tool name |

Deny and ask rules fail toward matching; allow rules fail toward not matching:

- A `shell` deny or ask rule matches when its words appear contiguously anywhere in the parsed
  command, so `shell(git push)` also stops `ls && git push` and `sudo git push -f`. A command that
  does not parse, or that puts an expansion where the command name goes, matches every rule whose
  first word it contains.
- A `shell` allow rule matches only one simple command whose words are all literal and whose words
  equal the rule (or start with it, for a trailing ` *`). Chains, pipes, redirections,
  substitutions, globs and variables never match. Per-subcommand rules as Claude Code has them are
  not offered.
- Path deny and ask rules match the path as written and the resolved path, so a symlink into a
  denied path is denied. Path allow rules match only the resolved path, so an allow on a symlinked
  alias does not apply.
- A `read` deny rule also keeps a file out of the [instruction files](instructions.md#what-is-refused)
  the system prompt is built from, so a rule that stops the agent reading `secrets/**` stops
  `CLAUDE.md` from importing it too.
- A bare `websearch` rule also governs the provider's own search: while a `deny` or `ask` rule
  covers `websearch`, or a `pre_tool_use` hook could (it has no `matcher`, its `matcher` names
  `websearch`, or its matcher cannot be read), it is not offered to the model, and in `manual`, `acceptEdits` and `dontAsk`
  it is offered only when an `allow` rule exists. In `auto` and `plan` it is offered otherwise.
  Because it runs inside the model's response, an offered provider search is checked once, when the
  request is built, not per query. A mode switch or a new rule takes effect on the next turn, so
  tightening the mode mid-turn (Shift+Tab, or the fallback when the guardian is unavailable) leaves
  the provider's search attached until that turn ends; see [Web search](tools.md#web-search).
- A `webfetch` never follows a redirect to another host: the redirect is returned to the model and
  a follow-up fetch is judged on its own host.

An invalid rule is a configuration error (exit code 2) naming the file and key. Rules from the
shared project file may only tighten (`deny`, `ask`); `allow` comes from the user file, an
untracked `settings.local.toml` and the command line. A [plugin's](plugins.md) manifest may add
`deny` and `ask` rules only. The audit log and the `gate_decision` event name the scope that
contributed a rule, for example `shell(rm) [user]`; a plugin rule reads `[plugin]`.

### Allow rules

An allow rule acts after the floor and never past it: a deny, ask, critical-path or protected-path
outcome stands whatever allow says, and `plan` still blocks edits. A matching allow rule skips the
prompt in `manual` and `acceptEdits`, pre-approves in `dontAsk`, and skips the guardian in `auto`.

While `auto` is on, **broad code-execution allow rules are suspended**: a bare `shell` or `agent`,
and `shell(<entry>)` or `shell(<entry> *)` for `sh`, `bash`, `zsh`, `python`, `node`,
`ruby`, `perl`, `deno`, `bun`, `cargo run`, `npm run`, `npm exec`, `npx`, `bunx`, `pnpm`, `yarn`, `make`, `eval`,
`exec`, `xargs`, `env`, `sudo`, `doas`, the interpreters `awk`, `gawk`, `mawk`, `nawk`, `php`, `lua`, `luajit`, `tclsh` and `wish`, the Windows shells `pwsh`, `powershell` and `cmd`, and the launchers `timeout`, `nohup`, `setsid`, `nice`, `stdbuf`, `chroot`, `command`, `busybox`, `ionice`, `watch` and `flock`. A wildcard rule is suspended whenever it can match something an entry covers: an interpreter counts by its program alone, so `shell(python -c *)`,
`shell(/usr/bin/python3 *)` and `shell(python3.12 *)` are suspended too, and so are `shell(cargo *)` and `shell(npm *)`, which reach `cargo run` and `npm run`. `shell(cargo test *)` stays. A call they would have approved goes to the guardian instead.
Narrower rules such as `shell(npm test)` stay. The terminal UI lists the suspended rules when you
enter `auto`, and a headless run lists them in the `init` event; leaving `auto` restores them. A
`pre_tool_use` hook's `allow` follows the same test on the hook's matcher.

The approval prompt can create allow rules:

- **Allow once** runs this call.
- **Allow session** adds the rule shown in the prompt for the rest of the session. It is recorded
  in the session ledger and never written to disk.
- **Always allow** appends the same rule to `[permissions] allow` in your user settings file,
  keeping the file's comments and layout, and applies it at once.

The diff an `edit`, `write` or `notebook_edit` prompt shows is display-only: it is not sent to the guardian, hooks,
telemetry or the ledger, and it changes no decision.

The proposed rule is the narrowest that covers the call: the exact command words for `shell`, the
resolved absolute path for a file tool, the host for `webfetch`. Prompts that the floor forced, and
`shell` calls that are not one literal simple command, offer only allow once and deny, because no
allow rule can act there.

## The shell pre-classifier

A small set of read-only commands runs without a prompt when the call is one simple command made of
literal words, parsed by a real shell parser, with every path inside the workspace roots. The
call's `cwd` is resolved first, and a `cwd` outside the roots disqualifies the call. Anything not
fully understood falls through to the next step: a chain, pipe, redirection, background marker,
substitution, variable, tilde, glob, brace pattern or assignment prefix.

| Command | Allowed flags | Values checked as in-scope paths |
|---|---|---|
| `ls` | `-a -A -l -h -1 -d -t -r -S -F` | positionals |
| `pwd` | none | |
| `wc` | `-l -w -c -m` | positionals |
| `file` | `-b -i --mime --mime-type --brief -m FILE --magic-file FILE` | positionals and the `-m` value |
| `cat` | `-n -b -s` | positionals |
| `head`, `tail` | `-n N -c N -q -v` | positionals |
| `rg` | `-i -s -S -n -N -H -w -x -F -v -c -l -A N -B N -C N -m N -t TYPE -T TYPE --hidden --no-heading -f FILE --file FILE --ignore-file FILE` | positionals and file values |
| `grep` | `-i -n -v -w -x -F -E -c -l -L -H -h -o -q -s -r -A N -B N -C N -f FILE --file FILE` | positionals and file values |

Any other flag, any `--flag=value` whose flag is not listed, and any short-flag cluster with an
unlisted letter falls through. `ls -la` is accepted as `-l -a`. A value-taking short flag takes an
attached value (`-n5`) or, as the last letter of a cluster, the next word (`-nA 3`). Numbers must
parse as integers. Excluded on purpose: `tail -f`, `file -f`, `rg --pre`, `rg -z`, `rg -L`,
`rg -g`, `grep -R` and `grep --exclude-from`, because they run programs, follow links or read
indirectly. No `git` subcommand is in the set, because repository configuration can make a
read-only subcommand run a program.

`[shell_preclassifier] allow_extra` adds commands to the set: a command (`make`) or a `command
subcommand` pair (`cargo metadata`). It is accepted from user-trust scopes only, an extra entry
takes no flags (any flag falls through), and its positional words must be in-scope paths. A grant
from the prompt never extends the set.

Each word of the command that could be a path
(including the value of `--flag=value`), as written and resolved against the `cwd`, is checked
against the `read` deny and ask rules, and against the `ls` or `grep` rules for those commands. A
deny match denies the call and an ask match asks, so `deny = ["read(.env)"]` also stops `cat .env`. Only the words written in the command are
checked: files a recursive search reaches under a directory (`grep -r KEY .`) are not checked
one by one. `read` deny rules are not a containment boundary for shell; use a `shell` deny or
ask rule, or the protected paths.

A pre-classified call is audited with `tier = tier05`. In `auto` these calls, the read-only
fast path and the [tracked and scratch edits](#the-pipeline) are the only guardian skips earned without a grant of yours; an out-of-scope read such
as `ls ../`, anything the pre-classifier cannot fully parse, and every MCP tool go to the guardian.
