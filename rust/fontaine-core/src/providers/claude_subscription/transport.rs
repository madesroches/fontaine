//! The request-scoped transport: rig's Messages wire encodes a request as it always does, and
//! this transport, instead of sending it over HTTP, runs the Claude CLI once for it and hands
//! back the Messages stream events the CLI's output carries.
//!
//! The encoded request's placeholder address and headers are discarded; only its body and its
//! `anthropic-beta` flags are used. Nothing here reads or forwards a credential.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rig_core::driver::{Exchange, Opened, Opening, Transport};
use rig_core::error::ProviderError;
use rig_core::providers::anthropic::Messages;
use rig_core::wire::{Body, Encoded, WireFrame};
use serde_json::json;

use super::cli::{
    ChildFiles, Cli, CliChild, ModelRow, ReadFail, Scratch, fixed_args, initialize_id,
    initialize_request, scratch_error,
};
use super::context::{self, Sent, Verdicts};
use super::inventory::INVENTORY_FLAG;
use super::protocol::{
    Event, Expect, Failure, Frame, Reader, SessionSeed, Translated, exit_failure, translate,
};
use super::usage::Usage;
use super::{ClaudeError, ErrorKind, USAGE_SETTINGS_URL, claude_error, read_idle_error};
use crate::providers::warn::Warn;

/// How the inert inventory is started: this executable in its hidden mode, unless a test
/// supplies another program.
#[derive(Clone, Debug)]
pub struct InventoryCommand {
    program: PathBuf,
    leading: Vec<OsString>,
}

impl InventoryCommand {
    /// This executable with the hidden inventory flag. It must handle the flag, as `fontaine`
    /// and `fontaine-evals` do.
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(InventoryCommand {
            program: running_image()?,
            leading: vec![OsString::from(INVENTORY_FLAG)],
        })
    }

    pub fn new(program: PathBuf, leading: Vec<OsString>) -> Self {
        InventoryCommand { program, leading }
    }
}

/// The executable image this process runs. On Linux, once a rebuild replaces the binary,
/// `current_exe` reads `<path> (deleted)`, a path the CLI cannot spawn, so every inventory started
/// from it fails. `/proc/<pid>/exe` keeps naming the running image, deleted or not, for as long as
/// this process lives, which outlasts every CLI it launches. It is this process's pid rather than
/// `self`, because the CLI resolves the path in its own process.
#[cfg(target_os = "linux")]
fn running_image() -> std::io::Result<PathBuf> {
    let image = PathBuf::from(format!("/proc/{}/exe", std::process::id()));
    if image.exists() {
        return Ok(image);
    }
    std::env::current_exe()
}

#[cfg(not(target_os = "linux"))]
fn running_image() -> std::io::Result<PathBuf> {
    std::env::current_exe()
}

/// Which model one transport serves and whose requests it carries.
#[derive(Clone, Debug)]
pub struct Selection {
    /// The id the user selected.
    pub alias: String,
    /// The id rig and the CLI receive.
    pub canonical: String,
    /// The Fontaine session, for deriving the CLI session id.
    pub session: String,
    /// The caller: the root, the guardian or a subagent.
    pub namespace: String,
    /// Set when the model cannot be used, so every request fails before any process starts.
    pub refused: Option<ClaudeError>,
}

#[derive(Clone)]
pub struct ClaudeTransport {
    cli: Arc<Cli>,
    selection: Arc<Selection>,
    inventory: InventoryCommand,
    usage: Arc<Usage>,
}

impl ClaudeTransport {
    pub fn new(
        cli: Arc<Cli>,
        selection: Selection,
        inventory: InventoryCommand,
        usage: Arc<Usage>,
    ) -> Self {
        ClaudeTransport {
            cli,
            selection: Arc::new(selection),
            inventory,
            usage,
        }
    }
}

impl Transport<Messages> for ClaudeTransport {
    fn send(&self, payload: Encoded, _exchange: Exchange) -> Opening<WireFrame> {
        let this = self.clone();
        Opening::new(async move { this.open(payload).await.map_err(|e| *e) })
    }
}

fn provider(error: ClaudeError) -> Failure {
    error.boxed()
}

