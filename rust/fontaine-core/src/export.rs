//! The conversation as Markdown for a person to read, the root's and every subagent's. It
//! is built from the model's conversation logs, so a compaction shows as the summary that
//! replaced the messages it took out, and after `/clear` only the conversation since is in
//! it. A session with subagents exports a folder: `main.md` with the root conversation and
//! a Subagents index, plus one `<agent-id>.md` per subagent.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use fontaine_agents::tree::{AgentRow, AgentTree};
use rig_core::conversation::{AuthorKind, Conversation};
use rig_core::message::{AssistantContent, Message, ToolResultContent, UserContent};
use rig_memory::{COMPACTED, load_async, summary_author};
use serde_json::Value;

use crate::adjudication::action_summary;
use crate::owner_only;
use crate::session::{CONVERSATION_LOG, Session};

#[derive(Debug)]
pub enum ExportError {
    Conversation(rig_memory::SessionError),
    /// The file is there already; the export never overwrites.
    Exists(PathBuf),
    Write {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Conversation(_) => f.write_str("the conversation could not be read"),
            ExportError::Exists(path) => write!(
                f,
                "{} already exists; choose another path, as an export never overwrites",
                path.display()
            ),
            ExportError::Write { path, .. } => write!(f, "cannot write {}", path.display()),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExportError::Conversation(source) => Some(source),
            ExportError::Exists(_) => None,
            ExportError::Write { source, .. } => Some(source),
        }
    }
}

/// One subagent of the session as the export lists it. The ledger is the spine (oldest
/// first); for an agent that also lives in this process's tree, status, parent and type are
/// the live row's instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSummary {
    pub id: String,
    pub parent: Option<String>,
    pub agent_type: String,
    /// The live status, the ledger's finished status, or `ended` when the ledger has no
    /// finished record for it.
    pub status: String,
    /// The agent runs in this process; its transcript file holds completed runs only.
    pub running: bool,
    /// Whether a transcript file is written and linked; false only when the log cannot be
    /// read at all.
    pub linked: bool,
}

/// The session's subagents, merged from the ledger (the spine, oldest first) and the live
/// tree (live status, parent and type), deduplicated by id.
pub fn subagents(session: &Session, tree: &AgentTree) -> Vec<AgentSummary> {
    let ledger_agents = session.ledger.view().agents.clone();
    let live: HashMap<String, AgentRow> = tree
        .rows()
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    let from_row = |row: &AgentRow| AgentSummary {
        id: row.id.clone(),
        parent: row.parent.clone(),
        agent_type: row.agent_type.clone(),
        status: row.status.as_str().to_string(),
        running: row.status.is_running(),
        linked: true,
    };
    let mut merged: Vec<AgentSummary> = ledger_agents
        .iter()
        .map(|record| match live.get(&record.id) {
            Some(row) => from_row(row),
            None => AgentSummary {
                id: record.id.clone(),
                parent: record.parent.clone(),
                agent_type: record.agent_type.clone(),
                status: record.ended.clone().unwrap_or_else(|| "ended".to_string()),
                running: false,
                linked: true,
            },
        })
        .collect();
    // Every live agent has a spawned record; this covers a tree fed by another ledger only.
    for row in live.values() {
        if !ledger_agents.iter().any(|record| record.id == row.id) {
            merged.push(from_row(row));
        }
    }
    merged
}

