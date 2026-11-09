//! Reading and editing the places an MCP server is declared that `fontaine mcp` writes: the
//! workspace-private `mcp-local.toml`, the repository's `.mcp.json`, and `[mcp.servers]` in the
//! user's `config.toml` and the repository's `.fontaine/settings.toml`.
//!
//! The files in the user's directories are edited under a cross-process lock; the repository's
//! are only replaced atomically, since a lock file would land in the work tree.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table};

use crate::config::{self, McpServer};
use crate::mcp_json;
use crate::user_config::parse_document;

pub const LOCAL_FILE: &str = "mcp-local.toml";
pub const JSON_FILE: &str = ".mcp.json";

/// A place `fontaine mcp add` can write and `remove` can clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Local,
    Project,
    User,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Local => "local",
            Scope::Project => "project",
            Scope::User => "user",
        }
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn local_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LOCAL_FILE)
}

pub fn json_path(root: &Path) -> PathBuf {
    root.join(JSON_FILE)
}

pub fn project_settings_path(root: &Path) -> PathBuf {
    root.join(config::PROJECT_DIR).join(config::PROJECT_FILE)
}

pub fn user_path(config_dir: &Path) -> PathBuf {
    config_dir.join(config::USER_FILE)
}

fn root_key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

/// What `mcp-local.toml` holds for one workspace.
#[derive(Debug, Default)]
pub struct LocalRead {
    pub file_present: bool,
    /// The file has a `projects."<root>"` table for this workspace.
    pub applies: bool,
    /// The file was read and parsed.
    pub loaded: bool,
    pub servers: Vec<(String, McpServer)>,
    pub warnings: Vec<String>,
}

/// Reads this workspace's entries of `mcp-local.toml`. A malformed file or entry is a warning
/// and is skipped, so `fontaine mcp remove` can repair it.
pub fn read_local(path: &Path, root: &Path) -> LocalRead {
    let mut out = LocalRead::default();
    let text = match crate::file_lock::read_shared(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.file_present = true;
            out.warnings.push(format!(
                "cannot read {}: {}",
                path.display(),
                fontaine_tools::error_chain::chain(&e)
            ));
            return out;
        }
    };
    out.file_present = true;
    let table: toml::Table = match toml::from_str(&text) {
        Ok(table) => table,
        Err(e) => {
            out.warnings.push(format!(
                "ignoring {}: {}",
                path.display(),
                config::bare_toml_message(&text, e.span(), e.message())
            ));
            return out;
        }
    };
    out.loaded = true;
    let Some(project) = table
        .get("projects")
        .and_then(toml::Value::as_table)
        .and_then(|projects| projects.get(&root_key(root)))
    else {
        return out;
    };
    out.applies = true;
    let Some(servers) = project.get("servers").and_then(toml::Value::as_table) else {
        return out;
    };
    for (name, value) in servers {
        let server: Result<McpServer, _> = value.clone().try_into();
        match server {
            Err(e) => out.warnings.push(format!(
                "ignoring MCP server `{name}` in {}: {}",
                path.display(),
                e.message()
            )),
            Ok(server) => match config::check_mcp_server(name, &server) {
                Ok(()) => out.servers.push((name.clone(), server)),
                Err(message) => out.warnings.push(format!(
                    "ignoring MCP server `{name}` in {}: {message}",
                    path.display()
                )),
            },
        }
    }
    out
}

