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
