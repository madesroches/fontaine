//! The SearXNG instance Fontaine runs in a container when no `[websearch] backend` is set: what
//! to run, the settings file it mounts, and which loopback port it listens on. One container,
//! named `fontaine-searxng`, is shared by every session and left running until it is stopped,
//! replaced, or the machine reboots.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fontaine_tools::websearch::{Managed, ManagedPlan, Prepared};
use url::Url;

use crate::owner_only;

/// Fully qualified so podman, which has no default registry, resolves it too.
pub const IMAGE: &str = "docker.io/searxng/searxng:latest";

/// The first start can pull the image. It also bounds how long a running container that does
/// not answer the probe is waited on before it counts as unhealthy.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(300);

const RUNTIMES: [&str; 2] = ["docker", "podman"];
const SETTINGS_DIR: &str = "searxng";
const SETTINGS_FILE: &str = "settings.yml";

/// How often a starting container is re-probed while waiting for it to answer, pacing the
/// same as the start loop in `fontaine_tools::websearch`.
const PROBE_INTERVAL: Duration = Duration::from_millis(100);
/// One runtime subcommand is killed after this long, so an unreachable daemon stalls a search
/// for seconds, not forever.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

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

/// The fixed name of the managed SearXNG container, shared by every session.
pub const NAME: &str = "fontaine-searxng";

/// Prefix of the old per-session container names; the sweep at each managed start removes any
/// that a hard kill or power loss left behind.
pub const NAME_PREFIX: &str = "fontaine-searxng-";

/// What `inspect` learned about a container name. `created` is included in `Starting` because
/// a concurrent `docker run --name` takes the name before the container starts, and removing a
/// container in that window would kill the winner's instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContainerState {
    /// `created`, `running` or `restarting`: it may still come up, so wait on the probe.
    Starting,
    /// `exited`, `dead`, or anything else: not serving and not about to, so replace it.
    Unhealthy,
}

fn parse_state(output: &str) -> ContainerState {
    match output.trim() {
        "created" | "running" | "restarting" => ContainerState::Starting,
        _ => ContainerState::Unhealthy,
    }
}

/// The host port in `<runtime> port` output: one `host:port` per line, the published mapping
/// first.
fn parse_published_port(output: &str) -> Option<u16> {
    output
        .lines()
        .filter_map(|line| line.rsplit(':').next())
        .filter_map(|port| port.trim().parse().ok())
        .next()
}

/// The container names in `<runtime> ps -a` output worth sweeping: the old per-session ones,
/// not the shared container itself and not an unrelated name the filter happened to match.
fn stray_names(listed: &[String]) -> Vec<&str> {
    listed
        .iter()
        .map(String::as_str)
        .filter(|name| name.starts_with(NAME_PREFIX))
        .collect()
}

/// The container runtime's CLI, with the subcommands the managed backend needs. Every call is
/// bounded so an unreachable daemon stalls a search for seconds, not forever, and runs on the
/// blocking pool so the executor keeps serving other tasks meanwhile.
struct RuntimeCli {
    program: PathBuf,
    env: Vec<(OsString, OsString)>,
}

