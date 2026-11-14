//! Claude Agent SDK mode: `fontaine` as the executable of an unmodified TypeScript or Python Agent
//! SDK program. The SDK launches it with the stream-json flags and no `-p`; Fontaine then serves
//! the server side of Claude Code's SDK protocol on its own agent loop, models, tools, sessions,
//! guardian and permission pipeline. The host's callbacks (`canUseTool`, hooks, SDK MCP servers)
//! plug into the seams that already exist, so none of them is a route around the floor, the
//! guardian or the risk-class registry.

pub mod approver;
pub mod cli;
pub mod control;
pub mod hooks;
pub mod mcp;
pub mod router;
pub mod translate;
pub mod wire;

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use fontaine_core::agent::HostMcp;
use fontaine_core::config::{Config, Origin, Slot};
use fontaine_core::model_roles::Roles;
use fontaine_core::permissions::HumanChannel;
use rig_core::schemars::Schema;
use serde_json::json;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::mpsc;

use self::approver::SdkApprover;
use self::control::{Attached, Control, Facts, Shared};
use self::mcp::SdkServers;
use self::router::{Command, Next, Router};
use self::translate::ClaudeSink;
use crate::headless::{self, Cancel, InputSource};
use crate::{Cli, Dirs, Host, SessionPrep, start_headless};

/// Puts the servers `--mcp-config` declared into the configuration at the CLI scope, where they
/// start without asking. With `--strict-mcp-config` nothing else starts.
pub fn apply_to_config(config: &mut Config, options: &cli::Options) {
    if options.strict_mcp {
        config.mcp_servers.clear();
        config.mcp_origins.clear();
    }
    for (name, server) in &options.mcp_servers {
        config.mcp_servers.insert(name.clone(), server.clone());
        config.mcp_origins.insert(
            name.clone(),
            Origin {
                slot: Slot::Cli,
                user_trust: true,
            },
        );
    }
}

/// Reads stdin on a detached thread: a blocking read on a pipe the host keeps open would
/// otherwise keep the process from exiting once the run is over.
struct DetachedStdin {
    rx: mpsc::UnboundedReceiver<std::io::Result<Vec<u8>>>,
    chunk: Vec<u8>,
    at: usize,
}

impl DetachedStdin {
    fn spawn() -> DetachedStdin {
        use std::io::Read;
        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        if tx.send(Ok(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                }
            }
        });
        DetachedStdin {
            rx,
            chunk: Vec::new(),
            at: 0,
        }
    }
}

impl AsyncRead for DetachedStdin {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if self.at < self.chunk.len() {
                let take = (self.chunk.len() - self.at).min(buf.remaining());
                let from = self.at;
                buf.put_slice(&self.chunk[from..from + take]);
                self.at += take;
                return Poll::Ready(Ok(()));
            }
            match self.rx.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                // The thread ended: end of input.
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(e)),
                Poll::Ready(Some(Ok(chunk))) => {
                    self.chunk = chunk;
                    self.at = 0;
                }
            }
        }
    }
}

/// The session loop's input: the host's user messages, in the order they arrived.
struct SdkInput {
    router: Arc<Router>,
}

impl InputSource for SdkInput {
    fn is_exhausted(&self) -> bool {
        false
    }

    async fn next(
        &mut self,
        runtime: &mut fontaine_core::agent::AgentRuntime,
    ) -> Result<Option<String>, String> {
        loop {
            match self.router.next().await {
                Next::Message(text) => return Ok(Some(text)),
                Next::Command(Command::SetModel { model, reply }) => {
                    let switched = runtime
                        .switch_main_model_for_session(model.as_deref())
                        .await
                        .map(|switched| switched.reference)
                        .map_err(|e| fontaine_tools::error_chain::chain(&e));
                    // The host may have stopped waiting.
                    let _ = reply.send(switched);
                }
                Next::Failed(why) => return Err(why),
                Next::Eof => return Ok(None),
            }
        }
    }

    fn pending(&self) -> bool {
        self.router.pending()
    }

    fn failure_code(&self) -> u8 {
        1
    }
}

