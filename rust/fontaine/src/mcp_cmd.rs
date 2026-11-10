//! `fontaine mcp`: declaring, listing, showing and removing MCP servers. The commands run outside
//! any session; the user typed them, so they need no permission pipeline. Nothing here starts a
//! server the interactive UI would ask about first.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use fontaine_core::config::{self, Config, Slot};
use fontaine_core::dirs::Dirs;
use fontaine_core::doctor_report::{McpGate, McpState};
use fontaine_core::mcp_auth::{FileCredentialStore, FileStateStore};
use fontaine_core::mcp_config::{self, Scope};
use fontaine_core::mcp_json;
use fontaine_core::plugins::PluginContributions;
use fontaine_core::trust;
use fontaine_tools::error_chain::chain;
use rmcp::transport::auth::{AuthorizationManager, AuthorizationRequest, OAuthState};

use crate::cli::{
    Cli, McpAction, McpAddArgs, McpArgs, McpGetArgs, McpListArgs, McpRemoveArgs, McpScopeArg,
    McpTransportArg, Toggle,
};
use crate::mcp_probe::{ProbedServer, apply_installed_plugins, probe_servers};
use crate::telemetry::{self, DebugSink};
use crate::{EXIT_CONFIG, Host, load_params, resolve_dirs};

/// Why a command did not succeed; the variant picks the exit code.
enum Failure {
    /// A usage or configuration error: exit 2.
    Usage(String),
    /// An unknown name, a duplicate, an ambiguous remove or an I/O error: exit 1.
    Failed(String),
}

impl Failure {
    fn failed(message: impl Into<String>) -> Failure {
        Failure::Failed(message.into())
    }

    fn usage(message: impl Into<String>) -> Failure {
        Failure::Usage(message.into())
    }

    fn io(context: &str, e: &std::io::Error) -> Failure {
        Failure::Failed(format!("{context}: {}", chain(e)))
    }
}

impl From<McpScopeArg> for Scope {
    fn from(arg: McpScopeArg) -> Scope {
        match arg {
            McpScopeArg::Local => Scope::Local,
            McpScopeArg::Project => Scope::Project,
            McpScopeArg::User => Scope::User,
        }
    }
}

