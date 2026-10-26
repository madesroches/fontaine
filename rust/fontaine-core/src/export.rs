//! The conversation as a Markdown file for a person to read. It is built from the model's
//! conversation log, so a compaction shows as the summary that replaced the messages it took
//! out, and after `/clear` only the conversation since is in it.

use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

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

/// Writes the session's conversation to `path`, which must not exist. The file is readable by
/// the current user alone, since a transcript can hold secrets.
pub async fn export_session(session: &Session, path: &Path) -> Result<(), ExportError> {
    let conversation = load_async(&session.store(), CONVERSATION_LOG)
        .await
        .map_err(ExportError::Conversation)?;
    // A name that cannot be read only leaves the title plain.
    let name = session.name().ok().flatten();
    let text = render(&conversation, &session.id, name.as_deref());
    write_new(path, &text)
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

/// The conversation as Markdown. Tool calls are one line each; tool results appear only as their
/// length.
pub fn render(conversation: &Conversation, id: &str, name: Option<&str>) -> String {
    let compacted: Vec<_> = conversation
        .asides()
        .filter(|(_, aside)| aside.kind == COMPACTED)
        .filter_map(|(_, aside)| aside.after)
        .collect();
    let summarizer = summary_author();
    let mut doc = Doc {
        out: String::new(),
        in_list: false,
    };
    doc.block(&format!("# {}", name.unwrap_or("Fontaine session")));
    doc.block(&format!("Session `{id}`"));
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
                for item in content {
                    match item {
                        UserContent::ToolResult(result) => {
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
                        other => {
                            if !headed {
                                doc.block(&heading);
                                if is_summary {
                                    doc.block(
                                        "_The conversation before this point was compacted into this summary._",
                                    );
                                }
                                headed = true;
                            }
                            match other {
                                UserContent::Text(text) => doc.block(&text.text),
                                _ => doc.block("_[attachment omitted]_"),
                            }
                        }
                    }
                }
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
    doc.out.push('\n');
    doc.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::conversation::Entry;
    use rig_core::message::{CallId, Text, ToolCall, ToolFunction, ToolName, ToolResult};
    use rig_memory::{SummaryState, apply_summary};
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
}
