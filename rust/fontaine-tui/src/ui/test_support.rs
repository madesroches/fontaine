//! A bare session state for unit tests that need to audit or record without a whole runtime.

use std::sync::Arc;

use fontaine_sdk::agents::tree::{AgentTree, Limits};
use fontaine_sdk::core::audit::AuditLog;
use fontaine_sdk::core::config::PermissionMode;
use fontaine_sdk::core::gate::{AutoContext, SessionState, StateParams, no_guardian};
use fontaine_sdk::core::ledger::Ledger;
use fontaine_sdk::core::permissions::DenyAll;
use fontaine_sdk::tools::scope::Scope;

pub(crate) fn session_state() -> Arc<SessionState> {
    // The directory lives as long as the process: a test keeps its audit log open.
    let dir = tempfile::tempdir().expect("a scratch directory").keep();
    let agents = Arc::new(AgentTree::new(Limits::default(), 1));
    agents.set_root_tools(|| {
        fontaine_sdk::tools::descriptor::names()
            .map(String::from)
            .collect()
    });
    Arc::new(SessionState::new(StateParams {
        session_id: "tui-test".into(),
        mode: PermissionMode::Manual,
        scope: Arc::new(Scope::new(dir.clone(), [])),
        deny: Vec::new(),
        ask: Vec::new(),
        allow: Vec::new(),
        ledger: Arc::new(Ledger::create(&dir).expect("a ledger")),
        audit: Arc::new(AuditLog::create(&dir, "tui-test").expect("an audit log")),
        approver: Arc::new(DenyAll),
        home: None,
        protected_dirs: Vec::new(),
        scratch: None,
        memory_root: None,
        guardian: no_guardian(),
        auto: AutoContext::default(),
        agents,
        agent_types: Arc::new(fontaine_sdk::tools::live::Live::new(
            fontaine_sdk::agents::registry::AgentTypes::builtin(),
        )),
        reveal_mcp_names: false,
        auto_edits: fontaine_sdk::core::gate::AutoEdits::default(),
        effort: None,
        retry: fontaine_sdk::core::retry::RetryPolicy::default(),
    }))
}
