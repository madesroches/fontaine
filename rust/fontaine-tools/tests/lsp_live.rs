//! The real rust-analyzer from the pinned toolchain against a two-file crate. Run by hand:
//! `cargo test -p fontaine-tools --test lsp_live -- --ignored`. It catches what the fake server
//! cannot: a real server's indexing latency, its position encoding, and flycheck diagnostics
//! arriving on the late path.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Dirs, as_root, scope};
use fontaine_tools::gate::CallView;
use fontaine_tools::lsp::{DiagnosticsStage, LspServerSpec, LspServers, LspTool, LspWorkspace};
use fontaine_tools::result_pipeline::ResultStage;
use fontaine_tools::risk::RiskClass;
use fontaine_tools::scope::Scope;
use rig_core::tool::{Tool, ToolOutput};
use serde_json::{Value, json};

struct Workspace(Arc<Scope>);

impl LspWorkspace for Workspace {
    fn scope(&self) -> Arc<Scope> {
        self.0.clone()
    }

    fn folders(&self) -> Vec<PathBuf> {
        self.0.roots().to_vec()
    }
}

async fn ask(tool: &LspTool, args: Value) -> Result<String, String> {
    tool.call(&mut as_root(), serde_json::from_value(args).unwrap())
        .await
        .map_err(|e| e.model_output().render())
}

async fn through_stage(stage: &DiagnosticsStage, tool: &str, args: &Value) -> String {
    let view = CallView {
        tool,
        risk: RiskClass::WorkspaceWrite,
        args,
        agent: "root",
        annotations: None,
        requires_user_interaction: false,
        synthetic: false,
    };
    stage
        .process(view, Ok(ToolOutput::text("done")))
        .await
        .unwrap()
        .render()
}

#[tokio::test]
#[ignore = "runs the real rust-analyzer"]
async fn rust_analyzer_answers_navigation_and_reports_a_type_error() {
    let dirs = Dirs::new();
    let root = dirs.dir("crate");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"live\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "mod util;\n\nfn main() {\n    println!(\"{}\", util::double(2));\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/util.rs"),
        "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n",
    )
    .unwrap();

    let workspace: Arc<dyn LspWorkspace> = Arc::new(Workspace(scope(&root, &[])));
    let spec = LspServerSpec {
        name: "rust-analyzer".to_string(),
        command: "rust-analyzer".to_string(),
        args: Vec::new(),
        env_vars: ["RUSTUP_HOME", "CARGO_HOME", "RUSTUP_TOOLCHAIN"]
            .map(String::from)
            .to_vec(),
        set_env: Vec::new(),
        extension_to_language: [(".rs".to_string(), "rust".to_string())].into(),
        initialization_options: None,
        settings: None,
        startup_timeout: Duration::from_secs(60),
        shutdown_timeout: Duration::from_secs(5),
        request_timeout: Duration::from_secs(60),
        restart_on_crash: false,
        max_restarts: 0,
        diagnostics: true,
    };
    let claims = [(".rs".to_string(), "rust-analyzer".to_string())].into();
    let lookup = |name: &str| std::env::var_os(name);
    let servers = LspServers::new(vec![spec], &claims, workspace.clone(), Arc::new(lookup));
    let tool = LspTool {
        servers: servers.clone(),
        workspace: workspace.clone(),
    };
    let stage = DiagnosticsStage {
        servers: servers.clone(),
        workspace,
    };

    // Indexing takes a while: ask until the server has an answer.
    let deadline = Instant::now() + Duration::from_secs(120);
    let definition = loop {
        let args = json!({
            "operation": "goToDefinition", "path": "src/main.rs", "line": 4, "character": 30
        });
        match ask(&tool, args).await {
            Ok(text) => break text,
            Err(e) if Instant::now() < deadline => {
                eprintln!("waiting for the index: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => panic!("no definition: {e}"),
        }
    };
    assert!(definition.contains("src/util.rs:1:8"), "{definition}");

    let references = ask(
        &tool,
        json!({"operation": "findReferences", "path": "src/util.rs", "line": 1, "character": 8}),
    )
    .await
    .unwrap();
    assert!(references.contains("src/main.rs:4:"), "{references}");

    std::fs::write(
        root.join("src/util.rs"),
        "pub fn double(x: i32) -> i32 {\n    \"oops\"\n}\n",
    )
    .unwrap();
    let edit = json!({"path": "src/util.rs"});
    let mut report = through_stage(&stage, "edit", &edit).await;
    // Flycheck runs `cargo check`, so the error may arrive on the late path.
    let deadline = Instant::now() + Duration::from_secs(120);
    while !report.contains("mismatched types") && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        report = through_stage(&stage, "read", &json!({})).await;
    }
    assert!(report.contains("mismatched types"), "{report}");
    servers.shutdown_all().await;
}
