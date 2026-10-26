mod commands;
mod mcp;
mod providers;
mod schedule;
#[cfg(test)]
mod tests;
mod turn;

use std::path::Path;

use crate::agents_panel::{AgentsPanel, Detail, PanelRow};
use crate::denials::{DeniedList, UnavailablePrompt};
use crate::ui::schedule::IDLE_POLL;
use crate::ui::turn::TurnEnd;
use crate::{Action, App, Command, Prompt, Ui, parse_command};
use crate::{reports, statusbar};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::PermissionMode;
use fontaine_core::ledger::Record;
use fontaine_core::notice::Notice;
use fontaine_tools::gate::ROOT_AGENT;
use fontaine_tools::lock::read_recovering;

impl<R: FnMut(&mut App)> Ui<R> {
    fn draw(&mut self, runtime: &AgentRuntime) {
        self.app.mode = runtime.state.mode();
        self.app.model = runtime.models.main.reference.to_string();
        self.app.prune_closed();
        let rows = runtime.state.agents().rows();
        self.app.running_agents = rows.iter().filter(|r| r.status.is_running()).count();
        if self.app.agents.is_some() {
            self.refresh_panel(runtime, &rows);
        }
        self.app.queued = runtime.steering.queued();
        let scheduler = runtime.session.scheduler();
        let scheduling = runtime.config.schedule.enabled;
        self.app.scheduled = if scheduling { scheduler.count() } else { 0 };
        self.app.wakeup_pending = scheduling && scheduler.wakeup_pending();
        // The completion index is advisory: a stale list only misses a suggestion.
        let files = read_recovering(&runtime.files).clone();
        self.app.set_files(files);
        (self.render)(&mut self.app);
    }

    /// Brings the agents panel up to date with the tree, the decisions and the queue.
    fn refresh_panel(&mut self, runtime: &AgentRuntime, rows: &[fontaine_agents::tree::AgentRow]) {
        let panel_rows: Vec<PanelRow> = rows
            .iter()
            .map(|row| PanelRow {
                id: row.id.clone(),
                agent_type: row.agent_type.clone(),
                model: row.model.clone(),
                status: row.status,
                tokens: row.spend.tokens(),
                token_budget: row.budget.tokens,
                cost_usd: row.spend.cost_usd,
                cost_budget: row.budget.cost_usd,
                depth: row.depth,
                stopped_because: row.exhausted.map(|why| why.describe()),
            })
            .collect();
        let selected = self.app.agents.as_ref().and_then(AgentsPanel::selected_id);
        let detail = match (&self.app.agents, selected) {
            (Some(panel), Some(id)) if panel.is_inspecting() => Some(Detail {
                pending: self.app.pending_for(&id),
                decisions: runtime
                    .state
                    .decisions_of(&id)
                    .iter()
                    .map(|d| {
                        format!(
                            "{} {} ({}{})",
                            d.tool,
                            d.decision,
                            d.tier,
                            d.rule
                                .as_deref()
                                .map_or(String::new(), |r| format!(": {r}"))
                        )
                    })
                    .collect(),
                log: runtime.state.agent_log(&id),
                id,
            }),
            _ => None,
        };
        if let Some(panel) = &mut self.app.agents {
            panel.set_rows(panel_rows);
            panel.detail = detail;
        }
    }

    fn on_prompt(&mut self, runtime: Option<&AgentRuntime>, prompt: Prompt) {
        match prompt {
            // The call's turn was cut short while the prompt sat in the queue.
            Prompt::Ask(_, reply) if reply.is_closed() => {}
            Prompt::Unavailable(_, reply) if reply.is_closed() => {}
            Prompt::Question(_, _, reply) if reply.is_closed() => {}
            Prompt::Question(agent, questions, reply) => {
                self.app.begin_question(agent, questions, reply);
            }
            Prompt::Ask(request, reply) => self.app.begin_approval(*request, reply),
            Prompt::Unavailable(info, reply) => {
                let candidates = runtime.map_or_else(Vec::new, AgentRuntime::guardian_candidates);
                self.app
                    .begin_unavailable(UnavailablePrompt::new(*info, candidates, reply));
            }
            Prompt::Notice(severity, message) => self.app.transcript.push_notice(Notice {
                severity,
                headline: message,
                hint: None,
            }),
        }
    }

