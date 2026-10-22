#![cfg(windows)]

mod common;

use common::pipeline::{Options, Rig};
use fontaine_core::audit::Decision;
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::tier::Tier;
use serde_json::{Value, json};

fn shell(command: &str) -> Value {
    json!({ "command": command })
}

async fn first(rig: &Rig, tool: &str, args: Value) -> (Decision, Tier) {
    let before = rig.audit().len();
    rig.check(tool, args).await;
    let record = rig.audit()[before].clone();
    (record.decision, record.tier)
}

#[tokio::test]
async fn critical_path_prompts_for_a_drive_root_and_the_user_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().canonicalize().unwrap().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let mut options = Options::new(Manual);
    options.home = Some(profile.clone());
    let rig = Rig::with(options);
    for command in [
        r"rm -rf 'C:\'".to_string(),
        "rm -rf 'C:/'".to_string(),
        r"rmdir 'C:\Windows'".to_string(),
        format!("rm -rf '{}'", profile.display()),
    ] {
        assert_eq!(
            first(&rig, "shell", shell(&command)).await,
            (Decision::Prompt, Tier::CriticalPath),
            "{command}"
        );
    }
    assert_eq!(
        first(&rig, "shell", shell("rm -rf build")).await,
        (Decision::Prompt, Tier::Mode)
    );
}

#[tokio::test]
async fn path_rules_ignore_case() {
    let rig =
        Rig::with(Options::new(Manual).settings("[permissions]\ndeny = [\"read(Secret/**)\"]\n"));
    std::fs::create_dir_all(rig.work().join("secret")).unwrap();
    std::fs::write(rig.work().join("secret").join("key.txt"), "k").unwrap();
    for path in ["secret/key.txt", "SECRET\\key.txt", "Secret/KEY.txt"] {
        assert_eq!(
            first(&rig, "read", json!({ "path": path })).await.1,
            Tier::DenyRule,
            "{path}"
        );
    }
}

#[tokio::test]
async fn path_rules_resolve_junctions() {
    let rig =
        Rig::with(Options::new(Manual).settings("[permissions]\ndeny = [\"read(secret/**)\"]\n"));
    std::fs::create_dir_all(rig.work().join("secret")).unwrap();
    std::fs::write(rig.work().join("secret").join("key.txt"), "k").unwrap();
    let link = rig.work().join("alias");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(rig.work().join("secret"))
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    assert_eq!(
        first(&rig, "read", json!({"path": "alias/key.txt"}))
            .await
            .1,
        Tier::DenyRule
    );
}

#[tokio::test]
async fn protected_paths_match_across_separators_and_case() {
    let rig = Rig::new(Manual);
    for path in [
        ".git\\config",
        ".GIT/config",
        "sub\\.Git\\hooks\\pre-commit",
        ".FONTAINE\\settings.toml",
        ".BashRC",
    ] {
        assert_eq!(
            first(&rig, "write", json!({ "path": path, "content": "x" })).await,
            (Decision::Prompt, Tier::ProtectedPath),
            "{path}"
        );
    }
}
