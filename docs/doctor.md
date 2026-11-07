# `fontaine doctor`

`fontaine doctor` answers "why isn't my setup doing what I wrote?". It reads the settings layers,
workspace trust, the models, the MCP servers, the instruction files, hooks and skills, and the
auto-memory store, and reports what loaded, what did not, and why. It starts no session, sends no
prompt, and changes nothing: it never repairs or deletes a file and writes none of your
configuration. The one exception is an expiring OAuth token, which resolving a model may refresh
and save, as any session would.

```
fontaine doctor [--format text|json] [--connect yes|no] [--all]
```

| Flag | Meaning |
|---|---|
| `--format text\|json` | The report as text (the default) or as one JSON object. |
| `--connect=yes\|no` | Whether to start the MCP servers a session would start and count their tools. The default is `yes`; write `--connect=no` to skip the probe. |
| `--all` | Also list the settings no layer set. By default only the keys a layer set are listed. |

A prompt that is exactly `doctor` runs the subcommand under `fontaine -p`; write
`fontaine -p -- doctor` to send "doctor" as a literal prompt. The global flags that shape a
session (`--settings`, `--model`, `--effort`, `--cd`, `--add-dir`, `--allowed-tools`,
`--disallowed-tools`, `--in-container`) apply, so the report describes the session those flags would start.

## Exit code

`0` when no check failed (warnings are allowed), `1` when any finding is a `fail`. A settings file
that does not load is reported inside the report, not as a bare exit-2 error: diagnosing it is the
command's purpose.

Every finding has a level:

- `ok`: nothing to do.
- `warn`: worth knowing, but a session still works as configured, such as an untrusted workspace,
  an MCP server that failed to start, or a model notice.
- `fail`: something stops a session from working as configured, such as a settings file that does
  not load, a guardian model that is unavailable, an unreadable `trust.toml`, a plugin or skill
  file that fails to load, or a memory index that points at a file that does not exist.

## Sections

**Install.** The version, the config and data directories, and the workspace root. When the
directories cannot be determined, that is the only section and it fails.

**Config layers.** One row per layer: managed (reserved, shown as not supported), user, project,
project-local and `--settings`. Each is absent, loaded, or failed. Doctor reads each file on its
own, so a bad layer does not hide the others; every failing layer shows its first rejected key
with the reason, or its parse error with the file and position. A project key the scope policy
forbids is named, as in `permissions.default_mode cannot be set from project scope`. A
project-local file that loads as project settings (because git tracks it, or the workspace is not
trusted) shows why. If the settings as a whole still fail to load, the loader's own error follows
as a `load` row; it can come from a check across layers, such as a bad `--allowed-tools` rule or
an inconsistent `[network]` section. The sections that need the settings are then reported as
`skipped: config did not load`.

**Effective settings.** The keys the [`/config`](tui.md#settings-panel) panel shows, with the value
a session would use and the layers that supplied it. Values come from the same renderers as
`/config`: account credentials and `[env]` values are never printed, only names, and command settings
(`statusline.command`, `guardian.scanner_command`) print as `<program> (+N args)`, the composition
script as `set` and its params as `<hidden>` (keys only).

**Trust.** Whether the workspace root is trusted (an untrusted one is a warning), whether
`trust.toml` can be read, and what the repository defines that would load: hooks, MCP servers and
skills. It states the two outcomes separately. A headless run loads all of it and starts every
server. The interactive UI loads it only for a trusted workspace and asks before starting an
unapproved repository server. See [workspace trust](configuration.md#workspace-trust).

**Guardian and models.** The resolved main model, the guardian model and how it was chosen
(`pinned`, `default` or `main model`), the guardian profile, and any startup notice. An
unavailable guardian model is a failure, because every call the guardian must judge would be
denied. A model that does not resolve (no main model, a missing credential, an unknown model, a
pinned guardian on an unknown account) is a failure that shows the full error chain. Doctor adds
no reachability probe of its own: an account with a static `models` list is not contacted, and a
listing account is asked for its models once, with a 10 second limit, as `fontaine models` does. Resolving a model refreshes an expiring OAuth token and saves it, as a
session would.

**MCP servers.** Each configured server with its origin, transport and approval state, and whether
it connected (`connected (N tools)`), failed to start (a warning, since a session warns and
continues), or was not started with the reason. Doctor follows the interactive rule without the
prompt. User and plugin servers are started. A server from the repository is skipped when the
workspace is not trusted, and a repository-defined one is also skipped until you have approved it
in the interactive UI, so doctor never runs repository code a session would not. Each probe lasts
at most 10 seconds and is closed before doctor exits. Doctor sends no tool call.

**Instructions, hooks, skills.** The instruction files loaded and where each came from, in load
order; the hook count per event and scope, with project hooks marked by what a headless and an
interactive run would do; the installed plugins' warnings; and the skill and agent-type counts. A
skill or agent file that fails to load is a failure naming the file.

**Memory.** Compares the project's [auto memory](memory.md) store with its index. The store checked
is the one for the current workspace, and an existing store is checked even with
`[memory] auto = false`. See [Integrity checks](memory.md#integrity-checks) for what is
reported.