impl ClaudeTransport {
    async fn open(&self, payload: Encoded) -> Result<Opened<WireFrame>, Failure> {
        let selection = &*self.selection;
        if let Some(refused) = &selection.refused {
            return Err(provider(refused.clone()));
        }
        let request = payload.request;
        let betas = request
            .headers()
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = match request.into_body() {
            Body::Bytes(bytes) => bytes,
            Body::Multipart(_) => {
                return Err(provider(ClaudeError::new(
                    ErrorKind::Unsupported,
                    "a multipart request cannot be sent through the Claude CLI",
                )));
            }
        };

        let translated = translate(
            &body,
            betas.as_deref(),
            &SessionSeed {
                session: &selection.session,
                namespace: &selection.namespace,
                canonical: &selection.canonical,
            },
        )
        .map_err(provider)?;

        self.cli.check_environment().map_err(provider)?;
        self.cli.check_version().await.map_err(provider)?;

        // The login probe and the request process start together. Nothing that can start an
        // inference is written until both have passed, and a failed login drops the process.
        let login = self.cli.check_login();
        let prepare = self.prepare(translated);
        tokio::pin!(login, prepare);
        let prepared = tokio::select! {
            checked = &mut login => {
                checked.map_err(provider)?;
                prepare.await
            }
            prepared = &mut prepare => {
                login.await.map_err(provider)?;
                prepared
            }
        };
        let (mut child, mut reader, sent, query) = prepared?;

        let idle = self.cli.timeouts.read_idle;
        reader.expect_response();
        write_frame(&mut child, &reader, &query.value, idle).await?;
        child.close_stdin();
        Ok(Opened::new(
            stream(Running {
                child: Some(child),
                reader,
                idle,
                pending: VecDeque::new(),
                sent: Some(sent),
                warn: self.cli.warn().clone(),
                verdicts: self.cli.verdicts().clone(),
            })
            .boxed(),
        ))
    }

    /// Starts the request's process, has it list its models, checks the selected row and replays
    /// every history frame. The final frame, the only one that asks for a response, is left
    /// unwritten. The cache layout the process's own `initialize` answer selects is applied to
    /// the frames in place.
    async fn prepare(
        &self,
        mut translated: Translated,
    ) -> Result<(CliChild, Reader, Sent, Frame), Failure> {
        let selection = &*self.selection;
        let scratch = Scratch::new(self.cli.warn().clone()).map_err(scratch_failed)?;
        let files = self
            .write_files(&scratch, &translated)
            .map_err(scratch_failed)?;
        let args = fixed_args(
            &files,
            Some(&selection.canonical),
            Some(&translated.session_id),
            translated.effort.as_deref(),
        );
        let idle = self.cli.timeouts.read_idle;
        let mut child = self.cli.launch(scratch, args, true).map_err(provider)?;
        let mut reader = Reader::new(
            Expect {
                models: vec![selection.canonical.clone(), selection.alias.clone()],
                aliases: translated.aliases.clone(),
                web_search: translated.web_search,
            },
            Some(self.usage.clone()),
        );

        // The model's current row: one that now draws from usage credits, or resolves
        // differently, is refused before a turn.
        let request_id = initialize_id();
        write_frame(&mut child, &reader, &initialize_request(&request_id), idle).await?;
        let listing = child.list_models(&request_id, idle, Some(&mut reader));
        let listing = match tokio::time::timeout(self.cli.timeouts.discovery, listing).await {
            Ok(listing) => listing?,
            Err(_) => {
                return Err(provider(ClaudeError::new(
                    ErrorKind::SetupTimeout,
                    "the Claude CLI did not list its models in time",
                )));
            }
        };
        check_row(&listing.rows, selection).map_err(provider)?;

        let prints = context::fingerprints(&translated.frames);
        let layout = context::layout(&listing.context, self.cli.verdicts());
        context::apply(&mut translated.frames, &listing.context, layout);
        let sent = Sent::new(
            &translated.session_id,
            listing.context.version,
            layout,
            prints,
            &translated.frames,
        )
        .with_settings(context::settings_print(
            translated.effort.as_deref(),
            &translated.settings,
        ));

        let query = translated
            .frames
            .pop()
            .expect("a translated request has a final frame");
        for frame in &translated.frames {
            // Only a history user frame is acknowledged: the CLI answers it with a zero-turn
            // result before the replay continues. History the assistant said needs no answer.
            let acked = frame.value.get("type").and_then(|t| t.as_str()) == Some("user");
            if acked {
                reader.expect_ack();
            }
            write_frame(&mut child, &reader, &frame.value, idle).await?;
            if acked {
                loop {
                    let line = next(&mut child, &reader, idle).await?;
                    let event = reader
                        .accept(&line)
                        .inspect_err(|e| rejected(&line, &reader, e))?;
                    match event {
                        Event::Ack => break,
                        Event::Nothing => {}
                        Event::Frame(_) | Event::Complete | Event::Cut => {
                            return Err(provider(ClaudeError::new(
                                ErrorKind::Protocol,
                                "the Claude CLI answered a history frame",
                            )));
                        }
                    }
                }
            }
        }
        Ok((child, reader, sent, query))
    }