/// Whether `mcp-local.toml` parses at all, for the layer listing.
pub fn check_local_file(path: &Path) -> Result<bool, config::ConfigError> {
    let text = match crate::file_lock::read_shared(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(config::ConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    toml::from_str::<toml::Table>(&text).map_err(|e| config::ConfigError::Parse {
        path: path.to_path_buf(),
        message: config::bare_toml_message(&text, e.span(), e.message()),
    })?;
    Ok(true)
}

/// Whether `.mcp.json` parses at all, for the layer listing.
pub fn check_json_file(path: &Path) -> Result<bool, config::ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(config::ConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    mcp_json::parse(&text, mcp_json::Expansion::AtStart).map_err(|message| {
        config::ConfigError::Parse {
            path: path.to_path_buf(),
            message,
        }
    })?;
    Ok(true)
}

/// What a repository's `.mcp.json` holds.
#[derive(Debug, Default)]
pub struct JsonRead {
    pub file_present: bool,
    /// The file was read and parsed.
    pub loaded: bool,
    pub servers: Vec<(String, McpServer)>,
    pub warnings: Vec<String>,
}

/// Reads a repository's `.mcp.json`, keeping `${VAR}` references raw. A malformed file or entry
/// is a warning and is skipped.
pub fn read_project_json(path: &Path) -> JsonRead {
    let mut out = JsonRead::default();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.file_present = true;
            out.warnings.push(format!(
                "cannot read {}: {}",
                path.display(),
                fontaine_tools::error_chain::chain(&e)
            ));
            return out;
        }
    };
    out.file_present = true;
    let parsed = match mcp_json::parse(&text, mcp_json::Expansion::AtStart) {
        Ok(parsed) => parsed,
        Err(message) => {
            out.warnings
                .push(format!("ignoring {}: {message}", path.display()));
            return out;
        }
    };
    out.loaded = true;
    out.warnings.extend(
        parsed
            .problems
            .into_iter()
            .map(|p| format!("ignoring an entry of {}: {p}", path.display())),
    );
    out.warnings.extend(parsed.warnings);
    for (name, server) in parsed.servers {
        match config::check_mcp_server(&name, &server) {
            Ok(()) => out.servers.push((name, server)),
            Err(message) => out.warnings.push(format!(
                "ignoring MCP server `{name}` in {}: {message}",
                path.display()
            )),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Editing

#[derive(Clone, Copy)]
enum Store {
    /// The user's own directory: a lock beside the file, owner-only.
    Private,
    /// The user's settings document: a lock beside the file, existing permissions kept.
    UserDocument,
    /// A file in the work tree: no lock, normal permissions for a new file.
    Repository,
}

impl Store {
    fn read(self, path: &Path) -> io::Result<String> {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            other => other,
        }
    }

    fn write(self, path: &Path, text: &str) -> io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        match self {
            Store::Private => crate::owner_only::write_atomic(path, text.as_bytes()),
            Store::UserDocument => {
                crate::owner_only::write_atomic_keeping_mode(path, text.as_bytes())
            }
            Store::Repository => {
                crate::owner_only::write_atomic_default_mode(path, text.as_bytes())
            }
        }
    }

    fn lock(self, path: &Path) -> io::Result<Option<crate::file_lock::FileLock>> {
        match self {
            Store::Private | Store::UserDocument => crate::file_lock::lock(path).map(Some),
            Store::Repository => Ok(None),
        }
    }
}

fn server_table(server: &McpServer) -> Table {
    let mut table = Table::new();
    if let Some(command) = &server.command {
        table.insert("command", toml_edit::value(command.as_str()));
    }
    let list = |items: &[String]| -> Item {
        let mut array = Array::new();
        for item in items {
            array.push(item.as_str());
        }
        toml_edit::value(array)
    };
    if let Some(args) = &server.args {
        table.insert("args", list(args));
    }
    if let Some(names) = &server.env_vars {
        table.insert("env_vars", list(names));
    }
    if let Some(env) = &server.env {
        let mut inline = InlineTable::new();
        for (key, value) in env {
            inline.insert(key, value.as_str().into());
        }
        table.insert("env", toml_edit::value(inline));
    }
    if let Some(url) = &server.url {
        table.insert("url", toml_edit::value(url.as_str()));
    }
    table
}

fn json_entry(server: &McpServer) -> Value {
    match (&server.command, &server.url) {
        (Some(command), _) => {
            let mut env: Map<String, Value> = server
                .env
                .iter()
                .flatten()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            for name in server.env_vars.iter().flatten() {
                env.entry(name.clone())
                    .or_insert_with(|| Value::String(format!("${{{name}}}")));
            }
            let mut entry = json!({
                "type": "stdio",
                "command": command,
                "args": server.args.clone().unwrap_or_default(),
            });
            if !env.is_empty() {
                entry["env"] = Value::Object(env);
            }
            entry
        }
        (None, url) => json!({ "type": "http", "url": url }),
    }
}

/// The table at `keys` inside `doc`, created (implicitly, so no empty header is written) on the
/// way when `create` is set.
fn table_at<'a>(
    doc: &'a mut DocumentMut,
    keys: &[&str],
    create: bool,
    path: &Path,
) -> io::Result<Option<&'a mut dyn toml_edit::TableLike>> {
    let mut table = doc.as_table_mut() as &mut dyn toml_edit::TableLike;
    for key in keys {
        if table.get(key).is_none() {
            if !create {
                return Ok(None);
            }
            let mut fresh = Table::new();
            fresh.set_implicit(true);
            table.insert(key, Item::Table(fresh));
        }
        table = table
            .get_mut(key)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| invalid(format!("`{key}` in {} is not a table", path.display())))?;
    }
    Ok(Some(table))
}

