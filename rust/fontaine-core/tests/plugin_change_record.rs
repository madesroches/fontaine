//! A plugin change the human made is recorded in the session ledger and the audit log.

mod common;

use common::pipeline::Rig;
use fontaine_core::audit::Decision;
use fontaine_core::config::PermissionMode;
use fontaine_core::ledger::Record;
use fontaine_core::tier::Tier;

#[tokio::test]
async fn a_plugin_change_reaches_the_ledger_and_the_audit_log() {
    let rig = Rig::new(PermissionMode::Manual);
    rig.state
        .record_plugin_change("install", "review", "sha256:abc");

    let ledger = rig.session.ledger.read().unwrap();
    assert!(ledger.iter().any(|entry| matches!(
        &entry.record,
        Record::PluginChange { op, plugin, fingerprint }
            if op == "install" && plugin == "review" && fingerprint == "sha256:abc"
    )));

    let records = rig.audit();
    let record = records
        .iter()
        .find(|r| r.rule.as_deref() == Some("plugin_change"))
        .expect("the change is audited");
    assert_eq!(record.tool, "plugin_change");
    assert_eq!(record.tier, Tier::Human);
    assert_eq!(record.decision, Decision::Allow);
    assert_eq!(
        record.explanation.as_deref(),
        Some("plugin install: review")
    );
}
