//! Starting MCP servers: a child process over stdio or a streamable-HTTP endpoint.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use fontaine_tools::error_chain::chain;
use fontaine_tools::guarded::Guard;
use fontaine_tools::tls::{LoadedNetwork, build_client};
use rig_core::tool::ManagedToolSink;
use rig_rmcp::{McpClientHandler, McpConnection};
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};

use crate::interaction::{InteractionTap, Marked};
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
        /// Variables set on the server after the base set and `env_vars`, overriding a copied
        /// value of the same name.
        env: Vec<(OsString, OsString)>,
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

/// The client rmcp builds for `from_uri` (no idle pooling, same-origin 307/308 redirects only),
/// with `network` applied; rmcp's own cannot take a proxy or extra roots.
fn http_client(network: &LoadedNetwork) -> reqwest::Result<reqwest::Client> {
    build_client(network, || {
        reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .redirect(same_origin_redirect_policy())
    })
}

/// Only same-origin 307 and 308 redirects are followed: those keep the method and body, and a
/// cross-origin hop would replay the session id and custom headers to another host.
fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let preserves_body = matches!(
            attempt.status(),
            reqwest::StatusCode::TEMPORARY_REDIRECT | reqwest::StatusCode::PERMANENT_REDIRECT
        );
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|original| original.origin() == attempt.url().origin());
        if !preserves_body || !same_origin {
            attempt.stop()
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error(format!("exceeded {MAX_REDIRECTS} redirects"))
        } else {
            attempt.follow()
        }
    })
}

const MAX_REDIRECTS: usize = 10;

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
    network: &LoadedNetwork,
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
) -> Result<Connected, McpError>
where
    S: ManagedToolSink + Send + Sync + 'static,
{
    let result = start(spec, sink, guard, reveal_names, network, env).await;
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
    let marked = Marked::default();
    let handler = McpClientHandler::new(
        client_config(),
        GuardedSink::new(sink, name, guard, reveal_names, marked.clone()),
    );
    let transport = InteractionTap::new(transport.into_transport(), marked);
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
    network: &LoadedNetwork,
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
) -> Result<Connected, McpError>
where
    S: ManagedToolSink + Send + Sync + 'static,
{
    let marked = Marked::default();
    let handler = McpClientHandler::new(
        client_config(),
        GuardedSink::new(sink, &spec.name, guard, reveal_names, marked.clone()),
    );
    let connection = match &spec.transport {
        Transport::Stdio {
            command,
            args,
            env_vars,
            env: set_env,
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
            for (name, value) in set_env {
                process.env(name, value);
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
                .connect(InteractionTap::new(transport, marked))
                .await
                .map_err(|e| fail(spec, e))?
        }
        Transport::Http { url } => {
            let client = http_client(network).map_err(|e| {
                fail(
                    spec,
                    format!(
                        "could not build the HTTP client: {}",
                        chain(&e.without_url())
                    ),
                )
            })?;
            let transport = StreamableHttpClientTransport::with_client(
                client,
                StreamableHttpClientTransportConfig::with_uri(url.as_str()),
            );
            handler
                .connect(InteractionTap::new(transport, marked))
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn status_of(client: &reqwest::Client, url: String) -> u16 {
        client.post(url).send().await.unwrap().status().as_u16()
    }

    #[tokio::test]
    async fn a_same_origin_307_is_followed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", "/b"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/b"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = http_client(&LoadedNetwork::default()).unwrap();
        assert_eq!(status_of(&client, format!("{}/a", server.uri())).await, 200);
    }

    #[tokio::test]
    async fn a_302_is_returned_unfollowed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/b"))
            .mount(&server)
            .await;
        let client = http_client(&LoadedNetwork::default()).unwrap();
        assert_eq!(status_of(&client, format!("{}/a", server.uri())).await, 302);
    }

    #[tokio::test]
    async fn a_cross_origin_307_is_returned_unfollowed() {
        let target = MockServer::start().await;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/a"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/b", target.uri()).as_str()),
            )
            .mount(&server)
            .await;
        let client = http_client(&LoadedNetwork::default()).unwrap();
        assert_eq!(status_of(&client, format!("{}/a", server.uri())).await, 307);
        assert!(target.received_requests().await.unwrap().is_empty());
    }
}
