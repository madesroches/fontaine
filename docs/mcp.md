# MCP servers

Fontaine can use tools from [Model Context Protocol](https://modelcontextprotocol.io) servers. A
server runs as a child process (stdio) or is reached over streamable HTTP, and each tool it lists
becomes a tool the model can call. MCP tools go through the same permission pipeline as every other
tool; nothing a server says about itself changes how they are judged.

In the terminal UI, `/mcp` lists the servers and whether each is connected, failed, left off or not
loaded.

## Declaring servers

Servers are declared under `[mcp.servers.<name>]`. Set exactly one of `command` (stdio) or `url`
(streamable HTTP).

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
| `url` | The endpoint of a streamable HTTP server. It is reached through the proxy, extra CA roots and client certificate of [`[network]`](configuration.md#network). |

A server name contains letters, digits, `_` and `-`, and has no `__` and no leading or trailing `_`.

**Environment.** A stdio server does not inherit Fontaine's environment. It gets a short base set
(`PATH`, `HOME`, `USER`, `LANG`, the temporary-directory variables and, on Windows, the system
variables it needs to find programs) and the variables named in `env_vars`. Values never appear in
configuration, and the API keys of your model accounts and the telemetry credentials are not
passed unless you name them. Every entry of [`[env]`](configuration.md#env) is set on the server as
well, replacing a copied variable of the same name. The server's standard error is not shown in the terminal UI; it goes to
the debug file (`--debug-file`).

**Where they are declared.** The user file, `--settings` and an untracked `settings.local.toml` of
a trusted root declare servers that start at once. A server declared in a repository's shared
`.fontaine/settings.toml` is [asked about](#servers-a-repository-defines) first. Extra workspace
roots (`--add-dir`) never contribute servers.

## Tool names

A tool `create_issue` of the server `github` is registered as `mcp__github__create_issue`, the
name [permission rules](permissions.md#rules) use. A character in a tool name that the
rule grammar cannot name becomes `_`. When a server announces that its tool list changed, Fontaine
fetches the list again and registers the new tools the same way; tools that went away are removed.
Only tools are used; resources and prompts are not.

A server that cannot be started, or that never answers the handshake, is reported (a notice in the
terminal UI, a line on stderr in a headless run) and left out; the session goes on without it.

## The trust model

- **Every MCP tool is `arbitrary`.** It is registered behind the same gate as the built-in tools
  and, like `shell` and `webfetch`, never earns the read-only fast path. In `manual` it asks, in
  `auto` the [guardian](guardian.md) judges it, in `dontAsk` and headless runs it is denied unless
  an allow rule covers it.
- **What a server declares is only text.** A tool can claim to be read-only, idempotent or
  non-destructive. Those hints are passed to the guardian as untrusted text inside the `tool_call`
  block of its request, labelled as the server's own claim, and never as a fact or a gate:

  ```
  server_declared_annotations (the server's own claim, not a fact): {"title":"Lookup","readOnlyHint":true}
  ```

  A `readOnlyHint` never skips the guardian.
- **Rules are the way to pre-approve.** `mcp__github`, `mcp__github__create_issue` and
  `mcp__github__*` in `allow`, `ask` or `deny` work like any other rule. Allow rules come from
  user-trust scopes only, so a repository can never allow its own server's tools.
- **Credentials stay out.** Nothing is sent to a streamable HTTP server but the protocol's own
  requests; Fontaine adds no credentials of its own.

## Servers a repository defines

A server in a repository's `.fontaine/settings.toml` is an executable the repository chose. In the
terminal UI:

1. Before anything it defines can start, an untrusted workspace root gets the [trust
   prompt](tui.md#workspace-trust). If you decline, the session runs without the repository's
   servers and hooks, and the question comes back next session.
2. In a trusted root each repository-defined server is then approved on its own, the first time it
   would start: the prompt shows the command line or URL and, for a command, the names of the environment variables it asks to receive (never their values). Your approval is recorded in
   `<data dir>/trust.toml` for that root, that name and that exact definition, so a server whose
   command, arguments, environment variable names or URL change is asked about again, and a repository cannot approve its own
   servers.

A headless run (`fontaine -p`) starts them without asking, as Claude Code's `-p` does. In both
surfaces a project server's tools are ordinary `arbitrary` tools: a repository can add a server,
but it cannot make a call of that server run without the same approval any call needs.

```toml trust.toml
[[trusted]]
root = '/home/me/code/myrepo'

[[approved_servers]]
root = '/home/me/code/myrepo'
name = "github"
digest = "9f2c1c8e1a7d4b1f"
```

## Telemetry

Starting a server emits an `mcp_server_connection` event and each call a `fontaine.mcp.call` span.
Server and tool names appear in them, and in every other tool field, only when
`log_tool_details = true`; otherwise they read `third-party`. See [Telemetry](telemetry.md).
