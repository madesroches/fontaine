mod common;

use common::{Endpoint, Output, Sandbox, Turn, Wire, write};
use serde_json::Value;

const EXIT_CONFIG: i32 = 2;

/// A sandbox plus a `target` directory beside the launch directory.
async fn setup() -> (Sandbox, std::path::PathBuf, Endpoint) {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("ok")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let target = s.work.parent().unwrap().join("target");
    std::fs::create_dir_all(&target).unwrap();
    (s, target.canonicalize().unwrap(), endpoint)
}

fn init(out: &Output) -> Value {
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    out.lines().remove(0)
}

fn project_path(init: &Value) -> Option<String> {
    let sources = init["config_sources"].as_array().unwrap();
    let project = sources.iter().find(|s| s["scope"] == "project").unwrap();
    project["loaded"]
        .as_bool()
        .unwrap()
        .then(|| project["path"].as_str().unwrap().to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn cd_makes_the_directory_the_first_workspace_root() {
    let (s, target, _e) = setup().await;
    let arg = target.to_str().unwrap();
    let out = s
        .headless(&["--output-format", "stream-json", "--cd", arg, "hi"])
        .await;
    let init = init(&out);
    assert_eq!(init["workspace_roots"][0], target.display().to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relative_cd_resolves_against_the_launch_directory() {
    let (s, target, _e) = setup().await;
    let sub = s.work.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let out = s
        .headless(&["--output-format", "stream-json", "--cd", "sub", "hi"])
        .await;
    let init = init(&out);
    assert_eq!(init["workspace_roots"][0], sub.display().to_string());
    assert_ne!(init["workspace_roots"][0], target.display().to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn project_settings_come_from_the_cd_directory() {
    let (s, target, _e) = setup().await;
    write(
        &target.join(".fontaine/settings.toml"),
        "[permissions]\ndeny = [\"read(.env)\"]\n",
    );
    write(
        &s.work.join(".fontaine/settings.toml"),
        "[permissions]\ndeny = [\"read(.env)\"]\n",
    );
    let with = s
        .headless(&[
            "--output-format",
            "stream-json",
            "-C",
            target.to_str().unwrap(),
            "hi",
        ])
        .await;
    let path = project_path(&init(&with)).expect("project settings load from --cd");
    assert!(path.starts_with(&target.display().to_string()), "{path}");

    std::fs::remove_file(target.join(".fontaine/settings.toml")).unwrap();
    let without = s
        .headless(&[
            "--output-format",
            "stream-json",
            "-C",
            target.to_str().unwrap(),
            "hi",
        ])
        .await;
    assert_eq!(project_path(&init(&without)), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_or_non_directory_cd_exits_with_the_config_code() {
    let (s, target, _e) = setup().await;
    let file = target.join("file.txt");
    write(&file, "x");
    for bad in ["does-not-exist", file.to_str().unwrap()] {
        let out = s.headless(&["--cd", bad, "hi"]).await;
        assert_eq!(out.code, Some(EXIT_CONFIG), "{}", out.stderr);
        assert!(out.stderr.contains("--cd"), "{}", out.stderr);
        assert!(out.stderr.contains(bad), "{}", out.stderr);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn relative_flag_paths_resolve_against_the_cd_directory() {
    let (s, target, _e) = setup().await;
    write(
        &target.join("extra.toml"),
        "[permissions]\ndeny = [\"read(.env)\"]\n",
    );
    std::fs::create_dir_all(target.join("shared")).unwrap();
    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "--cd",
            target.to_str().unwrap(),
            "--debug-file",
            "debug.log",
            "--settings",
            "extra.toml",
            "--add-dir",
            "shared",
            "hi",
        ])
        .await;
    let init = init(&out);
    assert!(target.join("debug.log").exists());
    assert!(!s.work.join("debug.log").exists());
    let roots: Vec<&str> = init["workspace_roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert!(
        roots.contains(&target.join("shared").to_str().unwrap()),
        "{roots:?}"
    );
    let cli = init["config_sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["scope"] == "cli")
        .unwrap();
    assert_eq!(cli["loaded"], serde_json::json!(true));
}
