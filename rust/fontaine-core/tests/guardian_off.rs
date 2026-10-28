//! The `/guardian-disable` escape hatch: a root call runs before the floor and the guardian,
//! the toggle is recorded, and children are never loosened by it.

mod common;

use common::pipeline::{Rig, allowed};
use fontaine_agents::mode::Ladder;
use fontaine_core::audit::Decision;
use fontaine_core::config::PermissionMode;
use fontaine_core::ledger::Record;
use fontaine_core::tier::Tier;
use fontaine_tools::gate::ROOT_AGENT;
use serde_json::json;

fn write_outside() -> serde_json::Value {
    json!({"path": "../outside.txt", "content": "x"})
}

fn child(rig: &Rig, own: Ladder) -> String {
    rig.agents
        .reserve(ROOT_AGENT, "worker", "m", Vec::new(), own, false)
        .unwrap()
}

#[tokio::test]
async fn guardian_off_runs_a_denied_call_and_is_recorded() {
    let rig = Rig::new(PermissionMode::Manual);

    let before = rig.audit().len();
    assert!(!allowed(&rig.check("write", write_outside()).await));
    assert_eq!(rig.last_first(before).decision, Decision::Deny);

    rig.state.set_guardian_off(true);
    assert!(rig.state.guardian_off());

    let before = rig.audit().len();
    let decision = rig.check("write", write_outside()).await;
    assert!(allowed(&decision));
    let record = rig.last_first(before);
    assert_eq!(record.decision, Decision::Allow);
    assert_eq!(record.tier, Tier::GuardianOff);

    let ledger = rig.session.ledger.read().unwrap();
    assert!(
        ledger
            .iter()
            .any(|entry| matches!(entry.record, Record::GuardianOff { off: true }))
    );
    assert!(
        rig.audit()
            .iter()
            .any(|record| record.rule.as_deref() == Some("guardian_off"))
    );

    rig.state.set_guardian_off(false);
    assert!(!rig.state.guardian_off());
    assert!(!allowed(&rig.check("write", write_outside()).await));
}

#[tokio::test]
async fn guardian_off_never_loosens_a_child() {
    let rig = Rig::new(PermissionMode::Auto);
    let id = child(&rig, Ladder::Manual);

    rig.state.set_guardian_off(true);
    // The root runs anything, but the child keeps its own manual posture and still asks.
    assert!(allowed(&rig.check("write", write_outside()).await));
    assert!(!allowed(
        &rig.check_as(&id, "shell", json!({"command": "python x.py"}))
            .await
    ));
    assert_eq!(rig.approver.asked(), 1);
}
