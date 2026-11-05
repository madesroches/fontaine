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

use super::cli::{ChildFiles, Cli, CliChild, ReadFail, Scratch, fixed_args, scratch_error};
use super::inventory::INVENTORY_FLAG;
use super::protocol::{Event, Expect, Failure, Reader, SessionSeed, exit_failure, translate};
use super::{ClaudeError, ErrorKind, USAGE_SETTINGS_URL, read_idle_error};

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
            program: std::env::current_exe()?,
            leading: vec![OsString::from(INVENTORY_FLAG)],
        })
    }

    pub fn new(program: PathBuf, leading: Vec<OsString>) -> Self {
        InventoryCommand { program, leading }
    }
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
}

impl ClaudeTransport {
    pub fn new(cli: Arc<Cli>, selection: Selection, inventory: InventoryCommand) -> Self {
        ClaudeTransport {
            cli,
            selection: Arc::new(selection),
            inventory,
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

        self.handshake().await.map_err(provider)?;

        let scratch = Scratch::new().map_err(scratch_failed)?;
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
        let mut reader = Reader::new(Expect {
            models: vec![selection.canonical.clone(), selection.alias.clone()],
            aliases: translated.aliases.clone(),
        });

        for frame in &translated.frames {
            // Only a history user frame is acknowledged: the CLI answers it with a zero-turn
            // result before the replay continues. History the assistant said needs no answer.
            let acked =
                !frame.query && frame.value.get("type").and_then(|t| t.as_str()) == Some("user");
            if frame.query {
                reader.expect_response();
            } else if acked {
                reader.expect_ack();
            }
            if let Err(error) = child.write_line(&frame.value, idle).await {
                // A CLI that exited early closes its stdin; classify the exit, not the write.
                if error.kind == ErrorKind::Protocol && child.exit_status().await.is_some() {
                    child.settle_stderr().await;
                    return Err(exit_failure(&child.stderr_text(), reader.native()));
                }
                return Err(provider(error));
            }
            if frame.query {
                break;
            }
            if acked {
                loop {
                    let line = next(&mut child, &reader, idle).await?;
                    match reader.accept(&line)? {
                        Event::Ack => break,
                        Event::Nothing => {}
                        Event::Frame(_) | Event::Complete => {
                            return Err(provider(ClaudeError::new(
                                ErrorKind::Protocol,
                                "the Claude CLI answered a history frame",
                            )));
                        }
                    }
                }
            }
        }
        child.close_stdin();
        Ok(Opened::new(
            stream(Running {
                child: Some(child),
                reader,
                idle,
                pending: VecDeque::new(),
            })
            .boxed(),
        ))
    }

    /// The preflight every request starts with, and the selected model's current row: a model
    /// that now draws from usage credits, or resolves differently, is refused before a turn.
    async fn handshake(&self) -> Result<(), ClaudeError> {
        let selection = &*self.selection;
        let rows = self.cli.discover().await?;
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

    fn write_files(
        &self,
        scratch: &Scratch,
        translated: &super::protocol::Translated,
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

fn scratch_failed(error: std::io::Error) -> Failure {
    provider(scratch_error(error))
}

/// The next stdout line, or the failure that ended the read.
async fn next(child: &mut CliChild, reader: &Reader, idle: Duration) -> Result<Vec<u8>, Failure> {
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
            match running.reader.accept(&line) {
                Ok(Event::Frame(frame)) => return Some((Ok(frame), Some(running))),
                Ok(Event::Nothing | Event::Ack) => {}
                Ok(Event::Complete) => {
                    if let Some(mut child) = running.child.take() {
                        child.stop().await;
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