impl RuntimeCli {
    async fn inspect(&self, name: &str) -> io::Result<Option<ContainerState>> {
        match self
            .output(&["inspect", "-f", "{{.State.Status}}", name])
            .await
        {
            Ok(output) => Ok(Some(parse_state(&output))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The host port published for the container's `8080/tcp`, if any.
    async fn published_port(&self, name: &str) -> io::Result<Option<u16>> {
        match self.output(&["port", name, "8080/tcp"]).await {
            Ok(output) => Ok(parse_published_port(&output)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn list_by_prefix(&self) -> io::Result<Vec<String>> {
        let output = self
            .output(&[
                "ps",
                "-a",
                "--filter",
                "name=fontaine-searxng",
                "--format",
                "{{.Names}}",
            ])
            .await?;
        Ok(output
            .lines()
            .filter_map(|line| line.split(',').next())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Removes a container by name; on failure returns its stderr for reporting.
    async fn rm(&self, name: &str) -> Result<(), String> {
        self.output(&["rm", "-f", name])
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Runs a subcommand with the runtime's minimal environment, killing it after
    /// [`COMMAND_TIMEOUT`]. A missing object ("no such container") is [`io::ErrorKind::NotFound`].
    async fn output(&self, args: &[&str]) -> io::Result<String> {
        let program = self.program.clone();
        let env = self.env.clone();
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let mut command = std::process::Command::new(&program);
            command
                .args(&args)
                .env_clear()
                .envs(env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn()?;
            let deadline = Instant::now() + COMMAND_TIMEOUT;
            loop {
                if child.try_wait()?.is_some() {
                    let output = child.wait_with_output()?;
                    if output.status.success() {
                        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
                    }
                    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                    let lower = stderr.to_ascii_lowercase();
                    let kind = if lower.contains("no such") {
                        io::ErrorKind::NotFound
                    } else {
                        io::ErrorKind::Other
                    };
                    return Err(io::Error::new(
                        kind,
                        format!("{}: {}", program.display(), stderr.trim()),
                    ));
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "{} did not answer within {} seconds",
                            program.display(),
                            COMMAND_TIMEOUT.as_secs()
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        })
        .await
        .map_err(io::Error::other)?
    }
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

/// Decides what the first search needs: an already-running shared instance to reuse, or a fresh
/// start under the fixed name. `wait` bounds how long a starting container that does not answer
/// is given before it counts as unhealthy.
async fn prepare(
    cli: &RuntimeCli,
    data_dir: &Path,
    wait: Duration,
    notice: &Arc<dyn Fn(&str) + Send + Sync>,
    warn: &Arc<dyn Fn(&str) + Send + Sync>,
    told: &Arc<std::sync::atomic::AtomicBool>,
) -> io::Result<Prepared> {
    for name in stray_names(&cli.list_by_prefix().await?) {
        if let Err(message) = cli.rm(name).await {
            warn(&format!(
                "could not remove the leftover SearXNG container {name}: {message}"
            ));
        }
    }
    let mut unhealthy = false;
    match cli.inspect(NAME).await? {
        None => {}
        Some(ContainerState::Unhealthy) => unhealthy = true,
        Some(ContainerState::Starting) => {
            let client = fontaine_tools::tls::build_client(&Default::default(), || {
                reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
            })
            .map_err(io::Error::other)?;
            let deadline = Instant::now() + wait;
            // A starting container may answer late or publish its port late; keep reading the
            // port back until the deadline, and only then count it as unhealthy.
            let mut url = None;
            loop {
                if url.is_none() {
                    url = cli
                        .published_port(NAME)
                        .await?
                        .map(|port| Url::parse(&format!("http://127.0.0.1:{port}")))
                        .transpose()
                        .expect("a loopback address with a port is a valid URL");
                }
                if let Some(url) = &url
                    && fontaine_tools::websearch::probe(&client, url).await
                {
                    return Ok(Prepared::Reuse(url.clone()));
                }
                if Instant::now() >= deadline {
                    unhealthy = true;
                    break;
                }
                tokio::time::sleep(PROBE_INTERVAL).await;
            }
        }
    }
    if unhealthy && let Err(message) = cli.rm(NAME).await {
        warn(&format!(
            "could not remove the unhealthy SearXNG container {NAME}: {message}"
        ));
        return Err(io::Error::other(format!(
            "cannot remove the unhealthy SearXNG container {NAME}: {message}"
        )));
    }
    if !told.swap(true, std::sync::atomic::Ordering::Relaxed) {
        notice(&format!(
            "starting SearXNG in a container for web search with {}; the first run pulls \
             the image and can take a few minutes",
            cli.program.display()
        ));
    }
    let settings = ensure_settings(data_dir)?;
    let port = free_port()?;
    Ok(Prepared::Start(ManagedPlan {
        argv: argv(&cli.program, NAME, port, &settings),
        base_url: Url::parse(&format!("http://127.0.0.1:{port}"))
            .expect("a loopback address with a port is a valid URL"),
        env: cli.env.clone(),
        fail_cleanup: Some(cleanup_argv(&cli.program, NAME)),
        shared: true,
    }))
}

/// The managed backend: the first search reuses the shared container when a healthy one is
/// running, replaces an unhealthy one, sweeps the old per-session strays, and otherwise starts
/// a fresh instance under the fixed name, announcing once that the first run may pull the
/// image. The command is fixed here; neither project config nor the model has a say in it.
pub fn managed(
    runtime: PathBuf,
    data_dir: PathBuf,
    env: Vec<(OsString, OsString)>,
    notice: Arc<dyn Fn(&str) + Send + Sync>,
    warn: Arc<dyn Fn(&str) + Send + Sync>,
) -> Managed {
    let told = Arc::new(std::sync::atomic::AtomicBool::new(false));
    Managed::new(STARTUP_TIMEOUT, move || {
        let cli = RuntimeCli {
            program: runtime.clone(),
            env: env.clone(),
        };
        let (data_dir, notice, warn, told) =
            (data_dir.clone(), notice.clone(), warn.clone(), told.clone());
        Box::pin(
            async move { prepare(&cli, &data_dir, STARTUP_TIMEOUT, &notice, &warn, &told).await },
        )
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
    fn fake_runtime(dir: &Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn no_notice() -> Arc<dyn Fn(&str) + Send + Sync> {
        Arc::new(|_| {})
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
    fn free_ports_are_loopback_ports_that_can_be_bound() {
        let port = free_port().unwrap();
        assert!(port > 0);
        std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    }

    #[test]
    fn the_argv_binds_loopback_and_mounts_the_settings_read_only() {
        let argv = argv(
            Path::new("/usr/bin/docker"),
            NAME,
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
                "fontaine-searxng",
                "-p",
                "127.0.0.1:4242:8080",
                "-v",
                "/d/settings.yml:/etc/searxng/settings.yml:ro",
                "docker.io/searxng/searxng:latest",
            ]
        );
    }

    #[test]
    fn the_cleanup_removes_the_shared_container_by_name() {
        assert_eq!(
            cleanup_argv(Path::new("/usr/bin/docker"), NAME),
            ["/usr/bin/docker", "rm", "-f", "fontaine-searxng"]
        );
    }

    #[test]
    fn the_port_is_the_host_side_of_the_first_published_mapping() {
        assert_eq!(parse_published_port("127.0.0.1:43749\n"), Some(43749));
        assert_eq!(parse_published_port("0.0.0.0:80\n127.0.0.1:81\n"), Some(80));
        assert_eq!(parse_published_port("garbage\n"), None);
        assert_eq!(parse_published_port(""), None);
    }

    #[test]
    fn inspect_output_is_a_starting_or_unhealthy_container() {
        assert_eq!(parse_state("running\n"), ContainerState::Starting);
        assert_eq!(parse_state("created\n"), ContainerState::Starting);
        assert_eq!(parse_state("restarting\n"), ContainerState::Starting);
        assert_eq!(parse_state("exited\n"), ContainerState::Unhealthy);
        assert_eq!(parse_state("dead\n"), ContainerState::Unhealthy);
        assert_eq!(parse_state(""), ContainerState::Unhealthy);
    }

    #[test]
    fn the_sweep_takes_old_per_session_names_only() {
        let listed: Vec<String> = [
            "fontaine-searxng-a798b73c",
            "fontaine-searxng",
            "someone-else",
            "fontaine-searxng-4f224762",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            stray_names(&listed),
            ["fontaine-searxng-a798b73c", "fontaine-searxng-4f224762"]
        );
    }

    /// A fake runtime script rooted at `state`; the returned CLI points at it. `body` holds the
    /// arms of a `case "$1"` dispatch.
    #[cfg(unix)]
    fn scripted_runtime(state: &Path, body: &str) -> RuntimeCli {
        let script = format!(
            "case \"$1\" in\n{}\nesac",
            body.replace("{state}", &state.display().to_string())
        );
        fake_runtime(state, "docker", &script);
        RuntimeCli {
            program: state.join("docker"),
            env: Vec::new(),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_running_container_that_answers_later_is_reused_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let cli = scripted_runtime(
            &state,
            "inspect) echo running ;;
\
             port) cat '{state}/port-out' ;;
\
             rm) echo \"$3\" >> '{state}/rms' ;;
\
             run) echo started >> '{state}/runs' ;;",
        );

        // The instance answers after a delay: the probe must wait, not remove the container.
        // The port is chosen after the delay, so nothing can take it in between.
        let appear = state.clone();
        let serve = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let late_port = listener.local_addr().unwrap().port();
            std::fs::write(appear.join("port-out"), format!("127.0.0.1:{late_port}\n")).unwrap();
            let server = wiremock::MockServer::builder()
                .listener(listener)
                .start()
                .await;
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .respond_with(wiremock::ResponseTemplate::new(200))
                .mount(&server)
                .await;
            server
        });

        let prepared = prepare(
            &cli,
            &dir.path().join("data"),
            Duration::from_secs(5),
            &no_notice(),
            &no_notice(),
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
        let url = match prepared {
            Prepared::Reuse(url) => url,
            Prepared::Start(_) => panic!("the answering container must be reused"),
        };
        let server = serve.await.unwrap();
        assert_eq!(
            url.port().unwrap(),
            Url::parse(&server.uri()).unwrap().port().unwrap()
        );
        assert!(
            !state.join("rms").exists(),
            "a running container is not removed"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_running_container_that_never_answers_is_removed_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let cli = scripted_runtime(
            &state,
            "inspect) echo running ;;
\
             port) echo 127.0.0.1:9 ;;
\
             rm) echo \"$3\" >> '{state}/rms' ;;
\
             run) echo started >> '{state}/runs' ;;",
        );
        let prepared = prepare(
            &cli,
            &dir.path().join("data"),
            Duration::from_millis(300),
            &no_notice(),
            &no_notice(),
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(matches!(prepared, Prepared::Start(_)), "{prepared:?}");
        assert_eq!(
            std::fs::read_to_string(state.join("rms")).unwrap().trim(),
            NAME
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_exited_container_is_removed_before_a_fresh_start() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let cli = scripted_runtime(
            &state,
            "inspect) echo exited ;;
\
             rm) echo \"$3\" >> '{state}/rms' ;;
\
             run) echo started >> '{state}/runs' ;;
\
             ps) true ;;",
        );
        let prepared = prepare(
            &cli,
            &dir.path().join("data"),
            Duration::from_millis(300),
            &no_notice(),
            &no_notice(),
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(matches!(prepared, Prepared::Start(_)), "{prepared:?}");
        assert_eq!(
            std::fs::read_to_string(state.join("rms")).unwrap().trim(),
            NAME
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_sweep_removes_each_old_per_session_container_and_keeps_going() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let cli = scripted_runtime(
            &state,
            "ps) printf '%s\\n' fontaine-searxng-a798b73c someone-else ;;
\
             inspect) echo exited ;;
\
             rm) echo \"$3\" >> '{state}/rms' ;;
\
             run) echo started >> '{state}/runs' ;;
\
             port) true ;;",
        );
        prepare(
            &cli,
            &dir.path().join("data"),
            Duration::from_millis(300),
            &no_notice(),
            &no_notice(),
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
        let rms = std::fs::read_to_string(state.join("rms")).unwrap();
        assert!(rms.contains("fontaine-searxng-a798b73c"), "{rms}");
        assert!(!rms.contains("someone-else"), "{rms}");
    }

    /// A container a concurrent `docker run --name` created but has not started yet: the name
    /// is taken, so it must be waited on, never removed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_created_container_that_answers_later_is_reused_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let cli = scripted_runtime(
            &state,
            "inspect) echo created ;;
\
             port) cat '{state}/port-out' ;;
\
             rm) echo \"$3\" >> '{state}/rms' ;;
\
             run) echo started >> '{state}/runs' ;;
\
             ps) true ;;",
        );
        // The published port appears only once the winner's client starts the container and the
        // instance answers a moment later; it is chosen after the delay so nothing can take it
        // in between.
        let appear = state.clone();
        let serve = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let late_port = listener.local_addr().unwrap().port();
            std::fs::write(appear.join("port-out"), format!("127.0.0.1:{late_port}\n")).unwrap();
            let server = wiremock::MockServer::builder()
                .listener(listener)
                .start()
                .await;
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .respond_with(wiremock::ResponseTemplate::new(200))
                .mount(&server)
                .await;
            server
        });
        let prepared = prepare(
            &cli,
            &dir.path().join("data"),
            Duration::from_secs(5),
            &no_notice(),
            &no_notice(),
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
        let url = match prepared {
            Prepared::Reuse(url) => url,
            Prepared::Start(_) => panic!("the created container must be waited on, not replaced"),
        };
        let server = serve.await.unwrap();
        assert_eq!(
            url.port().unwrap(),
            Url::parse(&server.uri()).unwrap().port().unwrap()
        );
        assert!(
            !state.join("rms").exists(),
            "a created container is not removed"
        );
    }

