//! The embeddable Fontaine agent: a guarded coding agent another Rust program can run in process.
//!
//! Build a session with [`SessionBuilder`], register tools ([`HostTool`]) and hooks
//! ([`CallbackHook`]), answer approvals through an [`Approver`], and read the run as a stream of
//! [`Event`]s, which serialize to the same `headless-v1` JSON that `fontaine -p` prints.
//!
//! The crate root and the [`permissions`] and [`config`] modules are the stable surface. The
//! [`core`], [`tools`], [`agents`], [`guardian`], [`assemble`] and [`run`] trees are public
//! because the `fontaine` binary, its terminal UI and the evals are built on them; they carry no
//! stability promise.
//!
//! ```no_run
//! use fontaine_sdk::{PermissionMode, SessionBuilder};
//! use futures::StreamExt;
//!
//! # async fn demo() -> Result<(), fontaine_sdk::EmbedError> {
//! let mut session = SessionBuilder::new(".")
//!     .mode(PermissionMode::Manual)
//!     .build()
//!     .await?;
//! let mut turn = session.send("list the files here");
//! while let Some(event) = turn.next().await {
//!     println!("{}", serde_json::to_string(&event?).expect("an event serializes"));
//! }
//! drop(turn);
//! session.close().await?;
//! # Ok(())
//! # }
//! ```

pub mod agents;
pub mod assemble;
pub mod core;
pub mod error;
pub mod events;
pub mod guardian;
pub mod run;
pub mod session;
pub mod tools;

pub use error::EmbedError;
pub use events::{Event, SCHEMA_VERSION};
pub use session::{InterruptHandle, Session, SessionBuilder, SessionRef, Turn};
pub use tools::host::HostTool;

pub use futures::future::BoxFuture;
pub use rig_core::tool::ToolExecutionError;

/// Who answers a prompt and what they are asked.
pub mod permissions {
    pub use crate::core::config::PermissionMode;
    pub use crate::core::permissions::{Answer, ApprovalRequest, Approver, DenyAll, HumanChannel};
}

/// The settings types the builder takes.
pub mod config {
    pub use crate::core::config::{HookEvent, PermissionMode};
    pub use crate::core::permissions::HookError;
    pub use crate::core::user_hooks::{CallbackHook, HookTransport};
}

pub use config::{CallbackHook, HookEvent, HookTransport};
pub use permissions::{Approver, PermissionMode};

#[cfg(test)]
mod boundary {
    use std::path::Path;

    fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("the source tree is readable") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    fn uses(root: &str, module: &str) -> bool {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(root);
        let mut files = Vec::new();
        sources(&src, &mut files);
        let mut found = false;
        for file in files {
            let text = std::fs::read_to_string(&file).expect("a source file is readable");
            for (n, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or(line);
                if code.contains(&format!("crate::{module}"))
                    || code.contains(&format!("crate::{{{module}"))
                    || code.contains(&format!(", {module}::"))
                        && code.trim_start().starts_with("use crate::{")
                {
                    eprintln!("{}:{}: {line}", file.display(), n + 1);
                    found = true;
                }
            }
        }
        found
    }

    #[test]
    fn the_guardian_module_does_not_reach_into_core_tools_or_agents() {
        for module in ["core", "tools", "agents"] {
            assert!(
                !uses("guardian", module),
                "src/guardian names crate::{module}; it must stay liftable"
            );
        }
    }

    #[test]
    fn the_tools_and_agents_modules_do_not_reach_into_core() {
        for root in ["tools", "agents"] {
            assert!(
                !uses(root, "core"),
                "src/{root} names crate::core; core sits above it"
            );
        }
    }
}