fn model_rows(config: &Config) -> Vec<serde_json::Value> {
    Roles::from_config(config)
        .map(|roles| {
            roles
                .names()
                .map(|name| {
                    json!({
                        "value": name,
                        "displayName": name,
                        "description": "a model role",
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Ends the run when the host terminates or interrupts the process.
async fn shut_down_on_signal(router: Arc<Router>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        if tokio::signal::ctrl_c().await.is_err() {
            return;
        }
    }
    router.shut_down();
}

/// Runs a Claude SDK session on the stdin and stdout of the process.
pub async fn run(
    sdk: cli::Options,
    command_line: &Cli,
    mut options: headless::Options,
    prep: SessionPrep,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
) -> u8 {
    let roles: Vec<String> = Roles::from_config(config)
        .map(|roles| roles.names().map(str::to_string).collect())
        .unwrap_or_default();
    let mut servers: Vec<String> = config.mcp_servers.keys().cloned().collect();
    servers.extend(sdk.mcp_servers.iter().map(|(name, _)| name.clone()));
    servers.extend(sdk.sdk_servers.iter().cloned());
    let shared = Shared::new(Facts {
        start_mode: sdk.start_mode.unwrap_or(
            config
                .permissions
                .default_mode
                .unwrap_or(fontaine_core::config::PermissionMode::Manual),
        ),
        roles,
        servers,
        models: model_rows(config),
        system_prompt: sdk.system_prompt.clone(),
        append_system_prompt: sdk.append_system_prompt.clone(),
        has_json_schema: options.json_schema.is_some(),
    });
    let cancel = Cancel::default();
    let router = Router::start(
        DetachedStdin::spawn(),
        tokio::io::stdout(),
        cancel.clone(),
        Arc::new(Control {
            shared: shared.clone(),
        }),
    );

    tokio::spawn(shut_down_on_signal(router.clone()));

    // The host's hooks, agents, prompts and servers arrive on stdin, and the runtime is built
    // from them.
    router.wait_started().await;
    let init = shared.take_init();
    if let Some(schema) = &init.json_schema {
        match Schema::try_from(schema.clone()) {
            Ok(schema) => options.json_schema = Some(schema),
            Err(_) => {
                eprintln!("fontaine: `initialize.jsonSchema` must be a JSON Schema object");
                return crate::EXIT_CONFIG;
            }
        }
    }
    let mut sdk_servers = sdk.sdk_servers.clone();
    for name in &init.sdk_servers {
        if !sdk_servers.contains(name) {
            sdk_servers.push(name.clone());
        }
    }
    let partial = options.include_partial_messages;
    let delegate = sdk.permission_prompt;
    let started = {
        let router = router.clone();
        start_headless(
            command_line,
            &options,
            prep,
            config,
            dirs,
            host,
            move |setup| {
                if delegate {
                    setup.approver = Arc::new(SdkApprover::new(router.clone()));
                }
                setup.human_channel = if delegate {
                    HumanChannel::Delegate
                } else {
                    HumanChannel::Absent
                };
                setup.callback_hooks = init.hooks;
                setup.session_agents = init.agents;
                if !sdk_servers.is_empty() {
                    setup.host_mcp = Some(HostMcp {
                        servers: sdk_servers,
                        factory: Arc::new(SdkServers {
                            router: router.clone(),
                        }),
                    });
                }
                setup.root_tools = sdk.root_tools.clone();
                setup.system_prompt = sdk.system_prompt.clone().or(init.system_prompt);
                setup.append_system_prompt = sdk
                    .append_system_prompt
                    .clone()
                    .or(init.append_system_prompt);
            },
        )
        .await
    };
    let started = match started {
        Ok(started) => started,
        Err(code) => return code,
    };
    let mut runtime = started.runtime;
    // Servers that fail to start are reported on stderr and skipped.
    runtime.connect_mcp(&[]).await;
    shared.attach(Attached {
        state: runtime.state.clone(),
        mcp: runtime.mcp_states_reader(),
    });
    router.attach(runtime.steering.clone(), true);

    let mut sink = ClaudeSink::new(router.clone(), partial);
    let code = headless::run_session(
        options,
        &mut runtime,
        &dirs.data,
        started.guardian_login,
        SdkInput {
            router: router.clone(),
        },
        &mut sink,
        &cancel,
    )
    .await;
    runtime.fire_session_end("other").await;
    router.close();
    router.flush().await;
    code
}