    fn write_files(
        &self,
        scratch: &Scratch,
        translated: &Translated,
    ) -> std::io::Result<ChildFiles> {
        let manifest = scratch.write("tools.json", &serde_json::to_vec(&translated.manifest)?)?;
        // JSON carries text, so a path that is not Unicode is carried lossily; the inventory then
        // fails to start and the setup check refuses the run.
        let mut command: Vec<String> = self
            .inventory
            .leading
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        command.push(manifest.to_string_lossy().into_owned());
        let mcp = json!({
            "mcpServers": {
                "fontaine": {
                    "command": self.inventory.program.to_string_lossy(),
                    "args": command,
                    "env": {},
                }
            }
        });
        Ok(ChildFiles {
            settings: scratch.write("settings.json", &serde_json::to_vec(&translated.settings)?)?,
            mcp_config: scratch.write("mcp.json", &serde_json::to_vec(&mcp)?)?,
            system_prompt: scratch.write("system.txt", translated.system.as_bytes())?,
        })
    }
}

/// The selected model's row must be listed, not credit-billed, and resolve as it did when it
/// was listed.
fn check_row(rows: &[ModelRow], selection: &Selection) -> Result<(), ClaudeError> {
    let row = rows
        .iter()
        .find(|row| row.id == selection.alias)
        .ok_or_else(|| {
            ClaudeError::new(
                ErrorKind::Catalog,
                format!(
                    "the Claude CLI no longer offers model `{}`",
                    selection.alias
                ),
            )
        })?;
    if row.credits {
        return Err(ClaudeError::new(
            ErrorKind::UsageCredits,
            format!(
                "the Claude CLI now marks model `{}` as drawing from paid usage credits, which this adapter never selects; see {USAGE_SETTINGS_URL}",
                row.id
            ),
        ));
    }
    if row.canonical != selection.canonical {
        return Err(ClaudeError::new(
            ErrorKind::Catalog,
            format!(
                "model `{}` now resolves to a different model than when it was listed; select it again",
                row.id
            ),
        ));
    }
    Ok(())
}

/// Writes one frame. A CLI that exited early closes its stdin, so a failed write is classified
/// by the exit rather than by the write.
async fn write_frame(
    child: &mut CliChild,
    reader: &Reader,
    value: &serde_json::Value,
    idle: Duration,
) -> Result<(), Failure> {
    if let Err(error) = child.write_line(value, idle).await {
        if error.kind == ErrorKind::Protocol && child.exit_status().await.is_some() {
            child.settle_stderr().await;
            return Err(exit_failure(&child.stderr_text(), reader.native()));
        }
        return Err(provider(error));
    }
    Ok(())
}

fn scratch_failed(error: std::io::Error) -> Failure {
    provider(scratch_error(error))
}

/// The next stdout line, or the failure that ended the read.
pub(super) async fn next(
    child: &mut CliChild,
    reader: &Reader,
    idle: Duration,
) -> Result<Vec<u8>, Failure> {
    match child.next_line(idle).await {
        Ok(Some(line)) => Ok(line),
        Ok(None) => {
            // Let the exit be observed so stderr is complete before it is classified.
            child.exit_status().await;
            child.settle_stderr().await;
            Err(exit_failure(&child.stderr_text(), reader.native()))
        }
        Err(ReadFail::Idle) => Err(Box::new(read_idle_error(idle))),
        Err(ReadFail::TooLong | ReadFail::Io) => Err(provider(ClaudeError::new(
            ErrorKind::Protocol,
            "the Claude CLI's output could not be read",
        ))),
    }
}

struct Running {
    child: Option<CliChild>,
    reader: Reader,
    idle: Duration,
    pending: VecDeque<WireFrame>,
    /// The request as sent, until its result is recorded.
    sent: Option<Sent>,
    warn: Warn,
    verdicts: Arc<Verdicts>,
}

/// The most of a rejected line the debug file keeps.
const LOGGED_LINE_BYTES: usize = 4096;

/// Records a protocol rejection for the owner-only debug file: the failure, the events accepted
/// before it and the line itself. A module-target event never reaches an export backend.
pub(super) fn rejected(line: &[u8], reader: &Reader, error: &ProviderError) {
    if !claude_error(error).is_some_and(|e| e.kind == ErrorKind::Protocol) {
        return;
    }
    let mut shown = String::from_utf8_lossy(line).into_owned();
    if shown.len() > LOGGED_LINE_BYTES {
        let mut end = LOGGED_LINE_BYTES;
        while !shown.is_char_boundary(end) {
            end -= 1;
        }
        shown.truncate(end);
        shown.push_str(&format!("…({} bytes)", line.len()));
    }
    tracing::debug!(
        error = %fontaine_tools::error_chain::chain(error),
        trail = %reader.trail(),
        line = %shown,
        "the Claude CLI sent an event this adapter rejected"
    );
}