pub fn run(cli: &Cli, args: &McpArgs, host: &Host, sink: Option<&DebugSink>) -> ExitCode {
    let result = match resolve_dirs(host) {
        Ok(dirs) => dispatch(cli, args, host, sink, &dirs),
        Err(e) => Err(Failure::usage(e.to_string())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("fontaine: {message}");
            ExitCode::from(EXIT_CONFIG)
        }
        Err(Failure::Failed(message)) => {
            eprintln!("fontaine: {message}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(
    cli: &Cli,
    args: &McpArgs,
    host: &Host,
    sink: Option<&DebugSink>,
    dirs: &Dirs,
) -> Result<(), Failure> {
    let root = std::fs::canonicalize(&host.cwd).unwrap_or_else(|_| host.cwd.clone());
    match &args.action {
        McpAction::Add(add_args) => add(add_args, dirs, &root),
        McpAction::Remove(remove_args) => remove(cli, remove_args, host, sink, dirs, &root),
        McpAction::List(list_args) => with_runtime(list(cli, list_args, host, sink, dirs)),
        McpAction::Get(get_args) => with_runtime(get(cli, get_args, host, sink, dirs, &root)),
        McpAction::Login(login_args) => with_runtime(login(cli, login_args, host, sink, dirs)),
        McpAction::Logout(logout_args) => logout(dirs, logout_args),
    }
}

fn with_runtime(
    future: impl std::future::Future<Output = Result<(), Failure>>,
) -> Result<(), Failure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::io("cannot start the async runtime", &e))?;
    runtime.block_on(future)
}

fn load_config(
    cli: &Cli,
    host: &Host,
    sink: Option<&DebugSink>,
    dirs: &Dirs,
) -> Result<Config, Failure> {
    let config = telemetry::with_debug_only(sink, || config::load(&load_params(cli, host, dirs)))
        .map_err(|e| Failure::usage(e.to_string()))?;
    for warning in &config.warnings {
        eprintln!("fontaine: warning: {warning}");
    }
    Ok(config)
}

fn scope_label(scope: Scope) -> String {
    format!("{} config", scope.label())
}

// ---------------------------------------------------------------------------------------------

fn add(args: &McpAddArgs, dirs: &Dirs, root: &Path) -> Result<(), Failure> {
    if !mcp_json::valid_server_name(&args.name) {
        return Err(Failure::usage(format!(
            "`{}` is not an MCP server name: {}",
            args.name,
            mcp_json::NAME_RULE
        )));
    }
    let scope: Scope = args.scope.into();
    let server = match args.transport {
        McpTransportArg::Stdio => {
            if !args.header.is_empty() {
                return Err(Failure::usage(
                    "`-H`/`--header` applies to http servers only",
                ));
            }
            mcp_config::stdio(
                &args.command_or_url,
                &args.args,
                args.env.iter().cloned().collect::<BTreeMap<_, _>>(),
                args.env_var.clone(),
            )
        }
        McpTransportArg::Http => {
            if !args.env.is_empty() || !args.env_var.is_empty() {
                return Err(Failure::usage(
                    "`-e` and `--env-var` apply to stdio servers only",
                ));
            }
            if !args.args.is_empty() {
                return Err(Failure::usage(
                    "an http server takes a URL and no arguments",
                ));
            }
            mcp_config::http_with_headers(
                &args.command_or_url,
                args.header.iter().cloned().collect(),
            )
        }
    };
    mcp_config::validate(&args.name, &server).map_err(Failure::usage)?;

    let (found, warnings) = mcp_config::find(&args.name, &dirs.config, root);
    for warning in warnings {
        eprintln!("fontaine: warning: {warning}");
    }
    if found.iter().any(|(found_scope, _)| *found_scope == scope) {
        return Err(Failure::failed(format!(
            "MCP server `{}` already exists in {}",
            args.name,
            scope_label(scope)
        )));
    }
    let path = mcp_config::add_server(scope, &dirs.config, root, &args.name, &server)
        .map_err(|e| Failure::io("cannot write the MCP server", &e))?;
    let kind = match args.transport {
        McpTransportArg::Stdio => "stdio",
        McpTransportArg::Http => "http",
    };
    println!(
        "Added {kind} MCP server `{}` ({}) to {}",
        args.name,
        server.command_line(),
        scope_label(scope)
    );
    println!("File modified: {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------------------------

fn remove(
    cli: &Cli,
    args: &McpRemoveArgs,
    host: &Host,
    sink: Option<&DebugSink>,
    dirs: &Dirs,
    root: &Path,
) -> Result<(), Failure> {
    let name = &args.name;
    let (found, warnings) = mcp_config::find(name, &dirs.config, root);
    for warning in warnings {
        eprintln!("fontaine: warning: {warning}");
    }
    let scope = match args.scope {
        Some(arg) => {
            let scope: Scope = arg.into();
            if !found.iter().any(|(s, _)| *s == scope) {
                return Err(Failure::failed(format!(
                    "no MCP server named `{name}` in {}",
                    scope_label(scope)
                )));
            }
            scope
        }
        None => {
            let scopes = mcp_config::scopes(&found);
            match scopes.as_slice() {
                [] => return Err(Failure::failed(not_removable(cli, host, sink, dirs, name))),
                [one] => *one,
                several => {
                    let mut message = format!(
                        "MCP server `{name}` exists in several scopes; say which to remove:"
                    );
                    for scope in several {
                        message.push_str(&format!(
                            "\n  fontaine mcp remove {name} -s {}",
                            scope.label()
                        ));
                    }
                    return Err(Failure::failed(message));
                }
            }
        }
    };
    let removal = mcp_config::remove_server(scope, &dirs.config, root, name);
    if removal.rewritten.is_empty() && removal.error.is_none() {
        return Err(Failure::failed(format!(
            "no MCP server named `{name}` in {}",
            scope_label(scope)
        )));
    }
    if !removal.rewritten.is_empty() {
        println!("Removed MCP server `{name}` from {}", scope_label(scope));
        for file in &removal.rewritten {
            println!("File modified: {}", file.display());
        }
    }
    match removal.error {
        Some(e) => Err(Failure::io("cannot remove the MCP server", &e)),
        None => Ok(()),
    }
}

/// What to say when `name` is in no scope `remove` can edit: it may be one it cannot.
fn not_removable(
    cli: &Cli,
    host: &Host,
    sink: Option<&DebugSink>,
    dirs: &Dirs,
    name: &str,
) -> String {
    let unknown = format!("no MCP server named `{name}`");
    let loaded = telemetry::with_debug_only(sink, || config::load(&load_params(cli, host, dirs)));
    let Ok(config) = loaded else {
        return unknown;
    };
    let (config, contributions, _) = apply_installed_plugins(config, dirs, host);
    match config.mcp_origins.get(name).map(|origin| origin.slot) {
        Some(Slot::Plugin) => match plugin_of(name, &contributions) {
            Some(plugin) => format!(
                "MCP server `{name}` comes from the plugin `{plugin}`; run `fontaine plugin uninstall {plugin}` to remove it"
            ),
            None => format!("MCP server `{name}` comes from a plugin; uninstall the plugin"),
        },
        Some(slot @ (Slot::ProjectLocal | Slot::Cli)) => format!(
            "MCP server `{name}` is declared in {}; edit that file to remove it",
            scope_file(&config, slot)
        ),
        _ => unknown,
    }
}

fn plugin_of<'a>(name: &str, contributions: &'a PluginContributions) -> Option<&'a str> {
    contributions
        .dirs
        .iter()
        .find(|dir| name.starts_with(&format!("plugin_{}_", dir.name)))
        .map(|dir| dir.name.as_str())
}

fn scope_file(config: &Config, slot: Slot) -> String {
    config
        .scopes
        .iter()
        .find(|entry| entry.slot == slot)
        .and_then(|entry| entry.path.as_ref())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the settings file".to_string())
}

// ---------------------------------------------------------------------------------------------

fn status(probed: &ProbedServer) -> String {
    match (probed.gate, &probed.state) {
        (McpGate::Untrusted, _) => "not started: workspace not trusted".to_string(),
        (McpGate::Unapproved, _) => "pending approval (run fontaine to approve)".to_string(),
        (McpGate::Start, McpState::Connected(tools)) => format!("connected ({tools} tools)"),
        (McpGate::Start, McpState::Failed(error)) => format!("failed: {error}"),
        (McpGate::Start, McpState::NotStarted(_)) => "not checked".to_string(),
    }
}

async fn list(
    cli: &Cli,
    args: &McpListArgs,
    host: &Host,
    sink: Option<&DebugSink>,
    dirs: &Dirs,
) -> Result<(), Failure> {
    let config = load_config(cli, host, sink, dirs)?;
    let (config, _, plugin_error) = apply_installed_plugins(config, dirs, host);
    if let Some(error) = plugin_error {
        eprintln!("fontaine: warning: plugins are not applied: {error}");
    }
    if config.mcp_servers.is_empty() {
        println!("No MCP servers configured. Use fontaine mcp add to add one.");
        return Ok(());
    }
    let trusted = trust::is_trusted(&dirs.data, &config.workspace);
    let probed = probe_servers(
        &config,
        dirs,
        host,
        trusted,
        args.connect != Toggle::No,
        None,
    )
    .await;
    for server in probed {
        println!(
            "{}: {} - {}",
            server.name,
            config.mcp_servers[&server.name].command_line(),
            status(&server)
        );
    }
    Ok(())
}

async fn get(
    cli: &Cli,
    args: &McpGetArgs,
    host: &Host,
    sink: Option<&DebugSink>,
    dirs: &Dirs,
    root: &Path,
) -> Result<(), Failure> {
    let name = &args.name;
    let config = load_config(cli, host, sink, dirs)?;
    let (config, contributions, plugin_error) = apply_installed_plugins(config, dirs, host);
    if let Some(error) = plugin_error {
        eprintln!("fontaine: warning: plugins are not applied: {error}");
    }
    let Some(server) = config.mcp_servers.get(name) else {
        return Err(Failure::failed(format!("no MCP server named `{name}`")));
    };
    let origin = config.mcp_origins.get(name).copied().unwrap_or_default();
    let trusted = trust::is_trusted(&dirs.data, &config.workspace);
    let probed = probe_servers(
        &config,
        dirs,
        host,
        trusted,
        args.connect != Toggle::No,
        Some(name),
    )
    .await;

    let writable = match origin.slot {
        Slot::Local => Some(Scope::Local),
        Slot::Project => Some(Scope::Project),
        Slot::User => Some(Scope::User),
        _ => None,
    };
    let (scope_text, hint) = match writable {
        Some(scope) => {
            // The load above already warned about any file that cannot be read.
            let (found, _) = mcp_config::find(name, &dirs.config, root);
            // `.fontaine/settings.toml` is listed after `.mcp.json` and wins a collision.
            let file = found
                .iter()
                .rev()
                .find(|(s, _)| *s == scope)
                .map(|(_, path)| path.clone())
                .unwrap_or_else(|| default_file(scope, dirs, root));
            (
                format!("{} ({})", scope.label(), file.display()),
                format!(
                    "To remove this server, run: fontaine mcp remove {name} -s {}",
                    scope.label()
                ),
            )
        }
        None if origin.slot == Slot::Plugin => {
            let plugin = plugin_of(name, &contributions);
            (
                match plugin {
                    Some(plugin) => format!("plugin ({plugin})"),
                    None => "plugin".to_string(),
                },
                match plugin {
                    Some(plugin) => {
                        format!("To remove this server, run: fontaine plugin uninstall {plugin}")
                    }
                    None => "To remove this server, uninstall its plugin".to_string(),
                },
            )
        }
        None => {
            let file = scope_file(&config, origin.slot);
            (
                format!("{} ({file})", origin.slot.label()),
                format!("To remove this server, edit {file}"),
            )
        }
    };

    println!("{name}:");
    println!("  Scope: {scope_text}");
    if let Some(probed) = probed.first() {
        println!("  Status: {}", status(probed));
    }
    println!(
        "  Type: {}",
        if server.command.is_some() {
            "stdio"
        } else {
            "http"
        }
    );
    if let Some(command) = &server.command {
        println!("  Command: {command}");
        if let Some(args) = server.args.as_ref().filter(|a| !a.is_empty()) {
            println!("  Args: {}", args.join(" "));
        }
    }
    if let Some(names) = server.env_vars.as_ref().filter(|n| !n.is_empty()) {
        println!(
            "  Environment variables passed through: {}",
            names.join(", ")
        );
    }
    if let Some(env) = server.env.as_ref().filter(|e| !e.is_empty()) {
        println!("  Environment:");
        for key in env.keys() {
            println!("    {key}=***");
        }
    }
    if let Some(url) = &server.url {
        println!("  URL: {url}");
    }
    println!();
    println!("{hint}");
    Ok(())
}

fn default_file(scope: Scope, dirs: &Dirs, root: &Path) -> PathBuf {
    match scope {
        Scope::Local => mcp_config::local_path(&dirs.config),
        Scope::Project => mcp_config::json_path(root),
        Scope::User => mcp_config::user_path(&dirs.config),
    }
}

// ---------------------------------------------------------------------------------------------

/// The loopback redirect target of `fontaine mcp login`: a fixed unregistered port, matching
/// the issue's Claude Code parity requirement for a fixed port.
const CALLBACK_PORT: u16 = 3111;
/// How long the loopback listener waits for the browser's redirect before giving up.
const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

async fn login(
    cli: &Cli,
    args: &crate::cli::McpLoginArgs,
    host: &Host,
    sink: Option<&DebugSink>,
    dirs: &Dirs,
) -> Result<(), Failure> {
    let name = &args.name;
    let config = load_config(cli, host, sink, dirs)?;
    let (config, _, plugin_error) = apply_installed_plugins(config, dirs, host);
    if let Some(error) = plugin_error {
        eprintln!("fontaine: warning: plugins are not applied: {error}");
    }
    // Resolved as at connect time (the `headers_helper` refusal and `${VAR}` expansion): the
    // stored token is bound to this URL, and a later connect refuses any other.
    let scrubbed = fontaine_core::tools::scrubbed_env(&config, (host.env_names)());
    let server = fontaine_core::agent::resolved_server(
        &config,
        name,
        &config.workspace,
        &host.env,
        &scrubbed,
    )
    .map_err(Failure::usage)?
    .ok_or_else(|| Failure::failed(format!("no MCP server named `{name}`")))?;
    let Some(url) = server.url else {
        return Err(Failure::usage(format!(
            "MCP server `{name}` is a stdio server; `login` applies to http servers"
        )));
    };
    let network = config
        .loaded_network(None)
        .map_err(|e| Failure::usage(chain(&e)))?;
    let oauth = fontaine_tools::mcp::auth::build_oauth_client(&network, &url)
        .map_err(|e| Failure::failed(chain(&e)))?;
    let mut manager =
        AuthorizationManager::new_with_oauth_http_client(url.as_str(), Arc::new(oauth))
            .await
            .map_err(|e| Failure::failed(chain(&e)))?;
    manager.set_credential_store(FileCredentialStore::new(&dirs.data, name, &url));
    manager.set_state_store(FileStateStore::new(&dirs.data, name));

    let redirect_uri = format!("http://127.0.0.1:{CALLBACK_PORT}/callback");
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, CALLBACK_PORT))
        .await
        .map_err(|e| {
            Failure::io(
                &format!("cannot listen for the OAuth callback on 127.0.0.1:{CALLBACK_PORT}"),
                &e,
            )
        })?;

    let mut state = OAuthState::Unauthorized(manager);
    let request = AuthorizationRequest::new(redirect_uri.clone()).with_client_name("Fontaine");
    state.start_authorization(request).await.map_err(|e| {
        Failure::failed(format!(
            "could not start the OAuth flow with `{url}`: {}",
            chain(&e)
        ))
    })?;
    let OAuthState::Session(session) = &state else {
        return Err(Failure::failed(
            "the OAuth flow did not reach the authorization stage".to_string(),
        ));
    };
    println!("Sign in to `{url}` in your browser to authorize Fontaine:");
    println!("{}", session.get_authorization_url());
    println!(
        "Fontaine does not open a browser: open the URL above yourself; the flow gives up after {} minutes.",
        LOGIN_TIMEOUT.as_secs() / 60
    );

    let callback =
        tokio::time::timeout(LOGIN_TIMEOUT, wait_for_callback(listener))
            .await
            .map_err(|_| {
                Failure::failed(format!(
                    "no OAuth callback arrived within {} minutes; run `fontaine mcp login {name}` to try again",
                    LOGIN_TIMEOUT.as_secs() / 60
                ))
            })??;
    let callback_url = format!("{redirect_uri}{callback}");
    session
        .handle_callback_url(&callback_url)
        .await
        .map_err(|e| Failure::failed(format!("could not complete the sign-in: {}", chain(&e))))?;
    println!("Signed in; the token is stored for `{name}`.");
    println!("Run `fontaine mcp logout {name}` to delete it.");
    Ok(())
}

