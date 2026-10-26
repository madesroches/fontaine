/// Variables every launched child process (an MCP server, the SearXNG instance) gets so it can
/// find programs and its home, and nothing that names a credential.
pub const BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "TMP",
    "TEMP",
    "TZ",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROGRAMDATA",
    "WINDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
];

/// The [`BASE_ENV`] variables that are set, with their values, for a child started with a
/// cleared environment.
pub fn base_env(
    lookup: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    BASE_ENV
        .iter()
        .filter_map(|name| lookup(name).map(|value| ((*name).into(), value)))
        .collect()
}

/// What a container runtime CLI needs, beyond [`BASE_ENV`], to reach its daemon or rootless
/// socket. Only the managed SearXNG launch passes these.
pub const CONTAINER_RUNTIME_ENV: &[&str] = &[
    "DOCKER_HOST",
    "DOCKER_CONFIG",
    "XDG_RUNTIME_DIR",
    "CONTAINER_HOST",
];

/// [`base_env`] plus the [`CONTAINER_RUNTIME_ENV`] variables that are set.
pub fn container_runtime_env(
    lookup: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut env = base_env(lookup);
    env.extend(
        CONTAINER_RUNTIME_ENV
            .iter()
            .filter_map(|name| lookup(name).map(|value| ((*name).into(), value))),
    );
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_container_runtime_variables_are_added_only_when_set() {
        let lookup = |name: &str| {
            ["PATH", "DOCKER_HOST", "AWS_SECRET_ACCESS_KEY"]
                .contains(&name)
                .then(|| std::ffi::OsString::from("v"))
        };
        let names: Vec<_> = container_runtime_env(&lookup)
            .into_iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["PATH", "DOCKER_HOST"]);
    }
}