    /// The #241 behavior: a failed removal reaches the user through `warn`. A leftover stray's
    /// failure does not stop the prepare; an unhealthy container's failure does.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_removal_is_reported_and_only_the_unhealthy_one_aborts() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let cli = scripted_runtime(
            &state,
            "ps) printf '%s\\n' fontaine-searxng-a798b73c ;;
\
             inspect) echo exited ;;
\
             rm) echo \"$3\" >> '{state}/rms'; if [ \"$3\" = fontaine-searxng-a798b73c ]; then exit 1; fi ;;
\
             run) echo started >> '{state}/runs' ;;",
        );
        let warned = Arc::new(std::sync::Mutex::new(Vec::new()));
        let warn: Arc<dyn Fn(&str) + Send + Sync> = {
            let warned = warned.clone();
            Arc::new(move |message: &str| warned.lock().unwrap().push(message.to_string()))
        };
        let no_notice: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});

        // The stray's removal fails: warned, and the prepare still starts a fresh instance.
        let prepared = prepare(
            &cli,
            &dir.path().join("data"),
            Duration::from_millis(300),
            &no_notice,
            &warn,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;
        assert!(matches!(prepared, Ok(Prepared::Start(_))), "{prepared:?}");
        {
            let warned = warned.lock().unwrap();
            assert_eq!(warned.len(), 1, "{warned:?}");
            assert!(
                warned[0].contains("leftover SearXNG container fontaine-searxng-a798b73c"),
                "{warned:?}"
            );
        }

        // Now no removal succeeds at all: the stray failure is still only reported, and the
        // shared container's failure aborts the prepare.
        let script = state.join("docker");
        let text = std::fs::read_to_string(&script).unwrap();
        std::fs::write(
            &script,
            text.replace(
                "if [ \"$3\" = fontaine-searxng-a798b73c ]; then exit 1; fi",
                "exit 1",
            ),
        )
        .unwrap();
        let warned = Arc::new(std::sync::Mutex::new(Vec::new()));
        let warn: Arc<dyn Fn(&str) + Send + Sync> = {
            let warned = warned.clone();
            Arc::new(move |message: &str| warned.lock().unwrap().push(message.to_string()))
        };
        let prepared = prepare(
            &cli,
            &dir.path().join("data"),
            Duration::from_millis(300),
            &no_notice,
            &warn,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;
        let message = prepared.unwrap_err().to_string();
        assert!(
            message.contains("cannot remove the unhealthy SearXNG container"),
            "{message}"
        );
        let warned = warned.lock().unwrap();
        assert_eq!(warned.len(), 2, "{warned:?}");
        assert!(
            warned[0].contains("leftover SearXNG container fontaine-searxng-a798b73c"),
            "{warned:?}"
        );
        assert!(
            warned[1].contains("unhealthy SearXNG container fontaine-searxng"),
            "{warned:?}"
        );
    }
}
