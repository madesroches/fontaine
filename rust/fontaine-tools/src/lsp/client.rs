//! One connection to a language server: spawn, handshake, requests with a timeout, and the
//! shutdown sequence. The framing and request ids are `async-lsp`'s.

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_lsp::router::Router;
use async_lsp::{MainLoop, ResponseError, ServerSocket};
use lsp_types::notification::{
    DidChangeConfiguration, DidChangeWorkspaceFolders, Exit, Initialized, Notification,
    PublishDiagnostics,
};
use lsp_types::request::{
    Initialize, RegisterCapability, Request, Shutdown, UnregisterCapability,
    WorkDoneProgressCreate, WorkspaceConfiguration,
};
use lsp_types::{
    ClientCapabilities, ClientInfo, DidChangeConfigurationParams, DidChangeWorkspaceFoldersParams,
    GeneralClientCapabilities, InitializeParams, InitializedParams, Position, PositionEncodingKind,
    PublishDiagnosticsClientCapabilities, TextDocumentClientCapabilities,
    TextDocumentSyncClientCapabilities, Url, WindowClientCapabilities, WorkspaceClientCapabilities,
    WorkspaceFolder, WorkspaceFoldersChangeEvent,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::diagnostics::DiagnosticStore;
use crate::error_chain::chain;
use crate::lock::lock_recovering;
use crate::procgroup;

/// How the server counts the characters of a line, negotiated at `initialize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}

impl Encoding {
    fn units(self, c: char) -> u32 {
        (match self {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
            Encoding::Utf32 => 1,
        }) as u32
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PositionError {
    Line {
        line: u32,
        lines: usize,
    },
    Character {
        line: u32,
        character: u32,
        width: usize,
    },
}

impl std::fmt::Display for PositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PositionError::Line { line, lines } => {
                write!(f, "line {line} is past the end of the file ({lines} lines)")
            }
            PositionError::Character {
                line,
                character,
                width,
            } => write!(
                f,
                "character {character} is past the end of line {line} ({width} characters)"
            ),
        }
    }
}

impl std::error::Error for PositionError {}

fn line_of(text: &str, index: usize) -> Option<&str> {
    text.split('\n')
        .nth(index)
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// A 1-based line and character (Unicode scalar values, as an editor shows them) as the
/// server's 0-based position.
pub fn to_lsp_position(
    encoding: Encoding,
    text: &str,
    line: u32,
    character: u32,
) -> Result<Position, PositionError> {
    let lines = text.split('\n').count();
    let line_text = line
        .checked_sub(1)
        .and_then(|index| line_of(text, index as usize))
        .ok_or(PositionError::Line { line, lines })?;
    let width = line_text.chars().count();
    let before = character.checked_sub(1).filter(|c| *c as usize <= width);
    let before = before.ok_or(PositionError::Character {
        line,
        character,
        width,
    })?;
    let units = line_text
        .chars()
        .take(before as usize)
        .map(|c| encoding.units(c))
        .sum();
    Ok(Position::new(line - 1, units))
}

/// The server's position as a 1-based line and character. Without the line's text the
/// character is taken as is.
pub fn from_lsp_position(
    encoding: Encoding,
    line_text: Option<&str>,
    position: Position,
) -> (u32, u32) {
    let column = match line_text {
        None => position.character,
        Some(text) => {
            let mut units = 0;
            let mut scalars = 0;
            for c in text.chars() {
                if units >= position.character {
                    break;
                }
                units += encoding.units(c);
                scalars += 1;
            }
            // A position past the end of the line counts the characters it does not have.
            scalars + position.character.saturating_sub(units)
        }
    };
    (position.line + 1, column + 1)
}

/// The text of line `index` (0-based) of `text`, for [`from_lsp_position`].
pub fn line_text(text: &str, index: u32) -> Option<&str> {
    line_of(text, index as usize)
}

#[derive(Clone, Debug)]
pub struct StartConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub initialization_options: Option<Value>,
    pub settings: Option<Value>,
    /// The workspace folders, primary root first.
    pub folders: Vec<PathBuf>,
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
}

/// Why a server did not reach `Running`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartFailure {
    pub detail: String,
    /// The last line the server wrote to stderr, if any.
    pub stderr: Option<String>,
}

#[derive(Debug)]
pub enum RequestError {
    Timeout(Duration),
    /// The connection is gone; the server exited or closed its pipes.
    Stopped,
    Server(String),
}

impl From<async_lsp::Error> for RequestError {
    fn from(error: async_lsp::Error) -> Self {
        match error {
            async_lsp::Error::Response(response) => RequestError::Server(response.message),
            async_lsp::Error::ServiceStopped | async_lsp::Error::Io(_) => RequestError::Stopped,
            other => RequestError::Server(other.to_string()),
        }
    }
}