/// Writes the session's conversation to `path`, which must not exist. The export is
/// readable by the current user alone, since a transcript can hold secrets. With no
/// subagents `path` is one Markdown file, as it has always been; with subagents it is a
/// folder holding `main.md` and one `<agent-id>.md` per subagent.
pub async fn export_session(
    session: &Session,
    tree: &AgentTree,
    path: &Path,
) -> Result<(), ExportError> {
    let name = session.name().ok().flatten();
    // Every load happens before the folder exists, so a failed load cannot leave a
    // half-written export behind.
    let root = load_async(&session.store(), CONVERSATION_LOG)
        .await
        .map_err(ExportError::Conversation)?;
    let mut agents = subagents(session, tree);
    if agents.is_empty() {
        let text = render(&root, &session.id, name.as_deref());
        return write_new(path, &text);
    }
    let store = session.store();
    let mut transcripts = Vec::with_capacity(agents.len());
    for agent in &mut agents {
        match load_async(&store, &agent.id).await {
            Ok(conversation) => transcripts.push(Some(conversation)),
            Err(e) => {
                tracing::warn!(agent = %agent.id, error = %e, "the subagent transcript could not be read");
                transcripts.push(None);
                agent.linked = false;
            }
        }
    }
    let mut files = vec![(
        PathBuf::from("main.md"),
        render_main(&root, &session.id, name.as_deref(), &agents),
    )];
    for (agent, transcript) in agents.iter().zip(&transcripts) {
        if let Some(conversation) = transcript {
            files.push((
                PathBuf::from(format!("{}.md", agent.id)),
                render_agent_file(agent, &session.id, conversation),
            ));
        }
    }
    export_folder(path, &files)
}

/// Creates the folder and writes one owner-only file per entry, never over an existing
/// file. A failed write removes the folder again: it can hold only what this export just
/// created.
fn export_folder(path: &Path, files: &[(PathBuf, String)]) -> Result<(), ExportError> {
    if let Err(e) = owner_only::create_dir(path) {
        return Err(if e.kind() == io::ErrorKind::AlreadyExists {
            ExportError::Exists(path.to_path_buf())
        } else {
            ExportError::Write {
                path: path.to_path_buf(),
                source: e,
            }
        });
    }
    let written = files
        .iter()
        .try_for_each(|(name, text)| write_new(&path.join(name), text));
    if let Err(e) = written {
        let _ = owner_only::remove_dir_all_retrying(path);
        return Err(e);
    }
    Ok(())
}

fn write_new(path: &Path, text: &str) -> Result<(), ExportError> {
    let fail = |source: io::Error| ExportError::Write {
        path: path.to_path_buf(),
        source,
    };
    let mut file = match owner_only::create_new(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ExportError::Exists(path.to_path_buf()));
        }
        Err(e) => return Err(fail(e)),
    };
    let written = file.write_all(text.as_bytes()).and_then(|()| file.flush());
    if let Err(e) = written {
        drop(file);
        // Best effort: a half-written transcript would be mistaken for a whole one.
        let _ = std::fs::remove_file(path);
        return Err(fail(e));
    }
    Ok(())
}

/// Collects the document, keeping consecutive tool lines together as one list.
struct Doc {
    out: String,
    in_list: bool,
}

impl Doc {
    fn new() -> Doc {
        Doc {
            out: String::new(),
            in_list: false,
        }
    }

    fn block(&mut self, text: &str) {
        if !self.out.is_empty() {
            self.out.push_str("\n\n");
        }
        self.out.push_str(text.trim_end());
        self.in_list = false;
    }

    fn item(&mut self, line: &str) {
        if self.in_list {
            self.out.push('\n');
        } else if !self.out.is_empty() {
            self.out.push_str("\n\n");
        }
        self.out.push_str(line);
        self.in_list = true;
    }
}

fn result_chars(content: &[ToolResultContent]) -> usize {
    content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text(text) => text.text.chars().count(),
            ToolResultContent::Json { value } => value.to_string().chars().count(),
            _ => 0,
        })
        .sum()
}

