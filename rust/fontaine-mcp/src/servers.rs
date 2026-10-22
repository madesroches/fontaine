//! Starting MCP servers: a child process over stdio or a streamable-HTTP endpoint.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use fontaine_tools::guarded::Guard;
use rig_core::tool::ManagedToolSink;
use rig_rmcp::{McpClientHandler, McpConnection};
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};

use crate::register::GuardedSink;

const TARGET: &str = "fontaine::telemetry";

#[derive(Clone, Debug)]
pub enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
        /// Names of environment variables passed through. Nothing else of the parent's
        /// environment but a short base set reaches the server.
        env_vars: Vec<String>,
        cwd: PathBuf,
    },
    Http {
        url: String,
    },
}

#[derive(Clone, Debug)]
pub struct ServerSpec {
    pub name: String,
    pub transport: Transport,
}

impl ServerSpec {
    pub fn kind(&self) -> &'static str {
        match self.transport {
            Transport::Stdio { .. } => "stdio",
            Transport::Http { .. } => "http",
        }
    }
}

#[derive(Debug)]
pub struct McpError {
    pub server: String,
    pub message: String,
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP server `{}`: {}", self.server, self.message)
    }
}

impl std::error::Error for McpError {}

/// A live server. Dropping it closes the connection and retires its tools.
pub struct Connected {
    pub server: String,
    pub connection: McpConnection,
    pub kind: &'static str,
}

/// Variables every server gets so it can find programs and its home, and nothing that names a
/// credential.
const BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "TMP",
    "TEMP",
    "TZ",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROGRAMDATA",
    "WINDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
];

fn client_config() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("fontaine", env!("CARGO_PKG_VERSION")),
    )
}

fn fail(spec: &ServerSpec, message: impl std::fmt::Display) -> McpError {
    McpError {
        server: spec.name.clone(),
        message: message.to_string(),
    }
}

/// Starts the server `spec` describes and registers its tools in `sink`, wrapped by `guard`.
/// The tools of a later list-changed notification are wrapped the same way.
pub async fn connect<S>(
    spec: &ServerSpec,
    sink: S,
    guard: Guard,
    reveal_names: bool,
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
) -> Result<Connected, McpError>
where
    S: ManagedToolSink + Send + Sync + 'static,
{
    let result = start(spec, sink, guard, reveal_names, env).await;
    let shown = if reveal_names {
        spec.name.as_str()
    } else {
        "third-party"
    };
    match &result {
        Ok(_) => tracing::info!(
            target: TARGET,
            server = shown,
            transport = spec.kind(),
            status = "connected",
            "mcp_server_connection"
        ),
        Err(_) => tracing::info!(
            target: TARGET,
            server = shown,
            transport = spec.kind(),
            status = "failed",
            "mcp_server_connection"
        ),
    }
    result
}

/// Starts the server behind an already-built transport, such as an in-process pipe, with the
/// same wrapping as [`connect`]. The server's name is `name`.
pub async fn connect_transport<S, T, E, A>(
    name: &str,
    transport: T,
    sink: S,
    guard: Guard,
    reveal_names: bool,
) -> Result<McpConnection, McpError>
where
    S: ManagedToolSink + Send + Sync + 'static,
    T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let handler = McpClientHandler::new(
        client_config(),
        GuardedSink::new(sink, name, guard, reveal_names),
    );
    handler.connect(transport).await.map_err(|e| McpError {
        server: name.to_string(),
        message: e.to_string(),
    })
}

async fn start<S>(
    spec: &ServerSpec,
    sink: S,
    guard: Guard,
    reveal_names: bool,
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
) -> Result<Connected, McpError>
where
    S: ManagedToolSink + Send + Sync + 'static,
{
    let handler = McpClientHandler::new(
        client_config(),
        GuardedSink::new(sink, &spec.name, guard, reveal_names),
    );
    let connection = match &spec.transport {
        Transport::Stdio {
            command,
            args,
            env_vars,
            cwd,
        } => {
            let mut process = tokio::process::Command::new(command);
            process.args(args).current_dir(cwd).env_clear();
            for name in BASE_ENV
                .iter()
                .copied()
                .chain(env_vars.iter().map(String::as_str))
            {
                if let Some(value) = env(name) {
                    process.env(name, value);
                }
            }
            let (transport, stderr) = TokioChildProcess::builder(process)
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| fail(spec, format!("cannot start `{command}`: {e}")))?;
            if let Some(stderr) = stderr {
                let name = spec.name.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncBufReadExt, BufReader};
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tracing::debug!(server = %name, "mcp stderr: {line}");
                    }
                });
            }
            handler
                .connect(transport)
                .await
                .map_err(|e| fail(spec, e))?
        }
        Transport::Http { url } => {
            let transport = StreamableHttpClientTransport::from_uri(url.as_str());
            handler
                .connect(transport)
                .await
                .map_err(|e| fail(spec, e))?
        }
    };
    Ok(Connected {
        server: spec.name.clone(),
        connection,
        kind: spec.kind(),
    })
}