/// The server's child process. Dropping it stops the process group without blocking.
struct ServerProcess {
    child: Mutex<Option<Child>>,
    pid: Option<u32>,
}

impl ServerProcess {
    fn exit_status(&self) -> Option<ExitStatus> {
        // Advisory: a half-updated child slot only delays noticing an exit.
        lock_recovering(&self.child)
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
    }

    fn take(&self) -> Option<Child> {
        lock_recovering(&self.child).take()
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if let Some(child) = lock_recovering(&self.child).take() {
            procgroup::terminate(self.pid);
            procgroup::detached_settle(child, self.pid, Duration::from_secs(2));
        }
    }
}

struct ClientState {
    server: Arc<str>,
    store: Arc<DiagnosticStore>,
    settings: Option<Value>,
    folders: Vec<WorkspaceFolder>,
}

pub struct Client {
    name: String,
    socket: ServerSocket,
    process: ServerProcess,
    pub encoding: Encoding,
    pub folders_supported: bool,
    done: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<String>>,
    request_timeout: Duration,
    shutdown_timeout: Duration,
}

fn workspace_folder(path: &Path) -> Option<WorkspaceFolder> {
    let uri = Url::from_directory_path(path).ok()?;
    let name = path
        .file_name()
        .map_or_else(|| "workspace".into(), |n| n.to_string_lossy().into_owned());
    Some(WorkspaceFolder { uri, name })
}

fn section_of(settings: &Value, section: Option<&str>) -> Value {
    let Some(section) = section else {
        return settings.clone();
    };
    section
        .split('.')
        .try_fold(settings, |value, key| value.get(key))
        .cloned()
        .unwrap_or(Value::Null)
}

fn router(state: ClientState) -> Router<ClientState> {
    let mut router = Router::new(state);
    router
        .notification::<PublishDiagnostics>(|state, params| {
            state.store.publish(
                &state.server,
                params.uri,
                params.version,
                params.diagnostics,
            );
            ControlFlow::Continue(())
        })
        // Log, progress and status notifications carry nothing the tool reports, and the
        // default for an unknown one ends the connection.
        .unhandled_notification(|_, _| ControlFlow::Continue(()))
        .request::<WorkspaceConfiguration, _>(|state, params| {
            let answer = params
                .items
                .iter()
                .map(|item| match &state.settings {
                    Some(settings) => section_of(settings, item.section.as_deref()),
                    None => Value::Null,
                })
                .collect::<Vec<_>>();
            std::future::ready(Ok::<_, ResponseError>(answer))
        })
        .request::<RegisterCapability, _>(|_, _| std::future::ready(Ok::<_, ResponseError>(())))
        .request::<UnregisterCapability, _>(|_, _| std::future::ready(Ok::<_, ResponseError>(())))
        .request::<WorkDoneProgressCreate, _>(|_, _| std::future::ready(Ok::<_, ResponseError>(())))
        .request::<lsp_types::request::WorkspaceFoldersRequest, _>(|state, ()| {
            std::future::ready(Ok::<_, ResponseError>(Some(state.folders.clone())))
        });
    router
}

fn client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![
                PositionEncodingKind::UTF32,
                PositionEncodingKind::UTF16,
            ]),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            workspace_folders: Some(true),
            configuration: Some(true),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities {
                did_save: Some(true),
                ..Default::default()
            }),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                version_support: Some(true),
                ..Default::default()
            }),
            document_symbol: Some(lsp_types::DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn describe_exit(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("signal {signal}");
        }
    }
    "an unknown exit status".to_string()
}

fn last_line(text: &str) -> Option<String> {
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    Some(line.chars().take(200).collect())
}

