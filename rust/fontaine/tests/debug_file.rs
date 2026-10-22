use std::process::{Command, Output};

use tempfile::TempDir;

struct Sandbox {
    _tmp: TempDir,
    work: std::path::PathBuf,
    config: std::path::PathBuf,
    data: std::path::PathBuf,
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

    fn run(&self, args: &[&str], log: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fontaine"));
        cmd.args(args)
            .current_dir(&self.work)
            .env("FONTAINE_CONFIG_DIR", &self.config)
            .env("FONTAINE_DATA_DIR", &self.data)
            .env_remove("FONTAINE_LOG");
        if let Some(log) = log {
            cmd.env("FONTAINE_LOG", log);
        }
        cmd.output().unwrap()
    }
}

#[test]
fn debug_file_records_the_config_loaded_event() {
    let s = Sandbox::new();
    let file = s.work.join("debug.log");
    let out = s.run(&["--debug-file", file.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(0));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("DEBUG"), "{text}");
    assert!(text.contains("config loaded"), "{text}");
    assert!(text.contains("scope_stack"), "{text}");
    assert!(text.contains("managed"), "{text}");
}

#[test]
fn the_debug_file_is_owner_only() {
    let s = Sandbox::new();
    let file = s.work.join("debug.log");
    // A pre-existing world-readable file is tightened before it is written.
    std::fs::write(&file, "old\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let out = s.run(&["--debug-file", file.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(0));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("old\n"), "appends rather than truncates");
}

#[test]
fn fontaine_log_warn_drops_the_debug_events() {
    let s = Sandbox::new();
    let file = s.work.join("debug.log");
    let out = s.run(&["--debug-file", file.to_str().unwrap()], Some("warn"));
    assert_eq!(out.status.code(), Some(0));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("config loaded"), "{text}");
}

#[test]
fn fontaine_log_alone_writes_no_file_and_nothing_to_stderr() {
    let s = Sandbox::new();
    let out = s.run(&[], Some("debug"));
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty() && out.stdout.is_empty());
    let entries: Vec<_> = std::fs::read_dir(&s.work).unwrap().collect();
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn an_unparseable_fontaine_log_directive_exits_two_naming_the_variable() {
    let s = Sandbox::new();
    let file = s.work.join("debug.log");
    let out = s.run(&["--debug-file", file.to_str().unwrap()], Some("[[bad"));
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("FONTAINE_LOG"));
}

#[test]
fn an_unopenable_debug_file_exits_two_naming_the_flag() {
    let s = Sandbox::new();
    let file = s.work.join("missing-dir/debug.log");
    let out = s.run(&["--debug-file", file.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--debug-file"));
}

#[test]
fn a_config_error_still_exits_two_with_a_debug_file() {
    let s = Sandbox::new();
    std::fs::write(s.config.join("config.toml"), "bogus = 1\n").unwrap();
    let file = s.work.join("debug.log");
    let out = s.run(&["--debug-file", file.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(2));
}