fn render_messages(doc: &mut Doc, conversation: &Conversation) {
    let compacted: Vec<_> = conversation
        .asides()
        .filter(|(_, aside)| aside.kind == COMPACTED)
        .filter_map(|(_, aside)| aside.after)
        .collect();
    let summarizer = summary_author();
    for entry in conversation.entries() {
        match entry.message {
            Message::System { .. } => {}
            Message::User { content } => {
                let is_summary = entry.author == Some(&summarizer) && compacted.contains(&entry.id);
                let heading = match entry.author {
                    _ if is_summary => "## Compaction summary".to_string(),
                    Some(author) if author.kind != AuthorKind::User || author.title.is_some() => {
                        format!("## Note from {}", author.label())
                    }
                    _ => "## User".to_string(),
                };
                let mut headed = false;
                // Non-text parts carry no prose worth exporting; one marker names how many.
                let mut omitted = 0usize;
                let flush = |doc: &mut Doc, headed: &mut bool, omitted: &mut usize| {
                    if *omitted == 0 {
                        return;
                    }
                    if !*headed {
                        doc.block(&heading);
                        if is_summary {
                            doc.block(
                                "_The conversation before this point was compacted into this summary._",
                            );
                        }
                        *headed = true;
                    }
                    let marker = if *omitted == 1 {
                        "_[1 attachment omitted]_".to_string()
                    } else {
                        format!("_[{} attachments omitted]_", *omitted)
                    };
                    doc.block(&marker);
                    *omitted = 0;
                };
                for item in content {
                    match item {
                        UserContent::ToolResult(result) => {
                            flush(doc, &mut headed, &mut omitted);
                            let mut line = format!(
                                "- result of `{}`: {} characters",
                                result.name.as_str(),
                                result_chars(&result.content)
                            );
                            if result.is_error {
                                line.push_str(" (error)");
                            }
                            doc.item(&line);
                        }
                        UserContent::Text(text) => {
                            flush(doc, &mut headed, &mut omitted);
                            if !headed {
                                doc.block(&heading);
                                if is_summary {
                                    doc.block(
                                        "_The conversation before this point was compacted into this summary._",
                                    );
                                }
                                headed = true;
                            }
                            doc.block(&text.text);
                        }
                        _ => omitted += 1,
                    }
                }
                flush(doc, &mut headed, &mut omitted);
            }
            Message::Assistant(message) => {
                let mut headed = false;
                for item in &message.content {
                    match item {
                        AssistantContent::Text(text) => {
                            if !headed {
                                doc.block("## Assistant");
                                headed = true;
                            }
                            doc.block(&text.text);
                        }
                        AssistantContent::ToolCall(call) => {
                            let args = Value::Object(call.function.arguments.clone());
                            doc.item(&format!(
                                "- `{}`: {}",
                                call.function.name.as_str(),
                                action_summary(call.function.name.as_str(), &args)
                            ));
                        }
                        // Reasoning, images and provider items are not the conversation.
                        _ => {}
                    }
                }
            }
        }
    }
}

/// The "## Subagents" index of the main file: one bullet per subagent, oldest first, each
/// linking its transcript file. A running agent's row says its file holds completed runs
/// only; a transcript that could not be read is listed without a link.
pub fn index_section(agents: &[AgentSummary]) -> String {
    let mut doc = Doc::new();
    doc.block("## Subagents");
    for agent in agents {
        let mut line = format!("- `{}` — {}", agent.id, agent.agent_type);
        if let Some(parent) = &agent.parent {
            line.push_str(&format!(", child of `{parent}`"));
        }
        line.push_str(": ");
        if agent.running {
            line.push_str("running at export; its file holds completed runs only");
        } else {
            line.push_str(&agent.status);
        }
        if agent.linked {
            line.push_str(&format!(" — [transcript]({}.md)", agent.id));
        } else {
            line.push_str(" — transcript unavailable");
        }
        doc.item(&line);
    }
    doc.out
}

/// One subagent's file: an identifying header, then the same render the root export uses.
/// A log with no completed runs says so.
pub fn render_agent_file(
    agent: &AgentSummary,
    session_id: &str,
    conversation: &Conversation,
) -> String {
    let mut doc = Doc::new();
    doc.block(&format!("# Subagent `{}` ({})", agent.id, agent.agent_type));
    doc.block(&format!("Session `{session_id}`"));
    if let Some(parent) = &agent.parent {
        doc.block(&format!("Parent: `{parent}`"));
    }
    doc.block(&format!("Status: {}", agent.status));
    if conversation.entries().next().is_none() {
        doc.block("_No completed runs at export time._");
    } else {
        render_messages(&mut doc, conversation);
    }
    doc.out.push('\n');
    doc.out
}

