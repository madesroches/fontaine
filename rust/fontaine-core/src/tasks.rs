//! The team's task board on disk, beside `todos.json` in the session directory. Agent ids do not
//! survive a resume, so what is loaded back has nobody working on it (the board settles that).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fontaine_agents::tasks::{Snapshot, TaskStore};
use fontaine_tools::error_chain::chain;

use crate::owner_only;
use crate::permissions::Approver;

pub const TASKS_FILE: &str = "tasks.json";

pub struct SessionTasks {
    path: PathBuf,
    approver: Arc<dyn Approver>,
}

impl SessionTasks {
    pub fn new(session_dir: &Path, approver: Arc<dyn Approver>) -> SessionTasks {
        SessionTasks {
            path: session_dir.join(TASKS_FILE),
            approver,
        }
    }

    /// What an earlier process of the session saved. A missing file is an empty board; one that
    /// cannot be read or parsed is too, with a warning, since a board that starts over loses
    /// less than a session that does not start.
    pub fn load(&self) -> Snapshot {
        let loaded = match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<Snapshot>(&bytes).map_err(|e| chain(&e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Snapshot::default(),
            Err(e) => Err(chain(&e)),
        };
        loaded.unwrap_or_else(|reason| {
            self.approver.warn(&format!(
                "the saved task board {} was not loaded: {reason}",
                self.path.display()
            ));
            Snapshot::default()
        })
    }
}

impl TaskStore for SessionTasks {
    fn save(&self, snapshot: &Snapshot) {
        let text = serde_json::to_vec(snapshot).expect("a board of plain tasks serializes");
        if let Err(e) = owner_only::write_atomic(&self.path, &text) {
            self.approver.warn(&format!(
                "the task board was not saved to {}: {}",
                self.path.display(),
                chain(&e)
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_agents::tasks::{TaskBoard, TaskStatus};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Warnings(Mutex<Vec<String>>);

    impl Approver for Warnings {
        fn approve<'a>(
            &'a self,
            _: crate::permissions::ApprovalRequest,
        ) -> futures::future::BoxFuture<'a, crate::permissions::Answer> {
            Box::pin(async { crate::permissions::Answer::Deny { feedback: None } })
        }

        fn warn(&self, message: &str) {
            self.0.lock().unwrap().push(message.to_string());
        }
    }

    #[test]
    fn a_saved_board_loads_back_without_owners() {
        let dir = tempfile::tempdir().unwrap();
        let approver = Arc::new(Warnings::default());
        let store = Arc::new(SessionTasks::new(dir.path(), approver.clone()));
        let board = TaskBoard::new(Some(store.clone()));
        let first = board.create("root", "first", "do it", &[]).unwrap();
        board.claim("agent-1", &first.id).unwrap();
        let restored = TaskBoard::restore(None, store.load());
        let task = restored.get(&first.id).unwrap();
        assert_eq!((task.status, task.owner), (TaskStatus::Pending, None));
        assert_eq!(restored.create("root", "next", "", &[]).unwrap().id, "t2");
        assert!(approver.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failed_save_is_warned() {
        let dir = tempfile::tempdir().unwrap();
        let approver = Arc::new(Warnings::default());
        let store = SessionTasks::new(&dir.path().join("gone"), approver.clone());
        store.save(&Snapshot::default());
        assert_eq!(approver.0.lock().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionTasks::new(dir.path(), Arc::new(Warnings::default()));
        store.save(&Snapshot::default());
        let mode = std::fs::metadata(dir.path().join(TASKS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[test]
    fn a_corrupt_file_warns_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(TASKS_FILE), b"{nope").unwrap();
        let approver = Arc::new(Warnings::default());
        let store = SessionTasks::new(dir.path(), approver.clone());
        assert_eq!(store.load(), Snapshot::default());
        assert_eq!(approver.0.lock().unwrap().len(), 1);
    }
}
