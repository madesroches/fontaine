# Tools

The model acts on the workspace through a fixed set of built-in tools. Every one of them is
registered behind a single gate: a call that is not allowed never reaches the tool, and every
result passes one post-call step before the model sees it.

## Built-in tools

| Tool | Risk class | What it does |
|---|---|---|
| `read` | read-only | Reads a text file (at most 16 MB), by line range. A Jupyter notebook (`.ipynb`) is shown as its cells: index, id and type, source, and text outputs (images are noted, not shown). |
| `ls` | read-only | Lists the files under a directory, honouring `.gitignore`. Arguments: `path`, `depth`, `glob`, `hidden`. |
| `glob` | read-only | Finds files whose path matches a glob pattern such as `src/**/*.rs`. Arguments: `pattern`, `path`. Returns at most 1,000 files. |
| `grep` | read-only | Searches file contents with a Rust regular expression, honouring `.gitignore`. Arguments: `pattern`, `path`, `glob`, `hidden`, `files`. Returns at most 200 matches; prefix the pattern with `(?i)` to ignore case. |
| `write` | workspace write | Replaces a whole file. |
| `edit` | workspace write | Replaces text in a file. The file must have been read first. |
| `notebook_edit` | workspace write | Edits one cell of a Jupyter notebook (`.ipynb`): `replace` a cell's source (default), `insert` a new cell after `cell_id` (at the start when none) or `delete` a cell. Arguments: `path`, `cell_id`, `cell_type` (`code` or `markdown`; required to insert), `edit_mode`, `new_source`. Name a cell by its id, or `cell-N` (0-based) when it has none. Replacing a code cell clears its outputs. The notebook must have been read first, and is refused if it changed since. The rest of the notebook and its key order are kept; indentation other than spaces is normalized to nbformat's 1-space form, and number formatting outside the edited cell may be normalized (`1e-05` becomes `0.00001`). A `.ipynb` that does not parse as a notebook (invalid JSON, nbformat 3) is read as plain text. `edit` and `write` still work on a notebook but their descriptions point the model here. |
| `shell` | arbitrary | Runs a shell command. |
| `webfetch` | arbitrary | Fetches one web page. |
| `websearch` | arbitrary | Searches the web through the backend set in [`[websearch]`](configuration.md#websearch); registered when one is configured, or by default when `docker` or `podman` is installed. Arguments: `query`, `allowed_domains`, `blocked_domains`. |
| `agent` | arbitrary | Starts a [subagent](agents.md) and returns its report, or an id when it runs in the background. |
| `skill` | read-only | Loads a [skill](extensions.md#skills) by name into the conversation. |
| `ask_user_question` | read-only | Asks you 1 to 4 multiple-choice questions and returns your answers; see [Questions](tui.md#questions-from-the-model). Only the main conversation can ask. |
| `todo_write` | read-only | Replaces the session task list. Argument: `todos`, a list of up to 50 items of `content` (at most 500 characters) and `status` (`pending`, `in_progress` or `completed`), at most one `in_progress`. An empty list clears it. The [terminal UI](tui.md#the-task-panel) shows the list. |
| `cron_create` | arbitrary | Schedules a prompt to run later in the session, once or on a recurring schedule, when the conversation is idle. Arguments: `cron` (five fields in your local time), `prompt` (at most 2,000 characters) and `recurring` (default true). See [Scheduled prompts](tui.md#scheduled-prompts). Only the main conversation can call it, and only in the terminal UI. Like the other three scheduling tools, it refuses a subagent. |
| `cron_jobs` | read-only | Lists the scheduled tasks: id, cadence, who set it, next run and the start of the prompt. Main conversation only. |
| `cron_delete` | read-only | Cancels a scheduled task. Argument: `id`. Main conversation only. |
| `schedule_wakeup` | read-only | Paces a self-paced `/loop` you started: `delay_seconds` (60 to 3,600) and `reason` pick when the loop's own prompt runs next, or `stop: true` ends the loop. It carries no prompt, so it can re-time text you wrote and never add new text. It fails when no such loop runs. Main conversation only. |
| `send_message` | arbitrary | Sends a queued message to another agent or to the main conversation; see [Subagents](agents.md#messages-between-agents). |

`read`, `write`, `edit` and `shell` are the file and process tools of the rig runtime, configured
rather than modified. `ls`, `glob` and `grep` are Fontaine's own front ends over rig's directory
listing and search. rig's own `list`, `search`, `delete` and `move` are not registered: the
model sees `ls`, `glob` and `grep` instead, and removing or renaming a file goes through `shell`,
which is an arbitrary tool and so is always gated as one.

The system prompt steers the model toward `ls`, `read`, `glob` and `grep` and away from `cat`,
`find` and `grep` inside a shell.

Tools of MCP servers join these at run time, always as arbitrary tools named
`mcp__<server>__<tool>`; see [MCP servers](mcp.md). `agent` and `send_message` are described in
[Subagents](agents.md), and `skill` in [Skills](extensions.md#skills).

## Risk classes

Each tool has a risk class, assigned when it is registered. The class never comes from a hint the
tool declares about itself.

- **read-only**: the tool only reads.
- **workspace write**: the tool writes files, and only inside a workspace root.
- **arbitrary**: the tool can do anything a shell or a network connection can. Tools imported
  from an MCP server are always arbitrary.

### What earns the read-only fast path

A read-only call is trusted only because Fontaine checked, in code, that every path it names lies
inside a workspace root after symlinks are resolved. The label alone earns nothing: reading
`~/.ssh/id_rsa`, a cloud credential file or a browser cookie store is a classic reconnaissance
step, and it is read-only too. A call whose path arguments cannot be read, or that names a path
outside every root, is outside the fast path.
File-tool calls on such a path are denied with a hint to use the session's scratch directory or to
add the directory with `/add-dir` or `--add-dir`; they are never offered for approval.

The `skill`, `ask_user_question`, `todo_write`, `cron_jobs`, `cron_delete` and `schedule_wakeup` tools are the read-only tools that name no path. `skill` takes
only a skill name, resolved against the skills loaded at startup, so all it can do is put that
skill's text in the conversation. `ask_user_question` can only show you text and collect your
choice, and its answer comes from you alone, never from a permission mode. `todo_write` takes only
the task list, which it keeps in the session. The three scheduling tools only list, cancel or re-time
scheduled text; `cron_create` is [arbitrary](permissions.md#scheduled-prompts) because it plants model-written text that
later runs as a turn without you typing anything. None needs a scope check. The deterministic floor
still runs first, so a deny or ask rule or a `pre_tool_use` hook can block any of them. An MCP tool
that declares itself read-only does not get the
same treatment: such tools are registered as arbitrary whatever the server says.

## Workspace roots

The workspace is an ordered set of roots. The first is the primary root: the project directory
(the launch directory, or `--cd`) after symlinks are resolved; relative paths and default
directories refer to it. More roots come from the repeatable `--add-dir <path>` flag and from
`[permissions] additional_directories` (see [Configuration](configuration.md#permissions)).

- A path is inside the workspace when it lies inside any root. A symlink from one root into
  another stays inside; a symlink out of every root does not.
- Each root has its own confined instance of every file tool and of `shell`, and a call is served
  by the instance of the root that holds its path. For `shell` that is the root that holds its
  `cwd`. The tool's own confinement and Fontaine's check use the same rule, so they cannot
  disagree.
- A relative `--add-dir` or configuration entry is resolved against the project directory (the launch directory, or `--cd`). An entry
  that does not exist, is not a directory or cannot be resolved is left out, with a warning that
  names the path and where it came from (in the conversation in the terminal UI, on stderr in a
  headless run), and the run continues with the other roots.
  A bad entry never widens the workspace.
- Roots grant file access, not configuration. Settings, hooks, MCP servers and extension files are
  read from the primary root only; a `.fontaine/settings.toml` in an added root is ignored.
- Only the user adds roots. No tool can. The roots Fontaine adds itself are the session's
  scratch directory, below, and, while [auto memory](memory.md) is on, the project's memory store.
  The store is labelled in `/status` and the system prompt, stays out of the file index, and keeps
  the protected-path check on every `write`, `edit` and `notebook_edit`.

### The scratch directory

Each session gets a private scratch directory, appended to the workspace roots after the session's startup
roots as a non-primary root (`/add-dir` roots come after it), so `write` and `edit` can stage a helper script or a temporary file without a prompt in
`acceptEdits` and without a detour through `shell`. The system prompt names it, and the denial for
a path outside every root points at it.

| Platform | Location |
|---|---|
| Linux and other Unix with `$XDG_RUNTIME_DIR` | `$XDG_RUNTIME_DIR/fontaine/<session id>/` |
| macOS, or Unix without a runtime directory | `<temp dir>/fontaine-<uid>/<session id>/` (`$TMPDIR`, else `/tmp`) |
| Windows | `%TEMP%\fontaine-<sid>\<session id>\` |

- It is readable by the current user only (mode 0700; a current-user-only ACL on Windows), and
  the directory above it is checked before use: on Unix, a name someone else created, a symlink or
  a directory open to other users is refused (on Windows the per-user `%TEMP%` and the SID in the
  directory name stand in for that). The path is canonical, and it is never inside
  Fontaine's config or data directory, so those stay protected.
- It is an ordinary root. The deterministic floor still applies (a protected file name such as
  `.bashrc` still asks), `shell` treats it like any root (read-only commands whose operands are inside it take the
  pre-classifier fast path, other commands go through the guardian as usual), and it appears
  in the guardian's workspace roots, in hook payloads and in the headless `init` event. Subagents
  share their parent's scratch directory.
- It is removed when the session ends, including before a `/resume` or `/branch` starts the next
  one. A session that dies without cleaning up (a crash, `kill -9`) is swept by the next Fontaine
  start: each session holds a lock beside its directory, and a directory whose lock is free is
  removed. A resumed session reuses its path.
- If it cannot be created, a warning says why and the session runs with the roots it had before.

On Windows the path forms Fontaine accepts include drive letters, UNC paths and MSYS-style
`/c/...` paths, which are translated to drive paths first; a path rooted at `/` that is not a
drive is outside every root. Paths are compared without regard to case, and junctions and symlinks
are resolved.

## Shell

`shell` runs `sh -c` in the root that holds its `cwd` (the primary root when none is given).

- **Confinement.** The `cwd` is resolved like any other path, and one outside every root is
  refused. What the command itself does is not confined: it can `cd` anywhere, which is why
  `shell` is an arbitrary tool.
- **Timeout.** A call may ask for a `timeout_secs`; a longer request is cut to 600 seconds.
  Without one the default is 120 seconds, and a call that prints nothing for 60 seconds is stopped.
  A command that prints more than 10 MB is killed.
- **Output.** Past 2,000 lines or 50 KB the result keeps the first and last lines and the lines
  with errors or failures from between.
- **Environment.** The child process does not inherit the telemetry credentials (every
  `MICROMEGAS_*` variable, `OTEL_EXPORTER_OTLP_HEADERS` and `[telemetry] api_key_env`), the API key
  variable of any configured account (`[accounts.*] api_key_env`) or the guardian scanner's token
  variable (`[guardian] scanner_hf_token_env`). What it does get is Fontaine's environment minus
  those variables, plus every entry of [`[env]`](configuration.md#env), which is applied after the
  scrub.
- **Windows.** The shell runs through Git Bash, so Git for Windows must be installed. Fontaine
  finds `git.exe` on `PATH`, then in the standard install locations, and puts Git's `usr\bin` and
  `bin` directories first on the shell's `PATH`, in front of the `PATH` from `[env]` when it sets one. `FONTAINE_GIT_BASH_PATH` names a `bash.exe` and
  overrides the search. When Git for Windows cannot be found, Fontaine exits with code 2 and a
  message naming Git for Windows and the variable. The command reaches Git Bash as written,
  including every `\\`; a syntax error names `eval` rather than `-c`.

## Web fetch

`webfetch` takes one argument, `url`, and makes one GET.

- HTML is converted to markdown; other text is returned as it is; a response that is not text is
  an error.
- It reads at most 10 MB and returns at most 100,000 characters, and says so when it cuts. The
  request times out after 30 seconds.
- It never follows a redirect to another host. The new URL is returned to the model, which may
  ask for it. Redirects within one host are followed.
- It identifies itself with the User-Agent `fontaine/<version> (+https://github.com/madesroches/fontaine)`.
- It asks for `text/markdown, text/html, */*`, so a site that can serve markdown sends it.
- It has no `prompt` argument and does no summarizing.
- It trusts the system CA store. When that cannot be loaded (a bare container with no
  `ca-certificates`), it falls back to the bundled Mozilla roots and records that in the debug log
  (`--debug-file`), not on screen. Set `SSL_CERT_FILE` or `SSL_CERT_DIR` to trust other roots
  instead of the system's, or `[network] ca_bundle` to trust extra ones as well.
- It honors [`[network]`](configuration.md#network): the proxy, extra CA roots and client
  certificate. Without a proxy key it still follows `HTTPS_PROXY` and `NO_PROXY`.

## Web search

Search reaches the model in one of two ways, in this order:

1. **The provider's own search**, when the account supports it. The first release has it for
   `anthropic` accounts on the Anthropic API (`api.anthropic.com`, the default). The search runs
   inside the model's response, and the model cites what it found. Other hosts of the kind (a
   gateway, Databricks) are unverified, so they use it only with `[websearch] native = "on"`, and
   `native = "off"` never uses it. It is offered only while no `deny` or `ask` rule covers
   `websearch` and no [`pre_tool_use` hook](hooks.md) could (one with no `matcher`, a `matcher`
   naming `websearch`, or one whose matcher cannot be read), since a hook never sees a search the
   provider runs; in `manual`, `acceptEdits` and `dontAsk` it also needs an `allow` rule for
   `websearch`, since a call there would otherwise ask or be denied. Switching modes or adding a
   rule takes effect on the next turn. If the endpoint rejects the request because of the search
   tool, the failure message says to set `native = "off"`; nothing retries without it.
2. **The `websearch` tool**, which every other account kind uses, and so does an `anthropic`
   account when the provider's search is not offered. It searches through `[websearch] backend`;
   with none set, it searches through a SearXNG container that Fontaine starts itself (see
   [Default search](#default-search)). `backend = "off"` removes the tool.

The tool takes a `query` and, optionally, one of `allowed_domains` or `blocked_domains` (giving both
is an error). Fontaine applies the domain lists to the results itself, so they work the same for
every backend: a result is kept or dropped when its host is the domain or a subdomain of it.

- The result is a numbered list of title, URL and snippet. It holds at most `max_results` entries
  (default 10) and 100,000 characters, and says so when it cuts. A query with no results is a
  plain answer, not an error.
- Backends are **Tavily** (a POST to `api.tavily.com` with a bearer key) and **SearXNG** (a GET to
  your instance's `/search?format=json`; the instance must have the `json` format enabled, and a
  403 is reported with that hint). A 401, 403 or 429 is reported with its status.
- A redirect from the backend is an error and is never followed, because it would send the query to
  another host.
- The queries go to the provider or to the configured backend. They are checked like any other call
  by the [permission pipeline](permissions.md), and the result passes the result scanner.

### Default search

With no `[websearch] backend`, and `docker` (or else `podman`) found on Fontaine's own `PATH`, the
`websearch` tool is registered and backed by SearXNG in a container, with no configuration. The
first search:

- writes `searxng/settings.yml` in Fontaine's data directory (the `json` format enabled, the
  limiter off, a random secret generated once and kept in that file; Fontaine rewrites the file if
  it differs, keeping the secret);
- picks a free loopback port and runs
  `<runtime> run --rm --name fontaine-searxng-<random> -p 127.0.0.1:<port>:8080 -v <settings.yml>:/etc/searxng/settings.yml:ro docker.io/searxng/searxng:latest`
  (no shell), so the instance is reachable from this machine only;
- warns once that SearXNG is starting and that the first run pulls the image, then waits up to 300
  seconds for it to answer.

Later searches and registry rebuilds reuse the container. When the session ends, when a start
fails or times out, and before a restart over a dead instance, Fontaine sends the `run` client
SIGTERM as described below for `command` and also starts a detached `<runtime> rm -f <name>`
(its own process group, no output, never awaited), because a SIGKILL right behind the SIGTERM
can reach the client before it has passed the stop to the daemon, which would leave the container
running. The `rm` finishes after Fontaine has exited. A machine that is powered off, or a Fontaine
killed with SIGKILL, can still leave a container behind; its name starts with `fontaine-searxng-`,
so `<runtime> ps --filter name=fontaine-searxng-` lists strays. The settings directory is owner-only; the file inside is
readable by other users because the container's unprivileged user must read it through the bind
mount, which keeps the host's permissions. On an SELinux host the mount may need a `:Z` label,
which Fontaine does not add: start SearXNG yourself with `command` instead. The container runtime
gets the minimal environment plus `DOCKER_HOST`, `DOCKER_CONFIG`, `XDG_RUNTIME_DIR` and
`CONTAINER_HOST` when they are set, so it can reach its daemon. The command is fixed by Fontaine; it
is not configurable and never comes from a project file.

Without a container runtime there is no default: Fontaine warns once per process, at startup, that web search
is unavailable, unless the provider's own search applies or `backend` is set (to `"off"` as well).
`backend = "off"` disables the tool and the warning.

### Your own SearXNG

SearXNG is also the free, no-account path with your own instance: run one on localhost and set
`base_url`, or let Fontaine start it with `command`:

```toml
[websearch]
backend = "searxng"
base_url = "http://127.0.0.1:8080"
command = ["docker", "run", "--rm", "-p", "127.0.0.1:8080:8080", "searxng/searxng"]
```

With `command`, the first search starts the process (an argv, no shell, a cleared environment
that keeps only what a program needs to find itself) and waits up to `startup_timeout_secs` for
`base_url` to answer. Later searches, and the rebuilds that `/add-dir` and `/model` cause, reuse
it, and it is stopped when the session ends: Fontaine puts it in its own process group and sends
SIGTERM to the group, and a background task sends SIGKILL if it has not exited after 10 seconds
(on Windows it is killed). A `docker run` forwards SIGTERM to its container, so `--rm` removes it.
Stopping never blocks the session, so a command that ignores SIGTERM can outlive a session that
exits within those 10 seconds. A start that fails or times out is a search error with the
process's last stderr lines; there is no fallback to another backend. A session killed by a signal
can leave the process running (and, for `docker run`, the container). The image's default settings do not enable the `json` format; give
the instance a `settings.yml` with `json` under `search.formats`.

Provider-native search is outside the [guardian](guardian.md) and the result scanner.

## Permissions

Every tool call goes through the permission pipeline: the deterministic floor, the read-only fast
path, the shell pre-classifier, allow rules and the session mode. See
[Permissions](permissions.md).

## Results and telemetry

A tool's result passes through the result pipeline before the model sees it. The pipeline has no
stage yet and returns the result unchanged; later stages attach to it instead of adding their own
hooks.

Each call is recorded as a `fontaine.tool` span and a `tool_result` event, which carry no
arguments and no output. See [Telemetry](telemetry.md#events).
