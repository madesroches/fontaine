//! The language servers of a session: which one handles which extension, when each starts, how
//! a crash is handled, and what each one has been told about the files.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, Url, VersionedTextDocumentIdentifier,
};
use rig_core::tool::ToolExecutionError;
use serde_json::Value;
use tokio::sync::Mutex;

use super::client::{Client, Encoding, StartConfig, StartFailure};
use super::diagnostics::{self, Diag, DiagnosticStore};
use crate::child_env::server_env;
use crate::error_chain::chain;
use crate::scope::Scope;

type EnvLookup = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// What the servers need to know about the session, read when they start and when an edit is
/// resolved.
pub trait LspWorkspace: Send + Sync {
    /// The roots the file tools are confined to now.
    fn scope(&self) -> Arc<Scope>;
    /// The roots a server treats as workspace folders, primary first: the scope without the
    /// session's own scratch and memory directories.
    fn folders(&self) -> Vec<PathBuf>;
}

/// One configured server, already merged from user config or a plugin file.
#[derive(Clone, Debug)]
pub struct LspServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env_vars: Vec<String>,
    /// The literal variables set on the child: the `[env]` table with the server's own `env`
    /// laid over it.
    pub set_env: Vec<(OsString, OsString)>,
    /// Extension (with its dot) to language id.
    pub extension_to_language: BTreeMap<String, String>,
    pub initialization_options: Option<Value>,
    pub settings: Option<Value>,
    pub startup_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub request_timeout: Duration,
    pub restart_on_crash: bool,
    pub max_restarts: u32,
    pub diagnostics: bool,
}

#[derive(Clone, Debug)]
enum FailureKind {
    Start,
    Crash,
}

#[derive(Clone, Debug)]
struct Failure {
    kind: FailureKind,
    detail: String,
    stderr: Option<String>,
}

impl Failure {
    fn start(failure: StartFailure) -> Failure {
        Failure {
            kind: FailureKind::Start,
            detail: failure.detail,
            stderr: failure.stderr,
        }
    }

    /// What a tool call that cannot run says.
    fn tool_message(&self, name: &str) -> String {
        match self.kind {
            FailureKind::Start => {
                let stderr = self
                    .stderr
                    .as_deref()
                    .map_or_else(String::new, |line| format!(" (last stderr line: {line})"));
                format!("{name} failed to start: {}{stderr}", self.detail)
            }
            FailureKind::Crash => {
                format!("{name} crashed ({}) and will not be restarted", self.detail)
            }
        }
    }

    /// What the edit's result says in place of diagnostics.
    fn unavailable(&self, name: &str) -> String {
        match self.kind {
            FailureKind::Start => format!("{name} failed to start ({})", self.detail),
            FailureKind::Crash => format!("{name} crashed ({})", self.detail),
        }
    }
}

struct DocState {
    version: i32,
    hash: u64,
}

enum Phase {
    Idle,
    Running(Arc<Client>),
    Failed(Failure),
}

struct SlotState {
    phase: Phase,
    restarts: u32,
    // What the running client has been told, so a new client starts from nothing.
    docs: HashMap<Url, DocState>,
}

struct Slot {
    spec: LspServerSpec,
    state: Mutex<SlotState>,
    encoding: AtomicU8,
}

