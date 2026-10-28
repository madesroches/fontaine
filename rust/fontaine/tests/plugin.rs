//! The `fontaine plugin` CLI against a local-directory marketplace, entirely offline.

mod common;

use common::Sandbox;
use serde_json::json;

fn write_marketplace(sandbox: &Sandbox, root: &str) {
    sandbox.add_file(
        &format!("{root}/.claude-plugin/marketplace.json"),
        r#"{
            "name": "team",
            "plugins": [
                {"name": "review", "source": "./plugins/review", "description": "reviews"}
            ]
        }"#,
    );
    sandbox.add_file(
        &format!("{root}/plugins/review/.claude-plugin/plugin.json"),
        r#"{"name": "review"}"#,
    );
    sandbox.add_file(
        &format!("{root}/plugins/review/skills/review/SKILL.md"),
        "---\ndescription: reviews a diff\n---\nLook carefully.",
    );
}

#[tokio::test]
async fn the_plugin_cli_installs_lists_updates_and_removes_a_local_marketplace() {
    let sandbox = Sandbox::new();
    write_marketplace(&sandbox, "team-market");
    let market_path = sandbox.work.join("team-market");
    let market_path = market_path.to_string_lossy().into_owned();

    let add = sandbox
        .run(&["plugin", "marketplace", "add", &market_path])
        .await;
    assert_eq!(add.code, Some(0), "{}", add.stderr);

    let list = sandbox.run(&["plugin", "marketplace", "list"]).await;
    assert!(list.stdout.contains("team"), "{}", list.stdout);

    let install = sandbox.run(&["plugin", "install", "review@team"]).await;
    assert_eq!(install.code, Some(0), "{}", install.stderr);
    assert!(install.stdout.contains("1 skill(s)"), "{}", install.stdout);

    let list = sandbox.run(&["plugin", "list", "--format", "json"]).await;
    let value: serde_json::Value = serde_json::from_str(list.stdout.trim()).unwrap();
    assert_eq!(value["plugins"][0]["name"], json!("review"));
    assert_eq!(value["plugins"][0]["enabled"], json!(true));

    let disable = sandbox.run(&["plugin", "disable", "review"]).await;
    assert_eq!(disable.code, Some(0), "{}", disable.stderr);
    let enable = sandbox.run(&["plugin", "enable", "review"]).await;
    assert_eq!(enable.code, Some(0), "{}", enable.stderr);

    // A path-source plugin moves only after `marketplace update`.
    let before = serde_json::from_str::<serde_json::Value>(
        &sandbox
            .run(&["plugin", "list", "--format", "json"])
            .await
            .stdout,
    )
    .unwrap();
    let old_pin = before["plugins"][0]["pin"].as_str().unwrap().to_string();
    sandbox.add_file(
        "team-market/plugins/review/skills/review/SKILL.md",
        "---\ndescription: reviews a diff\n---\nNew body.",
    );
    let update_market = sandbox
        .run(&["plugin", "marketplace", "update", "team"])
        .await;
    assert_eq!(update_market.code, Some(0), "{}", update_market.stderr);
    assert!(
        update_market.stdout.contains("moved"),
        "{}",
        update_market.stdout
    );

    let update_plugin = sandbox.run(&["plugin", "update", "review"]).await;
    assert_eq!(update_plugin.code, Some(0), "{}", update_plugin.stderr);
    let after = serde_json::from_str::<serde_json::Value>(
        &sandbox
            .run(&["plugin", "list", "--format", "json"])
            .await
            .stdout,
    )
    .unwrap();
    assert_ne!(after["plugins"][0]["pin"].as_str().unwrap(), old_pin);

    let remove = sandbox
        .run(&["plugin", "marketplace", "remove", "team"])
        .await;
    assert_ne!(
        remove.code,
        Some(0),
        "a marketplace with an installed plugin is refused"
    );
    let uninstall = sandbox.run(&["plugin", "uninstall", "review"]).await;
    assert_eq!(uninstall.code, Some(0), "{}", uninstall.stderr);
    let remove = sandbox
        .run(&["plugin", "marketplace", "remove", "team"])
        .await;
    assert_eq!(remove.code, Some(0), "{}", remove.stderr);
}

#[tokio::test]
async fn install_refuses_duplicate_names_and_name_mismatches() {
    let sandbox = Sandbox::new();
    write_marketplace(&sandbox, "team-market");
    sandbox.add_file(
        "team-market/.claude-plugin/marketplace.json",
        r#"{
            "name": "team",
            "plugins": [
                {"name": "review", "source": "./plugins/review"},
                {"name": "other", "source": "./plugins/review"}
            ]
        }"#,
    );
    let path = sandbox.work.join("team-market");
    let path = path.to_string_lossy().into_owned();
    assert_eq!(
        sandbox
            .run(&["plugin", "marketplace", "add", &path])
            .await
            .code,
        Some(0)
    );

    // The manifest of `other` is named `review`, which install must refuse.
    let install = sandbox.run(&["plugin", "install", "other@team"]).await;
    assert_ne!(install.code, Some(0), "{}", install.stdout);
    assert!(install.stderr.contains("review"), "{}", install.stderr);

    assert_eq!(
        sandbox
            .run(&["plugin", "install", "review@team"])
            .await
            .code,
        Some(0)
    );
    let dup = sandbox.run(&["plugin", "install", "review@team"]).await;
    assert_ne!(dup.code, Some(0));
    assert!(dup.stderr.contains("already installed"), "{}", dup.stderr);
}
