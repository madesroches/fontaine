//! `[shell] max_timeout_secs` reaches the registry the model's `shell` tool is built from.

mod common;

use common::World;
use common::runtime::build;
use fontaine_core::config::PermissionMode::Manual;
use rig_core::test_utils::MockStreamEvent;

async fn shell_description(w: &World) -> String {
    let built = build(w, Manual, Vec::<Vec<MockStreamEvent>>::new(), true);
    let defs = built
        .runtime
        .tool_server()
        .tool_defs(None)
        .await
        .expect("the tool definitions can be read");
    defs.iter()
        .find(|d| d.name == "shell")
        .expect("shell is registered")
        .description
        .clone()
}

#[tokio::test]
async fn the_configured_cap_reaches_the_shell_tool() {
    let w = World::new();
    assert!(shell_description(&w).await.contains("at most 600"));

    let w = World::new();
    w.user_file("[shell]\nmax_timeout_secs = 900\n");
    assert!(shell_description(&w).await.contains("at most 900"));
}
