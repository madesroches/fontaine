# MCP servers

Fontaine can use tools from [Model Context Protocol](https://modelcontextprotocol.io) servers. A
server runs as a child process (stdio) or is reached over streamable HTTP, and each tool it lists
becomes a tool the model can call. MCP tools go through the same permission pipeline as every other
tool; nothing a server says about itself changes how they are judged.

In the terminal UI, `/mcp` lists the servers and whether each is connected (with how many tools it
registered), pending (being reconnected), failed, left off or not loaded, and `/mcp reconnect`
restarts them; see [Reconnecting](#reconnecting). `fontaine mcp` adds, lists, shows and removes
servers from the command line; see [The `fontaine mcp` command](#the-fontaine-mcp-command).

## Declaring servers

Servers are declared under `[mcp.servers.<name>]` in a settings file, or with
[`fontaine mcp add`](#the-fontaine-mcp-command). Set exactly one of `command` (stdio) or `url`
(streamable HTTP). [Scopes](#scopes) says which files may declare them.

```toml
[mcp.servers.github]
command = "github-mcp-server"
args = ["stdio"]
env_vars = ["GITHUB_TOKEN"]

[mcp.servers.docs]
url = "https://mcp.example.com/docs"
```

| Key | Meaning |
|---|---|
| `command` | The program of a stdio server. It starts in the primary workspace root. |
| `args` | Its arguments. Stdio only. |
| `env_vars` | The names of environment variables to pass through. Stdio only. |
| `env` | Literal variables set on the server, after the pass-through and [`[env]`](#declaring-servers) layers, replacing a variable of the same name. Stdio only. |
| `url` | The endpoint of a streamable HTTP server. It is reached through the proxy, extra CA roots and client certificate of [`[network]`](configuration.md#network). |
| `headers` | Static headers sent on the server's protocol requests, as a map. HTTP servers only; never sent to an OAuth authorization server. See [Authentication](#authentication). |
| `headers_helper` | Path to an executable run at connect time; its `KEY: VALUE` stdout lines become headers, overriding static `headers` of the same name. HTTP servers only, and never in the project scope; see [Authentication](#authentication). |

A server name contains letters, digits, `_` and `-`, and has no `__` and no leading or trailing `_`.

**Environment.** A stdio server does not inherit Fontaine's environment. It gets a short base set
(`PATH`, `HOME`, `USER`, `LANG`, the temporary-directory variables and, on Windows, the system
variables it needs to find programs) and the variables named in `env_vars`. Values never appear in
configuration, and the API keys of your model accounts and the telemetry credentials are not
passed unless you name them. Every entry of [`[env]`](configuration.md#env) is set on the server as
well, replacing a copied variable of the same name, and a server's own `env` map lands last,
replacing a variable the other layers set. `env` values are literal: they are not expanded and
cannot reach into the process environment. A [plugin's](plugins.md) `.mcp.json` `env` values, in
contrast, are expanded at load, and the variables a plugin references are named in the install
summary. The server's standard error is not shown in the terminal UI; it goes to
the debug file (`--debug-file`).

## Scopes

A server is declared in one of three scopes, as in Claude Code.

| Scope | Where | Trust |
|---|---|---|
| `local` | `mcp-local.toml` in the [config directory](configuration.md#where-files-live), under `[projects."<root>".servers.<name>]`; private to you, in one workspace | Yours: starts at once, no workspace trust or approval |
| `project` | `.mcp.json` in the repository root, and `[mcp.servers]` in `.fontaine/settings.toml`; shared through the repository | The repository's: [workspace trust, then your approval](#servers-a-repository-defines) |
| `user` | `[mcp.servers]` in the user file (`config.toml`); every workspace | Yours |

`<root>` is the canonical launch directory, the one the project files are read from. A local file
entry is read only for its own workspace. The file lives in your config directory, which the
[floor](permissions.md#the-pipeline) protects, so a repository cannot write it.

These sources load too, and `fontaine mcp list` and `get` show them under their own labels:
`project-local` (an untracked `.fontaine/settings.local.toml` of a trusted root, which starts at
once), `cli` (the `--settings` file) and `plugin` (a [plugin's](plugins.md) `.mcp.json`, which
starts without the workspace-trust gate: install approved it, in the terminal UI and headless runs
alike). Extra workspace roots (`--add-dir`) never contribute servers.

**Precedence by name: `--settings` > local > project-local > project > user.** This is Claude
Code's local > project > user, with Fontaine's project-local and `--settings` layers kept in their
places. Within project, `.fontaine/settings.toml` wins over `.mcp.json`, and the collision warns
once at startup. A plugin's server whose name collides with a configured server is an error that
names the plugin.

A plugin's servers start and stop live with the plugin: `/plugin install`, `/plugin enable` and
`/reload-plugins` start the servers of a plugin they load, and `/plugin disable`, `/plugin
uninstall`, `/plugin update` and a reload that changes a plugin stop its servers before its
checkout changes and start the new definitions after. A server of a plugin that did not change
keeps running. On unload the server's tools leave the toolset first, so no new call starts;
calls already running get up to ten seconds to finish before the connection closes, and one that
is still running then ends with a transport error. A plugin that fails to load on reload (such as
a server name that collides) leaves the current set running. See
[Plugins](plugins.md#in-the-terminal-ui).

A malformed `mcp-local.toml` or `.mcp.json`, or an entry in one that cannot be used, is a warning
(at startup, in `fontaine doctor` and in `fontaine mcp`) and is skipped, so `fontaine mcp` can still
run to repair it.

## Tool names

A tool `create_issue` of the server `github` is registered as `mcp__github__create_issue`, the
name [permission rules](permissions.md#rules) use. A character in a tool name that the
rule grammar cannot name becomes `_`. When a server announces that its tool list changed, Fontaine
fetches the list again and registers the new tools the same way; tools that went away are removed.
Only tools are used; resources and prompts are not. A server may also ask you for input while a
tool runs, as [described below](#requests-for-input).

A server that cannot be started, or that never answers the handshake, is reported (a notice in the
terminal UI, a line on stderr in a headless run) and left out; the session goes on without it.
Starting a remote server is retried first when the failure looks temporary: a refused or reset
connection, a timeout, a 5xx or a 429. Fontaine tries up to three more times, after 1, 2 and 4
seconds, and all the attempts of one server share a 30 second deadline, so a refused server delays
startup by about 7 seconds and an unresponsive one by at most 30. Authentication, not-found and
protocol errors, and a stdio server that fails, are not retried. The servers start at once, so a
slow one does not delay the others. A server that failed to start can still be restarted by hand
with [`/mcp reconnect`](#reconnecting).

## The trust model

- **Every MCP tool is `arbitrary`.** It is registered behind the same gate as the built-in tools
  and, like `shell` and `webfetch`, never earns the read-only fast path. In `manual` it asks, in
  `auto` the [guardian](guardian.md) judges it, in `dontAsk` and headless runs it is denied unless
  an allow rule covers it. The one exception is a tool its server marks as
  [requiring your approval](#requiring-approval-for-a-tool): it always needs a person's approval
  and no allow rule pre-approves it, so it is denied in `dontAsk` and headless runs.
- **What a server declares is only text, with one exception.** A tool can claim to be read-only,
  idempotent or non-destructive. Those hints are passed to the guardian as untrusted text inside the `tool_call`
  block of its request, labelled as the server's own claim, and never as a fact or a gate:

  ```
  server_declared_annotations (the server's own claim, not a fact): {"title":"Lookup","readOnlyHint":true}
  ```

  A `readOnlyHint` never skips the guardian. The exception is the `requiresUserInteraction`
  marker below, the only declared value that changes a decision, and only toward asking.
- **Rules are the way to pre-approve.** `mcp__github`, `mcp__github__create_issue` and
  `mcp__github__*` in `allow`, `ask` or `deny` work like any other rule. Allow rules come from
  user-trust scopes only, so a repository can never allow its own server's tools. No rule
  pre-approves a tool its server marked as requiring your approval.
- **Credentials stay out of stdio.** A stdio server does not inherit your model-account keys,
  the telemetry credentials or any other secret: it gets the base set and what you name. A
  streamable HTTP server in the project scope likewise never receives credential variables
  through `${VAR}` expansion. Authenticating *to* a remote server is a different thing and is
  [described below](#authentication): it stores an OAuth token bound to that one URL, never a
  credential of yours.

### Requiring approval for a tool

A server can mark a tool as needing a person's approval on every call, as Claude Code allows, by
setting `_meta["anthropic/requiresUserInteraction"]` to the JSON Boolean `true` in that tool's
`tools/list` entry:

```json
{
  "name": "grant_access",
  "description": "Grant a user access to a project",
  "inputSchema": {"type": "object"},
  "_meta": {"anthropic/requiresUserInteraction": true}
}
```

Fontaine treats the marker as an implicit `ask` rule on that one tool, at the floor of the
[permission pipeline](permissions.md#the-pipeline). The tool asks wherever a person can answer, `auto` included,
and the prompt offers no "allow for this session" or "always allow". An allow rule or a hook's
allow does not skip it, and no guardian verdict approves it. In `dontAsk` and in headless runs the
call is denied. The audit record carries `tier = ask_rule` and
`rule = anthropic/requiresUserInteraction`; it never names the server or the tool. Any other value
(`"true"`, `1`, `false`) is ignored. A `deny` rule still wins.

Once a server has marked a tool, the marker holds until that server's connection ends (the next
session), even if a later tool list drops it. A marker added by a later tool list takes effect
when Fontaine refreshes the list.

## Servers a repository defines

A server in a repository's `.fontaine/settings.toml` or in its `.mcp.json` is an executable the
repository chose. A repository written for Claude Code works as it is: Fontaine reads its root
`.mcp.json` (`mcpServers`) as project scope. In the terminal UI:

1. Before anything it defines can start, an untrusted workspace root gets the [trust
   prompt](tui.md#workspace-trust). If you decline, the session runs without the repository's
   servers and hooks, and the question comes back next session.
2. In a trusted root each repository-defined server is then approved on its own, the first time it
   would start: the prompt shows the command line or URL and, for a command, the names of the environment variables it asks to receive (never their values). Your approval is recorded in
   `<data dir>/trust.toml` for that root, that name and that exact definition, so a server whose
   command, arguments, environment variable names or URL change is asked about again, and a repository cannot approve its own
   servers.

A headless run (`fontaine -p`) starts them without asking, as Claude Code's `-p` does. In both
surfaces a project server's tools are ordinary `arbitrary` tools, except that a tool can mark itself
as [requiring your approval](#requiring-approval-for-a-tool): a repository can add a server, but it
cannot make a call of that server run without the same approval any call needs, and it can only
make that approval stricter.

### `.mcp.json`

Each entry takes `type` (`stdio` or `http`; absent means `stdio` for a `command` entry and `http`
for a `url` entry), `command`, `args`, `env`, `url`, `headers` and `headersHelper`. An entry
with `type` `sse` or `ws` is skipped with a warning, since those transports are not supported
yet. `headers` and `headersHelper` behave as in the [key table](#declaring-servers): http only,
and a helper is refused in the project scope.

`${VAR}` and `${VAR:-default}` in `command`, `args`, `env` values, `url` and `headers` values
are expanded **when the server starts**, from Fontaine's environment. `${CLAUDE_PROJECT_DIR}` is
the session's workspace root, never a value inherited from a parent process. Because the text stays as written, the
approval prompt shows `${GITHUB_TOKEN}` and never the value, lists the variables the definition
references next to its environment names, and the approval survives a rotated token but not a
changed reference.

- **A credential variable is never expanded.** A repository's definition may not reference a
  variable that holds a credential Fontaine knows of: the keys of your model accounts and the
  telemetry credentials, the scanner's Hugging Face token, `MICROMEGAS_*` and OTLP header
  variables (the set tool subprocesses are [scrubbed](#declaring-servers) of), the web-search key
  and the well-known provider variables such as `ANTHROPIC_API_KEY`. Such a reference fails that
  server's start with an error naming the variable. In a headless run a repository's server starts
  without asking, and `url = "https://host/${ANTHROPIC_API_KEY}"` would otherwise hand a model key
  to a host the repository chose. (Claude Code expands any variable.)
- **An unset variable with no default fails only that server.** The error names the variable, and
  the other servers start. (Claude Code fails the whole config parse.)
- Definitions in the TOML files are not expanded.

```toml trust.toml
[[trusted]]
root = '/home/me/code/myrepo'

[[approved_servers]]
root = '/home/me/code/myrepo'
name = "github"
digest = "9f2c1c8e1a7d4b1f"
```

## Authentication

A streamable HTTP server can require OAuth 2.1, as the MCP authorization spec describes.
Fontaine drives the flow with the protocol library's own auth stack; nothing about it changes
how the server's tools are judged.

**`fontaine mcp login <name>`** signs in to one configured server:

1. It discovers the server's protected-resource metadata and authorization-server metadata,
   registers itself with the server (dynamic client registration; Fontaine has no hosted client
   identity of its own), and prints the authorize URL to open in your browser.
2. The server redirects to Fontaine's loopback listener on the fixed port 3111, where the
   sign-in completes; the listener gives up after 5 minutes. PKCE protects the exchange.
3. The token is stored under the server's name in `<data dir>/credentials.toml`, in a `[mcp]`
   table, through the same owner-only writer and cross-process lock as your other credentials.
   The entry also records the URL the token was issued for: a declaration whose `url` differs
   from the stored one never receives the token, and fails with a message naming
   `fontaine mcp login <name>` — so a project reusing a user-scope server's name cannot ride on
   its login.

While a token is valid it is sent as `Authorization: Bearer` and refreshed as it expires; a
refresh and a concurrent `fontaine mcp login` coordinate through the credentials lock, so they
cannot clobber each other. **`fontaine mcp logout <name>`** deletes the entry, including the
registration; the next login simply registers again.

A server that answers 401 while no usable token is stored fails permanently with
`authentication required; run fontaine mcp login <name>` — reconnecting cannot fix that, and a
[headless run](headless.md) never starts the flow for you. The `/mcp` listing's auth column
shows `ok`, `required` or `n/a` per server, and `/mcp reconnect` after a CLI login picks the new
token up.

**Headers** are the other way a remote server knows you. `headers` declares static headers on
an HTTP server (`fontaine mcp add -H KEY=value`, or `headers = { ... }` in a settings file;
`headers` in `.mcp.json`). They are sent on the server's protocol requests only — never to an
authorization server's token, metadata or authorize endpoints, which are a different origin and
use their own client — and a repository's `${VAR}` references in header values expand at start
under the same credential rules as its URL. `headers_helper` (`headersHelper` in `.mcp.json`)
names an executable run at connect time whose `KEY: VALUE` stdout lines become headers,
overriding static headers of the same name; Fontaine runs it only for scopes you control and
refuses it in the project scope, where a repository Fontaine merely opened must not make
Fontaine execute a script. Changing `headers` or `headers_helper` on an approved server
invalidates the approval, exactly like changing its URL.

## The `fontaine mcp` command

`fontaine mcp` manages servers outside any session. You typed the command, so it needs no
permission; nothing it does starts a server the terminal UI would ask about first.

```
fontaine mcp add [-s local|project|user] [-t stdio|http] [-e KEY=value]... [--env-var NAME]... [-H KEY=value]... <name> <command-or-url> [args...]
fontaine mcp list [--connect=no]
fontaine mcp get <name> [--connect=no]
fontaine mcp remove <name> [-s local|project|user]
fontaine mcp login <name>
fontaine mcp logout <name>
```

- **`add`** writes the server to the scope's file (`local` by default; `project` writes
  `.mcp.json` in Claude Code's shape) and prints the confirmation and `File modified: <path>`. A
  stdio server needs `--` before its own flags: `fontaine mcp add github -- github-mcp-server
  stdio`. `-e` sets a literal variable on the server (`env`). `--env-var NAME` passes a variable
  of Fontaine's environment through by name (`env_vars`), since Fontaine otherwise [scrubs](#declaring-servers)
  it; in `.mcp.json` it is written as `"NAME": "${NAME}"`. `-e` and `--env-var` are for stdio
  only; `-H KEY=value` declares a header for http servers only, repeatable, and is stored in the
  declaration. Adding a project server records no approval: the next terminal session asks. A
  name already in that scope is an error. `-t sse` is not supported yet (it is refused with a
  message naming the issue), and there is no `add-json`.
- **`list`** prints one line per server, `<name>: <command or URL> - <status>`, in name order.
  The status is `connected (N tools)`, `failed: <reason>`, `pending approval (run fontaine to
  approve)` for a repository's server you have not approved, or `not started: workspace not
  trusted`. It starts exactly the servers [`fontaine doctor`](doctor.md) would and no others, so a
  repository's server is never started by the command. `--connect=no` skips the probes and the
  status reads `not checked`.
- **`get`** shows one server: its scope and file, status, type, command, arguments, the names of
  the variables passed through, the `env` keys (values are shown as `***`), and the URL. It ends
  with the command that removes it (for a plugin's server, `fontaine plugin uninstall`).
- **`remove`** removes the server from the one writable scope that declares it. If several do, it
  exits 1 and lists the `-s` command for each. `-s project` removes it from `.mcp.json` and from
  `.fontaine/settings.toml`. Plugin, `--settings` and `settings.local.toml` servers are not
  writable scopes; the error names the file to edit or the plugin to uninstall. An approval left
  in `trust.toml` is inert.
- **`login`** signs in to a streamable HTTP server with OAuth, as [described
  below](#authentication); **`logout`** deletes the stored token.

Exit status: 0 on success; 1 for an unknown name, a duplicate, an ambiguous `remove` or an I/O
error; 2 for a usage or configuration error.

## Requests for input

A server can ask you for input while a tool runs (MCP elicitation). Fontaine supports the form mode:
the terminal UI shows a dialog with the server's message and one row per field the server asked for
(text, number, whole number, yes/no, a choice, or several choices), checks what you type against
what the server required, and sends your answer back to that server. You can fill the form in,
decline it (the server carries on without the input) or cancel it (the server stops what it was
doing). The keys are in [the terminal UI](tui.md#requests-for-input-from-an-mcp-server). A server's
request to open a link (URL mode) is always declined: Fontaine never opens a browser on a server's
say-so.

- **Only the terminal UI shows a dialog.** A [headless run](headless.md#questions), [AFK mode](afk.md),
  the [doctor](doctor.md) probe and a call made by a subagent answer `cancel` without showing
  anything. The dialog closes, and is answered `cancel`, when the call that caused it ends.
- **Your answer goes to the server and nowhere else.** It never reaches the model, the transcript,
  the audit log or telemetry, and the dialog says so. Don't enter secrets you wouldn't give that
  server. The server's message is cleaned of control characters before it is shown.
- **It decides nothing.** Answering never changes a permission decision; the call that led to the
  request was already approved like any other.

Fontaine offers the MCP protocol version 2025-11-25, which delivers a request for input as a
request to the client. A server that only speaks a newer version still connects, but its requests
for input are not served.

## Reconnecting

A remote (streamable HTTP) server that drops mid-session is reconnected. Fontaine notices when a
tool call fails because the transport is dead, or when the connection ends, closes it and retries up
to five times, waiting 1, 2, 4, 8 and 16 seconds before the attempts. Each attempt has a 30 second
limit. While it retries, `/mcp` shows the server as `pending (reconnecting, attempt N of 5)` and its
tools are withdrawn; a success registers them again and says so in the terminal UI. An
authentication or protocol error stops the retries at once, since waiting cannot fix it. After the
last attempt the server is marked failed:

```
MCP server `docs` disconnected · run /mcp reconnect docs
```

A stdio server that exits is marked failed straight away and its process is not started again, in a
headless run as well; there is no automatic retry for it.

`/mcp reconnect <name>` restarts one server, connected or not, at once and without waiting;
`/mcp reconnect all` restarts every failed server. Only servers Fontaine tried to start this
session can be restarted, including ones that failed at startup. A server you left off, or that
the workspace not being trusted keeps from loading, is never started this way, so the command cannot
get around workspace trust or your approval. It is not available while a turn runs.

## Large outputs

A successful tool result that is too large is saved to a file instead of entering the model's
context. The limit is 50,000 characters or `MAX_MCP_OUTPUT_TOKENS` tokens (default 25,000, counted
as the result's bytes divided by 4), whichever is hit first. The result is written in full to
`<scratch>/mcp-results/<tool>-<n>.txt`, in the session's scratch directory, and the model gets this
instead:

```
Output too large (60000 chars, ~15000 tokens), saved to <path>. Use read or grep on that file to see it.
```

The model reads the file with `read` or `grep` like any file in the workspace, and the content is
screened then like any other `read`. The file is removed with the session. A tool can raise the
character limit for its own results by setting `_meta["anthropic/maxResultSizeChars"]` to a positive
integer in its `tools/list` entry, up to 500,000; the token limit still applies. Anything else is
ignored.

If the session has no scratch directory, or the file cannot be written, the result keeps its first
and last halves of the limit with a `[… N chars cut …]` marker between them; Fontaine warns once
when a save fails. A result a server flags as an error (`isError`) longer than 11,000 characters keeps its
first and last 5,000 characters with the same marker. A result that carries an image is passed
through unchanged. `MAX_MCP_OUTPUT_TOKENS` is described under
[environment variables](configuration.md#command-line-and-environment).

## Telemetry

Starting a server emits an `mcp_server_connection` event, and so does each reconnect attempt, and
each call a `fontaine.mcp.call` span. Answering a request for input emits an `mcp_elicitation`
event with the action (`accept`, `decline` or `cancel`) and the server, never the message or the
answer.
Server and tool names appear in them, and in every other tool field, only when
`log_tool_details = true`; otherwise they read `third-party`. See [Telemetry](telemetry.md).
