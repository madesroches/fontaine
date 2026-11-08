use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Writes an executable `#!/bin/sh` script and returns once it can be exec'd. A fork in another
/// test thread can inherit the write descriptor until its own exec, and exec'ing the script in that
/// window fails with ETXTBSY; probing here keeps that out of the code under test.
pub(crate) fn write_executable(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    // The guard makes the probe side-effect free for scripts that log to a state file.
    std::fs::write(
        path,
        format!("#!/bin/sh\n[ -z \"$FONTAINE_TEST_PROBE\" ] || exit 0\n{body}\n"),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for _ in 0..500 {
        match Command::new(path).env("FONTAINE_TEST_PROBE", "1").status() {
            Ok(status) => {
                assert!(
                    status.success(),
                    "probe of {} failed: {status}",
                    path.display()
                );
                return;
            }
            Err(e) if e.kind() == ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("probe of {} failed: {e}", path.display()),
        }
    }
    panic!("{} stayed busy for 5s", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_does_not_run_the_body_but_a_normal_run_does() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let script = dir.path().join("nested/s.sh");
        write_executable(&script, &format!("echo x >> '{}'", marker.display()));
        assert!(!marker.exists());
        assert!(Command::new(&script).status().unwrap().success());
        assert!(marker.exists());
    }
}
