//! The non-interactive surface, `fontaine -p`. Its output is a versioned contract: see
//! `docs/headless.md` and `docs/schemas/headless-v1.schema.json`. The loop is
//! `fontaine_sdk::run`; this module reads the prompt and writes the events as JSON lines.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use fontaine_sdk::core::afk_report::AfkReport;
use fontaine_sdk::core::agent::AgentRuntime;
use fontaine_sdk::core::providers::ExternalLogin;
use fontaine_sdk::core::turn::SessionEvent;
use fontaine_sdk::core::usage_pause::PauseEvent;
use fontaine_sdk::events::{self, Event, write_stdout_line};
use fontaine_sdk::run::{Cancel, EventSink, InputSource, PauseObserver, ResultInfo, run_session};
pub use fontaine_sdk::run::{InputFormat, Options, OutputFormat};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

fn emit(event: &Event) {
    write_stdout_line(&serde_json::to_string(event).expect("an event serializes"));
}

/// Writes the `headless-v1` schema, byte for byte.
struct HeadlessV1Sink {
    stream: bool,
    include_partial_messages: bool,
}

impl HeadlessV1Sink {
    fn new(options: &Options) -> HeadlessV1Sink {
        HeadlessV1Sink {
            stream: options.output_format == OutputFormat::StreamJson,
            include_partial_messages: options.include_partial_messages,
        }
    }

    fn emit(&self, event: &Event) {
        if self.stream {
            emit(event);
        }
    }
}

impl EventSink for HeadlessV1Sink {
    fn notice(&mut self, message: &str) {
        eprintln!("fontaine: {message}");
    }

    fn session_start(&mut self, runtime: &AgentRuntime, data_dir: &Path) {
        self.emit(&events::init_event(runtime, data_dir));
    }

    fn event(&mut self, session_id: &str, event: &SessionEvent) {
        if let Some(event) = events::session_event(session_id, event, self.include_partial_messages)
        {
            self.emit(&event);
        }
    }

    fn goal_evaluation(
        &mut self,
        session_id: &str,
        judgment: Option<fontaine_sdk::core::goal::Judgment>,
        reason: &str,
        evaluations: u32,
    ) {
        self.emit(&events::goal_event(
            session_id,
            judgment,
            reason,
            evaluations,
        ));
    }

    fn wants_account_usage(&self) -> bool {
        self.stream
    }

    fn result(&mut self, info: &ResultInfo<'_>) {
        self.emit(&events::result_event(
            info.session_id,
            info.outcome,
            info.usage,
            info.goal,
            info.account_usage,
            info.afk,
            info.usage_limit,
        ));
    }

    fn afk_report(&mut self, session_id: &str, report: &AfkReport) {
        self.emit(&events::afk_report_event(session_id, report));
    }

    fn usage_observer(&self, session_id: &str) -> Option<PauseObserver> {
        if !self.stream {
            return None;
        }
        let id = session_id.to_string();
        Some(Arc::new(move |pause: &PauseEvent| {
            emit(&events::usage_event(&id, pause));
        }))
    }
}

enum Input {
    Single(Option<String>),
    Lines(tokio::io::Lines<BufReader<tokio::io::Stdin>>),
}

impl InputSource for Input {
    fn is_exhausted(&self) -> bool {
        matches!(self, Input::Single(None))
    }

    async fn next(&mut self, _runtime: &mut AgentRuntime) -> Result<Option<String>, String> {
        match self {
            Input::Single(prompt) => Ok(prompt.take()),
            Input::Lines(lines) => loop {
                let line = lines
                    .next_line()
                    .await
                    .map_err(|e| format!("cannot read stdin: {e}"))?;
                let Some(line) = line else { return Ok(None) };
                if line.trim().is_empty() {
                    continue;
                }
                return parse_user_line(&line).map(Some);
            },
        }
    }
}

fn parse_user_line(line: &str) -> Result<String, String> {
    let text = user_line_text(line)?;
    if text.trim().is_empty() {
        return Err("input line holds an empty prompt".to_string());
    }
    Ok(text)
}

