//! The `models` tool's source over a session's accounts.

mod common;

use std::ffi::OsString;
use std::sync::{Arc, RwLock};

use common::World;
use fontaine_core::agent::SessionModels;
use fontaine_core::child_runner::MainModel;
use fontaine_core::config::AccountKind;
use fontaine_core::model_roles::ModelChain;
use fontaine_core::models::{Listing, ModelHandle, ModelRef, Switcher};
use fontaine_tools::models_tool::ModelSource;
use rig_core::test_utils::MockCompletionModel;

fn main_model(text: &str, listing: &[&str]) -> MainModel {
    let handle = ModelHandle::new(
        ModelRef::parse(text).unwrap(),
        AccountKind::named("ollama"),
        MockCompletionModel::default().into(),
    );
    MainModel {
        handle: handle.clone(),
        chain: ModelChain::single(handle.reference.clone()),
        listing: Listing {
            models: listing.iter().map(|s| s.to_string()).collect(),
            refused: Vec::new(),
        },
    }
}

fn source(w: &World, main: Arc<RwLock<MainModel>>, with_switcher: bool) -> SessionModels {
    let switcher = Switcher::new(
        w.load().unwrap(),
        w.data.clone(),
        Arc::new(|_: &str| None::<OsString>),
        None,
    );
    SessionModels {
        roles: String::new(),
        switcher: with_switcher.then(|| Arc::new(RwLock::new(switcher))),
        main,
        guardian: Arc::new(|| None),
    }
}

fn world() -> World {
    let w = World::new();
    w.user_file(
        "[accounts.local]\nkind = \"ollama\"\nmodels = [\"llama3:latest\", \"qwen:7b\"]\n\n\
         [accounts.other]\nkind = \"ollama\"\nmodels = [\"phi\"]\n\n\
         [model]\nmodel = \"local/llama3\"\n",
    );
    w
}

#[tokio::test]
async fn an_unknown_account_errors_naming_the_known_ones() {
    let w = world();
    let main = Arc::new(RwLock::new(main_model("local/llama3", &["llama3:latest"])));
    let err = source(&w, main, true)
        .report(Some("nope"))
        .await
        .unwrap_err();
    assert!(
        err.contains("`nope`") && err.contains("local, other"),
        "{err}"
    );
}

#[tokio::test]
async fn without_a_switcher_the_models_cannot_be_listed() {
    let w = world();
    let main = Arc::new(RwLock::new(main_model("local/llama3", &[])));
    let err = source(&w, main, false).report(None).await.unwrap_err();
    assert_eq!(err, "models cannot be listed in this session");
}

#[tokio::test]
async fn roles_are_read_at_call_time() {
    let w = world();
    let main = Arc::new(RwLock::new(main_model("local/llama3", &["llama3:latest"])));
    let source = source(&w, main.clone(), true);
    let before = source.report(Some("local")).await.unwrap();
    assert!(before.contains("llama3:latest [main]"), "{before}");
    assert!(!before.contains("other"), "{before}");

    *main.write().unwrap() = main_model("local/qwen:7b", &["qwen:7b"]);
    let after = source.report(Some("local")).await.unwrap();
    assert!(after.contains("qwen:7b [main]"), "{after}");
    assert!(after.contains("llama3:latest\n"), "{after}");
}

#[tokio::test]
async fn the_session_roles_follow_the_report() {
    let w = world();
    let main = Arc::new(RwLock::new(main_model("local/llama3", &["llama3:latest"])));
    let mut source = source(&w, main, true);
    let plain = source.report(Some("local")).await.unwrap();
    assert!(!plain.contains("Roles"), "{plain}");

    source.roles = "- `coding`: local/qwen:7b, other/phi\n- `main`: local/llama3".to_string();
    let text = source.report(Some("local")).await.unwrap();
    let (accounts, roles) = text
        .split_once("Roles")
        .expect("a roles block after the report");
    assert!(accounts.contains("llama3:latest [main]"), "{text}");
    assert!(
        roles.contains("- `coding`: local/qwen:7b, other/phi")
            && roles.contains("- `main`: local/llama3"),
        "{text}"
    );
}
