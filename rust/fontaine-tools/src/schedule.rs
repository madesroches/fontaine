//! The scheduling tools: `cron_create`, `cron_jobs`, `cron_delete` and `schedule_wakeup`. They
//! name no file and only reach the session's scheduler behind the [`ScheduleStore`] seam, and
//! only the root agent may call them: a subagent that could plant a standing prompt would have
//! the main conversation run text its own parent never saw.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::Deserialize;
use serde_json::json;

use crate::error_chain::chain;
use crate::gate::{AgentId, ROOT_AGENT};

pub const MAX_JOBS: usize = 50;
pub const MAX_PROMPT_CHARS: usize = 2_000;
pub const MIN_WAKE_SECONDS: u64 = 60;
pub const MAX_WAKE_SECONDS: u64 = 3_600;
const MAX_REASON_CHARS: usize = 200;

/// Why the scheduler refused or failed. Callers dispatch on the kind to word what the model is
/// told; a failed save keeps its cause as a `source()`.
#[derive(Debug)]
pub enum ScheduleError {
    BadCron(String),
    NeverFires,
    TooMany,
    EmptyPrompt,
    PromptTooLong,
    BadDelay,
    /// `schedule_wakeup` with no self-paced loop running.
    NoLoop,
    /// A second self-paced loop while one runs.
    LoopActive,
    Persist {
        path: PathBuf,
        source: io::Error,
    },
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScheduleError::BadCron(reason) => write!(f, "invalid cron expression: {reason}"),
            ScheduleError::NeverFires => {
                f.write_str("the cron expression never matches a real date in the next 5 years")
            }
            ScheduleError::TooMany => {
                write!(f, "the session already has {MAX_JOBS} scheduled tasks")
            }
            ScheduleError::EmptyPrompt => f.write_str("the prompt is empty"),
            ScheduleError::PromptTooLong => {
                write!(f, "the prompt is longer than {MAX_PROMPT_CHARS} characters")
            }
            ScheduleError::BadDelay => write!(
                f,
                "delay_seconds must be between {MIN_WAKE_SECONDS} and {MAX_WAKE_SECONDS}"
            ),
            ScheduleError::NoLoop => f.write_str(
                "no self-paced loop is running; the user starts one with `/loop <prompt>`",
            ),
            ScheduleError::LoopActive => {
                f.write_str("a self-paced loop is already running; stop it first")
            }
            ScheduleError::Persist { path, .. } => write!(f, "cannot save {}", path.display()),
        }
    }
}

impl std::error::Error for ScheduleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScheduleError::Persist { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl ScheduleError {
    /// Whether the model sent something it can correct, as opposed to a failure of the store.
    fn is_invalid_input(&self) -> bool {
        !matches!(self, ScheduleError::Persist { .. })
    }