/// The conversation as Markdown. Tool calls are one line each; tool results appear only as their
/// length.
pub fn render(conversation: &Conversation, id: &str, name: Option<&str>) -> String {
    render_main(conversation, id, name, &[])
}

/// The main file of a folder export: the root render with the Subagents index after the
/// session header. With no agents it is [`render`] exactly.
fn render_main(
    conversation: &Conversation,
    id: &str,
    name: Option<&str>,
    agents: &[AgentSummary],
) -> String {
    let mut doc = Doc::new();
    doc.block(&format!("# {}", name.unwrap_or("Fontaine session")));
    doc.block(&format!("Session `{id}`"));
    if !agents.is_empty() {
        doc.block(&index_section(agents));
    }
    render_messages(&mut doc, conversation);
    doc.out.push('\n');
    doc.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_agents::tree::{AgentTree, Limits};
    use rig_core::conversation::Entry;
    use rig_core::message::{CallId, Text, ToolCall, ToolFunction, ToolName, ToolResult};
    use rig_memory::{SummaryState, apply_summary, save_async};
    use serde_json::json;

    fn call(id: &str, name: &str, args: Value) -> AssistantContent {
        AssistantContent::ToolCall(ToolCall::new(
            CallId::from_wire(id),
            ToolFunction::new(ToolName::new(name).unwrap(), args),
        ))
    }

    fn result(id: &str, name: &str, text: &str, is_error: bool) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: CallId::from_wire(id),
                name: ToolName::new(name).unwrap(),
                content: vec![ToolResultContent::Text(Text::from(text))],
                is_error,
            })],
        }
    }

    fn sample() -> Conversation {
        let mut conversation = Conversation::default();
        conversation.push(Message::user("list the files"));
        conversation.push(Message::Assistant(
            rig_core::message::AssistantMessage::new(vec![
                AssistantContent::Text(Text::from("Looking.")),
                call("c1", "shell", json!({ "command": "ls" })),
            ]),
        ));
        conversation.push(result("c1", "shell", "a.txt\nb.txt\n", false));
        conversation.push(Message::assistant("Two files."));
        conversation
    }

    fn summary(
        id: &str,
        parent: Option<&str>,
        agent_type: &str,
        status: &str,
        running: bool,
        linked: bool,
    ) -> AgentSummary {
        AgentSummary {
            id: id.into(),
            parent: parent.map(str::to_string),
            agent_type: agent_type.into(),
            status: status.into(),
            running,
            linked,
        }
    }

    fn agent_event(
        event: &str,
        agent: &str,
        parent: Option<&str>,
        detail: Option<&str>,
    ) -> crate::ledger::Record {
        crate::ledger::Record::Agent {
            event: event.into(),
            agent: agent.into(),
            parent: parent.map(str::to_string),
            detail: detail.map(str::to_string),
        }
    }

    #[test]
    fn roles_tool_calls_and_result_lengths_are_rendered_and_the_output_is_omitted() {
        let text = render(&sample(), "abc", Some("demo"));
        assert!(text.starts_with("# demo\n\nSession `abc`\n\n## User\n\nlist the files\n"));
        assert!(text.contains("## Assistant\n\nLooking.\n\n- `shell`: "));
        assert!(text.contains("ls"), "{text}");
        assert!(
            text.contains("\n- result of `shell`: 12 characters\n"),
            "{text}"
        );
        assert!(!text.contains("a.txt"), "{text}");
        assert!(text.contains("## Assistant\n\nTwo files."));
        // The call and its result form one list.
        assert!(!text.contains("`\n\n- result"), "{text}");
    }

    #[test]
    fn an_unnamed_session_gets_a_plain_title_and_an_error_result_says_so() {
        let mut conversation = sample();
        conversation.push(result("c2", "shell", "boom", true));
        let text = render(&conversation, "abc", None);
        assert!(text.starts_with("# Fontaine session\n"));
        assert!(text.contains("- result of `shell`: 4 characters (error)"));
    }

    #[test]
    fn a_compaction_summary_is_labelled_as_one() {
        let mut conversation = sample();
        let state = SummaryState {
            summary: "They listed files.".to_string(),
            ..SummaryState::default()
        };
        apply_summary(&mut conversation, None, &state).expect("a summary applies");
        let text = render(&conversation, "abc", None);
        assert!(text.contains("## Compaction summary"), "{text}");
        assert!(text.contains("They listed files."), "{text}");
        assert!(!text.contains("list the files"), "{text}");
    }

    #[test]
    fn a_message_by_another_writer_is_a_note_not_the_users_words() {
        let mut conversation = Conversation::default();
        conversation.push(
            Entry::new(Message::user("done"))
                .by(rig_core::conversation::Author::agent("a1b2c3d4", None)),
        );
        let text = render(&conversation, "abc", None);
        assert!(text.contains("## Note from agent a1b2c3d4"), "{text}");
        assert!(!text.contains("## User"), "{text}");
    }

    #[test]
    fn an_existing_file_is_never_overwritten_and_a_new_one_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.md");
        write_new(&path, "first").unwrap();
        assert!(matches!(
            write_new(&path, "second"),
            Err(ExportError::Exists(_))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        let missing = dir.path().join("no").join("out.md");
        assert!(matches!(
            write_new(&missing, "x"),
            Err(ExportError::Write { .. })
        ));
    }

    #[test]
    fn the_index_lists_agents_oldest_first_with_their_links_and_annotations() {
        let agents = [
            summary("agent-1", None, "worker", "done", false, true),
            summary("agent-2", Some("agent-1"), "explore", "running", true, true),
            summary("agent-3", Some("agent-1"), "worker", "ended", false, false),
        ];
        let text = index_section(&agents);
        assert_eq!(
            text,
            "## Subagents\n\n\
             - `agent-1` — worker: done — [transcript](agent-1.md)\n\
             - `agent-2` — explore, child of `agent-1`: running at export; its file holds completed runs only — [transcript](agent-2.md)\n\
             - `agent-3` — worker, child of `agent-1`: ended — transcript unavailable"
        );
    }

    #[test]
    fn an_agent_file_opens_with_its_identity_and_renders_like_the_root() {
        let agent = summary("agent-2", Some("agent-1"), "explore", "done", false, true);
        let text = render_agent_file(&agent, "abc", &sample());
        assert!(
            text.starts_with(
                "# Subagent `agent-2` (explore)\n\nSession `abc`\n\nParent: `agent-1`\n\nStatus: done\n"
            ),
            "{text}"
        );
        assert!(text.contains("## User\n\nlist the files"), "{text}");
        assert!(
            text.contains("- result of `shell`: 12 characters"),
            "{text}"
        );
    }

    #[test]
    fn an_agent_file_with_no_completed_run_says_so() {
        let agent = summary("agent-1", Some("main"), "worker", "ended", false, true);
        let text = render_agent_file(&agent, "abc", &Conversation::default());
        assert!(
            text.contains("Status: ended\n\n_No completed runs at export time._\n"),
            "{text}"
        );
    }

    fn session_with_agents(data: &Path) -> (Session, AgentTree) {
        let session = Session::create(data, "00000000-0000-4000-8000-000000000001").unwrap();
        session
            .ledger
            .append(agent_event(
                "spawned",
                "agent-1",
                Some("root"),
                Some("worker"),
            ))
            .unwrap();
        session
            .ledger
            .append(agent_event("finished", "agent-1", None, Some("done")))
            .unwrap();
        session
            .ledger
            .append(agent_event(
                "spawned",
                "agent-2",
                Some("agent-1"),
                Some("explore"),
            ))
            .unwrap();
        (session, AgentTree::new(Limits::default(), 3))
    }

    async fn store_conversation(session: &Session, log: &str, conversation: &mut Conversation) {
        save_async(&session.store(), log, conversation)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_session_with_recorded_agents_exports_a_folder_even_with_an_empty_tree() {
        // The shape after --resume: the ledger holds every agent, the tree is empty.
        let dir = tempfile::tempdir().unwrap();
        let (session, tree) = session_with_agents(dir.path());
        store_conversation(&session, CONVERSATION_LOG, &mut sample()).await;
        let mut child = sample();
        store_conversation(&session, "agent-1", &mut child).await;
        let out = dir.path().join("out");
        export_session(&session, &tree, &out).await.unwrap();
        let main = std::fs::read_to_string(out.join("main.md")).unwrap();
        assert!(
            main.contains("Session `00000000-0000-4000-8000-000000000001`"),
            "{main}"
        );
        assert!(
            main.contains(
                "## Subagents\n\n- `agent-1` — worker, child of `root`: done — [transcript](agent-1.md)"
            ),
            "{main}"
        );
        assert!(
            main.contains(
                "- `agent-2` — explore, child of `agent-1`: ended — [transcript](agent-2.md)"
            ),
            "{main}"
        );
        assert!(main.contains("## User\n\nlist the files"), "{main}");
        // agent-2's log was never written: an empty transcript, still a real file.
        let second = std::fs::read_to_string(out.join("agent-2.md")).unwrap();
        assert!(
            second.contains("# Subagent `agent-2` (explore)"),
            "{second}"
        );
        assert!(
            second.contains("_No completed runs at export time._"),
            "{second}"
        );
        let first = std::fs::read_to_string(out.join("agent-1.md")).unwrap();
        assert!(first.contains("# Subagent `agent-1` (worker)"), "{first}");
        assert!(first.contains("list the files"), "{first}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&out).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
            for name in ["main.md", "agent-1.md", "agent-2.md"] {
                let mode = std::fs::metadata(out.join(name))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o077, 0, "{name}: {mode:o}");
            }
        }
    }

    #[tokio::test]
    async fn an_export_never_merges_into_an_existing_folder_or_file() {
        let dir = tempfile::tempdir().unwrap();
        let (session, tree) = session_with_agents(dir.path());
        store_conversation(&session, CONVERSATION_LOG, &mut sample()).await;
        let existing = dir.path().join("out");
        owner_only::create_dir(&existing).unwrap();
        std::fs::write(existing.join("keep.txt"), "mine").unwrap();
        assert!(matches!(
            export_session(&session, &tree, &existing).await,
            Err(ExportError::Exists(_))
        ));
        assert_eq!(
            std::fs::read_to_string(existing.join("keep.txt")).unwrap(),
            "mine"
        );
        assert!(!existing.join("main.md").exists());
        let as_file = dir.path().join("file-target");
        write_new(&as_file, "occupied").unwrap();
        assert!(matches!(
            export_session(&session, &tree, &as_file).await,
            Err(ExportError::Exists(_))
        ));
        assert_eq!(std::fs::read_to_string(&as_file).unwrap(), "occupied");
    }

    #[tokio::test]
    async fn a_failed_write_removes_the_folder_the_export_created() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out");
        // The second file lands on the first: a write fails after the folder exists.
        let files = [
            (PathBuf::from("main.md"), "one".to_string()),
            (PathBuf::from("main.md"), "two".to_string()),
        ];
        assert!(matches!(
            export_folder(&target, &files),
            Err(ExportError::Exists(_))
        ));
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn a_session_without_subagents_keeps_the_single_file_export() {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::create(dir.path(), "00000000-0000-4000-8000-000000000002").unwrap();
        store_conversation(&session, CONVERSATION_LOG, &mut sample()).await;
        let tree = AgentTree::new(Limits::default(), 1);
        let out = dir.path().join("out.md");
        export_session(&session, &tree, &out).await.unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text, render(&sample(), &session.id, None));
        assert!(!text.contains("## Subagents"), "{text}");
    }
}
