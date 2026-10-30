use crate::commands;
use crate::reports;
use crate::reports::McpState;
use crate::{App, Ui};
use fontaine_core::agent::AgentRuntime;

impl<R: FnMut(&mut App)> Ui<R> {
    pub(crate) fn status(&mut self, runtime: &AgentRuntime) {
        let guardian = runtime.state.guardian();
        let mcp_connected = self
            .mcp_status
            .iter()
            .filter(|(_, state)| *state == McpState::Connected)
            .count();
        let text = reports::status(&reports::StatusInfo {
            version: env!("CARGO_PKG_VERSION"),
            session_id: &runtime.session.id,
            roots: &runtime.roots(),
            scratch: runtime.state.scratch.as_deref(),
            memory: runtime.state.memory_root.as_deref(),
            mode: runtime.state.mode(),
            model: &runtime.models.main.reference.to_string(),
            guardian_label: guardian.label(),
            guardian_unavailable: guardian.unavailable_reason(),
            guardian_off: runtime.state.guardian_off(),
            guardian_profile: runtime.guardian_profile().as_str(),
            output_style: runtime.output_style(),
            mcp_connected,
            mcp_configured: runtime.config.mcp_servers.len(),
        });
        self.app.transcript.notice(text);
    }

    pub(crate) fn permissions(&mut self, runtime: &AgentRuntime) {
        let mut text = reports::permissions(
            runtime.state.mode(),
            &runtime.state.rules(),
            &runtime.state.suspended_allow_rules(),
            &runtime.roots(),
            self.user_config.as_deref(),
        );
        if runtime.state.guardian_off() {
            text.push_str("\nguardian is off for this session; these rules are not being enforced");
        }
        self.app.transcript.notice(text);
    }

    pub(crate) fn mcp(&mut self, runtime: &AgentRuntime, argument: &str) {
        if !argument.is_empty() {
            self.app.transcript.error(format!(
                "/mcp takes no arguments (`{argument}`): it only lists the servers; reconnecting, enabling and disabling are not supported yet"
            ));
            return;
        }
        self.app.transcript.notice(reports::mcp_with_origins(
            &self.mcp_status,
            &runtime.config.mcp_origins,
        ));
    }

    pub(crate) fn plugins(&mut self, runtime: &AgentRuntime) {
        let text = match fontaine_core::plugins::read_registry(runtime.config_dir()) {
            Ok(registry) => reports::plugins(
                &registry,
                runtime.extensions(),
                &runtime.config.hooks,
                &runtime.config.mcp_servers,
                &runtime.config.mcp_origins,
            ),
            Err(e) => format!("cannot read the plugin registry: {e}"),
        };
        self.app.transcript.notice(text);
    }

    /// `/copy`: puts the `n`th latest assistant reply on the clipboard.
    pub(crate) fn copy(&mut self, argument: &str) {
        let n = match commands::reply_number(argument) {
            Ok(n) => n,
            Err(e) => {
                self.app.shown_mut().error(e);
                return;
            }
        };
        let Some(reply) = self.app.shown().assistant_reply(n).map(str::to_string) else {
            self.app
                .shown_mut()
                .error(format!("there is no assistant reply number {n} to copy"));
            return;
        };
        match (self.clipboard)(&reply) {
            Ok(()) => self.app.shown_mut().notice(format!(
                "copied reply {n} ({} characters) to the clipboard",
                reply.chars().count()
            )),
            Err(e) => self.app.shown_mut().error(format!(
                "could not copy to the clipboard: {}",
                fontaine_tools::error_chain::chain(&e)
            )),
        }
    }

    /// `/expand`: prints call `n` (the latest by default) in full, as new transcript lines.
    pub(crate) fn expand(&mut self, argument: &str) {
        let n = match commands::call_number(argument) {
            Ok(n) => n,
            Err(e) => {
                self.app.shown_mut().error(e);
                return;
            }
        };
        if let Err(e) = self.app.shown_mut().expand(n) {
            self.app.shown_mut().error(e);
        }
    }
}
