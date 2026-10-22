# Tools

The model acts on the workspace through a fixed set of built-in tools. Every one of them is
registered behind a single gate: a call that is not allowed never reaches the tool, and every
result passes one post-call step before the model sees it.

## Built-in tools

| Tool | Risk class | What it does |
|---|---|---|
| `read` | read-only | Reads a text file (at most 16 MB), by line range. |
| `ls` | read-only | Lists the files under a directory, honouring `.gitignore`. Arguments: `path`, `depth`, `glob`, `hidden`. |
| `glob` | read-only | Finds files whose path matches a glob pattern such as `src/**/*.rs`. Arguments: `pattern`, `path`. Returns at most 1,000 files. |
| `grep` | read-only | Searches file contents with a Rust regular expression, honouring `.gitignore`. Arguments: `pattern`, `path`, `glob`, `hidden`, `files`. Returns at most 200 matches; prefix the pattern with `(?i)` to ignore case. |
| `write` | workspace write | Replaces a whole file. |
| `edit` | workspace write | Replaces text in a file. The file must have been read first. |
| `shell` | arbitrary | Runs a shell command. |
| `webfetch` | arbitrary | Fetches one web page. |

`read`, `write`, `edit` and `shell` are the file and process tools of the rig runtime, configured
rather than modified. `ls`, `glob` and `grep` are Fontaine's own front ends over rig's directory
listing and search. rig's own `list`, `search`, `delete` and `move` are not registered: the
model sees `ls`, `glob` and `grep` instead, and removing or renaming a file goes through `shell`,
which is an arbitrary tool and so is always gated as one.

The system prompt steers the model toward `ls`, `read`, `glob` and `grep` and away from `cat`,
`find` and `grep` inside a shell.

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
File-tool calls on such a path are denied with a hint to add the directory with `/add-dir` or
`--add-dir`; they are never offered for approval.

## Workspace roots

The workspace is an ordered set of roots. The first is the primary root, the directory Fontaine
was launched from after symlinks are resolved; relative paths and default directories refer to it.
More roots come from the repeatable `--add-dir <path>` flag and from
`[permissions] additional_directories` (see [Configuration](configuration.md#permissions)).

- A path is inside the workspace when it lies inside any root. A symlink from one root into
  another stays inside; a symlink out of every root does not.
- Each root has its own confined instance of every file tool and of `shell`, and a call is served
  by the instance of the root that holds its path. For `shell` that is the root that holds its
  `cwd`. The tool's own confinement and Fontaine's check use the same rule, so they cannot
  disagree.
- A relative `--add-dir` or configuration entry is resolved against the launch directory. An entry
  that does not exist, is not a directory or cannot be resolved is left out, with a warning on
  stderr that names the path and where it came from, and the run continues with the other roots.
  A bad entry never widens the workspace.
- Roots grant file access, not configuration. Settings, hooks, MCP servers and extension files are
  read from the primary root only; a `.fontaine/settings.toml` in an added root is ignored.
- Only the user adds roots. No tool can.

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
  variable (`[guardian] scanner_hf_token_env`).
- **Windows.** The shell runs through Git Bash, so Git for Windows must be installed. Fontaine
  finds `git.exe` on `PATH`, then in the standard install locations, and puts Git's `usr\bin` and
  `bin` directories first on the shell's `PATH`. `FONTAINE_GIT_BASH_PATH` names a `bash.exe` and
  overrides the search. When Git for Windows cannot be found, Fontaine exits with code 2 and a
  message naming Git for Windows and the variable.

## Web fetch

`webfetch` takes one argument, `url`, and makes one GET.

- HTML is converted to markdown; other text is returned as it is; a response that is not text is
  an error.
- It reads at most 10 MB and returns at most 100,000 characters, and says so when it cuts. The
  request times out after 30 seconds.
- It never follows a redirect to another host. The new URL is returned to the model, which may
  ask for it. Redirects within one host are followed.
- It has no `prompt` argument and does no summarizing.

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
