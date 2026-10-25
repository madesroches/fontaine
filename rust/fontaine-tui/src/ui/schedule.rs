//! `/loop` and the scheduled turns it and the model's scheduling tools lead to.

use std::ops::ControlFlow::{self, Continue};
use std::time::Duration;

use fontaine_core::agent::AgentRuntime;
use fontaine_core::ledger::Record;
use fontaine_core::schedule::{
    FALLBACK_SECONDS, Job, JobSpec, Kind, LoopStep, Origin, SELF_PACED_NOTE, ScheduleError,
    Scheduler, local_time,
};
use fontaine_tools::error_chain::chain;

use crate::commands::{Interval, LoopRequest, parse_loop};
use crate::{App, Ui};

/// The longest an idle interface sleeps with a task pending before it looks at the clock again,
/// so a wall clock that jumped (a suspended laptop, a changed time) is noticed.
pub(super) const IDLE_POLL: Duration = Duration::from_secs(1);

const PREVIEW_CHARS: usize = 60;

fn preview(prompt: &str) -> String {
    let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PREVIEW_CHARS {
        return flat;
    }
    let cut: String = flat.chars().take(PREVIEW_CHARS).collect();
    format!("{cut}...")
}

fn describe(job: &Job) -> String {
    let by = match job.origin {
        Origin::User => "you",
        Origin::Model => "the model",
    };
    format!(
        "{}  {}  next {}  set by {by}  {}",
        job.id,
        job.cadence(),
        local_time(job.next_ms),
        preview(&job.prompt)
    )
}

impl<R: FnMut(&mut App)> Ui<R> {
    /// The scheduler's own complaints (a schedule that could not be saved), shown once.
    pub(super) fn show_scheduler_warning(&mut self, runtime: &AgentRuntime) {
        if let Some(warning) = runtime.session.scheduler().take_warning() {
            self.app.transcript.notice(warning);
        }
    }

    /// Esc at the idle prompt while a self-paced loop waits: the loop is over.
    pub(super) fn cancel_wakeup(&mut self, runtime: &AgentRuntime) {
        match runtime.session.scheduler().end_loop() {
            Ok(true) => self.app.transcript.notice("the loop is stopped"),
            Ok(false) => {}
            Err(e) => self.app.transcript.error(chain(&e)),
        }
    }

    /// What follows an iteration of the self-paced loop: the next one, one more try, or the end.
    pub(super) fn end_of_iteration(&mut self, runtime: &AgentRuntime) {
        match runtime.session.scheduler().iteration_ended() {
            Ok(LoopStep::FallbackArmed) => self.app.transcript.notice(format!(
                "the loop scheduled no next iteration; trying once more in {} minutes",
                FALLBACK_SECONDS / 60
            )),
            Ok(LoopStep::Ended) => self.app.transcript.notice(
                "the loop scheduled no next iteration again, so it has ended; /loop <prompt> starts it again",
            ),
            Ok(LoopStep::Continues | LoopStep::NoLoop) => {}
            Err(e) => {
                self.app.transcript.error(chain(&e));
                if runtime.session.scheduler().active_loop().is_none() {
                    self.app
                        .transcript
                        .notice("the loop has ended; /loop <prompt> starts it again");
                }
            }
        }
    }

