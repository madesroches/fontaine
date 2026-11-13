use crate::commands::{McpRequest, parse_mcp};
use crate::reports::McpState;
use crate::server_prompt;
use crate::{Action, App, Ui};
use fontaine_core::agent::{AgentRuntime, McpServerState, ReconnectTarget};

impl<R: FnMut(&mut App)> Ui<R> {
    /// The servers `/mcp` lists, by name: the ones that were never started (left off, or
    /// withheld by workspace trust) and the state of each one the session attempted, which
    /// changes as servers drop and come back.
    pub(crate) fn mcp_listing(&self, runtime: &AgentRuntime) -> Vec<(String, McpState)> {
        let mut servers = self.mcp_status.clone();
        servers.extend(runtime.mcp_states().into_iter().map(|(name, state)| {
            let state = match state {
                McpServerState::Connected {
                    tools,
                    resources,
                    prompts,
                } => McpState::Connected {
                    tools,
                    resources,
                    prompts,
                },
                McpServerState::Reconnecting { attempt } => McpState::Pending { attempt },
                McpServerState::Failed(error) => McpState::Failed(error),
            };
            (name, state)
        }));
        servers.sort_by(|a, b| a.0.cmp(&b.0));
        servers
    }

    /// `/mcp reconnect <name|all>`: restarts servers by hand and says how each one came out.
    pub(crate) async fn mcp_command(&mut self, runtime: &mut AgentRuntime, argument: &str) {
        let target = match parse_mcp(argument) {
            Ok(McpRequest::List) => {
                self.mcp(runtime);
                return;
            }
            Ok(McpRequest::Reconnect(target)) => target,
            Err(usage) => {
                self.app.transcript.error(usage);
                return;
            }
        };
        self.app.transcript.notice(match &target {
            ReconnectTarget::One(name) => format!("reconnecting `{name}`"),
            ReconnectTarget::AllFailed => "reconnecting the failed MCP servers".to_string(),
        });
        self.draw(runtime);
        let outcomes = runtime.reconnect_mcp(target.clone()).await;
        if outcomes.is_empty() {
            self.app.transcript.notice("no MCP server has failed");
        }
        for outcome in outcomes {
            match outcome.error {
                None => self
                    .app
                    .transcript
                    .notice(format!("MCP server `{}` is connected", outcome.server)),
                Some(e) => self.app.transcript.error(e),
            }
        }
    }

    /// Starts the MCP servers: the ones in the user's own settings at once, a repository's only
    /// after the human approves each. Returns false when the input closed meanwhile.
    pub(crate) async fn start_mcp(&mut self, runtime: &mut AgentRuntime) -> bool {
        let mut approved = Vec::new();
        if !runtime.project_executables() {
            for name in runtime.config.mcp_servers.keys() {
                let project = runtime
                    .config
                    .mcp_origins
                    .get(name)
                    .is_some_and(|origin| origin.is_project());
                if project {
                    self.mcp_status.push((name.clone(), McpState::NotTrusted));
                }
            }
        }
        for plan in runtime.mcp_plan().into_iter().filter(|p| p.needs_approval) {
            self.app
                .begin_server(server_prompt::new(&plan.name, &plan.description));
            let answer = loop {
                self.draw(runtime);
                let Some(input) = self.input.recv().await else {
                    return false;
                };
                match self.act_on(runtime, input) {
                    Action::Server(answer) => break answer,
                    Action::Quit => return false,
                    _ => {}
                }
            };
            if answer {
                match runtime.approve_mcp_server(&plan) {
                    Ok(()) => {}
                    Err(e) => self.app.transcript.error(format!(
                        "could not remember the approval of `{}`: {}",
                        plan.name,
                        fontaine_tools::error_chain::chain(&e)
                    )),
                }
                approved.push(plan.name.clone());
            } else {
                self.mcp_status.push((plan.name.clone(), McpState::LeftOff));
                self.app
                    .transcript
                    .notice(format!("MCP server `{}` was left off", plan.name));
            }
        }
        if runtime
            .mcp_plan()
            .iter()
            .any(|p| !p.needs_approval || approved.contains(&p.name))
        {
            self.app.transcript.notice("starting MCP servers");
            self.draw(runtime);
        }
        for outcome in runtime.connect_mcp(&approved).await {
            match outcome.error {
                None => {
                    self.app
                        .transcript
                        .notice(format!("MCP server `{}` is connected", outcome.server));
                }
                Some(e) => {
                    // A server whose definition could not be built has no slot, so `/mcp`
                    // lists it from here.
                    if !runtime
                        .mcp_states()
                        .iter()
                        .any(|(n, _)| *n == outcome.server)
                    {
                        self.mcp_status
                            .push((outcome.server.clone(), McpState::Failed(e.clone())));
                    }
                    self.app.transcript.error(e);
                }
            }
        }
        true
    }
}
