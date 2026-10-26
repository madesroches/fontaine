//! The SearXNG instance Fontaine runs in a container when no `[websearch] backend` is set: what
//! to run, the settings file it mounts, and which loopback port it listens on.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fontaine_tools::websearch::{Managed, ManagedPlan};
use url::Url;

use crate::owner_only;

/// Fully qualified so podman, which has no default registry, resolves it too.
pub const IMAGE: &str = "docker.io/searxng/searxng:latest";

/// The first start can pull the image.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(300);

const RUNTIMES: [&str; 2] = ["docker", "podman"];
const SETTINGS_DIR: &str = "searxng";
const SETTINGS_FILE: &str = "settings.yml";

/// The first container runtime on `path` (the value of a `PATH` variable), `docker` before
/// `podman`.
pub fn find_runtime(path: Option<&OsStr>) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = std::env::split_paths(path?).collect();
    RUNTIMES.iter().find_map(|name| {
        dirs.iter().find_map(|dir| {
            let candidate = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            is_executable(&candidate).then_some(candidate)
        })
    })
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn new_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn settings_text(secret: &str) -> String {
    format!(
        "use_default_settings: true\n\
         search:\n  formats:\n    - html\n    - json\n\
         server:\n  secret_key: \"{secret}\"\n  limiter: false\n"
    )
}

fn stored_secret(text: &str) -> Option<String> {
    let rest = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("secret_key:"))?;
    let secret = rest.trim().trim_matches(['"', '\'']);
    (!secret.is_empty()).then(|| secret.to_string())
}

/// Writes `<data_dir>/searxng/settings.yml`, keeping the secret an earlier run generated, and
/// returns its path. The directory is owner-only; the file itself must stay readable by the
/// container's unprivileged user, which a bind mount shows with the host's permissions.
pub fn ensure_settings(data_dir: &Path) -> io::Result<PathBuf> {
    let dir = data_dir.join(SETTINGS_DIR);
    std::fs::create_dir_all(data_dir)?;
    match owner_only::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let path = dir.join(SETTINGS_FILE);
    let secret = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| stored_secret(&text))
        .unwrap_or_else(new_secret);
    let text = settings_text(&secret);
    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == text) {
        return Ok(path);
    }
    owner_only::write_atomic(&path, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(path)
}

/// A loopback port nothing listens on now.
pub fn free_port() -> io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port())
}

/// Prefix of every managed container's name, which identifies strays a hard kill left behind.
pub const NAME_PREFIX: &str = "fontaine-searxng-";

fn new_name() -> String {
    format!("{NAME_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

/// Removes the container by name: it works whether or not the `docker run` client got to pass
/// its stop on.
fn cleanup_argv(runtime: &Path, name: &str) -> Vec<String> {
    vec![
        runtime.to_string_lossy().into_owned(),
        "rm".into(),
        "-f".into(),
        name.into(),
    ]
}

fn argv(runtime: &Path, name: &str, port: u16, settings: &Path) -> Vec<String> {
    vec![
        runtime.to_string_lossy().into_owned(),
        "run".into(),
        "--rm".into(),
        "--name".into(),
        name.into(),
        "-p".into(),
        format!("127.0.0.1:{port}:8080"),
        "-v".into(),
        format!("{}:/etc/searxng/settings.yml:ro", settings.display()),
        IMAGE.into(),
    ]
}

/// The managed backend: each start writes the settings, picks a port and says once that the
/// first run may pull the image. The command is fixed here; neither project config nor the
/// model has a say in it.
pub fn managed(
    runtime: PathBuf,
    data_dir: PathBuf,
    env: Vec<(OsString, OsString)>,
    notice: Arc<dyn Fn(&str) + Send + Sync>,
) -> Managed {
    let told = std::sync::atomic::AtomicBool::new(false);
    Managed::new(STARTUP_TIMEOUT, move || {
        let settings = ensure_settings(&data_dir)?;
        let port = free_port()?;
        if !told.swap(true, std::sync::atomic::Ordering::Relaxed) {
            notice(&format!(
                "starting SearXNG in a container for web search with {}; the first run pulls \
                 the image and can take a few minutes",
                runtime.display()
            ));
        }
        let name = new_name();
        Ok(ManagedPlan {
            argv: argv(&runtime, &name, port, &settings),
            cleanup: Some(cleanup_argv(&runtime, &name)),
            base_url: Url::parse(&format!("http://127.0.0.1:{port}"))
                .expect("a loopback address with a port is a valid URL"),
            env: env.clone(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake(dir: &Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn docker_is_preferred_over_podman_and_none_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_runtime(Some(dir.path().as_os_str())), None);
        assert_eq!(find_runtime(None), None);
        fake(dir.path(), "podman");
        assert_eq!(
            find_runtime(Some(dir.path().as_os_str())),
            Some(dir.path().join("podman"))
        );
        fake(dir.path(), "docker");
        assert_eq!(
            find_runtime(Some(dir.path().as_os_str())),
            Some(dir.path().join("docker"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_without_the_execute_bit_is_not_a_runtime() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("docker"), "").unwrap();
        assert_eq!(find_runtime(Some(dir.path().as_os_str())), None);
    }

    #[test]
    fn the_settings_enable_json_and_keep_their_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_settings(dir.path()).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        for line in [
            "use_default_settings: true",
            "- json",
            "limiter: false",
            "secret_key:",
        ] {
            assert!(first.contains(line), "{first}");
        }
        let secret = stored_secret(&first).unwrap();
        assert_eq!(secret.len(), 64);
        assert_eq!(ensure_settings(dir.path()).unwrap(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);

        std::fs::write(&path, "server:\n  secret_key: 'mine'\n").unwrap();
        ensure_settings(dir.path()).unwrap();
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert_eq!(stored_secret(&rewritten).as_deref(), Some("mine"));
        assert!(rewritten.contains("- json"));
    }

    #[cfg(unix)]
    #[test]
    fn only_the_owner_can_reach_the_settings() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_settings(dir.path()).unwrap();
        let mode = |p: &Path| p.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        // The container's user is not the owner, so the file stays world-readable inside the
        // owner-only directory.
        assert_eq!(mode(&path), 0o644);
    }

    #[test]
    fn the_argv_binds_loopback_and_mounts_the_settings_read_only() {
        let argv = argv(
            Path::new("/usr/bin/docker"),
            "fontaine-searxng-abc",
            4242,
            Path::new("/d/settings.yml"),
        );
        assert_eq!(
            argv,
            [
                "/usr/bin/docker",
                "run",
                "--rm",
                "--name",
                "fontaine-searxng-abc",
                "-p",
                "127.0.0.1:4242:8080",
                "-v",
                "/d/settings.yml:/etc/searxng/settings.yml:ro",
                "docker.io/searxng/searxng:latest",
            ]
        );
    }

    #[test]
    fn the_cleanup_removes_the_container_by_its_generated_name() {
        let (a, b) = (new_name(), new_name());
        assert!(a.starts_with(NAME_PREFIX));
        assert_ne!(a, b);
        assert_eq!(
            cleanup_argv(Path::new("/usr/bin/docker"), &a),
            ["/usr/bin/docker", "rm", "-f", a.as_str()]
        );
    }

    #[test]
    fn free_ports_are_loopback_ports_that_can_be_bound() {
        let port = free_port().unwrap();
        assert!(port > 0);
        std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    }
}