/// Accepts one connection and reads the request target of its first request: the path and query
/// of the redirect. Answers the browser with a page it can close.
async fn wait_for_callback(listener: tokio::net::TcpListener) -> Result<String, Failure> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    let (mut socket, _) = listener
        .accept()
        .await
        .map_err(|e| Failure::io("cannot accept the OAuth callback", &e))?;
    let mut buf = vec![0u8; 8192];
    let read = socket
        .read(&mut buf)
        .await
        .map_err(|e| Failure::io("cannot read the OAuth callback", &e))?;
    let text = String::from_utf8_lossy(&buf[..read]);
    let Some(target) = text.lines().next().and_then(|line| line.split(' ').nth(1)) else {
        return Err(Failure::failed(
            "the OAuth callback was not an HTTP request".to_string(),
        ));
    };
    let target = target.to_string();
    let body = b"Sign-in received; you can close this tab and return to Fontaine.";
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let mut writer = socket;
    let _ = writer.write_all(response.as_bytes()).await;
    let _ = writer.write_all(body).await;
    let _ = writer.shutdown().await;
    Ok(target)
}

fn logout(dirs: &Dirs, args: &crate::cli::McpLogoutArgs) -> Result<(), Failure> {
    match fontaine_core::credentials::mcp_remove(&dirs.data, &args.name) {
        Ok(true) => {
            println!("Deleted the stored credentials of `{}`.", args.name);
            Ok(())
        }
        Ok(false) => Err(Failure::failed(format!(
            "no stored credentials for `{}`",
            args.name
        ))),
        Err(e) => Err(Failure::failed(chain(&e))),
    }
}
