#![cfg(windows)]

use std::process::Command;

#[test]
fn rollback_is_unsupported_on_windows() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let config = root.join("config");
    std::fs::create_dir_all(&config).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fontaine"))
        .args(["update", "--rollback"])
        .current_dir(root)
        .env("FONTAINE_CONFIG_DIR", &config)
        .env("FONTAINE_DATA_DIR", root.join("data"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not supported on Windows"), "{stderr}");
}