    fn for_model(self) -> ToolExecutionError {
        if self.is_invalid_input() {
            ToolExecutionError::invalid_args(self.to_string())
        } else {
            ToolExecutionError::other(chain(&self))
        }
    }
}

/// A scheduled task as the tools show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobInfo {
    pub id: String,
    pub cadence: String,
    /// `you` for a task the user scheduled, `model` for one the model did.
    pub origin: &'static str,
    /// The next run, in the user's local time.
    pub next_run: String,
    pub prompt: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewJob {
    pub cron: String,
    pub prompt: String,
    pub recurring: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeRequest {
    After { seconds: u64, reason: String },
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeOutcome {
    Armed(JobInfo),
    Stopped,
}

/// Where scheduled tasks are kept. `fontaine-core` implements it over the session.
pub trait ScheduleStore: Send + Sync {
    fn create(&self, job: NewJob) -> Result<JobInfo, ScheduleError>;
    fn list(&self) -> Vec<JobInfo>;
    /// Whether a task with this id existed.
    fn delete(&self, id: &str) -> Result<bool, ScheduleError>;
    fn wake(&self, request: WakeRequest) -> Result<WakeOutcome, ScheduleError>;
}

fn require_root(context: &ToolContext) -> Result<(), ToolExecutionError> {
    let agent = context.get::<AgentId>().ok().flatten();
    match agent {
        Some(id) if id.0 == ROOT_AGENT => Ok(()),
        _ => Err(ToolExecutionError::other(
            "scheduling tools are only available to the main agent",
        )),
    }
}

fn describe(job: &JobInfo) -> String {
    format!(
        "{} ({}, set by {}) next run {}",
        job.id, job.cadence, job.origin, job.next_run
    )
}

const PROMPT_PREVIEW_CHARS: usize = 80;

fn preview(prompt: &str) -> String {
    let flat: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PROMPT_PREVIEW_CHARS {
        return flat;
    }
    let cut: String = flat.chars().take(PROMPT_PREVIEW_CHARS).collect();
    format!("{cut}...")
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronCreateArgs {
    pub cron: String,
    pub prompt: String,
    #[serde(default = "default_recurring")]
    pub recurring: bool,
}

fn default_recurring() -> bool {
    true
}

#[derive(Clone)]
pub struct CronCreateTool {
    pub store: Arc<dyn ScheduleStore>,
}

impl Tool for CronCreateTool {
    const NAME: &'static str = "cron_create";
    type Args = CronCreateArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Schedule a prompt to run later in this session, once or on a recurring schedule, \
             when the conversation is idle. `cron` is a standard 5-field expression (minute \
             hour day-of-month month day-of-week) in the user's local time; names such as MON \
             and the L, W and ? forms are not supported. Use it only when the user asks for \
             something to happen later or repeatedly. A recurring task expires after 7 days; at \
             most {MAX_JOBS} tasks; the prompt is at most {MAX_PROMPT_CHARS} characters. The \
             prompt runs as a harness note, not as the user speaking."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "cron": {
                    "type": "string",
                    "description": "Five fields: minute hour day-of-month month day-of-week, local time. For example `*/5 * * * *` or `30 14 * * 1-5`.",
                },
                "prompt": {
                    "type": "string",
                    "description": "What to do when the task is due.",
                },
                "recurring": {
                    "type": "boolean",
                    "description": "True (the default) runs at every match; false runs once at the next match.",
                },
            },
            "required": ["cron", "prompt"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: CronCreateArgs,
    ) -> Result<String, ToolExecutionError> {
        require_root(context)?;
        let recurring = args.recurring;
        let info = self
            .store
            .create(NewJob {
                cron: args.cron,
                prompt: args.prompt,
                recurring,
            })
            .map_err(ScheduleError::for_model)?;
        let kind = if recurring { "recurring" } else { "one-shot" };
        Ok(format!(
            "Scheduled {kind} task {}: {}. Next run {}.",
            info.id, info.cadence, info.next_run
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

#[derive(Clone)]
pub struct CronJobsTool {
    pub store: Arc<dyn ScheduleStore>,
}

impl Tool for CronJobsTool {
    const NAME: &'static str = "cron_jobs";
    type Args = NoArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "List the scheduled tasks of this session: id, cadence, who set it, next run and the \
         start of its prompt."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        _: NoArgs,
    ) -> Result<String, ToolExecutionError> {
        require_root(context)?;
        let jobs = self.store.list();
        if jobs.is_empty() {
            return Ok("No scheduled tasks.".to_string());
        }
        Ok(jobs
            .iter()
            .map(|job| format!("{}: {}", describe(job), preview(&job.prompt)))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronDeleteArgs {
    pub id: String,
}

#[derive(Clone)]
pub struct CronDeleteTool {
    pub store: Arc<dyn ScheduleStore>,
}

impl Tool for CronDeleteTool {
    const NAME: &'static str = "cron_delete";
    type Args = CronDeleteArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Cancel a scheduled task by the id `cron_create` or `cron_jobs` gave.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The task id." },
            },
            "required": ["id"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: CronDeleteArgs,
    ) -> Result<String, ToolExecutionError> {
        require_root(context)?;
        let existed = self
            .store
            .delete(args.id.trim())
            .map_err(ScheduleError::for_model)?;
        if existed {
            Ok(format!("Cancelled scheduled task {}.", args.id.trim()))
        } else {
            Err(ToolExecutionError::invalid_args(format!(
                "no scheduled task has the id `{}`; `cron_jobs` lists them",
                args.id.trim()
            )))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeupArgs {
    #[serde(default)]
    pub delay_seconds: Option<u64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub stop: bool,
}

#[derive(Clone)]
pub struct ScheduleWakeupTool {
    pub store: Arc<dyn ScheduleStore>,
}

fn wake_request(args: WakeupArgs) -> Result<WakeRequest, String> {
    if args.stop {
        if args.delay_seconds.is_some() {
            return Err("`stop` and `delay_seconds` cannot be combined".to_string());
        }
        return Ok(WakeRequest::Stop);
    }
    let seconds = args
        .delay_seconds
        .ok_or("give `delay_seconds`, or `stop: true` to end the loop")?;
    if !(MIN_WAKE_SECONDS..=MAX_WAKE_SECONDS).contains(&seconds) {
        return Err(ScheduleError::BadDelay.to_string());
    }
    let reason = args.reason.unwrap_or_default().trim().to_string();
    if reason.is_empty() {
        return Err("give a short `reason` for the delay".to_string());
    }
    if reason.chars().count() > MAX_REASON_CHARS {
        return Err(format!(
            "`reason` is longer than {MAX_REASON_CHARS} characters"
        ));
    }
    Ok(WakeRequest::After { seconds, reason })
}

impl Tool for ScheduleWakeupTool {
    const NAME: &'static str = "schedule_wakeup";
    type Args = WakeupArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Pace a self-paced loop the user started with `/loop <prompt>`. Call it at the end \
             of an iteration to choose when the loop's own prompt runs next \
             ({MIN_WAKE_SECONDS}-{MAX_WAKE_SECONDS} seconds from now), or with `stop: true` \
             when the work is done. It carries no prompt: it can only re-time the one the user \
             gave. It fails when no such loop is running."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "delay_seconds": {
                    "type": "integer",
                    "minimum": MIN_WAKE_SECONDS,
                    "maximum": MAX_WAKE_SECONDS,
                    "description": "Seconds until the next iteration.",
                },
                "reason": {
                    "type": "string",
                    "description": "Why this delay, in a few words.",
                },
                "stop": {
                    "type": "boolean",
                    "description": "True ends the loop instead of scheduling another iteration.",
                },
            },
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: WakeupArgs,
    ) -> Result<String, ToolExecutionError> {
        require_root(context)?;
        let request = wake_request(args).map_err(ToolExecutionError::invalid_args)?;
        let reason = match &request {
            WakeRequest::After { reason, .. } => Some(reason.clone()),
            WakeRequest::Stop => None,
        };
        match self.store.wake(request).map_err(ScheduleError::for_model)? {
            WakeOutcome::Stopped => Ok("The loop is stopped.".to_string()),
            WakeOutcome::Armed(info) => Ok(format!(
                "The loop runs again at {} ({}).",
                info.next_run,
                reason.unwrap_or_default()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Memory {
        created: Mutex<Vec<NewJob>>,
        jobs: Mutex<Vec<JobInfo>>,
        wakes: Mutex<Vec<WakeRequest>>,
        fail: bool,
        no_loop: bool,
    }

    fn info(id: &str) -> JobInfo {
        JobInfo {
            id: id.to_string(),
            cadence: "every 5 minutes".to_string(),
            origin: "the model",
            next_run: "2026-01-02 03:05".to_string(),
            prompt: "check the build".to_string(),
        }
    }

    impl ScheduleStore for Memory {
        fn create(&self, job: NewJob) -> Result<JobInfo, ScheduleError> {
            if self.fail {
                return Err(ScheduleError::Persist {
                    path: PathBuf::from("/s/schedule.json"),
                    source: io::Error::other("disk full"),
                });
            }
            if job.cron == "bad" {
                return Err(ScheduleError::BadCron("expected 5 fields".to_string()));
            }
            self.created.lock().unwrap().push(job);
            Ok(info("abcd1234"))
        }

        fn list(&self) -> Vec<JobInfo> {
            self.jobs.lock().unwrap().clone()
        }

        fn delete(&self, id: &str) -> Result<bool, ScheduleError> {
            Ok(self.jobs.lock().unwrap().iter().any(|j| j.id == id))
        }

        fn wake(&self, request: WakeRequest) -> Result<WakeOutcome, ScheduleError> {
            if self.no_loop {
                return Err(ScheduleError::NoLoop);
            }
            self.wakes.lock().unwrap().push(request.clone());
            Ok(match request {
                WakeRequest::Stop => WakeOutcome::Stopped,
                WakeRequest::After { .. } => WakeOutcome::Armed(info("wake0001")),
            })
        }
    }

    fn as_agent(id: &str) -> ToolContext {
        let mut context = ToolContext::new();
        context.insert(AgentId(id.to_string())).unwrap();
        context
    }

    fn create_args(cron: &str) -> CronCreateArgs {
        CronCreateArgs {
            cron: cron.to_string(),
            prompt: "check".to_string(),
            recurring: true,
        }
    }

    #[test]
    fn arguments_parse_and_reject_unknown_fields() {
        let ok: CronCreateArgs =
            serde_json::from_value(json!({"cron": "* * * * *", "prompt": "p"})).unwrap();
        assert!(ok.recurring, "recurring defaults to true");
        for bad in [
            json!({"cron": "* * * * *", "prompt": "p", "x": 1}),
            json!({"cron": "* * * * *"}),
        ] {
            assert!(serde_json::from_value::<CronCreateArgs>(bad).is_err());
        }
        assert!(serde_json::from_value::<NoArgs>(json!({"x": 1})).is_err());
        assert!(serde_json::from_value::<NoArgs>(json!({})).is_ok());
        assert!(serde_json::from_value::<CronDeleteArgs>(json!({"id": "a", "x": 1})).is_err());
        assert!(
            serde_json::from_value::<WakeupArgs>(json!({"delay_seconds": 60, "prompt": "p"}))
                .is_err(),
            "a wakeup carries no prompt"
        );
    }

    #[tokio::test]
    async fn create_reports_the_task_and_passes_the_arguments_on() {
        let store = Arc::new(Memory::default());
        let tool = CronCreateTool {
            store: store.clone(),
        };
        let text = tool
            .call(&mut as_agent(ROOT_AGENT), create_args("*/5 * * * *"))
            .await
            .unwrap();
        assert!(
            text.contains("abcd1234") && text.contains("every 5 minutes"),
            "{text}"
        );
        assert_eq!(store.created.lock().unwrap()[0].cron, "*/5 * * * *");
    }

    #[tokio::test]
    async fn a_subagent_or_an_unidentified_caller_is_refused_by_every_tool() {
        let store = Arc::new(Memory::default());
        for mut context in [as_agent("agent-1"), ToolContext::new()] {
            let create = CronCreateTool {
                store: store.clone(),
            };
            assert!(
                create
                    .call(&mut context, create_args("* * * * *"))
                    .await
                    .is_err()
            );
            let jobs = CronJobsTool {
                store: store.clone(),
            };
            assert!(jobs.call(&mut context, NoArgs {}).await.is_err());
            let delete = CronDeleteTool {
                store: store.clone(),
            };
            let args = CronDeleteArgs { id: "a".into() };
            assert!(delete.call(&mut context, args).await.is_err());
            let wake = ScheduleWakeupTool {
                store: store.clone(),
            };
            let args = WakeupArgs {
                delay_seconds: Some(60),
                reason: Some("r".into()),
                stop: false,
            };
            assert!(wake.call(&mut context, args).await.is_err());
        }
        assert!(store.created.lock().unwrap().is_empty());
        assert!(store.wakes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refusal_names_the_problem_and_a_store_failure_keeps_its_cause() {
        let tool = CronCreateTool {
            store: Arc::new(Memory::default()),
        };
        let error = tool
            .call(&mut as_agent(ROOT_AGENT), create_args("bad"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("expected 5 fields"), "{error}");

        let tool = CronCreateTool {
            store: Arc::new(Memory {
                fail: true,
                ..Memory::default()
            }),
        };
        let error = tool
            .call(&mut as_agent(ROOT_AGENT), create_args("* * * * *"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("/s/schedule.json"), "{error}");
        assert!(error.contains("disk full"), "{error}");
    }

    #[tokio::test]
    async fn jobs_are_listed_one_per_line_with_a_truncated_prompt() {
        let store = Arc::new(Memory::default());
        let tool = CronJobsTool {
            store: store.clone(),
        };
        let empty = tool
            .call(&mut as_agent(ROOT_AGENT), NoArgs {})
            .await
            .unwrap();
        assert_eq!(empty, "No scheduled tasks.");
        let mut long = info("aaaa0001");
        long.prompt = "word ".repeat(60);
        store.jobs.lock().unwrap().extend([long, info("bbbb0002")]);
        let text = tool
            .call(&mut as_agent(ROOT_AGENT), NoArgs {})
            .await
            .unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].starts_with("aaaa0001 (every 5 minutes, set by the model)"),
            "{text}"
        );
        assert!(lines[0].ends_with("..."), "{text}");
        assert!(lines[0].chars().count() < 200);
    }

    #[tokio::test]
    async fn deleting_an_unknown_id_is_an_error_naming_it() {
        let store = Arc::new(Memory::default());
        store.jobs.lock().unwrap().push(info("aaaa0001"));
        let tool = CronDeleteTool { store };
        let ok = tool
            .call(
                &mut as_agent(ROOT_AGENT),
                CronDeleteArgs {
                    id: "aaaa0001".into(),
                },
            )
            .await
            .unwrap();
        assert!(ok.contains("aaaa0001"));
        let error = tool
            .call(
                &mut as_agent(ROOT_AGENT),
                CronDeleteArgs { id: "zzzz".into() },
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("zzzz"), "{error}");
    }

    fn wake(seconds: Option<u64>, reason: Option<&str>, stop: bool) -> WakeupArgs {
        WakeupArgs {
            delay_seconds: seconds,
            reason: reason.map(str::to_string),
            stop,
        }
    }

    #[tokio::test]
    async fn wakeup_validates_its_arguments_before_reaching_the_store() {
        let store = Arc::new(Memory::default());
        let tool = ScheduleWakeupTool {
            store: store.clone(),
        };
        for bad in [
            wake(None, None, false),
            wake(Some(59), Some("r"), false),
            wake(Some(3601), Some("r"), false),
            wake(Some(120), None, false),
            wake(Some(120), Some("  "), false),
            wake(Some(120), Some(&"x".repeat(201)), false),
            wake(Some(120), Some("r"), true),
        ] {
            assert!(tool.call(&mut as_agent(ROOT_AGENT), bad).await.is_err());
        }
        assert!(store.wakes.lock().unwrap().is_empty());
        let text = tool
            .call(
                &mut as_agent(ROOT_AGENT),
                wake(Some(60), Some("waiting on CI"), false),
            )
            .await
            .unwrap();
        assert!(text.contains("waiting on CI"), "{text}");
        let stopped = tool
            .call(&mut as_agent(ROOT_AGENT), wake(None, None, true))
            .await
            .unwrap();
        assert_eq!(stopped, "The loop is stopped.");
        assert_eq!(store.wakes.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn wakeup_without_an_active_loop_is_an_error() {
        let tool = ScheduleWakeupTool {
            store: Arc::new(Memory {
                no_loop: true,
                ..Memory::default()
            }),
        };
        let error = tool
            .call(&mut as_agent(ROOT_AGENT), wake(Some(60), Some("r"), false))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("/loop"), "{error}");
    }

    #[test]
    fn the_persist_error_keeps_its_cause() {
        use std::error::Error;
        let error = ScheduleError::Persist {
            path: PathBuf::from("/s/schedule.json"),
            source: io::Error::other("disk full"),
        };
        assert_eq!(error.source().unwrap().to_string(), "disk full");
        assert!(!error.to_string().contains("disk full"));
    }
}