    /// Handles what queued up during a turn that has ended: the asks belong to calls that no
    /// longer exist, so they are dropped, but notices still count.
    fn drain_prompts(&mut self) {
        while let Ok(prompt) = self.prompts.try_recv() {
            match prompt {
                Prompt::Notice(severity, message) => self.app.transcript.push_notice(Notice {
                    severity,
                    headline: message,
                    hint: None,
                }),
                // A background agent's call is still waiting for its answer.
                Prompt::Ask(request, reply)
                    if request.agent != ROOT_AGENT && !reply.is_closed() =>
                {
                    self.app.begin_approval(*request, reply);
                }
                Prompt::Ask(..) | Prompt::Unavailable(..) | Prompt::Question(..) => {}
            }
        }
    }

    fn cycle_mode(&mut self, runtime: &AgentRuntime) {
        let state = &runtime.state;
        state.set_mode(statusbar::next_mode(state.mode()));
        if state.mode() == PermissionMode::Auto {
            self.announce_auto(runtime);
        }
    }

    /// What the human should know on entering `auto`: the allow rules it sets aside, and a
    /// guardian that cannot be asked.
    pub(crate) fn announce_auto(&mut self, runtime: &AgentRuntime) {
        let suspended = runtime.state.suspended_allow_rules();
        if !suspended.is_empty() {
            self.app.transcript.notice(format!(
                "auto mode sets aside these broad allow rules while it is on: {}",
                suspended.join(", ")
            ));
        }
        let guardian = runtime.state.guardian();
        if let Some(reason) = guardian.unavailable_reason() {
            self.app.transcript.error(format!(
                "the guardian model {} is unavailable ({reason}); calls that need it are denied until you fix it with /guardian-model",
                guardian.label()
            ));
        }
    }

    pub(crate) async fn run(&mut self, runtime: &mut AgentRuntime, launch_dir: &Path) {
        let tree = runtime.state.agents().clone();
        loop {
            // Registered before the tree is read: a child that finishes between the read and
            // the wait would otherwise go unseen until the next key.
            let changed = tree.changed().notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some((purpose, reference)) = self.pending_pick.take() {
                self.apply_pick(runtime, purpose, &reference).await;
            }
            while let Some(request) = self.pending_provider.pop_front() {
                self.provider_request(runtime, request).await;
            }
            // A background agent that finished while nothing was running has news for the
            // main conversation, which gets a turn of its own to read it.
            if tree.has_notifications(ROOT_AGENT) {
                if matches!(
                    self.notification_turn(runtime).await,
                    Some(TurnEnd::InputClosed)
                ) {
                    return;
                }
                continue;
            }
            let mut tick = None;
            if runtime.config.schedule.enabled {
                let scheduler = runtime.session.scheduler().clone();
                if let Some(fired) = scheduler.due() {
                    self.show_scheduler_warning(runtime);
                    if self.scheduled_turn(runtime, fired).await.is_break() {
                        return;
                    }
                    continue;
                }
                self.show_scheduler_warning(runtime);
                tick = scheduler.until_next_wake().map(|wait| wait.min(IDLE_POLL));
            }
            self.draw(runtime);
            let action = tokio::select! {
                input = self.input.recv() => match input {
                    Some(input) => self.app.handle_input(input),
                    None => return,
                },
                Some(prompt) = self.prompts.recv() => {
                    self.on_prompt(Some(runtime), prompt);
                    Action::None
                }
                _ = &mut changed => Action::None,
                () = async {
                    match tick {
                        Some(wait) => tokio::time::sleep(wait).await,
                        None => std::future::pending().await,
                    }
                } => Action::None,
            };
            match action {
                Action::Quit => return,
                Action::CycleMode => self.cycle_mode(runtime),
                Action::Submit(text) => {
                    self.remember(&text);
                    if self.submit(runtime, launch_dir, text).await.is_break() {
                        return;
                    }
                }
                Action::Shell(command) => {
                    self.remember(&format!("!{command}"));
                    if self.shell(runtime, command).await.is_break() {
                        return;
                    }
                }
                Action::EditQueued => self.edit_queued(runtime),
                Action::CancelWakeup => self.cancel_wakeup(runtime),
                Action::RetryDenied(index) => self.retry_denied(runtime, index),
                Action::Guardian(answer) => self.answer_unavailable(runtime, answer),
                Action::KillAgent(id) => self.kill_agent(runtime, &id),
                Action::PickModel(purpose, reference) => {
                    self.apply_pick(runtime, purpose, &reference).await;
                }
                Action::Providers(request) => self.provider_request(runtime, request).await,
                Action::Config(change) => self.config_change(runtime, change),
                Action::OpenPicker(slot) => self.open_config_picker(runtime, slot).await,
                Action::Server(_) | Action::None | Action::Interrupt | Action::Queue(_) => {}
            }
        }
    }