fn toml_add(
    store: Store,
    path: &Path,
    keys: &[&str],
    name: &str,
    server: &McpServer,
) -> io::Result<()> {
    let _lock = store.lock(path)?;
    let text = store.read(path)?;
    let mut doc = parse_document(&text, path)?;
    let servers = table_at(&mut doc, keys, true, path)?.expect("created on the way");
    servers.insert(name, Item::Table(server_table(server)));
    store.write(path, &doc.to_string())
}

fn toml_remove(store: Store, path: &Path, keys: &[&str], name: &str) -> io::Result<bool> {
    let _lock = store.lock(path)?;
    let text = match store.read(path) {
        Ok(text) if text.is_empty() => return Ok(false),
        Ok(text) => text,
        Err(e) => return Err(e),
    };
    let mut doc = parse_document(&text, path)?;
    let removed = match table_at(&mut doc, keys, false, path)? {
        Some(servers) => servers.remove(name).is_some(),
        None => false,
    };
    if !removed {
        return Ok(false);
    }
    // A table the removal leaves empty goes with it.
    for depth in (1..=keys.len()).rev() {
        let (parent_keys, leaf) = keys[..depth].split_at(depth - 1);
        let Some(parent) = table_at(&mut doc, parent_keys, false, path)? else {
            break;
        };
        let empty = parent
            .get(leaf[0])
            .and_then(Item::as_table_like)
            .is_some_and(|t| t.is_empty());
        if empty {
            parent.remove(leaf[0]);
        } else {
            break;
        }
    }
    store.write(path, &doc.to_string())?;
    Ok(true)
}

fn toml_has(
    read: fn(&Path) -> io::Result<String>,
    path: &Path,
    keys: &[&str],
    name: &str,
) -> io::Result<bool> {
    let text = match read(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let doc = parse_document(&text, path)?;
    let mut item = doc.as_item();
    for key in keys {
        match item.get(key) {
            Some(next) => item = next,
            None => return Ok(false),
        }
    }
    Ok(item.get(name).is_some())
}

fn read_json_object(path: &Path) -> io::Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(e),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(invalid(format!("{} is not a JSON object", path.display()))),
        Err(e) => Err(invalid(format!(
            "{} is not valid JSON: {e}",
            path.display()
        ))),
    }
}

fn write_json(path: &Path, object: Map<String, Value>) -> io::Result<()> {
    let mut text =
        serde_json::to_string_pretty(&Value::Object(object)).expect("a JSON value serializes");
    text.push('\n');
    Store::Repository.write(path, &text)
}