    pub(super) async fn loop_command(
        &mut self,
        runtime: &mut AgentRuntime,
        typed: &str,
        argument: &str,
    ) -> ControlFlow<()> {
        if !runtime.config.schedule.enabled {
            self.app.transcript.error(
                "scheduling is off; set `enabled = true` under `[schedule]` in your settings to use /loop",
            );
            return Continue(());
        }
        let request = match parse_loop(argument) {
            Ok(request) => request,
            Err(message) => {
                self.app.transcript.error(message);
                return Continue(());
            }
        };
        let scheduler = runtime.session.scheduler().clone();
        match request {
            LoopRequest::List => self.list_schedule(&scheduler),
            LoopRequest::Stop(id) => self.stop_schedule(&scheduler, id.as_deref()),
            LoopRequest::Every(interval) => {
                let Some(prompt) = self.start_interval(&scheduler, interval) else {
                    return Continue(());
                };
                return self.first_iteration(runtime, typed, prompt).await;
            }
            LoopRequest::SelfPaced(prompt) => {
                match scheduler.begin_loop(&prompt) {
                    Ok(_) => {}
                    Err(ScheduleError::LoopActive) => {
                        self.app.transcript.error(
                            "a self-paced loop is already running; /loop stop ends it first",
                        );
                        return Continue(());
                    }
                    Err(e) => {
                        self.app.transcript.error(chain(&e));
                        return Continue(());
                    }
                }
                self.app.transcript.notice(
                    "self-paced loop: the model picks the delay between iterations; Esc at the idle prompt or /loop stop ends it",
                );
                let model_prompt = format!("{prompt}\n\n{SELF_PACED_NOTE}");
                let flow = self.first_iteration(runtime, typed, model_prompt).await;
                if flow.is_continue() {
                    self.end_of_iteration(runtime);
                }
                return flow;
            }
        }
        Continue(())
    }

    /// Creates the recurring task and says which cadence it got; the prompt to run now.
    fn start_interval(&mut self, scheduler: &Scheduler, interval: Interval) -> Option<String> {
        let job = match scheduler.create(JobSpec {
            cron: interval.cron,
            prompt: interval.prompt.clone(),
            recurring: true,
            origin: Origin::User,
        }) {
            Ok(job) => job,
            Err(e) => {
                self.app.transcript.error(chain(&e));
                return None;
            }
        };
        let rounded = interval
            .rounded_from
            .map(|written| format!("{written} is not an even cadence; using it as "))
            .unwrap_or_default();
        self.app.transcript.notice(format!(
            "{rounded}{} (task {}, next run {}); /loop stop {} cancels it",
            job.cadence(),
            job.id,
            local_time(job.next_ms),
            job.id
        ));
        Some(interval.prompt)
    }

    /// The first iteration runs now as the line the human typed, which is what the guardian
    /// then takes as their intent for every later one.
    async fn first_iteration(
        &mut self,
        runtime: &mut AgentRuntime,
        typed: &str,
        prompt: String,
    ) -> ControlFlow<()> {
        runtime.append(Record::UserMessage {
            text: typed.to_string(),
        });
        self.converse(runtime, typed.to_string(), Some(prompt))
            .await
    }

    fn list_schedule(&mut self, scheduler: &Scheduler) {
        let mut lines: Vec<String> = scheduler.list().iter().map(describe).collect();
        if let Some(active) = scheduler.active_loop() {
            lines.insert(
                0,
                format!("self-paced loop {}: {}", active.id, preview(&active.prompt)),
            );
        }
        if lines.is_empty() {
            self.app.transcript.notice("no scheduled tasks");
        } else {
            self.app.transcript.notice(lines.join("\n"));
        }
    }

    fn stop_schedule(&mut self, scheduler: &Scheduler, id: Option<&str>) {
        let outcome = match id {
            None => scheduler.clear().map(|n| match n {
                0 => "no scheduled tasks".to_string(),
                1 => "cancelled 1 scheduled task".to_string(),
                n => format!("cancelled {n} scheduled tasks"),
            }),
            Some(id) => self.stop_one(scheduler, id),
        };
        match outcome {
            Ok(message) => self.app.transcript.notice(message),
            Err(e) => self.app.transcript.error(chain(&e)),
        }
    }

    fn stop_one(&self, scheduler: &Scheduler, id: &str) -> Result<String, ScheduleError> {
        let loop_id = scheduler.active_loop().map(|active| active.id);
        let wakeup = scheduler
            .list()
            .iter()
            .any(|job| job.id == id && matches!(job.kind, Kind::Wakeup { .. }));
        if loop_id.as_deref() == Some(id) || wakeup {
            scheduler.end_loop()?;
            return Ok("the loop is stopped".to_string());
        }
        if scheduler.delete(id)? {
            Ok(format!("cancelled scheduled task {id}"))
        } else {
            Ok(format!(
                "no scheduled task has the id `{id}`; /loop list shows them"
            ))
        }
    }
}
