use crate::reports::McpState;
use crate::server_prompt::ServerPrompt;
use crate::{Action, App, Ui};
use fontaine_core::agent::AgentRuntime;

impl<R: FnMut(&mut App)> Ui<R> {
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
            self.app.begin_server(ServerPrompt::new(
                plan.name.clone(),
                plan.description.clone(),
            ));
            let answer = loop {
                self.draw(runtime);
                let Some(input) = self.input.recv().await else {
                    return false;
                };
                match self.app.handle_input(input) {
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
                    self.mcp_status
                        .push((outcome.server.clone(), McpState::Connected));
                    self.app
                        .transcript
                        .notice(format!("MCP server `{}` is connected", outcome.server));
                }
                Some(e) => {
                    self.mcp_status
                        .push((outcome.server, McpState::Failed(e.clone())));
                    self.app.transcript.error(e);
                }
            }
        }
        true
    }
}