fn json_add(path: &Path, name: &str, server: &McpServer) -> io::Result<()> {
    let mut object = read_json_object(path)?;
    let servers = object
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| {
            invalid(format!(
                "`mcpServers` in {} is not an object",
                path.display()
            ))
        })?;
    servers.insert(name.to_string(), json_entry(server));
    write_json(path, object)
}

fn json_remove(path: &Path, name: &str) -> io::Result<bool> {
    let mut object = read_json_object(path)?;
    let removed = object
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .is_some_and(|servers| servers.shift_remove(name).is_some());
    if removed {
        write_json(path, object)?;
    }
    Ok(removed)
}

fn json_has(path: &Path, name: &str) -> io::Result<bool> {
    Ok(read_json_object(path)?
        .get("mcpServers")
        .and_then(Value::as_object)
        .is_some_and(|servers| servers.contains_key(name)))
}

/// Writes `server` as `name` in the file `scope` keeps, and returns that file. The name must
/// not already be defined there; [`find`] says whether it is.
pub fn add_server(
    scope: Scope,
    config_dir: &Path,
    root: &Path,
    name: &str,
    server: &McpServer,
) -> io::Result<PathBuf> {
    match scope {
        Scope::Local => {
            let path = local_path(config_dir);
            let root = root_key(root);
            toml_add(
                Store::Private,
                &path,
                &["projects", &root, "servers"],
                name,
                server,
            )?;
            Ok(path)
        }
        Scope::User => {
            let path = user_path(config_dir);
            toml_add(
                Store::UserDocument,
                &path,
                &["mcp", "servers"],
                name,
                server,
            )?;
            Ok(path)
        }
        Scope::Project => {
            let path = json_path(root);
            json_add(&path, name, server)?;
            Ok(path)
        }
    }
}