impl Client {
    /// Spawns the server and completes the handshake within `startup_timeout`. A server that
    /// exits, errors or stays silent is killed and reported.
    pub async fn start(
        config: StartConfig,
        store: Arc<DiagnosticStore>,
    ) -> Result<Client, StartFailure> {
        let mut command = tokio::process::Command::new(&config.command);
        command
            .args(&config.args)
            .current_dir(&config.cwd)
            .env_clear()
            .envs(config.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Its own group, so the signals reach what a wrapper script started too.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|e| StartFailure {
            detail: format!("cannot start `{}`: {}", config.command, chain(&e)),
            stderr: None,
        })?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("the stdin pipe was requested");
        let stdout = child.stdout.take().expect("the stdout pipe was requested");
        let stderr = child.stderr.take().expect("the stderr pipe was requested");
        let process = ServerProcess {
            child: Mutex::new(Some(child)),
            pid,
        };

        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let drain = {
            let (tail, name) = (stderr_tail.clone(), config.name.clone());
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(server = %name, "lsp stderr: {line}");
                    // Advisory: the tail only decorates an error message.
                    *lock_recovering(&tail) = line;
                }
            })
        };

        let folders: Vec<WorkspaceFolder> = config
            .folders
            .iter()
            .filter_map(|p| workspace_folder(p))
            .collect();
        let state = ClientState {
            server: config.name.as_str().into(),
            store,
            settings: config.settings.clone(),
            folders: folders.clone(),
        };
        let (mainloop, socket) = MainLoop::new_client(|_server| router(state));
        let done = Arc::new(AtomicBool::new(false));
        {
            let done = done.clone();
            let name = config.name.clone();
            tokio::spawn(async move {
                if let Err(e) = mainloop
                    .run_buffered(stdout.compat(), stdin.compat_write())
                    .await
                {
                    tracing::debug!(server = %name, "lsp connection ended: {}", chain(&e));
                }
                done.store(true, Ordering::SeqCst);
            });
        }

        let mut client = Client {
            name: config.name.clone(),
            socket,
            process,
            encoding: Encoding::Utf16,
            folders_supported: false,
            done,
            stderr_tail,
            request_timeout: config.request_timeout,
            shutdown_timeout: config.shutdown_timeout,
        };
        let handshake =
            tokio::time::timeout(config.startup_timeout, client.handshake(&config, folders));
        let failure = match handshake.await {
            Ok(Ok(())) => return Ok(client),
            Ok(Err(RequestError::Server(message))) => message,
            Ok(Err(RequestError::Timeout(_))) | Err(_) => format!(
                "no answer within {} seconds",
                config.startup_timeout.as_secs_f64()
            ),
            Ok(Err(RequestError::Stopped)) => {
                // The pipes closed: the exit status is the reason, once the OS has it.
                match client.wait_exit(Duration::from_secs(2)).await {
                    Some(status) => describe_exit(status),
                    None => "the connection closed".to_string(),
                }
            }
        };
        client.kill().await;
        // Let the drain task read the last bytes the dying server wrote.
        let _ = tokio::time::timeout(Duration::from_millis(300), drain).await;
        Err(StartFailure {
            detail: failure,
            stderr: last_line(&lock_recovering(&client.stderr_tail)),
        })
    }

    async fn handshake(
        &mut self,
        config: &StartConfig,
        folders: Vec<WorkspaceFolder>,
    ) -> Result<(), RequestError> {
        let root = folders.first().map(|f| f.uri.clone());
        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: root,
            initialization_options: config.initialization_options.clone(),
            capabilities: client_capabilities(),
            workspace_folders: Some(folders),
            client_info: Some(ClientInfo {
                name: "fontaine".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            ..Default::default()
        };
        let result = self.socket.request::<Initialize>(params).await?;
        self.encoding = match result.capabilities.position_encoding.as_ref() {
            Some(kind) if *kind == PositionEncodingKind::UTF32 => Encoding::Utf32,
            Some(kind) if *kind == PositionEncodingKind::UTF8 => Encoding::Utf8,
            _ => Encoding::Utf16,
        };
        self.folders_supported = result
            .capabilities
            .workspace
            .and_then(|w| w.workspace_folders)
            .and_then(|f| f.supported)
            .unwrap_or(false);
        self.socket.notify::<Initialized>(InitializedParams {})?;
        if let Some(settings) = &config.settings {
            self.socket
                .notify::<DidChangeConfiguration>(DidChangeConfigurationParams {
                    settings: settings.clone(),
                })?;
        }
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the connection is up and the process has not exited.
    pub fn is_alive(&self) -> bool {
        !self.done.load(Ordering::SeqCst) && self.process.exit_status().is_none()
    }

    /// The last line the server wrote to stderr.
    pub fn stderr_line(&self) -> Option<String> {
        last_line(&lock_recovering(&self.stderr_tail))
    }

    async fn wait_exit(&self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.process.exit_status() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Why the process is gone, for a crash report.
    pub async fn exit_detail(&self) -> String {
        match self.wait_exit(Duration::from_secs(1)).await {
            Some(status) => describe_exit(status),
            None => "the connection closed".to_string(),
        }
    }

    pub async fn request<R: Request>(&self, params: R::Params) -> Result<R::Result, RequestError>
    where
        R::Params: Send,
    {
        match tokio::time::timeout(self.request_timeout, self.socket.request::<R>(params)).await {
            Ok(result) => result.map_err(RequestError::from),
            Err(_) => Err(RequestError::Timeout(self.request_timeout)),
        }
    }

    pub fn notify<N: Notification>(&self, params: N::Params) -> Result<(), RequestError> {
        self.socket.notify::<N>(params).map_err(RequestError::from)
    }

    pub fn folders_added(&self, root: &Path) {
        if !self.folders_supported {
            return;
        }
        let Some(folder) = workspace_folder(root) else {
            return;
        };
        let event = WorkspaceFoldersChangeEvent {
            added: vec![folder],
            removed: Vec::new(),
        };
        if let Err(e) =
            self.notify::<DidChangeWorkspaceFolders>(DidChangeWorkspaceFoldersParams { event })
        {
            tracing::debug!(server = %self.name, "lsp workspace folders not sent: {e:?}");
        }
    }

    /// `shutdown` and `exit`, then waits out the rest of `shutdown_timeout` for the process to
    /// leave before killing it.
    pub async fn shutdown(&self) {
        let started = Instant::now();
        let _ =
            tokio::time::timeout(self.shutdown_timeout, self.socket.request::<Shutdown>(())).await;
        let _ = self.socket.notify::<Exit>(());
        let grace = self.shutdown_timeout.saturating_sub(started.elapsed());
        if let Some(mut child) = self.process.take() {
            procgroup::settle(&mut child, self.process.pid, grace).await;
        }
    }

    async fn kill(&mut self) {
        if let Some(mut child) = self.process.take() {
            procgroup::terminate(self.process.pid);
            procgroup::settle(&mut child, self.process.pid, Duration::from_millis(200)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "fn a() {}\nlet 😀 = é;\r\nlast";

    #[test]
    fn ascii_positions_shift_by_one_in_both_directions() {
        for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            let position = to_lsp_position(encoding, TEXT, 1, 4).unwrap();
            assert_eq!(position, Position::new(0, 3));
            assert_eq!(
                from_lsp_position(encoding, line_text(TEXT, 0), position),
                (1, 4)
            );
        }
    }

    #[test]
    fn multi_byte_characters_count_by_the_negotiated_unit() {
        // Line 2 is `let 😀 = é;`: the `=` is the 7th character.
        let at = |encoding| to_lsp_position(encoding, TEXT, 2, 7).unwrap();
        assert_eq!(at(Encoding::Utf32), Position::new(1, 6));
        assert_eq!(at(Encoding::Utf16), Position::new(1, 7));
        assert_eq!(at(Encoding::Utf8), Position::new(1, 9));
        for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            assert_eq!(
                from_lsp_position(encoding, line_text(TEXT, 1), at(encoding)),
                (2, 7),
                "{encoding:?}"
            );
        }
        // After `é`: 2-byte in UTF-8, one unit in UTF-16.
        assert_eq!(
            to_lsp_position(Encoding::Utf8, TEXT, 2, 11).unwrap(),
            Position::new(1, 14)
        );
    }

    #[test]
    fn the_end_of_a_line_and_the_empty_last_line_are_addressable() {
        // `last` has 4 characters; character 5 is just past the end.
        assert_eq!(
            to_lsp_position(Encoding::Utf16, TEXT, 3, 5).unwrap(),
            Position::new(2, 4)
        );
        assert_eq!(
            to_lsp_position(Encoding::Utf16, "a\n", 2, 1).unwrap(),
            Position::new(1, 0)
        );
        // A carriage return is not part of the line.
        assert_eq!(
            to_lsp_position(Encoding::Utf16, TEXT, 2, 11).unwrap(),
            Position::new(1, 11)
        );
        assert!(to_lsp_position(Encoding::Utf16, TEXT, 2, 12).is_err());
    }

    #[test]
    fn out_of_range_lines_and_characters_are_errors() {
        assert_eq!(
            to_lsp_position(Encoding::Utf32, TEXT, 4, 1),
            Err(PositionError::Line { line: 4, lines: 3 })
        );
        assert!(matches!(
            to_lsp_position(Encoding::Utf32, TEXT, 0, 1),
            Err(PositionError::Line { .. })
        ));
        assert!(matches!(
            to_lsp_position(Encoding::Utf32, TEXT, 1, 0),
            Err(PositionError::Character { .. })
        ));
        assert_eq!(
            to_lsp_position(Encoding::Utf32, TEXT, 1, 11),
            Err(PositionError::Character {
                line: 1,
                character: 11,
                width: 9
            })
        );
    }

    #[test]
    fn a_position_without_the_line_text_is_taken_as_is() {
        assert_eq!(
            from_lsp_position(Encoding::Utf16, None, Position::new(4, 9)),
            (5, 10)
        );
    }
}