/// The reply as a stream of Messages events. The terminal delta is held back until the response
/// and the final result are validated and the process is stopped, because rig ends its read at
/// that delta.
fn stream(
    running: Running,
) -> impl futures::Stream<Item = Result<WireFrame, ProviderError>> + Send + 'static {
    futures::stream::unfold(Some(running), |state| async move {
        let mut running = state?;
        loop {
            if let Some(frame) = running.pending.pop_front() {
                return Some((Ok(frame), Some(running)));
            }
            let child = running.child.as_mut()?;
            let line = match next(child, &running.reader, running.idle).await {
                Ok(line) => line,
                Err(error) => {
                    if let Some(mut child) = running.child.take() {
                        child.stop().await;
                    }
                    return Some((Err(*error), None));
                }
            };
            let accepted = running.reader.accept(&line);
            if let Err(error) = &accepted {
                rejected(&line, &running.reader, error);
            }
            match accepted {
                Ok(Event::Frame(frame)) => return Some((Ok(frame), Some(running))),
                Ok(Event::Nothing | Event::Ack) => {}
                Ok(event @ (Event::Complete | Event::Cut)) => {
                    if let Some(mut child) = running.child.take() {
                        child.stop().await;
                    }
                    // A cut response has no result to cross-check, so the cache self-check
                    // drops its record instead of reading a missing result as evidence.
                    let cache_usage = match event {
                        Event::Complete => running.reader.cache_usage(),
                        _ => None,
                    };
                    if let Some(sent) = running.sent.take()
                        && let Some(version) = context::finished(
                            sent,
                            cache_usage,
                            std::time::Instant::now(),
                            &running.verdicts,
                        )
                    {
                        running.warn.emit(&format!(
                            "the Claude CLI's replayed context was not read back from the \
                             prompt cache; the cache optimization is off for CLI version \
                             {}.{}.{} and the verdict is remembered across restarts (`cache-bench` measures it)",
                            version.0, version.1, version.2
                        ));
                    }
                    running.pending.extend(running.reader.release());
                    if running.pending.is_empty() {
                        return None;
                    }
                }
                Err(error) => {
                    if let Some(mut child) = running.child.take() {
                        child.stop().await;
                    }
                    return Some((Err(*error), None));
                }
            }
        }
    })
}

#[cfg(test)]
mod row_tests {
    use super::*;

    fn selection() -> Selection {
        Selection {
            alias: "sonnet".to_string(),
            canonical: "claude-sonnet-5-5".to_string(),
            session: "s".to_string(),
            namespace: "root".to_string(),
            refused: None,
        }
    }

    fn row(id: &str, canonical: &str, credits: bool) -> ModelRow {
        ModelRow {
            id: id.to_string(),
            canonical: canonical.to_string(),
            credits,
            capabilities: Default::default(),
        }
    }

    #[test]
    fn a_matching_row_passes() {
        let rows = [
            row("haiku", "claude-haiku-4-5", false),
            row("sonnet", "claude-sonnet-5-5", false),
        ];
        assert!(check_row(&rows, &selection()).is_ok());
    }

    #[test]
    fn a_missing_credit_billed_or_changed_row_is_refused() {
        let missing = check_row(&[row("opus", "claude-opus-5", false)], &selection()).unwrap_err();
        assert_eq!(missing.kind, ErrorKind::Catalog);
        assert_eq!(
            missing.to_string(),
            "the Claude CLI no longer offers model `sonnet`"
        );

        let credits =
            check_row(&[row("sonnet", "claude-sonnet-5-5", true)], &selection()).unwrap_err();
        assert_eq!(credits.kind, ErrorKind::UsageCredits);
        assert!(credits.to_string().contains("paid usage credits"));
        assert!(credits.to_string().contains(USAGE_SETTINGS_URL));

        let changed =
            check_row(&[row("sonnet", "claude-sonnet-6", false)], &selection()).unwrap_err();
        assert_eq!(changed.kind, ErrorKind::Catalog);
        assert_eq!(
            changed.to_string(),
            "model `sonnet` now resolves to a different model than when it was listed; select it again"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_inventory_runs_the_image_of_this_process_not_its_path() {
        let command = InventoryCommand::current_exe().expect("this executable");
        assert_eq!(
            command.program,
            PathBuf::from(format!("/proc/{}/exe", std::process::id()))
        );
        assert_eq!(
            std::fs::canonicalize(&command.program).expect("the image resolves"),
            std::fs::canonicalize(std::env::current_exe().expect("own path")).expect("own path")
        );
        assert_eq!(command.leading, vec![OsString::from(INVENTORY_FLAG)]);

        let status = std::process::Command::new(&command.program)
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .status()
            .expect("the image starts from its /proc path");
        assert!(status.success());
    }
}
