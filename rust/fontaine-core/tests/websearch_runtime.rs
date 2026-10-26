//! The client-side search tool inside a whole runtime: registered only when a backend is
//! configured, and a registry rebuild keeps the SearXNG process the first search started.

mod common;

use common::World;
use common::runtime::build;
use fontaine_core::config::PermissionMode::Manual;
use rig_core::test_utils::MockStreamEvent;

#[cfg(unix)]
fn as_root() -> rig_core::tool::ToolContext {
    use fontaine_tools::gate::{AgentId, ROOT_AGENT};
    let mut context = rig_core::tool::ToolContext::new();
    context
        .insert(AgentId(ROOT_AGENT.to_string()))
        .expect("the agent id serializes");
    context
}

#[test]
fn the_tool_exists_only_with_a_configured_backend() {
    let w = World::new();
    let built = build(&w, Manual, Vec::<Vec<MockStreamEvent>>::new(), true);
    assert!(
        !built
            .runtime
            .tool_names()
            .contains(&"websearch".to_string())
    );

    let w = World::new();
    w.user_file("[websearch]\nbackend = \"searxng\"\nbase_url = \"http://127.0.0.1:9\"\n");
    let built = build(&w, Manual, Vec::<Vec<MockStreamEvent>>::new(), true);
    assert!(
        built
            .runtime
            .tool_names()
            .contains(&"websearch".to_string())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_rebuild_keeps_the_launched_instance() {
    use std::os::unix::fs::PermissionsExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [{"title": "T", "url": "https://a.example/", "content": "s"}]
        })))
        .mount(&server)
        .await;

    let w = World::new();
    let script = w.root.join("start.sh");
    let starts = w.root.join("starts");
    common::write(
        &script,
        &format!(
            "#!/bin/sh\necho started >> '{}'\nexec sleep 300\n",
            starts.display()
        ),
    );
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    common::write(
        &w.config.join("output-styles/terse.md"),
        "---\ndescription: Short\n---\nBe brief.\n",
    );
    w.user_file(&format!(
        "[websearch]\nbackend = \"searxng\"\nbase_url = \"{}\"\ncommand = [\"{}\"]\n\
         [permissions]\nallow = [\"websearch\"]\n",
        server.uri(),
        script.display()
    ));
    let mut built = build(&w, Manual, Vec::<Vec<MockStreamEvent>>::new(), true);

    let search = |runtime: &fontaine_core::agent::AgentRuntime| {
        let handle = runtime.tool_server().clone();
        async move {
            handle
                .toolset()
                .execute("websearch", r#"{"query": "x"}"#, &mut as_root())
                .await
                .into_result()
                .map(|o| o.render())
                .map_err(|e| e.model_output().render())
        }
    };
    assert!(search(&built.runtime).await.unwrap().contains("1. T"));
    built.runtime.set_output_style(Some("terse")).unwrap();
    assert!(search(&built.runtime).await.unwrap().contains("1. T"));

    // The script may still be writing when the first probe is answered.
    for _ in 0..100 {
        if std::fs::read_to_string(&starts).is_ok_and(|s| s.ends_with('\n')) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let started = std::fs::read_to_string(&starts).unwrap();
    assert_eq!(started.lines().count(), 1, "{started}");
}
