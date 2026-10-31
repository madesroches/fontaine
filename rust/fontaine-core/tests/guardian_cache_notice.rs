//! The start-up notice for a pinned guardian whose model caches nothing below a length its
//! static prefix does not reach.

mod common;

use std::sync::{Arc, Mutex};

use common::World;
use fontaine_core::agent::build_guardian_telling;
use fontaine_core::config::Config;
use fontaine_core::models::{self, GuardianModel, ResolveParams};
use fontaine_core::permissions::{Answer, ApprovalRequest, Approver};
use fontaine_guardian::Profile;
use futures::future::BoxFuture;

#[derive(Default)]
struct Told {
    notices: Mutex<Vec<String>>,
}

impl Approver for Told {
    fn approve<'a>(&'a self, _: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }
    fn notice(&self, message: &str) {
        self.notices.lock().unwrap().push(message.to_string());
    }
}

async fn pinned(model: &str, extra: &str) -> (Config, GuardianModel) {
    let w = World::new();
    w.user_file(&format!(
        "[accounts.judge]\nkind = \"openrouter\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"{model}\"]\n{extra}\n[model]\nmodel = \"judge/{model}\"\n\n[guardian]\nmodel = \"judge/{model}\"\n"
    ));
    credentials(&w);
    let config = w.load().unwrap();
    let guardian = models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: &|_| None,
        model_override: None,
        session_id: None,
    })
    .await
    .unwrap()
    .guardian
    .expect("the guardian is configured");
    (config, guardian)
}

fn credentials(w: &World) {
    fontaine_core::credentials::store(&w.data, "judge", "sk-judge").unwrap();
}

fn notices(config: &Config, model: &GuardianModel, profile: Profile) -> Vec<String> {
    let told = Arc::new(Told::default());
    let approver: Arc<dyn Approver> = told.clone();
    let _ = build_guardian_telling(Some(model), config, profile, Some(&approver));
    let mut said = told.notices.lock().unwrap();
    std::mem::take(&mut *said)
}

#[tokio::test]
async fn a_minimum_above_the_prefix_is_announced_once() {
    let (config, model) = pinned("anthropic/claude-haiku-4.5", "").await;
    let said = notices(&config, &model, Profile::Minimal);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("judge/anthropic/claude-haiku-4.5") && said[0].contains("4096"),
        "{said:?}"
    );
}

#[tokio::test]
async fn the_prefix_size_depends_on_the_profile() {
    let (config, model) = pinned("anthropic/claude-3.5-haiku", "").await;
    assert_eq!(notices(&config, &model, Profile::Minimal).len(), 1);
    assert!(notices(&config, &model, Profile::Full).is_empty());
}

#[tokio::test]
async fn a_low_minimum_is_not_announced() {
    let (config, model) = pinned("anthropic/claude-sonnet-5", "").await;
    assert!(notices(&config, &model, Profile::Full).is_empty());
}

#[tokio::test]
async fn nothing_is_announced_with_caching_off() {
    let (config, model) = pinned("anthropic/claude-haiku-4.5", "prompt_caching = false\n").await;
    assert!(notices(&config, &model, Profile::Minimal).is_empty());
}