fn user_line_text(line: &str) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| format!("input line is not JSON: {e}"))?;
    if value.get("type").and_then(Value::as_str) != Some("user") {
        return Err("input line is not a `user` message".to_string());
    }
    let content = value
        .pointer("/message/content")
        .ok_or("input line has no `message.content`")?;
    match content {
        Value::String(text) => Ok(text.clone()),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(t) = block.get("text").and_then(Value::as_str)
                {
                    text.push_str(t);
                }
            }
            Ok(text)
        }
        _ => Err("`message.content` must be a string or an array of text blocks".to_string()),
    }
}

/// How long a prompt argument waits for piped stdin to start before running without it.
const STDIN_FIRST_DATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Reads stdin on a detached thread: a blocking read on a pipe that never closes would
/// otherwise keep the runtime from shutting down.
async fn read_piped_stdin() -> Result<String, String> {
    use std::io::Read;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 8192];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Ok(Vec::new()));
                    return;
                }
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
    let read_error = |e: std::io::Error| format!("cannot read the prompt from stdin: {e}");
    let mut bytes = Vec::new();
    match tokio::time::timeout(STDIN_FIRST_DATA_TIMEOUT, rx.recv()).await {
        Err(_) => {
            eprintln!(
                "Warning: no stdin data received in {}s, proceeding without it. If piping from a slow command, redirect stdin explicitly: < /dev/null to skip, or wait longer.",
                STDIN_FIRST_DATA_TIMEOUT.as_secs()
            );
            return Ok(String::new());
        }
        Ok(None) => return Ok(String::new()),
        Ok(Some(chunk)) => bytes.extend(chunk.map_err(read_error)?),
    }
    while !bytes.is_empty() {
        match rx.recv().await {
            Some(chunk) => {
                let chunk = chunk.map_err(read_error)?;
                if chunk.is_empty() {
                    break;
                }
                bytes.extend(chunk);
            }
            None => break,
        }
    }
    String::from_utf8(bytes).map_err(|e| format!("cannot read the prompt from stdin: {e}"))
}

/// Reads the prompt the run will use, or the stream the prompts come from.
async fn open_input(options: &mut Options) -> Result<Input, String> {
    match options.input_format {
        InputFormat::StreamJson => Ok(Input::Lines(BufReader::new(tokio::io::stdin()).lines())),
        InputFormat::Text => {
            let piped = if options.prompt.is_none() {
                if std::io::stdin().is_terminal() {
                    return Err("no prompt: pass it as an argument or on stdin".to_string());
                }
                let mut text = String::new();
                tokio::io::stdin()
                    .read_to_string(&mut text)
                    .await
                    .map_err(|e| format!("cannot read the prompt from stdin: {e}"))?;
                text
            } else if !std::io::stdin().is_terminal() {
                read_piped_stdin().await?
            } else {
                String::new()
            };
            let prompt = match options.prompt.take() {
                Some(prompt) if piped.trim().is_empty() => prompt,
                Some(prompt) => format!("{prompt}\n{}", piped.trim_end()),
                None => piped,
            };
            if prompt.trim().is_empty() {
                return Err("no prompt: pass it as an argument or on stdin".to_string());
            }
            Ok(Input::Single(Some(prompt)))
        }
    }
}

/// Runs the headless session and returns the process exit code: 0 completed, 1 run error or a
/// `/goal` that did not end met, 2 usage error, 3 an account's usage window stayed spent.
///
/// `guardian_login` is the pinned guardian's account and the login that signs it in, when that
/// account has nobody signed in: a run that consults the guardian then stops before it starts,
/// since every call would be denied.
pub async fn run(
    mut options: Options,
    mut runtime: AgentRuntime,
    data_dir: &Path,
    guardian_login: Option<(String, ExternalLogin)>,
) -> u8 {
    let input = match open_input(&mut options).await {
        Ok(input) => input,
        Err(message) => {
            eprintln!("fontaine: {message}");
            return 2;
        }
    };
    let mut sink = HeadlessV1Sink::new(&options);
    let cancel = Cancel::default();
    let code = run_session(
        options,
        &mut runtime,
        data_dir,
        guardian_login,
        input,
        &mut sink,
        &cancel,
    )
    .await;
    // One exit point, so an input error ends the session for the hooks too.
    runtime.fire_session_end("other").await;
    code
}