/// Removes `name` from every file `scope` keeps (for project, `.mcp.json` and
/// `.fontaine/settings.toml`). `false` means it was in none of them.
pub fn remove_server(scope: Scope, config_dir: &Path, root: &Path, name: &str) -> io::Result<bool> {
    match scope {
        Scope::Local => {
            let root = root_key(root);
            toml_remove(
                Store::Private,
                &local_path(config_dir),
                &["projects", &root, "servers"],
                name,
            )
        }
        Scope::User => toml_remove(
            Store::UserDocument,
            &user_path(config_dir),
            &["mcp", "servers"],
            name,
        ),
        Scope::Project => {
            // Both files are tried so a malformed one does not keep the other's entry.
            let from_json = json_remove(&json_path(root), name);
            let from_settings = toml_remove(
                Store::Repository,
                &project_settings_path(root),
                &["mcp", "servers"],
                name,
            );
            match (from_json, from_settings) {
                (Ok(a), Ok(b)) => Ok(a || b),
                (Ok(true), Err(_)) | (Err(_), Ok(true)) => Ok(true),
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        }
    }
}

/// Every scope and file that defines `name`, in scope order, and a warning for each file that
/// could not be read and so was skipped. Reads only.
pub fn find(name: &str, config_dir: &Path, root: &Path) -> (Vec<(Scope, PathBuf)>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let mut probe = |scope: Scope, path: PathBuf, has: io::Result<bool>| match has {
        Ok(true) => found.push((scope, path)),
        Ok(false) => {}
        Err(e) => warnings.push(format!(
            "ignoring {}: {}",
            path.display(),
            fontaine_tools::error_chain::chain(&e)
        )),
    };
    let local = local_path(config_dir);
    let key = root_key(root);
    let has = toml_has(
        crate::file_lock::read_shared,
        &local,
        &["projects", &key, "servers"],
        name,
    );
    probe(Scope::Local, local, has);
    let json = json_path(root);
    let has = json_has(&json, name);
    probe(Scope::Project, json, has);
    let settings = project_settings_path(root);
    let has = toml_has(
        |p| std::fs::read_to_string(p),
        &settings,
        &["mcp", "servers"],
        name,
    );
    probe(Scope::Project, settings, has);
    let user = user_path(config_dir);
    let has = toml_has(
        crate::file_lock::read_shared,
        &user,
        &["mcp", "servers"],
        name,
    );
    probe(Scope::User, user, has);
    found.sort_by_key(|(scope, _)| *scope);
    (found, warnings)
}

/// The scopes in `found`, once each.
pub fn scopes(found: &[(Scope, PathBuf)]) -> Vec<Scope> {
    let mut out: Vec<Scope> = found.iter().map(|(s, _)| *s).collect();
    out.dedup();
    out
}

/// A stdio server definition; empty lists are left out of the file.
pub fn stdio(
    command: &str,
    args: &[String],
    env: BTreeMap<String, String>,
    env_vars: Vec<String>,
) -> McpServer {
    McpServer {
        command: Some(command.to_string()),
        args: (!args.is_empty()).then(|| args.to_vec()),
        env_vars: (!env_vars.is_empty()).then_some(env_vars),
        env: (!env.is_empty()).then_some(env),
        url: None,
        expand: false,
    }
}

/// An HTTP server definition.
pub fn http(url: &str) -> McpServer {
    McpServer {
        command: None,
        args: None,
        env_vars: None,
        env: None,
        url: Some(url.to_string()),
        expand: false,
    }
}

/// Why `server` cannot be declared under `name`, if it cannot.
pub fn validate(name: &str, server: &McpServer) -> Result<(), String> {
    config::check_mcp_server(name, server)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct World {
        _dir: tempfile::TempDir,
        config: PathBuf,
        root: PathBuf,
    }

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        World {
            _dir: dir,
            config,
            root,
        }
    }

    fn echo() -> McpServer {
        stdio("echo", &["hi".to_string()], BTreeMap::new(), Vec::new())
    }

    #[test]
    fn a_local_server_round_trips_and_other_roots_are_untouched() {
        let w = world();
        let path = local_path(&w.config);
        std::fs::write(
            &path,
            "# mine\n[projects.\"/other\".servers.keep]\ncommand = \"k\"\n",
        )
        .unwrap();
        add_server(Scope::Local, &w.config, &w.root, "foo", &echo()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        assert!(text.contains("/other"), "{text}");
        let read = read_local(&path, &w.root);
        assert!(
            read.applies && read.warnings.is_empty(),
            "{:?}",
            read.warnings
        );
        assert_eq!(read.servers.len(), 1);
        assert_eq!(read.servers[0].0, "foo");
        assert_eq!(read.servers[0].1.args, Some(vec!["hi".to_string()]));
        assert!(!read_local(&path, Path::new("/nowhere")).applies);

        assert!(remove_server(Scope::Local, &w.config, &w.root, "foo").unwrap());
        assert!(!remove_server(Scope::Local, &w.config, &w.root, "foo").unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("keep") && !text.contains("foo"), "{text}");
    }

    #[test]
    fn user_scope_keeps_comments_and_order() {
        let w = world();
        let path = user_path(&w.config);
        std::fs::write(&path, "# top\n[tui]\ntheme = \"dark\" # inline\n").unwrap();
        add_server(Scope::User, &w.config, &w.root, "foo", &echo()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# top\n[tui]\ntheme = \"dark\" # inline\n"),
            "{text}"
        );
        assert!(text.contains("[mcp.servers.foo]"), "{text}");
        assert!(remove_server(Scope::User, &w.config, &w.root, "foo").unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "# top\n[tui]\ntheme = \"dark\" # inline\n");
    }

    #[test]
    fn project_scope_writes_claude_codes_shape_and_converts_pass_through_variables() {
        let w = world();
        let server = stdio(
            "srv",
            &["--x".to_string()],
            BTreeMap::from([("A".to_string(), "b".to_string())]),
            vec!["TOKEN".to_string()],
        );
        let path = add_server(Scope::Project, &w.config, &w.root, "foo", &server).unwrap();
        assert_eq!(path, w.root.join(".mcp.json"));
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value,
            json!({"mcpServers": {"foo": {
                "type": "stdio", "command": "srv", "args": ["--x"],
                "env": {"A": "b", "TOKEN": "${TOKEN}"}
            }}})
        );
        let http = McpServer {
            command: None,
            args: None,
            env_vars: None,
            env: None,
            url: Some("https://h/mcp".into()),
            expand: false,
        };
        add_server(Scope::Project, &w.config, &w.root, "web", &http).unwrap();
        let read = read_project_json(&path);
        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(read.servers.len(), 2);
        assert!(remove_server(Scope::Project, &w.config, &w.root, "foo").unwrap());
        assert!(!remove_server(Scope::Project, &w.config, &w.root, "foo").unwrap());
        assert_eq!(read_project_json(&path).servers.len(), 1);
    }

    #[test]
    fn project_remove_clears_the_settings_file_too_and_find_reports_every_file() {
        let w = world();
        std::fs::create_dir_all(w.root.join(".fontaine")).unwrap();
        std::fs::write(
            project_settings_path(&w.root),
            "[mcp.servers.foo]\ncommand = \"x\"\n",
        )
        .unwrap();
        add_server(Scope::Project, &w.config, &w.root, "foo", &echo()).unwrap();
        add_server(Scope::Local, &w.config, &w.root, "foo", &echo()).unwrap();
        add_server(Scope::User, &w.config, &w.root, "foo", &echo()).unwrap();
        let found = find("foo", &w.config, &w.root).0;
        assert_eq!(
            found.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            [Scope::Local, Scope::Project, Scope::Project, Scope::User]
        );
        assert_eq!(scopes(&found), [Scope::Local, Scope::Project, Scope::User]);
        assert!(find("bar", &w.config, &w.root).0.is_empty());
        assert!(remove_server(Scope::Project, &w.config, &w.root, "foo").unwrap());
        let found = find("foo", &w.config, &w.root).0;
        assert_eq!(scopes(&found), [Scope::Local, Scope::User]);
    }

    #[test]
    fn a_malformed_file_or_entry_is_a_warning_not_an_error() {
        let w = world();
        std::fs::write(json_path(&w.root), "{nope").unwrap();
        let read = read_project_json(&json_path(&w.root));
        assert!(read.servers.is_empty() && read.warnings.len() == 1);
        let path = local_path(&w.config);
        let key = root_key(&w.root);
        std::fs::write(
            &path,
            format!("[projects.\"{key}\".servers.bad]\ncommand = \"x\"\nurl = \"u\"\n[projects.\"{key}\".servers.ok]\ncommand = \"x\"\n"),
        )
        .unwrap();
        let read = read_local(&path, &w.root);
        assert_eq!(read.servers.len(), 1);
        assert_eq!(read.warnings.len(), 1);
        std::fs::write(&path, "= broken").unwrap();
        assert_eq!(read_local(&path, &w.root).warnings.len(), 1);
        assert!(check_local_file(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn repository_files_get_normal_permissions_and_no_lock_file() {
        use std::os::unix::fs::PermissionsExt;
        let w = world();
        add_server(Scope::Project, &w.config, &w.root, "foo", &echo()).unwrap();
        std::fs::create_dir_all(w.root.join(".fontaine")).unwrap();
        toml_add(
            Store::Repository,
            &project_settings_path(&w.root),
            &["mcp", "servers"],
            "bar",
            &echo(),
        )
        .unwrap();
        for path in [json_path(&w.root), project_settings_path(&w.root)] {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_ne!(mode & 0o044, 0, "{} is {mode:o}", path.display());
        }
        let mut names: Vec<String> = std::fs::read_dir(&w.root)
            .unwrap()
            .chain(std::fs::read_dir(w.root.join(".fontaine")).unwrap())
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert!(!names.iter().any(|n| n.ends_with(".lock")), "{names:?}");
    }
}