    fn kill_agent(&mut self, runtime: &AgentRuntime, id: &str) {
        if runtime.state.agents().kill(id) {
            self.app.transcript.notice(format!("killed {id}"));
        }
    }

    async fn submit(
        &mut self,
        runtime: &mut AgentRuntime,
        launch_dir: &Path,
        text: String,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        match parse_command(&text) {
            Some(Command::Exit) => return Break(()),
            Some(Command::AddDir(path)) => self.add_dir(runtime, launch_dir, &path),
            Some(Command::Denied) => {
                self.app.denied = Some(DeniedList::new(runtime.state.denied_calls()));
            }
            Some(Command::Config) => self.open_config(runtime),
            Some(Command::Agents) => self.app.agents = Some(AgentsPanel::new()),
            Some(Command::Todos) => self.app.todos.toggle(),
            Some(Command::OutputStyle(name)) => self.output_style(runtime, &name),
            Some(Command::Model(reference)) => self.model(runtime, &reference).await,
            Some(Command::GuardianModel(reference)) => self.guardian_model(runtime, &reference),
            Some(Command::Login(account)) => self.login(runtime, &account).await,
            Some(Command::Help) => {
                let (skills, _) = crate::commands::skill_menu(runtime.slash_skills());
                self.app.transcript.notice(reports::help(&skills));
            }
            Some(Command::Status) => self.status(runtime),
            Some(Command::Theme(name)) => self.theme(&name),
            Some(Command::Permissions) => self.permissions(runtime),
            Some(Command::Mcp(argument)) => self.mcp(&argument),
            Some(Command::Hooks) => {
                let hooks = &runtime.config.hooks;
                self.app.transcript.notice(reports::hooks(
                    &hooks.pre_tool_use,
                    &hooks.post_tool_use,
                    &hooks.permission_denied,
                    runtime.project_executables(),
                ));
            }
            Some(Command::Skills) => {
                self.app
                    .transcript
                    .notice(reports::skills(&runtime.extensions().skills));
            }
            Some(Command::Loop(argument)) => {
                return self.loop_command(runtime, &text, &argument).await;
            }
            Some(Command::Plan(description)) => {
                runtime.state.set_mode(PermissionMode::Plan);
                self.app.transcript.notice("plan mode on");
                if !description.is_empty() {
                    return self.converse(runtime, description, None).await;
                }
            }
            Some(Command::Copy(argument)) => self.copy(&argument),
            Some(Command::Clear(name)) => self.clear(runtime, &name).await,
            Some(Command::Compact(focus)) => return self.compact(runtime, focus).await,
            Some(Command::Context(argument)) => self.context(runtime, &argument).await,
            Some(Command::Other { name, argument }) => {
                let known = runtime
                    .slash_skills()
                    .iter()
                    .any(|(skill, _, _)| *skill == name);
                if !known {
                    self.app.transcript.error(format!(
                        "unknown command /{name}; type / to see the commands and skills"
                    ));
                    return Continue(());
                }
                // Recorded before the skill's commands run so the guardian judging them has
                // this turn's intent in the ledger.
                runtime.append(Record::UserMessage { text: text.clone() });
                let prompt = match self.expand_serving(runtime, &name, &argument).await {
                    Ok(Ok(prompt)) => prompt,
                    Ok(Err(e)) => {
                        self.app.transcript.error(format!(
                            "/{name}: {}",
                            fontaine_tools::error_chain::chain(&e)
                        ));
                        return Continue(());
                    }
                    Err(TurnEnd::InputClosed) => return Break(()),
                    Err(_) => {
                        self.app.transcript.notice("interrupted");
                        return Continue(());
                    }
                };
                return self.converse(runtime, text, Some(prompt)).await;
            }
            None => return self.converse(runtime, text, None).await,
        }
        Continue(())
    }

    /// Adds a submitted input to the prompt history; a history file that cannot be written is
    /// reported once.
    fn remember(&mut self, text: &str) {
        if let Some(error) = self.app.history.push(text) {
            self.app.transcript.notice(error);
        }
    }

    /// Up with messages queued: they come back into the input to be edited.
    fn edit_queued(&mut self, runtime: &AgentRuntime) {
        let messages = runtime.steering.retract();
        if messages.is_empty() {
            // The hook delivered them since the last draw, so the display is stale.
            self.app.queued.clear();
            self.app.recall_older();
        } else {
            self.app.restore_to_input(&messages.join("\n\n"));
        }
    }
}