fn encoding_code(encoding: Encoding) -> u8 {
    match encoding {
        Encoding::Utf8 => 0,
        Encoding::Utf16 => 1,
        Encoding::Utf32 => 2,
    }
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// A file made current on its server, ready to be queried.
pub struct Prepared {
    pub client: Arc<Client>,
    pub server: String,
    pub uri: Url,
    /// The file as the server was last told, for position conversion.
    pub text: String,
}

pub enum EditKind {
    Published {
        uri: Url,
        diags: Vec<Diag>,
        encoding: Encoding,
    },
    NoneYet,
    Unavailable(String),
}

pub struct EditReport {
    pub server: String,
    pub kind: EditKind,
}

pub struct LspServers {
    slots: Vec<Slot>,
    // Lower-cased extension with its dot, to the slot and language id that handle it.
    extensions: BTreeMap<String, (usize, String)>,
    store: Arc<DiagnosticStore>,
    workspace: Arc<dyn LspWorkspace>,
    env: EnvLookup,
}

impl LspServers {
    /// `claims` maps each extension to the name of the server that handles it, already resolved
    /// when several servers name the same one.
    pub fn new(
        specs: Vec<LspServerSpec>,
        claims: &BTreeMap<String, String>,
        workspace: Arc<dyn LspWorkspace>,
        env: EnvLookup,
    ) -> Arc<LspServers> {
        let mut extensions = BTreeMap::new();
        for (extension, name) in claims {
            let Some(index) = specs.iter().position(|s| &s.name == name) else {
                continue;
            };
            let language = specs[index]
                .extension_to_language
                .get(extension)
                .cloned()
                .unwrap_or_default();
            extensions.insert(extension.to_ascii_lowercase(), (index, language));
        }
        let slots = specs
            .into_iter()
            .map(|spec| Slot {
                spec,
                state: Mutex::new(SlotState {
                    phase: Phase::Idle,
                    restarts: 0,
                    docs: HashMap::new(),
                }),
                encoding: AtomicU8::new(encoding_code(Encoding::Utf16)),
            })
            .collect();
        Arc::new(LspServers {
            slots,
            extensions,
            store: Arc::new(DiagnosticStore::default()),
            workspace,
            env,
        })
    }

    /// Whether at least one server handles at least one extension, which is when the `lsp`
    /// tool and the diagnostics stage exist.
    pub fn is_active(&self) -> bool {
        !self.extensions.is_empty()
    }

    pub fn store(&self) -> &Arc<DiagnosticStore> {
        &self.store
    }

    pub fn encoding_of(&self, server: &str) -> Encoding {
        let code = self
            .slots
            .iter()
            .find(|slot| slot.spec.name == server)
            .map_or(1, |slot| slot.encoding.load(Ordering::SeqCst));
        match code {
            0 => Encoding::Utf8,
            2 => Encoding::Utf32,
            _ => Encoding::Utf16,
        }
    }

    fn lookup(&self, path: &Path) -> Option<(usize, &str)> {
        let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
        self.extensions
            .get(&format!(".{extension}"))
            .map(|(index, language)| (*index, language.as_str()))
    }

    fn no_server(path: &Path) -> ToolExecutionError {
        ToolExecutionError::other(match path.extension() {
            Some(extension) => format!(
                "no language server is configured for .{} files",
                extension.to_string_lossy()
            ),
            None => "no language server is configured for files without an extension".to_string(),
        })
    }

    async fn start(&self, slot: &Slot) -> Result<Arc<Client>, StartFailure> {
        let spec = &slot.spec;
        let folders = self.workspace.folders();
        let cwd = folders
            .first()
            .cloned()
            .unwrap_or_else(|| self.workspace.scope().primary().to_path_buf());
        let config = StartConfig {
            name: spec.name.clone(),
            command: spec.command.clone(),
            args: spec.args.clone(),
            cwd,
            env: server_env(&*self.env, &spec.env_vars, &spec.set_env),
            initialization_options: spec.initialization_options.clone(),
            settings: spec.settings.clone(),
            folders,
            startup_timeout: spec.startup_timeout,
            request_timeout: spec.request_timeout,
            shutdown_timeout: spec.shutdown_timeout,
        };
        let client = Client::start(config, self.store.clone()).await?;
        slot.encoding
            .store(encoding_code(client.encoding), Ordering::SeqCst);
        Ok(Arc::new(client))
    }

    fn forget_docs(&self, state: &mut SlotState) {
        for uri in state.docs.keys() {
            self.store.forget(uri);
        }
        state.docs.clear();
    }

    /// The running client, starting or restarting it as the state machine allows. A start that
    /// fails is terminal for the session; only a crash of a server that had reached `Running`
    /// takes the restart path.
    async fn ensure(&self, slot: &Slot, state: &mut SlotState) -> Result<Arc<Client>, Failure> {
        match &state.phase {
            Phase::Running(client) if client.is_alive() => return Ok(client.clone()),
            Phase::Failed(failure) => return Err(failure.clone()),
            Phase::Running(client) => {
                let detail = client.exit_detail().await;
                self.forget_docs(state);
                let spec = &slot.spec;
                if !(spec.restart_on_crash && state.restarts < spec.max_restarts) {
                    let failure = Failure {
                        kind: FailureKind::Crash,
                        detail,
                        stderr: None,
                    };
                    state.phase = Phase::Failed(failure.clone());
                    return Err(failure);
                }
                state.restarts += 1;
            }
            Phase::Idle => {}
        }
        match self.start(slot).await {
            Ok(client) => {
                state.phase = Phase::Running(client.clone());
                Ok(client)
            }
            Err(failure) => {
                let failure = Failure::start(failure);
                state.phase = Phase::Failed(failure.clone());
                Err(failure)
            }
        }
    }

    /// Tells the server what is on disk: `didOpen` the first time, `didChange` with the full
    /// text when it changed (or when `force`), then `didSave`. Returns the version the server
    /// now holds. A file that was deleted since gets `didClose`.
    fn sync(
        &self,
        state: &mut SlotState,
        client: &Client,
        uri: &Url,
        language: &str,
        text: &str,
        edit_by: Option<&str>,
    ) -> i32 {
        self.close_deleted(state, client);
        let hash = hash_text(text);
        let (version, send_open) = match state.docs.get(uri) {
            None => (1, true),
            Some(doc) if doc.hash != hash || edit_by.is_some() => (doc.version + 1, false),
            Some(doc) => return doc.version,
        };
        if let Some(agent) = edit_by {
            self.store.begin_sync(uri, agent, version);
        }
        let sent = if send_open {
            client.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: language.to_string(),
                    version,
                    text: text.to_string(),
                },
            })
        } else {
            client.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: text.to_string(),
                }],
            })
        };
        if sent.is_ok() {
            let _ = client.notify::<DidSaveTextDocument>(DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                text: None,
            });
        }
        state.docs.insert(uri.clone(), DocState { version, hash });
        version
    }

    fn close_deleted(&self, state: &mut SlotState, client: &Client) {
        let gone: Vec<Url> = state
            .docs
            .keys()
            .filter(|uri| uri.to_file_path().is_ok_and(|path| !path.exists()))
            .cloned()
            .collect();
        for uri in gone {
            let _ = client.notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
            });
            state.docs.remove(&uri);
            self.store.forget(&uri);
        }
    }

    /// Starts the server for `path` if needed and makes its view of the file current.
    pub async fn prepare(&self, path: &Path) -> Result<Prepared, ToolExecutionError> {
        let (index, language) = self.lookup(path).ok_or_else(|| Self::no_server(path))?;
        let slot = &self.slots[index];
        let name = slot.spec.name.clone();
        let mut state = slot.state.lock().await;
        let client = self
            .ensure(slot, &mut state)
            .await
            .map_err(|f| ToolExecutionError::other(f.tool_message(&name)))?;
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                self.close_deleted(&mut state, &client);
                return Err(ToolExecutionError::other(format!(
                    "cannot read {}: {}",
                    path.display(),
                    chain(&e)
                )));
            }
        };
        let uri = Url::from_file_path(path).map_err(|()| {
            ToolExecutionError::other(format!("{} is not an absolute path", path.display()))
        })?;
        self.sync(&mut state, &client, &uri, language, &text, None);
        Ok(Prepared {
            client,
            server: name,
            uri,
            text,
        })
    }

    /// After a successful `edit` or `write` of `path` by `agent`: syncs the file and waits a
    /// short while for the server's view of the new content. `None` when no server with
    /// diagnostics handles the file.
    pub async fn after_edit(&self, agent: &str, path: &Path) -> Option<EditReport> {
        let (index, language) = self.lookup(path)?;
        let slot = &self.slots[index];
        if !slot.spec.diagnostics {
            return None;
        }
        let server = slot.spec.name.clone();
        let unavailable = |why: String| {
            Some(EditReport {
                server: server.clone(),
                kind: EditKind::Unavailable(why),
            })
        };
        let uri = Url::from_file_path(path).ok()?;
        let mut state = slot.state.lock().await;
        let client = match self.ensure(slot, &mut state).await {
            Ok(client) => client,
            Err(failure) => return unavailable(failure.unavailable(&server)),
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                return unavailable(format!(
                    "{server} could not be told about {}: {}",
                    path.display(),
                    chain(&e)
                ));
            }
        };
        self.sync(&mut state, &client, &uri, language, &text, Some(agent));
        drop(state);
        let kind = match self.store.wait(&uri, diagnostics::WAIT).await {
            Some(diags) => EditKind::Published {
                uri,
                diags,
                encoding: client.encoding,
            },
            None => EditKind::NoneYet,
        };
        Some(EditReport { server, kind })
    }

    /// Tells running servers that advertise it about a root added by `/add-dir`.
    pub fn root_added(self: &Arc<Self>, root: PathBuf) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let servers = self.clone();
        runtime.spawn(async move {
            for slot in &servers.slots {
                let state = slot.state.lock().await;
                if let Phase::Running(client) = &state.phase {
                    client.folders_added(&root);
                }
            }
        });
    }

    /// Stops every server: `shutdown`, `exit`, a wait of the server's `shutdown_timeout`, then a
    /// kill. Forgets diagnostics and failures; a stopped server starts again on next need.
    pub async fn shutdown_all(&self) {
        let stops = self.slots.iter().map(|slot| async move {
            let mut state = slot.state.lock().await;
            let running = match std::mem::replace(&mut state.phase, Phase::Idle) {
                Phase::Running(client) => Some(client),
                Phase::Idle | Phase::Failed(_) => None,
            };
            state.restarts = 0;
            state.docs.clear();
            drop(state);
            if let Some(client) = running {
                client.shutdown().await;
            }
        });
        futures::future::join_all(stops).await;
        self.store.reset();
    }
}
