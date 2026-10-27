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
fn the_tool_does_not_exist_without_a_backend_or_a_container_runtime() {
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

mod default_backend {
    #[cfg(unix)]
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::*;
    use common::runtime::build_with_env;
    use fontaine_core::agent::AgentRuntime;
    use fontaine_core::config::AccountKind;
    use fontaine_core::models::{GuardianChoice, ModelHandle, ModelRef, Models};
    use rig_core::test_utils::MockCompletionModel;

    #[derive(Default)]
    struct Collect(Mutex<Vec<String>>);

    impl fontaine_core::permissions::Approver for Collect {
        fn approve<'a>(
            &'a self,
            _request: fontaine_core::permissions::ApprovalRequest,
        ) -> futures::future::BoxFuture<'a, fontaine_core::permissions::Answer> {
            Box::pin(async { fontaine_core::permissions::Answer::Deny { feedback: None } })
        }

        fn warn(&self, message: &str) {
            self.0.lock().unwrap().push(message.to_string());
        }
    }

    #[cfg(unix)]
    fn fake_runtime(dir: &Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn start(w: &World, path: Option<PathBuf>, approver: Arc<Collect>) -> AgentRuntime {
        let handle = ModelHandle::new(
            ModelRef::parse("test/mock").unwrap(),
            AccountKind::named("openai-compatible"),
            MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new()).into(),
        );
        let models = Models {
            main: handle,
            main_listing: Vec::new(),
            guardian: None,
            guardian_choice: GuardianChoice::MainFallback,
            notices: Vec::new(),
        };
        let path = path.map(std::ffi::OsString::from);
        build_with_env(
            w,
            Manual,
            models,
            None,
            true,
            Default::default(),
            None,
            approver,
            false,
            Arc::new(move |name| (name == "PATH").then(|| path.clone()).flatten()),
        )
    }

    fn has_tool(runtime: &AgentRuntime) -> bool {
        runtime.tool_names().contains(&"websearch".to_string())
    }

    #[cfg(unix)]
    #[test]
    fn a_runtime_on_the_path_registers_the_tool_by_default() {
        let w = World::new();
        let bin = w.root.join("bin");
        fake_runtime(&bin, "docker", "exit 0");
        let approver = Arc::new(Collect::default());
        let runtime = start(&w, Some(bin), approver.clone());
        assert!(has_tool(&runtime));
        assert!(approver.0.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn off_removes_the_default_tool_and_the_notice() {
        let w = World::new();
        let bin = w.root.join("bin");
        fake_runtime(&bin, "docker", "exit 0");
        w.user_file("[websearch]\nbackend = \"off\"\n");
        let approver = Arc::new(Collect::default());
        let runtime = start(&w, Some(bin), approver.clone());
        assert!(!has_tool(&runtime));
        assert!(approver.0.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn an_explicit_backend_is_used_whatever_is_installed() {
        let w = World::new();
        let bin = w.root.join("bin");
        fake_runtime(&bin, "docker", "exit 0");
        w.user_file("[websearch]\nbackend = \"searxng\"\nbase_url = \"http://127.0.0.1:9\"\n");
        let runtime = start(&w, Some(bin), Arc::default());
        assert!(has_tool(&runtime));
    }

    #[test]
    fn no_search_path_at_all_is_said_once_at_startup() {
        let w = World::new();
        let approver = Arc::new(Collect::default());
        let runtime = start(&w, None, approver.clone());
        assert!(!has_tool(&runtime));
        let warnings = approver.0.lock().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("web search is unavailable"),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("docker or podman"), "{warnings:?}");
    }

    #[test]
    fn off_and_an_explicit_backend_are_not_reported_as_unavailable() {
        for section in [
            "[websearch]\nbackend = \"off\"\n",
            "[websearch]\nbackend = \"searxng\"\nbase_url = \"http://127.0.0.1:9\"\n",
        ] {
            let w = World::new();
            w.user_file(section);
            let approver = Arc::new(Collect::default());
            start(&w, None, approver.clone());
            assert!(approver.0.lock().unwrap().is_empty(), "{section}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_first_search_starts_the_container_command_and_uses_its_port() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let w = World::new();
        let bin = w.root.join("bin");
        let argv = w.root.join("argv");
        fake_runtime(
            &bin,
            "docker",
            &format!(
                "[ \"$1\" = run ] || exit 0\nprintf '%s\\n' \"$@\" > '{0}.tmp'\nmv '{0}.tmp' '{0}'\nexec sleep 300",
                argv.display()
            ),
        );
        w.user_file("[permissions]\nallow = [\"websearch\"]\n");
        let approver = Arc::new(Collect::default());
        let search_path = format!("{}:/usr/bin:/bin", bin.display());
        let runtime = start(&w, Some(PathBuf::from(search_path)), approver.clone());

        // Stands in for the container: listens on the port Fontaine published.
        let served = tokio::spawn({
            let argv = argv.clone();
            async move {
                let text = loop {
                    if let Ok(text) = std::fs::read_to_string(&argv) {
                        break text;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                };
                let mapping = text.lines().find(|l| l.starts_with("127.0.0.1:")).unwrap();
                let port: u16 = mapping.split(':').nth(1).unwrap().parse().unwrap();
                let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
                let server = MockServer::builder().listener(listener).start().await;
                Mock::given(method("GET"))
                    .and(path("/search"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "results": [{"title": "T", "url": "https://a.example/", "content": "s"}]
                    })))
                    .mount(&server)
                    .await;
                server
            }
        });

        let handle = runtime.tool_server().clone();
        let out = handle
            .toolset()
            .execute("websearch", r#"{"query": "x"}"#, &mut as_root())
            .await
            .into_result()
            .map(|o| o.render())
            .unwrap_or_else(|e| panic!("{}", e.model_output().render()));
        assert!(out.contains("1. T"), "{out}");
        let _server = served.await.unwrap();

        let argv = std::fs::read_to_string(&argv).unwrap();
        let lines: Vec<&str> = argv.lines().collect();
        assert_eq!(&lines[..3], ["run", "--rm", "--name"], "{argv}");
        assert!(lines[3].starts_with("fontaine-searxng-"), "{argv}");
        assert_eq!(lines[4], "-p", "{argv}");
        assert!(lines[5].ends_with(":8080"), "{argv}");
        assert!(lines[7].ends_with("/etc/searxng/settings.yml:ro"), "{argv}");
        assert_eq!(lines[8], "docker.io/searxng/searxng:latest");
        let settings = std::fs::read_to_string(w.data.join("searxng/settings.yml")).unwrap();
        assert!(settings.contains("- json"), "{settings}");
        let warnings = approver.0.lock().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("pulls the image"), "{warnings:?}");
    }
}
