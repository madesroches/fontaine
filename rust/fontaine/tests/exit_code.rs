use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

struct Sandbox {
    _tmp: TempDir,
    work: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let sandbox = Sandbox {
            work: root.join("work"),
            config: root.join("config"),
            data: root.join("data"),
            _tmp: tmp,
        };
        for dir in [&sandbox.work, &sandbox.config, &sandbox.data] {
            std::fs::create_dir_all(dir).unwrap();
        }
        sandbox
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fontaine"))
            .args(args)
            .current_dir(&self.work)
            .env("FONTAINE_CONFIG_DIR", &self.config)
            .env("FONTAINE_DATA_DIR", &self.data)
            .env_remove("FONTAINE_LOG")
            .output()
            .unwrap()
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn an_interactive_start_with_no_terminal_is_a_usage_error() {
    let s = Sandbox::new();
    let out = s.run(&[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    assert!(
        stderr(&out).contains("needs a terminal"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_valid_settings_overlay_is_accepted() {
    let s = Sandbox::new();
    let file = s.work.join("extra.toml");
    write(&file, "[permissions]\ndefault_mode = \"plan\"\n");
    let out = s.run(&["--settings", file.to_str().unwrap()]);
    // Past config loading, the start ends at the missing terminal.
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("needs a terminal"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn auto_is_accepted_by_the_interactive_start_as_it_is_by_print() {
    let s = Sandbox::new();
    let interactive = s.run(&["--permission-mode", "auto"]);
    let headless = s.run(&["-p", "--permission-mode", "auto", "hello"]);
    // Neither stops at the mode: the interactive start reaches the missing terminal, the
    // headless one the missing model.
    assert_eq!(
        interactive.status.code(),
        Some(2),
        "{}",
        stderr(&interactive)
    );
    assert!(
        stderr(&interactive).contains("needs a terminal"),
        "{}",
        stderr(&interactive)
    );
    assert_eq!(headless.status.code(), Some(2), "{}", stderr(&headless));
    assert!(
        stderr(&headless).contains("no main model"),
        "{}",
        stderr(&headless)
    );
    for out in [&interactive, &headless] {
        assert!(!stderr(out).contains("not available"), "{}", stderr(out));
    }
}

#[test]
fn an_unknown_key_in_the_user_file_exits_two() {
    let s = Sandbox::new();
    write(&s.config.join("config.toml"), "[permisions]\n");
    let out = s.run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("config.toml"), "{}", stderr(&out));
}

#[test]
fn a_scope_violation_in_project_settings_exits_two() {
    let s = Sandbox::new();
    write(
        &s.work.join(".fontaine/settings.toml"),
        "[permissions]\nallow = [\"shell\"]\n",
    );
    let out = s.run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("permissions.allow"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_missing_settings_file_exits_two_naming_the_path() {
    let s = Sandbox::new();
    let out = s.run(&["--settings", "nope.toml"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("nope.toml"));
}

#[test]
fn default_mode_auto_in_the_user_file_loads() {
    let s = Sandbox::new();
    write(
        &s.config.join("config.toml"),
        "[permissions]\ndefault_mode = \"auto\"\n",
    );
    // Past config loading, the start ends at the missing terminal.
    let out = s.run(&[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("needs a terminal"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn default_mode_auto_in_project_settings_exits_two() {
    let s = Sandbox::new();
    write(
        &s.work.join(".fontaine/settings.toml"),
        "[permissions]\ndefault_mode = \"auto\"\n",
    );
    let out = s.run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("permissions.default_mode"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn telemetry_enabled_without_an_endpoint_exits_two() {
    let s = Sandbox::new();
    write(
        &s.config.join("config.toml"),
        "[telemetry]\nenabled = true\n",
    );
    let out = s.run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("endpoint"));
}

#[test]
fn a_usage_error_exits_two() {
    let s = Sandbox::new();
    assert_eq!(s.run(&["--no-such-flag"]).status.code(), Some(2));
}

#[test]
fn help_and_version_exit_zero() {
    let s = Sandbox::new();
    for flag in ["--help", "--version"] {
        let out = s.run(&[flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert!(!out.stdout.is_empty());
    }
}
